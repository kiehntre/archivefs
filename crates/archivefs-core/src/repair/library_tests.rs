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
    ApplySavedPlanError, ApplySavedPlanSelectedError, LibraryRepairPlan, LibraryScanRequest,
    RepairProfile, apply_library_repair_plan, apply_saved_plan, apply_saved_plan_selected,
    plan_file_from_scan, run_library_scan,
};
use crate::repair::plan::{PlanConflict, PlanConflictKind};
use crate::repair::proposal::{RepairAction, RepairProposalId, SafetyState};
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

/// SHA-1 of `b"xyz"` (3 bytes).
const SHA1_XYZ: &str = "66b27417d37e024c46526c2f6d358a754fc552f3";

/// A three-game DAT + three wrongly-named loose ROMs, so a scan of it
/// produces exactly three independent, non-conflicting Safe proposals.
fn three_proposal_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = write(
        dir,
        "three.dat",
        format!(
            r#"<datafile><header><name>Three</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
<game name="Gamma"><rom name="gamma.bin" size="3" sha1="{SHA1_XYZ}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "a.bin", b"test");
    write(&roms, "b.bin", b"abc");
    write(&roms, "c.bin", b"xyz");
    (dat, roms)
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

// ---------------------------------------------------------------------------
// apply_saved_plan_selected: safe selected-proposal apply.
// ---------------------------------------------------------------------------

/// Finds the fresh (not saved-JSON) proposal id whose source basename matches,
/// by re-running the same scan the trust boundary itself re-runs. Tests never
/// hand-guess proposal ids: they resolve them exactly as a caller must.
fn proposal_id_for(dat: &Path, roms: &Path, source_basename: &str) -> RepairProposalId {
    let outcome = scan(dat, roms);
    outcome
        .repair_plan
        .proposals
        .iter()
        .find(|p| p.source_path.file_name().unwrap() == source_basename)
        .expect("a proposal for the given source exists")
        .id
        .clone()
}

// 1. selecting one known proposal: only that fresh proposal executes.
#[test]
fn selecting_one_proposal_executes_only_that_one() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    assert_eq!(plan.safe_repair_count(), 3);

    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    let result = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&beta_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .expect("the selected proposal applies");
    assert_eq!(
        result.rename.expect("a rename batch ran").summary.applied,
        1
    );
    assert!(roms.join("beta.bin").exists());
    // Unselected files are untouched.
    assert!(roms.join("a.bin").exists(), "unselected source untouched");
    assert!(roms.join("c.bin").exists(), "unselected source untouched");
    assert!(!roms.join("alpha.bin").exists());
    assert!(!roms.join("gamma.bin").exists());
}

// 2. selecting multiple known proposals executes exactly those.
#[test]
fn selecting_multiple_proposals_executes_exactly_those() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);

    let alpha_id = proposal_id_for(&dat, &roms, "a.bin");
    let gamma_id = proposal_id_for(&dat, &roms, "c.bin");

    let result = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &[alpha_id, gamma_id],
        &options(dir.path()),
        &no_cancel(),
    )
    .expect("both selected proposals apply");
    assert_eq!(
        result.rename.expect("a rename batch ran").summary.applied,
        2
    );
    assert!(roms.join("alpha.bin").exists());
    assert!(roms.join("gamma.bin").exists());
    // Beta was never selected.
    assert!(roms.join("b.bin").exists(), "unselected source untouched");
    assert!(!roms.join("beta.bin").exists());
}

// 3. an unknown selected id refuses before any mutation.
#[test]
fn an_unknown_selected_id_refuses_before_mutation() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let bogus = RepairProposalId::new("does-not-exist").unwrap();

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &[bogus],
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::InvalidSelection(_)),
        "{error:?}"
    );
    assert!(roms.join("a.bin").exists());
    assert!(roms.join("b.bin").exists());
    assert!(roms.join("c.bin").exists());
}

