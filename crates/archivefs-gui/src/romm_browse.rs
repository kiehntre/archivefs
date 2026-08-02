//! Browsing the published RomM cache: records, conflicts and the stale summary.
//!
//! # Nothing here is unbounded
//!
//! The cache holds 36,259 records on the machine this was written against. A worker
//! loads it, filters it, and hands back at most one page of small GUI-facing rows;
//! the UI thread never sees a provider payload and never holds the catalogue. That
//! is the whole reason [`RecordRowView`] exists rather than passing
//! [`ExternalIdentityRecord`] to the renderer.
//!
//! # Filters compose
//!
//! Every filter narrows what the previous ones left. Setting a platform does not
//! clear a verdict, and clearing a title does not clear a presence filter - which
//! is the behaviour someone expects and the opposite of what a set of independent
//! radio groups would do.
//!
//! # A page belongs to a cache
//!
//! A page result carries the identity of the cache it came from, the filters that
//! produced it, and the offset and limit it covers. If any of those no longer match
//! what the view is asking for, the result is discarded rather than drawn - so an
//! import finishing mid-browse cannot mix records from two catalogues.
//!
//! # Metadata only
//!
//! The presence probe reads `symlink_metadata` and nothing else. No view here opens
//! a file, hashes anything, contacts RomM, or writes.

use std::path::{Path, PathBuf};

use archivefs_core::identity_source::cache::IdentityCache;
use archivefs_core::identity_source::matching::LocalPresence;
use archivefs_core::identity_source::model::{
    ExternalIdentityRecord, ExternalVerification, IdentityImportCounts,
};
use archivefs_core::identity_source::stale::{StaleGroup, StaleSummary};
use eframe::egui;

use crate::romm_game::{ArtworkAvailability, CoverOutcome, CoverState, fitted_cover_size};
use crate::romm_source::{CardRow, human_bytes};
use crate::ui::components as widgets;

/// How many records a page shows by default. Small enough to read on a
/// television, large enough that paging is not tedious.
pub(crate) const DEFAULT_PAGE_SIZE: usize = 25;
/// The most a page may ever hold, whatever is asked for.
pub(crate) const MAX_PAGE_SIZE: usize = 100;
/// The most conflicts one page shows.
pub(crate) const CONFLICT_PAGE_SIZE: usize = 20;
/// The longest title filter accepted. Longer than any real title, and bounded so a
/// pasted file cannot become a filter.
pub(crate) const MAX_TITLE_FILTER: usize = 200;

/// Which browsing view is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowseView {
    Records,
    Conflicts,
    StaleSummary,
}

impl BrowseView {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Records => "RomM records",
            Self::Conflicts => "Identity conflicts",
            Self::StaleSummary => "Stale records",
        }
    }
}

/// Which local presence a row must have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresenceFilter {
    Any,
    RegularFile,
    Directory,
    DanglingSymlink,
    Missing,
    MissingParent,
    /// Anything that is neither a file nor one of the named cases - a device, a
    /// socket, a path that could not be examined.
    Other,
}

impl PresenceFilter {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::RegularFile => "present regular file",
            Self::Directory => "present directory",
            Self::DanglingSymlink => "dangling symlink",
            Self::Missing => "missing path",
            Self::MissingParent => "missing parent folder",
            Self::Other => "refused or unsafe path",
        }
    }

    /// Whether this filter needs a metadata probe. `Any` does not, which is what
    /// keeps the common case free of 36,259 stat calls.
    pub(crate) fn needs_probe(self) -> bool {
        !matches!(self, Self::Any)
    }

    fn accepts(self, presence: LocalPresence) -> bool {
        match self {
            Self::Any => true,
            Self::RegularFile => presence == LocalPresence::File,
            Self::Directory => presence == LocalPresence::Directory,
            Self::DanglingSymlink => presence == LocalPresence::DanglingSymlink,
            Self::Missing => presence == LocalPresence::Absent,
            Self::MissingParent => presence == LocalPresence::ParentAbsent,
            Self::Other => presence == LocalPresence::Other,
        }
    }

    pub(crate) const ALL: [Self; 7] = [
        Self::Any,
        Self::RegularFile,
        Self::Directory,
        Self::DanglingSymlink,
        Self::Missing,
        Self::MissingParent,
        Self::Other,
    ];
}

/// What the records browser is asking for.
///
/// `Eq` matters: a page result is only drawn if the filters it was produced under
/// still equal the ones the view holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RecordFilters {
    /// `None` means every verdict.
    pub(crate) verdict: Option<ExternalVerification>,
    /// The canonical platform, taken from the registry rather than typed freely.
    pub(crate) canonical_platform: Option<String>,
    /// RomM's own platform name, as it appears in the cache.
    pub(crate) romm_platform: Option<String>,
    /// A case-insensitive substring of the title. Never compiled as a pattern.
    pub(crate) title: String,
    pub(crate) region: Option<String>,
    pub(crate) multi_file_only: bool,
    pub(crate) unknown_platform_only: bool,
    pub(crate) file_detail_omitted_only: bool,
    pub(crate) has_artwork_only: bool,
    pub(crate) presence: Option<PresenceFilter>,
}

impl RecordFilters {
    /// Whether any filter is active, for a "showing everything" note.
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether satisfying these filters needs a metadata probe per record.
    pub(crate) fn needs_presence_probe(&self) -> bool {
        self.presence.is_some_and(PresenceFilter::needs_probe)
    }

    /// The cheap half: everything decidable from the cached record alone.
    ///
    /// Split out so the expensive half only ever runs on records that already
    /// passed - probing a path is a syscall, and doing it for records a verdict
    /// filter has already excluded would be waste.
    fn accepts_without_probe(&self, record: &ExternalIdentityRecord) -> bool {
        if let Some(wanted) = self.verdict
            && record.verification != wanted
        {
            return false;
        }
        if let Some(platform) = &self.canonical_platform
            && record.platform_candidate.as_deref() != Some(platform.as_str())
        {
            return false;
        }
        if let Some(platform) = &self.romm_platform
            && record.provider_platform_name.as_deref() != Some(platform.as_str())
        {
            return false;
        }
        if !self.title.trim().is_empty() {
            // A plain case-insensitive substring test. Deliberately not a pattern:
            // raw input must never become something with its own execution cost.
            let needle = self.title.trim().to_lowercase();
            let matched = record
                .title
                .as_deref()
                .is_some_and(|title| title.to_lowercase().contains(&needle));
            if !matched {
                return false;
            }
        }
        if let Some(region) = &self.region
            && !record.regions.iter().any(|value| value == region)
        {
            return false;
        }
        if self.multi_file_only && record.related_files.len() < 2 {
            return false;
        }
        if self.unknown_platform_only && record.platform_candidate.is_some() {
            return false;
        }
        if self.file_detail_omitted_only && !file_detail_omitted(record) {
            return false;
        }
        if self.has_artwork_only && record.artwork.is_none() {
            return false;
        }
        true
    }
}

/// Whether this record's per-file list was left out because it was too large.
fn file_detail_omitted(record: &ExternalIdentityRecord) -> bool {
    record
        .evidence
        .iter()
        .any(|line| line.contains("per-file detail was not imported"))
}

