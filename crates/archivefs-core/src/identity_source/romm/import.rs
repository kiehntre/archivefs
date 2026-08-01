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
//! # Oversized pages
//!
//! A page of records can exceed the client's response ceiling: a real RomM 5.1.0
//! catalogue did so at offset 4400 with 100 records per page. The ceiling is not
//! raised in response - it is what stops a hostile or broken server handing over
//! an unbounded body. Instead the *same offset* is retried with a smaller page,
//! stepping down a fixed ladder, and the import continues at whatever size
//! worked. Because the offset does not move until a page succeeds, no record is
//! skipped and none is fetched twice. See [`next_page_size`].
//!
//! Two things happen after the ladder runs out, because a real catalogue needed
//! both:
//!
//! - **A single record can be too large on its own.** One PS4 game carried 28,831
//!   entries in its file list, making its own response 17.5 MB. That record is
//!   re-fetched *without* the per-file detail, which brings it to 436 KB. The file
//!   list is what is lost, and every record it happens to is counted and named in
//!   [`AdaptivePagination`] rather than quietly emptied.
//! - **The size recovers, carefully.** A reduction is caused by particular fat
//!   records, not by a property of the server: pages either side of that one were
//!   1.5 MB. Staying at the reduced size for the rest of a 36,259-record catalogue
//!   would need tens of thousands of requests and could not finish inside the
//!   deadline, so after [`RECOVERY_STREAK`] consecutive successes the size steps
//!   back up one rung, never above what was configured. The gate and a global
//!   budget of [`MAX_OVERSIZED_EVENTS`] refusals are what stop that from becoming
//!   an oscillation: past the budget the size freezes and never climbs again.
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

/// The most pages one import will walk at its configured page size.
///
/// A page-size reduction raises the real budget - see [`page_budget`] - because a
/// smaller page needs more requests to reach the same records. The record cap
/// stays the authority on how much an import may hold.
pub const MAX_IMPORT_PAGES: u32 = 2000;

/// The hard ceiling on page requests, whatever the page size falls to.
///
/// Reached only by a catalogue at the record cap fetched one record at a time, so
/// it bounds the pathological case without ever binding a real import.
pub const MAX_IMPORT_PAGES_ABSOLUTE: u32 = 250_000;

/// How many consecutive pages must succeed at a reduced size before it steps back
/// up one rung. Gates recovery so passing one fat record cannot start an
/// alternation between a size that fits and one that does not.
pub const RECOVERY_STREAK: u32 = 8;

/// The most oversized responses one import will absorb before it stops trying to
/// recover and holds the smallest size that worked.
///
/// Each refusal costs a read up to the ceiling, so this is what bounds the total
/// wasted transfer no matter how many fat records a catalogue holds.
pub const MAX_OVERSIZED_EVENTS: u32 = 64;

/// The page sizes an import steps down through when a response is too large.
///
/// Fixed and descending, so the sequence a given start produces is always the
/// same one. From the default of 100 it yields 100 -> 50 -> 25 -> 10 -> 5 -> 1.
pub const PAGE_SIZE_LADDER: &[u32] = &[200, 100, 50, 25, 10, 5, 1];

/// The next page size to try below `current`, or `None` when there is no smaller
/// one left and the failure is the record itself rather than the page.
///
/// Defined as "the largest ladder entry strictly below `current`" so that a
/// configured size which is not on the ladder still steps down deterministically:
/// 75 goes to 50, 13 to 10, 3 to 1.
pub fn next_page_size(current: u32) -> Option<u32> {
    PAGE_SIZE_LADDER
        .iter()
        .copied()
        .filter(|candidate| *candidate < current)
        .max()
}

/// The next page size *above* `current`, never exceeding `configured`.
///
/// The mirror of [`next_page_size`], used only by gated recovery.
pub fn previous_page_size(current: u32, configured: u32) -> Option<u32> {
    PAGE_SIZE_LADDER
        .iter()
        .copied()
        .filter(|candidate| *candidate > current && *candidate <= configured)
        .min()
}

