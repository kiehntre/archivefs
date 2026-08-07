//! Tests for the DAT source registry, its persistence, and the read-only audit.
//!
//! # What these tests never touch
//!
//! Every path used here is inside a `tempfile::TempDir`. Nothing reads the real
//! `HOME` (the one path-resolution test injects a home through the seam
//! [`super::config::dat_sources_config_path_in`], exactly as the cheat-source
//! config tests do, so parallel tests that do read `HOME` are unaffected).
//! Nothing reads a real ROM or DAT collection, and nothing here opens a socket:
//! the code under test has no network surface at all.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tempfile::TempDir;

use super::audit_run::{DatAuditError, DatAuditRequest, run_dat_audit};
use super::config::{
    DatSourceConfigEntry, DatSourcesConfig, dat_sources_config_path_in,
    load_dat_sources_config_from, save_dat_sources_config_to,
};
use super::validation::{DatFileOutcome, sniff_dat_format};
use super::*;
use crate::dat::limits::DatLimits;
use crate::dat::model::DatFormat;
use crate::dat::parser::DiagnosticSeverity;
use crate::dat::parsers::parse_dat_file;
use crate::safe_read::TrustedRoots;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const LOGIQX: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header>
        <name>Test No-Intro Collection</name>
        <version>2026-01-01</version>
        <author>No-Intro</author>
    </header>
    <game name="Super Game (World)">
        <rom name="super.bin" size="4" crc="0c7e7fd8" md5="098f6bcd4621d373cade4e832627b4f6" sha1="a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"/>
    </game>
</datafile>"#;

const CLRMAMEPRO: &str = "clrmamepro (\n\tname \"Test TOSEC Set\"\n\tversion 1.0\n)\n\
                          game (\n\tname \"Other Game\"\n\trom ( name other.bin size 1024 crc deadbeef )\n)\n";

/// The bytes whose MD5/SHA-1 appear in [`LOGIQX`] (the digests of `"test"`).
const SUPER_BIN_CONTENTS: &[u8] = b"test";

fn temp() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write fixture");
    path
}

/// A registry entry for `path`, named after it, as the GUI would build one.
fn entry_for(path: &Path, kind: DatSourceKind) -> DatSourceEntry {
    let registry = DatSourceRegistry::new();
    DatSourceEntry::new(
        registry.suggest_id(path),
        suggest_display_name(path),
        path.to_path_buf(),
        kind,
    )
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

// ---------------------------------------------------------------------------
// Registering sources
// ---------------------------------------------------------------------------

#[test]
fn a_valid_dat_file_can_be_registered() {
    let dir = temp();
    let path = write(dir.path(), "no-intro-nes.dat", LOGIQX);

    let mut registry = DatSourceRegistry::new();
    registry
        .add(entry_for(&path, DatSourceKind::File))
        .expect("a real Logiqx file registers");

    assert_eq!(registry.len(), 1);
    let entry = &registry.entries()[0];
    assert_eq!(entry.id, "no-intro-nes");
    assert_eq!(entry.display_name, "no-intro-nes.dat");
    assert_eq!(entry.kind, DatSourceKind::File);
    assert!(entry.enabled, "a file the user just picked starts enabled");
    assert_eq!(entry.priority, DEFAULT_DAT_PRIORITY);
    assert_eq!(
        entry.health.state(),
        DatHealthState::NotChecked,
        "registering must not claim a health it has not checked"
    );
}

#[test]
fn a_valid_dat_folder_can_be_registered_and_finds_only_dat_files() {
    let dir = temp();
    let folder = dir.path().join("dats");
    std::fs::create_dir(&folder).unwrap();
    write(&folder, "nointro.dat", LOGIQX);
    write(&folder, "tosec.dat", CLRMAMEPRO);
    // Two files an extension-only rule would sweep up.
    write(&folder, "notes.xml", "<notes><item>shopping</item></notes>");
    write(&folder, "readme.txt", "these are my dats");

    let mut registry = DatSourceRegistry::new();
    registry
        .add(entry_for(&folder, DatSourceKind::Folder))
        .expect("a folder of DATs registers");

    let report = validate_dat_source(&registry.entries()[0], DatLimits::default());
    assert_eq!(report.state, DatHealthState::Valid, "{report:?}");
    let names: Vec<&str> = report
        .files
        .iter()
        .map(|file| file.file_name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["nointro.dat", "tosec.dat"],
        "only real DAT files, in name order"
    );
    assert!(
        report.skipped.iter().any(|s| s.file_name == "notes.xml"),
        "an unrelated XML must be reported as skipped, not silently imported: {:?}",
        report.skipped
    );
    assert!(
        !report.skipped.iter().any(|s| s.file_name == "readme.txt"),
        "a file with no DAT extension is not even a candidate"
    );
}

#[test]
fn folder_discovery_is_deterministic() {
    let dir = temp();
    let folder = dir.path().join("dats");
    std::fs::create_dir(&folder).unwrap();
    for name in ["zulu.dat", "alpha.dat", "mike.dat"] {
        write(&folder, name, LOGIQX);
    }
    let first = discover_dat_files(&folder).unwrap().files;
    let second = discover_dat_files(&folder).unwrap().files;
    assert_eq!(first, second);
    let names: Vec<String> = first
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["alpha.dat", "mike.dat", "zulu.dat"]);
}

#[test]
fn registering_the_same_id_twice_is_refused() {
    let dir = temp();
    let first = write(dir.path(), "collection.dat", LOGIQX);
    let second = write(dir.path(), "collection.xml", LOGIQX);

    let mut registry = DatSourceRegistry::new();
    registry
        .add(entry_for(&first, DatSourceKind::File))
        .unwrap();

    // Same ID, different path: the registry must refuse rather than leave the
    // first entry present but unreachable by ID.
    let mut clashing = entry_for(&second, DatSourceKind::File);
    clashing.id = "collection".to_string();
    let error = registry
        .add(clashing)
        .expect_err("a duplicate ID must be refused");
    assert_eq!(
        error,
        DatRegistryError::DuplicateId {
            id: "collection".to_string()
        }
    );
    assert_eq!(registry.len(), 1);
}

#[test]
fn registering_the_same_path_twice_is_refused() {
    let dir = temp();
    let path = write(dir.path(), "collection.dat", LOGIQX);

    let mut registry = DatSourceRegistry::new();
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();

    let mut again = entry_for(&path, DatSourceKind::File);
    again.id = "a-different-id".to_string();
    let error = registry
        .add(again)
        .expect_err("registering one path twice must be refused");
    assert!(
        matches!(error, DatRegistryError::DuplicatePath { .. }),
        "got {error:?}"
    );
}