/// Identifies the cache a page came from.
///
/// Not a hash of the whole thing: the server, the import time and the record count
/// together change whenever a new cache is published, which is all a staleness check
/// needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CacheIdentity {
    pub(crate) server_id: String,
    pub(crate) imported_at_unix_seconds: i64,
    pub(crate) records: usize,
    pub(crate) format_version: u32,
}

impl CacheIdentity {
    pub(crate) fn of(cache: &IdentityCache) -> Self {
        Self {
            server_id: cache.server_id.clone(),
            imported_at_unix_seconds: cache.imported_at_unix_seconds,
            records: cache.records.len(),
            format_version: cache.format_version,
        }
    }
}

/// One record, reduced to what a row draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordRowView {
    pub(crate) romm_game_id: String,
    pub(crate) romm_platform_id: Option<String>,
    pub(crate) title: String,
    pub(crate) canonical_platform: Option<String>,
    pub(crate) romm_platform: Option<String>,
    pub(crate) verdict: ExternalVerification,
    pub(crate) romm_path: String,
    pub(crate) archivefs_path: Option<PathBuf>,
    /// `None` when no presence filter made a probe worthwhile.
    pub(crate) presence: Option<LocalPresence>,
    pub(crate) regions: Vec<String>,
    pub(crate) revision: Option<String>,
    pub(crate) file_size_bytes: Option<u64>,
    pub(crate) published_hashes: Vec<String>,
    /// Whether a local hash comparison actually happened and agreed.
    pub(crate) hash_verified: bool,
    pub(crate) related_files: usize,
    pub(crate) siblings: usize,
    pub(crate) stale_reason: Option<String>,
    pub(crate) file_detail_omitted: bool,
    pub(crate) has_artwork: bool,
    pub(crate) imported_at_unix_seconds: i64,
    pub(crate) romm_updated_at: Option<String>,
    /// Which instance this came from.
    pub(crate) provenance: String,
}

impl RecordRowView {
    fn of(record: &ExternalIdentityRecord, presence: Option<LocalPresence>) -> Self {
        Self {
            romm_game_id: record.provider_game_id.clone(),
            romm_platform_id: record.provider_platform_id.clone(),
            title: record
                .title
                .clone()
                .unwrap_or_else(|| "(untitled)".to_string()),
            canonical_platform: record.platform_candidate.clone(),
            romm_platform: record.provider_platform_name.clone(),
            verdict: record.verification,
            romm_path: record.provider_path.clone(),
            archivefs_path: record.archivefs_path.clone(),
            presence,
            regions: record.regions.clone(),
            revision: record.revision.clone(),
            file_size_bytes: record.file_size_bytes,
            published_hashes: record
                .hashes
                .iter()
                .map(|hash| hash.algorithm.label().to_string())
                .collect(),
            hash_verified: record.verification == ExternalVerification::ConfirmedExternal,
            related_files: record.related_files.len(),
            siblings: record.sibling_game_ids.len(),
            stale_reason: (record.verification == ExternalVerification::Stale)
                .then(|| stale_reason_of(record))
                .flatten(),
            file_detail_omitted: file_detail_omitted(record),
            has_artwork: record.artwork.is_some(),
            imported_at_unix_seconds: record.imported_at_unix_seconds,
            romm_updated_at: record.provider_updated_at.clone(),
            provenance: record.server_id.clone(),
        }
    }
}

/// The evidence line that explains a stale record, if there is one.
fn stale_reason_of(record: &ExternalIdentityRecord) -> Option<String> {
    record
        .evidence
        .iter()
        .find(|line| {
            line.contains("does not exist")
                || line.contains("is a directory")
                || line.contains("symlink")
                || line.contains("neither does the folder")
                || line.contains("not a regular file")
        })
        .cloned()
}

/// One record's full evidence, for the detail panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordDetailView {
    pub(crate) row: RecordRowView,
    pub(crate) rows: Vec<CardRow>,
    /// Every evidence line the importer and matcher recorded.
    pub(crate) evidence: Vec<String>,
    pub(crate) conflicts: Vec<ConflictLineView>,
    pub(crate) related_files: Vec<String>,
    pub(crate) sibling_game_ids: Vec<String>,
    pub(crate) metadata_ids: Vec<CardRow>,
    /// Typed artwork facts. Public references are provenance only and their raw URL
    /// never enters this GUI model.
    pub(crate) artwork: ArtworkAvailability,
    pub(crate) has_romm_thumbnail: bool,
    pub(crate) has_public_artwork_reference: bool,
    /// What this verdict means, in the project's own words.
    pub(crate) verdict_explanation: String,
    /// Why a directory stays stale, when that is the case.
    pub(crate) presence_explanation: Option<String>,
}

/// One conflict line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConflictLineView {
    pub(crate) field: String,
    pub(crate) romm: String,
    pub(crate) local: String,
    pub(crate) detail: String,
}

/// One page of records, tied to what produced it.
#[derive(Clone, Debug)]
pub(crate) struct RecordPageView {
    pub(crate) cache: CacheIdentity,
    pub(crate) filters: RecordFilters,
    pub(crate) offset: usize,
    pub(crate) limit: usize,
    /// How many records the filters matched, across the whole cache.
    pub(crate) matching: usize,
    pub(crate) total_in_cache: usize,
    pub(crate) rows: Vec<RecordRowView>,
    pub(crate) counts: IdentityImportCounts,
    /// Canonical platforms present in the cache, for the filter control.
    pub(crate) canonical_platforms: Vec<String>,
    pub(crate) romm_platforms: Vec<String>,
    pub(crate) regions: Vec<String>,
}

impl RecordPageView {
    pub(crate) fn page_number(&self) -> usize {
        self.offset
            .checked_div(self.limit)
            .map_or(1, |page| page + 1)
    }

    pub(crate) fn page_count(&self) -> usize {
        if self.limit == 0 {
            return 1;
        }
        self.matching.div_ceil(self.limit).max(1)
    }

    pub(crate) fn has_next(&self) -> bool {
        self.offset + self.rows.len() < self.matching
    }

    pub(crate) fn has_previous(&self) -> bool {
        self.offset > 0
    }
}

/// Builds one page from a cache.
///
/// Runs in a worker. `presence_for` is supplied so the probe is the caller's - and
/// so a test can decide every presence without a filesystem.
pub(crate) fn build_record_page(
    cache: &IdentityCache,
    filters: &RecordFilters,
    offset: usize,
    limit: usize,
    presence_for: &dyn Fn(&Path) -> LocalPresence,
) -> RecordPageView {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let probe = filters.needs_presence_probe();

    // One pass, counting matches and collecting only the page's worth of rows. The
    // catalogue is never copied.
    let mut matching = 0usize;
    let mut rows: Vec<RecordRowView> = Vec::with_capacity(limit);
    for record in &cache.records {
        if !filters.accepts_without_probe(record) {
            continue;
        }
        // The expensive half, only for records that already passed.
        let presence = probe.then(|| match record.archivefs_path.as_deref() {
            Some(path) => presence_for(path),
            // A record with no mapping has nothing to probe, and "absent" is the
            // honest answer for a path that does not exist to be looked at.
            None => LocalPresence::Absent,
        });
        if let Some(wanted) = filters.presence
            && let Some(seen) = presence
            && !wanted.accepts(seen)
        {
            continue;
        }
        if matching >= offset && rows.len() < limit {
            rows.push(RecordRowView::of(record, presence));
        }
        matching += 1;
    }

    // An offset past the end yields an empty page rather than an error, and the
    // caller can see it is past the end from `matching`.
    RecordPageView {
        cache: CacheIdentity::of(cache),
        filters: filters.clone(),
        offset,
        limit,
        matching,
        total_in_cache: cache.records.len(),
        rows,
        counts: cache.counts(),
        canonical_platforms: distinct(
            cache
                .records
                .iter()
                .filter_map(|record| record.platform_candidate.clone()),
        ),
        romm_platforms: distinct(
            cache
                .records
                .iter()
                .filter_map(|record| record.provider_platform_name.clone()),
        ),
        regions: distinct(
            cache
                .records
                .iter()
                .flat_map(|record| record.regions.iter().cloned()),
        ),
    }
}