// 4. an empty selection is refused explicitly.
#[test]
fn an_empty_selection_refuses_explicitly() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &[],
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::InvalidSelection(ref detail) if detail.contains("no proposals were selected")),
        "{error:?}"
    );
    assert!(roms.join("a.bin").exists());
    assert!(roms.join("b.bin").exists());
    assert!(roms.join("c.bin").exists());
}

// 5. a tampered saved proposal still refuses even when it is NOT selected:
//    selection must never weaken the full-plan re-proof.
#[test]
fn a_tampered_unselected_proposal_still_refuses() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let mut plan = saved_plan(&dat, &roms);
    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    // Tamper the "alpha" proposal's destination, but only select "beta".
    for proposal in &mut plan.repair_plan.proposals {
        if proposal.source_path.file_name().unwrap() == "a.bin"
            && let RepairAction::RenamePath { destination } = &mut proposal.action
        {
            *destination = roms.join("attacker.bin");
        }
    }

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&beta_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(roms.join("a.bin").exists(), "nothing was renamed");
    assert!(roms.join("b.bin").exists(), "nothing was renamed");
    assert!(!roms.join("attacker.bin").exists());
    assert!(!roms.join("beta.bin").exists());
}

// 6. a tampered *selected* proposal refuses.
#[test]
fn a_tampered_selected_proposal_refuses() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let mut plan = saved_plan(&dat, &roms);
    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    for proposal in &mut plan.repair_plan.proposals {
        if proposal.source_path.file_name().unwrap() == "b.bin"
            && let RepairAction::RenamePath { destination } = &mut proposal.action
        {
            *destination = roms.join("attacker.bin");
        }
    }

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&beta_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(roms.join("b.bin").exists(), "nothing was renamed");
    assert!(!roms.join("attacker.bin").exists());
}

// 7. a changed root refuses.
#[test]
fn a_changed_root_refuses_selected_apply() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    // A different (but scannable) root: same file layout, different path, so
    // the scan itself succeeds and only the scan-root identity mismatches.
    let other_root = dir.path().join("other-roms");
    std::fs::create_dir(&other_root).unwrap();
    write(&other_root, "a.bin", b"test");
    write(&other_root, "b.bin", b"abc");
    write(&other_root, "c.bin", b"xyz");

    let error = apply_saved_plan_selected(
        &plan,
        &other_root,
        &dat,
        plan.generation,
        std::slice::from_ref(&beta_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(roms.join("b.bin").exists());
}

// 8. a changed DAT/generation refuses.
#[test]
fn a_changed_generation_refuses_selected_apply() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation.wrapping_add(1),
        std::slice::from_ref(&beta_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            ApplySavedPlanSelectedError::Execute(RepairExecutionError::StalePlan { .. })
        ),
        "{error:?}"
    );
    assert!(roms.join("b.bin").exists());
}

// 9. a stale source identity refuses.
#[test]
fn a_stale_selected_source_identity_refuses() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    // Mutate the live source after the plan was saved but before apply.
    std::fs::write(roms.join("b.bin"), b"changed-content-same-name").unwrap();

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&beta_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    // The fresh re-scan no longer classifies "b.bin" as the same Safe repair
    // (its content, and therefore its DAT match, changed), so the saved plan
    // is no longer reproducible.
    assert!(
        matches!(error, ApplySavedPlanSelectedError::NotAuthorized(_)),
        "{error:?}"
    );
}

// 10. duplicate/ambiguous selected ids fail closed.
#[test]
fn duplicate_selected_ids_fail_closed() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &[beta_id.clone(), beta_id],
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::InvalidSelection(_)),
        "{error:?}"
    );
    assert!(roms.join("b.bin").exists());
}

