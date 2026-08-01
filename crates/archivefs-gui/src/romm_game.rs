//! The selected game's RomM identity: what RomM says about one local file, an
//! explicit hash verification, and its cover.
//!
//! # Provider-scoped
//!
//! Everything here describes RomM and only RomM. CheatBase evidence is not merged
//! in, and no verdict shown here claims to be ArchiveFS's combined opinion of the
//! file - that belongs to a later stage. This panel answers one question: what does
//! the imported RomM catalogue say about the file selected in the Library?
//!
//! # Nothing happens because a panel opened
//!
//! Opening the panel reads the published cache and `symlink_metadata`. It does not
//! hash, does not contact RomM, and does not write. Hashing happens when someone
//! presses Verify local file; a cover is fetched when someone presses Show cover.
//! Both are single, explicit, cancellable actions.
//!
//! # Ambiguity is shown, never resolved by picking the first
//!
//! When several RomM records translate to the same local file, the verdict is
//! Ambiguous and every claimant is listed. Choosing one is a person's decision,
//! recorded in the panel, and it still does not overwrite anything ArchiveFS
//! determined for itself.
//!
//! # Confirmed means hashed
//!
//! The six verdicts keep their exact meanings. In particular `Strong` is drawn as
//! Strong: RomM published hashes but this file has not been read, so nothing has
//! been compared. Only an explicit verification whose comparison agreed produces
//! Confirmed.

use std::path::{Path, PathBuf};

use archivefs_core::identity_source::artwork::{
    CachedThumbnail, THUMBNAIL_MAX_HEIGHT, THUMBNAIL_MAX_WIDTH,
};
use archivefs_core::identity_source::cache::IdentityCache;
use archivefs_core::identity_source::hashing::{LocalHashCache, LocalHashes};
use archivefs_core::identity_source::matching::{
    LocalFileFacts, LocalPresence, PathClaims, match_record,
};
use archivefs_core::identity_source::model::{
    ExternalIdentityRecord, ExternalVerification, LocalEvidenceStrength,
};
use eframe::egui;

use crate::romm_browse::{
    CacheIdentity, ConflictLineView, presence_explanation, presence_label, presence_tone,
    verdict_explanation, verdict_label, verdict_tone,
};
use crate::romm_source::{CardRow, human_bytes};
use crate::ui::{components as widgets, theme};

/// The most claimants one panel lists.
///
/// A contested path with more than a handful of claimants is a configuration
/// problem, not a choice to scroll through; the count is still reported in full.
pub(crate) const MAX_CANDIDATES: usize = 8;

/// The largest thumbnail file this reads back from the artwork cache.
///
/// The cache never writes anything larger, so this only bounds what happens if a
/// file in it were replaced by something else.
pub(crate) const MAX_THUMBNAIL_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Which algorithms a verification computes, as one line of text.
pub(crate) const VERIFIED_ALGORITHMS: &str = "CRC32, MD5 and SHA-1, in one pass";

/// What ArchiveFS itself says this file's platform is.
///
/// `manual` is the part that matters: a deliberate per-archive assignment is local
/// evidence of the strongest kind, so a RomM record that disagrees with it produces
/// Ambiguous rather than a quiet correction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LocalPlatformClaim {
    pub(crate) platform: Option<String>,
    pub(crate) manual: bool,
}

impl LocalPlatformClaim {
    pub(crate) fn strength(&self) -> LocalEvidenceStrength {
        match (self.platform.is_some(), self.manual) {
            (true, true) => LocalEvidenceStrength::Verified,
            (true, false) => LocalEvidenceStrength::Weak,
            (false, _) => LocalEvidenceStrength::None,
        }
    }

    fn description(&self) -> Option<String> {
        let platform = self.platform.as_deref()?;
        Some(if self.manual {
            format!("{platform}, assigned by hand")
        } else {
            format!("{platform}, detected automatically")
        })
    }
}

/// Whether a cover can be fetched for a record, and why not when it cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtworkAvailability {
    /// RomM recorded no artwork at all.
    None,
    /// RomM recorded only a scraper URL on a public host. Never fetched.
    PublicOnly,
    /// RomM has its own small cover, which is the only thing this fetches.
    Fetchable,
}

impl ArtworkAvailability {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "no cover recorded",
            Self::PublicOnly => "only a public scraper link",
            Self::Fetchable => "RomM has a small cover",
        }
    }

    /// Why no cover can be shown, in the project's own words.
    pub(crate) fn explanation(self) -> Option<&'static str> {
        match self {
            Self::None => Some("RomM recorded no cover for this game, so there is none to show."),
            Self::PublicOnly => Some(
                "The only cover RomM recorded is a link to a public metadata site such as IGDB or \
                 RetroAchievements. ArchiveFS fetches covers from your RomM server only, so this \
                 one is not requested.",
            ),
            Self::Fetchable => None,
        }
    }
}

/// One RomM record that could describe the selected file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateView {
    pub(crate) romm_game_id: String,
    pub(crate) romm_platform_id: Option<String>,
    pub(crate) title: String,
    pub(crate) canonical_platform: Option<String>,
    pub(crate) romm_platform: Option<String>,
    pub(crate) romm_path: String,
    /// This candidate's own verdict, recomputed against the file as it is now.
    pub(crate) verdict: ExternalVerification,
    pub(crate) verdict_explanation: String,
    pub(crate) rows: Vec<CardRow>,
    pub(crate) published_hashes: Vec<CardRow>,
    pub(crate) file_size_bytes: Option<u64>,
    pub(crate) regions: Vec<String>,
    pub(crate) revision: Option<String>,
    pub(crate) evidence: Vec<String>,
    pub(crate) conflicts: Vec<ConflictLineView>,
    pub(crate) artwork: ArtworkAvailability,
    pub(crate) related_files: usize,
    pub(crate) siblings: usize,
    pub(crate) provenance: String,
    /// Whether a stored local hash was actually compared for this candidate.
    pub(crate) hash_compared: bool,
}