/// Sorted, de-duplicated, and bounded so a pathological cache cannot produce a
/// filter control with thousands of entries.
fn distinct(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut all: Vec<String> = values.collect();
    all.sort();
    all.dedup();
    all.truncate(512);
    all
}

/// The detail for one record, found by its RomM id.
pub(crate) fn build_record_detail(
    cache: &IdentityCache,
    romm_game_id: &str,
    presence_for: &dyn Fn(&Path) -> LocalPresence,
) -> Option<RecordDetailView> {
    let record = cache
        .records
        .iter()
        .find(|record| record.provider_game_id == romm_game_id)?;
    // The detail panel always probes: it is one path, and the presence is the most
    // useful thing on it.
    let presence = record.archivefs_path.as_deref().map(presence_for);
    let row = RecordRowView::of(record, presence);

    let mut rows = vec![
        CardRow {
            label: "RomM id".to_string(),
            value: record.provider_game_id.clone(),
        },
        CardRow {
            label: "RomM platform id".to_string(),
            value: record
                .provider_platform_id
                .clone()
                .unwrap_or_else(|| "-".to_string()),
        },
        CardRow {
            label: "RomM path".to_string(),
            value: record.provider_path.clone(),
        },
        CardRow {
            label: "Local path".to_string(),
            value: record
                .archivefs_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "no mapping covers this record".to_string()),
        },
        CardRow {
            label: "Local presence".to_string(),
            value: presence
                .map(presence_label)
                .unwrap_or("not applicable")
                .to_string(),
        },
        CardRow {
            label: "Verdict".to_string(),
            value: verdict_label(record.verification).to_string(),
        },
        CardRow {
            label: "File size".to_string(),
            value: record
                .file_size_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "not published".to_string()),
        },
        CardRow {
            label: "Regions".to_string(),
            value: if record.regions.is_empty() {
                "-".to_string()
            } else {
                record.regions.join(", ")
            },
        },
        CardRow {
            label: "Revision".to_string(),
            value: record.revision.clone().unwrap_or_else(|| "-".to_string()),
        },
        CardRow {
            label: "Imported".to_string(),
            value: format!("unix {}", record.imported_at_unix_seconds),
        },
        CardRow {
            label: "RomM updated".to_string(),
            value: record
                .provider_updated_at
                .clone()
                .unwrap_or_else(|| "-".to_string()),
        },
        CardRow {
            label: "Provenance".to_string(),
            value: format!("imported from {}", record.server_id),
        },
    ];
    for hash in &record.hashes {
        rows.push(CardRow {
            label: format!("Published {}", hash.algorithm.label()),
            value: hash.value.clone(),
        });
    }
    if record.hashes.is_empty() {
        rows.push(CardRow {
            label: "Published hashes".to_string(),
            value: "none - RomM published no hash for this record".to_string(),
        });
    }

    Some(RecordDetailView {
        rows,
        evidence: record.evidence.clone(),
        conflicts: record
            .conflicts
            .iter()
            .map(|conflict| ConflictLineView {
                field: conflict.field.label().to_string(),
                romm: conflict.external.clone(),
                local: conflict.local.clone(),
                detail: conflict.detail.clone(),
            })
            .collect(),
        related_files: record.related_files.clone(),
        sibling_game_ids: record.sibling_game_ids.clone(),
        metadata_ids: record
            .metadata_provider_ids
            .iter()
            .map(|entry| CardRow {
                label: entry.provider.clone(),
                value: entry.id.clone(),
            })
            .collect(),
        artwork: crate::romm_game::availability_of(record),
        has_romm_thumbnail: record
            .artwork
            .as_ref()
            .is_some_and(|artwork| artwork.small_reference.is_some()),
        has_public_artwork_reference: record
            .artwork
            .as_ref()
            .is_some_and(|artwork| !artwork.reference.trim().is_empty()),
        verdict_explanation: verdict_explanation(record.verification).to_string(),
        presence_explanation: presence.and_then(presence_explanation),
        row,
    })
}

/// The project's own wording for a verdict. Not softened, and in particular Strong
/// is never presented as Confirmed.
pub(crate) fn verdict_label(verdict: ExternalVerification) -> &'static str {
    match verdict {
        ExternalVerification::ConfirmedExternal => "Confirmed",
        ExternalVerification::StrongExternal => "Strong",
        ExternalVerification::ProbableExternal => "Probable",
        ExternalVerification::Ambiguous => "Ambiguous",
        ExternalVerification::Stale => "Stale",
        ExternalVerification::Unmatched => "Unmatched",
    }
}

pub(crate) fn verdict_explanation(verdict: ExternalVerification) -> &'static str {
    match verdict {
        ExternalVerification::ConfirmedExternal => {
            "A local hash comparison actually happened and agreed with what RomM published."
        }
        ExternalVerification::StrongExternal => {
            "RomM supplied strong identity and hash metadata, but this local file has not been \
             explicitly verified. Nothing has been hashed on your machine for this record."
        }
        ExternalVerification::ProbableExternal => {
            "Path, title and platform evidence support the match, without verified hashes."
        }
        ExternalVerification::Ambiguous => {
            "Conflicting or duplicate evidence prevents a unique result, so both sides are kept \
             and neither is chosen."
        }
        ExternalVerification::Stale => {
            "The provider record maps to a path that cannot currently be treated as a comparable \
             regular file."
        }
        ExternalVerification::Unmatched => "No safe local match was established.",
    }
}

pub(crate) fn verdict_tone(verdict: ExternalVerification) -> widgets::StatusTone {
    match verdict {
        ExternalVerification::ConfirmedExternal => widgets::StatusTone::Success,
        ExternalVerification::StrongExternal => widgets::StatusTone::Info,
        ExternalVerification::ProbableExternal => widgets::StatusTone::Pending,
        ExternalVerification::Ambiguous => widgets::StatusTone::Warning,
        ExternalVerification::Stale => widgets::StatusTone::Warning,
        ExternalVerification::Unmatched => widgets::StatusTone::Pending,
    }
}

/// Every verdict, for the filter control.
pub(crate) const ALL_VERDICTS: [ExternalVerification; 6] = [
    ExternalVerification::ConfirmedExternal,
    ExternalVerification::StrongExternal,
    ExternalVerification::ProbableExternal,
    ExternalVerification::Ambiguous,
    ExternalVerification::Stale,
    ExternalVerification::Unmatched,
];

