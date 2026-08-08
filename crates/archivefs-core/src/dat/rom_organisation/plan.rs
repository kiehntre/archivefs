//! Read-only organisation planning.
//!
//! [`build_organisation_plan`] turns a batch of candidates (each with a
//! resolved platform identity) into an [`OrganisationPlan`]. It is strictly
//! read-only: it never creates, moves, renames or deletes anything, and only
//! reads each source's object kind (`symlink_metadata`) to decide eligibility.
//!
//! # Platform safety
//!
//! A candidate may only be Suggested when its platform is resolved strongly
//! enough: a manual assignment, verified DAT evidence, a canonical RomM
//! mapping, or an accepted strong identity. Unknown platforms, platform
//! conflicts and missing canonical/RomM slug mappings are never organised
//! silently - they are reported as Blocked/Unsupported and the user must
//! resolve them first.

use std::path::{Path, PathBuf};

use crate::dat::rename_apply::preflight::is_safe_basename;
use crate::dat::rename_plan::derive_proposed_basename;
use crate::platform::identity::PlatformIdentityResolution;

use super::model::{OrganisationMode, OrganisationPlan, OrganisationPlanEntry, OrganisationStatus};

/// One source ROM and its resolved platform identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganisationCandidate {
    pub source_path: PathBuf,
    /// The resolved platform identity for this source (from the current
    /// platform identity resolver), or `Unknown`/`Conflict`.
    pub resolution: PlatformIdentityResolution,
    /// The canonical game name *with its extension*, when known (for example
    /// from a RomM record or verified DAT evidence). Falls back to the source
    /// basename when absent, which keeps the name stable and only moves the
    /// file into the platform directory.
    pub canonical_name: Option<String>,
}

/// Everything the planner needs to build one plan.
pub struct OrganisationPlanRequest<'a> {
    pub master_root: &'a Path,
    pub mode: OrganisationMode,
    pub candidates: &'a [OrganisationCandidate],
    /// Resolves a canonical platform id to its canonical RomM-compatible slug.
    /// The only acceptable source for folder names; nothing is derived from
    /// display labels.
    pub slug_for_platform: &'a dyn Fn(&str) -> Option<String>,
    /// Bumped by the caller on every plan (re)build; a plan is stale the
    /// moment its generation no longer matches the caller's current one.
    pub generation: u64,
}

/// Builds a read-only organisation plan. Never mutates the filesystem.
pub fn build_organisation_plan(request: &OrganisationPlanRequest<'_>) -> OrganisationPlan {
    let mut entries: Vec<OrganisationPlanEntry> = request
        .candidates
        .iter()
        .map(|candidate| plan_entry(request, candidate))
        .collect();
    detect_collisions(request.master_root, &mut entries);
    entries.sort_by(|left, right| {
        (
            left.status_rank(),
            &left.source_path,
            &left.destination_path,
        )
            .cmp(&(
                right.status_rank(),
                &right.source_path,
                &right.destination_path,
            ))
    });
    OrganisationPlan {
        master_root: request.master_root.to_path_buf(),
        mode: request.mode,
        generation: request.generation,
        entries,
    }
}

