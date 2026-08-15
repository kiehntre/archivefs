//! Whole-library repair planner integration-style tests.
//!
//! Every test runs inside a `tempfile::TempDir`; nothing touches a real ROM
//! library or the real `HOME`. Mutations go through the Repair Center executor,
//! never a direct `std::fs::rename`.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use tempfile::TempDir;

use crate::dat::limits::DatLimits;
use crate::dat::rename_apply::identity::capture_identity;
use crate::dat::sources::DatSourceKind;
use crate::repair::execute::{RepairExecutionError, RepairExecutionOptions, RepairReverifyOutcome};
use crate::repair::library::{
    ApplySavedPlanError, LibraryRepairPlan, LibraryScanRequest, RepairProfile,
    apply_library_repair_plan, apply_saved_plan, plan_file_from_scan, run_library_scan,
};
use crate::repair::plan::{PlanConflict, PlanConflictKind};
use crate::repair::proposal::{RepairAction, SafetyState};
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

/// Scans and returns the serialisable plan document (the saved plan).
fn saved_plan(dat: &Path, roms: &Path) -> LibraryRepairPlan {
    plan_file_from_scan(&scan(dat, roms))
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

// ---------------------------------------------------------------------
// Files-encountered / DAT-candidate / ignored-ancillary reporting
// ---------------------------------------------------------------------

/// A library with one loose ROM needing a rename, one already-canonical ZIP,
/// and four ancillary (non-DAT) files across three extensions: 2 png, 1 pdf,
/// 1 txt. Mirrors the real shape that motivated this reporting split - a
/// scraper-managed frontend directory sitting next to the actual dumps.
fn candidate_and_ancillary_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    let dat = write(
        dir,
        "mixed.dat",
        format!(
            r#"<datafile><header><name>Mixed</name></header>
<game name="Super Game (World)"><rom name="super.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Archive Game (World)"><rom name="arch.rom" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();

    // Loose ROM under the wrong name: one safe repair.
    write(&roms, "wrongname.bin", b"test");

    // Already-canonical ZIP: one archive candidate, no proposal.
    let archive = roms.join("Archive Game (World).zip");
    let mut writer = ZipWriter::new(std::fs::File::create(&archive).unwrap());
    writer
        .start_file(
            "arch.rom",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.finish().unwrap();

    // Ancillary, non-DAT files: never referenced by the DAT at all.
    write(&roms, "cover.png", b"\x89PNGfake-cover-bytes");
    write(&roms, "cover2.png", b"\x89PNGanother-cover");
    write(&roms, "manual.pdf", b"%PDF-fake-manual-bytes");
    write(&roms, "info.txt", b"just some notes");

    (dat, roms)
}

// N1. ancillary files contribute to files encountered but not DAT candidates
#[test]
fn ancillary_files_count_toward_files_encountered_but_not_dat_candidates() {
    let dir = temp();
    let (dat, roms) = candidate_and_ancillary_fixture(dir.path());

    let outcome = scan(&dat, &roms);

    // 2 DAT-relevant files (loose rom + zip) + 4 ancillary = 6 walked files.
    assert_eq!(outcome.audit.files_scanned, 6);
    assert_eq!(outcome.report.counts.dat_candidates, 2);
    assert!(
        outcome.report.counts.dat_candidates < outcome.audit.files_scanned,
        "the ancillary files must not inflate the candidate count"
    );
}

// N2. candidate count matches the actual DAT-relevant files
#[test]
fn dat_candidate_count_matches_the_actual_dat_relevant_files() {
    let dir = temp();
    let (dat, roms) = candidate_and_ancillary_fixture(dir.path());

    let outcome = scan(&dat, &roms);

    // One loose rom, one archive - both genuinely DAT-relevant, regardless
    // of the archive's own outer-container bytes never matching anything.
    assert_eq!(outcome.report.counts.dat_candidates, 2);
}

// N3. ignored ancillary count is correct
#[test]
fn ignored_ancillary_count_is_correct() {
    let dir = temp();
    let (dat, roms) = candidate_and_ancillary_fixture(dir.path());

    let outcome = scan(&dat, &roms);

    assert_eq!(outcome.report.counts.ignored_ancillary, 4);
    assert_eq!(
        outcome.audit.files_scanned,
        outcome.report.counts.dat_candidates + outcome.report.counts.ignored_ancillary,
        "every walked file must land in exactly one of the two buckets"
    );
}

// N4. extension breakdown is correct
#[test]
fn ignored_ancillary_extension_breakdown_is_correct() {
    let dir = temp();
    let (dat, roms) = candidate_and_ancillary_fixture(dir.path());

    let outcome = scan(&dat, &roms);

    let breakdown = &outcome.report.ignored_ancillary_by_extension;
    assert_eq!(breakdown.get("png").copied(), Some(2));
    assert_eq!(breakdown.get("pdf").copied(), Some(1));
    assert_eq!(breakdown.get("txt").copied(), Some(1));
    assert_eq!(
        breakdown.len(),
        3,
        "no extra or missing extensions: {breakdown:?}"
    );
    let total: usize = breakdown.values().sum();
    assert_eq!(total, outcome.report.counts.ignored_ancillary);
}

// N5. existing complete/repair/canonical counts are unchanged by ancillary files
#[test]
fn existing_counts_are_unaffected_by_ancillary_files() {
    let dir = temp();
    let (dat, roms) = candidate_and_ancillary_fixture(dir.path());

    let outcome = scan(&dat, &roms);

    // `complete_sets` is archive-scoped (`dat::set::classify_archive_sets`
    // runs per opened archive): only the zip contributes here. The loose
    // rom's completeness shows up through the rename plan's own safe-repair
    // state, not through `audit.sets` - unaffected either way by the
    // ancillary files, which is what this test actually pins.
    assert_eq!(outcome.report.counts.complete_sets, 1);
    assert_eq!(outcome.report.counts.safe_repairs, 1);
    assert_eq!(outcome.report.counts.already_canonical, 1);
    assert_eq!(outcome.report.counts.incomplete_sets, 0);
    assert_eq!(outcome.report.counts.needs_review, 0);
    assert_eq!(outcome.report.counts.blocked_repair, 0);
    assert_eq!(outcome.report.counts.unsupported, 0);
    // The new accounting reconciles exactly with the pre-existing buckets:
    // every DAT candidate is either the safe repair or the already-canonical
    // archive, so nothing is left unaccounted for.
    assert_eq!(outcome.report.counts.unmatched_candidates, 0);
}

// N6. JSON compatibility is preserved for old saved plans
#[test]
fn a_plan_saved_before_the_new_fields_existed_still_deserialises() {
    // The exact shape `ReportCounts`/`LibraryRepairReport` had before this
    // batch - no `dat_candidates`, `ignored_ancillary`,
    // `unmatched_candidates`, or `ignored_ancillary_by_extension` at all.
    let old_counts_json = r#"{
        "complete_sets": 2,
        "incomplete_sets": 0,
        "bad_metadata_sets": 0,
        "needs_review_sets": 0,
        "safe_repairs": 1,
        "already_canonical": 1,
        "needs_review": 0,
        "blocked_repair": 0,
        "unsupported": 0,
        "scan_errors": 0
    }"#;
    let counts: crate::repair::library::ReportCounts =
        serde_json::from_str(old_counts_json).expect("an old ReportCounts document still parses");
    assert_eq!(counts.complete_sets, 2);
    assert_eq!(counts.dat_candidates, 0);
    assert_eq!(counts.ignored_ancillary, 0);
    assert_eq!(counts.unmatched_candidates, 0);

    let old_report_json = r#"{
        "counts": {
            "complete_sets": 0, "incomplete_sets": 0, "bad_metadata_sets": 0,
            "needs_review_sets": 0, "safe_repairs": 0, "already_canonical": 0,
            "needs_review": 0, "blocked_repair": 0, "unsupported": 0, "scan_errors": 0
        },
        "complete_sets": [], "incomplete_sets": [], "bad_metadata_sets": [],
        "needs_review_sets": [], "needs_review": [], "blocked": [],
        "unsupported": [], "scan_errors": []
    }"#;
    let report: crate::repair::library::LibraryRepairReport =
        serde_json::from_str(old_report_json).expect("an old report document still parses");
    assert!(report.ignored_ancillary_by_extension.is_empty());

    // And the new shape round-trips through serde without loss.
    let dir = temp();
    let (dat, roms) = candidate_and_ancillary_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let text = serde_json::to_string(&plan).expect("a new plan serialises");
    let round_tripped: LibraryRepairPlan =
        serde_json::from_str(&text).expect("a new plan deserialises");
    assert_eq!(round_tripped, plan);
}

// N7. no mutation path is involved in computing the new counts
#[test]
fn computing_the_new_counts_never_mutates_the_library() {
    let dir = temp();
    let (dat, roms) = candidate_and_ancillary_fixture(dir.path());
    let mut before: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&roms)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    before.sort_by(|a, b| a.0.cmp(&b.0));

    let outcome = scan(&dat, &roms);
    // Sanity: the new counting logic actually ran (non-trivial counts), not
    // a vacuous pass over an empty directory.
    assert_eq!(outcome.report.counts.ignored_ancillary, 4);

    let mut after: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(&roms)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    after.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(before, after, "the read-only scan must not touch any file");
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

// ---------------------------------------------------------------------------
// Authorization regressions: a saved plan is evidence, never permission.
// ---------------------------------------------------------------------------

// B. a stale independent generation refuses before any mutation.
#[test]
fn a_stale_generation_refuses_apply() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let plan = saved_plan(&dat, &roms);
    let error = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation.wrapping_add(1),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            ApplySavedPlanError::Execute(RepairExecutionError::StalePlan { .. })
        ),
        "{error:?}"
    );
    assert!(roms.join("wrongname.bin").exists(), "nothing was renamed");
}