#[test]
fn suggest_id_avoids_a_taken_one() {
    let dir = temp();
    let path = write(dir.path(), "collection.dat", LOGIQX);
    let mut registry = DatSourceRegistry::new();
    assert_eq!(registry.suggest_id(&path), "collection");
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    assert_eq!(registry.suggest_id(&path), "collection-2");
}

#[test]
fn an_unusable_source_id_is_refused() {
    for bad in [
        "",
        ".",
        "..",
        ".hidden",
        "trailing.",
        "has/slash",
        "has space",
    ] {
        assert!(
            validate_source_id(bad).is_err(),
            "'{bad}' must not be accepted as a source ID"
        );
    }
    for good in ["no-intro", "redump.ps2", "a_b_c", "X1"] {
        assert!(validate_source_id(good).is_ok(), "'{good}' should be fine");
    }
    assert!(
        validate_source_id(&"a".repeat(MAX_SOURCE_ID_BYTES + 1)).is_err(),
        "an over-long ID must be refused"
    );
}

// ---------------------------------------------------------------------------
// Path policy
// ---------------------------------------------------------------------------

#[test]
fn a_file_source_pointed_at_a_directory_is_refused() {
    let dir = temp();
    let folder = dir.path().join("dats");
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(
        validate_dat_path(&folder, DatSourceKind::File),
        Err(DatPathRefusal::ExpectedFileFoundDirectory)
    );
}

#[test]
fn a_folder_source_pointed_at_a_file_is_refused() {
    let dir = temp();
    let path = write(dir.path(), "one.dat", LOGIQX);
    assert_eq!(
        validate_dat_path(&path, DatSourceKind::Folder),
        Err(DatPathRefusal::ExpectedDirectoryFoundFile)
    );
}

#[test]
fn a_relative_or_traversing_path_is_refused() {
    assert_eq!(
        validate_dat_path(Path::new("relative/thing.dat"), DatSourceKind::File),
        Err(DatPathRefusal::NotAbsolute)
    );
    assert_eq!(
        validate_dat_path(Path::new("/tmp/../etc/passwd"), DatSourceKind::File),
        Err(DatPathRefusal::NonNormalComponent)
    );
}

#[test]
fn the_filesystem_root_cannot_be_registered() {
    assert_eq!(
        validate_dat_path(Path::new("/"), DatSourceKind::Folder),
        Err(DatPathRefusal::FilesystemRoot)
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_dat_path_is_refused() {
    // A registered path is re-read unattended on every validation and audit, so
    // it must not be able to change what it points at after the user approved
    // it. This is the difference between a registered path and one typed on the
    // command line.
    let dir = temp();
    let real = write(dir.path(), "real.dat", LOGIQX);
    let link = dir.path().join("link.dat");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    match validate_dat_path(&link, DatSourceKind::File) {
        Err(DatPathRefusal::SymlinkInPath(component)) => assert_eq!(component, link),
        other => panic!("a symlinked DAT path must be refused, got {other:?}"),
    }

    let mut registry = DatSourceRegistry::new();
    let error = registry
        .add(entry_for(&link, DatSourceKind::File))
        .expect_err("the registry must apply the same policy");
    assert!(matches!(error, DatRegistryError::Path { .. }), "{error:?}");
}

#[cfg(unix)]
#[test]
fn a_symlinked_parent_directory_is_refused() {
    let dir = temp();
    let real_folder = dir.path().join("real");
    std::fs::create_dir(&real_folder).unwrap();
    let path = write(&real_folder, "one.dat", LOGIQX);
    let linked_folder = dir.path().join("linked");
    std::os::unix::fs::symlink(&real_folder, &linked_folder).unwrap();

    let through_link = linked_folder.join("one.dat");
    assert!(
        matches!(
            validate_dat_path(&through_link, DatSourceKind::File),
            Err(DatPathRefusal::SymlinkInPath(_))
        ),
        "a symlinked parent redirects the read just as effectively as a symlinked file"
    );
    // The real path is fine, which is what shows the refusal is about the link
    // and not about the file.
    assert!(validate_dat_path(&path, DatSourceKind::File).is_ok());
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_a_dat_folder_is_skipped_with_a_reason() {
    let dir = temp();
    let folder = dir.path().join("dats");
    std::fs::create_dir(&folder).unwrap();
    write(&folder, "real.dat", LOGIQX);
    let outside = write(dir.path(), "outside.dat", LOGIQX);
    std::os::unix::fs::symlink(&outside, folder.join("escape.dat")).unwrap();

    let scan = discover_dat_files(&folder).unwrap();
    let names: Vec<String> = scan
        .files
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["real.dat"],
        "a link out of the folder is not taken"
    );
    let skipped = scan
        .skipped
        .iter()
        .find(|entry| entry.file_name == "escape.dat")
        .expect("the link must be reported, not silently dropped");
    assert!(skipped.reason.contains("symbolic link"), "{skipped:?}");
}

// ---------------------------------------------------------------------------
// Format detection and validation
// ---------------------------------------------------------------------------

#[test]
fn format_detection_names_both_supported_formats() {
    let dir = temp();
    let logiqx = write(dir.path(), "a.dat", LOGIQX);
    let clrmamepro = write(dir.path(), "b.dat", CLRMAMEPRO);

    assert_eq!(sniff_dat_format(&logiqx), Some(DatFormat::Logiqx));
    assert_eq!(sniff_dat_format(&clrmamepro), Some(DatFormat::ClrMamePro));

    let report = validate_dat_source(
        &entry_for(&logiqx, DatSourceKind::File),
        DatLimits::default(),
    );
    assert_eq!(report.formats, vec!["Logiqx XML".to_string()]);
    let report = validate_dat_source(
        &entry_for(&clrmamepro, DatSourceKind::File),
        DatLimits::default(),
    );
    assert_eq!(report.formats, vec!["ClrMamePro".to_string()]);
}

#[test]
fn unsupported_content_is_not_claimed_as_a_dat() {
    let dir = temp();
    // Well-formed XML that is not a DAT. Detection must not claim it, because
    // claiming it would mean reporting entry counts for someone's unrelated
    // document.
    let not_a_dat = write(
        dir.path(),
        "config.xml",
        "<?xml version=\"1.0\"?><settings><a/></settings>",
    );
    assert_eq!(sniff_dat_format(&not_a_dat), None);
    // Nor is an arbitrary text file a ClrMamePro DAT.
    let text = write(
        dir.path(),
        "notes.dat",
        "just some notes\nnothing structured\n",
    );
    assert_eq!(sniff_dat_format(&text), None);
}

#[test]
fn malformed_xml_is_reported_as_invalid_with_an_actionable_error() {
    let dir = temp();
    let path = write(
        dir.path(),
        "broken.dat",
        "<?xml version=\"1.0\"?>\n<datafile>\n<game name=\"X\">\n<rom name=\"a.bin\" crc=\"AAAAAAAA\"\n</game>\n</datafile>",
    );
    let before = std::fs::read(&path).unwrap();

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());
    assert_eq!(report.state, DatHealthState::Invalid);
    assert!(
        !report.summary.is_empty(),
        "an invalid DAT must come with something the user can act on"
    );
    assert!(
        matches!(report.files[0].outcome, DatFileOutcome::Failed { .. }),
        "{:?}",
        report.files[0]
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a failed parse must not have touched the file"
    );
}

#[test]
fn invalid_utf8_does_not_panic_and_is_reported() {
    let dir = temp();
    let path = dir.path().join("bad-encoding.dat");
    // A Logiqx root followed by bytes that are not valid UTF-8.
    let mut bytes = b"<?xml version=\"1.0\"?>\n<datafile>\n<game name=\"".to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe, 0x80, 0x81]);
    bytes.extend_from_slice(b"\">\n<rom name=\"a.bin\" crc=\"AAAAAAAA\"/>\n</game>\n</datafile>");
    std::fs::write(&path, &bytes).unwrap();

    // Sniffing must survive it: the ASCII root is still visible through a lossy
    // decode, so this is recognisably a Logiqx file whatever its body decodes to.
    assert_eq!(sniff_dat_format(&path), Some(DatFormat::Logiqx));

    // And validation must produce a verdict rather than unwinding.
    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());
    assert!(
        report.state.is_checked(),
        "an undecodable DAT still gets a verdict: {report:?}"
    );
    assert!(!report.summary.is_empty());
}

