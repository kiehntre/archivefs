//! The RomM identity source card, and the shape of its operations.
//!
//! # Why there is a view model
//!
//! The card shows about thirty numbers and a dozen conditional labels, and the
//! properties that matter most about it are negative ones: a token must never
//! appear, a failed operation must never erase the counts that were there before,
//! and a sample import must never be presented as readiness. Those are hard to
//! test by poking at rendered pixels and easy to test as data.
//!
//! So [`RommCardView`] is built from authoritative state by a pure function, and
//! [`show_romm_source_card`] only draws it. The tests assert on the view model,
//! plus a real headless render to prove the drawing carries no secret.
//!
//! # What this slice does not do
//!
//! Configuration, mappings, record browsing, conflicts, the stale-summary view and
//! the identity panel all belong to later slices. Their buttons are present and
//! visibly disabled with an honest label, because a card that silently lacks them
//! is harder to understand than one that says what is coming.

use std::path::PathBuf;

use archivefs_core::identity_source::artwork::ArtworkCacheStats;
use archivefs_core::identity_source::romm::capability::RommCapabilityReport;
use archivefs_core::identity_source::romm::import::{AdaptivePagination, ImportProgress};
use archivefs_core::identity_source::settings::ProviderSettings;
use archivefs_core::identity_source::status::{ProviderState, ProviderStatus};
use eframe::egui;

use crate::ui::{components as widgets, theme};

/// How many records a sample import takes. Bounded, and small enough that the
/// answer arrives while someone is still looking at the card.
pub(crate) const SAMPLE_IMPORT_RECORDS: usize = 25;

/// One mutating or diagnostic RomM operation the card can ask for.
///
/// Deliberately not `Copy`: each is dispatched once, and a value that can be
/// duplicated invites exactly the double-launch this slice has to prevent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RommOperation {
    /// Re-read settings, cache status and artwork stats. Touches no network.
    LoadStatus,
    TestConnection,
    SetEnabled(bool),
    SampleImport {
        records: usize,
    },
    FullImport,
    Refresh,
    ClearArtwork,
    /// Write the configuration. Validates again in the worker, and contacts nothing.
    SaveConfiguration(Box<archivefs_core::identity_source::settings::ProviderSettings>),
    /// Translate a bounded sample of provider paths. Publishes nothing.
    Preview {
        limit: usize,
    },
}

impl RommOperation {
    /// Whether this changes anything ArchiveFS owns.
    ///
    /// A status load does not, which is what makes it safe to run while a mutating
    /// operation is in flight.
    pub(crate) fn is_mutating(&self) -> bool {
        !matches!(self, Self::LoadStatus | Self::Preview { .. })
    }

    /// Whether this should disable the card's actions while it runs.
    ///
    /// Everything except a status load, which is a fast local read that happens
    /// when the page opens and must not make the card look busy.
    pub(crate) fn blocks_actions(&self) -> bool {
        !matches!(self, Self::LoadStatus)
    }

    /// Whether this contacts RomM.
    pub(crate) fn uses_network(&self) -> bool {
        matches!(
            self,
            Self::TestConnection
                | Self::SampleImport { .. }
                | Self::FullImport
                | Self::Refresh
                | Self::Preview { .. }
        )
    }

    /// Whether progress can be reported for it.
    pub(crate) fn reports_progress(&self) -> bool {
        matches!(
            self,
            Self::SampleImport { .. } | Self::FullImport | Self::Refresh
        )
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::LoadStatus => "Reading RomM status",
            Self::TestConnection => "Testing the connection to RomM",
            Self::SetEnabled(true) => "Enabling the RomM source",
            Self::SetEnabled(false) => "Disabling the RomM source",
            Self::SampleImport { .. } => "Importing a sample",
            Self::FullImport => "Importing the RomM catalogue",
            Self::Refresh => "Refreshing from RomM",
            Self::ClearArtwork => "Clearing cover thumbnails",
            Self::SaveConfiguration(_) => "Saving the RomM configuration",
            Self::Preview { .. } => "Previewing path mappings",
        }
    }
}

/// Everything the card needs, read from authoritative state rather than kept in
/// step by hand.
#[derive(Clone, Debug)]
pub(crate) struct RommSnapshot {
    pub(crate) settings: ProviderSettings,
    pub(crate) status: ProviderStatus,
    pub(crate) artwork: ArtworkCacheStats,
    /// Whether a usable token file is configured. Never the token.
    pub(crate) token_available: bool,
    /// Why the token is unusable, already redacted by the core loader.
    pub(crate) token_problem: Option<String>,
    pub(crate) cache_format_version: Option<u32>,
}

/// What a completed operation produced, for the result area.
#[derive(Clone, Debug)]
pub(crate) enum RommOperationOutcome {
    /// A refreshed authoritative snapshot. Not a user-visible result: it is how the
    /// card gets its numbers, and showing "status refreshed" after every operation
    /// would be noise.
    Snapshot(Box<RommSnapshot>),
    Connection(Box<RommConnectionSummary>),
    Sample(Box<RommImportSummary>),
    Import(Box<RommImportSummary>),
    Enabled(bool),
    ArtworkCleared {
        items: usize,
        bytes: u64,
    },
    Saved(Box<archivefs_core::identity_source::settings::ProviderSettings>),
    Preview(Box<crate::romm_config::RommPreviewSummary>),
}

