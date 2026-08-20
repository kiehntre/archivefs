//! Batch 11: read-only side-file role classification - milestone sections
//! 7-8, 31-32.
//!
//! Planning metadata only: nothing here moves, renames, or reclassifies a
//! file on disk. A role only ever changes *how this batch's planner talks
//! about* a file, never the file itself.
//!
//! # Evidence used
//!
//! - The file extension, checked against the *existing* platform registry
//!   (`platform::platform_candidates_for_extension`) for "is this actually a
//!   primary-content extension" - never a second, hand-maintained ROM
//!   extension list.
//! - A small, explicit side-file extension vocabulary (cue/m3u/patch/
//!   artwork/etc.) for the non-content roles.
//! - Optionally, a filename substring for the couple of cases the milestone
//!   explicitly sanctions (`readme`/`manual` in the name) - a role
//!   classification, never a platform or game identity claim (section 8's
//!   own worked example: "cover.jpg => Artwork is okay",
//!   "mario64.zip => Nintendo 64 is NOT okay").

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Milestone section 7's role vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideFileRole {
    /// A recognised primary-content extension for some canonical platform
    /// (per the existing platform registry) - this is the game/ROM/disc
    /// image itself, not a side file.
    PrimaryContent,
    CueSheet,
    Playlist,
    Patch,
    Manual,
    Artwork,
    Readme,
    Metadata,
    SaveOrState,
    /// A real file this batch has no confident role evidence for - never
    /// forced into a game folder or treated as content (section 39).
    UnknownSupport,
}

impl SideFileRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::PrimaryContent => "Primary content",
            Self::CueSheet => "Cue sheet",
            Self::Playlist => "Playlist",
            Self::Patch => "Patch",
            Self::Manual => "Manual",
            Self::Artwork => "Artwork",
            Self::Readme => "Readme",
            Self::Metadata => "Metadata",
            Self::SaveOrState => "Save or state",
            Self::UnknownSupport => "Unknown support file",
        }
    }

    /// Whether this role should ever be treated as a game/ROM the planner
    /// organises on its own (as opposed to attached to a set) - milestone
    /// section 30/32: side files never dominate planning stats as games.
    pub fn is_primary(self) -> bool {
        matches!(self, Self::PrimaryContent)
    }
}

const PATCH_EXTENSIONS: &[&str] = &["ips", "bps", "xdelta", "ppf", "ups"];
const ARTWORK_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp"];
const SAVE_STATE_EXTENSIONS: &[&str] = &[
    "sav", "srm", "state", "st0", "st1", "st2", "st3", "st4", "st5", "st6", "st7", "st8", "st9",
    "ss0", "ss1", "ss2", "ss3", "ss4", "ss5", "ss6", "ss7", "ss8", "ss9", "mcr",
];
const METADATA_EXTENSIONS: &[&str] = &["nfo", "xml", "json", "yaml", "yml"];

/// Classifies `path`'s role using only its extension and, for the two
/// filename-substring exceptions the milestone explicitly allows
/// (readme/manual), its basename - never anything that would assert a
/// platform or game identity from the name alone (section 8).
pub fn classify_side_file(path: &Path) -> SideFileRole {
    let Some(extension) = crate::platform::extension_of(path) else {
        return classify_by_basename(path).unwrap_or(SideFileRole::UnknownSupport);
    };
    let ext = extension.as_str();

    // Checked before the registry lookup below: `cue`/`m3u` are listed as
    // *weak* extensions for some optical platforms (evidence that a .cue
    // commonly accompanies that platform's images), but a cue sheet or
    // playlist is always a side file that *references* primary content,
    // never primary content itself (milestone section 9-10).
    match ext {
        "cue" => return SideFileRole::CueSheet,
        "m3u" | "m3u8" => return SideFileRole::Playlist,
        _ => {}
    }
    if !crate::platform::platform_candidates_for_extension(ext).is_empty() {
        return SideFileRole::PrimaryContent;
    }
    match ext {
        _ if PATCH_EXTENSIONS.contains(&ext) => return SideFileRole::Patch,
        _ if ARTWORK_EXTENSIONS.contains(&ext) => return SideFileRole::Artwork,
        _ if SAVE_STATE_EXTENSIONS.contains(&ext) => return SideFileRole::SaveOrState,
        "nfo" => return SideFileRole::Readme,
        _ if METADATA_EXTENSIONS.contains(&ext) => return SideFileRole::Metadata,
        _ => {}
    }
    if let Some(role) = classify_by_basename(path) {
        return role;
    }
    // "pdf"/"txt" are ambiguous by extension alone (a manual, a readme, or
    // neither) - resolved only by the basename check above; otherwise
    // honestly unknown rather than guessed.
    SideFileRole::UnknownSupport
}

/// The only filename-substring checks this module performs - both are
/// role classifications ("this looks like a manual/readme"), never a
/// platform or game identity claim. Case-insensitive, matched against the
/// file stem only (never the full path, so a directory named "readme"
/// higher up never leaks in).
fn classify_by_basename(path: &Path) -> Option<SideFileRole> {
    let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
    if stem.contains("readme") {
        return Some(SideFileRole::Readme);
    }
    if stem.contains("manual") {
        return Some(SideFileRole::Manual);
    }
    None
}

#[cfg(test)]
mod tests;