#[test]
fn a_file_above_the_size_limit_is_refused_rather_than_read() {
    let dir = temp();
    let path = write(dir.path(), "big.dat", LOGIQX);
    let tiny = DatLimits::builder().max_file_size(8).build();

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), tiny);
    assert_eq!(report.state, DatHealthState::Invalid);
    assert!(
        report.summary.contains("limit"),
        "the refusal must say it was a limit: {}",
        report.summary
    );
}

#[test]
fn a_large_entry_count_is_handled_and_its_ceiling_is_enforced() {
    let dir = temp();
    let mut xml =
        String::from("<?xml version=\"1.0\"?>\n<datafile>\n<header><name>Big</name></header>\n");
    for index in 0..5_000u32 {
        xml.push_str(&format!(
            "<game name=\"Game {index}\"><rom name=\"g{index}.bin\" size=\"16\" crc=\"{index:08x}\"/></game>\n"
        ));
    }
    xml.push_str("</datafile>\n");
    let path = write(dir.path(), "big.dat", &xml);

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());
    assert_eq!(report.state, DatHealthState::Valid, "{}", report.summary);
    assert_eq!(report.entry_count, 5_000);

    let capped = DatLimits::builder().max_entries(10).build();
    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), capped);
    assert_eq!(
        report.state,
        DatHealthState::Invalid,
        "the entry ceiling must be enforced, not merely declared"
    );
}

#[test]
fn two_dat_files_claiming_one_identity_are_reported_not_resolved() {
    let dir = temp();
    let folder = dir.path().join("dats");
    std::fs::create_dir(&folder).unwrap();
    // Same header name and version, two files. Neither is "the" one, so both
    // are named and neither is discarded.
    write(&folder, "copy-a.dat", LOGIQX);
    write(&folder, "copy-b.dat", LOGIQX);

    let report = validate_dat_source(
        &entry_for(&folder, DatSourceKind::Folder),
        DatLimits::default(),
    );
    assert_eq!(report.duplicate_identities.len(), 1, "{report:?}");
    let duplicate = &report.duplicate_identities[0];
    assert_eq!(duplicate.identity, "Test No-Intro Collection (2026-01-01)");
    assert_eq!(duplicate.file_names, vec!["copy-a.dat", "copy-b.dat"]);
    assert_eq!(
        report.state,
        DatHealthState::ValidWithWarnings,
        "a conflicting identity is worth flagging, not failing"
    );
    assert_eq!(report.files.len(), 2, "both files are still read");
}

#[test]
fn an_empty_folder_is_invalid_and_says_what_it_looked_for() {
    let dir = temp();
    let folder = dir.path().join("empty");
    std::fs::create_dir(&folder).unwrap();
    let report = validate_dat_source(
        &entry_for(&folder, DatSourceKind::Folder),
        DatLimits::default(),
    );
    assert_eq!(report.state, DatHealthState::Invalid);
    assert!(report.summary.contains(".dat"), "{}", report.summary);
}

/// A Logiqx XML DAT carrying the standard DOCTYPE plus `games` entries.
///
/// The DOCTYPE is expected parser behaviour and must be classified as a parser
/// note, never as a warning.
fn logiqx_with_doctype_and_entries(games: usize) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE datafile PUBLIC \"-//Logiqx//DTD ROM Management Datafile//EN\" \
         \"http://www.logiqx.com/Dats/datafile.dtd\">\n\
         <datafile>\n\
         <header><name>Test TOSEC Set</name><version>2026-01-01</version></header>\n",
    );
    for index in 0..games {
        xml.push_str(&format!(
            "<game name=\"Game {index}\"><rom name=\"g{index}.bin\" size=\"16\" crc=\"{index:08x}\"/></game>\n"
        ));
    }
    xml.push_str("</datafile>\n");
    xml
}

#[test]
fn a_doctype_parser_note_keeps_the_source_valid() {
    // Regression for the reported defect: a single TOSEC DAT whose only
    // diagnostic is the DOCTYPE note must be Valid, not Valid-with-warnings.
    let dir = temp();
    let path = write(
        dir.path(),
        "tosec.dat",
        &logiqx_with_doctype_and_entries(1005),
    );

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());
    assert_eq!(report.entry_count, 1005);
    assert_eq!(report.rom_count, 1005);
    assert_eq!(
        report.state,
        DatHealthState::Valid,
        "a parser note must never lower the verdict: {}",
        report.summary
    );
    let outcome = &report.files[0].outcome;
    let DatFileOutcome::Parsed { diagnostics, .. } = outcome else {
        panic!("the TOSEC DAT must parse");
    };
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(
        diagnostics[0].severity,
        DiagnosticSeverity::Note,
        "{diagnostics:?}"
    );
    assert!(
        diagnostics[0].message.contains("DOCTYPE"),
        "{diagnostics:?}"
    );
}

