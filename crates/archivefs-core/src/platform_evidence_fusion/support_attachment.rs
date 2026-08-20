//! Batch 12: read-only support-file attachment - milestone sections 24-26.
//!
//! [`side_file_classification::classify_side_file`] (Batch 11) answers
//! "what kind of file is this"; this module answers the separate question
//! "does this support file belong to a specific planned set" - and answers
//! it only from defensible, structural relationships, never "shares a
//! folder with something."

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::cue_m3u_parsing::{ParsedReference, parse_cue_file_references, parse_m3u_references};
use super::side_file_classification::{SideFileRole, classify_side_file};

/// Milestone section 25's association vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SupportAssociation {
    /// A defensible, structural relationship to a specific set exists -
    /// a cue/m3u's own resolved reference, or a manual/artwork file that
    /// is the *only* candidate in an unambiguous single-game context.
    Attached { set_label: String },
    /// A plausible relationship exists but is not proven strongly enough
    /// to attach automatically (e.g. more than one candidate set in the
    /// same directory).
    Candidate { reason: String },
    /// No defensible relationship was found - this is the honest default
    /// for a support file that merely happens to sit near other files.
    Unassociated,
    /// The file referenced something unsafe (path traversal, absolute
    /// path) - never resolved, always surfaced rather than silently
    /// dropped.
    UnsafeReference { detail: String },
}

/// One support file's role and association - the structured result this
/// module actually produces, milestone section 25.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupportFileAttachment {
    pub path: PathBuf,
    pub role: SideFileRole,
    pub association: SupportAssociation,
}

/// A single-game context: exactly one primary-content candidate is known
/// to be present, so an unambiguous manual/artwork attachment is
/// defensible. `None` (an ambiguous or empty context) means manual/artwork
/// attachment is never attempted - only cue/m3u's own explicit references
/// still resolve.
pub struct SingleGameContext<'a> {
    pub set_label: &'a str,
}

/// Attaches `path` (already known to be a support file - callers should
/// have already checked [`classify_side_file`] is not
/// [`SideFileRole::PrimaryContent`]) to a set, using only:
///
/// - for [`SideFileRole::CueSheet`]/[`SideFileRole::Playlist`]: the file's
///   own parsed, safety-checked references (never a folder-proximity
///   guess) - milestone section 24's first two examples;
/// - for [`SideFileRole::Manual`]/[`SideFileRole::Artwork`]: only when
///   `single_game_context` names exactly one unambiguous set for this
///   support file's own directory (milestone section 24's third example -
///   "located inside a single unambiguous set context");
/// - for [`SideFileRole::Patch`]: never automatically (milestone section
///   26 - a patch's target must be explicit/structured, which this crate
///   has no source for yet, so it is always `Unassociated` here, never
///   guessed from a nearby filename).
///
/// Every other role (`Readme`/`Metadata`/`SaveOrState`/`UnknownSupport`)
/// is `Unassociated` - arbitrary nearby files are never attached merely
/// for sharing a folder (milestone section 24's explicit prohibition).
pub fn attach_support_file(
    path: &Path,
    contents_if_cue_or_m3u: Option<&str>,
    single_game_context: Option<&SingleGameContext<'_>>,
) -> SupportFileAttachment {
    let role = classify_side_file(path);
    let association = match role {
        SideFileRole::CueSheet | SideFileRole::Playlist => {
            attach_via_references(path, role, contents_if_cue_or_m3u)
        }
        SideFileRole::Manual | SideFileRole::Artwork => match single_game_context {
            Some(context) => SupportAssociation::Attached {
                set_label: context.set_label.to_string(),
            },
            None => SupportAssociation::Unassociated,
        },
        // Patch (section 26), Readme, Metadata, SaveOrState,
        // UnknownSupport, PrimaryContent: never auto-attached.
        _ => SupportAssociation::Unassociated,
    };
    SupportFileAttachment {
        path: path.to_path_buf(),
        role,
        association,
    }
}

fn attach_via_references(
    path: &Path,
    role: SideFileRole,
    contents: Option<&str>,
) -> SupportAssociation {
    let Some(contents) = contents else {
        return SupportAssociation::Candidate {
            reason: "cue/m3u contents were not supplied to attach_support_file".to_string(),
        };
    };
    let references: Vec<ParsedReference> = match role {
        SideFileRole::CueSheet => parse_cue_file_references(path, contents),
        SideFileRole::Playlist => parse_m3u_references(path, contents),
        _ => unreachable!("attach_via_references is only called for CueSheet/Playlist"),
    };
    if references.is_empty() {
        return SupportAssociation::Candidate {
            reason: "no references found in this cue/m3u".to_string(),
        };
    }
    if let Some(unsafe_ref) = references.iter().find(|r| !r.is_safe()) {
        return SupportAssociation::UnsafeReference {
            detail: format!("{:?}: {:?}", unsafe_ref.raw, unsafe_ref.rejection),
        };
    }
    // Batch 13 (milestone section 8/20): a missing referenced member is a
    // real blocker, never silently ignored - the set stays a Candidate
    // rather than Attached until every safe reference actually resolves
    // to a real file.
    if let Some(missing) = references
        .iter()
        .find(|r| !r.resolved.as_ref().is_some_and(|p| p.is_file()))
    {
        return SupportAssociation::Candidate {
            reason: format!(
                "referenced member does not exist on disk: {:?}",
                missing.raw
            ),
        };
    }
    let set_label = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    SupportAssociation::Attached { set_label }
}

#[cfg(test)]
mod tests;