// 11. subset execution still uses the normal transaction + reverify path.
#[test]
fn subset_execution_uses_the_normal_transaction_and_reverify_path() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let beta_id = proposal_id_for(&dat, &roms, "b.bin");

    let journal_dir = dir.path().join("journal");
    let result = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&beta_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .expect("the selected proposal applies");
    let result = result.rename.expect("a rename batch ran");

    // A real journaled transaction was written.
    let journal_entries = std::fs::read_dir(&journal_dir)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(journal_entries, 1, "a journal file was written");

    // Reverify ran and confirmed the applied destination.
    assert_eq!(result.reverify.len(), 1);
    assert_eq!(result.reverify[0].outcome, RepairReverifyOutcome::Verified);
    assert_eq!(result.reverify[0].destination_path, roms.join("beta.bin"));
}

// ---------------------------------------------------------------------
// Duplicate-quarantine scan wiring (real library scan -> real quarantine
// planner). See `crate::repair::duplicate_scan` for the bridge itself;
// these tests exercise it only through the real, public `run_library_scan`
// entry point, exactly as a caller would.
// ---------------------------------------------------------------------

/// A DAT declaring one game/rom `canon.bin`, plus a loose file at the
/// canonical name and root (the survivor) and a second, identically-sized
/// wrongly-named loose file in a *different* directory with the same bytes
/// (a redundant duplicate that also needs an in-place DAT rename).
fn overlapping_duplicate_and_rename_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = write(
        dir,
        "overlap.dat",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header><name>Overlap</name></header>
    <game name="Game">
        <rom name="canon.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "canon.bin", b"test");
    let subdir = roms.join("subdir");
    std::fs::create_dir(&subdir).unwrap();
    write(&subdir, "wrong.bin", b"test");
    (dat, roms)
}

// H. overlapping/duplicate source conflict -> fail closed.
//
// `subdir/wrong.bin` is simultaneously: (a) a Suggested DAT rename source
// (it must become `subdir/canon.bin` in place) and (b) the redundant member
// of a duplicate group whose survivor is `roms/canon.bin` (so it also gets a
// quarantine MovePath proposal). One source, two proposed actions: the
// existing global conflict detector must fail this closed, exactly as it
// already does for any other duplicate-source case.
#[test]
fn a_source_that_is_both_a_rename_and_a_quarantine_target_fails_closed() {
    let dir = temp();
    let (dat, roms) = overlapping_duplicate_and_rename_fixture(dir.path());

    let outcome = scan(&dat, &roms);

    // Both proposals for `subdir/wrong.bin` are present in the plan...
    let wrong = roms.join("subdir").join("wrong.bin");
    let touching_wrong: Vec<_> = outcome
        .repair_plan
        .proposals
        .iter()
        .filter(|p| p.source_path == wrong)
        .collect();
    assert_eq!(
        touching_wrong.len(),
        2,
        "both the rename and the quarantine proposal must be present: {touching_wrong:?}"
    );

    // ...and the plan fails closed with a DuplicateSource conflict.
    assert!(outcome.repair_plan.has_conflicts());
    assert!(
        outcome
            .repair_plan
            .conflicts
            .iter()
            .any(|c| c.kind == PlanConflictKind::DuplicateSource),
        "{:?}",
        outcome.repair_plan.conflicts
    );
    assert!(!outcome.repair_plan.all_executable());
}

// I. ordinary DAT rename planning remains unchanged by this wiring.
#[test]
fn ordinary_dat_rename_planning_is_unaffected_by_duplicate_wiring() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());

    let outcome = scan(&dat, &roms);

    // Same three independent renames as before this slice existed - no
    // duplicate group exists in this fixture (three distinct games), so no
    // quarantine proposal is added and the plan is unchanged.
    assert_eq!(outcome.repair_plan.proposals.len(), 3);
    assert!(
        outcome
            .repair_plan
            .proposals
            .iter()
            .all(|p| matches!(p.action, RepairAction::RenamePath { .. }))
    );
    assert!(!outcome.repair_plan.has_conflicts());
    assert!(outcome.repair_plan.all_executable());
    assert_eq!(outcome.report.counts.safe_repairs, 3);
    assert_eq!(outcome.report.counts.duplicate_groups_examined, 0);
    assert_eq!(outcome.report.counts.duplicate_quarantine_safe, 0);
}