#[test]
fn a_real_warning_produces_valid_with_warnings() {
    // A malformed checksum is dropped and reported: the DAT is still usable but
    // worth investigating, so the verdict is Valid-with-warnings.
    let dir = temp();
    let path = write(
        dir.path(),
        "warn.dat",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile><game name="G"><rom name="a.bin" size="4" crc="not-a-checksum"/></game></datafile>"#,
    );

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());
    assert_eq!(
        report.state,
        DatHealthState::ValidWithWarnings,
        "{}",
        report.summary
    );
    let DatFileOutcome::Parsed { diagnostics, .. } = &report.files[0].outcome else {
        panic!("the DAT must parse");
    };
    assert!(
        diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Warning),
        "{diagnostics:?}"
    );
}

#[test]
fn a_real_parser_failure_is_invalid() {
    let dir = temp();
    let path = write(
        dir.path(),
        "broken.dat",
        "<?xml version=\"1.0\"?><datafile><game",
    );

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());
    assert_eq!(report.state, DatHealthState::Invalid);
    assert!(matches!(
        report.files[0].outcome,
        DatFileOutcome::Failed { .. }
    ));
}

#[test]
fn mixed_warnings_and_notes_produce_valid_with_warnings() {
    // A DOCTYPE (note) plus a dropped checksum (warning) in one file: the
    // warning is what decides, so the verdict is Valid-with-warnings.
    let dir = temp();
    let path = write(
        dir.path(),
        "mixed.dat",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile><game name="G"><rom name="a.bin" size="4" crc="not-a-checksum"/></game></datafile>"#,
    );

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());
    assert_eq!(
        report.state,
        DatHealthState::ValidWithWarnings,
        "{}",
        report.summary
    );
    let DatFileOutcome::Parsed { diagnostics, .. } = &report.files[0].outcome else {
        panic!("the DAT must parse");
    };
    assert!(
        diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Warning),
        "{diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Note),
        "{diagnostics:?}"
    );
}

#[test]
fn mixed_errors_warnings_and_notes_produce_invalid() {
    // A folder where one file fails to parse is Invalid even when the other
    // file parses with warnings and notes.
    let dir = temp();
    let folder = dir.path().join("mixed");
    std::fs::create_dir(&folder).unwrap();
    write(
        &folder,
        "broken.dat",
        "<?xml version=\"1.0\"?><datafile><game",
    );
    write(
        &folder,
        "ok.dat",
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile><game name="G"><rom name="a.bin" size="4" crc="not-a-checksum"/></game></datafile>"#,
    );

    let report = validate_dat_source(
        &entry_for(&folder, DatSourceKind::Folder),
        DatLimits::default(),
    );
    assert_eq!(
        report.state,
        DatHealthState::Invalid,
        "an error in any file makes the whole source invalid"
    );
}

#[test]
fn clrmamepro_diagnostics_carry_their_line() {
    // The ClrMamePro parser records the line of a diagnostic, and the report
    // keeps it, so the GUI can show a real location instead of "unavailable".
    let dir = temp();
    let path = write(
        dir.path(),
        "t.dat",
        "clrmamepro (\n\tname Test\n\tdescription this-description-is-too-long\n)\n\
         game ( name G rom ( name a.bin size 1 crc deadbeef ) )\n",
    );
    let limits = DatLimits::builder().max_description_length(10).build();
    let outcome = parse_dat_file(&path, limits).expect("the DAT parses");
    let warning = outcome
        .warnings
        .iter()
        .find(|w| w.severity() == DiagnosticSeverity::Warning)
        .expect("a truncation warning");
    assert_eq!(warning.code(), "description_truncated");
    assert!(
        warning.line.is_some(),
        "the ClrMamePro parser must record the offending line: {warning:?}"
    );

    let report = validate_dat_source(&entry_for(&path, DatSourceKind::File), limits);
    let DatFileOutcome::Parsed { diagnostics, .. } = &report.files[0].outcome else {
        panic!("the DAT must parse");
    };
    let diagnostic = diagnostics
        .iter()
        .find(|d| d.code == "description_truncated")
        .expect("the code survives into the report");
    assert_eq!(diagnostic.severity, DiagnosticSeverity::Warning);
    assert!(diagnostic.line.is_some(), "{diagnostic:?}");
}

#[test]
fn a_safety_limit_stop_reports_an_honest_total_when_it_knows_one() {
    let dir = temp();
    let folder = dir.path().join("big");
    std::fs::create_dir(&folder).unwrap();
    // More DAT files than one folder source reads: 512 read, but the scan
    // really saw all of them, so the total is known and must be reported.
    let total = validation::MAX_FOLDER_DAT_FILES + 17;
    for index in 0..total {
        write(&folder, &format!("set-{index:04}.dat"), LOGIQX);
    }

    let report = validate_dat_source(
        &entry_for(&folder, DatSourceKind::Folder),
        DatLimits::default(),
    );
    assert!(report.truncated);
    assert_eq!(report.files.len(), validation::MAX_FOLDER_DAT_FILES);
    assert_eq!(report.total_dat_files, Some(total));
    assert!(
        report.summary.contains("only the first were read"),
        "{}",
        report.summary
    );
}

#[test]
fn a_folder_that_fits_reports_its_exact_count() {
    let dir = temp();
    let folder = dir.path().join("small");
    std::fs::create_dir(&folder).unwrap();
    write(&folder, "a.dat", LOGIQX);
    write(&folder, "b.dat", LOGIQX);

    let report = validate_dat_source(
        &entry_for(&folder, DatSourceKind::Folder),
        DatLimits::default(),
    );
    assert!(!report.truncated);
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.total_dat_files, Some(2));
}

#[test]
fn a_source_whose_path_disappeared_is_unreadable_not_invalid() {
    let dir = temp();
    let path = write(dir.path(), "gone.dat", LOGIQX);
    let entry = entry_for(&path, DatSourceKind::File);
    std::fs::remove_file(&path).unwrap();

    let report = validate_dat_source(&entry, DatLimits::default());
    assert_eq!(
        report.state,
        DatHealthState::Unreadable,
        "a missing path is a different problem from a malformed one"
    );
    assert!(report.path_refusal.is_some());
}