impl CandidateView {
    /// The compact label used in activity history, so a private path is never
    /// written there when a platform and title are available.
    pub(crate) fn compact_label(&self) -> String {
        match &self.canonical_platform {
            Some(platform) => format!("{} ({platform})", self.title),
            None => self.title.clone(),
        }
    }
}

/// A local hash that has already been computed and stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredHashesView {
    pub(crate) rows: Vec<CardRow>,
    pub(crate) bytes_hashed: u64,
}

impl StoredHashesView {
    fn of(hashes: &LocalHashes) -> Self {
        Self {
            rows: vec![
                CardRow {
                    label: "CRC32".to_string(),
                    value: hashes.crc32.clone(),
                },
                CardRow {
                    label: "MD5".to_string(),
                    value: hashes.md5.clone(),
                },
                CardRow {
                    label: "SHA-1".to_string(),
                    value: hashes.sha1.clone(),
                },
            ],
            bytes_hashed: hashes.bytes_hashed,
        }
    }
}

/// Everything the panel draws for one selected file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GameIdentityPanel {
    /// Which cache produced this, so a page from a superseded import is discarded.
    pub(crate) cache: CacheIdentity,
    /// The file this describes. A result for any other path is discarded.
    pub(crate) local_path: PathBuf,
    /// A short name for the file, for headings.
    pub(crate) file_label: String,
    pub(crate) presence: LocalPresence,
    pub(crate) presence_explanation: Option<String>,
    pub(crate) local_size_bytes: Option<u64>,
    pub(crate) local_platform: Option<String>,
    /// The RomM verdict for this file, provider-scoped.
    pub(crate) verdict: ExternalVerification,
    pub(crate) verdict_explanation: String,
    /// One sentence saying what was found, in plain words.
    pub(crate) summary: String,
    pub(crate) candidates: Vec<CandidateView>,
    /// Which candidate the panel is showing, when one can be shown at all.
    pub(crate) chosen: Option<usize>,
    /// How many claimants there were beyond the ones listed.
    pub(crate) claimants_not_listed: usize,
    /// Total claimants, listed or not.
    pub(crate) claimants: usize,
    pub(crate) stored_hashes: Option<StoredHashesView>,
    /// `None` when Verify local file can be offered; otherwise why it cannot.
    pub(crate) verify_blocker: Option<String>,
    /// Whether a manual platform assignment is in force for this file.
    pub(crate) manual_platform: bool,
}

impl GameIdentityPanel {
    /// Whether more than one RomM record claims this file.
    pub(crate) fn is_ambiguous(&self) -> bool {
        self.claimants > 1
    }

    pub(crate) fn chosen_candidate(&self) -> Option<&CandidateView> {
        self.chosen.and_then(|index| self.candidates.get(index))
    }

    /// The rows describing the local file itself.
    pub(crate) fn local_rows(&self) -> Vec<CardRow> {
        let mut rows = vec![CardRow {
            label: "Local file".to_string(),
            value: presence_label(self.presence).to_string(),
        }];
        if let Some(bytes) = self.local_size_bytes {
            rows.push(CardRow {
                label: "Size on disk".to_string(),
                value: human_bytes(bytes),
            });
        }
        if let Some(platform) = &self.local_platform {
            rows.push(CardRow {
                label: "ArchiveFS platform".to_string(),
                value: platform.clone(),
            });
        }
        rows
    }
}

/// Resolves what RomM says about one local file.
///
/// Pure apart from the observation it is handed, which reads metadata only. Every
/// verdict is recomputed here rather than read from the cached record's stored
/// verdict, because a stored hash may have appeared since the import and the file may
/// have changed.
///
/// `facts_for` is injected rather than called directly so the whole resolution is
/// testable without a filesystem - and so no test can accidentally read a real file.
pub(crate) fn resolve_selected_game(
    cache: &IdentityCache,
    local_path: &Path,
    verified: &LocalHashCache,
    local_platform: &LocalPlatformClaim,
    chosen_game_id: Option<&str>,
    facts_for: &dyn Fn(&Path) -> LocalFileFacts,
) -> GameIdentityPanel {
    let facts = facts_for(local_path).with_local_platform(
        local_platform.platform.as_deref(),
        local_platform.strength(),
    );
    let presence = facts.presence;
    let claims = PathClaims::of(&cache.records);

    // Every record that translates to exactly this file. Not the first match: the
    // count is what decides whether this is ambiguous.
    let claiming: Vec<&ExternalIdentityRecord> = cache
        .records
        .iter()
        .filter(|record| record.archivefs_path.as_deref() == Some(local_path))
        .collect();
    let claimants = claiming.len();

    let mut candidates: Vec<CandidateView> = claiming
        .iter()
        .take(MAX_CANDIDATES)
        .map(|record| candidate_view(record, &facts, &claims, verified))
        .collect();
    // Ordered by title, then by RomM's own id. Deliberately not by verdict: every
    // record claiming a contested path scores Ambiguous for that very reason, so
    // ranking by verdict would only look like it meant something. A stable order is
    // what a person needs to compare two claimants across redraws.
    candidates.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then_with(|| left.romm_game_id.cmp(&right.romm_game_id))
    });

    // A person's explicit choice wins over the ranking; otherwise a single claimant
    // is shown and a contested path shows nothing until someone chooses.
    let chosen = chosen_game_id
        .and_then(|wanted| {
            candidates
                .iter()
                .position(|candidate| candidate.romm_game_id == wanted)
        })
        .or_else(|| (claimants == 1).then_some(0));

    let verdict = if claimants > 1 && chosen_game_id.is_none() {
        // Two records claiming one file is a real ambiguity, whatever either of them
        // would score alone.
        ExternalVerification::Ambiguous
    } else {
        chosen
            .and_then(|index| candidates.get(index))
            .map(|candidate| candidate.verdict)
            .unwrap_or(ExternalVerification::Unmatched)
    };

    let stored_hashes = verified.get(local_path).map(StoredHashesView::of);
    let verify_blocker = verify_blocker(presence, chosen.and_then(|index| candidates.get(index)));

    GameIdentityPanel {
        cache: CacheIdentity::of(cache),
        local_path: local_path.to_path_buf(),
        file_label: file_label(local_path),
        presence,
        presence_explanation: presence_explanation(presence),
        local_size_bytes: facts.fingerprint.as_ref().map(|print| print.size_bytes),
        local_platform: local_platform.description(),
        verdict,
        verdict_explanation: verdict_explanation(verdict).to_string(),
        summary: summary_for(claimants, verdict, presence, chosen_game_id.is_some()),
        candidates,
        chosen,
        claimants_not_listed: claimants.saturating_sub(MAX_CANDIDATES),
        claimants,
        stored_hashes,
        verify_blocker,
        manual_platform: local_platform.manual,
    }
}