/// Plans one candidate into an entry, classifying the status.
fn plan_entry(
    request: &OrganisationPlanRequest<'_>,
    candidate: &OrganisationCandidate,
) -> OrganisationPlanEntry {
    let source_basename = candidate
        .source_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let object_kind = crate::dat::rename_apply::identity::capture_identity(&candidate.source_path)
        .map(|identity| identity.kind)
        .ok();
    let mode = request.mode;

    let blocked = |reason: &str| OrganisationPlanEntry {
        source_path: candidate.source_path.clone(),
        destination_path: PathBuf::new(),
        platform: None,
        platform_display_name: String::new(),
        platform_source: String::new(),
        slug: None,
        mode,
        status: OrganisationStatus::Blocked,
        reason: Some(reason.to_string()),
    };

    let unsupported = |reason: &str, platform: Option<String>, display: &str, source: &str| {
        OrganisationPlanEntry {
            source_path: candidate.source_path.clone(),
            destination_path: PathBuf::new(),
            platform,
            platform_display_name: display.to_string(),
            platform_source: source.to_string(),
            slug: None,
            mode,
            status: OrganisationStatus::Unsupported,
            reason: Some(reason.to_string()),
        }
    };

    // Object kind decides mode eligibility (and whether the file may move at all).
    let kind = match object_kind {
        Some(kind) => kind,
        None => return blocked("the source file does not exist"),
    };
    use crate::dat::rename_apply::model::ObjectKind;
    match mode {
        OrganisationMode::MoveRealFile => {
            if kind != ObjectKind::RegularFile {
                let reason = if kind == ObjectKind::Symlink || kind == ObjectKind::BrokenSymlink {
                    "the source is a symlink; use 'Organise symlink only' to move the link \
                     object without touching its target"
                } else {
                    "this mode organises regular files only"
                };
                return blocked(reason);
            }
        }
        OrganisationMode::OrganiseSymlinkOnly => {
            if kind != ObjectKind::Symlink && kind != ObjectKind::BrokenSymlink {
                return blocked("this mode organises symlinks only; the source is not a symlink");
            }
        }
        OrganisationMode::RenameInPlace => {
            // Renaming an object in place preserves it; regular files and
            // symlink objects are both fine (the target is never touched).
            if kind == ObjectKind::Other {
                return blocked("the source is not a supported file object");
            }
        }
    }

    // Platform resolution gates the rest.
    let (platform, display_name, platform_source) = match &candidate.resolution {
        PlatformIdentityResolution::Unknown { .. } => {
            return blocked("no platform identity could be resolved for this game");
        }
        PlatformIdentityResolution::Conflict { .. } => {
            return blocked(
                "the platform identity is in conflict (sources disagree); resolve it before \
                 organising",
            );
        }
        PlatformIdentityResolution::Resolved {
            platform,
            display_name,
            evidence,
            ..
        } => {
            let source_label = evidence
                .iter()
                .map(|item| item.source.label())
                .max()
                .unwrap_or("Inference");
            (
                platform.clone(),
                display_name.clone(),
                source_label.to_string(),
            )
        }
    };
    let platform = match crate::platform::platform_by_id(&platform) {
        Some(registry_platform) => registry_platform.id.to_string(),
        None => return blocked("the resolved platform is not in the canonical registry"),
    };

    // The folder name MUST come from the canonical RomM slug mapping, never
    // from the display label. Rename-in-place has no platform folder, so it
    // needs no slug; the move modes require one.
    let slug = match mode {
        OrganisationMode::RenameInPlace => None,
        OrganisationMode::MoveRealFile | OrganisationMode::OrganiseSymlinkOnly => {
            let Some(slug) = (request.slug_for_platform)(&platform) else {
                return unsupported(
                    "no canonical RomM slug mapping exists for this platform; add an identity \
                     cache import to define it",
                    Some(platform),
                    &display_name,
                    &platform_source,
                );
            };
            if !is_safe_basename(&slug) {
                return blocked("the platform slug is not a safe single path component");
            }
            Some(slug)
        }
    };

    // Canonical filename: reuse the rename planner's derivation, never a
    // second filename engine. Extension mismatches are Unsupported.
    let rom_name = candidate
        .canonical_name
        .as_deref()
        .unwrap_or(&source_basename);
    let proposed_basename = match derive_proposed_basename(rom_name, &source_basename) {
        crate::dat::rename_plan::DeriveOutcome::Ok(derived) => derived.proposed_basename,
        crate::dat::rename_plan::DeriveOutcome::Blocked(reason) => return blocked(&reason),
        crate::dat::rename_plan::DeriveOutcome::Unsupported(reason) => {
            return unsupported(&reason, Some(platform), &display_name, &platform_source);
        }
    };
    if !is_safe_basename(&proposed_basename) {
        return blocked("the canonical filename is not a safe single path component");
    }

    // Destination derivation. Rename-in-place stays in the source directory;
    // the move modes place the file under the master root's platform folder.
    let destination_path = match (&mode, &slug) {
        (OrganisationMode::RenameInPlace, _) => candidate
            .source_path
            .parent()
            .map(|parent| parent.join(&proposed_basename))
            .unwrap_or_else(|| candidate.source_path.clone()),
        (_, Some(slug)) => request.master_root.join(slug).join(&proposed_basename),
        (_, None) => return blocked("no canonical platform slug is available for this move"),
    };

    let already_organised = if candidate.source_path == destination_path {
        true
    } else if mode != OrganisationMode::RenameInPlace
        && let Some(name) = destination_path.file_name()
        && name == source_basename.as_str()
    {
        // Same name: already organised when the source already sits in the
        // canonical platform folder.
        slug.as_deref().is_some_and(|slug| {
            candidate.source_path.parent() == Some(request.master_root.join(slug).as_path())
        })
    } else {
        false
    };

    OrganisationPlanEntry {
        source_path: candidate.source_path.clone(),
        destination_path,
        platform: Some(platform),
        platform_display_name: display_name,
        platform_source,
        slug,
        mode,
        status: if already_organised {
            OrganisationStatus::AlreadyOrganised
        } else {
            OrganisationStatus::Suggested
        },
        reason: None,
    }
}

