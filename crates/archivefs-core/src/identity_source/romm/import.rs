//! Driving one import, and the status it produces.
//!
//! # Bounds, and why each exists
//!
//! Every limit here answers a specific way a remote server can misbehave:
//!
//! - page size is clamped, so a configured page cannot be unbounded;
//! - the page count is capped, so a server that never reports the end cannot
//!   loop for ever;
//! - the record count is capped, so a server with a runaway catalogue cannot
//!   produce an unbounded cache;
//! - a page whose offset does not advance ends the walk, because a server
//!   returning the same page for ever is the other way to loop;
//! - the *server's* total is treated as a hint for progress only. The walk ends
//!   when a page comes back short or empty, not when the total says so, because
//!   a wrong total should not truncate an import or extend it indefinitely;
//! - an overall deadline stops an import that is technically progressing but
//!   will not finish today.
//!
//! # Nothing is published until everything succeeds
//!
//! Records accumulate in memory, are matched, are validated as a whole, and only
//! then written. Any failure - transport, malformed page, cancellation, a
//! validation refusal - returns an error and the previous cache stays exactly
//! where it was. See [`crate::identity_source::cache::publish_cache`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::capability::RommCapabilityReport;
use super::client::{MAX_PAGE_SIZE, RommClient, RommRequestError, RommTransport};
use super::config::ValidatedRommSource;
use super::normalise::{NormalisationReport, normalise_platform, normalise_rom};
use crate::identity_source::cache::{
    CACHE_FORMAT_VERSION, IdentityCache, MAX_CACHED_RECORDS, PublishFailure,
};
use crate::identity_source::model::{ExternalIdentityRecord, IdentityProvider};

/// Default page size. RomM's own default is 50; 100 halves the round trips
/// without approaching the clamp.
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// The most pages one import will walk.
pub const MAX_IMPORT_PAGES: u32 = 2000;

/// How long a whole import may take before it is abandoned.
pub const IMPORT_DEADLINE: Duration = Duration::from_secs(600);

/// How far the reported total may be exceeded before it is called inconsistent.
/// A small overshoot is normal if the catalogue grew mid-import; a large one
/// means the total cannot be trusted for progress.
pub const TOTAL_OVERSHOOT_TOLERANCE: u64 = 1000;

/// Progress during an import, for a caller to display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportProgress {
    pub pages_fetched: u32,
    pub records_fetched: usize,
    /// The server's reported total, when it looks trustworthy.
    pub reported_total: Option<u64>,
}

impl ImportProgress {
    /// A fraction, only when the total is present and plausible. `None` means
    /// "unknown" rather than a made-up number.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.reported_total?;
        if total == 0 || self.records_fetched as u64 > total {
            return None;
        }
        Some(self.records_fetched as f32 / total as f32)
    }
}

/// Why an import did not complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ImportFailure {
    /// The instance cannot be imported from, and why.
    NotCapable {
        detail: String,
    },
    Request(RommRequestError),
    /// A page's envelope did not describe a page.
    InvalidPagination {
        detail: String,
    },
    /// The server kept returning the same page.
    RepeatedPage {
        offset: u32,
    },
    TooManyRecords {
        count: usize,
        maximum: usize,
    },
    TooManyPages {
        maximum: u32,
    },
    /// The reported total and what arrived disagree beyond tolerance.
    InconsistentTotal {
        reported: u64,
        received: usize,
    },
    DeadlineExceeded {
        seconds: u64,
    },
    Cancelled,
    /// The import worked but the cache could not be published.
    Publish(PublishFailure),
}