fn candidate_view(
    record: &ExternalIdentityRecord,
    facts: &LocalFileFacts,
    claims: &PathClaims,
    verified: &LocalHashCache,
) -> CandidateView {
    let outcome = match_record(record, facts, claims, verified);
    let mut rows = Vec::new();
    rows.push(CardRow {
        label: "RomM game".to_string(),
        value: record.provider_game_id.clone(),
    });
    if let Some(platform) = &record.provider_platform_name {
        rows.push(CardRow {
            label: "RomM platform".to_string(),
            value: platform.clone(),
        });
    }
    if let Some(platform) = &record.platform_candidate {
        rows.push(CardRow {
            label: "Canonical platform".to_string(),
            value: platform.clone(),
        });
    }
    if let Some(bytes) = record.file_size_bytes {
        rows.push(CardRow {
            label: "Size RomM recorded".to_string(),
            value: human_bytes(bytes),
        });
    }
    if let Some(revision) = &record.revision {
        rows.push(CardRow {
            label: "Revision".to_string(),
            value: revision.clone(),
        });
    }
    if !record.regions.is_empty() {
        rows.push(CardRow {
            label: "Regions".to_string(),
            value: record.regions.join(", "),
        });
    }

    CandidateView {
        romm_game_id: record.provider_game_id.clone(),
        romm_platform_id: record.provider_platform_id.clone(),
        title: record
            .title
            .clone()
            .unwrap_or_else(|| "(untitled)".to_string()),
        canonical_platform: record.platform_candidate.clone(),
        romm_platform: record.provider_platform_name.clone(),
        romm_path: record.provider_path.clone(),
        verdict: outcome.verification,
        verdict_explanation: verdict_explanation(outcome.verification).to_string(),
        rows,
        published_hashes: record
            .hashes
            .iter()
            .map(|hash| CardRow {
                label: hash.algorithm.label().to_string(),
                value: hash.value.clone(),
            })
            .collect(),
        file_size_bytes: record.file_size_bytes,
        regions: record.regions.clone(),
        revision: record.revision.clone(),
        evidence: outcome.evidence,
        conflicts: outcome
            .conflicts
            .iter()
            .map(|conflict| ConflictLineView {
                field: conflict.field.label().to_string(),
                romm: conflict.external.clone(),
                local: conflict.local.clone(),
                detail: conflict.detail.clone(),
            })
            .collect(),
        artwork: availability_of(record),
        related_files: record.related_files.len(),
        siblings: record.sibling_game_ids.len(),
        provenance: record.server_id.clone(),
        hash_compared: outcome.hash_compared,
    }
}

pub(crate) fn availability_of(record: &ExternalIdentityRecord) -> ArtworkAvailability {
    match record.artwork.as_ref() {
        None => ArtworkAvailability::None,
        Some(artwork) if artwork.small_reference.is_some() => ArtworkAvailability::Fetchable,
        Some(_) => ArtworkAvailability::PublicOnly,
    }
}

/// Why Verify local file cannot be offered, or `None` when it can.
///
/// This is advisory: the worker refuses again for itself, because the file may change
/// between the panel being drawn and the button being pressed.
fn verify_blocker(presence: LocalPresence, candidate: Option<&CandidateView>) -> Option<String> {
    match presence {
        LocalPresence::File => {}
        LocalPresence::Directory => {
            return Some(
                "This is a folder, and a folder has no single set of bytes to hash. Folder-based \
                 games are recognised as present, but they cannot be hash-verified."
                    .to_string(),
            );
        }
        LocalPresence::DanglingSymlink => {
            return Some(
                "This is a symbolic link whose target is missing, so there is nothing to read."
                    .to_string(),
            );
        }
        LocalPresence::Absent => {
            return Some("There is no file at the mapped path.".to_string());
        }
        LocalPresence::ParentAbsent => {
            return Some(
                "The folder the mapped path lives in does not exist either, so no mapping reaches \
                 this record."
                    .to_string(),
            );
        }
        LocalPresence::Other => {
            return Some(
                "This is not a regular file - a device, socket or pipe - and ArchiveFS will not \
                 read it."
                    .to_string(),
            );
        }
    }
    let Some(candidate) = candidate else {
        return Some(
            "No RomM record maps to this file, so a hash would have nothing to be compared \
             against."
                .to_string(),
        );
    };
    if candidate.published_hashes.is_empty() {
        return Some(
            "RomM published no hash for this game, so hashing the file locally would produce \
             nothing to compare it against."
                .to_string(),
        );
    }
    None
}

