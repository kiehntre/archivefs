//! Ties the Xenia Canary provider and local profile discovery together:
//! strict Title ID/Media ID/module-hash compatibility matching, per-patch
//! selection, a safe merge-and-render of the destination `.patch.toml`,
//! staging, and the shared preview. Apply and rollback reuse the generic
//! shared transaction framework unchanged - this module never writes to
//! the real destination itself, only to a private staging directory.
//!
//! Xenia's own dataset legitimately has multiple files sharing one Title
//! ID (different Title Update / module-hash variants are separate
//! upstream files), so - unlike Dolphin's one-file-per-game model -
//! candidates here are per *provider document*, not per title. The
//! destination filename is always the chosen candidate's own upstream
//! filename, so different TU variants never collide with each other.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::shared_preview::{
    PreviewAdapter, PreviewIdentity, PreviewIdentityKind, PreviewIdentityState,
    PreviewMatchStrength, PreviewSourceItem, SharedPreviewError, SharedPreviewReport,
    SharedPreviewRequest, build_shared_preview,
};
use super::xenia_patch_document::{
    XeniaPatch, XeniaPatchDocument, XeniaWriteValue, parse_xenia_patch_toml,
};
use super::xenia_provider::{XeniaProviderDocument, XeniaProviderResult};

pub const MAX_STAGED_XENIA_PATCH_BYTES: u64 = 512 * 1024;
pub const MAX_EXISTING_XENIA_PATCH_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XeniaInstallPlanErrorKind {
    NoSelectedPatches,
    SelectionInvalid,
    StagingUnavailable,
    GeneratedFileTooLarge,
    PreviewFailed,
    DestinationUnreadable,
    DestinationTooLarge,
    DestinationPathUnsafe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XeniaInstallPlanError {
    pub kind: XeniaInstallPlanErrorKind,
    pub path: Option<PathBuf>,
    pub detail: String,
}

impl std::fmt::Display for XeniaInstallPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for XeniaInstallPlanError {}

fn error(
    kind: XeniaInstallPlanErrorKind,
    path: Option<&Path>,
    detail: impl Into<String>,
) -> XeniaInstallPlanError {
    XeniaInstallPlanError {
        kind,
        path: path.map(Path::to_path_buf),
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------
// 1. Candidate matching
// ---------------------------------------------------------------------

/// Why no candidate could be built at all - distinct from a per-candidate
/// [`XeniaCandidateCompatibility::Incompatible`], which means candidates
/// exist but a specific one does not match this exact archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XeniaOutcomeBlockedReason {
    NoVerifiedTitleIdAvailable,
    InvalidVerifiedTitleId,
    NoPatchesReturnedByProvider,
}

impl XeniaOutcomeBlockedReason {
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::NoVerifiedTitleIdAvailable => {
                "EmuWiz has no separately verified Xbox 360 Title ID for this archive yet."
            }
            Self::InvalidVerifiedTitleId => "The verified Title ID is not eight hex characters.",
            Self::NoPatchesReturnedByProvider => {
                "The Xenia Canary game-patches provider returned no files for this Title ID."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum XeniaCandidateCompatibility {
    /// Title ID matches, and every declared constraint (Media ID, module
    /// hash) either matches or is not declared by the file at all.
    ExactCompatible,
    /// Title ID matches and no declared constraint is contradicted, but at
    /// least one constraint (almost always the module hash, which
    /// EmuWiz never computes) could not be independently verified.
    /// Selectable only after explicit user acknowledgement.
    PartiallyVerified,
    /// Title ID mismatch, or a declared Media ID this archive's own
    /// verified Media ID contradicts. Never selectable.
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct XeniaCandidateEvidence {
    pub label: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XeniaCandidate {
    pub source_path: String,
    pub title_name: String,
    pub title_id: String,
    pub media_ids: Vec<String>,
    pub hashes: Vec<String>,
    pub compatibility: XeniaCandidateCompatibility,
    pub evidence: Vec<XeniaCandidateEvidence>,
    pub document_warnings: Vec<String>,
    pub patches: Vec<XeniaPatch>,
    /// True whenever this file declares a module hash - which EmuWiz
    /// can never compute or verify without decoding the module. Kept
    /// distinct from `compatibility` so the GUI can always explain *why*
    /// a candidate is only partially verified, even when every other
    /// signal matches exactly.
    pub requires_unverified_module_hash: bool,
}

impl XeniaCandidate {
    #[must_use]
    pub fn manually_selectable(&self) -> bool {
        self.compatibility != XeniaCandidateCompatibility::Incompatible
            && self.patches.iter().any(XeniaPatch::is_selectable)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XeniaCandidateOutcome {
    pub candidates: Vec<XeniaCandidate>,
    pub blocked_reason: Option<XeniaOutcomeBlockedReason>,
}

/// Builds one candidate per upstream provider document, classified
/// strictly: exact Title ID is required for any candidate at all; a
/// declared Media ID this archive's own verified Media ID contradicts is
/// always `Incompatible`; a declared module hash is never independently
/// verifiable and always demotes an otherwise-exact match to
/// `PartiallyVerified`. Title similarity never elevates a candidate -
/// only exact byte-for-byte identity fields do.
#[must_use]
pub fn build_xenia_candidates(
    provider_result: &XeniaProviderResult,
    verified_title_id: Option<&str>,
    verified_media_id: Option<&str>,
) -> XeniaCandidateOutcome {
    let Some(title_id) = verified_title_id else {
        return XeniaCandidateOutcome {
            candidates: Vec::new(),
            blocked_reason: Some(XeniaOutcomeBlockedReason::NoVerifiedTitleIdAvailable),
        };
    };
    if title_id.len() != 8 || !title_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return XeniaCandidateOutcome {
            candidates: Vec::new(),
            blocked_reason: Some(XeniaOutcomeBlockedReason::InvalidVerifiedTitleId),
        };
    }
    if provider_result.documents.is_empty() {
        return XeniaCandidateOutcome {
            candidates: Vec::new(),
            blocked_reason: Some(XeniaOutcomeBlockedReason::NoPatchesReturnedByProvider),
        };
    }
    let candidates = provider_result
        .documents
        .iter()
        .map(|document| classify_candidate(document, title_id, verified_media_id))
        .collect();
    XeniaCandidateOutcome {
        candidates,
        blocked_reason: None,
    }
}

fn classify_candidate(
    provider_document: &XeniaProviderDocument,
    verified_title_id: &str,
    verified_media_id: Option<&str>,
) -> XeniaCandidate {
    let document = &provider_document.document;
    let mut evidence = Vec::new();

    let compatibility = if document.is_fatally_malformed() {
        evidence.push(candidate_evidence(
            "title_id",
            "file could not be parsed; Title ID is unavailable",
        ));
        XeniaCandidateCompatibility::Incompatible
    } else if document.title_id != verified_title_id {
        evidence.push(candidate_evidence(
            "title_id",
            format!(
                "file declares Title ID {}, not the verified {verified_title_id}",
                document.title_id
            ),
        ));
        XeniaCandidateCompatibility::Incompatible
    } else {
        evidence.push(candidate_evidence(
            "title_id",
            format!("exact Title ID match: {verified_title_id}"),
        ));
        let media_conflict = !document.media_ids.is_empty()
            && verified_media_id.is_some_and(|media_id| {
                !document
                    .media_ids
                    .iter()
                    .any(|declared| declared == media_id)
            });
        if media_conflict {
            evidence.push(candidate_evidence(
                "media_id",
                format!(
                    "file requires Media ID in {:?}; archive declares {}",
                    document.media_ids,
                    verified_media_id.unwrap_or("unknown")
                ),
            ));
            XeniaCandidateCompatibility::Incompatible
        } else {
            let media_unverified = !document.media_ids.is_empty() && verified_media_id.is_none();
            if document.media_ids.is_empty() {
                evidence.push(candidate_evidence(
                    "media_id",
                    "file declares no Media ID constraint",
                ));
            } else if let Some(media_id) = verified_media_id {
                evidence.push(candidate_evidence(
                    "media_id",
                    format!("exact Media ID match: {media_id}"),
                ));
            } else {
                evidence.push(candidate_evidence(
                    "media_id",
                    "file declares a Media ID constraint, but this archive's Media ID could not be verified",
                ));
            }
            let hash_unverifiable = !document.hashes.is_empty();
            if hash_unverifiable {
                evidence.push(candidate_evidence(
                    "module_hash",
                    format!(
                        "file requires one of {} module hash(es); EmuWiz cannot compute or verify a module hash",
                        document.hashes.len()
                    ),
                ));
            } else {
                evidence.push(candidate_evidence(
                    "module_hash",
                    "file declares no module hash constraint",
                ));
            }
            if hash_unverifiable || media_unverified {
                XeniaCandidateCompatibility::PartiallyVerified
            } else {
                XeniaCandidateCompatibility::ExactCompatible
            }
        }
    };

    XeniaCandidate {
        source_path: provider_document.source_path.clone(),
        title_name: document.title_name.clone(),
        title_id: document.title_id.clone(),
        media_ids: document.media_ids.clone(),
        hashes: document.hashes.clone(),
        compatibility,
        evidence,
        document_warnings: document
            .warnings
            .iter()
            .map(|warning| format!("{:?}: {}", warning.kind, warning.detail))
            .collect(),
        patches: document.patches.clone(),
        requires_unverified_module_hash: !document.hashes.is_empty(),
    }
}

fn candidate_evidence(label: &'static str, detail: impl Into<String>) -> XeniaCandidateEvidence {
    XeniaCandidateEvidence {
        label,
        detail: detail.into(),
    }
}

// ---------------------------------------------------------------------
// 2. Existing destination
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LoadedXeniaDestination {
    pub path: PathBuf,
    pub existed: bool,
    pub digest: Option<String>,
    pub document: Option<XeniaPatchDocument>,
}

/// Reads the exact destination file this candidate would install to, if
/// one already exists. A missing file is a valid, ordinary outcome - the
/// shared transaction creates it after preview and confirmation. Existing
/// symlinks or non-regular entries are refused, never followed.
pub fn load_xenia_destination(
    patches_directory: &Path,
    file_name: &str,
) -> Result<LoadedXeniaDestination, XeniaInstallPlanError> {
    let path = patches_directory.join(file_name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(error(
            XeniaInstallPlanErrorKind::DestinationPathUnsafe,
            Some(&path),
            "existing destination is a symlink; it is never followed",
        )),
        Ok(metadata) if !metadata.is_file() => Err(error(
            XeniaInstallPlanErrorKind::DestinationPathUnsafe,
            Some(&path),
            "existing destination is not a regular file",
        )),
        Ok(metadata) => {
            if metadata.len() > MAX_EXISTING_XENIA_PATCH_BYTES {
                return Err(error(
                    XeniaInstallPlanErrorKind::DestinationTooLarge,
                    Some(&path),
                    format!("existing destination exceeds {MAX_EXISTING_XENIA_PATCH_BYTES} bytes"),
                ));
            }
            let bytes = fs::read(&path).map_err(|failure| {
                error(
                    XeniaInstallPlanErrorKind::DestinationUnreadable,
                    Some(&path),
                    format!("existing destination could not be read: {failure}"),
                )
            })?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            Ok(LoadedXeniaDestination {
                path,
                existed: true,
                digest: Some(hex_sha256(&bytes)),
                document: Some(parse_xenia_patch_toml(&text)),
            })
        }
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            Ok(LoadedXeniaDestination {
                path,
                existed: false,
                digest: None,
                document: None,
            })
        }
        Err(failure) => Err(error(
            XeniaInstallPlanErrorKind::DestinationUnreadable,
            Some(&path),
            format!("existing destination could not be inspected: {failure}"),
        )),
    }
}

// ---------------------------------------------------------------------
// 3. Patch selection
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XeniaPatchSelectionEntry {
    /// Position in the candidate's own `patches` list - stable across a
    /// selection session, and the key the GUI uses when toggling.
    pub index: usize,
    pub name: String,
    pub author: String,
    pub description: String,
    pub selectable: bool,
    pub selected: bool,
    /// Whether a patch with this exact name is already enabled in the
    /// real destination file on disk right now.
    pub already_enabled: bool,
    pub warnings: Vec<String>,
}

/// The whole picker state for one chosen candidate. Nothing is selected
/// by default - unlike Dolphin's own-file model, this is normally a new
/// install of upstream content the user has not reviewed yet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct XeniaPatchSelection {
    pub compatibility: XeniaCandidateCompatibility,
    pub entries: Vec<XeniaPatchSelectionEntry>,
    /// Required before `can_apply()` ever returns `true` for a
    /// `PartiallyVerified` candidate - the explicit expert override the
    /// task requires, mirrored on the same "separate approval, never a
    /// casual bypass" shape the shared transaction already uses for
    /// replacing a different existing file.
    pub partial_verification_acknowledged: bool,
}

impl XeniaPatchSelection {
    #[must_use]
    pub fn from_candidate(
        candidate: &XeniaCandidate,
        existing: Option<&XeniaPatchDocument>,
    ) -> Self {
        let entries = candidate
            .patches
            .iter()
            .enumerate()
            .map(|(index, patch)| {
                let already_enabled = existing.is_some_and(|document| {
                    document.patches.iter().any(|existing_patch| {
                        existing_patch.name == patch.name && existing_patch.enabled_by_default
                    })
                });
                XeniaPatchSelectionEntry {
                    index,
                    name: patch.name.clone(),
                    author: patch.author.clone(),
                    description: patch.description.clone(),
                    selectable: patch.is_selectable(),
                    selected: false,
                    already_enabled,
                    warnings: patch
                        .warnings
                        .iter()
                        .map(|warning| format!("{:?}: {}", warning.kind, warning.detail))
                        .collect(),
                }
            })
            .collect();
        Self {
            compatibility: candidate.compatibility,
            entries,
            partial_verification_acknowledged: false,
        }
    }

    #[must_use]
    pub fn selected_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.selected).count()
    }

    #[must_use]
    pub fn selectable_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.selectable).count()
    }

    pub fn set_selected(&mut self, index: usize, selected: bool) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.index == index) else {
            return false;
        };
        if selected && !entry.selectable {
            return false;
        }
        entry.selected = selected;
        true
    }

    pub fn select_all(&mut self) {
        for entry in &mut self.entries {
            if entry.selectable {
                entry.selected = true;
            }
        }
    }

    pub fn clear_all(&mut self) {
        for entry in &mut self.entries {
            entry.selected = false;
        }
    }

    /// Whether this selection could ever be staged: at least one patch
    /// selected, and - for a `PartiallyVerified` candidate only - the
    /// explicit acknowledgement has been given. `Incompatible` candidates
    /// never reach a `XeniaPatchSelection` at all (the GUI never offers
    /// one), so they are not re-checked here.
    #[must_use]
    pub fn can_apply(&self) -> bool {
        if self.selected_count() == 0 {
            return false;
        }
        match self.compatibility {
            XeniaCandidateCompatibility::PartiallyVerified => {
                self.partial_verification_acknowledged
            }
            XeniaCandidateCompatibility::ExactCompatible => true,
            XeniaCandidateCompatibility::Incompatible => false,
        }
    }

    pub fn resolve_names(&self) -> Result<Vec<String>, XeniaInstallPlanError> {
        if !self.can_apply() {
            return Err(error(
                XeniaInstallPlanErrorKind::SelectionInvalid,
                None,
                if self.selected_count() == 0 {
                    "no patches are selected; choose at least one before applying".to_string()
                } else {
                    "a partially verified candidate requires explicit acknowledgement before it can be applied".to_string()
                },
            ));
        }
        Ok(self
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.name.clone())
            .collect())
    }
}