#[test]
fn validation_never_modifies_the_dat_file_or_its_folder() {
    let dir = temp();
    let path = write(dir.path(), "readonly.dat", LOGIQX);
    let before = std::fs::read(&path).unwrap();

    let _ = validate_dat_source(&entry_for(&path, DatSourceKind::File), DatLimits::default());

    assert_eq!(std::fs::read(&path).unwrap(), before, "contents changed");
    let siblings: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        siblings,
        vec!["readonly.dat".to_string()],
        "validating must not write anything beside the DAT"
    );
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[test]
fn a_stored_verdict_goes_stale_when_the_file_changes() {
    let dir = temp();
    let path = write(dir.path(), "changing.dat", LOGIQX);
    let entry = entry_for(&path, DatSourceKind::File);
    let report = validate_dat_source(&entry, DatLimits::default());
    let health = report.to_health(&path, DatSourceKind::File);
    assert_eq!(health.state(), DatHealthState::Valid);
    assert!(!health.is_stale_for(&path, DatSourceKind::File));

    // Replace the file with different content of a different length.
    std::fs::write(&path, CLRMAMEPRO).unwrap();
    assert!(
        health.is_stale_for(&path, DatSourceKind::File),
        "a verdict must not keep claiming to describe a file it no longer describes"
    );
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn round_trip(registry: &DatSourceRegistry, dir: &Path) -> DatSourceRegistry {
    let path = dir.join("dat_sources.toml");
    save_dat_sources_config_to(&path, &registry.to_config()).expect("save");
    let config = load_dat_sources_config_from(&path).expect("load");
    let (reloaded, problems) = DatSourceRegistry::from_config(&config);
    assert!(problems.is_empty(), "{problems:?}");
    reloaded
}

#[test]
fn a_missing_or_empty_file_means_no_sources_yet() {
    let dir = temp();
    let absent = load_dat_sources_config_from(dir.path().join("nothing.toml")).unwrap();
    assert_eq!(absent, DatSourcesConfig::default());

    let empty = dir.path().join("empty.toml");
    std::fs::write(&empty, "   \n\t\n").unwrap();
    assert_eq!(
        load_dat_sources_config_from(&empty).unwrap(),
        DatSourcesConfig::default()
    );

    let (registry, problems) = DatSourceRegistry::from_config(&absent);
    assert!(registry.is_empty());
    assert!(problems.is_empty());
}

#[test]
fn a_disabled_source_survives_a_save_and_load() {
    let dir = temp();
    let path = write(dir.path(), "off.dat", LOGIQX);
    let mut registry = DatSourceRegistry::new();
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    registry.get_mut("off").unwrap().enabled = false;

    let reloaded = round_trip(&registry, dir.path());
    let entry = reloaded.get("off").expect("the entry survives");
    assert!(
        !entry.enabled,
        "a disabled source must come back disabled, not silently re-enabled"
    );
    assert_eq!(
        reloaded.sorted_all().len(),
        1,
        "and it must still be listed"
    );
}

#[test]
fn everything_a_user_set_round_trips() {
    let dir = temp();
    let path = write(dir.path(), "full.dat", LOGIQX);
    let mut registry = DatSourceRegistry::new();
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    {
        let entry = registry.get_mut("full").unwrap();
        entry.display_name = "My No-Intro set".to_string();
        entry.platform = Some("NES".to_string());
        entry.origin = Some("added via GUI".to_string());
        entry.priority = 42;
        entry.health =
            validate_dat_source(entry, DatLimits::default()).to_health(&path, DatSourceKind::File);
    }
    let before = registry.get("full").unwrap().clone();

    let reloaded = round_trip(&registry, dir.path());
    assert_eq!(reloaded.get("full"), Some(&before));
}

#[test]
fn an_unresolved_platform_is_kept_and_reported() {
    let dir = temp();
    let path = write(dir.path(), "future.dat", LOGIQX);
    let mut registry = DatSourceRegistry::new();
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    registry.get_mut("future").unwrap().platform = Some("SomePlatformThisBuildLacks".to_string());

    let reloaded = round_trip(&registry, dir.path());
    assert_eq!(
        reloaded.get("future").unwrap().platform.as_deref(),
        Some("SomePlatformThisBuildLacks"),
        "an assignment this build cannot resolve must survive, not be dropped"
    );
    assert!(!reloaded.get("future").unwrap().platform_is_resolved());

    let unresolved = reloaded.unresolved_settings();
    assert_eq!(unresolved.len(), 1, "{unresolved:?}");
    assert_eq!(
        unresolved[0].kind,
        UnresolvedDatSettingKind::UnresolvedPlatform
    );
    assert!(
        unresolved[0]
            .describe()
            .contains("SomePlatformThisBuildLacks")
    );
}

#[test]
fn a_resolvable_platform_round_trips_and_displays() {
    let dir = temp();
    let path = write(dir.path(), "aliased.dat", LOGIQX);
    let mut registry = DatSourceRegistry::new();
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    let canonical = crate::platform::canonical_ids()
        .first()
        .copied()
        .expect("the registry defines platforms");
    registry.get_mut("aliased").unwrap().platform = Some(canonical.to_string());

    let reloaded = round_trip(&registry, dir.path());
    let entry = reloaded.get("aliased").unwrap();
    assert!(entry.platform_is_resolved());
    assert_eq!(
        entry.platform_display().as_deref(),
        Some(crate::platform::display_name_for(canonical))
    );
    assert!(reloaded.unresolved_settings().is_empty());
}

#[test]
fn fields_from_a_newer_build_survive_a_load_edit_save() {
    // The property this file format exists for: this build must be able to edit
    // a document a newer one wrote without deleting what it does not
    // understand. `cheat_sources.toml` cannot do this - it is
    // `deny_unknown_fields` - which is precisely why DAT sources got their own
    // file rather than sharing that one.
    let dir = temp();
    let path = write(dir.path(), "shared.dat", LOGIQX);
    let config_path = dir.path().join("dat_sources.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
a_future_top_level_key = "kept"

[future_top_level_table]
setting = 3

[[sources]]
id = "shared"
display_name = "Shared"
path = "{}"
kind = "file"
enabled = true
a_future_entry_key = 7
"#,
            path.display()
        ),
    )
    .unwrap();

    let config = load_dat_sources_config_from(&config_path).unwrap();
    let (mut registry, problems) = DatSourceRegistry::from_config(&config);
    assert!(problems.is_empty(), "{problems:?}");

    // Edit something entirely unrelated, the way a user would.
    registry.get_mut("shared").unwrap().enabled = false;
    save_dat_sources_config_to(&config_path, &registry.to_config()).unwrap();

    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("a_future_top_level_key"), "{text}");
    assert!(text.contains("future_top_level_table"), "{text}");
    assert!(text.contains("a_future_entry_key"), "{text}");

    let reloaded = load_dat_sources_config_from(&config_path).unwrap();
    assert_eq!(
        reloaded.unknown_fields.get("a_future_top_level_key"),
        config.unknown_fields.get("a_future_top_level_key")
    );
    let (reloaded_registry, _) = DatSourceRegistry::from_config(&reloaded);
    let entry = reloaded_registry.get("shared").unwrap();
    assert!(!entry.enabled, "the edit was applied");
    assert_eq!(
        entry.unknown_fields.get("a_future_entry_key"),
        Some(&toml::Value::Integer(7)),
        "and the unknown key was kept"
    );

    // It is also *reported*, so the user is not left wondering.
    let unresolved = reloaded_registry.unresolved_settings();
    assert!(
        unresolved
            .iter()
            .any(|item| item.kind == UnresolvedDatSettingKind::UnknownFields
                && item.detail.contains("a_future_entry_key")),
        "{unresolved:?}"
    );
}