fn summary_for(
    claimants: usize,
    verdict: ExternalVerification,
    presence: LocalPresence,
    chosen: bool,
) -> String {
    if claimants == 0 {
        return "No record in the imported RomM catalogue maps to this file.".to_string();
    }
    if claimants > 1 && !chosen {
        return format!(
            "{claimants} RomM records map to this same file, so which one describes it cannot be \
             decided automatically. Choose one below to see its evidence; nothing is changed by \
             choosing."
        );
    }
    match verdict {
        ExternalVerification::ConfirmedExternal => {
            "This file was hashed and its hash matched what RomM published.".to_string()
        }
        ExternalVerification::StrongExternal => {
            "RomM's record agrees with everything ArchiveFS can check without reading the file. \
             The bytes themselves have not been compared."
                .to_string()
        }
        ExternalVerification::ProbableExternal => {
            "RomM's record is consistent with this file, but neither its size nor its hash could \
             be confirmed."
                .to_string()
        }
        ExternalVerification::Ambiguous => {
            "RomM's record and this file disagree about something that matters, so no single \
             identity can be claimed."
                .to_string()
        }
        ExternalVerification::Stale => format!(
            "RomM has a record for this path, but {}.",
            presence_label(presence).to_lowercase()
        ),
        ExternalVerification::Unmatched => {
            "RomM has a record that maps here, but nothing in it could be corroborated against \
             the file."
                .to_string()
        }
    }
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

// --- Hashing -------------------------------------------------------------

/// How far an explicit verification has got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HashProgressView {
    /// The file's name only. The full path is not carried into progress text.
    pub(crate) file_label: String,
    pub(crate) bytes_read: u64,
    pub(crate) total_bytes: u64,
    pub(crate) elapsed_seconds: u64,
    pub(crate) cancellation_requested: bool,
}

impl HashProgressView {
    pub(crate) fn fraction(&self) -> Option<f32> {
        (self.total_bytes > 0)
            .then(|| (self.bytes_read as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32)
    }

    /// The progress line, as text rather than as a bar alone - a bar conveys
    /// nothing to someone reading the screen aloud.
    pub(crate) fn line(&self) -> String {
        let percentage = self
            .fraction()
            .map(|fraction| format!(" ({:.0}%)", fraction * 100.0))
            .unwrap_or_default();
        let elapsed = format_elapsed(self.elapsed_seconds);
        let cancelling = if self.cancellation_requested {
            " Stopping."
        } else {
            ""
        };
        format!(
            "Reading {} - {} of {}{percentage}, {elapsed} elapsed. Computing {VERIFIED_ALGORITHMS}.{cancelling}",
            self.file_label,
            human_bytes(self.bytes_read),
            human_bytes(self.total_bytes),
        )
    }
}

fn format_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format!("{}m {}s", seconds / 60, seconds % 60)
}

/// One published hash beside the one just computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HashComparisonView {
    pub(crate) algorithm: String,
    pub(crate) romm: String,
    pub(crate) local: String,
    pub(crate) agrees: bool,
}

/// What an explicit verification concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerificationOutcomeView {
    pub(crate) local_path: PathBuf,
    pub(crate) file_label: String,
    /// The compact platform/title label, for activity history.
    pub(crate) compact_label: String,
    pub(crate) romm_game_id: String,
    pub(crate) comparisons: Vec<HashComparisonView>,
    /// Whether every algorithm both sides published agreed.
    pub(crate) all_agree: bool,
    /// Whether any algorithm disagreed. Both are recorded because RomM sometimes
    /// publishes one correct hash and one that is wrong for the same file.
    pub(crate) any_disagree: bool,
    pub(crate) verdict_before: ExternalVerification,
    pub(crate) verdict_after: ExternalVerification,
    pub(crate) bytes_hashed: u64,
    pub(crate) elapsed_seconds: u64,
    /// Where the verification was recorded, so it is clear that nothing else moved.
    pub(crate) stored_at: Option<PathBuf>,
    /// The panel, rebuilt from the stored verification.
    pub(crate) panel: Box<GameIdentityPanel>,
}

impl VerificationOutcomeView {
    /// The sentence stating what happened, and what it does and does not mean.
    pub(crate) fn conclusion(&self) -> String {
        if self.comparisons.is_empty() {
            return "The file was hashed, but RomM published no hash to compare it against, so \
                    nothing was confirmed."
                .to_string();
        }
        if self.all_agree {
            return format!(
                "Every hash RomM published matches this file. {} is now Confirmed.",
                self.compact_label
            );
        }
        if self.any_disagree && self.comparisons.iter().any(|line| line.agrees) {
            return "Some of the hashes RomM published match this file and some do not. That \
                    usually means RomM's own metadata for this game is inconsistent rather than \
                    that you have a different dump - both values are shown below."
                .to_string();
        }
        "None of the hashes RomM published match this file. This is a different dump from the one \
         RomM describes; nothing was changed, and both values are shown below."
            .to_string()
    }

    pub(crate) fn tone(&self) -> widgets::StatusTone {
        if self.all_agree && !self.comparisons.is_empty() {
            widgets::StatusTone::Success
        } else if self.comparisons.is_empty() {
            widgets::StatusTone::Pending
        } else {
            widgets::StatusTone::Warning
        }
    }

    /// Whether the verdict actually moved to Confirmed. Reported rather than assumed:
    /// a verification that agreed still cannot promote a record whose platform
    /// disagrees.
    pub(crate) fn promoted(&self) -> bool {
        self.verdict_after == ExternalVerification::ConfirmedExternal
            && self.verdict_before != ExternalVerification::ConfirmedExternal
    }
}

/// Builds the comparison lines for a computed hash against a record's published ones.
pub(crate) fn compare_hashes(
    record: &ExternalIdentityRecord,
    local: &LocalHashes,
) -> Vec<HashComparisonView> {
    record
        .hashes
        .iter()
        .map(|published| HashComparisonView {
            algorithm: published.algorithm.label().to_string(),
            romm: published.value.clone(),
            local: local.value(published.algorithm).to_string(),
            agrees: local.agrees_with(published),
        })
        .collect()
}