// ---------------------------------------------------------------------
// 4. Merge, render, and stage
// ---------------------------------------------------------------------

/// Merges the chosen candidate's own patches (with the user's selection
/// applied as `is_enabled`) into whatever the real destination file
/// already contains. Any existing patch whose name is *not* part of this
/// candidate is preserved completely untouched and unreordered ahead of
/// the candidate's own patches; this is the only path by which an
/// unrelated hand-added or previously-installed entry survives.
fn merge_patches(
    existing: Option<&XeniaPatchDocument>,
    candidate_patches: &[XeniaPatch],
    selected_names: &[String],
) -> Vec<XeniaPatch> {
    let candidate_names: BTreeSet<&str> = candidate_patches
        .iter()
        .map(|patch| patch.name.as_str())
        .collect();
    let mut merged = Vec::new();
    if let Some(document) = existing {
        for patch in &document.patches {
            if !candidate_names.contains(patch.name.as_str()) {
                merged.push(patch.clone());
            }
        }
    }
    for patch in candidate_patches {
        let mut patch = patch.clone();
        patch.enabled_by_default = selected_names.iter().any(|name| name == &patch.name);
        merged.push(patch);
    }
    merged
}

fn render_xenia_patch_toml(
    title_name: &str,
    title_id: &str,
    hashes: &[String],
    media_ids: &[String],
    patches: &[XeniaPatch],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("title_name = {title_name:?}\n"));
    out.push_str(&format!("title_id = {title_id:?}\n"));
    render_hex_field(&mut out, "hash", hashes);
    render_hex_field(&mut out, "media_id", media_ids);
    for patch in patches {
        out.push('\n');
        out.push_str("[[patch]]\n");
        out.push_str(&format!("    name = {:?}\n", patch.name));
        out.push_str(&format!("    desc = {:?}\n", patch.description));
        out.push_str(&format!("    author = {:?}\n", patch.author));
        out.push_str(&format!("    is_enabled = {}\n", patch.enabled_by_default));
        for write in &patch.writes {
            out.push_str(&format!("    [[patch.{}]]\n", write.kind.toml_key()));
            out.push_str(&format!("        address = 0x{:08x}\n", write.address));
            match &write.value {
                XeniaWriteValue::Integer(value) => {
                    out.push_str(&format!("        value = 0x{value:x}\n"));
                }
                XeniaWriteValue::Float(value) => {
                    out.push_str(&format!("        value = {value}\n"));
                }
                XeniaWriteValue::Text(value) | XeniaWriteValue::Utf16Text(value) => {
                    out.push_str(&format!("        value = {value:?}\n"));
                }
                XeniaWriteValue::Bytes(bytes) => {
                    let hex: String = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
                    out.push_str(&format!("        value = {hex:?}\n"));
                }
            }
        }
    }
    out
}

