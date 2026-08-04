//! Regression tests for defects found reviewing `feature/dat-audit-stage1`.
//!
//! Most tests here were written first against the unfixed branch, where they
//! demonstrated a defect, and now assert the corrected behaviour. Run against
//! `ffa66fc` they fail - three of them by refusing to compile, because the fix
//! added an enum variant and a summary field the old code does not have.
//!
//! Eight are deliberately not like that, and pass on both revisions:
//!
//! * `billion_laughs_is_neutralised` and `external_entity_reference_reads_no_file`
//!   record a property the branch already had. `quick-xml` performs no DTD
//!   processing, so nothing is ever expanded; these exist so that stays true.
//! * `a_complete_document_is_not_reported_as_truncated`,
//!   `a_genuinely_absent_file_is_still_not_in_dat`,
//!   `a_filename_match_is_still_reported_when_there_is_no_hash_at_all`,
//!   `a_crc32_present_but_size_disagreeing_is_ambiguous`,
//!   `a_cryptographic_collision_is_still_reported_as_confident` and
//!   `a_well_formed_checksum_produces_no_warning` guard the behaviour the fixes
//!   had to *preserve* - each names a case adjacent to something that changed,
//!   and would have caught the fix going too far.

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

#[test]
fn an_unresolvable_entity_is_reported_the_same_way_in_an_attribute_as_in_text() {
    // The same content must produce the same report whichever syntactic position
    // it arrives in. Attributes used to fall back to the raw text silently while
    // text nodes warned, so whether a malformed DAT was flagged depended on where
    // the author happened to put the string.
    let attribute = r#"<datafile><game name="A &myent; B"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let from_attribute = parse(attribute).expect("parsed");
    assert_eq!(from_attribute.dat.games[0].name, "A &myent; B");
    assert!(
        from_attribute
            .warnings
            .iter()
            .any(|w| w.to_string().contains("unresolvable entity")),
        "an attribute's unresolvable entity must be reported, got {:?}",
        from_attribute.warnings
    );

    let text = r#"<datafile><header><name>A &myent; B</name></header><game name="G"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let from_text = parse(text).expect("parsed");
    assert!(
        from_text
            .warnings
            .iter()
            .any(|w| w.to_string().contains("unresolvable entity"))
    );
}

#[test]
fn a_well_formed_dat_produces_no_entity_warnings_at_all() {
    // The counterpart: warning behaviour must be deterministic in both
    // directions, so ordinary escaped content stays silent.
    let xml = r#"<datafile><header><name>Tom &amp; Jerry</name></header><game name="Tom &amp; Jerry"><rom name="tom &amp; jerry.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    assert_eq!(out.dat.games[0].name, "Tom & Jerry");
    assert_eq!(out.dat.source.name.as_deref(), Some("Tom & Jerry"));
    assert!(
        out.warnings.is_empty(),
        "a well-formed DAT must warn about nothing, got {:?}",
        out.warnings
    );
}

#[test]
fn a_malformed_checksum_in_the_dat_is_reported_not_silently_dropped() {
    // `dat validate` reports hash coverage. A typo'd checksum used to reduce that
    // coverage with no explanation, so a broken DAT and a sparse one looked alike.
    let xml = r#"<datafile><game name="A"><rom name="a.bin" size="1" crc="NOTHEX!!" md5="tooshort"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    assert_eq!(out.dat.games[0].roms[0].crc32, None);
    assert_eq!(out.dat.games[0].roms[0].md5, None);
    let text = out
        .warnings
        .iter()
        .map(|w| w.to_string())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        text.contains("crc") && text.contains("md5"),
        "both malformed checksums should be reported, got {text}"
    );
}

#[test]
fn a_well_formed_checksum_produces_no_warning() {
    let xml = r#"<datafile><game name="A"><rom name="a.bin" size="1" crc="AABBCCDD" md5="d41d8cd98f00b204e9800998ecf8427e"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    assert_eq!(out.dat.games[0].roms[0].crc32.as_deref(), Some("aabbccdd"));
    assert!(out.warnings.is_empty(), "got {:?}", out.warnings);
}