#[test]
fn a_second_save_of_reloaded_content_is_byte_identical() {
    // The written form must be a fixed point, or every save would rewrite the
    // user's file and "did anything change?" would stop being answerable.
    let dir = temp();
    let path = write(dir.path(), "stable.dat", LOGIQX);
    let mut registry = DatSourceRegistry::new();
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    registry.get_mut("stable").unwrap().platform = Some("NES".to_string());

    let config_path = dir.path().join("dat_sources.toml");
    save_dat_sources_config_to(&config_path, &registry.to_config()).unwrap();
    let first = std::fs::read_to_string(&config_path).unwrap();

    let reloaded = load_dat_sources_config_from(&config_path).unwrap();
    let (reloaded, _) = DatSourceRegistry::from_config(&reloaded);
    save_dat_sources_config_to(&config_path, &reloaded.to_config()).unwrap();
    let second = std::fs::read_to_string(&config_path).unwrap();

    assert_eq!(first, second, "saving reloaded content must not drift");
}

/// A config entry with only the required fields set.
fn bare_config_entry(id: &str, path: &str) -> DatSourceConfigEntry {
    DatSourceConfigEntry {
        id: id.to_string(),
        display_name: id.to_string(),
        path: path.to_string(),
        kind: DatSourceKind::File,
        enabled: None,
        priority: None,
        platform: None,
        origin: None,
        added_unix_seconds: None,
        health_state: None,
        health_last_validated_unix_seconds: None,
        health_detail: None,
        health_entry_count: None,
        health_rom_count: None,
        health_file_count: None,
        health_formats: None,
        health_observed_size_bytes: None,
        health_observed_modified_unix_seconds: None,
        unknown_fields: toml::Table::new(),
    }
}

#[test]
fn an_entry_with_an_unusable_id_is_reported_rather_than_silently_dropped() {
    let config = DatSourcesConfig {
        sources: Some(vec![bare_config_entry("../escape", "/tmp/x.dat")]),
        unknown_fields: toml::Table::new(),
    };
    let (registry, problems) = DatSourceRegistry::from_config(&config);
    assert!(registry.is_empty());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("../escape"));
}