/// How many page requests this import may make.
///
/// Scales with the smallest page size used so far, so a reduction cannot turn the
/// page cap into the thing that fails a legitimate import - while staying bounded
/// by [`MAX_IMPORT_PAGES_ABSOLUTE`]. Monotone, because the page size only ever
/// decreases, so the budget never shrinks mid-import.
pub fn page_budget(record_limit: usize, smallest_page_size: u32) -> u32 {
    let needed = (record_limit as u64)
        .div_ceil(u64::from(smallest_page_size.max(1)))
        .saturating_add(1);
    needed.clamp(
        u64::from(MAX_IMPORT_PAGES),
        u64::from(MAX_IMPORT_PAGES_ABSOLUTE),
    ) as u32
}

/// How long a whole import may take before it is abandoned.
pub const IMPORT_DEADLINE: Duration = Duration::from_secs(600);

/// How far the reported total may be exceeded before it is called inconsistent.
/// A small overshoot is normal if the catalogue grew mid-import; a large one
/// means the total cannot be trusted for progress.
pub const TOTAL_OVERSHOOT_TOLERANCE: u64 = 1000;

/// One page-size reduction, reported as it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PageSizeReduction {
    /// The offset being retried. Unchanged by the reduction, which is the whole
    /// point: the same records are asked for again, in smaller pieces.
    pub offset: u32,
    pub from: u32,
    pub to: u32,
    /// The ceiling the response exceeded, so the message can name it.
    pub ceiling_bytes: usize,
}

/// Progress during an import, for a caller to display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportProgress {
    pub pages_fetched: u32,
    pub records_fetched: usize,
    /// The server's reported total, when it looks trustworthy.
    pub reported_total: Option<u64>,
    /// The page size in use for the next request.
    pub page_size: u32,
    /// Set only on the callback that announces a reduction, so a caller reports
    /// each one exactly once rather than on every page afterwards.
    pub reduction: Option<PageSizeReduction>,
}

/// What adaptive paging did over one import, for the final report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdaptivePagination {
    /// What the import was asked to start with.
    pub configured_page_size: u32,
    /// What it finished with.
    pub effective_page_size: u32,
    /// The smallest it ever used. Equal to `effective_page_size` today, kept
    /// separate because it answers a different question and would diverge if
    /// recovery upward were ever added.
    pub smallest_page_size: u32,
    /// How many times the size stepped down.
    pub reductions: u32,
    /// How many requests were refused for being too large. Equal to `reductions`
    /// unless the ladder ran out, when the last retry has no reduction to show.
    pub oversized_retries: u32,
    /// How many times the size stepped back up after a run of successes.
    pub recoveries: u32,
    /// Records whose per-file detail had to be left out to read them at all.
    /// Named, not just counted, because it is a real gap in what was imported.
    pub records_without_file_detail: Vec<String>,
}

impl AdaptivePagination {
    fn new(configured: u32) -> Self {
        Self {
            configured_page_size: configured,
            effective_page_size: configured,
            smallest_page_size: configured,
            reductions: 0,
            oversized_retries: 0,
            recoveries: 0,
            records_without_file_detail: Vec::new(),
        }
    }

    /// Whether paging had to adapt at all.
    pub fn adapted(&self) -> bool {
        self.reductions > 0
    }

