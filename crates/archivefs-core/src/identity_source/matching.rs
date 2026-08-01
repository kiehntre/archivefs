//! Comparing an imported record against the file ArchiveFS can actually see.
//!
//! Import produces claims. This module turns each claim into a verdict by looking
//! at the local file, and the verdict is the honest weakest thing the evidence
//! supports - never the strongest thing it permits.
//!
//! # The tiers, and what each really requires
//!
//! | Verdict | Requires |
//! |---------|----------|
//! | `ConfirmedExternal` | a published hash and a locally computed hash of the same algorithm agree |
//! | `StrongExternal` | translated path exists, size agrees, platform agrees |
//! | `ProbableExternal` | translated path exists and platform agrees, with size or hash unavailable |
//! | `Ambiguous` | any disagreement, or an unsafe one-to-many mapping |
//! | `Stale` | the file is gone, or its size no longer matches |
//! | `Unmatched` | no mapping, or nothing at that path |
//!
//! # Hashing is not automatic
//!
//! Matching never hashes anything by itself. It uses a hash only if one is
//! already cached for the exact file on disk, so importing a library does not
//! read every byte of it. `ConfirmedExternal` therefore appears only after an
//! explicit verification pass - which is the truth, rather than a comfortable
//! fiction.
//!
//! # Local evidence wins
//!
//! [`ExternalVerification::outranks`] is consulted, not bypassed. Where RomM and
//! a locally verified identity disagree the verdict is `Ambiguous` and both are
//! kept: the point is to show the disagreement, not to pick a side quietly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use super::hashing::{FileFingerprint, LocalHashCache};
use super::model::{
    ConflictField, ExternalIdentityRecord, ExternalVerification, IdentityConflict,
    LocalEvidenceStrength,
};

/// What ArchiveFS already knows about one local file, supplied by the caller.
///
/// Deliberately data rather than a lookup: matching stays pure and testable, and
/// the caller decides how much it is willing to spend establishing local facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalFileFacts {
    /// Present when the file exists and is readable.
    pub fingerprint: Option<FileFingerprint>,
    /// The platform ArchiveFS itself determined, if any.
    pub local_platform: Option<String>,
    /// How strong that determination is.
    pub local_strength: LocalEvidenceStrength,
}

impl LocalFileFacts {
    /// Observes what can be seen from metadata alone. No read, no hash.
    pub fn observe(path: &Path) -> Self {
        Self {
            fingerprint: FileFingerprint::observe(path),
            local_platform: None,
            local_strength: LocalEvidenceStrength::None,
        }
    }

    pub fn with_local_platform(
        mut self,
        platform: Option<&str>,
        strength: LocalEvidenceStrength,
    ) -> Self {
        self.local_platform = platform.map(str::to_string);
        self.local_strength = strength;
        self
    }

    pub fn exists(&self) -> bool {
        self.fingerprint.is_some()
    }
}

/// How many records pointed at each translated path, so a one-to-many or
/// many-to-one mapping can be recognised rather than silently resolved.
#[derive(Debug, Clone, Default)]
pub struct PathClaims {
    counts: BTreeMap<PathBuf, usize>,
}

impl PathClaims {
    /// Counts how many records claim each translated path.
    pub fn of(records: &[ExternalIdentityRecord]) -> Self {
        let mut counts: BTreeMap<PathBuf, usize> = BTreeMap::new();
        for record in records {
            if let Some(path) = &record.archivefs_path {
                *counts.entry(path.clone()).or_default() += 1;
            }
        }
        Self { counts }
    }

    pub fn claimants(&self, path: &Path) -> usize {
        self.counts.get(path).copied().unwrap_or(0)
    }

    /// Paths more than one record claims - a genuine ambiguity to surface.
    pub fn contested(&self) -> Vec<(&PathBuf, usize)> {
        self.counts
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|(path, count)| (path, *count))
            .collect()
    }
}

/// The verdict for one record, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchOutcome {
    pub verification: ExternalVerification,
    pub conflicts: Vec<IdentityConflict>,
    pub evidence: Vec<String>,
    /// Whether a hash comparison actually happened. `false` means the verdict
    /// rests on path, size and platform only, which is worth knowing.
    pub hash_compared: bool,
}

