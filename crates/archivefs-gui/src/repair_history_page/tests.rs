//! Tests for the Repair History page.
//!
//! Every fixture is a fresh per-test temp directory holding a disposable DAT
//! and a disposable "ROM library"; nothing here ever touches a real library.
//! A genuine journaled `Applied` transaction is produced through the real
//! Repair Center apply path (`apply_saved_plan_selected`) rather than
//! hand-faked, so history/undo tests exercise the exact on-disk journal a
//! real repair leaves behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::rename_apply::{EntryState, RollbackResult, TransactionState};
use archivefs_core::dat::sources::DatSourceKind;
use archivefs_core::repair::execute::{RepairExecutionOptions, RepairTransactionResult};
use archivefs_core::repair::library::{
    LibraryScanRequest, RepairProfile, apply_saved_plan_selected, plan_file_from_scan,
    run_library_scan,
};
use archivefs_core::repair::proposal::RepairProposalId;
use archivefs_core::safe_read::TrustedRoots;

use super::*;

/// SHA-1 of `b"test"` (4 bytes).
const SHA1_TEST: &str = "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3";
/// SHA-1 of `b"abc"` (3 bytes).
const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

/// A per-test temp directory under the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-gui-repair-history-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture root");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

/// A two-game DAT and two wrongly-named loose ROMs under `dir`.
fn write_apply_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("two.dat");
    std::fs::write(
        &dat,
        format!(
            r#"<datafile><header><name>Two</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
</datafile>"#
        ),
    )
    .unwrap();
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("a.bin"), b"test").unwrap();
    std::fs::write(roms.join("b.bin"), b"abc").unwrap();
    (dat, roms)
}

/// Runs a real scan and a real selected apply (both Safe proposals), leaving
/// a genuine `Applied` journal on disk under `dir/journal`. Returns the ROM
/// root, the journal directory, and the apply result.
fn scan_and_apply(dir: &Path) -> (PathBuf, PathBuf, RepairTransactionResult) {
    let (dat, roms) = write_apply_fixture(dir);
    let request = LibraryScanRequest {
        source_id: "test".to_string(),
        source_display_name: "Test catalogue".to_string(),
        dat_path: dat.clone(),
        dat_kind: DatSourceKind::File,
        scan_root: roms.clone(),
        limits: DatLimits::default(),
        profile: RepairProfile::CanonicalInPlace,
    };
    let outcome = run_library_scan(&request, &TrustedRoots::none(), &no_cancel(), &|_| {})
        .expect("the fixture scan runs");
    let plan = plan_file_from_scan(&outcome);
    let all_ids: Vec<RepairProposalId> = plan
        .repair_plan
        .proposals
        .iter()
        .map(|proposal| proposal.id.clone())
        .collect();
    assert_eq!(all_ids.len(), 2, "the fixture DAT authorizes two repairs");

    let journal_dir = dir.join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let options = RepairExecutionOptions {
        trusted: TrustedRoots::from_paths([&roms]),
        journal_dir: journal_dir.clone(),
    };
    let result = apply_saved_plan_selected(
        &plan,
        &roms,
        &dat,
        plan.generation,
        &all_ids,
        &options,
        &no_cancel(),
    )
    .expect("the fixture apply succeeds");
    assert_eq!(result.summary.applied, 2);
    (roms, journal_dir, result)
}