/// A DAT declaring one game/rom `canon.bin`; a library containing the
/// canonical keeper, a byte-identical redundant copy under a different
/// name, and one unrelated file that shares nothing with the DAT.
fn realistic_duplicate_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = write(
        dir,
        "realistic.dat",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header><name>Realistic</name></header>
    <game name="Game">
        <rom name="canon.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    write(&roms, "canon.bin", b"test");
    write(&roms, "redundant-copy.bin", b"test");
    write(&roms, "unrelated.txt", b"not part of the dat at all");
    (dat, roms)
}

// 12. Realistic, deterministic, end-to-end integration fixture: the actual
// library planner discovers the duplicate group through the real scan path,
// produces exactly one Safe quarantine proposal, leaves the unrelated file
// untouched, and mutates nothing.
#[test]
fn the_real_library_planner_discovers_and_plans_one_duplicate_group() {
    let dir = temp();
    let (dat, roms) = realistic_duplicate_fixture(dir.path());
    let keeper = roms.join("canon.bin");
    let duplicate = roms.join("redundant-copy.bin");
    let unrelated = roms.join("unrelated.txt");
    let unrelated_before = std::fs::read(&unrelated).unwrap();
    let keeper_before = std::fs::read(&keeper).unwrap();
    let duplicate_before = std::fs::read(&duplicate).unwrap();

    let outcome = scan(&dat, &roms);

    // Exactly one quarantine proposal, sourced from the redundant copy, never
    // the keeper.
    let quarantine_proposals: Vec<_> = outcome
        .repair_plan
        .proposals
        .iter()
        .filter(|p| matches!(p.action, RepairAction::MovePath { .. }))
        .collect();
    assert_eq!(quarantine_proposals.len(), 1, "{quarantine_proposals:?}");
    assert_eq!(quarantine_proposals[0].source_path, duplicate);
    assert_ne!(quarantine_proposals[0].source_path, keeper);
    assert!(quarantine_proposals[0].actionable());
    assert_eq!(quarantine_proposals[0].safety, SafetyState::Safe);

    assert_eq!(outcome.report.counts.duplicate_groups_examined, 1);
    assert_eq!(outcome.report.counts.duplicate_groups_content_proven, 1);
    assert_eq!(outcome.report.counts.duplicate_quarantine_safe, 1);
    assert_eq!(outcome.report.counts.duplicate_quarantine_needs_review, 0);

    assert!(!outcome.repair_plan.has_conflicts());
    assert!(outcome.repair_plan.all_executable());

    // No proposal at all touches the unrelated file.
    assert!(
        outcome
            .repair_plan
            .proposals
            .iter()
            .all(|p| p.source_path != unrelated)
    );

    // J. Planning performs no filesystem mutation and creates no
    // `.emuwiz-quarantine` directory.
    assert_eq!(std::fs::read(&keeper).unwrap(), keeper_before);
    assert_eq!(std::fs::read(&duplicate).unwrap(), duplicate_before);
    assert_eq!(std::fs::read(&unrelated).unwrap(), unrelated_before);
    assert!(keeper.exists());
    assert!(duplicate.exists());
    assert!(unrelated.exists());
    assert!(!roms.join(".emuwiz-quarantine").exists());
}

// ---------------------------------------------------------------------
// Duplicate-quarantine selected apply: quarantine-specific backend, live
// re-proof, and mixed rename+quarantine selections.
// ---------------------------------------------------------------------

/// Finds the (unique, in these fixtures) quarantine `MovePath` proposal's id
/// in a saved plan.
fn quarantine_proposal_id(plan: &LibraryRepairPlan) -> RepairProposalId {
    plan.repair_plan
        .proposals
        .iter()
        .find(|p| p.survivor_path.is_some())
        .expect("a quarantine proposal exists")
        .id
        .clone()
}