/// Assigns a verdict to one record.
///
/// `hashes` is consulted but never populated: a hash is used only when one is
/// already cached for this exact file, so matching an entire library performs no
/// reads at all.
pub fn match_record(
    record: &ExternalIdentityRecord,
    facts: &LocalFileFacts,
    claims: &PathClaims,
    hashes: &LocalHashCache,
) -> MatchOutcome {
    let mut evidence = Vec::new();
    let mut conflicts = Vec::new();

    // No mapping means nothing to compare. Not a fault: a RomM library may hold
    // platforms this ArchiveFS has no source folder for.
    let Some(path) = &record.archivefs_path else {
        evidence.push(
            "no configured path mapping covers this record, so no local file was compared"
                .to_string(),
        );
        return MatchOutcome {
            verification: ExternalVerification::Unmatched,
            conflicts,
            evidence,
            hash_compared: false,
        };
    };

    // Two or more records claiming the same local file is a real ambiguity: one
    // of them is wrong and this code cannot tell which.
    let claimants = claims.claimants(path);
    if claimants > 1 {
        conflicts.push(IdentityConflict {
            field: ConflictField::FileState,
            external: format!("{claimants} RomM records"),
            local: "one file".to_string(),
            detail: format!(
                "{claimants} RomM records translate to {}, so which one describes it cannot be \
                 decided from the mapping alone",
                path.display()
            ),
        });
        evidence.push("more than one RomM record claims this file".to_string());
        return MatchOutcome {
            verification: ExternalVerification::Ambiguous,
            conflicts,
            evidence,
            hash_compared: false,
        };
    }

    // The file has to be there.
    let Some(fingerprint) = &facts.fingerprint else {
        evidence.push(format!(
            "{} does not exist, so the record describes a file this library no longer has",
            path.display()
        ));
        return MatchOutcome {
            verification: ExternalVerification::Stale,
            conflicts,
            evidence,
            hash_compared: false,
        };
    };
    evidence.push(format!("{} exists", path.display()));

    // Size. A mismatch is decisive staleness: the bytes are not the bytes RomM
    // measured, so no amount of title agreement makes this the same file.
    let mut size_agrees = None;
    if let Some(published) = record.file_size_bytes {
        if published == fingerprint.size_bytes {
            size_agrees = Some(true);
            evidence.push(format!("file size agrees at {published} bytes"));
        } else {
            conflicts.push(IdentityConflict {
                field: ConflictField::FileSize,
                external: published.to_string(),
                local: fingerprint.size_bytes.to_string(),
                detail: format!(
                    "RomM recorded {published} bytes but the file is now {} bytes",
                    fingerprint.size_bytes
                ),
            });
            evidence.push("the file's size no longer matches what RomM recorded".to_string());
            return MatchOutcome {
                verification: ExternalVerification::Stale,
                conflicts,
                evidence,
                hash_compared: false,
            };
        }
    } else {
        evidence.push("RomM published no file size, so size could not be compared".to_string());
    }

    // Platform. A disagreement with a *verified* local platform is the case the
    // whole model exists for.
    let mut platform_agrees = None;
    match (&record.platform_candidate, &facts.local_platform) {
        (Some(external), Some(local)) if external == local => {
            platform_agrees = Some(true);
            evidence.push(format!("platform agrees: {external}"));
        }
        (Some(external), Some(local)) => {
            platform_agrees = Some(false);
            conflicts.push(IdentityConflict {
                field: ConflictField::Platform,
                external: external.clone(),
                local: local.clone(),
                detail: format!(
                    "RomM says {external} but ArchiveFS determined {local} from the file itself"
                ),
            });
        }
        (Some(external), None) => {
            evidence.push(format!(
                "RomM says {external}; ArchiveFS has no platform of its own for this file"
            ));
        }
        (None, _) => {
            evidence.push(
                "RomM's platform could not be mapped to a canonical ArchiveFS platform".to_string(),
            );
        }
    }

    // Hashes, but only from what is already cached.
    let mut hash_compared = false;
    let mut hash_agrees = None;
    if let Some(local) = hashes.get(path) {
        for published in &record.hashes {
            hash_compared = true;
            if local.agrees_with(published) {
                hash_agrees = Some(true);
                evidence.push(format!(
                    "{} matches the locally computed value",
                    published.algorithm.label()
                ));
                // One agreeing hash is enough; the others add nothing.
                break;
            }
            hash_agrees = Some(false);
            conflicts.push(IdentityConflict {
                field: ConflictField::Hash,
                external: published.value.clone(),
                local: local.value(published.algorithm).to_string(),
                detail: format!(
                    "the {} RomM published does not match the file's own",
                    published.algorithm.label()
                ),
            });
        }
        if record.hashes.is_empty() {
            evidence.push(
                "the file has been hashed locally, but RomM published no hash to compare"
                    .to_string(),
            );
        }
    } else if !record.hashes.is_empty() {
        evidence.push(
            "RomM published a hash, but this file has not been verified locally yet, so nothing \
             was compared"
                .to_string(),
        );
    }

    // A hash mismatch outranks everything: the bytes differ.
    if hash_agrees == Some(false) {
        evidence.push("a hash disagreement means this is not the same data".to_string());
        return MatchOutcome {
            verification: ExternalVerification::Ambiguous,
            conflicts,
            evidence,
            hash_compared,
        };
    }
    // A platform disagreement with verified local evidence is equally decisive.
    if platform_agrees == Some(false) {
        let verified = facts.local_strength == LocalEvidenceStrength::Verified;
        evidence.push(if verified {
            "ArchiveFS verified a different platform from the file itself, so the two disagree"
                .to_string()
        } else {
            "RomM and ArchiveFS suggest different platforms".to_string()
        });
        return MatchOutcome {
            verification: ExternalVerification::Ambiguous,
            conflicts,
            evidence,
            hash_compared,
        };
    }

    // Now the positive verdicts, weakest justification first.
    let candidate = if hash_agrees == Some(true) {
        // The bytes were compared and agree.
        ExternalVerification::ConfirmedExternal
    } else if size_agrees == Some(true) && platform_agrees == Some(true) {
        // Everything checkable without reading the file agrees.
        ExternalVerification::StrongExternal
    } else if record.title.is_some() {
        // The file is there, nothing contradicts the record, and it names a
        // title - but size or platform could not be confirmed, so this is as far
        // as the evidence goes.
        ExternalVerification::ProbableExternal
    } else {
        // The file is there but nothing corroborates the record at all.
        evidence.push(
            "the file exists but nothing in the record could be corroborated against it"
                .to_string(),
        );
        ExternalVerification::Unmatched
    };

    // Finally, the rule that external evidence never displaces stronger local
    // evidence. The verdict itself is not weakened - it is still what the
    // evidence supports - but the fact that local evidence leads is recorded, so
    // a caller presenting this cannot mistake it for permission to overwrite.
    if candidate.is_usable() && !candidate.outranks(facts.local_strength) {
        evidence.push(format!(
            "ArchiveFS's own identity is {} and is not displaced by this record",
            match facts.local_strength {
                LocalEvidenceStrength::Verified => "verified",
                LocalEvidenceStrength::Weak => "weaker but still local",
                LocalEvidenceStrength::None => "absent",
            }
        ));
    }

    MatchOutcome {
        verification: candidate,
        conflicts,
        evidence,
        hash_compared,
    }
}