impl ImportFailure {
    pub fn detail(&self) -> String {
        match self {
            Self::NotCapable { detail } => detail.clone(),
            Self::Request(error) => error.detail(),
            Self::InvalidPagination { detail } => {
                format!("RomM returned a page this import could not use: {detail}")
            }
            Self::RepeatedPage { offset } => format!(
                "RomM kept returning the same page at offset {offset}, so the import stopped \
                 rather than looping"
            ),
            Self::TooManyRecords { count, maximum } => format!(
                "RomM offered at least {count} records, above the {maximum} this import will hold"
            ),
            Self::TooManyPages { maximum } => {
                format!("the import reached its {maximum}-page limit without finishing")
            }
            Self::InconsistentTotal { reported, received } => format!(
                "RomM reported {reported} records but {received} arrived, which is too large a \
                 discrepancy to treat as a complete import"
            ),
            Self::DeadlineExceeded { seconds } => {
                format!("the import did not finish within {seconds} seconds")
            }
            Self::Cancelled => "the import was cancelled".to_string(),
            Self::Publish(failure) => failure.detail(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NotCapable { .. } => "not_capable",
            Self::Request(error) => error.code(),
            Self::InvalidPagination { .. } => "invalid_pagination",
            Self::RepeatedPage { .. } => "repeated_page",
            Self::TooManyRecords { .. } => "too_many_records",
            Self::TooManyPages { .. } => "too_many_pages",
            Self::InconsistentTotal { .. } => "inconsistent_total",
            Self::DeadlineExceeded { .. } => "deadline_exceeded",
            Self::Cancelled => "cancelled",
            Self::Publish(_) => "publish_failed",
        }
    }

    /// Every failure preserves the previous cache. Stated as code so it cannot
    /// drift from the promise.
    pub fn previous_cache_preserved(&self) -> bool {
        true
    }
}

/// What one import produced, before it is published.
#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub cache: IdentityCache,
    pub progress: ImportProgress,
    pub normalisation: NormalisationReport,
}

/// How much of the catalogue to take. A bounded sample is what a person should
/// try first, and what a smoke test uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportScope {
    /// Everything, subject to the module's bounds.
    Full,
    /// At most this many records - still paginated, just stopped early.
    Sample { max_records: usize },
}

impl ImportScope {
    fn record_limit(self) -> usize {
        match self {
            Self::Full => MAX_CACHED_RECORDS,
            Self::Sample { max_records } => max_records.min(MAX_CACHED_RECORDS),
        }
    }
}