// C. a selected Safe quarantine proposal applies through the
// quarantine-specific backend (`build_quarantine_transaction` /
// `apply_quarantine_transaction`), never the generic repair executor.
#[test]
fn a_selected_quarantine_proposal_applies_through_the_quarantine_backend() {
    let dir = temp();
    let (dat, roms) = realistic_duplicate_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let quarantine_id = quarantine_proposal_id(&plan);

    let result = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&quarantine_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .expect("the selected quarantine proposal applies");

    assert!(result.rename.is_none(), "no rename proposal was selected");
    assert_eq!(result.quarantine.len(), 1);
    let quarantine = &result.quarantine[0];
    assert_eq!(quarantine.survivor_path, roms.join("canon.bin"));
    assert_eq!(quarantine.result.summary.applied, 1);
    assert_eq!(quarantine.result.summary.failed, 0);

    // The generic executor requires a `MovePath` destination directory to
    // already exist (see `execute::validate_action`); `.emuwiz-quarantine`
    // did not exist before this call, so its existence now is direct
    // evidence the quarantine-specific backend (which creates it) ran, not
    // the generic one (which would have refused outright).
    assert!(roms.join(".emuwiz-quarantine").exists());
    assert!(
        !roms.join("redundant-copy.bin").exists(),
        "the duplicate moved out of its original location"
    );
    assert!(roms.join("canon.bin").exists(), "the survivor is untouched");
    assert_eq!(std::fs::read(roms.join("canon.bin")).unwrap(), b"test");
}

// D/E. a survivor that changed between scan and apply is caught by the fresh
// re-scan/re-proof before any mutation - mirrors
// `a_stale_selected_source_identity_refuses` for the quarantine survivor
// instead of an ordinary rename source.
#[test]
fn a_changed_survivor_refuses_the_selected_quarantine_apply() {
    let dir = temp();
    let (dat, roms) = realistic_duplicate_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    let quarantine_id = quarantine_proposal_id(&plan);

    // Mutate the survivor's content after the plan was saved but before
    // apply: its DAT match (and therefore the whole duplicate group) is no
    // longer reproducible by a fresh scan.
    std::fs::write(roms.join("canon.bin"), b"different-content-same-slot").unwrap();

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&quarantine_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(
        roms.join("redundant-copy.bin").exists(),
        "nothing was moved"
    );
    assert!(!roms.join(".emuwiz-quarantine").exists());
}

/// A three-game DAT plus: one ordinary wrongly-named loose ROM (ties to
/// nothing duplicated), and one already-canonical ROM with a byte-identical
/// redundant copy under a different name (a duplicate-quarantine
/// candidate). The two proposals this produces - one `RenamePath`, one
/// `MovePath` - share no source or destination, so they never conflict.
fn mixed_rename_and_duplicate_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = write(
        dir,
        "mixed.dat",
        format!(
            r#"<datafile><header><name>Mixed</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    // Ordinary rename: wrongly-named, no duplicate.
    write(&roms, "a.bin", b"test");
    // Already-canonical survivor plus a redundant duplicate.
    write(&roms, "beta.bin", b"abc");
    write(&roms, "beta-dup.bin", b"abc");
    (dat, roms)
}

// G. a mixed selection (one ordinary rename id, one quarantine id) with no
// overlap between them applies both, each through its own backend, in one
// call.
#[test]
fn a_mixed_non_conflicting_selection_applies_both_backends() {
    let dir = temp();
    let (dat, roms) = mixed_rename_and_duplicate_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);

    let rename_id = plan
        .repair_plan
        .proposals
        .iter()
        .find(|p| matches!(p.action, RepairAction::RenamePath { .. }))
        .expect("the ordinary rename proposal exists")
        .id
        .clone();
    let quarantine_id = quarantine_proposal_id(&plan);

    let result = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &[rename_id, quarantine_id],
        &options(dir.path()),
        &no_cancel(),
    )
    .expect("both non-conflicting proposals apply");

    let rename = result.rename.expect("the rename batch ran");
    assert_eq!(rename.summary.applied, 1);
    assert!(roms.join("alpha.bin").exists(), "the ordinary rename ran");

    assert_eq!(result.quarantine.len(), 1);
    assert_eq!(result.quarantine[0].survivor_path, roms.join("beta.bin"));
    assert_eq!(result.quarantine[0].result.summary.applied, 1);
    assert!(
        !roms.join("beta-dup.bin").exists(),
        "the duplicate moved out of its original location"
    );
    assert!(roms.join("beta.bin").exists(), "the survivor is untouched");
    assert_eq!(std::fs::read(roms.join("beta.bin")).unwrap(), b"abc");
}