/// What is at a path, in words. A present directory is never called nonexistent.
pub(crate) fn presence_label(presence: LocalPresence) -> &'static str {
    match presence {
        LocalPresence::File => "Present regular file",
        LocalPresence::Directory => "Present directory",
        LocalPresence::DanglingSymlink => "Dangling symlink",
        LocalPresence::Absent => "Missing path",
        LocalPresence::ParentAbsent => "Missing parent folder",
        LocalPresence::Other => "Refused or unsafe path",
    }
}

pub(crate) fn presence_tone(presence: LocalPresence) -> widgets::StatusTone {
    match presence {
        LocalPresence::File => widgets::StatusTone::Success,
        LocalPresence::Directory => widgets::StatusTone::Info,
        LocalPresence::DanglingSymlink | LocalPresence::Absent | LocalPresence::ParentAbsent => {
            widgets::StatusTone::Warning
        }
        LocalPresence::Other => widgets::StatusTone::Blocked,
    }
}

/// Why this presence produces the verdict it does, where that is not obvious.
pub(crate) fn presence_explanation(presence: LocalPresence) -> Option<String> {
    match presence {
        LocalPresence::Directory => Some(
            "The game is present as a folder, so it is not missing. It stays Stale because a \
             directory cannot be compared against a single published file size or hash - there is \
             no one file to measure."
                .to_string(),
        ),
        LocalPresence::DanglingSymlink => Some(
            "The link is still there but its target is gone, so the file it stood for no longer \
             exists."
                .to_string(),
        ),
        LocalPresence::ParentAbsent => Some(
            "Neither the file nor the folder that would hold it is present - often a collection \
             that is not on this machine at all."
                .to_string(),
        ),
        _ => None,
    }
}

/// One conflicting record, for the conflicts view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConflictRowView {
    pub(crate) romm_game_id: String,
    pub(crate) title: String,
    pub(crate) verdict: ExternalVerification,
    pub(crate) romm_path: String,
    pub(crate) archivefs_path: Option<PathBuf>,
    pub(crate) canonical_platform: Option<String>,
    pub(crate) romm_platform: Option<String>,
    pub(crate) conflicts: Vec<ConflictLineView>,
    /// Every evidence line, so nothing is dropped from display.
    pub(crate) evidence: Vec<String>,
    /// Set when ArchiveFS's own identity was kept in preference to RomM's.
    pub(crate) local_evidence_retained: Option<String>,
    pub(crate) competing_records: Vec<String>,
    pub(crate) provenance: String,
}

/// One page of conflicts.
#[derive(Clone, Debug)]
pub(crate) struct ConflictPageView {
    pub(crate) cache: CacheIdentity,
    pub(crate) offset: usize,
    pub(crate) limit: usize,
    pub(crate) matching: usize,
    pub(crate) total_in_cache: usize,
    pub(crate) rows: Vec<ConflictRowView>,
}

impl ConflictPageView {
    pub(crate) fn is_empty(&self) -> bool {
        self.matching == 0
    }

    pub(crate) fn has_next(&self) -> bool {
        self.offset + self.rows.len() < self.matching
    }

    pub(crate) fn has_previous(&self) -> bool {
        self.offset > 0
    }
}

/// Builds one page of conflicts. Deterministic: the cache is already sorted, and
/// this preserves that order.
pub(crate) fn build_conflict_page(
    cache: &IdentityCache,
    offset: usize,
    limit: usize,
) -> ConflictPageView {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let all = cache.conflicts();
    let start = offset.min(all.len());
    let end = start.saturating_add(limit).min(all.len());
    let rows = all[start..end]
        .iter()
        .map(|record| ConflictRowView {
            romm_game_id: record.provider_game_id.clone(),
            title: record
                .title
                .clone()
                .unwrap_or_else(|| "(untitled)".to_string()),
            verdict: record.verification,
            romm_path: record.provider_path.clone(),
            archivefs_path: record.archivefs_path.clone(),
            canonical_platform: record.platform_candidate.clone(),
            romm_platform: record.provider_platform_name.clone(),
            conflicts: record
                .conflicts
                .iter()
                .map(|conflict| ConflictLineView {
                    field: conflict.field.label().to_string(),
                    romm: conflict.external.clone(),
                    local: conflict.local.clone(),
                    detail: conflict.detail.clone(),
                })
                .collect(),
            evidence: record.evidence.clone(),
            // The matcher records this when a locally verified identity outranked
            // what the provider claimed.
            local_evidence_retained: record
                .evidence
                .iter()
                .find(|line| line.contains("not displaced") || line.contains("stronger"))
                .cloned(),
            competing_records: record.sibling_game_ids.clone(),
            provenance: record.server_id.clone(),
        })
        .collect();
    ConflictPageView {
        cache: CacheIdentity::of(cache),
        offset,
        limit,
        matching: all.len(),
        total_in_cache: cache.records.len(),
        rows,
    }
}

/// The stale summary, with the cache it describes.
#[derive(Clone, Debug)]
pub(crate) struct StaleSummaryView {
    pub(crate) cache: CacheIdentity,
    pub(crate) summary: StaleSummary,
}

impl StaleSummaryView {
    /// The conclusion, shown only when the counted evidence supports it.
    pub(crate) fn interpretation(&self) -> Option<&'static str> {
        self.summary.looks_like_library_drift.then_some(
            "Most stale records represent ordinary library drift or broken links, not a \
             path-mapping failure.",
        )
    }

    /// A group's share of the population.
    pub(crate) fn share(&self, count: usize) -> String {
        if self.summary.stale == 0 {
            "0%".to_string()
        } else {
            format!("{:.1}%", count as f64 * 100.0 / self.summary.stale as f64)
        }
    }
}

/// Progress while probing paths, so a long summary is not a frozen window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StaleProgress {
    pub(crate) probed: usize,
    pub(crate) total: usize,
}

impl StaleProgress {
    pub(crate) fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| self.probed as f32 / self.total as f32)
    }
}

/// What a browsing view wants the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BrowseRequest {
    /// Load a page with these filters. The view holds the filters; this carries the
    /// offset it wants.
    LoadRecords {
        offset: usize,
        limit: usize,
    },
    OpenDetail {
        romm_game_id: String,
    },
    /// Load the visible detail record's RomM-hosted thumbnail.
    LoadDetailCover {
        local_path: PathBuf,
        romm_game_id: String,
    },
    CloseDetail,
    LoadConflicts {
        offset: usize,
    },
    RunStaleSummary,
    Cancel,
    Switch(BrowseView),
    Close,
}

/// What the browsing panel remembers between frames.
#[derive(Clone)]
pub(crate) struct BrowseState {
    pub(crate) view: BrowseView,
    pub(crate) filters: RecordFilters,
    pub(crate) page_size: usize,
    /// The last page that arrived and still matches what is being asked for.
    pub(crate) page: Option<Box<RecordPageView>>,
    pub(crate) detail: Option<Box<RecordDetailView>>,
    /// The exact row whose detail worker is allowed to land. This is separate from
    /// `detail` so changing filters or switching views can invalidate an in-flight
    /// result before it arrives.
    pub(crate) pending_detail_id: Option<String>,
    pub(crate) detail_problem: Option<String>,
    pub(crate) detail_cover: CoverState,
    pub(crate) detail_cover_texture: Option<egui::TextureHandle>,
    pub(crate) detail_cover_key: Option<String>,
    pub(crate) conflicts: Option<Box<ConflictPageView>>,
    pub(crate) stale: Option<Box<StaleSummaryView>>,
    /// Set when a result was discarded because the cache moved on.
    pub(crate) needs_reload: bool,
    pub(crate) title_input: String,
}