/// The connection test, reduced to what the card shows. Built in the worker so no
/// provider payload reaches the UI thread.
#[derive(Clone, Debug, Default)]
pub(crate) struct RommConnectionSummary {
    pub(crate) server_id: String,
    pub(crate) romm_version: Option<String>,
    pub(crate) api_version: Option<String>,
    pub(crate) version_supported: bool,
    pub(crate) endpoints: Vec<String>,
    pub(crate) missing_endpoints: Vec<String>,
    pub(crate) read_scopes: Vec<String>,
    pub(crate) authenticated_reads: Vec<(String, bool)>,
    pub(crate) supports_pagination: bool,
    pub(crate) hash_fields: Vec<String>,
    pub(crate) artwork_fields: Vec<String>,
    pub(crate) exposes_file_list: bool,
    pub(crate) supports_client_tokens: bool,
    pub(crate) configured_path_kind: String,
    pub(crate) observed_path_kind: Option<String>,
    pub(crate) path_kind_agrees: bool,
    pub(crate) can_import: bool,
    pub(crate) blocking_reason: Option<String>,
}

impl RommConnectionSummary {
    /// Reduces a capability report. Takes only the fields the card names, so a
    /// future provider field cannot leak into the UI by being added upstream.
    pub(crate) fn from_report(
        report: &RommCapabilityReport,
        configured_path_kind: &str,
        observed_path_kind: Option<&str>,
        authenticated_reads: Vec<(String, bool)>,
    ) -> Self {
        let heartbeat = report.heartbeat.as_ref();
        let reads_ok = authenticated_reads.iter().all(|(_, ok)| *ok);
        Self {
            server_id: report.server_id.clone(),
            romm_version: heartbeat.map(|beat| beat.version.clone()),
            api_version: report.api.api_version.clone(),
            version_supported: heartbeat.is_some_and(|beat| beat.is_supported()),
            endpoints: report.api.available_endpoints.clone(),
            missing_endpoints: report.api.missing_endpoints.clone(),
            read_scopes: report.api.declared_read_scopes.clone(),
            supports_pagination: report.api.supports_limit_offset_pagination,
            hash_fields: report.api.available_hash_fields.clone(),
            artwork_fields: report.api.available_artwork_fields.clone(),
            exposes_file_list: report.api.exposes_file_list,
            supports_client_tokens: report.api.supports_client_tokens,
            configured_path_kind: configured_path_kind.to_string(),
            path_kind_agrees: observed_path_kind
                .is_none_or(|observed| observed == configured_path_kind),
            observed_path_kind: observed_path_kind.map(str::to_string),
            can_import: report.api.can_import() && reads_ok,
            blocking_reason: report.api.blocking_reason(),
            authenticated_reads,
        }
    }
}

/// An import's result, whether or not it published.
#[derive(Clone, Debug, Default)]
pub(crate) struct RommImportSummary {
    /// `false` for a sample, always. A sample is a preview.
    pub(crate) published: bool,
    pub(crate) cache_path: Option<PathBuf>,
    pub(crate) cache_bytes: Option<u64>,
    pub(crate) records: usize,
    pub(crate) platforms: usize,
    pub(crate) confirmed: usize,
    pub(crate) strong: usize,
    pub(crate) probable: usize,
    pub(crate) ambiguous: usize,
    pub(crate) stale: usize,
    pub(crate) unmatched: usize,
    pub(crate) unknown_platforms: usize,
    pub(crate) invalid_hashes: usize,
    pub(crate) multi_file_groups: usize,
    pub(crate) pages_fetched: u32,
    pub(crate) elapsed_milliseconds: u128,
    pub(crate) adaptive: Option<AdaptivePagination>,
    /// On failure: what went wrong, and whether the old cache survived.
    pub(crate) failure: Option<String>,
    pub(crate) failure_code: Option<String>,
    pub(crate) previous_cache_usable: bool,
}

/// Live progress, kept small because it is replaced many times a second.
#[derive(Clone, Debug, Default)]
pub(crate) struct RommProgress {
    pub(crate) pages_fetched: u32,
    pub(crate) records_fetched: usize,
    pub(crate) reported_total: Option<u64>,
    pub(crate) page_size: u32,
    /// Human notes worth keeping on screen - a page-size reduction, a record whose
    /// file list was too large. Bounded, so a long import cannot grow this without
    /// limit.
    pub(crate) notes: Vec<String>,
}

/// The most notes kept. Beyond this the oldest are dropped: they are a running
/// commentary, not a log.
pub(crate) const MAX_PROGRESS_NOTES: usize = 6;

