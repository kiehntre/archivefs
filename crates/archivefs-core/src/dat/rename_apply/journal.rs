//! Durable journal persistence for rename transactions.
//!
//! A journal is the source of truth for a rename batch: it is written,
//! durably, **before** the first mutation, and updated durably after every
//! state transition, so a process crash can never lose the record of what was
//! renamed and what was not. Recovery on startup reads these journals; it never
//! resumes anything automatically.
//!
//! # Format and privacy
//!
//! The journal is JSON, serialised from [`RenameTransaction`]. It deliberately
//! contains the full source and destination paths - rollback cannot work
//! without them - and this is the one place those paths are kept verbatim.
//! General History & Logs never sees them. Unknown fields written by a future
//! build round-trip verbatim rather than failing the read. No secrets are ever
//! written here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ArchiveFsError;
use crate::dat::rename_apply::model::{
    EntryState, RenameTransaction, TransactionState,
};

/// The journal directory's name under the ArchiveFS data directory.
pub const RENAME_TRANSACTIONS_DIRECTORY: &str = "rename-transactions";

/// How long a journal filename may grow before it is unusable.
const MAX_JOURNAL_NAME_BYTES: usize = 128;

/// Monotonic per-process sequence for transaction ids and journal names.
static TRANSACTION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The default journal directory: `~/.local/share/archivefs/rename-transactions`.
pub fn default_rename_transaction_dir() -> Result<PathBuf, ArchiveFsError> {
    rename_transaction_dir_in(std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")))
}

/// The logic behind [`default_rename_transaction_dir`], with the home injected
/// so tests can exercise the no-home case without racing on the environment.
pub fn rename_transaction_dir_in(home: Option<OsString>) -> Result<PathBuf, ArchiveFsError> {
    let home = home.ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("archivefs")
        .join(RENAME_TRANSACTIONS_DIRECTORY))
}

/// A stable, unique transaction id: `<unix_seconds>-<sequence>`.
pub fn new_transaction_id(now_unix: u64) -> String {
    let sequence = TRANSACTION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{now_unix}-{sequence}")
}

/// The journal filename for a transaction id, or `None` when the id is not a
/// safe single filename component.
pub fn journal_file_name(transaction_id: &str) -> Option<String> {
    if transaction_id.is_empty()
        || transaction_id.len() > MAX_JOURNAL_NAME_BYTES
        || transaction_id.contains(['/', '\\', '\0'])
        || transaction_id == "."
        || transaction_id == ".."
    {
        return None;
    }
    Some(format!("{transaction_id}.json"))
}

/// The full path of a transaction's journal.
pub fn journal_path(dir: &Path, transaction_id: &str) -> Option<PathBuf> {
    journal_file_name(transaction_id).map(|name| dir.join(name))
}

/// Writes (or rewrites) a transaction journal durably: temp file in the same
/// directory, `sync_all`, atomic rename into place, parent-directory sync.
pub fn write_journal(dir: &Path, transaction: &RenameTransaction) -> Result<(), ArchiveFsError> {
    let name = journal_file_name(&transaction.transaction_id).ok_or_else(|| {
        ArchiveFsError::Config(format!(
            "transaction id '{}' cannot name a journal file",
            transaction.transaction_id
        ))
    })?;
    let path = dir.join(name);
    let body = serde_json::to_string_pretty(transaction).map_err(|error| {
        ArchiveFsError::Config(format!("failed to serialize rename transaction journal: {error}"))
    })?;
    crate::atomic_write_text(&path, &format!("{body}\n"))
}

/// Reads a journal back, tolerating unknown fields written by a future build.
pub fn read_journal(path: &Path) -> Result<RenameTransaction, ArchiveFsError> {
    let text = std::fs::read_to_string(path)
        .map_err(|source| ArchiveFsError::io(path.to_path_buf(), source))?;
    serde_json::from_str(&text).map_err(|error| {
        ArchiveFsError::Config(format!(
            "failed to parse rename transaction journal {}: {error}",
            path.display()
        ))
    })
}

/// Every transaction journal in `dir`, parsed, in a stable order.
///
/// A journal that fails to parse is reported as a problem rather than silently
/// dropped: an unreadable journal may still be the only record of renames that
/// happened, and deleting it would destroy that record.
pub fn list_journals(dir: &Path) -> (Vec<RenameTransaction>, Vec<String>) {
    let mut transactions = Vec::new();
    let mut problems = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return (transactions, problems);
    };
    let mut files: Vec<PathBuf> = read_dir
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    for path in files {
        match read_journal(&path) {
            Ok(transaction) => transactions.push(transaction),
            Err(error) => problems.push(format!("{}: {error}", path.display())),
        }
    }
    transactions.sort_by(|a, b| a.transaction_id.cmp(&b.transaction_id));
    (transactions, problems)
}

/// The transactions in `dir` that an interrupted batch left behind: journals
/// whose overall state is not settled, in a deterministic order.
pub fn find_recovery_transactions(dir: &Path) -> (Vec<RenameTransaction>, Vec<String>) {
    let (all, problems) = list_journals(dir);
    let interrupted: Vec<RenameTransaction> = all
        .into_iter()
        .filter(|transaction| transaction.state.needs_recovery())
        .collect();
    (interrupted, problems)
}