impl Default for BrowseState {
    fn default() -> Self {
        Self {
            view: BrowseView::Records,
            filters: RecordFilters::default(),
            page_size: DEFAULT_PAGE_SIZE,
            page: None,
            detail: None,
            pending_detail_id: None,
            detail_problem: None,
            detail_cover: CoverState::Idle,
            detail_cover_texture: None,
            detail_cover_key: None,
            conflicts: None,
            stale: None,
            needs_reload: false,
            title_input: String::new(),
        }
    }
}

impl BrowseState {
    pub(crate) fn opened_at(view: BrowseView) -> Self {
        Self {
            view,
            ..Self::default()
        }
    }

    /// Whether a page result is the one currently wanted.
    ///
    /// Everything that could make it wrong is compared: the cache it came from, the
    /// filters that produced it, and the page geometry. Anything else is a result
    /// from a superseded request.
    pub(crate) fn accepts_page(&self, page: &RecordPageView, cache: &CacheIdentity) -> bool {
        page.cache == *cache && page.filters == self.filters && page.limit == self.page_size
    }

    /// The same check for a conflict page. A conflicts view has no filters, but it
    /// is just as wrong to draw one cache's conflicts while another is published.
    pub(crate) fn accepts_conflicts(&self, page: &ConflictPageView, cache: &CacheIdentity) -> bool {
        page.cache == *cache
    }

    /// And for a stale summary, which is the most expensive result to produce and so
    /// the one most likely to be outlived by the cache it describes.
    pub(crate) fn accepts_stale(&self, view: &StaleSummaryView, cache: &CacheIdentity) -> bool {
        view.cache == *cache
    }

    pub(crate) fn begin_detail(&mut self, romm_game_id: String) {
        self.detail = None;
        self.detail_problem = None;
        self.detail_cover = CoverState::Idle;
        self.detail_cover_texture = None;
        self.detail_cover_key = None;
        self.pending_detail_id = Some(romm_game_id);
    }

    pub(crate) fn invalidate_detail_request(&mut self) {
        self.pending_detail_id = None;
    }

    pub(crate) fn accepts_detail(
        &self,
        requested_id: &str,
        detail: Option<&RecordDetailView>,
    ) -> bool {
        self.view == BrowseView::Records
            && self.pending_detail_id.as_deref() == Some(requested_id)
            && detail.is_none_or(|view| view.row.romm_game_id == requested_id)
    }

    pub(crate) fn accepts_cover(&self, outcome: &CoverOutcome) -> bool {
        self.detail.as_ref().is_some_and(|detail| {
            detail.row.romm_game_id == outcome.romm_game_id
                && detail.row.archivefs_path.as_deref() == Some(outcome.local_path.as_path())
        })
    }
}

/// Draws whichever browsing view is open.
pub(crate) fn show_browse_panel(
    ui: &mut egui::Ui,
    state: &mut BrowseState,
    busy: bool,
    stale_progress: Option<&StaleProgress>,
) -> Option<BrowseRequest> {
    let mut request = None;
    widgets::section_header(
        ui,
        state.view.title(),
        Some("Reads the published RomM cache. No request is made to RomM."),
    );
    widgets::card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for view in [
                BrowseView::Records,
                BrowseView::Conflicts,
                BrowseView::StaleSummary,
            ] {
                let selected = state.view == view;
                let style = if selected {
                    widgets::ActionStyle::Primary
                } else {
                    widgets::ActionStyle::Secondary
                };
                if widgets::action_button(ui, view.title(), style, !selected && !busy).clicked() {
                    request = Some(BrowseRequest::Switch(view));
                }
            }
            if widgets::action_button(ui, "Close", widgets::ActionStyle::Quiet, true).clicked() {
                request = Some(BrowseRequest::Close);
            }
        });
        if state.needs_reload {
            widgets::banner(
                ui,
                "The identity cache changed",
                "An import finished while this was open, so what was on screen no longer \
                 describes the current catalogue. Reload to see it.",
                widgets::StatusTone::Warning,
            );
        }
        if let Some(problem) = &state.detail_problem {
            widgets::banner(
                ui,
                "Record details unavailable",
                problem,
                widgets::StatusTone::Warning,
            );
        }
        ui.separator();

        match state.view {
            BrowseView::Records => {
                if let Some(found) = show_records(ui, state, busy) {
                    request = Some(found);
                }
            }
            BrowseView::Conflicts => {
                if let Some(found) = show_conflicts(ui, state, busy) {
                    request = Some(found);
                }
            }
            BrowseView::StaleSummary => {
                if let Some(found) = show_stale_summary(ui, state, busy, stale_progress) {
                    request = Some(found);
                }
            }
        }
    });
    if let Some(detail) = state.detail.as_deref().cloned() {
        let context = ui.ctx().clone();
        let viewport = context.input(|input| input.screen_rect().size());
        let (initial, maximum) = detail_window_sizes(viewport);
        let mut open = true;
        let mut close_clicked = false;
        egui::Window::new("RomM record details")
            .id(egui::Id::new("romm_record_detail_dialog"))
            .collapsible(false)
            .resizable(true)
            .open(&mut open)
            .default_size(initial)
            .max_size(maximum)
            .show(&context, |ui| {
                let footer_height = 44.0;
                let body_height = detail_body_height(ui.available_height(), footer_height);
                egui::ScrollArea::vertical()
                    .id_salt("romm_record_detail_body")
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .max_height(body_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        show_record_detail(
                            ui,
                            &detail,
                            &state.detail_cover,
                            &mut state.detail_cover_texture,
                            &mut state.detail_cover_key,
                        )
                    });
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), footer_height - 4.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        close_clicked = widgets::action_button(
                            ui,
                            "Close",
                            widgets::ActionStyle::Primary,
                            true,
                        )
                        .clicked();
                    },
                );
            });
        if !open || close_clicked || context.input(|input| input.key_pressed(egui::Key::Escape)) {
            request = Some(BrowseRequest::CloseDetail);
        } else if detail.artwork == ArtworkAvailability::Fetchable
            && matches!(state.detail_cover, CoverState::Idle)
            && !busy
            && request.is_none()
        {
            if let Some(local_path) = detail.row.archivefs_path.clone() {
                request = Some(BrowseRequest::LoadDetailCover {
                    local_path,
                    romm_game_id: detail.row.romm_game_id.clone(),
                });
            } else {
                state.detail_cover = CoverState::Refused(
                    "this cached record has no validated local path to bind the request to"
                        .to_string(),
                );
            }
        }
    }
    request
}

fn detail_window_sizes(viewport: egui::Vec2) -> (egui::Vec2, egui::Vec2) {
    let maximum = egui::vec2(
        (viewport.x - 32.0).max(240.0).min(viewport.x.max(1.0)),
        (viewport.y - 32.0).max(240.0).min(viewport.y.max(1.0)),
    );
    let initial = egui::vec2(680.0_f32.min(maximum.x), 720.0_f32.min(maximum.y));
    (initial, maximum)
}