impl RommProgress {
    /// Folds one core progress event in, turning an adaptive reduction into a
    /// sentence rather than leaving the reader to interpret three numbers.
    pub(crate) fn absorb(&mut self, event: ImportProgress) {
        self.pages_fetched = event.pages_fetched;
        self.records_fetched = event.records_fetched;
        self.reported_total = event.reported_total;
        self.page_size = event.page_size;
        if let Some(reduction) = event.reduction {
            self.note(format!(
                "Large response encountered at offset {}. Retrying the same records with a \
                 smaller page size ({} instead of {}).",
                reduction.offset, reduction.to, reduction.from
            ));
        }
    }

    pub(crate) fn note(&mut self, note: String) {
        if self.notes.last().is_some_and(|last| *last == note) {
            return;
        }
        self.notes.push(note);
        if self.notes.len() > MAX_PROGRESS_NOTES {
            self.notes.remove(0);
        }
    }

    /// A fraction, only when the total is plausible.
    pub(crate) fn fraction(&self) -> Option<f32> {
        let total = self.reported_total?;
        if total == 0 || self.records_fetched as u64 > total {
            return None;
        }
        Some(self.records_fetched as f32 / total as f32)
    }
}

/// One labelled row in the card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardRow {
    pub(crate) label: String,
    pub(crate) value: String,
}

fn row(label: &str, value: impl Into<String>) -> CardRow {
    CardRow {
        label: label.to_string(),
        value: value.into(),
    }
}

/// One action the card offers, and whether it can be taken right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardAction {
    pub(crate) label: String,
    pub(crate) operation: Option<RommOperation>,
    pub(crate) enabled: bool,
    /// Why it cannot be taken, when it cannot. Shown as a tooltip rather than left
    /// to guesswork.
    pub(crate) disabled_reason: Option<String>,
    pub(crate) style: CardActionStyle,
    /// Set for actions that arrive in a later slice, so the label can say so and
    /// the button can never look functional.
    pub(crate) coming_next: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardActionStyle {
    Primary,
    Secondary,
    Quiet,
    Destructive,
}

/// Everything the card draws, as data.
#[derive(Clone, Debug)]
pub(crate) struct RommCardView {
    pub(crate) state_label: String,
    pub(crate) state_detail: Option<String>,
    pub(crate) badges: Vec<(String, widgets::StatusTone)>,
    /// The handful of rows worth seeing without expanding anything.
    pub(crate) summary_rows: Vec<CardRow>,
    /// Verdict counts, collapsible.
    pub(crate) verdict_rows: Vec<CardRow>,
    /// Data-quality counts, collapsible.
    pub(crate) quality_rows: Vec<CardRow>,
    pub(crate) cache_rows: Vec<CardRow>,
    pub(crate) artwork_rows: Vec<CardRow>,
    pub(crate) actions: Vec<CardAction>,
    /// The last error, already redacted by the core that produced it.
    pub(crate) last_error: Option<String>,
    pub(crate) offline_browsing: bool,
    /// True while an operation is running, which is what disables the actions.
    pub(crate) busy: bool,
    pub(crate) busy_label: Option<String>,
    pub(crate) cancellable: bool,
    /// Whether the running operation is talking to RomM, so the card can say so.
    pub(crate) contacting_romm: bool,
}