fn render_hex_field(out: &mut String, key: &str, values: &[String]) {
    match values.len() {
        0 => {}
        1 => out.push_str(&format!("{key} = {:?}\n", values[0])),
        _ => {
            out.push_str(&format!("{key} = [\n"));
            for value in values {
                out.push_str(&format!("    {value:?},\n"));
            }
            out.push_str("]\n");
        }
    }
}

#[derive(Debug, Clone)]
pub struct StagedXeniaPatchFile {
    pub staging_root: PathBuf,
    pub path: PathBuf,
    pub digest: String,
    pub contents: String,
    pub selected_patch_count: usize,
}

/// Renders the merged file and writes it atomically into a private
/// staging directory - the shared transaction installs files by digest
/// from an approved source root, and a generated/merged body has no
/// separate file in the profile to point at. The real destination file
/// is never written to directly here.
pub fn stage_xenia_patch_file(
    staging_root: &Path,
    file_name: &str,
    candidate: &XeniaCandidate,
    existing: Option<&XeniaPatchDocument>,
    selected_names: &[String],
) -> Result<StagedXeniaPatchFile, XeniaInstallPlanError> {
    if selected_names.is_empty() {
        return Err(error(
            XeniaInstallPlanErrorKind::NoSelectedPatches,
            None,
            "refusing to stage a patch file with no patches selected",
        ));
    }
    let merged = merge_patches(existing, &candidate.patches, selected_names);
    let contents = render_xenia_patch_toml(
        &candidate.title_name,
        &candidate.title_id,
        &candidate.hashes,
        &candidate.media_ids,
        &merged,
    );
    if contents.len() as u64 > MAX_STAGED_XENIA_PATCH_BYTES {
        return Err(error(
            XeniaInstallPlanErrorKind::GeneratedFileTooLarge,
            None,
            format!("generated patch file exceeds {MAX_STAGED_XENIA_PATCH_BYTES} bytes"),
        ));
    }
    fs::create_dir_all(staging_root).map_err(|failure| {
        error(
            XeniaInstallPlanErrorKind::StagingUnavailable,
            Some(staging_root),
            format!("staging directory unavailable: {failure}"),
        )
    })?;
    let path = staging_root.join(file_name);
    let temporary = staging_root.join(format!(".{file_name}.partial"));
    fs::write(&temporary, &contents).map_err(|failure| {
        error(
            XeniaInstallPlanErrorKind::StagingUnavailable,
            Some(&temporary),
            format!("staged file could not be written: {failure}"),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|failure| {
        let _ = fs::remove_file(&temporary);
        error(
            XeniaInstallPlanErrorKind::StagingUnavailable,
            Some(&path),
            format!("staged file could not be finalized: {failure}"),
        )
    })?;
    Ok(StagedXeniaPatchFile {
        staging_root: staging_root.to_path_buf(),
        path,
        digest: hex_sha256(contents.as_bytes()),
        contents,
        selected_patch_count: selected_names.len(),
    })
}

// ---------------------------------------------------------------------
// 5. Preview
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct XeniaInstallPreviewRequest {
    pub selected_archive: PathBuf,
    /// The Xenia profile's own root directory - the destination root
    /// every preview entry is relative to (matches the real
    /// `<configuration_path>/patches/<file>.patch.toml` layout exactly).
    pub configuration_path: PathBuf,
    pub title_id: String,
    pub compatibility: XeniaCandidateCompatibility,
    pub staged: StagedXeniaPatchFile,
}

#[derive(Debug, Clone)]
pub struct XeniaInstallPreview {
    pub report: SharedPreviewReport,
    pub staged: StagedXeniaPatchFile,
}

/// Wraps the staged file in the same shared preview every write-capable
/// adapter uses. `ExactCompatible` maps to `VerifiedExact`;
/// `PartiallyVerified` maps to `Strong` - the shared preview accepts both
/// for the Xenia adapter, but the transaction plan can only ever be built
/// once `XeniaPatchSelection::can_apply()` is satisfied, which for a
/// `Strong` match additionally requires the explicit acknowledgement.
pub fn build_xenia_install_preview(
    request: &XeniaInstallPreviewRequest,
) -> Result<XeniaInstallPreview, XeniaInstallPlanError> {
    let file_name = request
        .staged
        .path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            error(
                XeniaInstallPlanErrorKind::StagingUnavailable,
                Some(&request.staged.path),
                "staged file has no usable filename",
            )
        })?;
    let relative = PathBuf::from("patches").join(file_name);
    let match_strength = match request.compatibility {
        XeniaCandidateCompatibility::ExactCompatible => PreviewMatchStrength::VerifiedExact,
        XeniaCandidateCompatibility::PartiallyVerified => PreviewMatchStrength::Strong,
        XeniaCandidateCompatibility::Incompatible => PreviewMatchStrength::Unsupported,
    };
    let report = build_shared_preview(&SharedPreviewRequest {
        adapter: PreviewAdapter::Xenia,
        selected_archive: request.selected_archive.clone(),
        platform: Some("Xbox360".to_string()),
        identity: PreviewIdentity {
            kind: PreviewIdentityKind::XeniaTitleId,
            state: PreviewIdentityState::Verified,
            value: Some(request.title_id.clone()),
            archive_path: request.selected_archive.clone(),
            revision: None,
        },
        destination_root: request.configuration_path.clone(),
        source_items: vec![PreviewSourceItem {
            adapter: PreviewAdapter::Xenia,
            source_path: request.staged.path.clone(),
            expected_source_digest: Some(request.staged.digest.clone()),
            destination_relative_paths: vec![relative],
            match_strength,
        }],
    })
    .map_err(|failure| preview_error(&failure))?;
    Ok(XeniaInstallPreview {
        report,
        staged: request.staged.clone(),
    })
}