    /// Whether anything was imported with less detail than usual.
    pub fn lost_file_detail(&self) -> bool {
        !self.records_without_file_detail.is_empty()
    }
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
    /// Even a single record, asked for without its file list, exceeded the
    /// ceiling. There is nothing smaller left to ask for.
    OversizedRecord {
        offset: u32,
        page_size: u32,
        ceiling_bytes: usize,
    },
    /// Too many responses were refused for size. The import stops rather than
    /// spending the rest of the deadline reading bodies up to the ceiling and
    /// discarding them.
    TooManyOversizedPages {
        events: u32,
        maximum: u32,
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
            Self::OversizedRecord {
                offset,
                page_size,
                ceiling_bytes,
            } => format!(
                "even {page_size} record(s) at offset {offset} came back larger than the \
                 {ceiling_bytes}-byte ceiling, even without its file list, so this import cannot \
                 get past that point. The record's own id is not reported because that would mean \
                 reading the response that was refused; `GET /api/roms?limit=1&offset={offset}` on \
                 the RomM side identifies it. Nothing was published and any previous cache is \
                 untouched."
            ),
            Self::TooManyOversizedPages { events, maximum } => format!(
                "{events} RomM responses were too large to read, over the {maximum} this import \
                 will absorb; each one has to be read up to the ceiling before it can be refused, \
                 so continuing would spend the whole deadline discarding data. Nothing was \
                 published and any previous cache is untouched."
            ),
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
            Self::OversizedRecord { .. } => "oversized_record",
            Self::TooManyOversizedPages { .. } => "too_many_oversized_pages",
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
    pub adaptive: AdaptivePagination,
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
    configured_page_size: u32,
    on_progress: impl FnMut(ImportProgress),
    cancel: Option<&AtomicBool>,
) -> Result<ImportOutcome, ImportFailure> {
    import_identity_with_deadline(
        source,
        transport,
        scope,
        capability,
        configured_page_size,
        on_progress,
        cancel,
        IMPORT_DEADLINE,
    )
}

/// [`import_identity`] with the deadline supplied.
///
/// Exists so the tests can prove the deadline is honoured *inside* the oversized-
/// page retry sequence and not merely between pages - which a 600-second constant
/// makes untestable. Crate-internal: the deadline is not a caller's choice.
#[allow(clippy::too_many_arguments)]
pub(crate) fn import_identity_with_deadline<T: RommTransport>(
    source: &ValidatedRommSource,
    transport: &T,
    scope: ImportScope,
    capability: &RommCapabilityReport,
    configured_page_size: u32,
    mut on_progress: impl FnMut(ImportProgress),
    cancel: Option<&AtomicBool>,
    deadline: Duration,
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
    let mut page_size = configured_page_size.clamp(1, MAX_PAGE_SIZE);
    let configured_page_size = page_size;
    let mut adaptive = AdaptivePagination::new(page_size);
    // Per-file detail is asked for by default and only dropped as the last step
    // before giving up on a record entirely.
    let mut with_files = true;
    // Consecutive successful pages at the current size, for gated recovery.
    let mut success_streak: u32 = 0;
    let mut progress = ImportProgress {
        pages_fetched: 0,
        records_fetched: 0,
        reported_total: None,
        page_size,
        reduction: None,
    };
    let record_limit = scope.record_limit();
    let mut offset: u32 = 0;
    let mut last_offset: Option<u32> = None;
    // The last total the server reported. Only ever a hint - and re-read on every
    // page, including the pages of a retry, so a total that changes mid-import
    // affects nothing but the progress fraction.
    let mut server_total: Option<u64> = None;

    loop {
        if cancelled(cancel) {
            return Err(ImportFailure::Cancelled);
        }
        if started.elapsed() > deadline {
            return Err(ImportFailure::DeadlineExceeded {
                seconds: deadline.as_secs(),
            });
        }
        // Scaled by the smallest page size used so far, so stepping down cannot
        // make the page cap fail an import the record cap would have allowed.
        let budget = page_budget(record_limit, adaptive.smallest_page_size);
        if progress.pages_fetched >= budget {
            return Err(ImportFailure::TooManyPages { maximum: budget });
        }

        // Fetch this offset, stepping the page size down until a response fits.
        // The offset is not touched in here: every attempt asks for the same
        // records, so nothing can be skipped or counted twice.
        let page = loop {
            if cancelled(cancel) {
                return Err(ImportFailure::Cancelled);
            }
            if started.elapsed() > deadline {
                return Err(ImportFailure::DeadlineExceeded {
                    seconds: deadline.as_secs(),
                });
            }
            match client.roms_page_detail(page_size, offset, with_files, cancel) {
                Ok(page) => break page,
                Err(RommRequestError::ResponseTooLarge { limit }) => {
                    adaptive.oversized_retries += 1;
                    // Each refusal costs a read up to the ceiling, so absorbing an
                    // unlimited number of them would spend the deadline on data
                    // that is thrown away.
                    if adaptive.oversized_retries > MAX_OVERSIZED_EVENTS {
                        return Err(ImportFailure::TooManyOversizedPages {
                            events: adaptive.oversized_retries,
                            maximum: MAX_OVERSIZED_EVENTS,
                        });
                    }
                    // The ceiling is not negotiable; what is asked for is. Raising
                    // the ceiling instead would remove the only bound on how much a
                    // server can make this process read.
                    if let Some(smaller) = next_page_size(page_size) {
                        let reduction = PageSizeReduction {
                            offset,
                            from: page_size,
                            to: smaller,
                            ceiling_bytes: limit,
                        };
                        page_size = smaller;
                        success_streak = 0;
                        adaptive.reductions += 1;
                        adaptive.effective_page_size = smaller;
                        adaptive.smallest_page_size = adaptive.smallest_page_size.min(smaller);
                        // Announced once, on its own callback, so a caller can say
                        // so without repeating it on every later page.
                        progress.page_size = smaller;
                        progress.reduction = Some(reduction);
                        on_progress(progress);
                        progress.reduction = None;
                    } else if with_files {
                        // One record, and still too big. Its file list is the only
                        // part large enough to be worth dropping, so it goes - and
                        // the record is recorded as lacking it.
                        with_files = false;
                    } else {
                        return Err(ImportFailure::OversizedRecord {
                            offset,
                            page_size,
                            ceiling_bytes: limit,
                        });
                    }
                }
                Err(other) => return Err(ImportFailure::Request(other)),
            }
        };

        // The envelope has to describe the request that actually succeeded - the
        // retry's, not the original one - which is why these read `page_size` and
        // `offset` as they now stand. Checking it at all is only possible because
        // the client reports the server's own numbers alongside the request's.
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
        // More records than were asked for would break the accounting that keeps
        // the walk from skipping or repeating, so it is refused rather than
        // absorbed. Fewer is normal: that is the end of the catalogue.
        if page.items.len() > page.requested_limit as usize {
            return Err(ImportFailure::InvalidPagination {
                detail: format!(
                    "asked for {} record(s) but the page returned {}",
                    page.requested_limit,
                    page.items.len()
                ),
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
                Some(mut record) => {
                    // A page fetched without file detail produces records whose
                    // empty file list means "not asked for", not "no files". Saying
                    // so on the record is the difference between a known gap and a
                    // silent inaccuracy.
                    if !page.with_files {
                        record.evidence.push(
                            "RomM's file list for this record was too large to read, so its \
                             per-file detail was not imported"
                                .to_string(),
                        );
                        adaptive
                            .records_without_file_detail
                            .push(record.provider_game_id.clone());
                    }
                    records.push(record);
                }
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
        // A success at a reduced size counts towards climbing back. Recovery is
        // gated on a run of successes and stops entirely once the refusal budget is
        // spent, which is what keeps this from oscillating.
        success_streak = success_streak.saturating_add(1);
        with_files = true;
        if page_size < configured_page_size
            && success_streak >= RECOVERY_STREAK
            && adaptive.oversized_retries < MAX_OVERSIZED_EVENTS
            && let Some(larger) = previous_page_size(page_size, configured_page_size)
        {
            page_size = larger;
            success_streak = 0;
            adaptive.recoveries += 1;
            adaptive.effective_page_size = larger;
            progress.page_size = larger;
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
        // A short or empty page is the end of the catalogue, judged against the
        // size actually requested for this page - so after a reduction the test
        // uses the smaller size, and a full small page is not mistaken for the
        // end. This, not the server's total, is what ends the walk, and it is
        // checked before the total is judged, because reaching the end is exactly
        // the moment a trailing page's total should not condemn an import that has
        // in fact just finished.
        if received < page.requested_limit as usize {
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
        // Advanced by what actually arrived, not by any number the envelope
        // claimed. A server that reports one limit and delivers another cannot
        // make the walk skip records or fetch them twice.
        let step = u32::try_from(received).map_err(|_| ImportFailure::InvalidPagination {
            detail: "the page returned more records than an offset can express".to_string(),
        })?;
        offset = offset
            .checked_add(step)
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
        adaptive,
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
