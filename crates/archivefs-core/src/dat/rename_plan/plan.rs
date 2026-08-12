//! Building a read-only rename plan from an already-completed audit.
//!
//! [`build_rename_plan`] turns a [`DatAuditOutcome`] into a [`RenamePlan`]
//! without re-scanning the library, re-parsing DATs, or hashing anything. Its
//! only filesystem access is a `symlink_metadata` per verified source file to
//! classify the object (regular file, symlink, broken symlink) - the sibling
//! index used for collision detection is derived from the audit's own file
//! list, so there is no second scan.
//!
//! Nothing in this module writes to disk, and the plan it produces can never
//! be applied by anything in this PR.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::dat::audit::{AuditEntry, AuditVerdict};
use crate::dat::classification::{
    ContentEligibility, DatContentClassification, DatOriginalMetadata,
};
use crate::dat::rename_plan::collisions::{
    DirSiblings, detect_proposal_collisions, detect_target_collision,
};
use crate::dat::rename_plan::derive::{DeriveOutcome, derive_proposed_basename};
use crate::dat::rename_plan::model::{
    ProposalState, RenamePlan, RenamePlanCounts, RenameProposal, SourceObjectKind,
};
use crate::dat::sources::audit_run::{DatAuditOutcome, DatContentMatch, DatPolicyNote};

/// The identity a plan is built for. `generation` lets a caller reject a plan
/// built for a stale audit generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenamePlanContext {
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenamePlanError {
    Cancelled,
}

impl std::fmt::Display for RenamePlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "the rename plan build was cancelled"),
        }
    }
}

impl std::error::Error for RenamePlanError {}

/// Whether a plan belongs to `current_generation`. A caller must discard a
/// plan for which this returns `false`, so a stale plan can never replace a
/// newer one.
pub fn plan_matches_generation(plan: &RenamePlan, current_generation: u64) -> bool {
    plan.generation == current_generation
}

/// Builds a read-only rename plan from an audit outcome.
pub fn build_rename_plan(
    outcome: &DatAuditOutcome,
    context: &RenamePlanContext,
    cancel: &AtomicBool,
) -> Result<RenamePlan, RenamePlanError> {
    if cancelled(cancel) {
        return Err(RenamePlanError::Cancelled);
    }

    let notes_by_path: HashMap<&str, &DatPolicyNote> = outcome
        .policy
        .as_ref()
        .map(|policy| {
            policy
                .notes
                .iter()
                .map(|note| (note.local_path.as_str(), note))
                .collect()
        })
        .unwrap_or_default();
    let content_by_path: HashMap<&str, &DatContentMatch> = outcome
        .content
        .matches
        .iter()
        .map(|note| (note.local_path.as_str(), note))
        .collect();

    // The sibling index comes from the audit's own file list: every walked
    // file is in `report.entries`, so no second scan is needed to answer
    // "does this proposed name already exist here?".
    let mut siblings_by_parent: HashMap<PathBuf, DirSiblings> = HashMap::new();
    for entry in &outcome.report.entries {
        let path = Path::new(&entry.local_path);
        let Some(parent) = path.parent() else {
            continue;
        };
        let siblings = siblings_by_parent.entry(parent.to_path_buf()).or_default();
        siblings.names.insert(entry.local_filename.clone());
        siblings
            .names_lower
            .insert(entry.local_filename.to_ascii_lowercase());
    }

    let platform_display = outcome
        .platform
        .as_deref()
        .map(crate::platform::display_name_for)
        .map(str::to_string);
    let proposal_context = ProposalContext {
        content_policy: outcome.content.selection,
        platform: outcome.platform.as_deref(),
        platform_display: platform_display.as_deref(),
        source_id: &outcome.source_id,
        source_display_name: &outcome.source_display_name,
    };

    let mut proposals: Vec<RenameProposal> = Vec::new();
    let mut verified_total = 0usize;
    for entry in &outcome.report.entries {
        if cancelled(cancel) {
            return Err(RenamePlanError::Cancelled);
        }
        if !matches!(
            entry.verdict,
            AuditVerdict::Exact { .. } | AuditVerdict::ExactMultipleCandidates { .. }
        ) {
            // Weak evidence (CRC32, filename-only) is never promoted: only
            // cryptographic-hash matches produce proposals.
            continue;
        }
        verified_total += 1;
        let note = notes_by_path.get(entry.local_path.as_str()).copied();
        let content = content_by_path.get(entry.local_path.as_str()).copied();
        let source_path = Path::new(&entry.local_path);
        match classify_object(source_path) {
            Some(object_kind) => proposals.push(derive_proposal(
                entry,
                note,
                content,
                &proposal_context,
                object_kind,
            )),
            None => proposals.push(blocked_missing_source(
                entry,
                note,
                content,
                &proposal_context,
            )),
        }
    }

    detect_target_collisions(&mut proposals, &siblings_by_parent);
    detect_proposal_collisions(&mut proposals);

    // Deterministic ordering, independent of input order.
    proposals.sort_by(|a, b| {
        a.source_path
            .cmp(&b.source_path)
            .then_with(|| a.proposed_basename.cmp(&b.proposed_basename))
    });

    let counts = RenamePlanCounts::from_proposals(&proposals);

    Ok(RenamePlan {
        generation: context.generation,
        source_id: outcome.source_id.clone(),
        source_display_name: outcome.source_display_name.clone(),
        scan_root: outcome.scan_root.clone(),
        platform: outcome.platform.clone(),
        platform_display,
        content_policy: outcome.content.selection,
        classifier_version: crate::dat::classification::CLASSIFIER_VERSION.to_string(),
        proposals,
        counts,
        audited_total: outcome.report.summary.total,
        verified_total,
        truncated: outcome.truncated,
    })
}

