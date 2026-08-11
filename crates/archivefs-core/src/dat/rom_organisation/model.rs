//! The canonical ROM organisation model.
//!
//! An organisation plan is a **read-only proposal**: for every candidate ROM
//! it names the destination under the configured master ROM root, the
//! canonical platform it belongs to, the provenance of that platform
//! assignment, and a status. Generating a plan never creates, moves, renames
//! or deletes anything; only an explicitly approved transaction may mutate.
//!
//! The three organisation modes are separate, explicit user choices and are
//! never combined implicitly:
//!
//! - [`OrganisationMode::RenameInPlace`] renames the file to its canonical
//!   name inside its current directory;
//! - [`OrganisationMode::MoveRealFile`] moves the real file into the
//!   canonical platform directory under the master ROM root;
//! - [`OrganisationMode::OrganiseSymlinkOnly`] moves only the symlink *object*
//!   (never dereferencing or touching its target).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::dat::classification::{
    ContentSelectionPolicy, DatContentClassification, DatOriginalMetadata,
};

/// How a game's file should be organised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganisationMode {
    /// Rename the file in place to its canonical name (same directory).
    RenameInPlace,
    /// Move the real file into the canonical platform directory.
    MoveRealFile,
    /// Move only the symlink object into the canonical platform directory;
    /// the link target is never dereferenced or touched.
    OrganiseSymlinkOnly,
}

impl OrganisationMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::RenameInPlace => "Rename in place",
            Self::MoveRealFile => "Move real file",
            Self::OrganiseSymlinkOnly => "Organise symlink only",
        }
    }
}

/// The status of one proposed organisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrganisationStatus {
    /// Ready to apply after explicit approval.
    Suggested,
    /// The file already sits at its canonical destination.
    AlreadyOrganised,
    /// The destination is occupied, or two plans target the same path.
    Conflict,
    /// The file cannot be organised (unknown/conflicted platform, unsafe
    /// name, wrong object kind for the mode, ...).
    Blocked,
    /// The feature does not support this case safely (missing RomM slug
    /// mapping, cross-filesystem move, directory source, ...).
    Unsupported,
}

impl OrganisationStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Suggested => "Suggested",
            Self::AlreadyOrganised => "Already organised",
            Self::Conflict => "Conflict",
            Self::Blocked => "Blocked",
            Self::Unsupported => "Unsupported",
        }
    }
}

/// One proposed organisation for one source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganisationPlanEntry {
    pub source_path: PathBuf,
    pub destination_path: PathBuf,
    /// Canonical platform id, when resolved strongly enough.
    pub platform: Option<String>,
    /// The platform's display name, for the user.
    pub platform_display_name: String,
    /// Human provenance of the platform assignment: "RomM", "Verified DAT",
    /// "Manual", "Existing game identity", ...
    pub platform_source: String,
    /// The canonical RomM-compatible platform slug, when a mapping exists.
    pub slug: Option<String>,
    pub mode: OrganisationMode,
    pub content_classification: Option<DatContentClassification>,
    pub original_metadata: DatOriginalMetadata,
    pub status: OrganisationStatus,
    /// Why the entry is Blocked / Unsupported / Conflict, when it is.
    pub reason: Option<String>,
}

/// A read-only organisation plan over one batch of candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganisationPlan {
    /// The configured canonical ROM root (never hard-coded).
    pub master_root: PathBuf,
    pub mode: OrganisationMode,
    pub content_policy: ContentSelectionPolicy,
    pub classifier_version: String,
    /// Bumped whenever the plan is (re)built; apply rejects a stale plan.
    pub generation: u64,
    pub entries: Vec<OrganisationPlanEntry>,
}

impl OrganisationPlan {
    /// The entries that are Suggested and therefore eligible to apply.
    pub fn suggested(&self) -> impl Iterator<Item = &OrganisationPlanEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.status == OrganisationStatus::Suggested)
    }
}