// C. a tampered destination refuses.
#[test]
fn a_tampered_destination_refuses_apply() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let mut plan = saved_plan(&dat, &roms);
    for proposal in &mut plan.repair_plan.proposals {
        if let RepairAction::RenamePath { destination } = &mut proposal.action {
            *destination = roms.join("attacker.bin");
        }
    }

    let error = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(roms.join("wrongname.bin").exists(), "nothing was renamed");
    assert!(!roms.join("attacker.bin").exists());
}

// D. a tampered source + matching tampered identity refuses.
#[test]
fn a_tampered_source_and_identity_refuses_apply() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");
    // A different file the DAT does not authorize (content differs).
    write(&roms, "other.bin", b"abc");

    let mut plan = saved_plan(&dat, &roms);
    for proposal in &mut plan.repair_plan.proposals {
        proposal.source_path = roms.join("other.bin");
        proposal.expected_source_identity = capture_identity(&roms.join("other.bin")).ok();
    }

    let error = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(
        roms.join("other.bin").exists(),
        "the tampered source was never renamed"
    );
}

// E. a RenamePath -> MovePath tamper refuses.
#[test]
fn a_rename_to_move_tamper_refuses_apply() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let mut plan = saved_plan(&dat, &roms);
    for proposal in &mut plan.repair_plan.proposals {
        if let RepairAction::RenamePath { destination } = &proposal.action {
            proposal.action = RepairAction::MovePath {
                destination: destination.clone(),
            };
        }
    }

    let error = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(roms.join("wrongname.bin").exists(), "nothing was renamed");
}

