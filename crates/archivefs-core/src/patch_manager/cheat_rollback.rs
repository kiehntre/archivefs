use super::cheat_install_result::{CheatInstallOutcome, CheatInstallPath, parse_cheat_install_run};
use super::cheat_rollback_result::*;
use super::destination_safety::assess_destination;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const CHEAT_ROLLBACK_RUNS_DIRECTORY_NAME: &str = "cheat-rollback-runs";

#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultPoint {
    TempWrite,
    Verification,
    Rename,
    Removal,
    JournalWrite,
}
#[cfg(test)]
thread_local! { static ROLLBACK_FAULT: std::cell::Cell<Option<FaultPoint>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
fn should_inject(point: FaultPoint) -> bool {
    ROLLBACK_FAULT.with(|f| f.get() == Some(point))
}
#[cfg(not(test))]
fn should_inject(_point: FaultPoint) -> bool {
    false
}

pub struct CheatRollbackOptions {
    pub journal_path: PathBuf,
    pub destination_root: PathBuf,
    pub backup_directory: PathBuf,
    pub rollback_journal_directory: PathBuf,
    pub dry_run: bool,
    pub confirmed: bool,
    pub run_id: String,
    pub started_at_unix_seconds: u64,
}
pub struct CheatRollbackRunOutcome {
    pub run: CheatRollbackRun,
    pub journal_path: Option<PathBuf>,
    pub journal_error: Option<String>,
}
fn hash_file(p: &Path) -> Result<String, String> {
    let mut f = fs::File::open(p).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut b = [0u8; 8192];
    loop {
        let n = f.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        };
        h.update(&b[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{:02x}", b)).collect())
}
fn safe_relative_path(
    root: &Path,
    encoded_root: &CheatInstallPath,
    encoded: &CheatInstallPath,
) -> Result<(PathBuf, Vec<String>), String> {
    if encoded_root.lossy != CheatInstallPath::from_path(root).lossy
        || encoded_root.display != CheatInstallPath::from_path(root).display
    {
        return Err("journal destination root does not match supplied root".into());
    }
    let rel = Path::new(&encoded.display)
        .strip_prefix(Path::new(&encoded_root.display))
        .map_err(|_| "journal path is outside supplied root".to_string())?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| match c {
            Component::Normal(v) => Ok(v.to_string_lossy().into_owned()),
            _ => Err("journal path contains absolute or traversal component".to_string()),
        })
        .collect::<Result<_, _>>()?;
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
        return Err(
            "journal destination must contain exactly platform and filename components".into(),
        );
    }
    let actual = root.join(&parts[0]).join(&parts[1]);
    if CheatInstallPath::from_path(&actual) != *encoded {
        return Err("journal destination disagrees with safely reconstructed destination".into());
    }
    Ok((actual, parts))
}

fn safe_backup_path(root: &Path, encoded: &CheatInstallPath) -> Result<PathBuf, String> {
    let encoded_root = CheatInstallPath::from_path(root);
    let rel = Path::new(&encoded.display)
        .strip_prefix(Path::new(&encoded_root.display))
        .map_err(|_| "backup path is outside expected backup root".to_string())?;
    let mut out = root.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Normal(v) => out.push(v),
            _ => return Err("backup path contains absolute or traversal component".into()),
        }
    }
    if out == *root {
        return Err("backup path is empty".into());
    }
    if CheatInstallPath::from_path(&out) != *encoded {
        return Err("backup path cannot be reconstructed losslessly".into());
    }
    let mut cur = root.to_path_buf();
    let md = fs::symlink_metadata(&cur).map_err(|e| e.to_string())?;
    if md.file_type().is_symlink() || !md.is_dir() {
        return Err("backup root is not a plain directory".into());
    }
    for c in rel.components() {
        cur.push(c.as_os_str());
        let md = fs::symlink_metadata(&cur).map_err(|_| "backup path is missing".to_string())?;
        if md.file_type().is_symlink() {
            return Err("backup path contains symlink".into());
        }
    }
    Ok(out)
}

