//! Read-only detection of ArchiveFS-managed cheat and patch entries that can
//! no longer be accounted for.
//!
//! # What proves ownership on this build
//!
//! This check never guesses. An entry is only ever treated as ArchiveFS's if
//! one of exactly two things proves it:
//!
//! 1. **An install journal.** `SharedApplyJournal` is a complete ownership
//!    record: the adapter, the selected archive, the verified game identity,
//!    the profile, the exact destination file, and the digest of what was
//!    written. Every adapter is covered this way, because every install goes
//!    through the shared transaction.
//! 2. **An adapter's own in-file marker.** Only PCSX2 has one on this build:
//!    `// ArchiveFS managed block: <id>` … `// End ArchiveFS managed block`,
//!    parsed by the existing [`parse_pnach_document`], which already bounds
//!    the file and already reports malformed and duplicate blocks.
//!
//! Dolphin `GameSettings` and Xenia `.patch.toml` files carry **no** ArchiveFS
//! marker on this build. That means a managed entry whose journal has been
//! deleted is undetectable for those two adapters, and this module says so
//! rather than pretending otherwise - see the narrowed deferred entry in
//! `DEFERRED_CHECKS`. Inventing a marker here would be worse than useless: it
//! would risk classifying a user's own codes as ArchiveFS's.
//!
//! # What is never reported
//!
//! - A user's own Gecko, Action Replay, PNACH or Xenia entry. Without one of
//!   the two ownership proofs above, an entry is simply not ArchiveFS's, and
//!   is left entirely alone.
//! - An empty managed section. `[Gecko_Enabled]` with nothing in it, or a
//!   `.pnach` with no managed blocks left, is the normal result of a
//!   successful uninstall. That is not damage and produces no finding.
//!
//! # Read-only
//!
//! Journals and managed files are read with bounded reads; nothing is opened
//! for writing, no directory is walked outside a discovered profile
//! destination, no symlink is followed, no game image is read, no emulator is
//! launched, and nothing contacts a network.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    DoctorCategory, DoctorSeverity, DoctorSubsystem, Finding, Measurement, NotCheckedCheck,
};
use crate::emulator_environment::EncodedPath;
use crate::patch_manager::{
    MAX_MANAGED_PNACH_BYTES, PreviewAdapter, SharedApplyStatus, SharedHistoryReport,
    parse_pnach_document,
};

// --- Bounds ---------------------------------------------------------------

/// Never inspect more profile destinations than this in one scan.
pub const MAX_SCANNED_PROFILES: usize = 16;
/// Never inspect more files inside one profile destination than this.
///
/// Set from what a real machine holds, not from a round number: a PCSX2
/// profile carrying the community patches pack has several thousand `.pnach`
/// files, and truncating that would leave most of it unchecked while
/// declaring the scan partial on an entirely healthy setup. The total byte
/// budget below is the bound that actually limits cost; this one only stops an
/// absurd directory.
pub const MAX_FILES_PER_PROFILE: usize = 8192;
/// Never read a single managed file larger than this. Matches the limit the
/// PNACH parser itself enforces, so a file this check accepts is one the real
/// install path would also accept.
pub const MAX_MANAGED_FILE_BYTES: u64 = MAX_MANAGED_PNACH_BYTES as u64;
/// Never read more than this in total across one subsystem.
pub const MAX_TOTAL_MANAGED_BYTES: u64 = 16 * 1024 * 1024;
/// Above this many orphan findings, report one summary instead.
pub const MAX_INDIVIDUAL_ORPHAN_FINDINGS: usize = 10;
/// Above this many malformed files, report one aggregate instead.
pub const MAX_INDIVIDUAL_MALFORMED_FINDINGS: usize = 5;

// --- Model ----------------------------------------------------------------

/// A managed format ArchiveFS writes and can recognise again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedFormat {
    /// Dolphin `GameSettings/<ID>.ini`. Journal-anchored only: no in-file
    /// ArchiveFS marker exists on this build.
    DolphinGameSettings,
    /// PCSX2 `.pnach`. Has a real in-file marker, so both directions work.
    Pcsx2Pnach,
    /// Xenia `patches/*.patch.toml`. Journal-anchored only.
    XeniaPatch,
    /// RetroArch `.cht`. Journal-anchored only.
    RetroArchCheat,
}

impl ManagedFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::DolphinGameSettings => "Dolphin GameSettings",
            Self::Pcsx2Pnach => "PCSX2 PNACH",
            Self::XeniaPatch => "Xenia patch",
            Self::RetroArchCheat => "RetroArch cheat file",
        }
    }

    /// Whether this format carries an ArchiveFS ownership marker inside the
    /// file, so an entry can be recognised without a journal.
    pub fn has_in_file_marker(self) -> bool {
        matches!(self, Self::Pcsx2Pnach)
    }

    fn from_adapter(adapter: PreviewAdapter) -> Self {
        match adapter {
            PreviewAdapter::Dolphin => Self::DolphinGameSettings,
            PreviewAdapter::Pcsx2 => Self::Pcsx2Pnach,
            PreviewAdapter::Xenia => Self::XeniaPatch,
            PreviewAdapter::RetroArch => Self::RetroArchCheat,
        }
    }
}

/// What ArchiveFS concluded about one managed entry. These are the states the
/// milestone distinguishes; each is a separate, defensible observation rather
/// than one vague "orphan".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedEntryState {
    /// The journal exists, the destination is still there, and its content
    /// still matches what was installed. Healthy - never a finding.
    Owned,
    /// The journal records a partial failure, so the install never completed.
    IncompleteInstall,
    /// A rollback was already recorded: this entry was deliberately removed.
    /// Healthy - never a finding.
    RolledBack,
    /// The destination file the journal names is gone.
    DestinationMissing,
    /// The destination is still there but no longer matches what ArchiveFS
    /// wrote. Something else edited or replaced it.
    DestinationChanged,
    /// The archive the install came from is no longer on disk.
    SourceGameMissing,
    /// The profile directory the install targeted is gone.
    ProfileUnavailable,
    /// A `.pnach` carries ArchiveFS managed blocks but no journal accounts for
    /// that file. Only detectable for a format with an in-file marker.
    OwnershipRecordMissing,
    /// An ArchiveFS marker is present but structurally broken.
    MalformedMarker,
}

impl ManagedEntryState {
    /// `None` for the healthy states.
    fn severity(self) -> Option<DoctorSeverity> {
        match self {
            Self::Owned | Self::RolledBack => None,
            // A half-finished install is the one state that can leave an
            // emulator file in an unintended condition.
            Self::IncompleteInstall => Some(DoctorSeverity::Warning),
            Self::MalformedMarker => Some(DoctorSeverity::Warning),
            // Everything else is a bookkeeping mismatch, not damage: the user
            // may simply have tidied up by hand.
            Self::DestinationMissing
            | Self::DestinationChanged
            | Self::SourceGameMissing
            | Self::ProfileUnavailable
            | Self::OwnershipRecordMissing => Some(DoctorSeverity::Info),
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::Owned => "accounted for by an install record",
            Self::IncompleteInstall => "the install that wrote it did not finish",
            Self::RolledBack => "already removed by a recorded rollback",
            Self::DestinationMissing => "the file it was written to is no longer there",
            Self::DestinationChanged => "the file no longer matches what ArchiveFS wrote",
            Self::SourceGameMissing => "the game it came from is no longer in the library",
            Self::ProfileUnavailable => "the emulator profile it was written to is gone",
            Self::OwnershipRecordMissing => "no install record accounts for it",
            Self::MalformedMarker => "its ArchiveFS marker is structurally broken",
        }
    }

