//! Preflight checks: everything that must be re-verified immediately before a
//! rename is executed.
//!
//! Every check below is run twice: once for the whole batch (so a hard
//! conflict stops the batch before it starts), and again immediately before
//! each individual rename (so a hostile change between review and apply, or
//! between preflight and rename, is caught). Any failure means the entry is
//! **not** renamed - the executor marks it Skipped or ApplyFailed and stops
//! rather than continuing blindly.

use std::collections::BTreeSet;

use crate::safe_read::TrustedRoots;

use super::identity::{capture_identity, identity_matches};
use super::model::TransactionEntry;

/// A named reason a preflight check failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightFailure {
    SourceMissing,
    SourceNotRegular,
    SourceIsSymlink,
    SourceIdentityChanged,
    SourceBasenameChanged,
    DestinationExists,
    DestinationCaseCollision,
    DestinationUnsafe,
    DestinationParentChanged,
    OutsideTrustedRoot,
    CrossFilesystemUnsupported,
    GenerationMismatch {
        current: u64,
        expected: u64,
    },
    NotApproved,
    NotActionable,
    ConflictingBatchTarget,
}

impl PreflightFailure {
    pub fn reason(&self) -> String {
        match self {
            Self::SourceMissing => "the source file no longer exists".to_string(),
            Self::SourceNotRegular => "the source is no longer a regular file".to_string(),
            Self::SourceIsSymlink => {
                "the source has been replaced by a symlink; a symlink is never renamed".to_string()
            }
            Self::SourceIdentityChanged => {
                "the source file is no longer the same object that was reviewed".to_string()
            }
            Self::SourceBasenameChanged => "the source file has been renamed since review".to_string(),
            Self::DestinationExists => {
                "the destination name now exists; it is never overwritten".to_string()
            }
            Self::DestinationCaseCollision => {
                "a sibling whose name differs from the destination only by case now exists"
                    .to_string()
            }
            Self::DestinationUnsafe => "the destination name is no longer a safe filename".to_string(),
            Self::DestinationParentChanged => {
                "the destination parent directory is no longer the same directory".to_string()
            }
            Self::OutsideTrustedRoot => {
                "the rename would operate outside the configured trusted roots".to_string()
            }
            Self::CrossFilesystemUnsupported => {
                "the source and destination are not in the same directory".to_string()
            }
            Self::GenerationMismatch { current, expected } => format!(
                "the plan generation changed since approval (now {current}, expected {expected}); \
                 the plan is stale"
            ),
            Self::NotApproved => {
                "this proposal has not been explicitly approved by the user".to_string()
            }
            Self::NotActionable => {
                "this proposal is not in a Suggested, actionable state".to_string()
            }
            Self::ConflictingBatchTarget => {
                "two proposals in this batch target the same destination".to_string()
            }
        }
    }
}

/// Options that shape a preflight pass.
#[derive(Debug, Clone)]
pub struct PreflightOptions<'a> {
    /// The plan generation the transaction was built for. Must equal the
    /// caller's current generation.
    pub plan_generation: u64,
    /// The current plan generation.
    pub current_generation: u64,
    /// The set of source paths the user explicitly approved.
    pub approved_paths: &'a BTreeSet<String>,
    /// The configured trusted roots. An empty set refuses everything.
    pub trusted: &'a TrustedRoots,
    /// The destination paths of every entry in the batch (to detect two
    /// entries targeting one destination).
    pub batch_destinations: &'a BTreeSet<String>,
}