/// Builds the view from authoritative state. Pure: no clock, no filesystem, no
/// network, so a test can state exactly what a given state renders as.
pub(crate) fn build_card_view(
    snapshot: Option<&RommSnapshot>,
    running: Option<&RommOperation>,
    cancellation_requested: bool,
) -> RommCardView {
    let busy = running.is_some_and(RommOperation::blocks_actions);
    let busy_label = running
        .filter(|operation| operation.blocks_actions())
        .map(|operation| operation.label().to_string());
    let cancellable =
        running.is_some_and(RommOperation::reports_progress) && !cancellation_requested;

    let Some(snapshot) = snapshot else {
        // Before the first status load there is nothing truthful to show, and
        // guessing would mean showing zeroes that look like real counts.
        return RommCardView {
            state_label: "Reading local state".to_string(),
            state_detail: None,
            badges: vec![("Checking".to_string(), widgets::StatusTone::Pending)],
            summary_rows: Vec::new(),
            verdict_rows: Vec::new(),
            quality_rows: Vec::new(),
            cache_rows: Vec::new(),
            artwork_rows: Vec::new(),
            actions: Vec::new(),
            last_error: None,
            offline_browsing: false,
            busy: true,
            busy_label: Some("Reading RomM status".to_string()),
            cancellable: false,
            contacting_romm: false,
        };
    };

    let status = &snapshot.status;
    let settings = &snapshot.settings;
    let configured = !settings.source.url.trim().is_empty();
    let counts = &status.counts;

    let mut badges = vec![(status.state.label().to_string(), state_tone(&status.state))];
    badges.push(if settings.source.enabled {
        ("Enabled".to_string(), widgets::StatusTone::Success)
    } else {
        ("Disabled".to_string(), widgets::StatusTone::Pending)
    });
    badges.push(if configured {
        ("Configured".to_string(), widgets::StatusTone::Info)
    } else {
        ("Not configured".to_string(), widgets::StatusTone::Warning)
    });
    badges.push((
        "Read-only towards RomM".to_string(),
        widgets::StatusTone::Info,
    ));
    if status.state.can_browse() {
        badges.push((
            "Browsable offline".to_string(),
            widgets::StatusTone::Success,
        ));
    }

    let summary_rows = vec![
        row(
            "URL",
            if configured {
                settings.source.url.clone()
            } else {
                "not configured".to_string()
            },
        ),
        row("Path shape", settings.source.provider_path_kind.label()),
        row(
            "Mappings",
            format!("{} configured", settings.source.mappings.len()),
        ),
        row(
            "Token file",
            match (&settings.source.token_path, snapshot.token_available) {
                // The path, never the contents.
                (Some(path), true) => format!("{} (usable)", path.display()),
                (Some(path), false) => format!(
                    "{} - {}",
                    path.display(),
                    snapshot.token_problem.as_deref().unwrap_or("not usable")
                ),
                (None, _) => "not configured".to_string(),
            },
        ),
        row("Page size", settings.effective_page_size().to_string()),
        row(
            "RomM version",
            status
                .server_version
                .clone()
                .unwrap_or_else(|| "unknown until tested".to_string()),
        ),
        row("Records", status.records_imported.to_string()),
        row("Platforms", status.platforms_imported.to_string()),
        row(
            "Last import",
            status
                .last_successful_refresh_unix_seconds
                .map(|seconds| format!("unix {seconds}"))
                .unwrap_or_else(|| "never".to_string()),
        ),
    ];

    let verdict_rows = vec![
        row("Confirmed", counts.confirmed.to_string()),
        row("Strong", counts.strong.to_string()),
        row("Probable", counts.probable.to_string()),
        row("Ambiguous", counts.ambiguous.to_string()),
        row("Stale", counts.stale.to_string()),
        row("Unmatched", counts.unmatched.to_string()),
    ];

    let quality_rows = vec![
        row("Unknown platforms", status.unknown_platforms.to_string()),
        row("Invalid hashes", status.invalid_hashes.to_string()),
        row("Multi-file groups", status.multi_file_groups.to_string()),
        row("Locally verified", status.locally_verified.to_string()),
        row("Duplicate targets", status.duplicate_mappings.to_string()),
    ];

    let cache_rows = vec![
        row(
            "Cache",
            status
                .cache_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "none published".to_string()),
        ),
        row(
            "Cache size",
            status
                .cache_size_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "-".to_string()),
        ),
        row(
            "Cache version",
            snapshot
                .cache_format_version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "-".to_string()),
        ),
    ];

    let artwork_rows = vec![
        row(
            "Thumbnails",
            format!(
                "{} cached, {}",
                snapshot.artwork.items,
                human_bytes(snapshot.artwork.bytes)
            ),
        ),
        row(
            "Thumbnail limit",
            format!(
                "{} (least-recently-used eviction)",
                human_bytes(snapshot.artwork.maximum_bytes)
            ),
        ),
        row(
            "Thumbnail cache version",
            snapshot.artwork.format_version.to_string(),
        ),
        row(
            "Last cleanup",
            snapshot
                .artwork
                .last_cleanup_unix_seconds
                .map(|seconds| format!("unix {seconds}"))
                .unwrap_or_else(|| "never".to_string()),
        ),
    ];

    let actions = build_actions(snapshot, configured, busy, cancellable);

    RommCardView {
        state_label: status.state.label().to_string(),
        state_detail: state_detail(&status.state),
        badges,
        summary_rows,
        verdict_rows,
        quality_rows,
        cache_rows,
        artwork_rows,
        actions,
        last_error: status.last_error.clone(),
        offline_browsing: status.state.can_browse(),
        busy,
        busy_label,
        cancellable,
        contacting_romm: running.is_some_and(RommOperation::uses_network),
    }
}