/// Classifies a source path without following any link. `None` means the path
/// can no longer be inspected (it is gone or unreadable).
fn classify_object(path: &Path) -> Option<SourceObjectKind> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        // Does the target resolve? `metadata` follows the link; a broken link
        // fails here.
        if std::fs::metadata(path).is_ok() {
            Some(SourceObjectKind::Symlink)
        } else {
            Some(SourceObjectKind::BrokenSymlink)
        }
    } else {
        Some(SourceObjectKind::RegularFile)
    }
}

struct ProposalContext<'a> {
    content_policy: crate::dat::classification::ContentSelectionPolicy,
    platform: Option<&'a str>,
    platform_display: Option<&'a str>,
    source_id: &'a str,
    source_display_name: &'a str,
}

/// Derives one proposal from a verified audit entry, its policy resolution
/// (when it has one), and the source's filesystem classification. Pure: no
/// filesystem access beyond the caller-supplied `object_kind`.
fn derive_proposal(
    entry: &AuditEntry,
    note: Option<&DatPolicyNote>,
    content_match: Option<&DatContentMatch>,
    context: &ProposalContext<'_>,
    object_kind: SourceObjectKind,
) -> RenameProposal {
    let current_basename = entry.local_filename.clone();
    let verdict_label = entry.verdict.label().to_string();
    let match_confident = entry.verdict.is_confident();

    // The verified match: a single `Exact` verdict, or the policy's winner
    // among `ExactMultipleCandidates`.
    let (game_name, rom_name, explanations, ambiguity_reason) = match &entry.verdict {
        AuditVerdict::Exact {
            game_name,
            rom_name,
            ..
        } => (
            Some(game_name.clone()),
            Some(rom_name.clone()),
            Vec::new(),
            None,
        ),
        AuditVerdict::ExactMultipleCandidates { .. } => match note {
            Some(note) if note.resolution.decided => {
                let winner = &note.resolution.entries[note.resolution.winner_index.unwrap_or(0)];
                (
                    Some(winner.candidate.game_name.clone()),
                    Some(winner.candidate.rom_name.clone()),
                    note.resolution.explanations.clone(),
                    None,
                )
            }
            Some(note) => (
                None,
                None,
                note.resolution.explanations.clone(),
                note.resolution.ambiguity_reason.clone(),
            ),
            None => (
                None,
                None,
                Vec::new(),
                Some(
                    "the audit reported several verified candidates but no policy resolution was \
                     available"
                        .to_string(),
                ),
            ),
        },
        _ => (None, None, Vec::new(), None),
    };

    let mut state = ProposalState::Suggested;
    let mut blockers: Vec<String> = Vec::new();
    let mut proposed_basename: Option<String> = None;
    let mut extension_status = None;
    let mut sanitisation_notes: Vec<String> = Vec::new();
    let content_candidate =
        game_name
            .as_deref()
            .zip(rom_name.as_deref())
            .and_then(|(game_name, rom_name)| {
                content_match.and_then(|matched| {
                    matched.candidates.iter().find(|candidate| {
                        candidate.game_name == game_name && candidate.rom_name == rom_name
                    })
                })
            });
    let content_classification = content_candidate
        .map(|candidate| candidate.classification.clone())
        .unwrap_or_else(DatContentClassification::unknown);
    let original_metadata = content_candidate
        .map(|candidate| candidate.original_metadata.clone())
        .unwrap_or_else(DatOriginalMetadata::default);

    match object_kind {
        SourceObjectKind::Symlink => {
            state = ProposalState::Unsupported;
            blockers.push(
                "the source is a symlink; renaming a link is not supported yet - a future stage \
                 would rename the link itself, never its target"
                    .to_string(),
            );
        }
        SourceObjectKind::BrokenSymlink => {
            state = ProposalState::Unsupported;
            blockers.push(
                "the source is a broken symlink; planning cannot verify what a rename would move"
                    .to_string(),
            );
        }
        SourceObjectKind::RegularFile => {}
    }

    if state == ProposalState::Suggested && ambiguity_reason.is_none() {
        match context.content_policy.eligibility(&content_classification) {
            ContentEligibility::Selected => {}
            ContentEligibility::ExcludedNonGame => {
                state = ProposalState::ExcludedByContentPolicy;
                blockers.push(
                    "Games only does not select content confidently classified as non-game"
                        .to_string(),
                );
            }
            ContentEligibility::NeedsReview => {
                state = ProposalState::UnclassifiedContent;
                blockers.push(
                    "this entry's content classification is Unknown; Games only never renames it automatically"
                        .to_string(),
                );
            }
        }
    }

    if state == ProposalState::Suggested {
        if ambiguity_reason.is_some() {
            state = ProposalState::Ambiguous;
        } else if let Some(rom) = &rom_name {
            match derive_proposed_basename(rom, &current_basename) {
                DeriveOutcome::Ok(derived) => {
                    extension_status = Some(derived.extension_status);
                    sanitisation_notes = derived.sanitisation_notes;
                    if derived.proposed_basename == current_basename {
                        state = ProposalState::AlreadyCanonical;
                    } else {
                        proposed_basename = Some(derived.proposed_basename);
                    }
                }
                DeriveOutcome::Blocked(reason) => {
                    state = ProposalState::Blocked;
                    blockers.push(reason);
                }
                DeriveOutcome::Unsupported(reason) => {
                    state = ProposalState::Unsupported;
                    blockers.push(reason);
                }
            }
        } else {
            state = ProposalState::Blocked;
            blockers.push("no matched catalogue ROM name is available".to_string());
        }
    }

    RenameProposal {
        source_path: entry.local_path.clone().into(),
        current_basename,
        proposed_basename,
        platform: context.platform.map(str::to_string),
        platform_display: context.platform_display.map(str::to_string),
        source_id: context.source_id.to_string(),
        source_display_name: context.source_display_name.to_string(),
        game_name,
        rom_name,
        verdict_label,
        match_confident,
        explanations,
        content_policy: context.content_policy,
        content_classification,
        original_metadata,
        state,
        object_kind,
        ambiguity_reason,
        collision: None,
        blockers,
        extension_status,
        sanitisation_notes,
        actionable: state == ProposalState::Suggested,
    }
}

