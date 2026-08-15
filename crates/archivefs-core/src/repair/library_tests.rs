//! Whole-library repair planner integration-style tests.
//!
//! Every test runs inside a `tempfile::TempDir`; nothing touches a real ROM
//! library or the real `HOME`. Mutations go through the Repair Center executor,
//! never a direct `std::fs::rename`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tempfile::TempDir;

use crate::dat::limits::DatLimits;
use crate::dat::sources::DatSourceKind;
use crate::repair::execute::{RepairExecutionError, RepairExecutionOptions, RepairReverifyOutcome};
use crate::repair::library::{
    LibraryScanRequest, RepairProfile, apply_library_repair_plan, plan_file_from_scan,
    run_library_scan,
};
use crate::safe_read::TrustedRoots;

/// The SHA-1 of `b"test"` (4 bytes), used across DAT fixtures.
const SHA1_TEST: &str = "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3";
/// The SHA-1 of `b"abc"` (3 bytes).
const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

fn temp() -> TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

fn write(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write fixture");
    path
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

fn request(dat: &Path, roms: &Path) -> LibraryScanRequest {
    LibraryScanRequest {
        source_id: "test".to_string(),
        source_display_name: "Test catalogue".to_string(),
        dat_path: dat.to_path_buf(),
        dat_kind: DatSourceKind::File,
        scan_root: roms.to_path_buf(),
        limits: DatLimits::default(),
        profile: RepairProfile::CanonicalInPlace,
    }
}

fn scan(dat: &Path, roms: &Path) -> crate::repair::library::LibraryScanOutcome {
    run_library_scan(
        &request(dat, roms),
        &TrustedRoots::none(),
        &no_cancel(),
        &|_| {},
    )
    .expect("the scan runs")
}

fn options(dir: &Path) -> RepairExecutionOptions {
    let journal_dir = dir.join("journal");
    std::fs::create_dir_all(&journal_dir).expect("journal dir");
    RepairExecutionOptions {
        trusted: TrustedRoots::from_paths([dir]),
        journal_dir,
    }
}

/// A single-game, single-ROM Logiqx DAT declaring `super.bin` (the bytes of
/// `"test"`), so a loose file with those bytes is an `Exact` match.
fn single_rom_dat(dir: &Path) -> PathBuf {
    write(
        dir,
        "single.dat",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header><name>Single</name></header>
    <game name="Super Game (World)">
        <rom name="super.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
</datafile>"#
        )
        .as_bytes(),
    )
}

// A. canonical loose ROM safe rename
#[test]
fn a_loose_rom_gets_a_safe_canonical_rename() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let outcome = scan(&dat, &roms);

    assert_eq!(outcome.repair_plan.proposals.len(), 1);
    let proposal = &outcome.repair_plan.proposals[0];
    assert_eq!(proposal.source_path, roms.join("wrongname.bin"));
    assert_eq!(proposal.destination(), Some(&roms.join("super.bin")));
    assert!(proposal.actionable());
    assert_eq!(outcome.report.counts.safe_repairs, 1);
}

