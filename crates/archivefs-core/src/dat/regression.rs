//! Regression tests for defects found reviewing `feature/dat-audit-stage1`.
//!
//! Every test here was written first against the unfixed branch, where it
//! demonstrated the defect; each now asserts the corrected behaviour. The two
//! entity-attack cases are the exception - they passed before the review as
//! well, and exist to keep proving that no entity is ever expanded.

#![cfg(test)]

use super::audit::{AuditVerdict, KnownFileEvidence, audit_files};
use super::index::DatIndex;
use super::limits::{DEFAULT_MAX_ROMS_PER_ENTRY, DatLimits};
use super::model::{DatGameEntry, DatRomEntry, ParsedDat};
use super::parsers::clrmamepro::parse_clrmamepro;
use super::parsers::logiqx::parse_logiqx;

fn write(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.dat");
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

fn parse(content: &str) -> Result<super::parser::ParseOutcome, super::parser::ParseError> {
    let (_d, p) = write(content);
    parse_logiqx(&p, DatLimits::default())
}

fn parse_cmp(content: &str) -> super::parser::ParseOutcome {
    let (_d, p) = write(content);
    parse_clrmamepro(&p, DatLimits::default()).unwrap()
}

// --- Logiqx: entity references in attributes ----------------------------

#[test]
fn attribute_entities_are_decoded() {
    // Real DAT files are full of `&amp;` in names. Storing the raw text gives a
    // name that matches nothing and displays wrongly.
    let xml = r#"<datafile><game name="Tom &amp; Jerry"><rom name="tom &amp; jerry.bin" size="4" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).unwrap();
    assert_eq!(out.dat.games[0].name, "Tom & Jerry");
    assert_eq!(out.dat.games[0].roms[0].name, "tom & jerry.bin");
}

#[test]
fn numeric_character_references_in_attributes_are_decoded() {
    let xml = r#"<datafile><game name="Caf&#233;"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).unwrap();
    assert_eq!(out.dat.games[0].name, "Café");
}

// --- Logiqx: the per-game ROM ceiling -----------------------------------

#[test]
fn rom_ceiling_is_enforced_for_self_closing_roms() {
    // Every real Logiqx DAT writes ROMs as self-closing elements, so this is the
    // path the ceiling actually has to cover.
    let mut xml = String::from(r#"<datafile><game name="G">"#);
    for i in 0..5_000 {
        xml.push_str(&format!(
            r#"<rom name="r{i}.bin" size="1" crc="aabbccdd"/>"#
        ));
    }
    xml.push_str("</game></datafile>");
    let (_d, p) = write(&xml);
    let limits = DatLimits::builder().max_roms_per_entry(8).build();
    let result = parse_logiqx(&p, limits);
    assert!(
        matches!(
            result,
            Err(super::parser::ParseError::RomsPerEntryExceeded { .. })
        ),
        "expected the ROM ceiling to stop this, got {result:?}"
    );
}

#[test]
fn rom_ceiling_is_enforced_even_when_the_game_has_no_name() {
    let mut xml = String::from(r#"<datafile><game>"#);
    for i in 0..64 {
        xml.push_str(&format!(
            r#"<rom name="r{i}.bin" size="1" crc="aabbccdd"/>"#
        ));
    }
    xml.push_str("</game></datafile>");
    let (_d, p) = write(&xml);
    let limits = DatLimits::builder().max_roms_per_entry(4).build();
    assert!(matches!(
        parse_logiqx(&p, limits),
        Err(super::parser::ParseError::RomsPerEntryExceeded { .. })
    ));
}

// --- Logiqx: truncation and entity failures are reported ----------------

#[test]
fn truncation_at_an_element_boundary_is_reported() {
    // quick-xml rejects a cut inside a tag; a cut cleanly between elements just
    // ends with elements still open, and used to pass without a word.
    let xml = r#"<datafile><game name="A"><rom name="a.bin" size="1" crc="aabbccdd"/></game>"#;
    let out = parse(xml).expect("recovered entries are still returned");
    assert_eq!(out.dat.games.len(), 1);
    assert!(
        out.warnings
            .iter()
            .any(|w| w.to_string().contains("truncated")),
        "truncation should be warned about, got {:?}",
        out.warnings
    );
}

#[test]
fn a_complete_document_is_not_reported_as_truncated() {
    let xml =
        r#"<datafile><game name="A"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).unwrap();
    assert!(
        !out.warnings
            .iter()
            .any(|w| w.to_string().contains("truncated")),
        "a well-formed document must not be called truncated: {:?}",
        out.warnings
    );
}

#[test]
fn an_unresolvable_entity_in_text_is_reported_and_the_text_kept() {
    let xml = r#"<datafile><header><name>Before &myent; After</name></header><game name="A"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    let name = out.dat.source.name.as_deref().unwrap_or("");
    assert!(
        name.contains("Before") && name.contains("After"),
        "the surrounding text must survive, got {name:?}"
    );
    assert!(
        out.warnings
            .iter()
            .any(|w| w.to_string().contains("unresolvable entity")),
        "the failure must be reported, got {:?}",
        out.warnings
    );
}

// --- Logiqx: entity attacks stay neutralised ----------------------------

#[test]
fn billion_laughs_is_neutralised() {
    let xml = r#"<?xml version="1.0"?>
<!DOCTYPE datafile [
  <!ENTITY a "aaaaaaaaaa">
  <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
  <!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
  <!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;">
]>
<datafile><header><name>&d;</name></header>
<game name="A"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).expect("a DOCTYPE is inert text, not an error");
    let name = out.dat.source.name.as_deref().unwrap_or("");
    assert!(
        name.len() < 64,
        "no entity may be expanded; header name was {} bytes",
        name.len()
    );
    assert_eq!(out.dat.games.len(), 1, "the rest of the DAT still parses");
}

#[test]
fn external_entity_reference_reads_no_file() {
    let xml = r#"<?xml version="1.0"?>
<!DOCTYPE datafile [
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<datafile><header><name>&xxe;</name></header>
<game name="A"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    let name = out.dat.source.name.as_deref().unwrap_or("");
    assert!(
        !name.contains("root:"),
        "an external entity must never be resolved, got {name:?}"
    );
}

// --- Audit fixtures ------------------------------------------------------

fn dat_with(roms: Vec<(&str, DatRomEntry)>) -> ParsedDat {
    ParsedDat {
        source: super::model::DatSource {
            format: super::model::DatFormat::Logiqx,
            ecosystem: super::model::DatEcosystem::GenericLogiqx,
            file_path: "t.dat".into(),
            name: None,
            description: None,
            version: None,
            author: None,
            homepage: None,
            clrmamepro_header: None,
            entry_count: roms.len(),
            rom_count: roms.len(),
            parse_warnings: Vec::new(),
        },
        games: roms
            .into_iter()
            .map(|(game, rom)| DatGameEntry {
                name: game.to_string(),
                description: None,
                roms: vec![rom],
                clone_of: None,
                sample_of: None,
                board: None,
                rebuild_to: None,
                year: None,
                manufacturer: None,
                source_file: None,
                comment: None,
            })
            .collect(),
    }
}

fn rom(name: &str, crc: Option<&str>, md5: Option<&str>, size: Option<u64>) -> DatRomEntry {
    DatRomEntry {
        name: name.into(),
        size_bytes: size,
        crc32: crc.map(str::to_string),
        md5: md5.map(str::to_string),
        sha1: None,
        sha256: None,
        status: None,
        merge: None,
        date: None,
    }
}

/// The usual No-Intro shape: CRC32 and MD5 published, no SHA-256.
fn no_intro_index() -> DatIndex {
    DatIndex::build(&dat_with(vec![(
        "Super Game",
        rom(
            "super.bin",
            Some("abcdef01"),
            Some("d41d8cd98f00b204e9800998ecf8427e"),
            Some(4096),
        ),
    )]))
}

// --- Audit: every shared algorithm is tried -----------------------------

#[test]
fn a_hash_the_dat_does_not_publish_falls_through_to_one_it_does() {
    // The caller knows the SHA-256; the DAT publishes CRC32 and MD5. Stopping at
    // the strongest hash the caller held reported a perfect match as absent.
    let index = no_intro_index();
    let known = KnownFileEvidence::new("a/super.bin", "super.bin")
        .with_sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        .with_md5("d41d8cd98f00b204e9800998ecf8427e")
        .with_crc32("abcdef01")
        .with_size(4096);
    let report = audit_files(&[known], &index);
    assert!(
        matches!(
            report.entries[0].verdict,
            AuditVerdict::Exact {
                algorithm: "MD5",
                ..
            }
        ),
        "expected an MD5 Exact, got {:?}",
        report.entries[0].verdict
    );
}

#[test]
fn a_genuinely_absent_file_is_still_not_in_dat() {
    let index = no_intro_index();
    let known = KnownFileEvidence::new("a/other.bin", "other.bin")
        .with_md5("11111111111111111111111111111111")
        .with_crc32("11111111")
        .with_size(1);
    let report = audit_files(&[known], &index);
    assert_eq!(report.entries[0].verdict, AuditVerdict::NotInDat);
}

// --- Audit: hash case ----------------------------------------------------

#[test]
fn an_uppercase_known_hash_matches_the_index() {
    let index = no_intro_index();
    let known = KnownFileEvidence::new("a/super.bin", "super.bin")
        .with_md5("D41D8CD98F00B204E9800998ECF8427E");
    let report = audit_files(&[known], &index);
    assert!(
        matches!(report.entries[0].verdict, AuditVerdict::Exact { .. }),
        "an uppercase hash must still match, got {:?}",
        report.entries[0].verdict
    );
}

#[test]
fn a_malformed_known_hash_is_not_treated_as_evidence() {
    // "not a hash" is not a comparison that found nothing - it is no comparison
    // at all, and must not produce NotInDat.
    let index = no_intro_index();
    let known = KnownFileEvidence::new("a/super.bin", "super.bin").with_md5("nonsense");
    let report = audit_files(&[known], &index);
    assert!(
        matches!(report.entries[0].verdict, AuditVerdict::FilenameOnly { .. }),
        "got {:?}",
        report.entries[0].verdict
    );
}

// --- Audit: CRC32 confidence --------------------------------------------

#[test]
fn a_crc32_collision_is_not_reported_as_confident() {
    let index = DatIndex::build(&dat_with(vec![
        ("Game 0", rom("r0.bin", Some("abcdef01"), None, Some(4096))),
        ("Game 1", rom("r1.bin", Some("abcdef01"), None, Some(4096))),
    ]));
    let known = KnownFileEvidence::new("a/x.bin", "x.bin")
        .with_crc32("abcdef01")
        .with_size(4096);
    let report = audit_files(&[known], &index);
    let verdict = &report.entries[0].verdict;
    assert!(
        matches!(verdict, AuditVerdict::ProbableMultipleCandidates { .. }),
        "a 32-bit checksum collision is not an Exact verdict, got {verdict:?}"
    );
    assert!(!verdict.is_confident());
    assert_eq!(report.summary.probable_multiple, 1);
    assert_eq!(report.summary.exact_multiple, 0);
}

#[test]
fn a_cryptographic_collision_is_still_reported_as_confident() {
    let index = DatIndex::build(&dat_with(vec![
        (
            "Game 0",
            rom(
                "r0.bin",
                None,
                Some("d41d8cd98f00b204e9800998ecf8427e"),
                Some(4096),
            ),
        ),
        (
            "Game 1",
            rom(
                "r1.bin",
                None,
                Some("d41d8cd98f00b204e9800998ecf8427e"),
                Some(4096),
            ),
        ),
    ]));
    let known =
        KnownFileEvidence::new("a/x.bin", "x.bin").with_md5("d41d8cd98f00b204e9800998ecf8427e");
    let report = audit_files(&[known], &index);
    assert!(report.entries[0].verdict.is_confident());
    assert_eq!(report.summary.exact_multiple, 1);
}

// --- Audit: a compared hash never falls through to the filename ---------

#[test]
fn a_crc32_absent_from_the_dat_is_not_in_dat_not_a_filename_match() {
    // The DAT holds a ROM called super.bin. This file is also called super.bin,
    // but its CRC32 says it is a different dump - reporting the name match would
    // contradict the evidence already gathered.
    let index = no_intro_index();
    let known = KnownFileEvidence::new("a/super.bin", "super.bin")
        .with_crc32("11111111")
        .with_size(4096);
    let report = audit_files(&[known], &index);
    assert_eq!(report.entries[0].verdict, AuditVerdict::NotInDat);
}

#[test]
fn a_filename_match_is_still_reported_when_there_is_no_hash_at_all() {
    let index = no_intro_index();
    let known = KnownFileEvidence::new("a/super.bin", "super.bin");
    let report = audit_files(&[known], &index);
    assert!(matches!(
        report.entries[0].verdict,
        AuditVerdict::FilenameOnly { .. }
    ));
}

#[test]
fn a_crc32_present_but_size_disagreeing_is_ambiguous() {
    let index = no_intro_index();
    let known = KnownFileEvidence::new("a/super.bin", "super.bin")
        .with_crc32("abcdef01")
        .with_size(999);
    let report = audit_files(&[known], &index);
    assert!(
        matches!(report.entries[0].verdict, AuditVerdict::Ambiguous { .. }),
        "got {:?}",
        report.entries[0].verdict
    );
}

// --- ClrMamePro ----------------------------------------------------------

#[test]
fn clrmamepro_strips_quotes_from_header_fields() {
    let out = parse_cmp(
        "clrmamepro (\n\tname \"C64 Games\"\n\tauthor \"TOSEC\"\n)\ngame (\n\tname \"G\"\n\trom ( name \"a.prg\" size 1 crc aabbccdd )\n)\n",
    );
    assert_eq!(out.dat.source.name.as_deref(), Some("C64 Games"));
    assert_eq!(out.dat.source.author.as_deref(), Some("TOSEC"));
}

#[test]
fn clrmamepro_reads_md5_and_sha1_from_a_single_line_rom() {
    // Keys were read as alphabetic-only, so `md5 <hash>` tokenised as the key
    // `md` with the value `5` and the hash itself was discarded. Every strong
    // hash in every ClrMamePro DAT was silently lost.
    let out = parse_cmp(
        "game (\n\tname \"G\"\n\trom ( name \"a.prg\" size 100 crc aabbccdd md5 d41d8cd98f00b204e9800998ecf8427e sha1 da39a3ee5e6b4b0d3255bfef95601890afd80709 )\n)\n",
    );
    let r = &out.dat.games[0].roms[0];
    assert_eq!(r.crc32.as_deref(), Some("aabbccdd"));
    assert_eq!(r.md5.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
    assert_eq!(
        r.sha1.as_deref(),
        Some("da39a3ee5e6b4b0d3255bfef95601890afd80709")
    );
}

#[test]
fn clrmamepro_reads_sha256_from_a_multi_line_rom() {
    let out = parse_cmp(
        "game (\n\tname \"G\"\n\trom (\n\t\tname \"a.prg\"\n\t\tsize 100\n\t\tsha256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\t)\n)\n",
    );
    let r = &out.dat.games[0].roms[0];
    assert_eq!(
        r.sha256.as_deref(),
        Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
    );
}

#[test]
fn clrmamepro_strong_hashes_reach_the_index_and_the_audit() {
    // The end-to-end consequence of the tokeniser fix: a TOSEC-style DAT can now
    // be audited on MD5 instead of falling back to CRC32 for everything.
    let out = parse_cmp(
        "clrmamepro (\n\tname \"TOSEC Set\"\n)\ngame (\n\tname \"G\"\n\trom ( name \"a.prg\" size 100 crc aabbccdd md5 d41d8cd98f00b204e9800998ecf8427e )\n)\n",
    );
    let index = DatIndex::build(&out.dat);
    assert_eq!(index.md5_count(), 1);
    let known =
        KnownFileEvidence::new("x/a.prg", "a.prg").with_md5("d41d8cd98f00b204e9800998ecf8427e");
    let report = audit_files(&[known], &index);
    assert!(matches!(
        report.entries[0].verdict,
        AuditVerdict::Exact {
            algorithm: "MD5",
            ..
        }
    ));
}

#[test]
fn the_default_rom_ceiling_clears_a_mame_sized_machine() {
    // The ceiling is now enforced on the form real DATs use, so its default has
    // to sit far above real data. A MAME machine with thousands of ROM regions is
    // ordinary, not an attack.
    let mut xml = String::from(r#"<datafile><game name="neogeo">"#);
    for i in 0..20_000 {
        xml.push_str(&format!(
            r#"<rom name="r{i}.bin" size="1" crc="aabbccdd"/>"#
        ));
    }
    xml.push_str("</game></datafile>");
    let (_d, p) = write(&xml);
    let out =
        parse_logiqx(&p, DatLimits::default()).expect("a large but legitimate machine must parse");
    assert_eq!(out.dat.games[0].roms.len(), 20_000);
    const {
        assert!(
            DEFAULT_MAX_ROMS_PER_ENTRY >= 1_000_000,
            "the backstop must not be the thing that rejects a real catalogue"
        )
    };
}