// H. a selection drawn from a plan with an unresolved duplicate-source
// conflict (a rename target and a quarantine source sharing one file) fails
// closed - `select_repair_plan_subset` already requires the *whole* fresh
// plan to be conflict-free before any selection is honoured.
#[test]
fn a_selection_from_a_duplicate_source_conflicted_plan_fails_closed() {
    let dir = temp();
    let (dat, roms) = overlapping_duplicate_and_rename_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    assert!(
        plan.repair_plan.has_conflicts(),
        "the fixture must contain the DuplicateSource conflict this test exercises"
    );

    // Any proposal id at all - even one otherwise unrelated to the conflict -
    // must refuse, because the whole plan is not conflict-free.
    let any_id = plan.repair_plan.proposals[0].id.clone();

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&any_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::InvalidSelection(_)),
        "{error:?}"
    );
    assert!(roms.join("canon.bin").exists());
    assert!(roms.join("subdir").join("wrong.bin").exists());
    assert!(!roms.join(".emuwiz-quarantine").exists());
}

// A whole-plan (unselected) apply fails closed rather than mixing backends
// automatically, when the fresh plan contains any duplicate-quarantine
// proposal.
#[test]
fn a_whole_plan_apply_refuses_when_quarantine_proposals_are_present() {
    let dir = temp();
    let (dat, roms) = realistic_duplicate_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);

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
        matches!(
            error,
            ApplySavedPlanError::QuarantineRequiresSelectedApply { count: 1 }
        ),
        "{error:?}"
    );
    assert!(roms.join("redundant-copy.bin").exists(), "nothing moved");
    assert!(!roms.join(".emuwiz-quarantine").exists());
}

// A saved proposal whose action is an ordinary `RenamePath` but whose
// `survivor_path` was tampered to `Some` (as if hand-edited in the saved
// JSON, or corrupted) is rejected by the saved-plan re-proof before
// anything executes: the fresh scan's equivalent proposal has
// `survivor_path == None`, so `re_prove_saved_plan`'s field-by-field
// comparison refuses the mismatch. `is_duplicate_quarantine()` reporting
// `true` for the tampered value is never itself permission to run it.
#[test]
fn a_tampered_rename_with_a_forced_survivor_path_is_rejected_by_reproof() {
    let dir = temp();
    let (dat, roms) = three_proposal_fixture(dir.path());
    let mut plan = saved_plan(&dat, &roms);

    let target = plan
        .repair_plan
        .proposals
        .iter_mut()
        .find(|p| matches!(p.action, RepairAction::RenamePath { .. }))
        .expect("an ordinary rename proposal exists");
    assert!(!target.is_duplicate_quarantine());
    target.survivor_path = Some(roms.join("does-not-matter.bin"));
    assert!(target.is_duplicate_quarantine());
    let tampered_id = target.id.clone();

    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        std::slice::from_ref(&tampered_id),
        &options(dir.path()),
        &no_cancel(),
    )
    .unwrap_err();
    assert!(
        matches!(error, ApplySavedPlanSelectedError::NotAuthorized(_)),
        "{error:?}"
    );
    assert!(roms.join("a.bin").exists(), "nothing was renamed or moved");
    assert!(!roms.join(".emuwiz-quarantine").exists());
}