impl OrganisationPlanEntry {
    fn status_rank(&self) -> u8 {
        match self.status {
            OrganisationStatus::Suggested => 0,
            OrganisationStatus::AlreadyOrganised => 1,
            OrganisationStatus::Conflict => 2,
            OrganisationStatus::Blocked => 3,
            OrganisationStatus::Unsupported => 4,
        }
    }
}

/// Detects collisions across the batch and against the live destination
/// directory, and marks the affected entries as Conflict. Never auto-resolves.
fn detect_collisions(master_root: &Path, entries: &mut [OrganisationPlanEntry]) {
    let count = entries.len();
    // Exact duplicates and case-only duplicates across the batch.
    for left_index in 0..count {
        if entries[left_index].destination_path.as_os_str().is_empty() {
            continue;
        }
        if !matches!(
            entries[left_index].status,
            OrganisationStatus::Suggested | OrganisationStatus::AlreadyOrganised
        ) {
            continue;
        }
        let left_destination = entries[left_index]
            .destination_path
            .to_string_lossy()
            .into_owned();
        for right_index in (left_index + 1)..count {
            let right = &entries[right_index];
            if right.destination_path.as_os_str().is_empty() {
                continue;
            }
            if !matches!(
                right.status,
                OrganisationStatus::Suggested | OrganisationStatus::AlreadyOrganised
            ) {
                continue;
            }
            let right_destination = right.destination_path.to_string_lossy().into_owned();
            if left_destination == right_destination {
                mark_conflict(entries, left_index, "two plans target the same destination");
                mark_conflict(
                    entries,
                    right_index,
                    "two plans target the same destination",
                );
            }
            if left_destination.eq_ignore_ascii_case(&right_destination)
                && left_destination != right_destination
            {
                mark_conflict(entries, left_index, "two plans differ only by case");
                mark_conflict(entries, right_index, "two plans differ only by case");
            }
        }
    }

    // Live destination: occupied, or a case-only sibling. Read-only.
    for index in 0..count {
        if entries[index].destination_path.as_os_str().is_empty() {
            continue;
        }
        if entries[index].status == OrganisationStatus::AlreadyOrganised {
            continue;
        }
        if std::fs::symlink_metadata(&entries[index].destination_path).is_ok() {
            mark_conflict(
                entries,
                index,
                "the destination already exists; nothing is overwritten",
            );
            continue;
        }
        let Some(parent) = entries[index].destination_path.parent() else {
            continue;
        };
        let file_name = entries[index]
            .destination_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if file_name.is_empty() {
            continue;
        }
        if let Ok(dir_entries) = std::fs::read_dir(parent) {
            let collision = dir_entries.flatten().any(|dir_entry| {
                let name = dir_entry.file_name().to_string_lossy().into_owned();
                name.eq_ignore_ascii_case(&file_name) && name != file_name
            });
            if collision {
                mark_conflict(
                    entries,
                    index,
                    "a file differing from the destination only by case exists",
                );
            }
        }
    }

    let _ = master_root;
}

fn mark_conflict(entries: &mut [OrganisationPlanEntry], index: usize, reason: &str) {
    let entry = &mut entries[index];
    if entry.status == OrganisationStatus::Conflict {
        return;
    }
    entry.status = OrganisationStatus::Conflict;
    entry.reason = Some(reason.to_string());
}