// --- Covers --------------------------------------------------------------

/// A decoded thumbnail, ready for the UI thread to upload.
///
/// Decoding happens on the worker: a 200x280 upload is cheap, a decode is not, and
/// the UI thread must not stall on either.
#[derive(Clone, Debug)]
pub(crate) struct CoverImage {
    pub(crate) key: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes: u64,
    pub(crate) image: egui::ColorImage,
    /// Whether this came from the cache without a request being made.
    pub(crate) from_cache: bool,
}

impl PartialEq for CoverImage {
    fn eq(&self, other: &Self) -> bool {
        // Compared by identity rather than by pixels: two thumbnails with the same
        // key are the same cover, and comparing 224,000 bytes to say so is waste.
        self.key == other.key
            && self.width == other.width
            && self.height == other.height
            && self.bytes == other.bytes
            && self.from_cache == other.from_cache
    }
}

/// What the cover area is showing.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum CoverState {
    /// Nothing asked for yet. This is what an opened panel starts at.
    #[default]
    Idle,
    Loading,
    Ready(Box<CoverImage>),
    /// RomM recorded no cover, or only a public one.
    Unavailable(ArtworkAvailability),
    /// A fetch or decode was refused. The core's own wording, which never carries a
    /// URL or a token.
    Refused(String),
    Cancelled,
}

impl CoverState {
    pub(crate) fn line(&self) -> String {
        match self {
            Self::Idle => "No cover requested.".to_string(),
            Self::Loading => "Fetching the cover from your RomM server.".to_string(),
            Self::Ready(image) => format!(
                "{}x{} cover, {}{}.",
                image.width,
                image.height,
                human_bytes(image.bytes),
                if image.from_cache {
                    ", already cached - no request was made"
                } else {
                    ", newly fetched"
                }
            ),
            Self::Unavailable(availability) => availability
                .explanation()
                .unwrap_or("No cover is available.")
                .to_string(),
            Self::Refused(detail) => detail.clone(),
            Self::Cancelled => "The cover fetch was stopped. Nothing was cached.".to_string(),
        }
    }

    pub(crate) fn tone(&self) -> widgets::StatusTone {
        match self {
            Self::Ready(_) => widgets::StatusTone::Success,
            Self::Refused(_) => widgets::StatusTone::Warning,
            Self::Idle | Self::Loading | Self::Unavailable(_) | Self::Cancelled => {
                widgets::StatusTone::Pending
            }
        }
    }
}

/// One cover result, bound to what asked for it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CoverOutcome {
    pub(crate) local_path: PathBuf,
    pub(crate) romm_game_id: String,
    pub(crate) state: CoverState,
    /// Artwork cache totals as they are now, read after the fetch rather than
    /// incremented, so a concurrent clear cannot leave a wrong figure on screen.
    pub(crate) cached_items: u64,
    pub(crate) cached_bytes: u64,
}

/// Decodes a cached thumbnail into pixels, on the worker.
///
/// PNG only, which is the only thing the artwork cache writes. The read is bounded
/// because a file in ArchiveFS's own cache directory is still a file on disk.
pub(crate) fn decode_thumbnail(
    thumbnail: &CachedThumbnail,
    from_cache: bool,
) -> Result<CoverImage, String> {
    use image::ImageDecoder as _;

    let metadata = std::fs::metadata(&thumbnail.path)
        .map_err(|_| "The cached cover could not be read.".to_string())?;
    if metadata.len() > MAX_THUMBNAIL_READ_BYTES {
        return Err("The cached cover is larger than a thumbnail should ever be.".to_string());
    }
    let bytes = std::fs::read(&thumbnail.path)
        .map_err(|_| "The cached cover could not be read.".to_string())?;
    let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(&bytes))
        .map_err(|_| "The cached cover is not a readable PNG.".to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(THUMBNAIL_MAX_WIDTH);
    limits.max_image_height = Some(THUMBNAIL_MAX_HEIGHT);
    limits.max_alloc = Some(MAX_THUMBNAIL_READ_BYTES);
    decoder
        .set_limits(limits)
        .map_err(|_| "The cached cover is larger than a thumbnail should ever be.".to_string())?;
    let (width, height) = decoder.dimensions();
    let mut pixels = vec![0_u8; usize::try_from(decoder.total_bytes()).unwrap_or(0)];
    let colour_type = decoder.color_type();
    decoder
        .read_image(&mut pixels)
        .map_err(|_| "The cached cover could not be decoded.".to_string())?;
    let rgba = match colour_type {
        image::ColorType::Rgba8 => pixels,
        image::ColorType::Rgb8 => pixels
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
            .collect(),
        other => {
            return Err(format!(
                "The cached cover uses a {other:?} layout this build does not read."
            ));
        }
    };
    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
    Ok(CoverImage {
        key: thumbnail.key.clone(),
        width,
        height,
        bytes: thumbnail.bytes,
        image,
        from_cache,
    })
}

// --- Panel state ---------------------------------------------------------

/// What the app holds for the panel.
#[derive(Default)]
pub(crate) struct GamePanelState {
    /// The file the panel is about. Everything else is discarded when this changes.
    pub(crate) local_path: Option<PathBuf>,
    pub(crate) panel: Option<Box<GameIdentityPanel>>,
    /// A person's explicit choice among claimants.
    pub(crate) chosen_game_id: Option<String>,
    pub(crate) verification: Option<Box<VerificationOutcomeView>>,
    pub(crate) cover: CoverState,
    /// Uploaded on the UI thread, and dropped when the cover changes.
    pub(crate) cover_texture: Option<egui::TextureHandle>,
    pub(crate) cover_key: Option<String>,
    /// The artwork cache's totals as read after the last cover load. Read from the
    /// cache rather than incremented, so a clear cannot leave a stale figure here.
    pub(crate) cover_cache: Option<(u64, u64)>,
    /// Set when a result arrived that no longer answers what is on screen.
    pub(crate) needs_reload: bool,
    /// Whether someone closed the panel for this selection.
    ///
    /// Kept per selection rather than globally: closing it for one game should not
    /// hide it for the next one chosen.
    pub(crate) dismissed: bool,
}