/// Two independent duplicate-content groups in two subdirectories, each with
/// an already-canonical survivor and one byte-identical redundant copy, so a
/// selection spanning both produces two quarantine transactions (one per
/// survivor), processed in `BTreeMap<PathBuf, _>` (survivor path) order:
/// `groupa` before `groupb`.
fn two_independent_duplicate_groups_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = write(
        dir,
        "two-groups.dat",
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header><name>TwoGroups</name></header>
    <game name="GameA">
        <rom name="canon-a.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
    <game name="GameB">
        <rom name="canon-b.bin" size="3" sha1="{SHA1_ABC}"/>
    </game>
</datafile>"#
        )
        .as_bytes(),
    );
    let roms = dir.join("roms");
    let group_a = roms.join("groupa");
    let group_b = roms.join("groupb");
    std::fs::create_dir_all(&group_a).unwrap();
    std::fs::create_dir_all(&group_b).unwrap();
    write(&group_a, "canon-a.bin", b"test");
    write(&group_a, "redundant-a.bin", b"test");
    write(&group_b, "canon-b.bin", b"abc");
    write(&group_b, "redundant-b.bin", b"abc");
    (dat, roms)
}

// Multi-survivor partial failure: group A (survivor `groupa/canon-a.bin`)
// applies successfully and is durably journaled; group B (survivor
// `groupb/canon-b.bin`) deterministically fails its own apply (a foreign
// file is planted at its exact, precomputed quarantine destination before
// the call, so the shared rename engine's no-clobber preflight refuses it -
// never a race, never a content mutation that would also change what the
// fresh re-scan inside `apply_saved_plan_selected` sees, so the whole-plan
// re-proof still passes and both groups are genuinely selected and
// attempted).
//
// This proves the actual behaviour the hostile review disputed: earlier
// completed group results are NOT silently discarded from the returned
// error - they are carried in `ApplySavedPlanSelectedError::QuarantineApply`'s
// `completed` field (see its doc and the fix in `apply_saved_plan_selected`).
#[test]
fn a_later_quarantine_group_failure_does_not_lose_an_earlier_groups_success() {
    let dir = temp();
    let (dat, roms) = two_independent_duplicate_groups_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);

    let quarantine_ids: Vec<RepairProposalId> = plan
        .repair_plan
        .proposals
        .iter()
        .filter(|p| p.is_duplicate_quarantine())
        .map(|p| p.id.clone())
        .collect();
    assert_eq!(quarantine_ids.len(), 2, "{quarantine_ids:?}");

    let group_b_proposal = plan
        .repair_plan
        .proposals
        .iter()
        .find(|p| p.is_duplicate_quarantine() && p.source_path.ends_with("redundant-b.bin"))
        .expect("group B's quarantine proposal exists");
    let group_b_destination = group_b_proposal
        .destination()
        .expect("a MovePath destination")
        .clone();
    let group_b_bucket = group_b_destination
        .parent()
        .expect("the destination has a content-hash bucket parent")
        .to_path_buf();
    let outside = tempfile::tempdir().unwrap();

    // Make group B's own content-hash bucket directory a symlink out of the
    // trust boundary, before any apply runs. This is deterministic (no
    // race, no content mutation that would also change what the fresh
    // re-scan inside `apply_saved_plan_selected` sees - the destination
    // *file* path still resolves through the symlink to a location that
    // does not exist, so the whole-plan "destination already exists"
    // conflict check does not trip either) and it can only ever affect
    // group B: `apply_quarantine_transaction` refuses outright the instant
    // it finds a symlink where a quarantine directory it needs must be a
    // real directory (see its own `a_symlinked_content_bucket_directory_\
    // refuses_before_any_mutation` unit test in `quarantine::tests`).
    std::fs::create_dir_all(group_b_bucket.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &group_b_bucket).unwrap();

    let opts = options(dir.path());
    let error = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &quarantine_ids,
        &opts,
        &no_cancel(),
    )
    .unwrap_err();

    let completed = match &error {
        ApplySavedPlanSelectedError::QuarantineApply { completed, detail } => {
            assert!(detail.contains("symlink"), "{detail}");
            completed
        }
        other => panic!("expected QuarantineApply carrying the completed groups, got {other:?}"),
    };

    // Group A's result is not lost: it is visible in the error's
    // `completed` field, its transaction is genuinely Applied, and its file
    // actually moved on disk.
    assert_eq!(completed.quarantine.len(), 1, "{completed:?}");
    let group_a_result = &completed.quarantine[0];
    assert_eq!(
        group_a_result.survivor_path,
        roms.join("groupa").join("canon-a.bin")
    );
    assert_eq!(group_a_result.result.summary.applied, 1);
    assert_eq!(group_a_result.result.summary.failed, 0);
    assert_eq!(
        group_a_result.result.transaction.state,
        crate::dat::rename_apply::TransactionState::Applied
    );
    assert!(!roms.join("groupa").join("redundant-a.bin").exists());
    assert!(roms.join("groupa").join("canon-a.bin").exists());

    // Group B never moved anything: its source is untouched, and the
    // symlink that blocked it is untouched too - no unrelated file
    // corruption, and nothing outside the trust boundary was written to.
    assert!(roms.join("groupb").join("redundant-b.bin").exists());
    assert!(roms.join("groupb").join("canon-b.bin").exists());
    assert!(
        !group_b_destination.exists(),
        "nothing was moved into group B's bucket"
    );
    assert_eq!(
        std::fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing was ever written through the symlink outside the trust boundary"
    );

    // Group A's already-journaled transaction remains independently
    // rollbackable, even though the call as a whole returned `Err`.
    let mut group_a_transaction = group_a_result.result.transaction.clone();
    let rollback = crate::dat::rename_apply::rollback_transaction(
        &mut group_a_transaction,
        &opts.journal_dir,
        &no_cancel(),
    )
    .expect("group A's transaction rolls back");
    assert!(
        matches!(
            rollback.result,
            crate::dat::rename_apply::RollbackResult::FullyRolledBack
        ),
        "{:?}",
        rollback.result
    );
    assert!(roms.join("groupa").join("redundant-a.bin").exists());
}