fn build_actions(
    snapshot: &RommSnapshot,
    configured: bool,
    busy: bool,
    cancellable: bool,
) -> Vec<CardAction> {
    let enabled = snapshot.settings.source.enabled;
    let has_cache = snapshot.status.state.can_browse();
    let ready_to_talk = configured && snapshot.token_available;
    let needs_configuration = "Configure the URL and token file first.".to_string();
    let busy_reason = "Another RomM operation is running.".to_string();

    let gate = |allowed: bool, reason: &str| -> (bool, Option<String>) {
        if busy {
            (false, Some(busy_reason.clone()))
        } else if allowed {
            (true, None)
        } else {
            (false, Some(reason.to_string()))
        }
    };

    let mut actions = Vec::new();

    let (test_enabled, test_reason) = gate(ready_to_talk, &needs_configuration);
    actions.push(CardAction {
        label: "Test connection".to_string(),
        operation: Some(RommOperation::TestConnection),
        enabled: test_enabled,
        disabled_reason: test_reason,
        style: CardActionStyle::Secondary,
        coming_next: false,
    });

    let (toggle_enabled, toggle_reason) = gate(configured, &needs_configuration);
    actions.push(CardAction {
        label: if enabled { "Disable" } else { "Enable" }.to_string(),
        operation: Some(RommOperation::SetEnabled(!enabled)),
        enabled: toggle_enabled,
        disabled_reason: toggle_reason,
        style: CardActionStyle::Quiet,
        coming_next: false,
    });

    let import_allowed = ready_to_talk && enabled;
    let import_reason = if !enabled {
        "Enable the source first.".to_string()
    } else {
        needs_configuration.clone()
    };
    let (sample_enabled, sample_reason) = gate(import_allowed, &import_reason);
    actions.push(CardAction {
        label: format!("Import sample ({SAMPLE_IMPORT_RECORDS})"),
        operation: Some(RommOperation::SampleImport {
            records: SAMPLE_IMPORT_RECORDS,
        }),
        enabled: sample_enabled,
        disabled_reason: sample_reason,
        style: CardActionStyle::Secondary,
        coming_next: false,
    });

    let (full_enabled, full_reason) = gate(import_allowed, &import_reason);
    actions.push(CardAction {
        label: if has_cache {
            "Refresh".to_string()
        } else {
            "Import full catalogue".to_string()
        },
        operation: Some(if has_cache {
            RommOperation::Refresh
        } else {
            RommOperation::FullImport
        }),
        enabled: full_enabled,
        disabled_reason: full_reason,
        style: CardActionStyle::Primary,
        coming_next: false,
    });

    // Cancel is the one action that is enabled *because* something is running.
    actions.push(CardAction {
        label: "Cancel".to_string(),
        operation: None,
        enabled: cancellable,
        disabled_reason: (!cancellable).then(|| "Nothing cancellable is running.".to_string()),
        style: CardActionStyle::Quiet,
        coming_next: false,
    });

    let (clear_enabled, clear_reason) = gate(
        snapshot.artwork.items > 0,
        "There are no cached thumbnails to clear.",
    );
    actions.push(CardAction {
        label: "Clear cover thumbnails".to_string(),
        operation: Some(RommOperation::ClearArtwork),
        enabled: clear_enabled,
        disabled_reason: clear_reason,
        style: CardActionStyle::Destructive,
        coming_next: false,
    });

    // Configuration and mappings live in their own dialog, which needs no source to
    // be configured already - that is what it is for.
    actions.push(CardAction {
        label: "Configure".to_string(),
        operation: None,
        enabled: !busy,
        disabled_reason: busy.then(|| busy_reason.clone()),
        style: CardActionStyle::Secondary,
        coming_next: false,
    });

    // Later slices. Present, honestly labelled, and impossible to click.
    for label in ["Browse records", "View stale summary"] {
        actions.push(CardAction {
            label: format!("{label} (coming next)"),
            operation: None,
            enabled: false,
            disabled_reason: Some(
                "This arrives in the next slice of RomM GUI work. The command line already has it."
                    .to_string(),
            ),
            style: CardActionStyle::Quiet,
            coming_next: true,
        });
    }

    actions
}

fn state_tone(state: &ProviderState) -> widgets::StatusTone {
    match state {
        ProviderState::Ready => widgets::StatusTone::Success,
        ProviderState::ReadyOffline => widgets::StatusTone::Info,
        ProviderState::Importing => widgets::StatusTone::Active,
        ProviderState::NotConfigured | ProviderState::Disabled | ProviderState::NeverImported => {
            widgets::StatusTone::Pending
        }
        ProviderState::Stale { .. } => widgets::StatusTone::Warning,
        ProviderState::Error { .. } => widgets::StatusTone::Blocked,
    }
}

fn state_detail(state: &ProviderState) -> Option<String> {
    match state {
        ProviderState::NotConfigured => Some(
            "Point ArchiveFS at your RomM instance and a read-only token file to get started."
                .to_string(),
        ),
        ProviderState::NeverImported => Some(
            "Configured and switched on. Nothing has been imported yet, which is not a fault - \
             run an import when you are ready."
                .to_string(),
        ),
        ProviderState::Disabled => Some(
            "Switched off. Anything already imported stays browsable, and nothing will be \
             refreshed."
                .to_string(),
        ),
        ProviderState::Importing => Some("An import is running.".to_string()),
        ProviderState::Ready => None,
        ProviderState::ReadyOffline => Some(
            "Serving imported identity without having reached RomM. This is the offline case \
             working as intended."
                .to_string(),
        ),
        ProviderState::Stale { detail } => Some(detail.clone()),
        ProviderState::Error { detail } => Some(detail.clone()),
    }
}

/// Bytes as a person would read them.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= GIB {
        if bytes.is_multiple_of(GIB) {
            format!("{} GiB", bytes / GIB)
        } else {
            format!("{:.1} GiB", bytes as f64 / GIB as f64)
        }
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

/// What the card's own dialog state remembers between frames.
#[derive(Clone, Debug, Default)]
pub(crate) struct RommCardState {
    pub(crate) clear_artwork_confirm: bool,
    pub(crate) show_verdicts: bool,
    pub(crate) show_quality: bool,
    /// The last completed operation's result, kept until the next one starts.
    pub(crate) last_outcome: Option<RommResultView>,
}

/// A completed operation, reduced to what the result area draws.
#[derive(Clone, Debug)]
pub(crate) struct RommResultView {
    pub(crate) succeeded: bool,
    pub(crate) headline: String,
    pub(crate) rows: Vec<CardRow>,
    pub(crate) notes: Vec<String>,
}