/// Exhaustive check of the one rule that stops a name collision being dressed up
/// as evidence: once any hash has actually been compared, the filename fallback
/// is unreachable whatever the outcome.
#[test]
fn the_filename_fallback_is_unreachable_after_any_real_hash_comparison() {
    let index = no_intro_index();

    // Every combination of "holds a well-formed hash", against a DAT that holds
    // an entry whose ROM name is the same as this file's.
    let hashes: [(&str, &str); 4] = [
        (
            "sha256",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        ("sha1", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        ("md5", "11111111111111111111111111111111"),
        ("crc32", "11111111"),
    ];
    for mask in 1..16u8 {
        let mut known = KnownFileEvidence::new("a/super.bin", "super.bin");
        for (bit, (kind, value)) in hashes.iter().enumerate() {
            if mask & (1 << bit) == 0 {
                continue;
            }
            known = match *kind {
                "sha256" => known.with_sha256(*value),
                "sha1" => known.with_sha1(*value),
                "md5" => known.with_md5(*value),
                _ => known.with_crc32(*value),
            };
        }
        // With and without a size, since size only qualifies the CRC32 path.
        for sized in [false, true] {
            let candidate = if sized {
                known.clone().with_size(4096)
            } else {
                known.clone()
            };
            let report = audit_files(&[candidate], &index);
            assert!(
                !matches!(report.entries[0].verdict, AuditVerdict::FilenameOnly { .. }),
                "mask {mask} sized={sized} fell back to the filename: {:?}",
                report.entries[0].verdict
            );
        }
    }
}

#[test]
fn probable_multiple_candidates_serialises_under_its_own_name() {
    let index = DatIndex::build(&dat_with(vec![
        ("Game 0", rom("r0.bin", Some("abcdef01"), None, Some(4096))),
        ("Game 1", rom("r1.bin", Some("abcdef01"), None, Some(4096))),
    ]));
    let known = KnownFileEvidence::new("a/x.bin", "x.bin")
        .with_crc32("abcdef01")
        .with_size(4096);
    let report = audit_files(&[known], &index);
    let json = serde_json::to_string(&report).expect("serialises");
    assert!(
        json.contains("probable_multiple_candidates"),
        "the weak-evidence verdict must be distinguishable in JSON: {json}"
    );
    assert!(
        !json.contains("exact_multiple_candidates"),
        "it must not be reported as an exact match: {json}"
    );
}

#[test]
fn exactly_one_warning_for_one_unresolvable_entity_in_an_attribute() {
    let xml = r#"<datafile><game name="A &myent; B"><rom name="a.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    let entity_warnings = out
        .warnings
        .iter()
        .filter(|w| w.to_string().contains("unresolvable entity"))
        .count();
    assert_eq!(
        entity_warnings, 1,
        "one bad entity in one attribute must produce exactly one warning, got {entity_warnings}"
    );
}

#[test]
fn an_unresolvable_entity_in_one_attribute_does_not_affect_others() {
    // The game name carries an unresolvable entity; the ROM's name and CRC are
    // ordinary. The fallback keeps the game name's raw text and the warning
    // reports only that one failure. The ROM's attributes must parse normally.
    let xml = r#"<datafile><game name="A &myent; B"><rom name="ok.bin" size="4096" crc="AABBCCDD" md5="d41d8cd98f00b204e9800998ecf8427e"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    assert_eq!(out.dat.games[0].name, "A &myent; B");
    let rom = &out.dat.games[0].roms[0];
    assert_eq!(rom.name, "ok.bin");
    assert_eq!(rom.size_bytes, Some(4096));
    assert_eq!(rom.crc32.as_deref(), Some("aabbccdd"));
    assert_eq!(rom.md5.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
    let entity_warnings = out
        .warnings
        .iter()
        .filter(|w| w.to_string().contains("unresolvable entity"))
        .count();
    assert_eq!(
        entity_warnings, 1,
        "only the game name's attribute had a bad entity, got {entity_warnings}"
    );
}

#[test]
fn json_output_is_deterministic_across_repeated_parses() {
    let xml = r#"<datafile><header><name>Deterministic Test</name></header><game name="Tom &amp; Jerry"><rom name="t.bin" size="4096" crc="aabbccdd" md5="d41d8cd98f00b204e9800998ecf8427e"/></game></datafile>"#;
    let (_d, p) = write(xml);
    let limits = DatLimits::default();
    let first = parse_logiqx(&p, limits).unwrap();
    let limits2 = DatLimits::default();
    let second = parse_logiqx(&p, limits2).unwrap();
    let json1 = serde_json::to_string_pretty(&first.dat).unwrap();
    let json2 = serde_json::to_string_pretty(&second.dat).unwrap();
    assert_eq!(
        json1, json2,
        "repeated parses of the same DAT must produce byte-identical JSON"
    );
}

// --- RUSTSEC-2026-0194: duplicate attribute names ------------------------

#[test]
fn a_start_tag_with_many_duplicate_attribute_names_completes_promptly() {
    // quick-xml 0.37 checked a start tag for duplicate attribute names in
    // quadratic time (RUSTSEC-2026-0194), so a DAT carrying one enormous tag was
    // a denial of service against `dat inspect`. A DAT file is attacker-supplied
    // by definition, and this parser reads attributes from every start tag.
    //
    // The assertion is on the *outcome*, not on a stopwatch: the parse must
    // finish and return a controlled result rather than run away or panic. The
    // wall-clock bound is deliberately loose - it is there to fail a genuine
    // regression to quadratic behaviour, not to measure the machine.
    let mut attributes = String::new();
    for index in 0..20_000 {
        // Repeated names, which is the case the advisory is about.
        attributes.push_str(&format!(r#" dup="{index}""#));
    }
    let xml = format!(
        r#"<datafile><game name="G"><rom name="a.bin" size="1" crc="aabbccdd"{attributes}/></game></datafile>"#
    );
    let (_d, path) = write(&xml);

    let started = std::time::Instant::now();
    let result = parse_logiqx(&path, DatLimits::default());
    let elapsed = started.elapsed();

    // A controlled result either way: parsed, or refused with a real error.
    match &result {
        Ok(outcome) => {
            assert_eq!(outcome.dat.games.len(), 1);
            assert_eq!(outcome.dat.games[0].roms.len(), 1);
            // The attributes this parser cares about are still read correctly.
            let rom = &outcome.dat.games[0].roms[0];
            assert_eq!(rom.name, "a.bin");
            assert_eq!(rom.crc32.as_deref(), Some("aabbccdd"));
        }
        Err(error) => {
            // A refusal is acceptable; a panic or a hang is not.
            let _ = error.to_string();
        }
    }
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "parsing 20,000 duplicate attributes took {elapsed:?}, which suggests the \
         quadratic duplicate-name check is back"
    );
}

#[test]
fn duplicate_attribute_names_do_not_corrupt_the_parsed_rom() {
    // The narrower correctness question behind the same input: a repeated
    // attribute must not change what the parser reads for the ones it uses.
    let xml = r#"<datafile><game name="G"><rom name="a.bin" size="7" crc="aabbccdd" dup="1" dup="2" dup="3"/></game></datafile>"#;
    let (_d, path) = write(xml);
    match parse_logiqx(&path, DatLimits::default()) {
        Ok(outcome) => {
            let rom = &outcome.dat.games[0].roms[0];
            assert_eq!(rom.name, "a.bin");
            assert_eq!(rom.size_bytes, Some(7));
            assert_eq!(rom.crc32.as_deref(), Some("aabbccdd"));
        }
        Err(error) => {
            // Refusing a malformed document is also a controlled outcome.
            assert!(!error.to_string().is_empty());
        }
    }
}

#[test]
fn an_attribute_that_is_not_valid_utf8_is_reported_not_silently_replaced() {
    // Decoding lossily before unescaping would swap the bad bytes for U+FFFD and
    // then unescape cleanly, so a corrupted identifier reached the catalogue with
    // nothing to notice. Text nodes already warned; attributes must too.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.dat");
    let mut bytes = br#"<datafile><game name="G"><rom name="bad"#.to_vec();
    bytes.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8 inside the ROM name
    bytes.extend_from_slice(br#".bin" size="1" crc="aabbccdd"/></game></datafile>"#);
    std::fs::write(&path, &bytes).unwrap();

    let out = parse_logiqx(&path, DatLimits::default()).expect("parsed");
    assert!(
        out.warnings
            .iter()
            .any(|w| w.to_string().contains("not valid UTF-8")),
        "invalid UTF-8 in an attribute must be reported, got {:?}",
        out.warnings
    );
    // Still parsed, still usable - the point is that the loss is visible.
    assert_eq!(out.dat.games[0].roms.len(), 1);
}

#[test]
fn a_valid_utf8_attribute_produces_no_encoding_warning() {
    let xml = r#"<datafile><game name="Café"><rom name="café.bin" size="1" crc="aabbccdd"/></game></datafile>"#;
    let out = parse(xml).expect("parsed");
    assert_eq!(out.dat.games[0].name, "Café");
    assert!(out.warnings.is_empty(), "got {:?}", out.warnings);
}