// Full-apply of a mixed plan (at least one Safe ordinary `RenamePath` plus
// at least one Safe duplicate-quarantine proposal) refuses outright, before
// touching anything: no backend ever runs, no journal is written, and no
// `.emuwiz-quarantine` directory is created.
#[test]
fn a_full_apply_of_a_mixed_plan_refuses_before_any_mutation() {
    let dir = temp();
    let (dat, roms) = mixed_rename_and_duplicate_fixture(dir.path());
    let plan = saved_plan(&dat, &roms);
    assert!(
        plan.repair_plan
            .proposals
            .iter()
            .any(|p| matches!(p.action, RepairAction::RenamePath { .. })),
        "the fixture must include an ordinary Safe rename"
    );
    assert!(
        plan.repair_plan
            .proposals
            .iter()
            .any(|p| p.is_duplicate_quarantine()),
        "the fixture must include a Safe duplicate-quarantine proposal"
    );

    let opts = options(dir.path());
    let error =
        apply_saved_plan(&plan, &roms, &dat, plan.generation, &opts, &no_cancel()).unwrap_err();
    assert!(
        matches!(
            error,
            ApplySavedPlanError::QuarantineRequiresSelectedApply { count: 1 }
        ),
        "{error:?}"
    );

    // The rename source is untouched...
    assert!(roms.join("a.bin").exists());
    assert!(!roms.join("alpha.bin").exists());
    // ...and the quarantine source is untouched.
    assert!(roms.join("beta-dup.bin").exists());
    assert!(roms.join("beta.bin").exists());
    assert_eq!(std::fs::read(roms.join("beta.bin")).unwrap(), b"abc");

    // No journal was written for any mutation, and no quarantine directory
    // was created.
    assert_eq!(
        std::fs::read_dir(&opts.journal_dir).unwrap().count(),
        0,
        "refusal happens before any transaction is journaled"
    );
    assert!(!roms.join(".emuwiz-quarantine").exists());
}