/// Renders a completed operation as a result view.
pub(crate) fn build_result_view(
    operation: &RommOperation,
    outcome: Result<&RommOperationOutcome, &str>,
) -> RommResultView {
    match outcome {
        Err(message) => RommResultView {
            succeeded: false,
            // The message comes from the core, which redacts its own errors.
            headline: format!("{} failed", operation.label()),
            rows: vec![row("Reason", message)],
            notes: Vec::new(),
        },
        Ok(RommOperationOutcome::Snapshot(_)) => RommResultView {
            succeeded: true,
            headline: "Status refreshed".to_string(),
            rows: Vec::new(),
            notes: Vec::new(),
        },
        Ok(RommOperationOutcome::Enabled(enabled)) => RommResultView {
            succeeded: true,
            headline: format!(
                "RomM source {}",
                if *enabled { "enabled" } else { "disabled" }
            ),
            rows: Vec::new(),
            notes: vec![if *enabled {
                "Nothing was contacted. Run a connection test or an import when you are ready."
                    .to_string()
            } else {
                "Configuration and imported identity are kept.".to_string()
            }],
        },
        Ok(RommOperationOutcome::ArtworkCleared { items, bytes }) => RommResultView {
            succeeded: true,
            headline: format!("Cleared {items} cover thumbnail(s)"),
            rows: vec![row("Reclaimed", human_bytes(*bytes))],
            notes: vec![
                "The imported identity, RomM's own artwork and your ROM files were not touched."
                    .to_string(),
            ],
        },
        Ok(RommOperationOutcome::Saved(settings)) => RommResultView {
            succeeded: true,
            headline: "Configuration saved".to_string(),
            rows: crate::romm_config::describe_saved(settings),
            notes: vec![
                "Written atomically to ArchiveFS's own configuration. Nothing was contacted, and \
                 no import ran - use Test connection or Refresh when you are ready."
                    .to_string(),
            ],
        },
        Ok(RommOperationOutcome::Preview(summary)) => RommResultView {
            succeeded: summary.path_shape_agrees() && summary.refused == 0,
            headline: summary.headline(),
            rows: crate::romm_config::preview_count_rows(summary),
            notes: vec![format!(
                "Sampled {} path(s) from {}. Nothing was imported, published or changed.",
                summary.examples.len(),
                summary.sample_source
            )],
        },
        Ok(RommOperationOutcome::Connection(summary)) => build_connection_result(summary),
        Ok(RommOperationOutcome::Sample(summary)) => build_import_result(summary, true),
        Ok(RommOperationOutcome::Import(summary)) => build_import_result(summary, false),
    }
}

fn build_connection_result(summary: &RommConnectionSummary) -> RommResultView {
    let mut rows = vec![
        row("Server", summary.server_id.clone()),
        row(
            "RomM version",
            format!(
                "{} ({})",
                summary.romm_version.as_deref().unwrap_or("unknown"),
                if summary.version_supported {
                    "supported"
                } else {
                    "not verified against this build"
                }
            ),
        ),
        row(
            "API version",
            summary
                .api_version
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        ),
        row("Endpoints", summary.endpoints.join(", ")),
        row("Read scopes", summary.read_scopes.join(", ")),
        row("Pagination", yes_no(summary.supports_pagination)),
        row(
            "Hash fields",
            if summary.hash_fields.is_empty() {
                "none".to_string()
            } else {
                summary.hash_fields.join(", ")
            },
        ),
        row(
            "Artwork fields",
            if summary.artwork_fields.is_empty() {
                "none".to_string()
            } else {
                summary.artwork_fields.join(", ")
            },
        ),
        row("Multi-file detail", yes_no(summary.exposes_file_list)),
        row("Client tokens", yes_no(summary.supports_client_tokens)),
    ];
    for (endpoint, ok) in &summary.authenticated_reads {
        rows.push(row(
            &format!("Read {endpoint}"),
            if *ok { "ok" } else { "failed" },
        ));
    }
    rows.push(row(
        "Path shape",
        match &summary.observed_path_kind {
            Some(observed) => format!(
                "configured {}, reported {observed}",
                summary.configured_path_kind
            ),
            None => format!(
                "configured {}, nothing to sample",
                summary.configured_path_kind
            ),
        },
    ));

    let mut notes = vec!["Nothing was imported, cached or changed in RomM.".to_string()];
    if !summary.path_kind_agrees
        && let Some(observed) = &summary.observed_path_kind
    {
        notes.push(format!(
            "This server reports {observed} paths but the source is set to {}. Until that is \
             changed, every record will stay unmatched.",
            summary.configured_path_kind
        ));
    }
    if !summary.missing_endpoints.is_empty() {
        notes.push(format!(
            "Missing endpoints: {}",
            summary.missing_endpoints.join(", ")
        ));
    }
    if let Some(reason) = &summary.blocking_reason {
        notes.push(reason.clone());
    }

    RommResultView {
        succeeded: summary.can_import,
        headline: if summary.can_import {
            "RomM answered and is ready to import".to_string()
        } else {
            "RomM answered, but an import cannot run yet".to_string()
        },
        rows,
        notes,
    }
}