    fn finding_id(self) -> &'static str {
        match self {
            Self::Owned | Self::RolledBack => "managed_entry.owned",
            Self::IncompleteInstall => "managed_entry.incomplete_install",
            Self::DestinationMissing => "managed_entry.destination_missing",
            Self::DestinationChanged => "managed_entry.destination_changed",
            Self::SourceGameMissing => "managed_entry.source_game_missing",
            Self::ProfileUnavailable => "managed_entry.profile_unavailable",
            Self::OwnershipRecordMissing => "managed_entry.ownership_record_missing",
            Self::MalformedMarker => "managed_entry.malformed_marker",
        }
    }
}

/// One managed entry ArchiveFS could account for, or could not.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ManagedEntry {
    pub format: ManagedFormat,
    pub state: ManagedEntryState,
    pub destination: EncodedPath,
    /// How ownership was proven.
    pub ownership_evidence: String,
    /// The operation that installed it, when a journal named one.
    pub operation_id: Option<String>,
    /// The verified game identity recorded at install time.
    pub game_identity: Option<String>,
    /// The archive the install came from.
    pub source_archive: Option<EncodedPath>,
    /// The profile the install targeted.
    pub profile_root: Option<EncodedPath>,
    /// Managed block ids found in the file, for a format with an in-file
    /// marker.
    pub managed_block_ids: Vec<String>,
    /// What Doctor deliberately did not change - stated so the report is
    /// unambiguous about being read-only.
    pub left_untouched: &'static str,
}

/// A file whose ArchiveFS marker could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MalformedManagedFile {
    pub format: ManagedFormat,
    pub path: EncodedPath,
    pub detail: String,
}

/// A file this scan deliberately did not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedManagedFile {
    pub path: EncodedPath,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ManagedEntryScan {
    pub entries: Vec<ManagedEntry>,
    pub malformed: Vec<MalformedManagedFile>,
    pub skipped: Vec<SkippedManagedFile>,
    /// True when a bound stopped the scan early, so the result is partial.
    pub truncated: bool,
    /// Formats that were scanned for their own in-file marker, as opposed to
    /// being covered only by journals.
    pub marker_scanned_formats: Vec<ManagedFormat>,
    pub bytes_read: u64,
}

impl ManagedEntryScan {
    pub fn orphans(&self) -> Vec<&ManagedEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.state.severity().is_some())
            .collect()
    }
}

/// One profile destination this scan may look inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedScanTarget {
    pub format: ManagedFormat,
    /// The directory ArchiveFS writes managed files into.
    pub destination_root: PathBuf,
}

// --- The scan -------------------------------------------------------------

/// Accounts for every managed entry the install history records, and - for the
/// one format with an in-file marker - looks for managed files no journal
/// accounts for.
///
/// Read-only throughout: bounded `fs::read` of journal-named destinations and
/// of `.pnach` files inside supplied profile destinations, `symlink_metadata`
/// for safety, and nothing else.
pub fn scan_managed_entries(
    history: &SharedHistoryReport,
    targets: &[ManagedScanTarget],
) -> ManagedEntryScan {
    let mut scan = ManagedEntryScan {
        entries: Vec::new(),
        malformed: Vec::new(),
        skipped: Vec::new(),
        truncated: false,
        marker_scanned_formats: Vec::new(),
        bytes_read: 0,
    };

    // --- Direction 1: every journal entry, checked against live state.
    let mut journalled_destinations: BTreeSet<PathBuf> = BTreeSet::new();
    for (_, journal) in &history.journals {
        if journal.dry_run {
            continue;
        }
        let format = ManagedFormat::from_adapter(journal.context.adapter);
        for entry in &journal.entries {
            // Only entries that actually wrote something are ArchiveFS's.
            if !matches!(
                entry.outcome,
                crate::patch_manager::SharedApplyOutcome::InstalledNew
                    | crate::patch_manager::SharedApplyOutcome::ReplacedExisting
            ) {
                continue;
            }
            let destination = PathBuf::from(&entry.plan_entry.destination_root.display)
                .join(&entry.plan_entry.destination_relative_path.display);
            journalled_destinations.insert(destination.clone());
            let profile_root = PathBuf::from(&entry.plan_entry.destination_root.display);
            let source_archive = PathBuf::from(&journal.context.selected_archive.display);

            let state = classify_journalled_entry(
                journal.status,
                journal.rollback_operation_id.is_some(),
                &destination,
                &profile_root,
                &source_archive,
                entry.final_destination_digest.as_deref(),
                &mut scan,
            );
            scan.entries.push(ManagedEntry {
                format,
                state,
                destination: EncodedPath::from_path(&destination),
                ownership_evidence: format!(
                    "install journal operation {} recorded writing this exact file",
                    journal.operation_id
                ),
                operation_id: Some(journal.operation_id.clone()),
                game_identity: Some(journal.context.verified_game_identity.clone()),
                source_archive: Some(EncodedPath::from_path(&source_archive)),
                profile_root: Some(EncodedPath::from_path(&profile_root)),
                managed_block_ids: Vec::new(),
                left_untouched: "Doctor read this file's metadata and content only. Nothing was written, moved or removed.",
            });
        }
    }

    // --- Direction 2: files carrying an ArchiveFS marker with no journal.
    //
    // Only possible for a format whose marker lives in the file. PCSX2 is the
    // only one on this build; Dolphin and Xenia have no marker, so a managed
    // entry whose journal is gone is undetectable for them and is reported as
    // a narrowed deferred check rather than silently missed.
    for target in targets.iter().take(MAX_SCANNED_PROFILES) {
        if !target.format.has_in_file_marker() {
            continue;
        }
        if !scan.marker_scanned_formats.contains(&target.format) {
            scan.marker_scanned_formats.push(target.format);
        }
        scan_marked_files(target, &journalled_destinations, &mut scan);
    }
    if targets.len() > MAX_SCANNED_PROFILES {
        scan.truncated = true;
    }

    scan.entries.sort_by(|left, right| {
        (
            left.format,
            &left.destination.display,
            left.state.finding_id(),
        )
            .cmp(&(
                right.format,
                &right.destination.display,
                right.state.finding_id(),
            ))
    });
    scan.malformed
        .sort_by(|left, right| left.path.display.cmp(&right.path.display));
    scan
}