// B. verified ZIP outer rename
#[test]
fn a_verified_zip_gets_a_safe_outer_rename() {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let dir = temp();
    let dat = write(
        dir.path(),
        "zip.dat",
        format!(
            r#"<datafile><header><name>ZIP</name></header>
<game name="Game (World)"><rom name="game.rom" size="4" sha1="{SHA1_TEST}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let archive = roms.join("collection.zip");
    let mut writer = ZipWriter::new(std::fs::File::create(&archive).unwrap());
    writer
        .start_file(
            "game.rom",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"test").unwrap();
    writer.finish().unwrap();

    let outcome = scan(&dat, &roms);

    assert_eq!(outcome.repair_plan.proposals.len(), 1);
    let proposal = &outcome.repair_plan.proposals[0];
    assert!(proposal.is_outer_archive);
    assert_eq!(proposal.source_path, archive);
    assert_eq!(proposal.destination(), Some(&roms.join("Game (World).zip")));
    assert_eq!(outcome.report.counts.complete_sets, 1);
}

// C. verified 7z outer rename
#[test]
fn a_verified_7z_gets_a_safe_outer_rename() {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    use std::io::Cursor;

    let dir = temp();
    let dat = write(
        dir.path(),
        "sevenz.dat",
        format!(
            r#"<datafile><header><name>7z</name></header>
<game name="Game (World)"><rom name="game.rom" size="4" sha1="{SHA1_TEST}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let archive = roms.join("collection.7z");
    let mut writer = ArchiveWriter::new(std::fs::File::create(&archive).unwrap()).unwrap();
    let mut entry = ArchiveEntry::new();
    entry.name = "game.rom".to_string();
    entry.has_stream = true;
    entry.size = 4;
    writer
        .push_archive_entry(entry, Some(Cursor::new(b"test".to_vec())))
        .unwrap();
    writer.finish().unwrap();

    let outcome = scan(&dat, &roms);

    assert_eq!(outcome.repair_plan.proposals.len(), 1);
    let proposal = &outcome.repair_plan.proposals[0];
    assert!(proposal.is_outer_archive);
    assert_eq!(proposal.source_path, archive);
    assert_eq!(proposal.destination(), Some(&roms.join("Game (World).7z")));
}

// D. a `.rar` stays compatible when no provider exists
#[test]
fn a_rar_file_is_scanned_but_produces_no_safe_repair() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "game.rar", b"not-a-real-rar");

    let outcome = scan(&dat, &roms);

    assert!(outcome.repair_plan.proposals.is_empty());
    assert_eq!(outcome.report.counts.safe_repairs, 0);
}

// E. a CHD never produces an accidental rename
#[test]
fn a_chd_file_produces_no_safe_repair() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "game.chd", b"not-a-real-chd");

    let outcome = scan(&dat, &roms);

    assert!(outcome.repair_plan.proposals.is_empty());
}

// F. ambiguous DAT result -> no Safe repair
#[test]
fn an_ambiguous_dat_result_produces_no_safe_repair() {
    let dir = temp();
    let dat = write(
        dir.path(),
        "ambiguous.dat",
        format!(
            r#"<datafile><header><name>Ambiguous</name></header>
<game name="Game (World)"><rom name="world.rom" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Game (USA)"><rom name="usa.rom" size="4" sha1="{SHA1_TEST}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "whatever.rom", b"test");

    let outcome = scan(&dat, &roms);

    assert!(outcome.repair_plan.proposals.is_empty());
    assert!(outcome.report.counts.needs_review >= 1);
}

// G. incomplete set -> no Safe repair
#[test]
fn an_incomplete_set_produces_no_safe_repair() {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let dir = temp();
    let dat = write(
        dir.path(),
        "two-rom.dat",
        format!(
            r#"<datafile><header><name>Two ROM</name></header>
<game name="Game (World)">
<rom name="game.rom" size="4" sha1="{SHA1_TEST}"/>
<rom name="extra.rom" size="3" sha1="{SHA1_ABC}"/>
</game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let archive = roms.join("collection.zip");
    let mut writer = ZipWriter::new(std::fs::File::create(&archive).unwrap());
    writer
        .start_file(
            "game.rom",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"test").unwrap();
    writer.finish().unwrap();

    let outcome = scan(&dat, &roms);

    assert!(outcome.repair_plan.proposals.is_empty());
    assert_eq!(outcome.report.counts.incomplete_sets, 1);
}

// J. default (read-only) scan never mutates the library
#[test]
fn a_scan_never_mutates_the_library() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let source = write(&roms, "wrongname.bin", b"test");
    let before = std::fs::read(&source).unwrap();

    let _ = scan(&dat, &roms);

    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert!(source.exists());
    assert!(!roms.join("super.bin").exists());
}

// M. partial scan fails closed (unhashed evidence never becomes a safe repair)
#[test]
fn unhashed_evidence_is_surfaced_and_never_becomes_a_safe_repair() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let dangling = roms.join("dangling.rom");
    std::os::unix::fs::symlink(roms.join("gone"), &dangling).unwrap();

    let outcome = scan(&dat, &roms);

    assert!(outcome.repair_plan.proposals.is_empty());
    assert!(
        outcome
            .report
            .scan_errors
            .iter()
            .any(|e| e.contains("dangling.rom")),
        "the unhashable file is surfaced: {:?}",
        outcome.report.scan_errors
    );
}

// H. stale source after plan -> apply refuses
#[test]
fn a_stale_source_refuses_apply() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let source = write(&roms, "wrongname.bin", b"test");

    let outcome = scan(&dat, &roms);
    let plan = plan_file_from_scan(&outcome);
    assert_eq!(plan.safe_repair_count(), 1);

    std::fs::write(&source, b"different size content").unwrap();

    let error =
        apply_library_repair_plan(&plan, plan.generation, &options(dir.path()), &no_cancel())
            .unwrap_err();
    assert!(
        matches!(error, RepairExecutionError::StaleSource { .. }),
        "{error:?}"
    );
    assert!(source.exists());
}