fn preview_error(failure: &SharedPreviewError) -> XeniaInstallPlanError {
    XeniaInstallPlanError {
        kind: XeniaInstallPlanErrorKind::PreviewFailed,
        path: None,
        detail: failure.to_string(),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch_manager::xenia_patch_document::parse_xenia_patch_toml;
    use crate::patch_manager::xenia_provider::XeniaProviderDocument;

    fn provider_result(documents: Vec<XeniaProviderDocument>) -> XeniaProviderResult {
        XeniaProviderResult {
            provider_id: "xenia_canary_game_patches".to_string(),
            provider_display_name: "Xenia Canary game-patches".to_string(),
            source_repository: "xenia-canary/game-patches".to_string(),
            source_commit: "1".repeat(40),
            retrieved_at_unix_seconds: 1_000,
            title_id: "415607D2".to_string(),
            documents,
            attribution: "test".to_string(),
            license: "test".to_string(),
            warnings: Vec::new(),
        }
    }

    fn document(source_path: &str, toml: &str) -> XeniaProviderDocument {
        XeniaProviderDocument {
            source_path: source_path.to_string(),
            document: parse_xenia_patch_toml(toml),
        }
    }

    const QUAKE4_TOML: &str = r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "Performance fix"
    desc = "Disables the FPS limit."
    author = "Sowa_95"
    is_enabled = false
    [[patch.be32]]
        address = 0x821b7140
        value = 0x39600001
"#;

    const CATHERINE_WITH_MEDIA_ID: &str = r#"
title_name = "Catherine"
title_id = "415407D7"
hash = "C451BB35FB61698F"
media_id = "580DEC6A"
[[patch]]
    name = "1920x1080 Resolution"
    desc = ""
    author = "Sowa_95"
    is_enabled = false
    [[patch.be16]]
        address = 0x8204a9a2
        value = 0x0780
"#;

    const NO_HASH_NO_MEDIA_TOML: &str = r#"
title_name = "Clean"
title_id = "415607D2"
[[patch]]
    name = "n"
    desc = ""
    author = "a"
    is_enabled = false
    [[patch.be8]]
        address = 0x1
        value = 0x1
"#;

    #[test]
    fn exact_title_id_and_module_hash_match_is_only_partially_verified() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        assert!(outcome.blocked_reason.is_none());
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(
            outcome.candidates[0].compatibility,
            XeniaCandidateCompatibility::PartiallyVerified
        );
        assert!(outcome.candidates[0].requires_unverified_module_hash);
    }

    #[test]
    fn no_hash_and_no_media_constraint_is_exact_compatible() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Clean.patch.toml",
            NO_HASH_NO_MEDIA_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        assert_eq!(
            outcome.candidates[0].compatibility,
            XeniaCandidateCompatibility::ExactCompatible
        );
    }

    #[test]
    fn media_id_constraint_with_verified_match_is_reported_as_matched() {
        let result = provider_result(vec![document(
            "patches/415407D7 - Catherine.patch.toml",
            CATHERINE_WITH_MEDIA_ID,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415407D7"), Some("580DEC6A"));
        assert_eq!(
            outcome.candidates[0].compatibility,
            XeniaCandidateCompatibility::PartiallyVerified // still gated by the module hash
        );
        assert!(
            outcome.candidates[0]
                .evidence
                .iter()
                .any(|item| item.label == "media_id" && item.detail.contains("exact"))
        );
    }

    #[test]
    fn incompatible_title_id_is_never_selectable() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D3"), None);
        assert_eq!(
            outcome.candidates[0].compatibility,
            XeniaCandidateCompatibility::Incompatible
        );
        assert!(!outcome.candidates[0].manually_selectable());
    }

    #[test]
    fn incompatible_media_id_is_rejected_even_with_matching_title_id() {
        let result = provider_result(vec![document(
            "patches/415407D7 - Catherine.patch.toml",
            CATHERINE_WITH_MEDIA_ID,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415407D7"), Some("FFFFFFFF"));
        assert_eq!(
            outcome.candidates[0].compatibility,
            XeniaCandidateCompatibility::Incompatible
        );
    }

    #[test]
    fn missing_module_hash_evidence_never_silently_upgrades_to_exact() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        assert_ne!(
            outcome.candidates[0].compatibility,
            XeniaCandidateCompatibility::ExactCompatible
        );
    }

    #[test]
    fn no_verified_title_id_blocks_before_any_candidate_is_built() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, None, None);
        assert!(outcome.candidates.is_empty());
        assert_eq!(
            outcome.blocked_reason,
            Some(XeniaOutcomeBlockedReason::NoVerifiedTitleIdAvailable)
        );
    }

    #[test]
    fn partially_verified_candidate_requires_acknowledgement_before_it_can_apply() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        let candidate = &outcome.candidates[0];
        let mut selection = XeniaPatchSelection::from_candidate(candidate, None);
        selection.select_all();
        assert!(!selection.can_apply(), "not acknowledged yet");
        assert!(selection.resolve_names().is_err());
        selection.partial_verification_acknowledged = true;
        assert!(selection.can_apply());
        assert_eq!(selection.resolve_names().unwrap(), vec!["Performance fix"]);
    }

    #[test]
    fn exact_compatible_candidate_never_needs_acknowledgement() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Clean.patch.toml",
            NO_HASH_NO_MEDIA_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        let candidate = &outcome.candidates[0];
        let mut selection = XeniaPatchSelection::from_candidate(candidate, None);
        selection.select_all();
        assert!(selection.can_apply());
    }

    #[test]
    fn already_enabled_reflects_the_real_destination_file_not_upstream_defaults() {
        let existing = parse_xenia_patch_toml(
            r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "Performance fix"
    desc = ""
    author = "Sowa_95"
    is_enabled = true
    [[patch.be32]]
        address = 0x821b7140
        value = 0x39600001
"#,
        );
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        let selection =
            XeniaPatchSelection::from_candidate(&outcome.candidates[0], Some(&existing));
        assert!(selection.entries[0].already_enabled);
        assert!(
            !selection.entries[0].selected,
            "already_enabled never auto-selects"
        );
    }

    #[test]
    fn staging_preserves_an_unrelated_existing_patch_and_writes_only_the_selection() {
        let existing = parse_xenia_patch_toml(
            r#"
title_name = "Quake 4"
title_id = "415607D2"
hash = "4768B579A3C5F134"
[[patch]]
    name = "Hand added"
    desc = "kept"
    author = "someone"
    is_enabled = true
    [[patch.be8]]
        address = 0x9999
        value = 0x1
"#,
        );
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        let candidate = &outcome.candidates[0];
        let staging_root = std::env::temp_dir().join(format!(
            "archivefs-xenia-install-plan-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let staged = stage_xenia_patch_file(
            &staging_root,
            "415607D2 - Quake 4.patch.toml",
            candidate,
            Some(&existing),
            &["Performance fix".to_string()],
        )
        .unwrap();
        assert!(staged.contents.contains("Hand added"));
        assert!(staged.contents.contains("Performance fix"));
        let rendered = parse_xenia_patch_toml(&staged.contents);
        let hand_added = rendered
            .patches
            .iter()
            .find(|patch| patch.name == "Hand added")
            .unwrap();
        assert!(
            hand_added.enabled_by_default,
            "unrelated patch stays enabled"
        );
        let performance = rendered
            .patches
            .iter()
            .find(|patch| patch.name == "Performance fix")
            .unwrap();
        assert!(performance.enabled_by_default);
        let _ = fs::remove_dir_all(&staging_root);
    }

    #[test]
    fn staging_with_no_selected_patches_is_refused() {
        let result = provider_result(vec![document(
            "patches/415607D2 - Quake 4.patch.toml",
            QUAKE4_TOML,
        )]);
        let outcome = build_xenia_candidates(&result, Some("415607D2"), None);
        let staging_root = std::env::temp_dir().join("archivefs-xenia-install-plan-empty-test");
        let result = stage_xenia_patch_file(
            &staging_root,
            "x.patch.toml",
            &outcome.candidates[0],
            None,
            &[],
        );
        assert!(result.is_err());
    }
}