#[test]
fn a_second_entry_claiming_one_id_is_reported_rather_than_shadowing_the_first() {
    let config = DatSourcesConfig {
        sources: Some(vec![
            bare_config_entry("x", "/tmp/first.dat"),
            bare_config_entry("x", "/tmp/second.dat"),
        ]),
        unknown_fields: toml::Table::new(),
    };
    let (registry, problems) = DatSourceRegistry::from_config(&config);
    assert_eq!(registry.len(), 1);
    assert_eq!(
        registry.get("x").unwrap().path,
        PathBuf::from("/tmp/first.dat")
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn a_hand_edited_priority_is_clamped_into_range() {
    let mut entry = bare_config_entry("x", "/tmp/x.dat");
    entry.priority = Some(0);
    let (registry, _) = DatSourceRegistry::from_config(&DatSourcesConfig {
        sources: Some(vec![entry]),
        unknown_fields: toml::Table::new(),
    });
    assert_eq!(registry.get("x").unwrap().priority, MIN_DAT_PRIORITY);
}

#[test]
fn a_failed_save_leaves_the_previous_file_intact() {
    let dir = temp();
    let path = write(dir.path(), "a.dat", LOGIQX);
    let mut registry = DatSourceRegistry::new();
    registry.add(entry_for(&path, DatSourceKind::File)).unwrap();

    let good = dir.path().join("dat_sources.toml");
    save_dat_sources_config_to(&good, &registry.to_config()).unwrap();
    let before = std::fs::read_to_string(&good).unwrap();

    // A directory cannot be renamed over, so this save must fail.
    let blocked = dir.path().join("a-directory");
    std::fs::create_dir(&blocked).unwrap();
    assert!(save_dat_sources_config_to(&blocked, &registry.to_config()).is_err());

    assert_eq!(std::fs::read_to_string(&good).unwrap(), before);
}

#[test]
fn a_save_leaves_no_temporary_file_behind() {
    let dir = temp();
    let target = dir.path().join("config").join("dat_sources.toml");
    save_dat_sources_config_to(&target, &DatSourcesConfig::default()).unwrap();
    let strays: Vec<String> = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "dat_sources.toml")
        .collect();
    assert!(strays.is_empty(), "left behind: {strays:?}");
}

#[test]
fn the_registry_path_is_the_documented_one_and_needs_a_home() {
    // Injected through the seam so this never depends on, or disturbs, the real
    // HOME - other tests in this crate read it concurrently.
    let path = dat_sources_config_path_in(Some(std::ffi::OsString::from("/home/example")))
        .expect("a home resolves");
    assert_eq!(
        path,
        PathBuf::from("/home/example/.config/archivefs/dat_sources.toml")
    );
    assert!(
        dat_sources_config_path_in(None).is_err(),
        "an absent HOME must not resolve to a relative path"
    );
}

#[test]
fn the_dat_registry_is_a_different_file_from_the_cheat_preferences() {
    let dat = dat_sources_config_path_in(Some(std::ffi::OsString::from("/home/example"))).unwrap();
    assert!(dat.to_string_lossy().ends_with("dat_sources.toml"));
    if let Ok(cheat) = crate::patch_manager::default_cheat_sources_config_path() {
        assert_ne!(
            dat.file_name(),
            cheat.file_name(),
            "DAT and cheat preferences must not share a file"
        );
    }
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

#[test]
fn removing_a_source_removes_the_registry_entry_and_nothing_else() {
    let dir = temp();
    let folder = dir.path().join("dats");
    std::fs::create_dir(&folder).unwrap();
    let dat = write(&folder, "keep.dat", LOGIQX);
    let rom = write(&folder, "not-a-dat.bin", "pretend ROM bytes");
    let dat_before = std::fs::read(&dat).unwrap();
    let rom_before = std::fs::read(&rom).unwrap();

    let mut registry = DatSourceRegistry::new();
    registry
        .add(entry_for(&folder, DatSourceKind::Folder))
        .unwrap();
    let removed = registry.remove("dats").expect("the entry is removed");

    assert_eq!(removed.path, folder);
    assert!(registry.is_empty());
    assert!(
        dat.exists(),
        "removing a source must not delete the DAT file"
    );
    assert!(rom.exists(), "nor anything else in the folder");
    assert!(folder.exists(), "nor the folder itself");
    assert_eq!(std::fs::read(&dat).unwrap(), dat_before);
    assert_eq!(std::fs::read(&rom).unwrap(), rom_before);

    // And saving afterwards writes a registry without it, still touching only
    // the preferences file.
    let reloaded = round_trip(&registry, dir.path());
    assert!(reloaded.is_empty());
    assert!(dat.exists());
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

#[test]
fn sources_are_ordered_by_priority_then_id_and_disabled_ones_keep_their_place() {
    let dir = temp();
    let mut registry = DatSourceRegistry::new();
    for name in ["bravo", "alpha", "charlie"] {
        let path = write(dir.path(), &format!("{name}.dat"), LOGIQX);
        registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    }
    let ids = |registry: &DatSourceRegistry| -> Vec<String> {
        registry
            .sorted_all()
            .iter()
            .map(|entry| entry.id.clone())
            .collect()
    };
    let before = ids(&registry);
    assert_eq!(before, vec!["alpha", "bravo", "charlie"]);

    registry.get_mut("alpha").unwrap().enabled = false;
    assert_eq!(
        before,
        ids(&registry),
        "disabling must not reorder the listing"
    );

    registry.get_mut("charlie").unwrap().priority = 1;
    let ordered = ids(&registry);
    assert_eq!(ordered[0], "charlie", "lower priority is consulted first");
}

#[test]
fn platform_relevance_excludes_disabled_and_other_platforms() {
    let dir = temp();
    let mut registry = DatSourceRegistry::new();
    for name in ["nes-set", "ps2-set", "unassigned"] {
        let path = write(dir.path(), &format!("{name}.dat"), LOGIQX);
        registry.add(entry_for(&path, DatSourceKind::File)).unwrap();
    }
    registry.get_mut("nes-set").unwrap().platform = Some("NES".to_string());
    registry.get_mut("ps2-set").unwrap().platform = Some("PS2".to_string());

    let nes: Vec<&str> = registry
        .sorted_enabled_for_platform("NES")
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(nes.contains(&"nes-set"));
    assert!(
        nes.contains(&"unassigned"),
        "an unassigned source is relevant everywhere"
    );
    assert!(!nes.contains(&"ps2-set"));

    registry.get_mut("nes-set").unwrap().enabled = false;
    let nes: Vec<&str> = registry
        .sorted_enabled_for_platform("NES")
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    assert!(!nes.contains(&"nes-set"));
}

// ---------------------------------------------------------------------------
// Read-only audit
// ---------------------------------------------------------------------------

/// A ROM folder holding one file the catalogue knows and one it does not.
fn audit_fixture() -> (TempDir, DatAuditRequest) {
    let dir = temp();
    let dat = write(dir.path(), "collection.dat", LOGIQX);
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("super.bin"), SUPER_BIN_CONTENTS).unwrap();
    std::fs::write(roms.join("mystery.bin"), b"not in any catalogue").unwrap();

    let request = DatAuditRequest {
        source_id: "collection".to_string(),
        source_display_name: "Test No-Intro Collection".to_string(),
        dat_path: dat,
        dat_kind: DatSourceKind::File,
        scan_root: roms,
        limits: DatLimits::default(),
    };
    (dir, request)
}

/// A recursive listing of `(relative path, contents)`, for proving nothing
/// changed.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(directory) = queue.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                queue.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                out.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn an_audit_finds_an_exact_match_and_reports_what_is_absent() {
    let (_dir, request) = audit_fixture();
    let cancel = no_cancel();
    let outcome =
        run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {}).expect("the audit runs");

    assert_eq!(outcome.report.summary.total, 2);
    assert_eq!(
        outcome.report.summary.exact, 1,
        "the file whose hashes are in the DAT must match exactly: {:?}",
        outcome.report.entries
    );
    assert_eq!(
        outcome.report.summary.not_in_dat, 1,
        "and the one that is not must say so"
    );
    assert!(outcome.unhashed.is_empty());
    assert_eq!(outcome.files_scanned, 2);
    assert!(!outcome.truncated);

    // Provenance travels with the result.
    assert_eq!(outcome.source_id, "collection");
    assert_eq!(
        outcome.catalogue_names,
        vec!["Test No-Intro Collection".to_string()]
    );
    assert!(outcome.headline().contains("Test No-Intro Collection"));
}

#[test]
fn an_audit_makes_no_change_to_the_files_it_reads() {
    // The guarantee the whole feature rests on: an audit is a read. Not a
    // rename, not a move, not a repair, and not a report written beside the
    // ROMs.
    let (dir, request) = audit_fixture();
    let before_roms = snapshot(&request.scan_root);
    let before_all = snapshot(dir.path());

    let cancel = no_cancel();
    run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {}).expect("the audit runs");

    assert_eq!(
        snapshot(&request.scan_root),
        before_roms,
        "an audit changed the ROM folder"
    );
    assert_eq!(
        snapshot(dir.path()),
        before_all,
        "an audit created, removed or altered something"
    );
}

#[test]
fn an_audit_can_be_cancelled_before_it_starts() {
    let (_dir, request) = audit_fixture();
    let cancel = AtomicBool::new(true);
    let error = run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {})
        .expect_err("a cancelled audit produces no report");
    assert_eq!(error, DatAuditError::Cancelled);
}