#[allow(clippy::too_many_arguments)]
fn classify_journalled_entry(
    status: SharedApplyStatus,
    rolled_back: bool,
    destination: &Path,
    profile_root: &Path,
    source_archive: &Path,
    installed_digest: Option<&str>,
    scan: &mut ManagedEntryScan,
) -> ManagedEntryState {
    if rolled_back {
        return ManagedEntryState::RolledBack;
    }
    if status == SharedApplyStatus::PartialFailure {
        return ManagedEntryState::IncompleteInstall;
    }
    if !profile_root.is_dir() {
        return ManagedEntryState::ProfileUnavailable;
    }
    let Ok(metadata) = fs::symlink_metadata(destination) else {
        return ManagedEntryState::DestinationMissing;
    };
    if metadata.file_type().is_symlink() {
        // Never follow a symlink to inspect content; report the target as not
        // safely checkable rather than reading through it.
        scan.skipped.push(SkippedManagedFile {
            path: EncodedPath::from_path(destination),
            reason: "the destination is a symlink, which ArchiveFS never follows",
        });
        return ManagedEntryState::DestinationChanged;
    }
    if !metadata.is_file() {
        return ManagedEntryState::DestinationChanged;
    }
    // A missing source archive is worth knowing about even when the
    // destination is intact: the cheat is installed for a game that is gone.
    if !source_archive.as_os_str().is_empty() && !source_archive.exists() {
        return ManagedEntryState::SourceGameMissing;
    }
    // Compare against what was installed, when the journal recorded a digest.
    match installed_digest {
        Some(expected) => match read_bounded(destination, &metadata, scan) {
            Some(bytes) => {
                if digest_hex(&bytes) == expected {
                    ManagedEntryState::Owned
                } else {
                    ManagedEntryState::DestinationChanged
                }
            }
            // Could not read it within bounds: do not guess either way.
            None => ManagedEntryState::Owned,
        },
        None => ManagedEntryState::Owned,
    }
}

/// Looks for files carrying an ArchiveFS in-file marker that no journal
/// accounts for.
fn scan_marked_files(
    target: &ManagedScanTarget,
    journalled: &BTreeSet<PathBuf>,
    scan: &mut ManagedEntryScan,
) {
    let Ok(read_dir) = fs::read_dir(&target.destination_root) else {
        return;
    };
    // Counts every directory entry seen, not just the `.pnach` files, so a
    // profile directory holding a huge number of unrelated files still stops
    // this scan rather than walking all of them.
    for (inspected, entry) in read_dir.filter_map(Result::ok).enumerate() {
        if inspected >= MAX_FILES_PER_PROFILE {
            scan.truncated = true;
            break;
        }
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "pnach")
        {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            scan.skipped.push(SkippedManagedFile {
                path: EncodedPath::from_path(&path),
                reason: "the file is a symlink, which ArchiveFS never follows",
            });
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let Some(bytes) = read_bounded(&path, &metadata, scan) else {
            continue;
        };
        match parse_pnach_document(&bytes) {
            Ok(document) => {
                let ids: Vec<String> = document.managed_block_ids().iter().cloned().collect();
                // No managed block at all means this is entirely the user's
                // file. Leave it alone and say nothing about it.
                //
                // An *empty* managed set after an uninstall is the same thing:
                // there is nothing of ArchiveFS's left, which is exactly what
                // a successful uninstall looks like. Never a finding.
                if ids.is_empty() {
                    continue;
                }
                if journalled.contains(&path) {
                    // Already accounted for by direction 1.
                    continue;
                }
                scan.entries.push(ManagedEntry {
                    format: target.format,
                    state: ManagedEntryState::OwnershipRecordMissing,
                    destination: EncodedPath::from_path(&path),
                    ownership_evidence: format!(
                        "the file carries {} ArchiveFS managed block marker(s)",
                        ids.len()
                    ),
                    operation_id: None,
                    game_identity: None,
                    source_archive: None,
                    profile_root: Some(EncodedPath::from_path(&target.destination_root)),
                    managed_block_ids: ids,
                    left_untouched: "Doctor read this file only. The managed blocks, and all of your own content around them, are untouched.",
                });
            }
            Err(error) => scan.malformed.push(MalformedManagedFile {
                format: target.format,
                path: EncodedPath::from_path(&path),
                detail: error.detail.clone(),
            }),
        }
    }
}

/// Bounded read. Refuses an oversized file and records why, rather than
/// reading it anyway or silently ignoring it.
fn read_bounded(
    path: &Path,
    metadata: &fs::Metadata,
    scan: &mut ManagedEntryScan,
) -> Option<Vec<u8>> {
    if metadata.len() > MAX_MANAGED_FILE_BYTES {
        scan.skipped.push(SkippedManagedFile {
            path: EncodedPath::from_path(path),
            reason: "the file is larger than the managed-file byte limit, so it was not read",
        });
        scan.truncated = true;
        return None;
    }
    if scan.bytes_read.saturating_add(metadata.len()) > MAX_TOTAL_MANAGED_BYTES {
        scan.skipped.push(SkippedManagedFile {
            path: EncodedPath::from_path(path),
            reason: "the scan reached its total byte budget before reaching this file",
        });
        scan.truncated = true;
        return None;
    }
    let bytes = fs::read(path).ok()?;
    scan.bytes_read = scan.bytes_read.saturating_add(bytes.len() as u64);
    Some(bytes)
}

fn digest_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// --- Findings -------------------------------------------------------------