impl GamePanelState {
    /// Points the panel at a different file, discarding everything about the old one.
    ///
    /// The texture is dropped with it: showing one game's cover beside another's
    /// identity would be worse than showing no cover.
    pub(crate) fn focus(&mut self, path: Option<&Path>) -> bool {
        if self.local_path.as_deref() == path {
            return false;
        }
        self.local_path = path.map(Path::to_path_buf);
        self.panel = None;
        self.chosen_game_id = None;
        self.verification = None;
        self.cover = CoverState::Idle;
        self.cover_texture = None;
        self.cover_key = None;
        self.cover_cache = None;
        self.needs_reload = false;
        self.dismissed = false;
        true
    }

    /// Whether a resolved panel still answers what is on screen.
    pub(crate) fn accepts_panel(&self, panel: &GameIdentityPanel) -> bool {
        self.local_path.as_deref() == Some(panel.local_path.as_path())
    }

    pub(crate) fn accepts_cover(&self, outcome: &CoverOutcome) -> bool {
        self.local_path.as_deref() == Some(outcome.local_path.as_path())
            && self
                .panel
                .as_ref()
                .and_then(|panel| panel.chosen_candidate())
                .is_some_and(|candidate| candidate.romm_game_id == outcome.romm_game_id)
    }

    pub(crate) fn accepts_verification(&self, outcome: &VerificationOutcomeView) -> bool {
        self.local_path.as_deref() == Some(outcome.local_path.as_path())
    }
}

/// What the panel is asking the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GamePanelRequest {
    /// Read the published cache and rebuild the panel. No network, no hashing.
    Resolve,
    /// Hash this file and compare it with one record. Explicit, always.
    Verify {
        romm_game_id: String,
    },
    /// Fetch or read this record's small cover.
    LoadCover {
        romm_game_id: String,
    },
    /// Choose which claimant to show.
    Choose {
        romm_game_id: String,
    },
    Cancel,
    Close,
    /// Show it again after it was closed.
    Reopen,
}

/// What the renderer needs that is not in the state.
pub(crate) struct GamePanelInputs<'a> {
    /// Whether a RomM operation is running, so actions are disabled honestly.
    pub(crate) busy: bool,
    /// Why actions are unavailable, when they are.
    pub(crate) busy_reason: Option<&'a str>,
    /// Progress from a running verification, if this is what is running.
    pub(crate) hash_progress: Option<&'a HashProgressView>,
    /// Whether an import has been published at all.
    pub(crate) cache_present: bool,
}

/// Draws the panel.
///
/// Every button either does something or says why it cannot. Escape closes, which is
/// what a controller's B button sends.
pub(crate) fn show_game_identity_panel(
    ui: &mut egui::Ui,
    state: &mut GamePanelState,
    inputs: &GamePanelInputs<'_>,
) -> Option<GamePanelRequest> {
    let mut request = None;
    widgets::section_header(
        ui,
        "RomM identity",
        Some("What your RomM catalogue says about the archive selected in the Library."),
    );
    let Some(local_path) = state.local_path.clone() else {
        widgets::card(ui, |ui| {
            ui.label("Select an archive in the Library to see what RomM knows about it.");
        });
        return None;
    };
    if state.dismissed {
        // Closed for this selection. Still stated, rather than vanishing, so it is
        // clear the panel exists and why nothing is shown.
        widgets::card(ui, |ui| {
            ui.label("Closed for this archive.");
            if widgets::action_button(ui, "Show RomM identity", widgets::ActionStyle::Quiet, true)
                .clicked()
            {
                request = Some(GamePanelRequest::Reopen);
            }
        });
        return request;
    }

    widgets::card(ui, |ui| {
        if widgets::path_value(ui, "File", &local_path) {
            ui.ctx().copy_text(local_path.display().to_string());
        }
        if !inputs.cache_present {
            widgets::banner(
                ui,
                "No RomM catalogue yet",
                "Import from RomM on the Sources page first. Until then there is nothing to \
                 compare this file against.",
                widgets::StatusTone::Pending,
            );
            return;
        }
        if state.needs_reload {
            widgets::banner(
                ui,
                "The identity cache changed",
                "An import finished while this was open, so what is shown no longer describes the \
                 current catalogue. Reload to see it.",
                widgets::StatusTone::Warning,
            );
        }

        ui.horizontal_wrapped(|ui| {
            let label = if state.panel.is_some() {
                "Reload identity"
            } else {
                "Look up in RomM"
            };
            if widgets::action_button(ui, label, widgets::ActionStyle::Primary, !inputs.busy)
                .clicked()
            {
                request = Some(GamePanelRequest::Resolve);
            }
            if inputs.busy
                && widgets::action_button(ui, "Stop", widgets::ActionStyle::Quiet, true).clicked()
            {
                request = Some(GamePanelRequest::Cancel);
            }
            if widgets::action_button(ui, "Close", widgets::ActionStyle::Quiet, true).clicked() {
                request = Some(GamePanelRequest::Close);
            }
        });
        if let Some(reason) = inputs.busy_reason {
            ui.label(egui::RichText::new(reason).weak());
        }
        ui.label(
            egui::RichText::new(
                "Reads the published cache and the file's metadata. Nothing is hashed and no \
                 request is made to RomM until you ask for one.",
            )
            .weak(),
        );

        // Cloned so the renderers below can take `state` mutably as well: the panel is
        // a handful of small rows, and a clone per frame is cheaper than threading two
        // borrows through every one of them.
        let Some(panel) = state.panel.as_deref().cloned() else {
            ui.separator();
            ui.label("Not looked up yet.");
            return;
        };

        ui.separator();
        show_verdict(ui, &panel);
        ui.add_space(theme::SECTION_GAP / 2.0);

        if panel.candidates.is_empty() {
            ui.label(&panel.summary);
            return;
        }
        if panel.is_ambiguous()
            && let Some(found) = show_candidate_choice(ui, &panel, state, inputs.busy)
        {
            request = Some(found);
        }

        let Some(candidate) = panel.chosen_candidate().cloned() else {
            return;
        };
        ui.add_space(theme::SECTION_GAP / 2.0);
        show_candidate(ui, &candidate, &panel);

        ui.add_space(theme::SECTION_GAP / 2.0);
        if let Some(found) = show_verification(ui, &panel, &candidate, state, inputs) {
            request = Some(found);
        }

        ui.add_space(theme::SECTION_GAP / 2.0);
        if let Some(found) = show_cover(ui, &candidate, state, inputs) {
            request = Some(found);
        }
    });

    // Escape leaves the panel, which is also what a controller's back button sends.
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        request = Some(GamePanelRequest::Close);
    }
    request
}