// I. destination created after plan -> apply refuses
#[test]
fn a_created_destination_refuses_apply() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let outcome = scan(&dat, &roms);
    let plan = plan_file_from_scan(&outcome);

    write(&roms, "super.bin", b"someone else");

    let error =
        apply_library_repair_plan(&plan, plan.generation, &options(dir.path()), &no_cancel())
            .unwrap_err();
    assert!(
        matches!(error, RepairExecutionError::NotExecutable { .. }),
        "{error:?}"
    );
    assert!(roms.join("wrongname.bin").exists());
}

// K + L. apply executes one safe rename, then reverify sees the canonical result
#[test]
fn apply_executes_one_safe_rename_and_reverify_confirms_it() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let outcome = scan(&dat, &roms);
    let plan = plan_file_from_scan(&outcome);

    let result =
        apply_library_repair_plan(&plan, plan.generation, &options(dir.path()), &no_cancel())
            .expect("the rename applies");

    assert_eq!(result.summary.applied, 1);
    assert_eq!(result.summary.failed, 0);
    assert!(roms.join("super.bin").exists());
    assert!(!roms.join("wrongname.bin").exists());
    assert!(
        result
            .reverify
            .iter()
            .all(|e| e.outcome == RepairReverifyOutcome::Verified)
    );

    let rescanned = scan(&dat, &roms);
    assert!(rescanned.repair_plan.proposals.is_empty());
    assert_eq!(rescanned.report.counts.already_canonical, 1);
}

// N. JSON plan round trip does not bypass safety
#[test]
fn a_json_plan_round_trip_does_not_bypass_safety() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let outcome = scan(&dat, &roms);
    let plan = plan_file_from_scan(&outcome);

    let json = serde_json::to_string(&plan).unwrap();
    let reparsed: crate::repair::library::LibraryRepairPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(reparsed, plan);

    let mut tampered = reparsed.clone();
    for proposal in &mut tampered.repair_plan.proposals {
        proposal.safety = crate::repair::proposal::SafetyState::NeedsReview;
    }
    let error = apply_library_repair_plan(
        &tampered,
        tampered.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, RepairExecutionError::NotExecutable { .. }),
        "{error:?}"
    );
}

// O. two safe renames batch correctly
#[test]
fn two_safe_renames_batch_correctly() {
    let dir = temp();
    let dat = write(
        dir.path(),
        "two.dat",
        format!(
            r#"<datafile><header><name>Two</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "a.bin", b"test");
    write(&roms, "b.bin", b"abc");

    let outcome = scan(&dat, &roms);
    let plan = plan_file_from_scan(&outcome);
    assert_eq!(plan.safe_repair_count(), 2);

    let result =
        apply_library_repair_plan(&plan, plan.generation, &options(dir.path()), &no_cancel())
            .expect("both renames apply");
    assert_eq!(result.summary.applied, 2);
    assert!(roms.join("alpha.bin").exists());
    assert!(roms.join("beta.bin").exists());
}

// P. a batch conflict refuses the whole transaction
#[test]
fn a_batch_conflict_refuses_the_whole_transaction() {
    let dir = temp();
    let dat = write(
        dir.path(),
        "conflict.dat",
        format!(
            r#"<datafile><header><name>Conflict</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let a = write(&roms, "a.bin", b"test");
    let b = write(&roms, "b.bin", b"abc");

    let outcome = scan(&dat, &roms);
    let plan = plan_file_from_scan(&outcome);
    assert_eq!(plan.safe_repair_count(), 2);

    write(&roms, "beta.bin", b"preexisting");

    let error =
        apply_library_repair_plan(&plan, plan.generation, &options(dir.path()), &no_cancel())
            .unwrap_err();
    assert!(
        matches!(error, RepairExecutionError::NotExecutable { .. }),
        "{error:?}"
    );
    assert!(a.exists());
    assert!(b.exists());
}