/// Findings for managed entries that could not be accounted for.
///
/// No repair is offered: Stage 1C-A is diagnostic only, and removing a managed
/// entry touches a live emulator file. Each finding says so explicitly.
pub fn findings_from_managed_entries(scan: &ManagedEntryScan) -> Vec<Finding> {
    let mut findings = Vec::new();
    let orphans = scan.orphans();

    if orphans.len() <= MAX_INDIVIDUAL_ORPHAN_FINDINGS {
        for entry in &orphans {
            let severity = entry.state.severity().expect("filtered by `orphans`");
            let mut evidence = vec![
                format!("Managed format: {}", entry.format.label()),
                format!("Ownership evidence: {}", entry.ownership_evidence),
                format!("Why it is reported: {}", entry.state.reason()),
                format!("Left untouched: {}", entry.left_untouched),
            ];
            if let Some(identity) = &entry.game_identity {
                evidence.push(format!("Verified game identity at install: {identity}"));
            }
            if let Some(operation) = &entry.operation_id {
                evidence.push(format!("Install operation: {operation}"));
            }
            if let Some(archive) = &entry.source_archive {
                evidence.push(format!("Installed from: {}", archive.display));
            }
            if let Some(profile) = &entry.profile_root {
                evidence.push(format!("Emulator profile: {}", profile.display));
            }
            if !entry.managed_block_ids.is_empty() {
                evidence.push(format!(
                    "ArchiveFS managed block ids: {}",
                    entry.managed_block_ids.join(", ")
                ));
            }
            findings.push(
                Finding::new(
                    entry.state.finding_id(),
                    DoctorCategory::ManagedEntries,
                    DoctorSubsystem::ManagedEntries,
                    severity,
                    format!("{}: {}", entry.format.label(), entry.state.reason()),
                    format!(
                        "ArchiveFS installed an entry at {}, but {}.",
                        entry.destination.display,
                        entry.state.reason()
                    ),
                )
                .with_affected(entry.destination.clone())
                .with_evidence(evidence)
                .with_measurements(
                    [
                        ("managed_format", Measurement::text(entry.format.label())),
                        ("orphan_reason", Measurement::text(entry.state.reason())),
                        (
                            "ownership_proof",
                            Measurement::text(if entry.operation_id.is_some() {
                                "install_journal"
                            } else {
                                "in_file_marker"
                            }),
                        ),
                        (
                            "managed_block_count",
                            Measurement::Integer(entry.managed_block_ids.len() as u64),
                        ),
                    ]
                    .into_iter()
                    .chain(
                        entry
                            .operation_id
                            .as_ref()
                            .map(|id| ("operation_id", Measurement::text(id))),
                    ),
                )
                .with_guidance(
                    managed_why_it_matters(entry.state),
                    "No Doctor repair is available yet. Nothing has been changed, and the emulator file is exactly as it was.",
                ),
            );
        }
    } else {
        let mut evidence: Vec<String> = orphans
            .iter()
            .take(MAX_INDIVIDUAL_ORPHAN_FINDINGS)
            .map(|entry| {
                format!(
                    "{} at {}: {}",
                    entry.format.label(),
                    entry.destination.display,
                    entry.state.reason()
                )
            })
            .collect();
        evidence.push(format!(
            "... and {} more",
            orphans.len() - MAX_INDIVIDUAL_ORPHAN_FINDINGS
        ));
        evidence.push(format!(
            "Individual entries are not listed separately above {MAX_INDIVIDUAL_ORPHAN_FINDINGS}, so this one result covers them all."
        ));
        evidence.push(
            "Left untouched: every managed file was read only. Nothing was written, moved or removed."
                .to_string(),
        );
        findings.push(
            Finding::new(
                "managed_entry.several_unaccounted",
                DoctorCategory::ManagedEntries,
                DoctorSubsystem::ManagedEntries,
                orphans
                    .iter()
                    .filter_map(|entry| entry.state.severity())
                    .min_by_key(|severity| severity.rank())
                    .unwrap_or(DoctorSeverity::Info),
                "Several ArchiveFS-managed entries could not be accounted for",
                format!(
                    "{} managed cheat or patch entries no longer match ArchiveFS's own install records.",
                    orphans.len()
                ),
            )
            .with_evidence(evidence)
            .with_measurements([
                ("unaccounted_entries", Measurement::Integer(orphans.len() as u64)),
                ("individual_findings_truncated", Measurement::Flag(true)),
                (
                    "individual_finding_limit",
                    Measurement::Integer(MAX_INDIVIDUAL_ORPHAN_FINDINGS as u64),
                ),
                ("scan_truncated", Measurement::Flag(scan.truncated)),
            ])
            .with_guidance(
                "This is bookkeeping, not damage: the emulator files themselves are intact.",
                "No Doctor repair is available yet.",
            ),
        );
    }

    // Malformed markers, individually up to a small limit then aggregated.
    if !scan.malformed.is_empty() {
        if scan.malformed.len() <= MAX_INDIVIDUAL_MALFORMED_FINDINGS {
            for file in &scan.malformed {
                findings.push(
                    Finding::new(
                        ManagedEntryState::MalformedMarker.finding_id(),
                        DoctorCategory::ManagedEntries,
                        DoctorSubsystem::ManagedEntries,
                        DoctorSeverity::Warning,
                        format!("{}: broken ArchiveFS marker", file.format.label()),
                        format!(
                            "{} contains an ArchiveFS managed block ArchiveFS can no longer parse.",
                            file.path.display
                        ),
                    )
                    .with_affected(file.path.clone())
                    .with_evidence(vec![
                        format!("Parser detail: {}", file.detail),
                        format!("Managed format: {}", file.format.label()),
                        "Left untouched: the file was read only. Your own content in it is unaffected.".to_string(),
                    ])
                    .with_measurements([
                        ("managed_format", Measurement::text(file.format.label())),
                        (
                            "orphan_reason",
                            Measurement::text(ManagedEntryState::MalformedMarker.reason()),
                        ),
                    ])
                    .with_guidance(
                        "ArchiveFS will refuse to install into this file until its managed block structure is valid, to avoid corrupting whatever is in there.",
                        "No Doctor repair is available yet. The file can be corrected by hand, or the managed block removed.",
                    ),
                );
            }
        } else {
            findings.push(
                Finding::new(
                    "managed_entry.several_malformed_markers",
                    DoctorCategory::ManagedEntries,
                    DoctorSubsystem::ManagedEntries,
                    DoctorSeverity::Warning,
                    "Several files have a broken ArchiveFS marker",
                    format!(
                        "{} managed files contain an ArchiveFS managed block that can no longer be parsed.",
                        scan.malformed.len()
                    ),
                )
                .with_evidence(
                    scan.malformed
                        .iter()
                        .take(MAX_INDIVIDUAL_MALFORMED_FINDINGS)
                        .map(|file| format!("{}: {}", file.path.display, file.detail))
                        .chain(std::iter::once(format!(
                            "... and {} more",
                            scan.malformed.len() - MAX_INDIVIDUAL_MALFORMED_FINDINGS
                        )))
                        .collect::<Vec<_>>(),
                )
                .with_measurements([
                    (
                        "malformed_files",
                        Measurement::Integer(scan.malformed.len() as u64),
                    ),
                    ("individual_findings_truncated", Measurement::Flag(true)),
                    (
                        "individual_finding_limit",
                        Measurement::Integer(MAX_INDIVIDUAL_MALFORMED_FINDINGS as u64),
                    ),
                ])
                .with_guidance(
                    "ArchiveFS will refuse to install into these files until their managed block structure is valid.",
                    "No Doctor repair is available yet.",
                ),
            );
        }
    }
    findings
}

fn managed_why_it_matters(state: ManagedEntryState) -> &'static str {
    match state {
        ManagedEntryState::IncompleteInstall => {
            "An install that did not finish may have left an emulator file part-way between two states. History & Logs can roll it back."
        }
        ManagedEntryState::DestinationChanged => {
            "Something other than ArchiveFS edited this file, so ArchiveFS's record of it is out of date. Your edits are intact."
        }
        ManagedEntryState::DestinationMissing => {
            "The file is gone, so the cheat or patch is no longer installed. That is fine if you removed it deliberately."
        }
        ManagedEntryState::SourceGameMissing => {
            "The cheat is still installed for a game that is no longer in your library, so it does nothing."
        }
        ManagedEntryState::ProfileUnavailable => {
            "The emulator profile has moved or been removed, so this record can no longer be matched to anything."
        }
        ManagedEntryState::OwnershipRecordMissing => {
            "ArchiveFS wrote a managed block here but has no record of doing so - most likely its install history was cleared."
        }
        ManagedEntryState::MalformedMarker => {
            "ArchiveFS will not install into a file whose managed block it cannot parse."
        }
        ManagedEntryState::Owned | ManagedEntryState::RolledBack => "",
    }
}