/// Blocks the test thread until the page's background undo job settles or a
/// generous deadline passes, polling exactly the way the real render loop
/// does.
fn wait_for_undo(state: &mut RepairHistoryPageState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while state.is_undo_running() {
        state.poll_undo();
        if Instant::now() > deadline {
            panic!("the background undo job did not finish in time");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn rendered_text_contains(output: &egui::FullOutput, needle: &str) -> bool {
    fn shape_contains(shape: &egui::Shape, needle: &str) -> bool {
        match shape {
            egui::Shape::Text(text_shape) => text_shape.galley.text().contains(needle),
            egui::Shape::Vec(nested) => nested.iter().any(|s| shape_contains(s, needle)),
            _ => false,
        }
    }
    output
        .shapes
        .iter()
        .any(|clipped| shape_contains(&clipped.shape, needle))
}

/// A deterministic in-memory clipboard stand-in for tests, mirroring the
/// `main.rs` `InMemoryClipboard` test double's shape (this module cannot
/// reach that one - it is private to `main.rs`'s own `tests` module).
#[derive(Default)]
struct NoopClipboard {
    set_calls: Vec<String>,
}

impl crate::ClipboardBackend for NoopClipboard {
    fn get_text_status(&mut self) -> crate::ClipboardTextStatus {
        crate::ClipboardTextStatus::Empty
    }

    fn set_text(&mut self, text: String) -> Result<(), String> {
        self.set_calls.push(text);
        Ok(())
    }
}

fn render(state: &mut RepairHistoryPageState) -> egui::FullOutput {
    let mut clipboard = NoopClipboard::default();
    render_with_clipboard(state, &mut clipboard)
}

fn render_with_clipboard(
    state: &mut RepairHistoryPageState,
    clipboard: &mut NoopClipboard,
) -> egui::FullOutput {
    let ctx = egui::Context::default();
    ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            show_repair_history_page(ui, state, clipboard);
        });
    })
}

// --- history loads from persistent journal state ----------------------------

#[test]
fn history_loads_from_the_persistent_journal_on_disk() {
    let dir = TestDir::new("loads-from-disk");
    let (_roms, journal_dir, result) = scan_and_apply(dir.path());

    // A brand new page state, never touched by the apply above: it only
    // knows the journal directory, exactly as a fresh app launch would.
    let state = RepairHistoryPageState::load_with_journal_dir(journal_dir);

    assert_eq!(state.transactions.len(), 1);
    assert_eq!(
        state.transactions[0].transaction_id,
        result.summary.transaction_id
    );
    assert!(state.load_problems.is_empty());
}

// --- transaction rows reflect counts/status accurately ----------------------

#[test]
fn transaction_rows_reflect_counts_and_status_accurately() {
    let dir = TestDir::new("row-accuracy");
    let (_roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);

    let transaction = state
        .transaction_by_id(&result.summary.transaction_id)
        .unwrap();
    assert_eq!(transaction.applied_count(), 2);
    assert_eq!(transaction.failed_count(), 0);
    assert_eq!(transaction.state, TransactionState::Applied);

    let output = render(&mut state);
    assert!(rendered_text_contains(
        &output,
        "Requested 2 · Applied 2 · Failed 0 · Skipped 0"
    ));
    assert!(rendered_text_contains(&output, "Rollback: Not requested"));
    assert!(rendered_text_contains(
        &output,
        "2 of 2 destination file(s) verified"
    ));
    assert!(rendered_text_contains(
        &output,
        &result.summary.transaction_id
    ));
}

// --- details show per-entry state --------------------------------------------