#[test]
fn cancelling_partway_stops_the_run() {
    let (_dir, request) = audit_fixture();
    let cancel = AtomicBool::new(false);
    // Cancel as soon as the catalogue is ready, i.e. before any file is hashed.
    let error = run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|progress| {
        if matches!(
            progress,
            super::audit_run::DatAuditProgress::CatalogueReady { .. }
        ) {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .expect_err("cancelling mid-run must stop it");
    assert_eq!(error, DatAuditError::Cancelled);
}

#[test]
fn an_audit_reports_progress_in_order() {
    let (_dir, request) = audit_fixture();
    let cancel = no_cancel();
    let seen = std::sync::Mutex::new(Vec::new());
    run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|progress| {
        seen.lock().unwrap().push(progress);
    })
    .expect("the audit runs");

    let seen = seen.into_inner().unwrap();
    use super::audit_run::DatAuditProgress as P;
    assert!(matches!(seen.first(), Some(P::ReadingCatalogue { .. })));
    assert!(seen.iter().any(|p| matches!(p, P::CatalogueReady { .. })));
    assert!(seen.iter().any(|p| matches!(p, P::Hashing { .. })));
    assert!(matches!(seen.last(), Some(P::Comparing { .. })));
}

#[test]
fn an_audit_against_a_folder_source_merges_its_catalogues() {
    let dir = temp();
    let dats = dir.path().join("dats");
    std::fs::create_dir(&dats).unwrap();
    write(&dats, "a.dat", LOGIQX);
    write(&dats, "b.dat", CLRMAMEPRO);
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("super.bin"), SUPER_BIN_CONTENTS).unwrap();

    let request = DatAuditRequest {
        source_id: "dats".to_string(),
        source_display_name: "Folder".to_string(),
        dat_path: dats,
        dat_kind: DatSourceKind::Folder,
        scan_root: roms,
        limits: DatLimits::default(),
    };
    let cancel = no_cancel();
    let outcome =
        run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {}).expect("the audit runs");
    assert_eq!(outcome.catalogue_names.len(), 2);
    assert_eq!(outcome.report.summary.exact, 1);
}

#[test]
fn an_audit_of_an_empty_folder_says_so_rather_than_reporting_all_clear() {
    let dir = temp();
    let dat = write(dir.path(), "collection.dat", LOGIQX);
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();

    let request = DatAuditRequest {
        source_id: "collection".to_string(),
        source_display_name: "Collection".to_string(),
        dat_path: dat,
        dat_kind: DatSourceKind::File,
        scan_root: empty,
        limits: DatLimits::default(),
    };
    let cancel = no_cancel();
    let error = run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {})
        .expect_err("an empty folder is reported, not silently 'all clear'");
    assert!(
        matches!(error, DatAuditError::NothingToAudit(_)),
        "{error:?}"
    );
}

#[test]
fn an_audit_against_an_unparseable_catalogue_fails_with_a_reason() {
    let dir = temp();
    let dat = write(
        dir.path(),
        "broken.dat",
        "<?xml version=\"1.0\"?><datafile><game",
    );
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("a.bin"), b"x").unwrap();

    let request = DatAuditRequest {
        source_id: "broken".to_string(),
        source_display_name: "Broken".to_string(),
        dat_path: dat,
        dat_kind: DatSourceKind::File,
        scan_root: roms,
        limits: DatLimits::default(),
    };
    let cancel = no_cancel();
    let error = run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {})
        .expect_err("a catalogue that does not parse cannot produce verdicts");
    assert!(matches!(error, DatAuditError::NoCatalogue(_)), "{error:?}");
}

#[test]
fn an_audit_descends_into_subfolders_within_its_depth_limit() {
    let dir = temp();
    let dat = write(dir.path(), "collection.dat", LOGIQX);
    let roms = dir.path().join("roms");
    let nested = roms.join("nes").join("licensed");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("super.bin"), SUPER_BIN_CONTENTS).unwrap();

    let request = DatAuditRequest {
        source_id: "collection".to_string(),
        source_display_name: "Collection".to_string(),
        dat_path: dat,
        dat_kind: DatSourceKind::File,
        scan_root: roms,
        limits: DatLimits::default(),
    };
    let cancel = no_cancel();
    let outcome =
        run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {}).expect("the audit runs");
    assert_eq!(outcome.report.summary.exact, 1);
}

#[cfg(unix)]
#[test]
fn an_audit_does_not_follow_a_symlinked_rom_without_trusted_roots() {
    let dir = temp();
    let dat = write(dir.path(), "collection.dat", LOGIQX);
    let outside = dir.path().join("outside.bin");
    std::fs::write(&outside, SUPER_BIN_CONTENTS).unwrap();
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::os::unix::fs::symlink(&outside, roms.join("super.bin")).unwrap();

    let request = DatAuditRequest {
        source_id: "collection".to_string(),
        source_display_name: "Collection".to_string(),
        dat_path: dat,
        dat_kind: DatSourceKind::File,
        scan_root: roms,
        limits: DatLimits::default(),
    };
    let cancel = no_cancel();
    let outcome = run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {})
        .expect("the audit still completes");

    // The link is not followed, so no hash evidence exists for it - and the
    // report says exactly that rather than reporting a match it did not earn.
    assert_eq!(outcome.report.summary.exact, 0);
    assert_eq!(outcome.unhashed.len(), 1, "{:?}", outcome.unhashed);
    assert!(
        outcome.unhashed[0].detail.contains("symlink"),
        "{:?}",
        outcome.unhashed[0]
    );
}

#[test]
fn hashing_is_what_turns_a_name_match_into_an_exact_one() {
    // With hash evidence the verdict is `Exact`; `unhashed` is the only place a
    // name-only result can come from, and it is empty here.
    let (_dir, request) = audit_fixture();
    let cancel = no_cancel();
    let outcome =
        run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {}).expect("the audit runs");
    assert_eq!(outcome.report.summary.exact, 1);
    assert_eq!(outcome.report.summary.filename_only, 0);
    assert!(outcome.unhashed.is_empty());
    assert!(outcome.bytes_hashed > 0);
}

#[test]
fn an_audit_refuses_a_relative_scan_root() {
    let (_dir, mut request) = audit_fixture();
    request.scan_root = PathBuf::from("roms");
    let cancel = no_cancel();
    let error = run_dat_audit(&request, &TrustedRoots::none(), &cancel, &|_| {})
        .expect_err("a relative scan root must be refused");
    assert!(matches!(error, DatAuditError::ScanPath(_)), "{error:?}");
}