fn build_import_result(summary: &RommImportSummary, sample: bool) -> RommResultView {
    if let Some(failure) = &summary.failure {
        let mut notes = vec![if summary.previous_cache_usable {
            "The identity you already had is untouched and still browsable.".to_string()
        } else {
            "Nothing was published, and there was no previous cache to lose.".to_string()
        }];
        if let Some(adaptive) = &summary.adaptive
            && adaptive.reductions > 0
        {
            notes.push(format!(
                "Paging had already reduced from {} to {} over {} oversized response(s).",
                adaptive.configured_page_size,
                adaptive.smallest_page_size,
                adaptive.oversized_retries
            ));
        }
        return RommResultView {
            succeeded: false,
            headline: if sample {
                "Sample import failed".to_string()
            } else {
                "Import failed".to_string()
            },
            rows: vec![
                row("Reason", failure.clone()),
                row(
                    "Code",
                    summary
                        .failure_code
                        .clone()
                        .unwrap_or_else(|| "-".to_string()),
                ),
            ],
            notes,
        };
    }

    let mut rows = vec![
        row("Records", summary.records.to_string()),
        row("Platforms", summary.platforms.to_string()),
        row("Confirmed", summary.confirmed.to_string()),
        row("Strong", summary.strong.to_string()),
        row("Probable", summary.probable.to_string()),
        row("Ambiguous", summary.ambiguous.to_string()),
        row("Stale", summary.stale.to_string()),
        row("Unmatched", summary.unmatched.to_string()),
        row("Unknown platforms", summary.unknown_platforms.to_string()),
        row("Invalid hashes", summary.invalid_hashes.to_string()),
        row("Multi-file groups", summary.multi_file_groups.to_string()),
        row(
            "Pages",
            format!(
                "{} in {} ms",
                summary.pages_fetched, summary.elapsed_milliseconds
            ),
        ),
    ];
    if let Some(path) = &summary.cache_path {
        rows.push(row("Published to", path.display().to_string()));
    }
    if let Some(bytes) = summary.cache_bytes {
        rows.push(row("Cache size", human_bytes(bytes)));
    }

    let mut notes = Vec::new();
    if sample {
        notes.push(
            "A sample is a preview: nothing was published, and any identity you already had is \
             exactly as it was. The source's state has not changed."
                .to_string(),
        );
    } else if summary.published {
        notes.push(
            "Published atomically: the new identity became visible in one step, so a reader never \
             saw a half-written cache."
                .to_string(),
        );
    }
    if let Some(adaptive) = &summary.adaptive {
        if adaptive.reductions > 0 {
            notes.push(format!(
                "Some responses were too large to read. Paging stepped down from {} to {} over {} \
                 refused response(s) and recovered {} time(s); the same records were always \
                 re-requested, so nothing was skipped.",
                adaptive.configured_page_size,
                adaptive.smallest_page_size,
                adaptive.oversized_retries,
                adaptive.recoveries
            ));
        }
        if !adaptive.records_without_file_detail.is_empty() {
            notes.push(format!(
                "Game identity imported for {} record(s), but their detailed file list was \
                 omitted because the provider response exceeded the safety limit: RomM id {}.",
                adaptive.records_without_file_detail.len(),
                adaptive
                    .records_without_file_detail
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    RommResultView {
        succeeded: true,
        headline: if sample {
            format!(
                "Sample of {} record(s) - nothing published",
                summary.records
            )
        } else {
            format!("Imported {} record(s)", summary.records)
        },
        rows,
        notes,
    }
}

fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_string()
}

/// One progress event from a worker.
///
/// Lives beside the card rather than in `main` so the worker and the renderer agree
/// on the shape, and so nothing that carries a provider payload can be added to it
/// without touching this file.
#[derive(Clone, Debug)]
pub(crate) enum RommProgressEvent {
    Import(ImportProgress),
    /// A note the core cannot phrase itself, such as which record lost its file
    /// list. Already free of provider payloads.
    Note(String),
}

/// What the card wants the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RommCardRequest {
    Start(RommOperation),
    Cancel,
    /// Open the configuration and mappings dialog.
    OpenConfigure,
}

/// Draws the card. Thin by design: every decision was made in
/// [`build_card_view`], which is why the tests can make those decisions'
/// assertions without a window.
pub(crate) fn show_romm_source_card(
    ui: &mut egui::Ui,
    view: &RommCardView,
    state: &mut RommCardState,
    progress: Option<&RommProgress>,
) -> Option<RommCardRequest> {
    let mut request = None;
    widgets::section_header(
        ui,
        "RomM",
        Some("Optional local identity source. Read-only towards RomM."),
    );
    widgets::card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.strong("RomM");
            for (label, tone) in &view.badges {
                widgets::status_badge(ui, label.clone(), *tone);
            }
        });
        // Spelled out as well as badged: a coloured chip is not readable at
        // television distance, and it is not readable at all to a screen reader.
        ui.label(format!("State: {}", view.state_label));
        if let Some(detail) = &view.state_detail {
            ui.label(detail);
        }
        if view.offline_browsing {
            ui.label("Imported identity is browsable without reaching RomM.");
        }

        for CardRow { label, value } in &view.summary_rows {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{label}:"));
                ui.strong(value);
            });
        }

        // Two collapsible blocks, so the card is not a wall of thirty numbers on a
        // television while still having every number a person might want.
        ui.add_space(theme::SECTION_GAP / 2.0);
        egui::CollapsingHeader::new("Match verdicts")
            .default_open(state.show_verdicts)
            .show(ui, |ui| {
                for CardRow { label, value } in &view.verdict_rows {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{label}:"));
                        ui.strong(value);
                    });
                }
            });
        egui::CollapsingHeader::new("Data quality and cache")
            .default_open(state.show_quality)
            .show(ui, |ui| {
                for CardRow { label, value } in
                    view.quality_rows.iter().chain(view.cache_rows.iter())
                {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{label}:"));
                        ui.strong(value);
                    });
                }
            });
        egui::CollapsingHeader::new("Cover thumbnails")
            .default_open(false)
            .show(ui, |ui| {
                for CardRow { label, value } in &view.artwork_rows {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{label}:"));
                        ui.strong(value);
                    });
                }
            });

        if let Some(error) = &view.last_error {
            widgets::banner(
                ui,
                "Last operation failed",
                error,
                widgets::StatusTone::Warning,
            );
        }

        // Live progress.
        if let Some(progress) = progress {
            ui.add_space(theme::SECTION_GAP / 2.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    view.busy_label
                        .clone()
                        .unwrap_or_else(|| "Working".to_string()),
                );
            });
            ui.label(format!(
                "Page {} · {} record(s) · page size {}",
                progress.pages_fetched, progress.records_fetched, progress.page_size
            ));
            if let Some(fraction) = progress.fraction() {
                ui.add(egui::ProgressBar::new(fraction).show_percentage());
            }
            for note in &progress.notes {
                ui.label(note);
            }
        } else if view.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    view.busy_label
                        .clone()
                        .unwrap_or_else(|| "Working".to_string()),
                );
            });
        }
        if view.busy {
            // Whether the network is in use, and whether it can be stopped, are the
            // two things worth knowing while waiting.
            let mut notes = Vec::new();
            if view.contacting_romm {
                notes.push("Contacting RomM.");
            }
            if view.cancellable {
                notes.push("This can be cancelled.");
            }
            if !notes.is_empty() {
                ui.label(notes.join(" "));
            }
        }

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for action in &view.actions {
                let style = match action.style {
                    CardActionStyle::Primary => widgets::ActionStyle::Primary,
                    CardActionStyle::Secondary => widgets::ActionStyle::Secondary,
                    CardActionStyle::Quiet => widgets::ActionStyle::Quiet,
                    CardActionStyle::Destructive => widgets::ActionStyle::Destructive,
                };
                let mut response =
                    widgets::action_button(ui, action.label.clone(), style, action.enabled);
                if let Some(reason) = &action.disabled_reason {
                    response = response.on_disabled_hover_text(reason);
                }
                if response.clicked() {
                    match &action.operation {
                        // Clearing thumbnails is destructive, so it asks first.
                        Some(RommOperation::ClearArtwork) => {
                            state.clear_artwork_confirm = true;
                        }
                        Some(operation) => {
                            request = Some(RommCardRequest::Start(operation.clone()));
                        }
                        None if action.label == "Cancel" => {
                            request = Some(RommCardRequest::Cancel);
                        }
                        None if action.label == "Configure" => {
                            request = Some(RommCardRequest::OpenConfigure);
                        }
                        None => {}
                    }
                }
            }
        });

        if state.clear_artwork_confirm {
            widgets::banner(
                ui,
                "Clear cached cover thumbnails?",
                "Only ArchiveFS's own thumbnails are removed. Your imported identity, RomM's own \
                 artwork and your ROM files are untouched, and covers are refetched when they are \
                 next shown.",
                widgets::StatusTone::Warning,
            );
            ui.horizontal(|ui| {
                if widgets::action_button(
                    ui,
                    "Confirm clear",
                    widgets::ActionStyle::Destructive,
                    !view.busy,
                )
                .clicked()
                {
                    state.clear_artwork_confirm = false;
                    request = Some(RommCardRequest::Start(RommOperation::ClearArtwork));
                }
                if ui.button("Cancel").clicked() {
                    state.clear_artwork_confirm = false;
                }
            });
        }

        if let Some(result) = &state.last_outcome {
            ui.add_space(theme::SECTION_GAP / 2.0);
            widgets::banner(
                ui,
                &result.headline,
                &result
                    .notes
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "Completed.".to_string()),
                if result.succeeded {
                    widgets::StatusTone::Success
                } else {
                    widgets::StatusTone::Warning
                },
            );
            widgets::technical_details(ui, "Details", |ui| {
                for CardRow { label, value } in &result.rows {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{label}:"));
                        ui.strong(value);
                    });
                }
                for note in result.notes.iter().skip(1) {
                    ui.label(note);
                }
            });
        }
    });
    request
}

#[cfg(test)]
mod tests;