/// Applies matching to a whole set of records, in place.
///
/// `facts_for` is supplied by the caller so this stays testable without a
/// filesystem, and so the caller controls what local facts cost.
pub fn match_all(
    records: &mut [ExternalIdentityRecord],
    hashes: &LocalHashCache,
    mut facts_for: impl FnMut(&ExternalIdentityRecord) -> LocalFileFacts,
    cancel: Option<&AtomicBool>,
) -> Result<(), MatchCancelled> {
    let claims = PathClaims::of(records);
    for record in records.iter_mut() {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(MatchCancelled);
        }
        let facts = facts_for(record);
        let outcome = match_record(record, &facts, &claims, hashes);
        record.verification = outcome.verification;
        record.conflicts = outcome.conflicts;
        // Import-time evidence is kept and the match's evidence appended, so the
        // record explains both what RomM said and what was checked.
        record.evidence.extend(outcome.evidence);
    }
    Ok(())
}

/// Matching was cancelled part-way. The records touched so far are already
/// updated, which is why a caller discards the whole attempt rather than
/// publishing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchCancelled;

/// A multi-disc or multi-file group, preserved rather than flattened.
///
/// Stage 1B does not decide grouping policy - which disc is "the" game, how a
/// group is presented - but it keeps enough structure that a later stage can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityGroup {
    /// The record the provider links the others to.
    pub primary_game_id: String,
    pub title: Option<String>,
    /// Every record in the group, including the primary.
    pub member_game_ids: Vec<String>,
    /// The files the provider listed for the primary record.
    pub related_files: Vec<String>,
    /// How many members matched a local file.
    pub matched_members: usize,
    /// True when some members matched and some did not - a partial group, which
    /// is a real state and not an error.
    pub partial: bool,
}

/// Builds groups from sibling relationships the provider published.
///
/// A record with no siblings and one file is not a group and is not reported as
/// one - only genuine multi-file or multi-disc structure appears here.
pub fn build_groups(records: &[ExternalIdentityRecord]) -> Vec<IdentityGroup> {
    let mut groups: Vec<IdentityGroup> = Vec::new();
    let mut claimed: Vec<String> = Vec::new();

    for record in records {
        if record.sibling_game_ids.is_empty() && record.related_files.len() < 2 {
            continue;
        }
        if claimed.contains(&record.provider_game_id) {
            continue;
        }
        let mut member_game_ids = vec![record.provider_game_id.clone()];
        for sibling in &record.sibling_game_ids {
            if !member_game_ids.contains(sibling) {
                member_game_ids.push(sibling.clone());
            }
        }
        member_game_ids.sort();
        claimed.extend(member_game_ids.iter().cloned());
        let matched_members = member_game_ids
            .iter()
            .filter(|id| {
                records
                    .iter()
                    .find(|candidate| &&candidate.provider_game_id == id)
                    .is_some_and(|candidate| candidate.verification.is_usable())
            })
            .count();
        groups.push(IdentityGroup {
            primary_game_id: record.provider_game_id.clone(),
            title: record.title.clone(),
            partial: matched_members > 0 && matched_members < member_game_ids.len(),
            matched_members,
            member_game_ids,
            related_files: record.related_files.clone(),
        });
    }
    groups.sort_by(|left, right| left.primary_game_id.cmp(&right.primary_game_id));
    groups
}