fn detail_body_height(available_height: f32, footer_height: f32) -> f32 {
    (available_height - footer_height).max(96.0)
}

fn show_records(ui: &mut egui::Ui, state: &mut BrowseState, busy: bool) -> Option<BrowseRequest> {
    let mut request = None;
    let platforms = state
        .page
        .as_ref()
        .map(|page| page.canonical_platforms.clone())
        .unwrap_or_default();
    let romm_platforms = state
        .page
        .as_ref()
        .map(|page| page.romm_platforms.clone())
        .unwrap_or_default();
    let regions = state
        .page
        .as_ref()
        .map(|page| page.regions.clone())
        .unwrap_or_default();

    let mut filters_changed = false;

    // --- Filters ---------------------------------------------------------
    ui.horizontal_wrapped(|ui| {
        ui.label("Verdict");
        let before = state.filters.verdict;
        egui::ComboBox::from_id_salt("romm-verdict")
            .selected_text(
                state
                    .filters
                    .verdict
                    .map(verdict_label)
                    .unwrap_or("any")
                    .to_string(),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.filters.verdict, None, "any");
                for verdict in ALL_VERDICTS {
                    ui.selectable_value(
                        &mut state.filters.verdict,
                        Some(verdict),
                        verdict_label(verdict),
                    );
                }
            });
        filters_changed |= before != state.filters.verdict;

        ui.label("Presence");
        let before = state.filters.presence;
        egui::ComboBox::from_id_salt("romm-presence")
            .selected_text(
                state
                    .filters
                    .presence
                    .unwrap_or(PresenceFilter::Any)
                    .label()
                    .to_string(),
            )
            .show_ui(ui, |ui| {
                for presence in PresenceFilter::ALL {
                    let value = (presence != PresenceFilter::Any).then_some(presence);
                    ui.selectable_value(&mut state.filters.presence, value, presence.label());
                }
            });
        filters_changed |= before != state.filters.presence;
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("Platform");
        let before = state.filters.canonical_platform.clone();
        egui::ComboBox::from_id_salt("romm-platform")
            .selected_text(
                state
                    .filters
                    .canonical_platform
                    .clone()
                    .unwrap_or_else(|| "any".to_string()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.filters.canonical_platform, None, "any");
                // Straight from the cache's canonical values, which come from the
                // registry - never a substring guess.
                for platform in &platforms {
                    ui.selectable_value(
                        &mut state.filters.canonical_platform,
                        Some(platform.clone()),
                        platform,
                    );
                }
            });
        filters_changed |= before != state.filters.canonical_platform;

        ui.label("RomM platform");
        let before = state.filters.romm_platform.clone();
        egui::ComboBox::from_id_salt("romm-provider-platform")
            .selected_text(
                state
                    .filters
                    .romm_platform
                    .clone()
                    .unwrap_or_else(|| "any".to_string()),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.filters.romm_platform, None, "any");
                for platform in &romm_platforms {
                    ui.selectable_value(
                        &mut state.filters.romm_platform,
                        Some(platform.clone()),
                        platform,
                    );
                }
            });
        filters_changed |= before != state.filters.romm_platform;

        if !regions.is_empty() {
            ui.label("Region");
            let before = state.filters.region.clone();
            egui::ComboBox::from_id_salt("romm-region")
                .selected_text(
                    state
                        .filters
                        .region
                        .clone()
                        .unwrap_or_else(|| "any".to_string()),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.filters.region, None, "any");
                    for region in &regions {
                        ui.selectable_value(
                            &mut state.filters.region,
                            Some(region.clone()),
                            region,
                        );
                    }
                });
            filters_changed |= before != state.filters.region;
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("Title contains");
        if ui
            .add(
                egui::TextEdit::singleline(&mut state.title_input)
                    .desired_width(220.0)
                    .hint_text("part of a title"),
            )
            .changed()
        {
            // Bounded, and never compiled as a pattern.
            state.title_input.truncate(MAX_TITLE_FILTER);
            state.filters.title = state.title_input.clone();
            filters_changed = true;
        }
        filters_changed |= ui
            .checkbox(&mut state.filters.multi_file_only, "Multi-file only")
            .changed();
        filters_changed |= ui
            .checkbox(
                &mut state.filters.unknown_platform_only,
                "Unknown platform only",
            )
            .changed();
        filters_changed |= ui
            .checkbox(
                &mut state.filters.file_detail_omitted_only,
                "File detail omitted",
            )
            .changed();
        filters_changed |= ui
            .checkbox(&mut state.filters.has_artwork_only, "Has artwork reference")
            .changed();
    });

    if filters_changed {
        // Any change restarts at the first page: keeping an offset that was
        // meaningful under the old filters would show an arbitrary slice.
        request = Some(BrowseRequest::LoadRecords {
            offset: 0,
            limit: state.page_size,
        });
    }

    // --- Page ------------------------------------------------------------
    let Some(page) = state.page.as_ref() else {
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading the cached records...");
            });
        } else if request.is_none() {
            request = Some(BrowseRequest::LoadRecords {
                offset: 0,
                limit: state.page_size,
            });
        }
        return request;
    };

    ui.separator();
    // The whole catalogue's verdict spread, so the page in view has context.
    widgets::status_rows(
        ui,
        &[
            (
                "Confirmed",
                &page.counts.confirmed.to_string(),
                widgets::StatusTone::Success,
            ),
            (
                "Strong",
                &page.counts.strong.to_string(),
                widgets::StatusTone::Info,
            ),
            (
                "Probable",
                &page.counts.probable.to_string(),
                widgets::StatusTone::Pending,
            ),
            (
                "Ambiguous",
                &page.counts.ambiguous.to_string(),
                widgets::StatusTone::Warning,
            ),
            (
                "Stale",
                &page.counts.stale.to_string(),
                widgets::StatusTone::Warning,
            ),
            (
                "Unmatched",
                &page.counts.unmatched.to_string(),
                widgets::StatusTone::Pending,
            ),
        ]
        .map(|(label, value, tone)| (label, value.as_str(), tone)),
    );
    ui.horizontal_wrapped(|ui| {
        ui.label(if page.filters.is_empty() {
            format!(
                "Showing all {} record(s). Page {} of {}.",
                page.total_in_cache,
                page.page_number(),
                page.page_count()
            )
        } else {
            format!(
                "{} of {} record(s) match these filters. Page {} of {}.",
                page.matching,
                page.total_in_cache,
                page.page_number(),
                page.page_count()
            )
        });
        if widgets::action_button(
            ui,
            "Previous",
            widgets::ActionStyle::Secondary,
            page.has_previous() && !busy,
        )
        .clicked()
        {
            request = Some(BrowseRequest::LoadRecords {
                offset: page.offset.saturating_sub(page.limit),
                limit: page.limit,
            });
        }
        if widgets::action_button(
            ui,
            "Next",
            widgets::ActionStyle::Secondary,
            page.has_next() && !busy,
        )
        .clicked()
        {
            request = Some(BrowseRequest::LoadRecords {
                offset: page.offset + page.limit,
                limit: page.limit,
            });
        }
    });

    if page.rows.is_empty() {
        widgets::empty_state(
            ui,
            "No records match these filters",
            "Relax a filter, or clear the title text. The cache itself is unchanged.",
            None,
        );
        return request;
    }

    for row in &page.rows {
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(&row.title);
                widgets::status_badge(ui, verdict_label(row.verdict), verdict_tone(row.verdict));
                if let Some(presence) = row.presence {
                    widgets::status_badge(ui, presence_label(presence), presence_tone(presence));
                }
                if let Some(platform) = &row.canonical_platform {
                    widgets::status_badge(ui, platform.clone(), widgets::StatusTone::Info);
                } else {
                    widgets::status_badge(ui, "unknown platform", widgets::StatusTone::Warning);
                }
                if row.related_files >= 2 {
                    widgets::status_badge(
                        ui,
                        format!("{} files", row.related_files),
                        widgets::StatusTone::Info,
                    );
                }
                if row.file_detail_omitted {
                    widgets::status_badge(ui, "file list omitted", widgets::StatusTone::Warning);
                }
            });
            // Long paths wrap and carry their full value rather than overflowing.
            ui.add(egui::Label::new(&row.romm_path).wrap())
                .on_hover_text(&row.romm_path);
            match &row.archivefs_path {
                Some(path) => {
                    ui.add(egui::Label::new(format!("-> {}", path.display())).wrap())
                        .on_hover_text(path.display().to_string());
                }
                None => {
                    ui.label("No mapping covers this record.");
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("RomM id {}", row.romm_game_id));
                if let Some(platform) = &row.romm_platform {
                    ui.label(format!("· RomM platform {platform}"));
                }
                if !row.regions.is_empty() {
                    ui.label(format!("· {}", row.regions.join(", ")));
                }
                if let Some(revision) = &row.revision {
                    ui.label(format!("· rev {revision}"));
                }
                if let Some(size) = row.file_size_bytes {
                    ui.label(format!("· {}", human_bytes(size)));
                }
                ui.label(if row.published_hashes.is_empty() {
                    "· no published hash".to_string()
                } else {
                    format!("· {}", row.published_hashes.join("/"))
                });
                ui.label(if row.hash_verified {
                    "· hash verified locally"
                } else {
                    "· not locally verified"
                });
            });
            if let Some(reason) = &row.stale_reason {
                ui.add(egui::Label::new(reason).wrap());
            }
            let details_clicked =
                widgets::action_button(ui, "Details", widgets::ActionStyle::Secondary, true)
                    .clicked();
            if details_clicked && request.is_none() {
                request = Some(BrowseRequest::OpenDetail {
                    romm_game_id: row.romm_game_id.clone(),
                });
            }
        });
    }

    request
}