/// Whether a transaction's journal still exists on disk.
pub fn journal_exists(dir: &Path, transaction_id: &str) -> bool {
    journal_path(dir, transaction_id)
        .is_some_and(|path| std::fs::symlink_metadata(&path).is_ok())
}

/// Removes a transaction's journal. Used only after a transaction is fully
/// settled and the user has dismissed it from recovery.
pub fn remove_journal(dir: &Path, transaction_id: &str) -> Result<(), ArchiveFsError> {
    let Some(path) = journal_path(dir, transaction_id) else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ArchiveFsError::io(path, source)),
    }
}

/// Counts applied entries in a state persisted to the journal.
pub fn persisted_applied_count(transaction: &RenameTransaction) -> usize {
    transaction
        .entries
        .iter()
        .filter(|entry| entry.state == EntryState::Applied)
        .count()
}

/// The overall state a transaction should move to after an apply pass.
pub fn terminal_state_after_apply(transaction: &RenameTransaction) -> TransactionState {
    if transaction.failed_count() > 0 {
        TransactionState::ApplyFailed
    } else {
        TransactionState::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::rename_apply::model::{
        ObjectIdentity, ObjectKind, TransactionEntry,
    };
    use std::path::PathBuf;

    fn entry(source: &str, destination: &str) -> TransactionEntry {
        TransactionEntry {
            source_path: PathBuf::from(source),
            destination_path: PathBuf::from(destination),
            original_basename: "old.bin".to_string(),
            proposed_basename: "new.bin".to_string(),
            identity: ObjectIdentity {
                size_bytes: 100,
                modified_unix: 1,
                kind: ObjectKind::RegularFile,
                #[cfg(unix)]
                ino: 7,
                #[cfg(unix)]
                dev: 8,
            },
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: EntryState::Planned,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        }
    }

    fn transaction(state: TransactionState) -> RenameTransaction {
        RenameTransaction {
            transaction_id: "1-0".to_string(),
            plan_generation: 3,
            created_at_unix: 10,
            source_scan_root: "/tmp/roms".to_string(),
            state,
            entries: vec![entry("/tmp/roms/a.bin", "/tmp/roms/A.bin")],
            unknown: Default::default(),
        }
    }

    #[test]
    fn a_journal_round_trips_through_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let tx = transaction(TransactionState::Planned);
        write_journal(dir.path(), &tx).unwrap();
        let loaded = read_journal(&journal_path(dir.path(), "1-0").unwrap()).unwrap();
        assert_eq!(loaded, tx);
    }

    #[test]
    fn unknown_future_fields_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1-0.json");
        std::fs::write(
            &path,
            r#"{
  "transaction_id": "1-0",
  "plan_generation": 3,
  "created_at_unix": 10,
  "source_scan_root": "/tmp/roms",
  "state": "planned",
  "future_transaction_field": 42,
  "entries": [
    {
      "source_path": "/tmp/roms/a.bin",
      "destination_path": "/tmp/roms/A.bin",
      "original_basename": "old.bin",
      "proposed_basename": "new.bin",
      "identity": {"size_bytes": 100, "modified_unix": 1, "kind": "regular_file"},
      "preflight_passed": false,
      "preflight_failures": [],
      "state": "planned",
      "future_entry_field": "kept"
    }
  ]
}
"#,
        )
        .unwrap();
        let loaded = read_journal(&path).unwrap();
        assert_eq!(loaded.state, TransactionState::Planned);
        assert_eq!(loaded.unknown.get("future_transaction_field"), Some(&serde_json::json!(42)));
        assert_eq!(
            loaded.entries[0]
                .unknown
                .get("future_entry_field"),
            Some(&serde_json::json!("kept"))
        );
    }

    #[test]
    fn a_missing_field_defaults_to_planned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        std::fs::write(&path, r#"{"transaction_id":"x","plan_generation":0,"created_at_unix":0,"source_scan_root":"","entries":[]}"#).unwrap();
        let loaded = read_journal(&path).unwrap();
        assert_eq!(loaded.state, TransactionState::Planned);
        assert!(loaded.entries.is_empty());
    }

    #[test]
    fn incomplete_transactions_are_found_and_settled_ones_are_not() {
        let dir = tempfile::tempdir().unwrap();
        for (id, state) in [
            ("interrupted", TransactionState::Applying),
            ("failed", TransactionState::ApplyFailed),
            ("settled", TransactionState::RolledBack),
            ("complete", TransactionState::Applied),
        ] {
            let mut tx = transaction(state);
            tx.transaction_id = id.to_string();
            write_journal(dir.path(), &tx).unwrap();
        }
        let (recovery, problems) = find_recovery_transactions(dir.path());
        assert!(problems.is_empty(), "{problems:?}");
        let ids: Vec<&str> = recovery.iter().map(|tx| tx.transaction_id.as_str()).collect();
        assert_eq!(ids, vec!["failed", "interrupted"]);
    }

    #[test]
    fn an_unparseable_journal_is_reported_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("corrupt.json"), "not json {{").unwrap();
        let (recovery, problems) = find_recovery_transactions(dir.path());
        assert!(recovery.is_empty());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(std::fs::symlink_metadata(dir.path().join("corrupt.json")).is_ok());
    }
}