fn show_verdict(ui: &mut egui::Ui, panel: &GameIdentityPanel) {
    ui.horizontal_wrapped(|ui| {
        // The badge is a shorthand; the label beside it is the fact, so the verdict
        // is never carried by colour alone.
        widgets::status_badge(
            ui,
            verdict_label(panel.verdict),
            verdict_tone(panel.verdict),
        );
        ui.label(format!("RomM verdict: {}", verdict_label(panel.verdict)));
        widgets::status_badge(
            ui,
            presence_label(panel.presence),
            presence_tone(panel.presence),
        );
    });
    ui.label(&panel.summary);
    ui.label(egui::RichText::new(&panel.verdict_explanation).weak());
    if let Some(detail) = &panel.presence_explanation {
        ui.label(egui::RichText::new(detail).weak());
    }
    for row in panel.local_rows() {
        ui.label(format!("{}: {}", row.label, row.value));
    }
    if panel.manual_platform {
        widgets::banner(
            ui,
            "Your platform assignment stands",
            "You assigned this archive's platform by hand. RomM's record is shown for comparison \
             and never replaces it.",
            widgets::StatusTone::Success,
        );
    }
}

fn show_candidate_choice(
    ui: &mut egui::Ui,
    panel: &GameIdentityPanel,
    state: &GamePanelState,
    busy: bool,
) -> Option<GamePanelRequest> {
    let mut request = None;
    widgets::banner(
        ui,
        "More than one RomM record maps here",
        &format!(
            "{} records translate to this same file. ArchiveFS will not guess which one is right. \
             Choosing one below shows its evidence and changes nothing on disk, in RomM, or in \
             ArchiveFS's own identity.",
            panel.claimants
        ),
        widgets::StatusTone::Warning,
    );
    if panel.claimants_not_listed > 0 {
        ui.label(format!(
            "Showing the {} strongest of {}; {} more are not listed.",
            panel.candidates.len(),
            panel.claimants,
            panel.claimants_not_listed
        ));
    }
    for candidate in &panel.candidates {
        let selected = state.chosen_game_id.as_deref() == Some(candidate.romm_game_id.as_str());
        ui.horizontal_wrapped(|ui| {
            let style = if selected {
                widgets::ActionStyle::Primary
            } else {
                widgets::ActionStyle::Secondary
            };
            if widgets::action_button(
                ui,
                format!(
                    "{} - {} - {}",
                    candidate.title,
                    candidate
                        .canonical_platform
                        .clone()
                        .unwrap_or_else(|| "platform unknown".to_string()),
                    verdict_label(candidate.verdict)
                ),
                style,
                !busy,
            )
            .clicked()
            {
                request = Some(GamePanelRequest::Choose {
                    romm_game_id: candidate.romm_game_id.clone(),
                });
            }
            if selected {
                ui.label("chosen");
            }
        });
    }
    request
}

fn show_candidate(ui: &mut egui::Ui, candidate: &CandidateView, panel: &GameIdentityPanel) {
    ui.label(egui::RichText::new(&candidate.title).strong());
    ui.horizontal_wrapped(|ui| {
        widgets::status_badge(
            ui,
            verdict_label(candidate.verdict),
            verdict_tone(candidate.verdict),
        );
        ui.label(verdict_label(candidate.verdict));
    });
    for row in &candidate.rows {
        ui.label(format!("{}: {}", row.label, row.value));
    }
    ui.label(format!("RomM path: {}", candidate.romm_path));
    if candidate.related_files > 0 {
        ui.label(format!(
            "{} related files in this RomM game.",
            candidate.related_files
        ));
    }
    if candidate.siblings > 0 {
        ui.label(format!(
            "{} sibling games share this platform folder.",
            candidate.siblings
        ));
    }
    if !panel.is_ambiguous() && candidate.hash_compared {
        ui.label("A stored local hash was compared for this record.");
    }

    widgets::technical_details(ui, "romm-game-evidence", |ui| {
        if candidate.evidence.is_empty() {
            ui.label("No evidence lines were recorded.");
        }
        for line in &candidate.evidence {
            ui.label(format!("- {line}"));
        }
        if !candidate.conflicts.is_empty() {
            ui.separator();
            for conflict in &candidate.conflicts {
                ui.label(format!(
                    "{}: RomM says {}, this file says {}. {}",
                    conflict.field, conflict.romm, conflict.local, conflict.detail
                ));
            }
        }
        if !candidate.published_hashes.is_empty() {
            ui.separator();
            for row in &candidate.published_hashes {
                ui.label(format!("{} RomM published: {}", row.label, row.value));
            }
        }
        ui.separator();
        ui.label(format!("Imported from {}.", candidate.provenance));
    });
}