fn show_record_detail(
    ui: &mut egui::Ui,
    detail: &RecordDetailView,
    cover: &CoverState,
    cover_texture: &mut Option<egui::TextureHandle>,
    cover_key: &mut Option<String>,
) {
    widgets::card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong(&detail.row.title);
            widgets::status_badge(
                ui,
                verdict_label(detail.row.verdict),
                verdict_tone(detail.row.verdict),
            );
        });
        ui.label(&detail.verdict_explanation);
        if let Some(explanation) = &detail.presence_explanation {
            widgets::banner(
                ui,
                "About this path",
                explanation,
                widgets::StatusTone::Info,
            );
        }
        for CardRow { label, value } in &detail.rows {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{label}:"));
                ui.add(egui::Label::new(value).wrap()).on_hover_text(value);
            });
        }
        if !detail.conflicts.is_empty() {
            ui.strong("Disagreements");
            for line in &detail.conflicts {
                ui.label(format!(
                    "{}: RomM {} vs local {}",
                    line.field, line.romm, line.local
                ));
                ui.label(&line.detail);
            }
        }
        widgets::technical_details(ui, "romm-record-evidence", |ui| {
            ui.strong("Evidence");
            for line in &detail.evidence {
                ui.add(egui::Label::new(line).wrap());
            }
            if !detail.metadata_ids.is_empty() {
                ui.strong("Metadata ids");
                for CardRow { label, value } in &detail.metadata_ids {
                    ui.label(format!("{label} = {value}"));
                }
            }
            if !detail.related_files.is_empty() {
                ui.strong(format!("Files ({})", detail.related_files.len()));
                for file in detail.related_files.iter().take(20) {
                    ui.add(egui::Label::new(file).wrap());
                }
                if detail.related_files.len() > 20 {
                    ui.label(format!(
                        "and {} more, not listed here",
                        detail.related_files.len() - 20
                    ));
                }
            }
            if !detail.sibling_game_ids.is_empty() {
                ui.label(format!(
                    "Sibling RomM records: {}",
                    detail.sibling_game_ids.join(", ")
                ));
            }
        });
        ui.separator();
        show_record_artwork(ui, detail, cover, cover_texture, cover_key);
    });
}

fn show_record_artwork(
    ui: &mut egui::Ui,
    detail: &RecordDetailView,
    cover: &CoverState,
    cover_texture: &mut Option<egui::TextureHandle>,
    cover_key: &mut Option<String>,
) {
    ui.strong("Artwork");
    ui.label(if detail.has_romm_thumbnail {
        "RomM-hosted path_cover_small: available"
    } else {
        "RomM-hosted path_cover_small: not recorded"
    });
    if detail.has_public_artwork_reference {
        ui.label(
            "Public artwork reference recorded, but ArchiveFS does not fetch from public hosts.",
        );
    }

    let displayed = if detail.artwork == ArtworkAvailability::Fetchable {
        cover.clone()
    } else {
        CoverState::Unavailable(detail.artwork)
    };
    ui.label(displayed.line());
    match displayed {
        CoverState::Ready(image) => {
            if cover_key.as_deref() != Some(image.key.as_str()) {
                *cover_texture = Some(ui.ctx().load_texture(
                    format!("romm-record-cover-{}", image.key),
                    image.image.clone(),
                    egui::TextureOptions::LINEAR,
                ));
                *cover_key = Some(image.key.clone());
            }
            if let Some(texture) = cover_texture.as_ref() {
                ui.add(
                    egui::Image::new(texture)
                        .fit_to_exact_size(fitted_cover_size(image.width, image.height))
                        .alt_text("RomM-hosted thumbnail for this record"),
                );
            }
        }
        CoverState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading RomM thumbnail");
            });
        }
        CoverState::Idle
        | CoverState::Unavailable(_)
        | CoverState::Refused(_)
        | CoverState::Offline(_)
        | CoverState::Failed(_)
        | CoverState::Cancelled => {
            ui.label(egui::RichText::new("Artwork placeholder").weak());
        }
    }
}