// F. a tampered scan_root cannot expand the trusted mutation root.
#[test]
fn a_tampered_scan_root_refuses_apply() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let mut plan = saved_plan(&dat, &roms);
    plan.scan_root = "/".to_string();

    let error = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(roms.join("wrongname.bin").exists(), "nothing was renamed");
}

// G. a tampered safety/conflicts field is ignored: the fresh scan is authority.
#[test]
fn tampered_safety_and_conflicts_are_not_authority() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let mut plan = saved_plan(&dat, &roms);
    let first_id = plan.repair_plan.proposals[0].id.clone();
    for proposal in &mut plan.repair_plan.proposals {
        proposal.safety = SafetyState::NeedsReview;
    }
    plan.repair_plan.conflicts.push(PlanConflict {
        kind: PlanConflictKind::UnsupportedProposal,
        detail: "tampered".to_string(),
        proposal_ids: vec![first_id],
    });

    // The saved safety/conflicts are ignored; the fresh scan authorizes and the
    // correct rename still executes.
    let result = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .expect("the fresh scan is authoritative");
    assert_eq!(result.summary.applied, 1);
    assert!(
        roms.join("super.bin").exists(),
        "the canonical rename happened"
    );
}

// H. an untouched saved plan still applies.
#[test]
fn an_untouched_saved_plan_applies() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let plan = saved_plan(&dat, &roms);
    let result = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .expect("the untouched plan applies");
    assert_eq!(result.summary.applied, 1);
    assert!(roms.join("super.bin").exists());
    assert!(!roms.join("wrongname.bin").exists());
}

// I. scan and plan remain read-only.
#[test]
fn plan_and_preview_remain_read_only() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    let source = write(&roms, "wrongname.bin", b"test");
    let before = std::fs::read(&source).unwrap();

    let plan = saved_plan(&dat, &roms);
    let _ = crate::repair::library::preview_library_repair_plan(&plan, plan.generation);

    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert!(source.exists());
    assert!(!roms.join("super.bin").exists());
}

// J. refusal happens before journal creation or filesystem mutation.
#[test]
fn refusal_happens_before_journal_or_mutation() {
    let dir = temp();
    let dat = single_rom_dat(dir.path());
    let roms = dir.path().join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "wrongname.bin", b"test");

    let mut plan = saved_plan(&dat, &roms);
    for proposal in &mut plan.repair_plan.proposals {
        if let RepairAction::RenamePath { destination } = &mut proposal.action {
            *destination = roms.join("attacker.bin");
        }
    }

    let journal_dir = dir.path().join("journal");
    let error = apply_saved_plan(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(matches!(error, ApplySavedPlanError::NotAuthorized(_)));

    // No journal entry was written and nothing was mutated.
    let journal_entries = std::fs::read_dir(&journal_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(journal_entries, 0, "no journal file was written");
    assert!(roms.join("wrongname.bin").exists());
    assert!(!roms.join("attacker.bin").exists());
}