pub fn execute_cheat_rollback(
    journal_path: &Path,
    options: &CheatRollbackOptions,
) -> CheatRollbackRunOutcome {
    let txt = match fs::read_to_string(journal_path) {
        Ok(t) => t,
        Err(e) => {
            return failed_run(journal_path, options, format!("{e}"));
        }
    };
    let orig = match parse_cheat_install_run(&txt) {
        Ok(r) => r,
        Err(e) => {
            return failed_run(journal_path, options, e.to_string());
        }
    };
    let effective_dry = options.dry_run || !options.confirmed;
    let mut entries = Vec::new();
    for e in orig.entries.iter() {
        let dest = e.destination_path.clone();
        let mut r = CheatRollbackEntryResult {
            original_outcome: e.outcome,
            destination_path: dest.clone(),
            expected_installed_hash: e
                .resulting_destination_hash
                .clone()
                .or(e.expected_source_hash.clone()),
            expected_previous_hash: e.previous_destination_hash.clone(),
            observed_current_hash: None,
            backup_path: e.backup_path.clone(),
            outcome: CheatRollbackOutcome::NoChangeRequired,
            wrote: false,
            error_code: None,
            message: String::new(),
            retryable: false,
        };
        if !e.applied
            || !matches!(
                e.outcome,
                CheatInstallOutcome::InstalledNew | CheatInstallOutcome::ReplacedWithBackup
            )
        {
            r.message = "original install made no change".into();
            entries.push(r);
            continue;
        }
        let Some(cp) = dest else {
            r.outcome = CheatRollbackOutcome::FailedInvalidJournal;
            r.message = "missing destination".into();
            entries.push(r);
            continue;
        };
        let Some(journal_root) = orig.destination_root.as_ref() else {
            r.outcome = CheatRollbackOutcome::FailedInvalidJournal;
            r.message = "install journal has no destination root".into();
            entries.push(r);
            continue;
        };
        if e.resulting_destination_hash.is_none() {
            r.outcome = CheatRollbackOutcome::FailedInvalidJournal;
            r.message = "applied install entry has no resulting destination hash".into();
            entries.push(r);
            continue;
        }
        let (p, parts) = match safe_relative_path(&options.destination_root, journal_root, &cp) {
            Ok(v) => v,
            Err(msg) => {
                r.outcome = CheatRollbackOutcome::FailedUnsafeDestination;
                r.error_code = Some("unsafe_destination".into());
                r.message = msg;
                entries.push(r);
                continue;
            }
        };
        if assess_destination(
            &options.destination_root,
            std::ffi::OsStr::new(&parts[0]),
            std::ffi::OsStr::new(&parts[1]),
        )
        .is_err()
        {
            r.outcome = CheatRollbackOutcome::FailedUnsafeDestination;
            r.error_code = Some("unsafe_destination".into());
            entries.push(r);
            continue;
        }
        let md = match fs::symlink_metadata(&p) {
            Ok(m) => Some(m),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                r.outcome = CheatRollbackOutcome::FailedIo;
                r.message = e.to_string();
                entries.push(r);
                continue;
            }
        };
        if md
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink() || m.is_dir())
        {
            r.outcome = CheatRollbackOutcome::FailedUnsafeDestination;
            entries.push(r);
            continue;
        }
        let cur = if md.is_some() {
            hash_file(&p).ok()
        } else {
            None
        };
        r.observed_current_hash = cur.clone();
        if e.outcome == CheatInstallOutcome::InstalledNew {
            if md.is_none() {
                r.outcome = CheatRollbackOutcome::AlreadyRestored;
            } else if cur.as_deref() != r.expected_installed_hash.as_deref() {
                r.outcome = CheatRollbackOutcome::FailedDestinationChanged;
                r.message = "destination changed".into();
            } else if effective_dry {
                r.outcome = CheatRollbackOutcome::WouldRemoveInstalledFile;
            } else if should_inject(FaultPoint::Removal) {
                r.outcome = CheatRollbackOutcome::FailedIo;
                r.error_code = Some("injected_removal_failure".into());
            } else {
                match fs::remove_file(&p) {
                    Ok(_) => {
                        r.outcome = CheatRollbackOutcome::RemovedInstalledFile;
                        r.wrote = true
                    }
                    Err(err) => {
                        r.outcome = CheatRollbackOutcome::FailedIo;
                        r.message = err.to_string();
                    }
                }
            }
        } else {
            let Some(bp_enc) = e.backup_path.as_ref() else {
                r.outcome = CheatRollbackOutcome::FailedBackupMissing;
                entries.push(r);
                continue;
            };
            let bp = match safe_backup_path(&options.backup_directory, bp_enc) {
                Ok(p) => p,
                Err(msg) => {
                    r.outcome = CheatRollbackOutcome::FailedUnsafeBackupPath;
                    r.message = msg;
                    entries.push(r);
                    continue;
                }
            };
            let bmd = fs::symlink_metadata(&bp).ok();
            if bmd.is_none() {
                r.outcome = CheatRollbackOutcome::FailedBackupMissing;
                entries.push(r);
                continue;
            }
            if bmd
                .as_ref()
                .is_some_and(|m| m.file_type().is_symlink() || !m.is_file())
            {
                r.outcome = CheatRollbackOutcome::FailedUnsafeBackupPath;
                entries.push(r);
                continue;
            }
            let bh = hash_file(&bp).ok();
            if bh.as_deref() != e.previous_destination_hash.as_deref() {
                r.outcome = CheatRollbackOutcome::FailedBackupChanged;
                entries.push(r);
                continue;
            }
            if cur.as_deref() == e.previous_destination_hash.as_deref() {
                r.outcome = CheatRollbackOutcome::AlreadyRestored;
            } else if cur.as_deref() != r.expected_installed_hash.as_deref() {
                r.outcome = CheatRollbackOutcome::FailedDestinationChanged;
            } else if effective_dry {
                r.outcome = CheatRollbackOutcome::WouldRestoreBackup;
            } else {
                let parent = p.parent().unwrap_or(Path::new("."));
                let tmp = parent.join(format!(
                    ".archivefs-rollback-{}-{}",
                    options.run_id,
                    std::process::id()
                ));
                let result = (|| -> Result<(), String> {
                    if should_inject(FaultPoint::TempWrite) {
                        return Err("injected temporary restore write failure".into());
                    }
                    let bytes = fs::read(&bp).map_err(|e| e.to_string())?;
                    let mut f = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&tmp)
                        .map_err(|e| e.to_string())?;
                    std::io::Write::write_all(&mut f, &bytes).map_err(|e| e.to_string())?;
                    f.sync_all().map_err(|e| e.to_string())?;
                    if should_inject(FaultPoint::Verification) {
                        return Err("injected restore verification failure".into());
                    }
                    if hash_file(&tmp).ok().as_deref() != e.previous_destination_hash.as_deref() {
                        return Err("restore verification hash mismatch".into());
                    }
                    if should_inject(FaultPoint::Rename) {
                        return Err("injected restore rename failure".into());
                    }
                    fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
                    let _ = fs::File::open(parent).and_then(|f| f.sync_all());
                    Ok(())
                })();
                match result {
                    Ok(_) => {
                        r.outcome = CheatRollbackOutcome::RestoredBackup;
                        r.wrote = true
                    }
                    Err(err) => {
                        let _ = fs::remove_file(&tmp);
                        r.outcome = if err.contains("verification") {
                            CheatRollbackOutcome::FailedVerification
                        } else {
                            CheatRollbackOutcome::FailedIo
                        };
                        r.message = err;
                    }
                }
            }
        }
        entries.push(r);
    }
    let summary = CheatRollbackSummary::from_entries(&entries);
    let status = CheatRollbackRunStatus::derive(&summary, effective_dry);
    let run = CheatRollbackRun {
        schema_version: CHEAT_ROLLBACK_RUN_SCHEMA_VERSION,
        run_id: options.run_id.clone(),
        original_install_run_id: orig.run_id,
        original_journal_path: CheatInstallPath::from_path(journal_path),
        started_at_unix_seconds: options.started_at_unix_seconds,
        completed_at_unix_seconds: Some(options.started_at_unix_seconds),
        dry_run: effective_dry,
        confirmed: options.confirmed,
        destination_root: CheatInstallPath::from_path(&options.destination_root),
        entries,
        summary,
        status,
        rollback_journal_path: None,
        journal_write_error: None,
    };
    if effective_dry {
        return CheatRollbackRunOutcome {
            run,
            journal_path: None,
            journal_error: None,
        };
    }
    let path = options
        .rollback_journal_directory
        .join(format!("{}.json", options.run_id));
    let mut run = run;
    run.rollback_journal_path = Some(CheatInstallPath::from_path(&path));
    let json = serde_json::to_string_pretty(&run).unwrap();
    let err = (|| -> Result<(), String> {
        if should_inject(FaultPoint::JournalWrite) {
            return Err("injected rollback journal write failure".into());
        }
        fs::create_dir_all(&options.rollback_journal_directory).map_err(|e| e.to_string())?;
        if fs::symlink_metadata(&path).is_ok() {
            return Err("rollback journal already exists".into());
        };
        let tmp = options.rollback_journal_directory.join(format!(
            ".archivefs-rollback-journal-{}",
            std::process::id()
        ));
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut f, json.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        let _ = fs::File::open(&options.rollback_journal_directory).and_then(|f| f.sync_all());
        Ok(())
    })()
    .err();
    if let Some(error) = &err {
        run.rollback_journal_path = None;
        run.journal_write_error = Some(error.clone());
    }
    CheatRollbackRunOutcome {
        run,
        journal_path: if err.is_none() { Some(path) } else { None },
        journal_error: err,
    }
}
pub fn execute_cheat_rollback_run(
    journal_path: &Path,
    options: &CheatRollbackOptions,
) -> CheatRollbackRunOutcome {
    execute_cheat_rollback(journal_path, options)
}
fn failed_run(path: &Path, o: &CheatRollbackOptions, msg: String) -> CheatRollbackRunOutcome {
    let e = CheatRollbackEntryResult {
        original_outcome: CheatInstallOutcome::FailedWrite,
        destination_path: None,
        expected_installed_hash: None,
        expected_previous_hash: None,
        observed_current_hash: None,
        backup_path: None,
        outcome: CheatRollbackOutcome::FailedInvalidJournal,
        wrote: false,
        error_code: Some("invalid_journal".into()),
        message: msg,
        retryable: false,
    };
    let s = CheatRollbackSummary::from_entries(std::slice::from_ref(&e));
    let dry_run = o.dry_run || !o.confirmed;
    let run = CheatRollbackRun {
        schema_version: 1,
        run_id: o.run_id.clone(),
        original_install_run_id: String::new(),
        original_journal_path: CheatInstallPath::from_path(path),
        started_at_unix_seconds: o.started_at_unix_seconds,
        completed_at_unix_seconds: Some(o.started_at_unix_seconds),
        dry_run,
        confirmed: o.confirmed,
        destination_root: CheatInstallPath::from_path(&o.destination_root),
        entries: vec![e],
        summary: s,
        status: CheatRollbackRunStatus::derive(&s, dry_run),
        rollback_journal_path: None,
        journal_write_error: None,
    };
    CheatRollbackRunOutcome {
        run,
        journal_path: None,
        journal_error: None,
    }
}