/// Runs every preflight check for one entry. Returns `Ok(())` when the entry
/// is safe to rename right now, or `Err(failures)` listing every failing
/// check.
pub fn run_preflight(
    entry: &TransactionEntry,
    options: &PreflightOptions<'_>,
) -> Result<(), Vec<PreflightFailure>> {
    let mut failures = Vec::new();

    if options.plan_generation != options.current_generation {
        failures.push(PreflightFailure::GenerationMismatch {
            current: options.current_generation,
            expected: options.plan_generation,
        });
    }

    if !options
        .approved_paths
        .contains(&entry.source_path.to_string_lossy().into_owned())
    {
        failures.push(PreflightFailure::NotApproved);
    }

    // The destination basename must still be a single, safe component.
    if !is_safe_basename(&entry.proposed_basename) {
        failures.push(PreflightFailure::DestinationUnsafe);
    }

    // Same-directory requirement.
    let source_parent = entry.source_path.parent();
    let destination_parent = entry.destination_path.parent();
    if source_parent != destination_parent {
        failures.push(PreflightFailure::CrossFilesystemUnsupported);
    }

    // Trusted-root containment. Only meaningful when there are roots.
    if !options.trusted.is_empty() {
        let Some(source_parent) = source_parent else {
            failures.push(PreflightFailure::OutsideTrustedRoot);
            return Err(failures);
        };
        let canonical_parent = std::fs::canonicalize(source_parent).ok();
        let inside = canonical_parent
            .as_deref()
            .is_some_and(|parent| options.trusted.contains_canonical(parent));
        let destination_parent_ok = destination_parent
            .and_then(|parent| std::fs::canonicalize(parent).ok())
            .as_deref()
            .is_some_and(|parent| options.trusted.contains_canonical(parent));
        if !inside || !destination_parent_ok {
            failures.push(PreflightFailure::OutsideTrustedRoot);
        }
    }

    // Source object identity.
    match capture_identity(&entry.source_path) {
        Err(_) => failures.push(PreflightFailure::SourceMissing),
        Ok(current) => {
            if current.kind == super::model::ObjectKind::Symlink
                || current.kind == super::model::ObjectKind::BrokenSymlink
            {
                failures.push(PreflightFailure::SourceIsSymlink);
            } else if current.kind != super::model::ObjectKind::RegularFile {
                failures.push(PreflightFailure::SourceNotRegular);
            }
            if !identity_matches(&entry.identity, &current) {
                failures.push(PreflightFailure::SourceIdentityChanged);
            }
        }
    }

    // Source basename unchanged.
    let source_basename = entry
        .source_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    if source_basename.as_deref() != Some(entry.original_basename.as_str()) {
        failures.push(PreflightFailure::SourceBasenameChanged);
    }

    // Destination must not exist.
    if std::fs::symlink_metadata(&entry.destination_path).is_ok() {
        failures.push(PreflightFailure::DestinationExists);
    } else {
        // Case-only collision re-checked against the live directory: a file
        // whose name differs from the destination only by case may have
        // appeared since the plan was built.
        if let Some(parent) = entry.source_path.parent()
            && let Ok(entries) = std::fs::read_dir(parent)
        {
            let proposed_lower = entry.proposed_basename.to_ascii_lowercase();
            let case_collision = entries.flatten().any(|dir_entry| {
                let name = dir_entry.file_name();
                let name = name.to_string_lossy();
                name.to_ascii_lowercase() == proposed_lower
                    && name != entry.original_basename
                    && name != entry.proposed_basename
            });
            if case_collision {
                failures.push(PreflightFailure::DestinationCaseCollision);
            }
        }
    }

    // No conflicting batch target.
    if options
        .batch_destinations
        .contains(&entry.destination_path.to_string_lossy().into_owned())
    {
        failures.push(PreflightFailure::ConflictingBatchTarget);
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

/// Whether `basename` is safe to rename to: a single component that is not
/// `.`/`..`, empty, or containing a path separator or NUL.
pub fn is_safe_basename(basename: &str) -> bool {
    !basename.is_empty()
        && basename != "."
        && basename != ".."
        && !basename.contains(['/', '\\', '\0'])
}

/// The destination paths that appear more than once in a batch - the only
/// ones that are a *conflict*. A destination targeted by exactly one entry is
/// that entry's own target and is not a conflict.
pub fn batch_destinations(entries: &[TransactionEntry]) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut duplicates: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        let destination = entry.destination_path.to_string_lossy().into_owned();
        if !seen.insert(destination.clone()) {
            duplicates.insert(destination);
        }
    }
    duplicates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn entry(source: &Path, destination: &Path) -> TransactionEntry {
        TransactionEntry {
            source_path: source.to_path_buf(),
            destination_path: destination.to_path_buf(),
            original_basename: source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            proposed_basename: destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            identity: super::super::identity::capture_identity(source).unwrap(),
            preflight_passed: false,
            preflight_failures: Vec::new(),
            state: super::super::model::EntryState::Planned,
            failure_reason: None,
            applied_at_unix: None,
            rolled_back_at_unix: None,
            unknown: Default::default(),
        }
    }

    fn options<'a>(
        approved: &'a BTreeSet<String>,
        trusted: &'a TrustedRoots,
        destinations: &'a BTreeSet<String>,
        generation: u64,
    ) -> PreflightOptions<'a> {
        PreflightOptions {
            plan_generation: generation,
            current_generation: generation,
            approved_paths: approved,
            trusted,
            batch_destinations: destinations,
        }
    }

    #[test]
    fn a_clean_regular_file_passes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let entry = entry(&source, &destination);
        let approved = BTreeSet::from([source.to_string_lossy().into_owned()]);
        let trusted = TrustedRoots::from_paths([dir.path()]);
        // No destination is duplicated in the batch.
        let destinations = BTreeSet::new();
        let result = run_preflight(&entry, &options(&approved, &trusted, &destinations, 1));
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn a_missing_source_fails() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("gone.bin");
        let destination = dir.path().join("b.bin");
        // The entry was reviewed while the file existed; it is gone by the
        // time preflight runs.
        std::fs::write(&source, b"data").unwrap();
        let entry = entry(&source, &destination);
        std::fs::remove_file(&source).unwrap();
        let approved = BTreeSet::from([source.to_string_lossy().into_owned()]);
        let trusted = TrustedRoots::from_paths([dir.path()]);
        let destinations = BTreeSet::new();
        let result = run_preflight(&entry, &options(&approved, &trusted, &destinations, 1));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .iter()
                .any(|f| f == &PreflightFailure::SourceMissing)
        );
    }

    #[test]
    fn an_existing_destination_fails() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        std::fs::write(&destination, b"taken").unwrap();
        let entry = entry(&source, &destination);
        let approved = BTreeSet::from([source.to_string_lossy().into_owned()]);
        let trusted = TrustedRoots::from_paths([dir.path()]);
        let destinations = BTreeSet::new();
        let result = run_preflight(&entry, &options(&approved, &trusted, &destinations, 1));
        assert!(
            result
                .unwrap_err()
                .iter()
                .any(|f| f == &PreflightFailure::DestinationExists)
        );
    }

    #[test]
    fn a_symlink_substitution_fails() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let entry = entry(&source, &dir.path().join("b.bin"));
        std::fs::remove_file(&source).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &source).unwrap();
        let approved = BTreeSet::from([source.to_string_lossy().into_owned()]);
        let trusted = TrustedRoots::from_paths([dir.path()]);
        let destinations = BTreeSet::new();
        let result = run_preflight(&entry, &options(&approved, &trusted, &destinations, 1));
        assert!(
            result
                .unwrap_err()
                .iter()
                .any(|f| f == &PreflightFailure::SourceIsSymlink)
        );
    }

    #[test]
    fn a_stale_generation_fails() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let entry = entry(&source, &destination);
        let approved = BTreeSet::from([source.to_string_lossy().into_owned()]);
        let trusted = TrustedRoots::from_paths([dir.path()]);
        let destinations = BTreeSet::new();
        let mut opts = options(&approved, &trusted, &destinations, 1);
        opts.current_generation = 2;
        assert!(
            run_preflight(&entry, &opts)
                .unwrap_err()
                .iter()
                .any(|f| matches!(f, PreflightFailure::GenerationMismatch { .. }))
        );
    }

    #[test]
    fn outside_trusted_roots_fails() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = outside.path().join("b.bin");
        let entry = entry(&source, &destination);
        let approved = BTreeSet::from([source.to_string_lossy().into_owned()]);
        let trusted = TrustedRoots::from_paths([dir.path()]);
        let destinations = BTreeSet::new();
        let result = run_preflight(&entry, &options(&approved, &trusted, &destinations, 1));
        assert!(
            result
                .unwrap_err()
                .iter()
                .any(|f| f == &PreflightFailure::OutsideTrustedRoot)
        );
    }

    #[test]
    fn an_unapproved_proposal_fails() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.bin");
        std::fs::write(&source, b"data").unwrap();
        let destination = dir.path().join("b.bin");
        let entry = entry(&source, &destination);
        let approved = BTreeSet::new();
        let trusted = TrustedRoots::from_paths([dir.path()]);
        let destinations = BTreeSet::new();
        let result = run_preflight(&entry, &options(&approved, &trusted, &destinations, 1));
        assert!(
            result
                .unwrap_err()
                .iter()
                .any(|f| f == &PreflightFailure::NotApproved)
        );
    }
}