fn show_verification(
    ui: &mut egui::Ui,
    panel: &GameIdentityPanel,
    candidate: &CandidateView,
    state: &GamePanelState,
    inputs: &GamePanelInputs<'_>,
) -> Option<GamePanelRequest> {
    let mut request = None;
    ui.label(egui::RichText::new("Hash verification").strong());
    if let Some(stored) = &panel.stored_hashes {
        ui.label(format!(
            "This file has already been hashed: {} read.",
            human_bytes(stored.bytes_hashed)
        ));
        for row in &stored.rows {
            ui.label(format!("{}: {}", row.label, row.value));
        }
    } else {
        ui.label("This file has not been hashed. Nothing has been read from it.");
    }

    match &panel.verify_blocker {
        Some(reason) => {
            // Shown disabled with the reason rather than hidden, so the action is
            // never a button that quietly does nothing.
            widgets::action_button(
                ui,
                "Verify local file",
                widgets::ActionStyle::Secondary,
                false,
            );
            widgets::banner(
                ui,
                "Cannot verify this file",
                reason,
                widgets::StatusTone::Pending,
            );
        }
        None => {
            if widgets::action_button(
                ui,
                "Verify local file",
                widgets::ActionStyle::Secondary,
                !inputs.busy,
            )
            .clicked()
            {
                request = Some(GamePanelRequest::Verify {
                    romm_game_id: candidate.romm_game_id.clone(),
                });
            }
            ui.label(egui::RichText::new(format!(
                "Reads this one file and computes {VERIFIED_ALGORITHMS}. Nothing else is hashed, \
                 and the file is not modified."
            )).weak());
        }
    }

    if let Some(progress) = inputs.hash_progress {
        ui.label(progress.line());
        if let Some(fraction) = progress.fraction() {
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
        }
        if widgets::action_button(ui, "Stop hashing", widgets::ActionStyle::Quiet, true).clicked() {
            request = Some(GamePanelRequest::Cancel);
        }
    }

    if let Some(outcome) = state.verification.as_deref() {
        ui.separator();
        widgets::banner(
            ui,
            "Verification result",
            &outcome.conclusion(),
            outcome.tone(),
        );
        for line in &outcome.comparisons {
            ui.label(format!(
                "{}: RomM {} / this file {} - {}",
                line.algorithm,
                line.romm,
                line.local,
                if line.agrees { "match" } else { "differ" }
            ));
        }
        ui.label(format!(
            "{} read in {}. Verdict {} before, {} after.",
            human_bytes(outcome.bytes_hashed),
            format_elapsed(outcome.elapsed_seconds),
            verdict_label(outcome.verdict_before),
            verdict_label(outcome.verdict_after),
        ));
        if let Some(path) = &outcome.stored_at {
            ui.label(
                egui::RichText::new(format!(
                    "Recorded for {} in {}. The imported catalogue was not rewritten.",
                    outcome.file_label,
                    path.display()
                ))
                .weak(),
            );
        }
        if outcome.all_agree && !outcome.promoted() {
            ui.label(
                "The hashes agree, but something else in the record still disagrees, so this is \
                 not Confirmed.",
            );
        }
    }
    request
}

fn show_cover(
    ui: &mut egui::Ui,
    candidate: &CandidateView,
    state: &mut GamePanelState,
    inputs: &GamePanelInputs<'_>,
) -> Option<GamePanelRequest> {
    let mut request = None;
    ui.label(egui::RichText::new("Cover").strong());
    ui.horizontal_wrapped(|ui| {
        // Both the availability and the state are named in text; neither is carried
        // by the badge colour alone.
        widgets::status_badge(ui, candidate.artwork.label(), state.cover.tone());
        ui.label(candidate.artwork.label());
    });
    match candidate.artwork {
        ArtworkAvailability::Fetchable => {
            let already = matches!(state.cover, CoverState::Ready(_));
            let label = if already {
                "Reload cover"
            } else {
                "Show cover"
            };
            if widgets::action_button(ui, label, widgets::ActionStyle::Quiet, !inputs.busy)
                .clicked()
            {
                request = Some(GamePanelRequest::LoadCover {
                    romm_game_id: candidate.romm_game_id.clone(),
                });
            }
        }
        availability => {
            widgets::action_button(ui, "Show cover", widgets::ActionStyle::Quiet, false);
            if let Some(detail) = availability.explanation() {
                ui.label(detail);
            }
        }
    }
    ui.label(state.cover.line());

    // Uploading here, on the UI thread, from pixels a worker decoded. The key guards
    // it: a texture is only replaced when the cover actually changed.
    if let CoverState::Ready(cover) = state.cover.clone() {
        if state.cover_key.as_deref() != Some(cover.key.as_str()) {
            state.cover_texture = Some(ui.ctx().load_texture(
                format!("romm-cover-{}", cover.key),
                cover.image.clone(),
                egui::TextureOptions::LINEAR,
            ));
            state.cover_key = Some(cover.key.clone());
        }
        if let Some(texture) = &state.cover_texture {
            let size = egui::vec2(
                cover.width.min(THUMBNAIL_MAX_WIDTH) as f32,
                cover.height.min(THUMBNAIL_MAX_HEIGHT) as f32,
            );
            ui.add(egui::Image::new(texture).fit_to_exact_size(size));
        }
    } else if matches!(state.cover, CoverState::Loading) {
        ui.add(egui::Spinner::new());
    }
    if let Some((items, bytes)) = state.cover_cache {
        ui.label(
            egui::RichText::new(format!(
                "Cover cache holds {items} thumbnail(s), {}.",
                human_bytes(bytes)
            ))
            .weak(),
        );
    }
    request
}

#[cfg(test)]
mod tests;