/// What this check deliberately could not cover, stated on every run.
pub fn not_checked_from_managed_entries(scan: &ManagedEntryScan) -> Vec<NotCheckedCheck> {
    let mut items = Vec::new();
    // The honest headline limitation: two of four formats have no marker.
    let markerless: Vec<&'static str> = [
        ManagedFormat::DolphinGameSettings,
        ManagedFormat::XeniaPatch,
        ManagedFormat::RetroArchCheat,
    ]
    .iter()
    .filter(|format| !format.has_in_file_marker())
    .map(|format| format.label())
    .collect();
    items.push(NotCheckedCheck {
        name: "Managed entries with no install record".to_string(),
        reason: format!(
            "{} files carry no ArchiveFS marker inside them, so an entry whose install record was deleted cannot be recognised. ArchiveFS will not guess, because a user's own codes look identical.",
            markerless.join(", ")
        ),
        next_step: "Nothing to do. Keep ArchiveFS's install history and this stays covered."
            .to_string(),
    });
    if scan.truncated {
        items.push(NotCheckedCheck {
            name: "Complete managed-entry scan".to_string(),
            reason: "The scan reached one of its bounds, so some managed files were not inspected."
                .to_string(),
            next_step: "The bounds exist to keep a diagnostic fast and safe; the reported results are still accurate.".to_string(),
        });
    }
    for file in &scan.skipped {
        items.push(NotCheckedCheck {
            name: "One managed file".to_string(),
            reason: format!("{}: {}", file.path.display, file.reason),
            next_step: "Nothing to do.".to_string(),
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::tests::{TempTree, snapshot_tree};
    use crate::patch_manager::{
        PreviewDestinationState, PreviewProposedAction, SharedApplyContext, SharedApplyEntry,
        SharedApplyJournal, SharedApplyOutcome, SharedPlanEntry, SharedTransactionPath,
    };
    use std::os::unix::fs::symlink;

    const MANAGED_PNACH: &[u8] = b"// ArchiveFS managed block: op-1\npatch=1,EE,00100000,word,1\n// End ArchiveFS managed block\n";
    const USER_PNACH: &[u8] = b"// my own codes\npatch=1,EE,00200000,word,2\n";

    fn digest(bytes: &[u8]) -> String {
        digest_hex(bytes)
    }

    fn plan_entry(root: &Path, relative: &str) -> SharedPlanEntry {
        SharedPlanEntry {
            adapter: PreviewAdapter::Pcsx2,
            selected_archive: SharedTransactionPath::from_path(Path::new("/mnt/games/game.zip")),
            verified_game_identity: "SLUS-20946".to_string(),
            source_path: SharedTransactionPath::from_path(Path::new("/tmp/source.pnach")),
            source_digest: "0".repeat(64),
            destination_root: SharedTransactionPath::from_path(root),
            destination_relative_path: SharedTransactionPath::from_path(Path::new(relative)),
            destination_pre_state: PreviewDestinationState::Missing,
            destination_pre_digest: None,
            proposed_action: PreviewProposedAction::Install,
            backup_required: false,
            parent_creation_approved: false,
        }
    }

    fn apply_entry(root: &Path, relative: &str, final_digest: Option<String>) -> SharedApplyEntry {
        SharedApplyEntry {
            plan_entry: plan_entry(root, relative),
            destination_existed_before_apply: Some(false),
            destination_parent_existed_before_apply: Some(true),
            observed_source_digest: None,
            observed_destination_digest: None,
            backup_path: None,
            backup_digest: None,
            temporary_path: None,
            final_destination_digest: final_digest,
            created_directories: Vec::new(),
            replacement_approved: false,
            verification_succeeded: true,
            outcome: SharedApplyOutcome::InstalledNew,
            stages: Vec::new(),
            warnings: Vec::new(),
            failures: Vec::new(),
        }
    }

    fn journal(
        archive: &Path,
        entries: Vec<SharedApplyEntry>,
        status: SharedApplyStatus,
    ) -> SharedApplyJournal {
        SharedApplyJournal {
            schema_version: 1,
            operation_id: "op-1".to_string(),
            plan_id: "plan-1".to_string(),
            timestamp_unix_seconds: 1_700_000_000,
            context: SharedApplyContext {
                adapter: PreviewAdapter::Pcsx2,
                selected_archive: SharedTransactionPath::from_path(archive),
                verified_game_identity: "SLUS-20946".to_string(),
                profile_id: "pcsx2-native".to_string(),
                source_mode: "archive".to_string(),
            },
            approved_source_root: SharedTransactionPath::from_path(Path::new("/tmp")),
            destination_root: SharedTransactionPath::from_path(Path::new("/tmp")),
            dry_run: false,
            entries,
            status,
            rollback_operation_id: None,
        }
    }

    fn history(journals: Vec<SharedApplyJournal>) -> SharedHistoryReport {
        SharedHistoryReport {
            journals: journals
                .into_iter()
                .map(|journal| {
                    (
                        SharedTransactionPath::from_path(Path::new("/tmp/journal.json")),
                        journal,
                    )
                })
                .collect(),
            warnings: Vec::new(),
            complete: true,
        }
    }

    /// A profile directory with one archive present, so the "source game is
    /// gone" branch is not triggered by accident.
    fn fixture(tree: &TempTree) -> (PathBuf, PathBuf) {
        let profile = tree.path().join("patches");
        fs::create_dir_all(&profile).expect("fixture");
        let archive = tree.path().join("game.zip");
        fs::write(&archive, b"archive").expect("fixture");
        (profile, archive)
    }

    /// Test 53
    #[test]
    fn an_intact_managed_entry_is_accounted_for_and_produces_no_finding() {
        let tree = TempTree::new("managed-owned");
        let (profile, archive) = fixture(&tree);
        fs::write(profile.join("SLUS-20946.pnach"), MANAGED_PNACH).expect("fixture");
        let history = history(vec![journal(
            &archive,
            vec![apply_entry(
                &profile,
                "SLUS-20946.pnach",
                Some(digest(MANAGED_PNACH)),
            )],
            SharedApplyStatus::Success,
        )]);
        let scan = scan_managed_entries(&history, &[]);
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].state, ManagedEntryState::Owned);
        assert!(scan.orphans().is_empty());
        assert!(findings_from_managed_entries(&scan).is_empty());
    }

    /// Test 54
    #[test]
    fn a_managed_entry_whose_file_is_gone_is_reported() {
        let tree = TempTree::new("managed-missing");
        let (profile, archive) = fixture(&tree);
        let history = history(vec![journal(
            &archive,
            vec![apply_entry(
                &profile,
                "SLUS-20946.pnach",
                Some(digest(MANAGED_PNACH)),
            )],
            SharedApplyStatus::Success,
        )]);
        let scan = scan_managed_entries(&history, &[]);
        assert_eq!(scan.entries[0].state, ManagedEntryState::DestinationMissing);
        let findings = findings_from_managed_entries(&scan);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "managed_entry.destination_missing");
        assert_eq!(
            findings[0].severity,
            DoctorSeverity::Info,
            "a deliberately removed cheat is not damage"
        );
    }

    /// Test 55
    #[test]
    fn a_managed_entry_someone_else_edited_is_reported_as_changed() {
        let tree = TempTree::new("managed-changed");
        let (profile, archive) = fixture(&tree);
        fs::write(profile.join("SLUS-20946.pnach"), b"edited by hand\n").expect("fixture");
        let history = history(vec![journal(
            &archive,
            vec![apply_entry(
                &profile,
                "SLUS-20946.pnach",
                Some(digest(MANAGED_PNACH)),
            )],
            SharedApplyStatus::Success,
        )]);
        let scan = scan_managed_entries(&history, &[]);
        assert_eq!(scan.entries[0].state, ManagedEntryState::DestinationChanged);
        let findings = findings_from_managed_entries(&scan);
        assert!(
            findings[0]
                .why_it_matters
                .as_deref()
                .expect("guidance")
                .contains("Your edits are intact"),
            "a person must be told their own change was not undone"
        );
    }

    /// Test 56
    #[test]
    fn an_entry_that_was_already_rolled_back_is_never_reported() {
        let tree = TempTree::new("managed-rolled-back");
        let (profile, archive) = fixture(&tree);
        let mut journal = journal(
            &archive,
            vec![apply_entry(&profile, "SLUS-20946.pnach", None)],
            SharedApplyStatus::Success,
        );
        journal.rollback_operation_id = Some("op-2".to_string());
        let scan = scan_managed_entries(&history(vec![journal]), &[]);
        assert_eq!(scan.entries[0].state, ManagedEntryState::RolledBack);
        assert!(
            findings_from_managed_entries(&scan).is_empty(),
            "a recorded rollback is a completed removal, not an orphan"
        );
    }

    /// Test 57
    #[test]
    fn an_install_that_did_not_finish_is_a_warning() {
        let tree = TempTree::new("managed-partial");
        let (profile, archive) = fixture(&tree);
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(&profile, "SLUS-20946.pnach", None)],
                SharedApplyStatus::PartialFailure,
            )]),
            &[],
        );
        assert_eq!(scan.entries[0].state, ManagedEntryState::IncompleteInstall);
        let findings = findings_from_managed_entries(&scan);
        assert_eq!(findings[0].severity, DoctorSeverity::Warning);
        assert_eq!(findings[0].id, "managed_entry.incomplete_install");
    }

    /// Test 58
    #[test]
    fn a_cheat_installed_for_a_game_that_is_gone_is_reported() {
        let tree = TempTree::new("managed-source-gone");
        let (profile, archive) = fixture(&tree);
        fs::write(profile.join("SLUS-20946.pnach"), MANAGED_PNACH).expect("fixture");
        fs::remove_file(&archive).expect("fixture");
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(
                    &profile,
                    "SLUS-20946.pnach",
                    Some(digest(MANAGED_PNACH)),
                )],
                SharedApplyStatus::Success,
            )]),
            &[],
        );
        assert_eq!(scan.entries[0].state, ManagedEntryState::SourceGameMissing);
        assert_eq!(
            findings_from_managed_entries(&scan)[0].id,
            "managed_entry.source_game_missing"
        );
    }

    /// Test 59
    #[test]
    fn a_profile_that_no_longer_exists_is_reported_as_unavailable() {
        let tree = TempTree::new("managed-profile-gone");
        let (_, archive) = fixture(&tree);
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(
                    &tree.path().join("removed-profile"),
                    "SLUS-20946.pnach",
                    None,
                )],
                SharedApplyStatus::Success,
            )]),
            &[],
        );
        assert_eq!(scan.entries[0].state, ManagedEntryState::ProfileUnavailable);
    }

    /// Test 60
    #[test]
    fn a_dry_run_journal_is_never_treated_as_an_install() {
        let tree = TempTree::new("managed-dry-run");
        let (profile, archive) = fixture(&tree);
        let mut journal = journal(
            &archive,
            vec![apply_entry(&profile, "SLUS-20946.pnach", None)],
            SharedApplyStatus::DryRun,
        );
        journal.dry_run = true;
        let scan = scan_managed_entries(&history(vec![journal]), &[]);
        assert!(
            scan.entries.is_empty(),
            "a dry run wrote nothing, so it owns nothing"
        );
    }

    /// Test 61
    #[test]
    fn an_entry_that_installed_nothing_is_never_treated_as_owned() {
        let tree = TempTree::new("managed-skipped");
        let (profile, archive) = fixture(&tree);
        let mut entry = apply_entry(&profile, "SLUS-20946.pnach", None);
        entry.outcome = SharedApplyOutcome::SkippedConflict;
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![entry],
                SharedApplyStatus::Success,
            )]),
            &[],
        );
        assert!(scan.entries.is_empty());
    }

    /// Test 62
    #[test]
    fn a_pnach_with_an_archivefs_marker_and_no_install_record_is_reported() {
        let tree = TempTree::new("managed-no-record");
        let (profile, _) = fixture(&tree);
        fs::write(profile.join("SLUS-20946.pnach"), MANAGED_PNACH).expect("fixture");
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile.clone(),
            }],
        );
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(
            scan.entries[0].state,
            ManagedEntryState::OwnershipRecordMissing
        );
        assert_eq!(scan.entries[0].managed_block_ids, vec!["op-1".to_string()]);
        let findings = findings_from_managed_entries(&scan);
        assert_eq!(findings[0].id, "managed_entry.ownership_record_missing");
        assert!(
            findings[0]
                .evidence
                .iter()
                .any(|item| item.contains("managed block marker")),
            "the proof of ownership must be stated"
        );
    }

    /// Test 63
    #[test]
    fn a_users_own_pnach_is_never_reported_as_orphaned() {
        let tree = TempTree::new("managed-user-owned");
        let (profile, _) = fixture(&tree);
        fs::write(profile.join("SLUS-20946.pnach"), USER_PNACH).expect("fixture");
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        assert!(
            scan.entries.is_empty(),
            "a file with no ArchiveFS marker is the user's, and ArchiveFS must not claim it"
        );
        assert!(findings_from_managed_entries(&scan).is_empty());
    }

    /// Test 64
    #[test]
    fn a_managed_file_left_empty_after_an_uninstall_is_not_damage() {
        let tree = TempTree::new("managed-empty-section");
        let (profile, _) = fixture(&tree);
        // What a successful uninstall leaves behind: the user's own content,
        // with every ArchiveFS block removed.
        fs::write(profile.join("SLUS-20946.pnach"), USER_PNACH).expect("fixture");
        fs::write(profile.join("SLUS-00001.pnach"), b"").expect("fixture");
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        assert!(
            scan.orphans().is_empty(),
            "no remaining managed block means nothing of ArchiveFS's is left, which is correct"
        );
    }

    /// Test 65
    #[test]
    fn a_broken_archivefs_marker_is_reported_with_the_parsers_own_detail() {
        let tree = TempTree::new("managed-malformed");
        let (profile, _) = fixture(&tree);
        // A start marker with no end marker: the existing parser's
        // `MalformedManagedBlock` case.
        fs::write(
            profile.join("SLUS-20946.pnach"),
            b"// ArchiveFS managed block: op-1\npatch=1,EE,00100000,word,1\n",
        )
        .expect("fixture");
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        assert_eq!(scan.malformed.len(), 1);
        assert!(!scan.malformed[0].detail.is_empty());
        let findings = findings_from_managed_entries(&scan);
        assert_eq!(findings[0].id, "managed_entry.malformed_marker");
        assert_eq!(findings[0].severity, DoctorSeverity::Warning);
    }

    /// Test 66
    #[test]
    fn many_unaccounted_entries_collapse_into_one_result() {
        let tree = TempTree::new("managed-flood");
        let (profile, archive) = fixture(&tree);
        let entries: Vec<SharedApplyEntry> = (0..MAX_INDIVIDUAL_ORPHAN_FINDINGS + 3)
            .map(|index| apply_entry(&profile, &format!("SLUS-{index:05}.pnach"), None))
            .collect();
        let scan = scan_managed_entries(
            &history(vec![journal(&archive, entries, SharedApplyStatus::Success)]),
            &[],
        );
        let findings = findings_from_managed_entries(&scan);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "managed_entry.several_unaccounted");
        assert!(
            findings[0]
                .evidence
                .iter()
                .any(|item| item.contains("and 3 more"))
        );
        assert_eq!(
            findings[0].measurements.get("unaccounted_entries"),
            Some(&Measurement::Integer(
                (MAX_INDIVIDUAL_ORPHAN_FINDINGS + 3) as u64
            ))
        );
    }

    /// Test 67
    #[test]
    fn exactly_the_limit_is_still_listed_individually() {
        let tree = TempTree::new("managed-at-limit");
        let (profile, archive) = fixture(&tree);
        let entries: Vec<SharedApplyEntry> = (0..MAX_INDIVIDUAL_ORPHAN_FINDINGS)
            .map(|index| apply_entry(&profile, &format!("SLUS-{index:05}.pnach"), None))
            .collect();
        let scan = scan_managed_entries(
            &history(vec![journal(&archive, entries, SharedApplyStatus::Success)]),
            &[],
        );
        assert_eq!(
            findings_from_managed_entries(&scan).len(),
            MAX_INDIVIDUAL_ORPHAN_FINDINGS
        );
    }

    /// Test 68
    #[test]
    fn many_malformed_files_collapse_into_one_result() {
        let tree = TempTree::new("managed-many-malformed");
        let (profile, _) = fixture(&tree);
        for index in 0..MAX_INDIVIDUAL_MALFORMED_FINDINGS + 2 {
            fs::write(
                profile.join(format!("SLUS-{index:05}.pnach")),
                b"// ArchiveFS managed block: op\npatch=1,EE,00100000,word,1\n",
            )
            .expect("fixture");
        }
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        let findings = findings_from_managed_entries(&scan);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "managed_entry.several_malformed_markers");
    }

    /// Test 69
    #[test]
    fn an_oversized_file_is_skipped_and_said_to_be_skipped() {
        let tree = TempTree::new("managed-oversized");
        let (profile, _) = fixture(&tree);
        let mut oversized = Vec::from(MANAGED_PNACH);
        oversized.resize(MAX_MANAGED_FILE_BYTES as usize + 1, b'\n');
        fs::write(profile.join("SLUS-20946.pnach"), &oversized).expect("fixture");
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        assert!(scan.entries.is_empty());
        assert!(scan.truncated);
        assert_eq!(scan.skipped.len(), 1);
        assert!(
            not_checked_from_managed_entries(&scan)
                .iter()
                .any(|item| item.reason.contains("byte limit")),
            "a file that was not read must be declared, not silently passed"
        );
    }

    /// Test 70
    #[test]
    fn the_total_byte_budget_stops_the_scan_rather_than_reading_on() {
        let tree = TempTree::new("managed-budget");
        let (profile, _) = fixture(&tree);
        let chunk = vec![b'\n'; 400 * 1024];
        for index in 0..48 {
            let mut contents = Vec::from(MANAGED_PNACH);
            contents.extend_from_slice(&chunk);
            fs::write(profile.join(format!("SLUS-{index:05}.pnach")), &contents).expect("fixture");
        }
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        assert!(scan.bytes_read <= MAX_TOTAL_MANAGED_BYTES);
        assert!(scan.truncated);
        assert!(!scan.skipped.is_empty());
    }

    /// Test 71
    #[test]
    fn a_symlinked_managed_file_is_never_followed() {
        let tree = TempTree::new("managed-symlink");
        let (profile, _) = fixture(&tree);
        let real = tree.path().join("elsewhere.pnach");
        fs::write(&real, MANAGED_PNACH).expect("fixture");
        symlink(&real, profile.join("SLUS-20946.pnach")).expect("fixture");
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        assert!(scan.entries.is_empty());
        assert_eq!(scan.skipped.len(), 1);
        assert!(scan.skipped[0].reason.contains("symlink"));
    }

    /// Test 72
    #[test]
    fn a_journalled_destination_that_is_a_symlink_is_not_read_through() {
        let tree = TempTree::new("managed-journal-symlink");
        let (profile, archive) = fixture(&tree);
        let real = tree.path().join("elsewhere.pnach");
        fs::write(&real, MANAGED_PNACH).expect("fixture");
        symlink(&real, profile.join("SLUS-20946.pnach")).expect("fixture");
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(
                    &profile,
                    "SLUS-20946.pnach",
                    Some(digest(MANAGED_PNACH)),
                )],
                SharedApplyStatus::Success,
            )]),
            &[],
        );
        assert_eq!(scan.entries[0].state, ManagedEntryState::DestinationChanged);
        assert!(
            scan.skipped
                .iter()
                .any(|file| file.reason.contains("symlink"))
        );
    }

    /// Test 73
    #[test]
    fn a_file_already_covered_by_a_journal_is_not_reported_twice() {
        let tree = TempTree::new("managed-no-double-count");
        let (profile, archive) = fixture(&tree);
        fs::write(profile.join("SLUS-20946.pnach"), MANAGED_PNACH).expect("fixture");
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(
                    &profile,
                    "SLUS-20946.pnach",
                    Some(digest(MANAGED_PNACH)),
                )],
                SharedApplyStatus::Success,
            )]),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        assert_eq!(
            scan.entries.len(),
            1,
            "one file must produce one entry, whichever direction found it"
        );
        assert_eq!(scan.entries[0].state, ManagedEntryState::Owned);
    }

    /// Test 74
    #[test]
    fn a_format_with_no_in_file_marker_is_never_walked_for_orphans() {
        let tree = TempTree::new("managed-no-marker-format");
        let (profile, _) = fixture(&tree);
        fs::write(profile.join("GALE01.ini"), b"[Gecko_Enabled]\n$Some code\n").expect("fixture");
        let scan = scan_managed_entries(
            &history(Vec::new()),
            &[ManagedScanTarget {
                format: ManagedFormat::DolphinGameSettings,
                destination_root: profile,
            }],
        );
        assert!(scan.entries.is_empty());
        assert!(scan.marker_scanned_formats.is_empty());
        assert!(
            !ManagedFormat::DolphinGameSettings.has_in_file_marker(),
            "Dolphin GameSettings carries no ArchiveFS marker on this build"
        );
    }

    /// Test 75
    #[test]
    fn the_absence_of_a_marker_for_three_formats_is_stated_on_every_run() {
        let scan = scan_managed_entries(&history(Vec::new()), &[]);
        let not_checked = not_checked_from_managed_entries(&scan);
        let item = not_checked
            .iter()
            .find(|item| item.name == "Managed entries with no install record")
            .expect("the honest limitation must always be reported");
        for label in [
            "Dolphin GameSettings",
            "Xenia patch",
            "RetroArch cheat file",
        ] {
            assert!(
                item.reason.contains(label),
                "`{label}` has no marker and that must be named"
            );
        }
        assert!(item.reason.contains("will not guess"));
    }

    /// Test 76
    #[test]
    fn a_managed_finding_carries_machine_readable_values() {
        let tree = TempTree::new("managed-measurements");
        let (profile, archive) = fixture(&tree);
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(&profile, "SLUS-20946.pnach", None)],
                SharedApplyStatus::Success,
            )]),
            &[],
        );
        let findings = findings_from_managed_entries(&scan);
        let measurements = &findings[0].measurements;
        assert_eq!(
            measurements.get("managed_format"),
            Some(&Measurement::Text("PCSX2 PNACH".to_string()))
        );
        assert_eq!(
            measurements.get("orphan_reason"),
            Some(&Measurement::Text(
                ManagedEntryState::DestinationMissing.reason().to_string()
            ))
        );
        assert_eq!(
            measurements.get("ownership_proof"),
            Some(&Measurement::Text("install_journal".to_string()))
        );
    }

    /// Test 77
    #[test]
    fn more_scan_targets_than_the_bound_marks_the_result_partial() {
        let tree = TempTree::new("managed-target-bound");
        let (profile, _) = fixture(&tree);
        let targets: Vec<ManagedScanTarget> = (0..MAX_SCANNED_PROFILES + 1)
            .map(|_| ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile.clone(),
            })
            .collect();
        let scan = scan_managed_entries(&history(Vec::new()), &targets);
        assert!(scan.truncated);
    }

    /// Test 78
    #[test]
    fn every_reported_entry_states_what_was_left_untouched() {
        let tree = TempTree::new("managed-untouched-wording");
        let (profile, archive) = fixture(&tree);
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(&profile, "SLUS-20946.pnach", None)],
                SharedApplyStatus::Success,
            )]),
            &[],
        );
        for entry in scan.orphans() {
            assert!(entry.left_untouched.contains("Nothing was written"));
        }
        for finding in findings_from_managed_entries(&scan) {
            assert!(
                finding
                    .evidence
                    .iter()
                    .any(|item| item.starts_with("Left untouched:")),
                "{} must say what Doctor did not do",
                finding.id
            );
        }
    }

    /// Read-only proof 7: scanning a real profile tree with journals, managed
    /// files, user files and a malformed file changes nothing at all.
    #[test]
    fn scanning_managed_entries_leaves_the_tree_byte_for_byte_unchanged() {
        let tree = TempTree::new("managed-read-only-proof");
        let (profile, archive) = fixture(&tree);
        fs::write(profile.join("SLUS-20946.pnach"), MANAGED_PNACH).expect("fixture");
        fs::write(profile.join("SLUS-00002.pnach"), USER_PNACH).expect("fixture");
        fs::write(
            profile.join("SLUS-00003.pnach"),
            b"// ArchiveFS managed block: broken\npatch=1,EE,00100000,word,1\n",
        )
        .expect("fixture");
        let before = snapshot_tree(tree.path());

        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(
                    &profile,
                    "SLUS-20946.pnach",
                    Some(digest(MANAGED_PNACH)),
                )],
                SharedApplyStatus::Success,
            )]),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        let _ = findings_from_managed_entries(&scan);
        let _ = not_checked_from_managed_entries(&scan);

        assert_eq!(
            snapshot_tree(tree.path()),
            before,
            "a managed-entry scan must not remove an orphan, repair a marker or write anything"
        );
    }

    /// Read-only proof 8: no orphan finding offers a repair, so no Delete,
    /// Clean, Repair or Fix button can exist for one in this milestone.
    #[test]
    fn no_managed_finding_offers_a_repair_or_names_a_destructive_action() {
        let tree = TempTree::new("managed-no-repair");
        let (profile, archive) = fixture(&tree);
        fs::write(
            profile.join("SLUS-00003.pnach"),
            b"// ArchiveFS managed block: broken\npatch=1,EE,00100000,word,1\n",
        )
        .expect("fixture");
        let scan = scan_managed_entries(
            &history(vec![journal(
                &archive,
                vec![apply_entry(&profile, "SLUS-20946.pnach", None)],
                SharedApplyStatus::Success,
            )]),
            &[ManagedScanTarget {
                format: ManagedFormat::Pcsx2Pnach,
                destination_root: profile,
            }],
        );
        let findings = findings_from_managed_entries(&scan);
        assert!(!findings.is_empty());
        for finding in &findings {
            assert!(finding.repair.is_none(), "{} offers a repair", finding.id);
            assert!(
                finding.recovery.is_none(),
                "{} advertises a repair",
                finding.id
            );
            let next = finding.next_step.as_deref().unwrap_or_default();
            assert!(
                next.contains("No Doctor repair is available yet"),
                "{} must say plainly that nothing will be done for it",
                finding.id
            );
            for forbidden in ["delete", "clean", "repair now", "fix"] {
                assert!(
                    !finding.id.contains(forbidden),
                    "{} reads like an action rather than an observation",
                    finding.id
                );
            }
        }
    }

    /// Read-only proof 9: this module contains no write, delete,
    /// permission-changing or process-spawning call.
    #[test]
    fn this_module_contains_no_mutating_call() {
        let whole = include_str!("managed.rs");
        let source = whole
            .split_once("#[cfg(test)]")
            .expect("this file ends with its own test module")
            .0;
        for forbidden in [
            "fs::write",
            "fs::create_dir",
            "fs::remove_",
            "fs::rename",
            "fs::set_permissions",
            "File::create",
            "OpenOptions",
            "Command",
            "ureq",
            "merge_managed_pnach_cheats",
            "execute_shared_rollback",
            "execute_cheat_rollback",
            "execute_shared_apply",
        ] {
            assert!(
                !source.contains(forbidden),
                "`{forbidden}` must never appear in a read-only diagnostic module"
            );
        }
    }
}