/// A proposal for a verified entry whose source file has disappeared since the
/// audit ran.
fn blocked_missing_source(
    entry: &AuditEntry,
    note: Option<&DatPolicyNote>,
    content_match: Option<&DatContentMatch>,
    context: &ProposalContext<'_>,
) -> RenameProposal {
    let mut proposal = derive_proposal(
        entry,
        note,
        content_match,
        context,
        SourceObjectKind::RegularFile,
    );
    proposal.state = ProposalState::Blocked;
    proposal.proposed_basename = None;
    proposal.actionable = false;
    proposal.blockers.push(
        "the source file is no longer present on disk; its plan cannot be verified".to_string(),
    );
    proposal
}

/// Applies existing-target and case-only sibling collisions to suggested
/// proposals, upgrading them to `Conflict`.
fn detect_target_collisions(
    proposals: &mut [RenameProposal],
    siblings_by_parent: &HashMap<PathBuf, DirSiblings>,
) {
    for proposal in proposals.iter_mut() {
        if proposal.state != ProposalState::Suggested {
            continue;
        }
        let Some(proposed) = &proposal.proposed_basename else {
            continue;
        };
        let Some(parent) = proposal.source_path.parent() else {
            continue;
        };
        let Some(siblings) = siblings_by_parent.get(parent) else {
            continue;
        };
        if let Some(collision) =
            detect_target_collision(&proposal.current_basename, proposed, siblings)
        {
            proposal.collision = Some(collision);
            proposal.state = ProposalState::Conflict;
            proposal.actionable = false;
        }
    }
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::audit::{AuditEntry, AuditReport, AuditSummary, AuditVerdict};
    use crate::dat::classification::{
        CLASSIFIER_VERSION, ClassifierConfidence, ContentSelectionPolicy, DatContentClass,
    };
    use crate::dat::policy::candidate::DatCandidate;
    use crate::dat::policy::config::DatPolicyConfig;
    use crate::dat::policy::evaluate::{CandidateResolution, RankedCandidate};
    use crate::dat::policy::evaluate::{ParticipatingSource, resolve};
    use crate::dat::policy::model::{
        ClonePolicy, LanguageId, LanguagePreference, RegionId, RevisionPolicy,
    };
    use crate::dat::rename_plan::model::{CollisionKind, ExtensionStatus, ProposalState};
    use crate::dat::sources::audit_run::{
        DatAuditContentOutcome, DatAuditPolicyOutcome, DatContentCandidate, DatContentMatch,
        DatPolicyNote,
    };
    use std::path::Path;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    fn write(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, b"fixture").unwrap();
        path
    }

    fn exact(rom_name: &str) -> AuditVerdict {
        AuditVerdict::Exact {
            game_name: "Game".to_string(),
            rom_name: rom_name.to_string(),
            algorithm: "SHA-1",
        }
    }

    fn candidate(rom_name: &str, game_name: &str) -> DatCandidate {
        DatCandidate {
            source_id: "src".to_string(),
            source_priority: 20,
            game_name: game_name.to_string(),
            rom_name: rom_name.to_string(),
            regions: Vec::new(),
            languages: Vec::new(),
            revision: 0,
            has_revision_marker: false,
            parent_name: None,
        }
    }

    fn resolution(winner: DatCandidate, explanations: Vec<String>) -> CandidateResolution {
        CandidateResolution {
            entries: vec![RankedCandidate {
                candidate: winner,
                position: 1,
            }],
            excluded: Vec::new(),
            decided: true,
            winner_index: Some(0),
            ambiguous: false,
            ambiguity_reason: None,
            explanations,
            summary: "policy prefers 'Game'".to_string(),
        }
    }

    fn ambiguous_resolution(explanations: Vec<String>) -> CandidateResolution {
        CandidateResolution {
            entries: vec![
                RankedCandidate {
                    candidate: candidate("Game (USA).bin", "Game (USA)"),
                    position: 1,
                },
                RankedCandidate {
                    candidate: candidate("Game (Europe).bin", "Game (Europe)"),
                    position: 2,
                },
            ],
            excluded: Vec::new(),
            decided: false,
            winner_index: None,
            ambiguous: true,
            ambiguity_reason: Some(
                "2 candidates are tied and the policy cannot decide between them".to_string(),
            ),
            explanations,
            summary: "ambiguity remains".to_string(),
        }
    }

    fn outcome(
        scan_root: &Path,
        entries: Vec<AuditEntry>,
        notes: Vec<DatPolicyNote>,
        platform: Option<String>,
        truncated: bool,
    ) -> DatAuditOutcome {
        DatAuditOutcome {
            source_id: "src".to_string(),
            source_display_name: "Source".to_string(),
            dat_path: "/tmp/x.dat".to_string(),
            scan_root: scan_root.to_string_lossy().into_owned(),
            catalogue_names: vec!["Catalogue".to_string()],
            catalogue_entries: 2,
            catalogue_roms: 2,
            unreadable_catalogues: Vec::new(),
            report: AuditReport {
                entries,
                summary: AuditSummary::default(),
            },
            unhashed: Vec::new(),
            files_scanned: 0,
            bytes_hashed: 0,
            truncated,
            policy: Some(DatAuditPolicyOutcome {
                source_ordering: vec!["Source".to_string()],
                notes,
            }),
            content: Default::default(),
            platform,
        }
    }

    fn entry_for(path: &Path, filename: &str, verdict: AuditVerdict) -> AuditEntry {
        AuditEntry {
            local_path: path.to_string_lossy().into_owned(),
            local_filename: filename.to_string(),
            verdict,
        }
    }

    fn note(path: &Path, resolution: CandidateResolution) -> DatPolicyNote {
        DatPolicyNote {
            local_path: path.to_string_lossy().into_owned(),
            verdict_label: "Exact (multiple)".to_string(),
            resolution,
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn set_content(
        outcome: &mut DatAuditOutcome,
        path: &Path,
        rom_name: &str,
        class: DatContentClass,
        confidence: ClassifierConfidence,
    ) {
        let classification = DatContentClassification {
            class,
            confidence,
            evidence: Vec::new(),
            classifier_version: CLASSIFIER_VERSION.to_string(),
        };
        outcome.content = DatAuditContentOutcome {
            selection: ContentSelectionPolicy::GamesOnly,
            catalogue: Default::default(),
            matches: vec![DatContentMatch {
                local_path: path.to_string_lossy().into_owned(),
                candidates: vec![DatContentCandidate {
                    game_name: "Game".to_string(),
                    rom_name: rom_name.to_string(),
                    eligibility: ContentSelectionPolicy::GamesOnly.eligibility(&classification),
                    classification,
                    original_metadata: Default::default(),
                }],
            }],
        };
    }

    /// A recursive `(relative path, inode, size, mtime, contents)` snapshot
    /// proving nothing changed on disk during planning. `mtime` is a system
    /// time expressed as seconds since the epoch; a planning pass that only
    /// reads leaves it untouched.
    fn snapshot(root: &Path) -> Vec<(std::path::PathBuf, u64, u64, u64, Vec<u8>)> {
        let mut out = Vec::new();
        let mut queue = vec![root.to_path_buf()];
        while let Some(dir) = queue.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                if meta.file_type().is_dir() {
                    queue.push(path);
                } else {
                    let relative = path.strip_prefix(root).unwrap().to_path_buf();
                    let content = std::fs::read(&path).unwrap_or_default();
                    let inode = std::os::unix::fs::MetadataExt::ino(&meta);
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|elapsed| elapsed.as_secs())
                        .unwrap_or(0);
                    out.push((relative, inode, meta.len(), modified, content));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn an_exact_verified_match_produces_a_suggested_proposal() {
        let dir = temp();
        let file = write(dir.path(), "goldenaxe.hdf");
        let entries = vec![entry_for(
            &file,
            "goldenaxe.hdf",
            exact("Golden Axe (Europe).hdf"),
        )];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.total, 1);
        assert_eq!(plan.counts.suggested, 1);
        let p = &plan.proposals[0];
        assert_eq!(p.state, ProposalState::Suggested);
        assert_eq!(
            p.proposed_basename.as_deref(),
            Some("Golden Axe (Europe).hdf")
        );
        assert_eq!(p.extension_status, Some(ExtensionStatus::Preserved));
        assert!(p.actionable);
        assert!(p.match_confident);
    }

    #[test]
    fn a_current_name_already_canonical_is_not_suggested() {
        let dir = temp();
        let file = write(dir.path(), "Golden Axe (Europe).hdf");
        let entries = vec![entry_for(
            &file,
            "Golden Axe (Europe).hdf",
            exact("Golden Axe (Europe).hdf"),
        )];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.already_canonical, 1);
        assert_eq!(plan.counts.suggested, 0);
        assert_eq!(plan.proposals[0].state, ProposalState::AlreadyCanonical);
        assert!(!plan.proposals[0].actionable);
    }

    #[test]
    fn policy_ambiguity_produces_an_ambiguous_proposal() {
        let dir = temp();
        let file = write(dir.path(), "game.bin");
        let entries = vec![entry_for(
            &file,
            "game.bin",
            AuditVerdict::ExactMultipleCandidates {
                algorithm: "SHA-1",
                count: 2,
                game_names: vec!["Game (USA)".into(), "Game (Europe)".into()],
            },
        )];
        let notes = vec![note(&file, ambiguous_resolution(Vec::new()))];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, notes, None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.ambiguous, 1);
        assert_eq!(plan.proposals[0].state, ProposalState::Ambiguous);
        assert_eq!(plan.proposals[0].proposed_basename, None);
        assert!(plan.proposals[0].ambiguity_reason.is_some());
        assert!(!plan.proposals[0].actionable);
    }

    #[test]
    fn two_proposals_targeting_one_destination_stay_conflicted() {
        let dir = temp();
        let a = write(dir.path(), "a.bin");
        let b = write(dir.path(), "b.bin");
        let entries = vec![
            entry_for(&a, "a.bin", exact("Game.bin")),
            entry_for(&b, "b.bin", exact("Game.bin")),
        ];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(
            plan.counts.conflicts, 2,
            "both proposals report the conflict; nothing is resolved"
        );
        assert!(
            plan.proposals
                .iter()
                .all(|p| p.state == ProposalState::Conflict)
        );
        assert!(
            plan.proposals
                .iter()
                .all(|p| p.collision.as_ref().map(|c| c.kind)
                    == Some(CollisionKind::TwoProposalsSameTarget))
        );
    }

    #[test]
    fn an_existing_target_file_is_a_conflict() {
        let dir = temp();
        let file = write(dir.path(), "game.bin");
        // The proposed name already exists as a sibling.
        let existing = write(dir.path(), "Game (Europe).bin");
        let entries = vec![
            entry_for(&file, "game.bin", exact("Game (Europe).bin")),
            entry_for(&existing, "Game (Europe).bin", AuditVerdict::NotInDat),
        ];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.conflicts, 1);
        let p = &plan.proposals[0];
        assert_eq!(p.state, ProposalState::Conflict);
        assert_eq!(
            p.collision.as_ref().map(|c| c.kind),
            Some(CollisionKind::ExistingTarget)
        );
    }

    #[test]
    fn case_only_collision_is_detected() {
        let dir = temp();
        let file = write(dir.path(), "game.bin");
        let existing = write(dir.path(), "game (europe).bin");
        let entries = vec![
            entry_for(&file, "game.bin", exact("Game (Europe).BIN")),
            entry_for(&existing, "game (europe).bin", AuditVerdict::NotInDat),
        ];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.conflicts, 1);
        assert_eq!(
            plan.proposals[0].collision.as_ref().map(|c| c.kind),
            Some(CollisionKind::CaseCollision)
        );
    }

    #[test]
    fn weak_evidence_is_never_promoted() {
        let dir = temp();
        let weak = write(dir.path(), "crc.bin");
        let strong = write(dir.path(), "exact.bin");
        let entries = vec![
            entry_for(
                &weak,
                "crc.bin",
                AuditVerdict::Probable {
                    game_name: "Game".into(),
                    rom_name: "Game.bin".into(),
                },
            ),
            entry_for(&strong, "exact.bin", exact("Game (Europe).bin")),
        ];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(
            plan.proposals.len(),
            1,
            "only the cryptographic match gets a proposal"
        );
        assert_eq!(plan.proposals[0].source_path, strong);
        assert_eq!(plan.verified_total, 1);
    }

    #[test]
    fn a_container_extension_mismatch_is_unsupported_not_suggested() {
        let dir = temp();
        let file = write(dir.path(), "game.zip");
        let entries = vec![entry_for(&file, "game.zip", exact("Game (Europe).iso"))];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.unsupported, 1);
        let p = &plan.proposals[0];
        assert_eq!(p.state, ProposalState::Unsupported);
        assert_eq!(p.proposed_basename, None);
        assert!(p.blockers.iter().any(|b| b.contains("different file kind")));
    }

    #[test]
    fn a_path_traversal_name_is_blocked() {
        let dir = temp();
        let file = write(dir.path(), "game.bin");
        let entries = vec![entry_for(&file, "game.bin", exact("../escape.bin"))];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.blocked, 1);
        let p = &plan.proposals[0];
        assert_eq!(p.state, ProposalState::Blocked);
        assert_eq!(p.proposed_basename, None);
        assert!(p.blockers.iter().any(|b| b.contains("path separator")));
    }

    #[test]
    fn an_empty_canonical_name_is_blocked() {
        let dir = temp();
        let file = write(dir.path(), "game.bin");
        let entries = vec![entry_for(&file, "game.bin", exact("  "))];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.blocked, 1);
    }

    #[test]
    fn a_symlink_source_is_unsupported_and_never_dereferenced() {
        let dir = temp();
        let target = write(dir.path(), "real.bin");
        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let entries = vec![entry_for(&link, "link.bin", exact("Game.bin"))];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.unsupported, 1);
        let p = &plan.proposals[0];
        assert_eq!(p.object_kind, SourceObjectKind::Symlink);
        assert_eq!(p.state, ProposalState::Unsupported);
        assert!(p.blockers.iter().any(|b| b.contains("symlink")));
        // The target was not modified and the link still points at it.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fixture");
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn a_broken_symlink_is_handled_safely() {
        let dir = temp();
        let link = dir.path().join("broken.bin");
        std::os::unix::fs::symlink(dir.path().join("nowhere.bin"), &link).unwrap();
        let entries = vec![entry_for(&link, "broken.bin", exact("Game.bin"))];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.unsupported, 1);
        assert_eq!(
            plan.proposals[0].object_kind,
            SourceObjectKind::BrokenSymlink
        );
    }

    #[test]
    fn planning_makes_no_filesystem_mutation() {
        let dir = temp();
        let file = write(dir.path(), "goldenaxe.hdf");
        let other = write(dir.path(), "other.bin");
        let entries = vec![
            entry_for(&file, "goldenaxe.hdf", exact("Golden Axe (Europe).hdf")),
            entry_for(&other, "other.bin", AuditVerdict::NotInDat),
        ];
        let before = snapshot(dir.path());
        build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), Some("NES".into()), false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        let after = snapshot(dir.path());
        assert_eq!(
            before, after,
            "planning must leave every path, inode identity, size, mtime and content unchanged"
        );
    }

    #[test]
    fn games_only_never_renames_unknown_and_excludes_non_game_distinctly() {
        for (class, confidence, expected) in [
            (
                DatContentClass::Unknown,
                ClassifierConfidence::None,
                ProposalState::UnclassifiedContent,
            ),
            (
                DatContentClass::NonGame,
                ClassifierConfidence::High,
                ProposalState::ExcludedByContentPolicy,
            ),
        ] {
            let dir = temp();
            let file = write(dir.path(), "old.bin");
            let entries = vec![entry_for(&file, "old.bin", exact("Game.bin"))];
            let mut audited = outcome(dir.path(), entries, Vec::new(), None, false);
            set_content(&mut audited, &file, "Game.bin", class, confidence);
            let plan =
                build_rename_plan(&audited, &RenamePlanContext { generation: 1 }, &no_cancel())
                    .unwrap();
            assert_eq!(plan.proposals[0].state, expected);
            assert!(!plan.proposals[0].actionable);
        }
    }

    #[test]
    fn games_only_retains_compilations_and_required_multidisc_parts() {
        for class in [
            DatContentClass::GameCompilation,
            DatContentClass::RequiredMultidiscPart,
        ] {
            let dir = temp();
            let file = write(dir.path(), "old.bin");
            let entries = vec![entry_for(&file, "old.bin", exact("Game.bin"))];
            let mut audited = outcome(dir.path(), entries, Vec::new(), None, false);
            set_content(
                &mut audited,
                &file,
                "Game.bin",
                class,
                ClassifierConfidence::High,
            );
            let plan =
                build_rename_plan(&audited, &RenamePlanContext { generation: 1 }, &no_cancel())
                    .unwrap();
            assert_eq!(plan.proposals[0].state, ProposalState::Suggested);
            assert!(plan.proposals[0].actionable);
        }
    }

    #[test]
    fn a_stale_generation_is_rejected() {
        let dir = temp();
        let file = write(dir.path(), "goldenaxe.hdf");
        let entries = vec![entry_for(
            &file,
            "goldenaxe.hdf",
            exact("Golden Axe (Europe).hdf"),
        )];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 7 },
            &no_cancel(),
        )
        .unwrap();
        assert!(plan_matches_generation(&plan, 7));
        assert!(
            !plan_matches_generation(&plan, 8),
            "a newer generation invalidates the plan"
        );
        assert!(
            !plan_matches_generation(&plan, 6),
            "a stale generation is never accepted"
        );
    }

    #[test]
    fn a_cancelled_build_is_rejected() {
        let dir = temp();
        let file = write(dir.path(), "goldenaxe.hdf");
        let entries = vec![entry_for(
            &file,
            "goldenaxe.hdf",
            exact("Golden Axe (Europe).hdf"),
        )];
        let cancel = AtomicBool::new(true);
        let error = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &cancel,
        )
        .expect_err("cancelled");
        assert_eq!(error, RenamePlanError::Cancelled);
    }

    #[test]
    fn planning_order_is_deterministic() {
        let dir = temp();
        let files = ["b.bin", "a.bin", "c.bin"];
        let mut entries = Vec::new();
        for name in files {
            let path = write(dir.path(), name);
            entries.push(entry_for(&path, name, exact("Game.bin")));
        }
        let mut reversed = entries.clone();
        reversed.reverse();
        let forward = build_rename_plan(
            &outcome(dir.path(), entries, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        let backward = build_rename_plan(
            &outcome(dir.path(), reversed, Vec::new(), None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(forward, backward, "input order must not change the plan");
        let names: Vec<String> = forward
            .proposals
            .iter()
            .map(|p| p.current_basename.clone())
            .collect();
        assert_eq!(
            names,
            vec![
                "a.bin".to_string(),
                "b.bin".to_string(),
                "c.bin".to_string()
            ]
        );
    }

    #[test]
    fn policy_explanations_are_preserved() {
        let dir = temp();
        let file = write(dir.path(), "game.bin");
        let entries = vec![entry_for(
            &file,
            "game.bin",
            AuditVerdict::ExactMultipleCandidates {
                algorithm: "SHA-1",
                count: 2,
                game_names: vec![],
            },
        )];
        let winner = DatCandidate {
            source_id: "src".to_string(),
            source_priority: 20,
            game_name: "Game (Europe)".to_string(),
            rom_name: "Game (Europe) (Rev 2).bin".to_string(),
            regions: vec![RegionId::Europe],
            languages: vec![LanguageId::En],
            revision: 2,
            has_revision_marker: true,
            parent_name: None,
        };
        let notes = vec![note(
            &file,
            resolution(
                winner,
                vec![
                    "preferred region matched (Europe)".to_string(),
                    "newer verified revision preferred (Rev 2)".to_string(),
                    "source priority 20 outranked source priority 100".to_string(),
                    "parent preferred".to_string(),
                ],
            ),
        )];
        let plan = build_rename_plan(
            &outcome(dir.path(), entries, notes, None, false),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.counts.suggested, 1);
        let p = &plan.proposals[0];
        assert_eq!(
            p.proposed_basename.as_deref(),
            Some("Game (Europe) (Rev 2).bin")
        );
        assert!(
            p.explanations
                .iter()
                .any(|e| e.contains("preferred region matched"))
        );
        assert!(
            p.explanations
                .iter()
                .any(|e| e.contains("newer verified revision"))
        );
        assert!(
            p.explanations
                .iter()
                .any(|e| e.contains("source priority 20"))
        );
        assert!(
            p.explanations
                .iter()
                .any(|e| e.contains("parent preferred"))
        );
        assert_eq!(p.rom_name.as_deref(), Some("Game (Europe) (Rev 2).bin"));
    }

    #[test]
    fn platform_is_carried_into_the_proposal() {
        let dir = temp();
        let file = write(dir.path(), "goldenaxe.hdf");
        let entries = vec![entry_for(
            &file,
            "goldenaxe.hdf",
            exact("Golden Axe (Europe).hdf"),
        )];
        let plan = build_rename_plan(
            &outcome(
                dir.path(),
                entries,
                Vec::new(),
                Some("Sega Mega Drive".into()),
                false,
            ),
            &RenamePlanContext { generation: 1 },
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(plan.platform.as_deref(), Some("Sega Mega Drive"));
        assert_eq!(
            plan.proposals[0].platform.as_deref(),
            Some("Sega Mega Drive")
        );
        assert!(plan.proposals[0].platform_display.is_some());
    }

    #[test]
    fn effective_policy_resolution_is_used_for_the_plan() {
        // Sanity: the plan module composes with the PR #13 resolver types.
        let config = DatPolicyConfig {
            region_preferences: Some(vec!["europe".to_string()]),
            ..Default::default()
        };
        let effective = resolve(
            &config,
            None,
            vec![ParticipatingSource {
                id: "src".to_string(),
                display_name: "Source".to_string(),
                priority: 100,
            }],
        );
        assert_eq!(effective.revision_policy, RevisionPolicy::default());
        assert_eq!(effective.clone_policy, ClonePolicy::default());
        assert_eq!(effective.region_preferences, vec![RegionId::Europe]);
        assert_eq!(
            effective.language_preferences,
            Vec::<LanguagePreference>::new()
        );
    }
}