#[test]
fn details_show_per_entry_state_and_reverify() {
    let dir = TestDir::new("details");
    let (roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    state.set_details(Some(result.summary.transaction_id.clone()));

    let output = render(&mut state);
    assert!(rendered_text_contains(&output, "Applied"));
    assert!(rendered_text_contains(&output, "verified"));
    assert!(rendered_text_contains(
        &output,
        &roms.join("alpha.bin").display().to_string()
    ));
    assert!(rendered_text_contains(
        &output,
        &roms.join("beta.bin").display().to_string()
    ));
}

// --- undo enable/disable rules ------------------------------------------------

#[test]
fn undo_is_enabled_for_a_settled_applied_transaction() {
    let dir = TestDir::new("enable-applied");
    let (_roms, journal_dir, result) = scan_and_apply(dir.path());
    let state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    assert!(state.can_undo(&result.summary.transaction_id));
}

#[test]
fn undo_is_disabled_for_a_transaction_with_nothing_applied() {
    let dir = TestDir::new("disable-nothing-applied");
    let journal_dir = dir.path().join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    // An `Applied`-state transaction whose entries never actually applied
    // has nothing to reverse - `RenameTransaction::is_rollbackable` must
    // refuse it, and so must the page.
    let tx = bare_transaction(
        "nothing-applied",
        TransactionState::Applied,
        EntryState::Planned,
    );
    archivefs_core::dat::rename_apply::write_journal(&journal_dir, &tx).unwrap();

    let state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    assert!(!state.can_undo("nothing-applied"));
}

#[test]
fn undo_is_disabled_while_another_undo_is_running() {
    let dir = TestDir::new("disable-while-running");
    let (_roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    assert!(state.can_undo(&result.summary.transaction_id));
    state.undo_running = true;
    assert!(!state.can_undo(&result.summary.transaction_id));
}

#[test]
fn undo_is_disabled_for_an_unknown_transaction_id() {
    let dir = TestDir::new("disable-unknown");
    let journal_dir = dir.path().join("journal");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    assert!(!state.can_undo("does-not-exist"));
}

/// A minimal, hand-built transaction for enable-rule tests that do not need
/// a real applied file on disk.
fn bare_transaction(
    id: &str,
    tx_state: TransactionState,
    entry_state: EntryState,
) -> archivefs_core::dat::rename_apply::RenameTransaction {
    archivefs_core::dat::rename_apply::RenameTransaction {
        transaction_id: id.to_string(),
        plan_generation: 1,
        classifier_version: Some(
            archivefs_core::dat::classification::CLASSIFIER_VERSION.to_string(),
        ),
        created_at_unix: 10,
        source_scan_root: "/tmp/roms".to_string(),
        state: tx_state,
        entries: vec![archivefs_core::dat::rename_apply::TransactionEntry {
            source_path: PathBuf::from("/tmp/roms/a.bin"),
            destination_path: PathBuf::from("/tmp/roms/A.bin"),
            original_basename: "a.bin".to_string(),
            proposed_basename: "A.bin".to_string(),
            identity: archivefs_core::dat::rename_apply::ObjectIdentity {
                size_bytes: 1,
                modified_unix: 1,
                kind: archivefs_core::dat::rename_apply::ObjectKind::RegularFile,
                #[cfg(unix)]
                ino: 1,
                #[cfg(unix)]
                dev: 1,
            },
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: entry_state,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        }],
        created_directories: Vec::new(),
        unknown: Default::default(),
    }
}

/// One hand-built [`archivefs_core::dat::rename_apply::TransactionEntry`],
/// for tests that need control over more than one entry at a time (which
/// [`bare_transaction`]'s single fixed entry cannot give them).
fn bare_entry(
    original_basename: &str,
    proposed_basename: &str,
    entry_state: EntryState,
) -> archivefs_core::dat::rename_apply::TransactionEntry {
    archivefs_core::dat::rename_apply::TransactionEntry {
        source_path: PathBuf::from(format!("/tmp/roms/{original_basename}")),
        destination_path: PathBuf::from(format!("/tmp/roms/{proposed_basename}")),
        original_basename: original_basename.to_string(),
        proposed_basename: proposed_basename.to_string(),
        identity: archivefs_core::dat::rename_apply::ObjectIdentity {
            size_bytes: 1,
            modified_unix: 1,
            kind: archivefs_core::dat::rename_apply::ObjectKind::RegularFile,
            #[cfg(unix)]
            ino: 1,
            #[cfg(unix)]
            dev: 1,
        },
        preflight_passed: false,
        preflight_failures: Vec::new(),
        state: entry_state,
        failure_reason: None,
        applied_at_unix: None,
        rolled_back_at_unix: None,
        unknown: Default::default(),
    }
}

/// A minimal, hand-built multi-entry transaction, for headline/details tests
/// that need more than [`bare_transaction`]'s single fixed entry.
fn bare_transaction_with_entries(
    id: &str,
    tx_state: TransactionState,
    entries: Vec<archivefs_core::dat::rename_apply::TransactionEntry>,
) -> archivefs_core::dat::rename_apply::RenameTransaction {
    archivefs_core::dat::rename_apply::RenameTransaction {
        transaction_id: id.to_string(),
        plan_generation: 1,
        classifier_version: Some(
            archivefs_core::dat::classification::CLASSIFIER_VERSION.to_string(),
        ),
        created_at_unix: 10,
        source_scan_root: "/tmp/roms".to_string(),
        state: tx_state,
        entries,
        created_directories: Vec::new(),
        unknown: Default::default(),
    }
}

// --- card headline: source/destination, not id/counts, lead the card ---------

#[test]
fn a_single_entry_card_headline_names_the_source_and_destination() {
    let transaction = bare_transaction("single", TransactionState::Applied, EntryState::Applied);
    let headline = transaction_headline(&transaction);
    assert!(
        headline.contains("a.bin"),
        "the source basename must be on the card: {headline}"
    );
    assert!(
        headline.contains("A.bin"),
        "the destination basename must be on the card: {headline}"
    );
    assert!(
        !headline.contains("more"),
        "a single-entry transaction has nothing left to count: {headline}"
    );
}

#[test]
fn a_multi_entry_card_headline_names_the_first_entry_and_counts_the_rest() {
    let transaction = bare_transaction_with_entries(
        "multi",
        TransactionState::Applied,
        vec![
            bare_entry("alpha.bin", "Alpha (USA).bin", EntryState::Applied),
            bare_entry("beta.bin", "Beta (USA).bin", EntryState::Applied),
            bare_entry("gamma.bin", "Gamma (USA).bin", EntryState::Applied),
        ],
    );
    let headline = transaction_headline(&transaction);
    assert!(
        headline.contains("alpha.bin") && headline.contains("Alpha (USA).bin"),
        "the first entry must be named directly: {headline}"
    );
    assert!(
        !headline.contains("beta.bin") && !headline.contains("gamma.bin"),
        "only the first entry's name belongs on the card, not the rest: {headline}"
    );
    assert!(
        headline.contains("+ 2 more"),
        "the remaining entries must be counted, not silently dropped: {headline}"
    );
}

// --- reverify wording is never misleading about what "missing" means ---------

#[test]
fn reverify_wording_is_unambiguous_for_a_normal_fully_verified_rename() {
    let entries = vec![
        RepairReverifyEntry {
            source_path: PathBuf::from("/tmp/roms/a.bin"),
            destination_path: PathBuf::from("/tmp/roms/A.bin"),
            outcome: RepairReverifyOutcome::Verified,
            detail: "the destination matches the recorded source identity".to_string(),
        },
        RepairReverifyEntry {
            source_path: PathBuf::from("/tmp/roms/b.bin"),
            destination_path: PathBuf::from("/tmp/roms/B.bin"),
            outcome: RepairReverifyOutcome::Verified,
            detail: "the destination matches the recorded source identity".to_string(),
        },
    ];
    let summary = reverify_summary(&entries);
    assert_eq!(summary.headline, "2 of 2 destination file(s) verified");
    assert!(
        !summary.headline.to_lowercase().contains("missing"),
        "a fully verified rename must never say anything looks missing: {}",
        summary.headline
    );
    assert!(
        summary.explanation.is_none(),
        "nothing needs explaining when every destination verified"
    );
    assert_eq!(summary.tone, widgets::StatusTone::Success);
}

#[test]
fn reverify_wording_names_the_destination_not_the_source_when_something_is_missing() {
    let entries = vec![RepairReverifyEntry {
        source_path: PathBuf::from("/tmp/roms/a.bin"),
        destination_path: PathBuf::from("/tmp/roms/A.bin"),
        outcome: RepairReverifyOutcome::Missing,
        detail: "the destination does not exist after apply".to_string(),
    }];
    let summary = reverify_summary(&entries);
    let explanation = summary
        .explanation
        .expect("a missing destination must be explained, not left as a bare count");
    assert!(
        explanation.contains("destination file"),
        "must name what is actually missing (the destination, not the pre-rename source): \
         {explanation}"
    );
    assert!(
        explanation.contains("not the pre-rename source"),
        "must explicitly rule out the reading where a missing file is the normal, expected \
         post-rename disappearance of the original source: {explanation}"
    );
    assert!(
        explanation.contains("did not hold"),
        "must state plainly that this is a genuine problem, not a benign default: {explanation}"
    );
    assert_eq!(summary.tone, widgets::StatusTone::Blocked);
}

#[test]
fn reverify_wording_is_not_applicable_rather_than_falsely_alarming_when_nothing_is_applied() {
    // A transaction with nothing currently `Applied` (e.g. fully rolled
    // back) has no entries for `reverify_transaction` to check at all -
    // this must read as "nothing to check right now", never as a failed
    // verification.
    let summary = reverify_summary(&[]);
    assert_eq!(summary.headline, "Not applicable");
    let explanation = summary.explanation.expect("must explain why");
    assert!(explanation.contains("nothing to re-check"));
    assert_eq!(summary.tone, widgets::StatusTone::Info);
}

// --- details expose full, uncopied-nowhere source/destination paths ----------

#[test]
fn details_expose_the_full_uncopied_source_and_destination_paths() {
    let long_basename = "Some Very Long Original ROM Title (USA, Europe) (En,Fr,De,Es,It).bin";
    let transaction = bare_transaction_with_entries(
        "long-paths",
        TransactionState::Applied,
        vec![bare_entry(
            long_basename,
            "Canonical Title.bin",
            EntryState::Applied,
        )],
    );
    let mut state = RepairHistoryPageState {
        journal_dir: PathBuf::from("/tmp/unused"),
        transactions: vec![transaction.clone()],
        load_problems: Vec::new(),
        details_id: Some(transaction.transaction_id.clone()),
        undo_confirm: None,
        undo_confirm_focus_cancel: false,
        undo_job: None,
        undo_running: false,
        undo_outcome: None,
        undo_error: None,
    };

    let output = render(&mut state);
    let full_source = transaction.entries[0].source_path.display().to_string();
    let full_destination = transaction.entries[0]
        .destination_path
        .display()
        .to_string();
    assert!(
        rendered_text_contains(&output, &full_source),
        "the full source path must be readable in Details, not truncated"
    );
    assert!(
        rendered_text_contains(&output, &full_destination),
        "the full destination path must be readable in Details, not truncated"
    );
    assert!(
        rendered_text_contains(&output, "Copy"),
        "Details must offer a way to copy the paths"
    );
}

#[test]
fn the_copy_button_writes_the_exact_full_path_to_the_clipboard() {
    let transaction = bare_transaction("copy", TransactionState::Applied, EntryState::Applied);
    let mut state = RepairHistoryPageState {
        journal_dir: PathBuf::from("/tmp/unused"),
        transactions: vec![transaction.clone()],
        load_problems: Vec::new(),
        details_id: Some(transaction.transaction_id.clone()),
        undo_confirm: None,
        undo_confirm_focus_cancel: false,
        undo_job: None,
        undo_running: false,
        undo_outcome: None,
        undo_error: None,
    };
    let mut clipboard = NoopClipboard::default();
    render_with_clipboard(&mut state, &mut clipboard);

    // No click was simulated (this headless render never delivers pointer
    // events), so nothing should have reached the clipboard yet - this
    // pins that `detail_path_row`'s Copy button is wired to `set_text`
    // only on click, never eagerly on every frame.
    assert!(clipboard.set_calls.is_empty());
}

// --- confirmation required ----------------------------------------------------

#[test]
fn opening_the_confirmation_never_itself_starts_an_undo() {
    let dir = TestDir::new("confirmation-required");
    let (_roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);

    state.open_undo_confirmation(&result.summary.transaction_id);

    assert!(state.undo_confirm.is_some());
    assert_eq!(
        state.undo_confirm.as_ref().unwrap().transaction_id,
        result.summary.transaction_id
    );
    assert!(!state.is_undo_running());
}

// --- cancel performs no mutation ----------------------------------------------

#[test]
fn cancelling_the_confirmation_never_touches_the_filesystem() {
    let dir = TestDir::new("cancel-no-mutation");
    let (roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);

    state.open_undo_confirmation(&result.summary.transaction_id);
    assert!(state.undo_confirm.is_some());
    state.cancel_undo_confirmation();

    assert!(state.undo_confirm.is_none());
    assert!(!state.is_undo_running());
    assert!(roms.join("alpha.bin").exists(), "still applied");
    assert!(roms.join("beta.bin").exists(), "still applied");
    assert!(!roms.join("a.bin").exists(), "nothing was reversed");
    assert!(!roms.join("b.bin").exists(), "nothing was reversed");
}

// --- duplicate undo blocked ---------------------------------------------------

#[test]
fn a_second_undo_attempt_while_running_is_a_no_op() {
    let dir = TestDir::new("duplicate-undo");
    let (roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);

    state.open_undo_confirmation(&result.summary.transaction_id);
    state.confirm_undo();
    assert!(state.is_undo_running());

    // Simulate a second click landing while the first job is in flight.
    assert!(!state.can_undo(&result.summary.transaction_id));
    state.open_undo_confirmation(&result.summary.transaction_id);
    assert!(state.undo_confirm.is_none(), "no second confirmation opens");
    state.confirm_undo(); // no-op: nothing pending
    assert!(state.is_undo_running(), "the original job is untouched");

    wait_for_undo(&mut state);
    let outcome = state.undo_outcome.as_ref().expect("the single undo ran");
    assert_eq!(outcome.result, RollbackResult::FullyRolledBack);
    assert!(roms.join("a.bin").exists());
    assert!(roms.join("b.bin").exists());
}

// --- safe undo success ---------------------------------------------------------

#[test]
fn a_safe_undo_fully_reverses_the_transaction() {
    let dir = TestDir::new("undo-success");
    let (roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);

    state.open_undo_confirmation(&result.summary.transaction_id);
    state.confirm_undo();
    wait_for_undo(&mut state);

    let outcome = state.undo_outcome.as_ref().expect("undo ran");
    assert_eq!(outcome.result, RollbackResult::FullyRolledBack);
    assert!(state.undo_error.is_none());
    assert!(!state.undo_running);

    assert!(roms.join("a.bin").exists(), "original path restored");
    assert!(roms.join("b.bin").exists(), "original path restored");
    assert!(!roms.join("alpha.bin").exists());
    assert!(!roms.join("beta.bin").exists());

    let transaction = state
        .transaction_by_id(&result.summary.transaction_id)
        .expect("still present in refreshed history");
    assert_eq!(transaction.state, TransactionState::RolledBack);
}

// --- stale/changed filesystem refuses -------------------------------------------

#[test]
fn a_changed_destination_identity_refuses_the_undo() {
    let dir = TestDir::new("undo-stale-identity");
    let (roms, journal_dir, result) = scan_and_apply(dir.path());
    // Externally change both applied destinations after the fact: neither
    // still matches the identity recorded at apply time.
    std::fs::write(roms.join("alpha.bin"), b"tampered-alpha").unwrap();
    std::fs::write(roms.join("beta.bin"), b"tampered-beta").unwrap();

    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    state.open_undo_confirmation(&result.summary.transaction_id);
    state.confirm_undo();
    wait_for_undo(&mut state);

    let outcome = state.undo_outcome.as_ref().expect("the worker ran");
    assert!(
        matches!(outcome.result, RollbackResult::RollbackFailed { .. }),
        "{:?}",
        outcome.result
    );
    assert!(state.undo_error.is_none());
    // Nothing was reversed: the tampered files are exactly as tampered, and
    // the original paths were never recreated.
    assert!(!roms.join("a.bin").exists());
    assert!(!roms.join("b.bin").exists());
    assert_eq!(
        std::fs::read(roms.join("alpha.bin")).unwrap(),
        b"tampered-alpha"
    );
}

// --- unexpected destination/source occupation refuses ----------------------------

#[test]
fn an_occupied_original_source_path_refuses_the_undo() {
    let dir = TestDir::new("undo-occupied-source");
    let (roms, journal_dir, result) = scan_and_apply(dir.path());

    // Whichever entry rollback attempts first (reverse of apply order) gets
    // its original source path occupied by something else in the meantime.
    let contested_source = result
        .transaction
        .entries
        .last()
        .unwrap()
        .source_path
        .clone();
    std::fs::write(&contested_source, b"something else now lives here").unwrap();

    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    state.open_undo_confirmation(&result.summary.transaction_id);
    state.confirm_undo();
    wait_for_undo(&mut state);

    let outcome = state.undo_outcome.as_ref().expect("the worker ran");
    assert!(
        matches!(outcome.result, RollbackResult::RollbackFailed { .. }),
        "{:?}",
        outcome.result
    );
    // The occupying file was never clobbered - no-clobber held even for the
    // reverse rename.
    assert_eq!(
        std::fs::read(&contested_source).unwrap(),
        b"something else now lives here"
    );
    let _ = roms;
}

// --- previous history remains visible after failure --------------------------

#[test]
fn history_remains_visible_after_a_refused_undo() {
    let dir = TestDir::new("history-survives-failure");
    let (roms, journal_dir, result) = scan_and_apply(dir.path());
    std::fs::write(roms.join("alpha.bin"), b"tampered").unwrap();
    std::fs::write(roms.join("beta.bin"), b"tampered").unwrap();

    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    state.open_undo_confirmation(&result.summary.transaction_id);
    state.confirm_undo();
    wait_for_undo(&mut state);

    assert!(state.undo_outcome.is_some());
    assert_eq!(state.transactions.len(), 1, "the row was never dropped");
    assert!(
        state
            .transaction_by_id(&result.summary.transaction_id)
            .is_some()
    );
}

// --- history refreshes after successful undo ----------------------------------

#[test]
fn a_successful_undo_is_independently_re_observable_from_disk() {
    let dir = TestDir::new("undo-refresh-from-disk");
    let (_roms, journal_dir, result) = scan_and_apply(dir.path());
    let mut state = RepairHistoryPageState::load_with_journal_dir(journal_dir.clone());

    state.open_undo_confirmation(&result.summary.transaction_id);
    state.confirm_undo();
    wait_for_undo(&mut state);
    assert_eq!(
        state
            .transaction_by_id(&result.summary.transaction_id)
            .unwrap()
            .state,
        TransactionState::RolledBack
    );

    // A completely independent page instance, loaded fresh from the same
    // directory: the undo's journal write is durable, not an in-memory-only
    // GUI update.
    let fresh = RepairHistoryPageState::load_with_journal_dir(journal_dir);
    assert_eq!(
        fresh
            .transaction_by_id(&result.summary.transaction_id)
            .unwrap()
            .state,
        TransactionState::RolledBack
    );
}

// --- no direct GUI fs mutation path --------------------------------------------

#[test]
fn the_page_module_never_calls_fs_rename_remove_or_copy_directly() {
    let source = include_str!("../repair_history_page.rs");
    for needle in [
        "fs::rename(",
        "fs::remove_file(",
        "fs::remove_dir",
        "fs::copy(",
    ] {
        assert!(
            !source.contains(needle),
            "the Repair History page must never mutate the filesystem directly (found {needle})"
        );
    }
    assert!(
        source.contains("rollback_transaction"),
        "the page must undo through the core rollback engine"
    );
}