/// Imports platforms and ROM records into an unpublished cache.
///
/// Performs no local matching and writes nothing: the caller matches, then
/// publishes. Splitting it that way is what makes "a failed refresh keeps the old
/// cache" structural rather than a promise - there is no code path here that
/// touches the live file.
pub fn import_identity<T: RommTransport>(
    source: &ValidatedRommSource,
    transport: &T,
    scope: ImportScope,
    capability: &RommCapabilityReport,
    mut on_progress: impl FnMut(ImportProgress),
    cancel: Option<&AtomicBool>,
) -> Result<ImportOutcome, ImportFailure> {
    if let Some(reason) = capability.api.blocking_reason() {
        return Err(ImportFailure::NotCapable { detail: reason });
    }
    let started = Instant::now();
    let client = RommClient::new(source, transport);
    let imported_at = now_unix_seconds();

    // Platforms first: a small, single request, and it is what the ROM records'
    // platform ids refer to.
    let platforms = client
        .platforms(cancel)
        .map_err(ImportFailure::Request)?
        .iter()
        .filter_map(normalise_platform)
        .collect::<Vec<_>>();

    let mut records: Vec<ExternalIdentityRecord> = Vec::new();
    let mut report = NormalisationReport::default();
    let mut progress = ImportProgress {
        pages_fetched: 0,
        records_fetched: 0,
        reported_total: None,
    };
    let record_limit = scope.record_limit();
    let mut offset: u32 = 0;
    let mut last_offset: Option<u32> = None;
    // The last total the server reported. Only ever a hint.
    let mut server_total: Option<u64> = None;

    loop {
        if cancelled(cancel) {
            return Err(ImportFailure::Cancelled);
        }
        if started.elapsed() > IMPORT_DEADLINE {
            return Err(ImportFailure::DeadlineExceeded {
                seconds: IMPORT_DEADLINE.as_secs(),
            });
        }
        if progress.pages_fetched >= MAX_IMPORT_PAGES {
            return Err(ImportFailure::TooManyPages {
                maximum: MAX_IMPORT_PAGES,
            });
        }

        let page = client
            .roms_page(DEFAULT_PAGE_SIZE, offset, cancel)
            .map_err(ImportFailure::Request)?;

        // The envelope has to describe the page that was asked for. A server
        // echoing a different offset is not paginating, and checking that is only
        // possible because the client reports the server's own numbers rather
        // than the request's.
        if let Some(reported) = page.reported_offset
            && reported != offset
        {
            return Err(ImportFailure::InvalidPagination {
                detail: format!("asked for offset {offset} but the page reports offset {reported}"),
            });
        }
        if let Some(reported) = page.reported_limit
            && (reported == 0 || reported > MAX_PAGE_SIZE)
        {
            return Err(ImportFailure::InvalidPagination {
                detail: format!("the page reports an unusable limit of {reported}"),
            });
        }
        // The same offset arriving twice means the walk is not advancing.
        if last_offset == Some(offset) {
            return Err(ImportFailure::RepeatedPage { offset });
        }
        last_offset = Some(offset);
        // The largest total the server ever claimed, not the last one. A trailing
        // empty page that reports zero must not be allowed to condemn a catalogue
        // the earlier pages described consistently - while a server that claims a
        // genuinely too-small total on every page is still caught, because the
        // maximum of those claims is still too small.
        server_total = Some(server_total.map_or(page.total, |seen| seen.max(page.total)));
        progress.pages_fetched += 1;

        let received = page.items.len();
        for item in &page.items {
            match normalise_rom(
                item,
                source.server_id(),
                source.mappings(),
                imported_at,
                &mut report,
            ) {
                Some(record) => records.push(record),
                None => report.skipped_records += 1,
            }
            if records.len() > record_limit {
                // For a full import this is a real limit; for a sample it is the
                // requested stopping point, handled below.
                if matches!(scope, ImportScope::Full) {
                    return Err(ImportFailure::TooManyRecords {
                        count: records.len(),
                        maximum: record_limit,
                    });
                }
                break;
            }
        }
        progress.records_fetched = records.len();
        // The total is only offered as progress when it is plausible.
        progress.reported_total = server_total.filter(|total| {
            *total > 0 && records.len() as u64 <= total.saturating_add(TOTAL_OVERSHOOT_TOLERANCE)
        });
        on_progress(progress);

        // A sample stops when it has enough.
        if records.len() >= record_limit {
            records.truncate(record_limit);
            break;
        }
        // A short or empty page is the end of the catalogue. This, not the
        // server's total, is what ends the walk - and it is checked before the
        // total is judged, because reaching the end is exactly the moment a
        // trailing page's total should not be allowed to condemn an import that
        // has in fact just finished.
        if received < page.effective_limit() as usize {
            break;
        }

        // Still walking, and the arriving records have already blown past the
        // reported total by more than the tolerance. That cannot be a total, so
        // the import stops here rather than after walking a catalogue whose size
        // nobody can state.
        if matches!(scope, ImportScope::Full)
            && let Some(total) = server_total
            && records.len() as u64 > total.saturating_add(TOTAL_OVERSHOOT_TOLERANCE)
        {
            return Err(ImportFailure::InconsistentTotal {
                reported: total,
                received: records.len(),
            });
        }
        offset =
            offset
                .checked_add(page.effective_limit())
                .ok_or(ImportFailure::InvalidPagination {
                    detail: "the next offset would overflow".to_string(),
                })?;
    }

    // A final guard for the case where the walk ended on a short page before the
    // in-loop check could fire.
    if matches!(scope, ImportScope::Full)
        && let Some(total) = server_total
        && records.len() as u64 > total.saturating_add(TOTAL_OVERSHOOT_TOLERANCE)
    {
        return Err(ImportFailure::InconsistentTotal {
            reported: total,
            received: records.len(),
        });
    }

    let mut cache = IdentityCache {
        format_version: CACHE_FORMAT_VERSION,
        provider: IdentityProvider::Romm,
        server_id: source.server_id().to_string(),
        server_version: capability
            .heartbeat
            .as_ref()
            .map(|heartbeat| heartbeat.version.clone()),
        source_fingerprint: source_fingerprint(source),
        imported_at_unix_seconds: imported_at,
        platforms,
        records,
        rejected_hashes: report.rejected_hashes.clone(),
        unknown_platforms: report.unknown_platforms.clone(),
        server_reported_total: server_total,
    };
    cache.sort_deterministically();
    Ok(ImportOutcome {
        cache,
        progress,
        normalisation: report,
    })
}

/// A fingerprint of the configuration that produced a cache.
///
/// Covers the origin and every mapping, so a changed mapping is visible as a
/// reason to refresh. Never covers the token: the fingerprint is written to the
/// cache, and a token must not be.
pub fn source_fingerprint(source: &ValidatedRommSource) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(source.server_id().as_bytes());
    for mapping in source.mappings().as_slice() {
        digest.update(b"\0");
        digest.update(mapping.provider_prefix.as_bytes());
        digest.update(b"=>");
        digest.update(mapping.archivefs_prefix.to_string_lossy().as_bytes());
    }
    digest
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}