fn show_conflicts(ui: &mut egui::Ui, state: &mut BrowseState, busy: bool) -> Option<BrowseRequest> {
    let mut request = None;
    let Some(page) = state.conflicts.as_ref() else {
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for conflicting identity claims...");
            });
        } else {
            request = Some(BrowseRequest::LoadConflicts { offset: 0 });
        }
        return request;
    };

    if page.is_empty() {
        // Precise about what it does and does not mean.
        widgets::empty_state(
            ui,
            "No conflicting identity claims were found in the current RomM cache.",
            "Two records claiming one file, or RomM disagreeing with locally verified evidence, \
             would appear here. This says nothing about stale or unmatched records - those have \
             their own views.",
            None,
        );
        ui.label(format!(
            "Checked all {} cached record(s).",
            page.total_in_cache
        ));
        return request;
    }

    ui.horizontal_wrapped(|ui| {
        ui.label(format!(
            "{} record(s) conflict. Showing {}-{}.",
            page.matching,
            page.offset,
            page.offset + page.rows.len()
        ));
        if widgets::action_button(
            ui,
            "Previous",
            widgets::ActionStyle::Secondary,
            page.has_previous() && !busy,
        )
        .clicked()
        {
            request = Some(BrowseRequest::LoadConflicts {
                offset: page.offset.saturating_sub(page.limit),
            });
        }
        if widgets::action_button(
            ui,
            "Next",
            widgets::ActionStyle::Secondary,
            page.has_next() && !busy,
        )
        .clicked()
        {
            request = Some(BrowseRequest::LoadConflicts {
                offset: page.offset + page.limit,
            });
        }
    });
    ui.label(
        "Nothing here is resolved automatically. Where ArchiveFS had stronger local evidence, it \
         was kept and RomM's claim recorded beside it.",
    );

    for row in &page.rows {
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(&row.title);
                widgets::status_badge(ui, verdict_label(row.verdict), verdict_tone(row.verdict));
                ui.label(format!("RomM id {}", row.romm_game_id));
            });
            ui.add(egui::Label::new(&row.romm_path).wrap());
            if let Some(path) = &row.archivefs_path {
                ui.add(egui::Label::new(format!("-> {}", path.display())).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "Platform: {} (RomM: {})",
                    row.canonical_platform.as_deref().unwrap_or("unknown"),
                    row.romm_platform.as_deref().unwrap_or("-")
                ));
            });
            for line in &row.conflicts {
                widgets::status_rows(
                    ui,
                    &[
                        ("Field", line.field.as_str(), widgets::StatusTone::Info),
                        (
                            "RomM says",
                            line.romm.as_str(),
                            widgets::StatusTone::Warning,
                        ),
                        ("Locally", line.local.as_str(), widgets::StatusTone::Success),
                    ],
                );
                ui.add(egui::Label::new(&line.detail).wrap());
            }
            if let Some(retained) = &row.local_evidence_retained {
                widgets::banner(
                    ui,
                    "ArchiveFS's own identity was kept",
                    retained,
                    widgets::StatusTone::Success,
                );
            }
            if !row.competing_records.is_empty() {
                ui.label(format!(
                    "Competing RomM records: {}",
                    row.competing_records.join(", ")
                ));
            }
            widgets::technical_details(ui, format!("conflict-{}", row.romm_game_id), |ui| {
                for line in &row.evidence {
                    ui.add(egui::Label::new(line).wrap());
                }
                ui.label(format!("Imported from {}", row.provenance));
            });
        });
    }
    request
}

fn show_stale_summary(
    ui: &mut egui::Ui,
    state: &mut BrowseState,
    busy: bool,
    progress: Option<&StaleProgress>,
) -> Option<BrowseRequest> {
    let mut request = None;
    ui.label(
        "Groups every stale record by what is actually at its path. Reads file metadata only - no \
         contents, no hashing, no request to RomM, and nothing is written or repaired.",
    );
    ui.horizontal_wrapped(|ui| {
        if widgets::action_button(
            ui,
            "Run stale summary",
            widgets::ActionStyle::Primary,
            !busy,
        )
        .clicked()
        {
            request = Some(BrowseRequest::RunStaleSummary);
        }
        if widgets::action_button(ui, "Cancel", widgets::ActionStyle::Quiet, busy).clicked() {
            request = Some(BrowseRequest::Cancel);
        }
    });
    if busy {
        ui.horizontal(|ui| {
            ui.spinner();
            match progress {
                Some(progress) => {
                    ui.label(format!(
                        "Checking paths: {} of {}",
                        progress.probed, progress.total
                    ));
                }
                None => {
                    ui.label("Reading the cache...");
                }
            }
        });
        if let Some(fraction) = progress.and_then(StaleProgress::fraction) {
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
        }
        // Deliberately no partial result: a half-probed partition would read as a
        // finding rather than as an unfinished pass.
        ui.label("Partial results are not shown, because a half-checked partition is misleading.");
    }

    let Some(view) = state.stale.as_ref() else {
        return request;
    };
    let summary = &view.summary;

    ui.separator();
    ui.strong(format!(
        "{} of {} cached record(s) are stale",
        summary.stale, summary.total_in_cache
    ));
    if let Some(interpretation) = view.interpretation() {
        widgets::banner(
            ui,
            "Interpretation",
            interpretation,
            widgets::StatusTone::Info,
        );
    } else if summary.stale > 0 {
        widgets::banner(
            ui,
            "Worth checking the mappings",
            "A large share of these is neither flagged missing by RomM nor a broken link, which is \
             the shape a path-mapping fault would take.",
            widgets::StatusTone::Warning,
        );
    }

    for reason in &summary.by_reason {
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(reason.label);
                widgets::status_badge(
                    ui,
                    format!("{} ({})", reason.count, view.share(reason.count)),
                    widgets::StatusTone::Info,
                );
                widgets::status_badge(
                    ui,
                    format!("{} flagged missing by RomM", reason.romm_reports_missing),
                    widgets::StatusTone::Pending,
                );
            });
            for example in &reason.examples {
                ui.add(egui::Label::new(format!("e.g. {}", example.romm_path)).wrap())
                    .on_hover_text(&example.romm_path);
            }
            let remaining = reason.count.saturating_sub(reason.examples.len());
            if remaining > 0 {
                ui.label(format!("and {remaining} more, not listed here"));
            }
        });
    }

    let group_section = |ui: &mut egui::Ui, title: &str, groups: &[StaleGroup], omitted: usize| {
        widgets::technical_details(ui, title, |ui| {
            ui.strong(title);
            for group in groups {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{}", group.count));
                    ui.add(egui::Label::new(&group.key).wrap())
                        .on_hover_text(&group.key);
                    ui.label(format!("({} flagged by RomM)", group.romm_reports_missing));
                });
            }
            if omitted > 0 {
                ui.label(format!("and {omitted} more, not listed separately"));
            }
        });
    };
    group_section(
        ui,
        "By platform",
        &summary.by_platform,
        summary.platforms_not_listed,
    );
    group_section(
        ui,
        "By RomM path prefix",
        &summary.by_romm_prefix,
        summary.romm_prefixes_not_listed,
    );
    group_section(
        ui,
        "By local folder",
        &summary.by_local_prefix,
        summary.local_prefixes_not_listed,
    );
    group_section(
        ui,
        "By file extension",
        &summary.by_extension,
        summary.extensions_not_listed,
    );
    group_section(ui, "By mapping used", &summary.by_mapping, 0);

    widgets::status_rows(
        ui,
        &[
            (
                "Flagged missing by RomM",
                &summary.romm_reports_missing.to_string(),
                widgets::StatusTone::Info,
            ),
            (
                "Dangling symlinks",
                &summary.dangling_symlinks.to_string(),
                widgets::StatusTone::Warning,
            ),
            (
                "Present directories",
                &summary.present_as_directory.to_string(),
                widgets::StatusTone::Info,
            ),
            (
                "Genuinely multi-file",
                &summary.multi_file.to_string(),
                widgets::StatusTone::Info,
            ),
        ]
        .map(|(label, value, tone)| (label, value.as_str(), tone)),
    );
    request
}

#[cfg(test)]
mod tests;
