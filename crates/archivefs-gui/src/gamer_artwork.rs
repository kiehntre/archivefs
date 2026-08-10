//! Cover artwork for the Gamer View game list.
//!
//! # Why this exists separately from the Details panel's cover
//!
//! The Details panel loads exactly one cover, on an explicit button press, through
//! [`RommOperation`](crate::RommOperation). That queue holds one operation at a
//! time and *drops* anything asked for while something else runs
//! (`start_romm_operation` returns `false`), because it also carries imports and
//! hash verifications, which must not overlap. A scrolling list of games needs many
//! covers at once and must never block a mount behind a thumbnail, so it cannot use
//! that queue - which is precisely why Gamer View drew platform icons and never a
//! RomM cover.
//!
//! Nothing here is a second downloader or a second image cache. Fetching,
//! validation, thumbnailing, bounding and on-disk storage all remain
//! [`archivefs_core::identity_source::artwork::ArtworkCache`]'s job, and decoding
//! remains [`crate::romm_game::decode_thumbnail`]'s. What this module adds is the
//! scheduling a list needs: which records to ask about, in what order, how many at
//! once, and how to hold the answers so scrolling back does not ask again.
//!
//! # Only RomM's own artwork
//!
//! The same rule the core enforces applies here and is checked again before a
//! request is made: only `path_cover_small`, a path on the approved RomM instance,
//! is ever fetched. `url_cover` points at IGDB or RetroAchievements and is recorded
//! for provenance only - a record carrying nothing else resolves to
//! [`NoCover::PublicOnly`] and draws the placeholder without any request being
//! made.
//!
//! # Answers belong to records, not to rows
//!
//! `egui`'s `show_rows` reuses row positions as the list scrolls, so anything
//! stored per row index would eventually be drawn beside a different game. Every
//! answer here is keyed by the record's own local path and carries the
//! `provider_game_id` it was resolved for; a row draws a cover only when both match
//! the record it is drawing. A reply for a library generation that has since been
//! replaced is discarded rather than stored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui;

use crate::ui::theme;

/// How many rows beyond the visible window are asked for, on each side.
///
/// Enough that a steady scroll usually finds the next cover already decoded, small
/// enough that a flick through a 13,891-record library asks about tens of records
/// rather than thousands.
pub(crate) const LOOK_AHEAD_ROWS: usize = 6;

/// The most new records one frame may ask about.
///
/// A jump to the far end of the list would otherwise queue the whole newly visible
/// window at once. The remainder is asked for on the following frames, which is
/// still faster than a person can read them.
pub(crate) const MAX_REQUESTS_PER_FRAME: usize = 6;

/// How many answers are held in memory at once.
///
/// Bounds texture memory, and is the reason a large library cannot accumulate work
/// or pixels for every record. Well above any window a person can see, so scrolling
/// away and back within a screenful or two never costs a second request.
pub(crate) const MAX_TRACKED_COVERS: usize = 256;

/// The box a row's cover is drawn inside, matching the existing artwork slot so
/// adding covers changes no row's height.
pub(crate) const COVER_BOX: f32 = 56.0;

/// The most selected-game requests one frame may emit.
///
/// One, because there is only ever one selected game - and stating it as a ceiling
/// is what makes the fairness argument checkable: a burst of selection changes can
/// take at most one of the frame's [`MAX_REQUESTS_PER_FRAME`] slots, so visible
/// rows always keep the rest.
pub(crate) const MAX_SELECTED_REQUESTS_PER_FRAME: usize = 1;

/// How many higher-priority jobs the worker may serve before it must take a lower
/// one.
///
/// Priority alone is not fairness: a selection the user keeps changing would sit at
/// the head of the queue forever and the visible rows behind it would never be
/// read. After this many, the oldest lower-priority job goes next regardless.
pub(crate) const FAIRNESS_RUN: u32 = 4;

/// The most jobs the worker will hold. Beyond it the lowest-priority, oldest work
/// is dropped - it describes rows that are no longer on screen, and the UI re-asks
/// for anything it still wants on the next frame it draws.
pub(crate) const MAX_QUEUED_JOBS: usize = 256;

/// What a cover request is for, in the order it deserves to be served.
///
/// The selected game is what a person is looking at, so it goes first even when a
/// long look-ahead backlog is already queued - which FIFO alone would not give.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CoverPriority {
    Selected,
    Visible,
    LookAhead,
}

/// Why a row has no cover to draw.
///
/// Every variant draws the same placeholder; the distinction exists so the reason
/// can be stated in a tooltip rather than leaving a person guessing, and so tests
/// can tell "RomM has no artwork for this" apart from "this failed to load".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoCover {
    /// The file is not in the imported RomM catalogue at all.
    NoRommIdentity,
    /// RomM has the record but recorded no artwork for it.
    NoArtwork,
    /// RomM recorded only a scraper URL on a public host. Never fetched.
    PublicOnly,
    /// Nothing cached, and RomM could not be reached or is not configured.
    Unavailable,
    /// A request or a decode was refused or failed.
    Failed,
}

impl NoCover {
    /// What a person is told when they hover the placeholder. Never carries a URL,
    /// a path or a token, matching the core's own wording rules.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::NoRommIdentity => "No RomM identity for this file, so no cover is available.",
            Self::NoArtwork => "RomM recorded no artwork for this game.",
            Self::PublicOnly => {
                "Public artwork reference recorded, but EmuWiz does not fetch from public hosts."
            }
            Self::Unavailable => "No cached cover, and RomM was not reachable.",
            Self::Failed => "The cover could not be loaded.",
        }
    }
}

/// What resolving one record produced.
#[derive(Clone, Debug)]
pub(crate) enum CoverAnswer {
    /// Decoded pixels, ready for the UI thread to upload. Decoding happens on the
    /// worker: the upload is cheap, the decode is not.
    Ready(Box<crate::romm_game::CoverImage>),
    /// The record still resolves to the cover the caller already holds decoded, so
    /// nothing was read and nothing was decoded.
    ///
    /// This is what makes an identity refresh cheap. The key is a digest of the
    /// server, the `provider_game_id` and RomM's own artwork identity, so a key that
    /// still matches *is* proof that both the record's identity and its artwork are
    /// unchanged - there is no way to answer `Unchanged` for a record whose provider
    /// id moved.
    Unchanged {
        key: String,
    },
    None(NoCover),
}

/// One record to resolve, and what the caller already has for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CoverJob {
    pub(crate) local_path: PathBuf,
    pub(crate) priority: CoverPriority,
    /// The cover key already decoded for this record, when one is being
    /// revalidated after an identity refresh. Lets the worker answer
    /// [`CoverAnswer::Unchanged`] instead of reading and decoding the thumbnail
    /// again.
    pub(crate) held_key: Option<String>,
}

/// One answer, bound to what asked for it.
///
/// `generation` and `provider_game_id` are what make a late answer safe: an answer
/// from a replaced library is dropped, and an answer is only ever drawn beside the
/// record whose id it names.
#[derive(Clone, Debug)]
pub(crate) struct CoverReply {
    pub(crate) generation: u64,
    pub(crate) local_path: PathBuf,
    /// The RomM record this answers for, or `None` when the path has no RomM
    /// identity at all.
    pub(crate) provider_game_id: Option<String>,
    pub(crate) answer: CoverAnswer,
}

/// What one row's cover area is showing.
#[derive(Clone)]
pub(crate) enum CoverSlot {
    /// Asked for, not yet answered. The placeholder is drawn meanwhile, so the row
    /// never changes height when the answer arrives.
    Loading,
    Ready {
        texture: egui::TextureHandle,
        provider_game_id: String,
        /// The artwork cache key these pixels came from, offered back to the worker
        /// on revalidation so an unchanged record costs no decode.
        key: String,
    },
    /// Decoded before an identity refresh, not yet confirmed against the new
    /// catalogue.
    ///
    /// The texture is kept - that is what makes an unchanged record free - but the
    /// *placeholder* is drawn until the refreshed catalogue confirms this path still
    /// resolves to the same record. Drawing the held cover meanwhile is exactly the
    /// thing that would show one game's art beside another after a re-import.
    Revalidating {
        texture: egui::TextureHandle,
        provider_game_id: String,
        key: String,
        /// Whether the worker has already been asked to confirm this. Without it a
        /// revalidating row would be re-asked on every frame until its reply
        /// arrived, which is a request per frame per visible row.
        requested: bool,
    },
    None(NoCover),
}

/// Gamer View's scheduling and holding of covers.
///
/// Deliberately free of threads and of any network or disk access: it decides what
/// to ask for and what to keep, and is driven entirely through [`Self::visible`]
/// and [`Self::absorb`]. That is what lets every rule it enforces be tested without
/// a RomM instance.
#[derive(Default)]
pub(crate) struct GamerCoverCache {
    /// Bumped when the loaded library is replaced. Answers naming an older
    /// generation are discarded: the same path may now be a different file.
    generation: u64,
    slots: HashMap<PathBuf, CoverSlot>,
    /// The frame each slot was last drawn or asked for, for eviction order.
    last_used: HashMap<PathBuf, u64>,
    frame: u64,
}

impl GamerCoverCache {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Points the cache at a replaced library, discarding everything about the old
    /// one.
    ///
    /// Paths are the identity a slot is keyed by, and a reloaded library may map the
    /// same path to a different archive, so every slot and every texture goes. This
    /// is the only thing that clears the cache: a search or a platform change
    /// narrows which records are *visible* without changing what any of them is, so
    /// throwing their covers away would only cause the same covers to be fetched
    /// again.
    pub(crate) fn library_changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.slots.clear();
        self.last_used.clear();
    }

    /// What is drawn for one record right now, or `None` when nothing has been asked
    /// for it yet.
    ///
    /// A `Ready` slot is returned only when it names the record being drawn. That
    /// check is what makes a reused row position unable to inherit the previous
    /// occupant's cover.
    pub(crate) fn slot_for(
        &self,
        local_path: &Path,
        provider_game_id: Option<&str>,
    ) -> Option<&CoverSlot> {
        match self.slots.get(local_path)? {
            CoverSlot::Ready {
                provider_game_id: held,
                ..
            } if provider_game_id.is_some_and(|wanted| wanted != held) => None,
            slot => Some(slot),
        }
    }

    /// Points the cache at a refreshed RomM catalogue.
    ///
    /// Called when an import or a cache replacement succeeded, so what any path
    /// resolves to may have changed. Unlike [`Self::library_changed`] this keeps the
    /// decoded textures: an import usually leaves most records exactly as they were,
    /// and re-decoding thousands of thumbnails to discover that would be waste.
    ///
    /// What it does not keep is the *binding*. Every ready slot becomes
    /// [`CoverSlot::Revalidating`], which draws the placeholder until the new
    /// catalogue confirms the record, so a path whose provider id moved cannot show
    /// the previous record's cover even for a frame. Everything else is dropped so
    /// it is asked again - which is what lets a game that has just gained a RomM
    /// identity acquire artwork without a restart.
    ///
    /// The generation bump discards replies already in flight against the old
    /// catalogue.
    pub(crate) fn identity_refreshed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let mut retained = HashMap::with_capacity(self.slots.len());
        for (path, slot) in self.slots.drain() {
            match slot {
                CoverSlot::Ready {
                    texture,
                    provider_game_id,
                    key,
                }
                | CoverSlot::Revalidating {
                    texture,
                    provider_game_id,
                    key,
                    ..
                } => {
                    retained.insert(
                        path,
                        CoverSlot::Revalidating {
                            texture,
                            provider_game_id,
                            key,
                            // A second import supersedes the first, so anything
                            // already asked is asked again against the newer
                            // catalogue.
                            requested: false,
                        },
                    );
                }
                // A pending request and a "no cover" answer both described the old
                // catalogue. Dropping them is what makes a newly matched record
                // eligible on the very next frame it is visible.
                CoverSlot::Loading | CoverSlot::None(_) => {}
            }
        }
        self.slots = retained;
    }

    /// Declares the window of records on screen and returns the ones to ask about.
    ///
    /// `visible` is the rows the list is actually drawing; `look_ahead` extends it on
    /// both sides. Anything already asked for or already answered is not asked again,
    /// which is what makes scrolling away and back free. At most
    /// [`MAX_REQUESTS_PER_FRAME`] records are returned, so no single frame can queue
    /// a whole library.
    ///
    /// The returned paths are marked [`CoverSlot::Loading`] before they are handed
    /// out, so the same record cannot be asked for twice while an answer is in
    /// flight.
    pub(crate) fn visible(
        &mut self,
        selected: Option<&Path>,
        window: &[PathBuf],
        look_ahead: &[PathBuf],
    ) -> Vec<CoverJob> {
        self.frame = self.frame.wrapping_add(1);
        for path in selected
            .into_iter()
            .chain(window.iter().map(PathBuf::as_path))
        {
            self.last_used.insert(path.to_path_buf(), self.frame);
        }
        for path in look_ahead {
            self.last_used.insert(path.clone(), self.frame);
        }

        let mut wanted: Vec<CoverJob> = Vec::new();
        // Three passes in the order the results are wanted on screen. The selected
        // game is what a person is actually looking at, so it is asked for first
        // even when it is also an ordinary visible row; the look-ahead is asked for
        // last, and only with whatever is left of the frame's budget.
        let mut consider = |cache: &mut Self, path: &Path, priority: CoverPriority| {
            if wanted.len() >= MAX_REQUESTS_PER_FRAME {
                return;
            }
            if priority == CoverPriority::Selected
                && wanted
                    .iter()
                    .filter(|job| job.priority == CoverPriority::Selected)
                    .count()
                    >= MAX_SELECTED_REQUESTS_PER_FRAME
            {
                return;
            }
            if wanted.iter().any(|job| job.local_path == path) {
                return;
            }
            let held_key = match cache.slots.get(path) {
                // Already asked, or already answered. Nothing to do.
                Some(CoverSlot::Loading | CoverSlot::Ready { .. } | CoverSlot::None(_)) => return,
                // Already asked to confirm; waiting on the answer.
                Some(CoverSlot::Revalidating {
                    requested: true, ..
                }) => return,
                // Waiting on the refreshed catalogue. Offer the key it already
                // holds, so an unchanged record is confirmed without a decode.
                Some(CoverSlot::Revalidating { key, .. }) => Some(key.clone()),
                None => None,
            };
            wanted.push(CoverJob {
                local_path: path.to_path_buf(),
                priority,
                held_key,
            });
        };

        if let Some(path) = selected {
            consider(self, path, CoverPriority::Selected);
        }
        for path in window {
            consider(self, path, CoverPriority::Visible);
        }
        for path in look_ahead {
            consider(self, path, CoverPriority::LookAhead);
        }

        for job in &wanted {
            match self.slots.get_mut(&job.local_path) {
                // Marked as asked, keeping the texture that makes an unchanged
                // record free to confirm. Replacing it with `Loading` would throw
                // those pixels away.
                Some(CoverSlot::Revalidating { requested, .. }) => *requested = true,
                Some(_) => {}
                None => {
                    self.slots
                        .insert(job.local_path.clone(), CoverSlot::Loading);
                }
            }
        }
        self.evict();
        wanted
    }

    /// Takes one answer, uploading its pixels if it is still wanted.
    ///
    /// Returns whether the answer was kept. A stale generation is dropped here, and
    /// so is an answer for a record no longer tracked - both would otherwise hold a
    /// texture nothing will ever draw.
    pub(crate) fn absorb(&mut self, context: &egui::Context, reply: CoverReply) -> bool {
        if reply.generation != self.generation {
            return false;
        }
        if !self.slots.contains_key(&reply.local_path) {
            // Evicted while in flight. Keeping it would reintroduce an entry the
            // bound has already decided not to hold.
            return false;
        }
        let slot = match reply.answer {
            CoverAnswer::None(reason) => CoverSlot::None(reason),
            CoverAnswer::Unchanged { key } => {
                // The refreshed catalogue resolves this path to the same cover key,
                // which by construction means the same record and the same artwork.
                // The texture already held is promoted back to visible without a
                // read, a decode or an upload.
                let Some(CoverSlot::Revalidating {
                    texture,
                    provider_game_id,
                    key: held,
                    ..
                }) = self.slots.get(&reply.local_path)
                else {
                    // Nothing left to confirm - evicted, or already superseded.
                    return false;
                };
                if held != &key {
                    // Should not happen: the worker only answers `Unchanged` for the
                    // key it was offered. Refusing is the safe reading either way.
                    return false;
                }
                CoverSlot::Ready {
                    texture: texture.clone(),
                    provider_game_id: provider_game_id.clone(),
                    key,
                }
            }
            CoverAnswer::Ready(image) => {
                let Some(provider_game_id) = reply.provider_game_id else {
                    // A cover with no record to attach it to cannot be drawn safely.
                    return false;
                };
                CoverSlot::Ready {
                    texture: context.load_texture(
                        format!("archivefs-gamer-cover-{}", image.key),
                        image.image.clone(),
                        egui::TextureOptions::LINEAR,
                    ),
                    provider_game_id,
                    key: image.key.clone(),
                }
            }
        };
        self.slots.insert(reply.local_path, slot);
        true
    }

    /// Drops the least recently seen slots once the bound is exceeded.
    fn evict(&mut self) {
        if self.slots.len() <= MAX_TRACKED_COVERS {
            return;
        }
        let mut order: Vec<(u64, PathBuf)> = self
            .slots
            .keys()
            .map(|path| (self.last_used.get(path).copied().unwrap_or(0), path.clone()))
            .collect();
        order.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let excess = self.slots.len() - MAX_TRACKED_COVERS;
        for (_, path) in order.into_iter().take(excess) {
            self.slots.remove(&path);
            self.last_used.remove(&path);
        }
    }

    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.slots.len()
    }
}

// --- The featured cover -------------------------------------------------

/// How wide the featured panel's content column is allowed to get.
///
/// The panel itself is around 730px at 1920x1080, and a title, a status line and a
/// Mount button stretched across all of it read as a form rather than a feature.
/// Constraining the column keeps the block cohesive and keeps Mount emphatic
/// without becoming a banner.
pub(crate) const GAMER_FEATURED_CONTENT_MAX_WIDTH: f32 = 460.0;

/// The tallest a featured cover is drawn, so a 1440p panel does not turn one
/// thumbnail into a poster.
pub(crate) const FEATURED_COVER_MAX_HEIGHT: f32 = 300.0;

/// Below this there is not enough of an image left to be worth the space, and the
/// artwork is dropped rather than the actions.
pub(crate) const FEATURED_COVER_MIN_HEIGHT: f32 = 72.0;

/// The first-frame estimate of what the title, status and actions need beneath the
/// artwork.
///
/// Only an estimate, and only until the panel has drawn once: from then on the
/// caller measures the block and reserves its real height. That is what makes
/// "reduce the artwork before hiding the actions" true rather than hoped for - the
/// cover only ever gets what is genuinely left over, however the title wrapped.
pub(crate) const FEATURED_RESERVED_BELOW: f32 = 300.0;

/// A cover's portrait shape. Boxes are reserved at this ratio; the image inside is
/// fitted to it and letterboxed, never stretched or cropped.
pub(crate) const FEATURED_COVER_ASPECT: f32 = 2.0 / 3.0;

/// The box reserved for the featured cover, or `None` when there is not enough
/// height to give it any.
///
/// `budget` is the height left for the artwork *after* the caller has set aside
/// what the title, the status and the actions need - measured, not estimated. The
/// subtraction belongs there rather than here: doing it in both places takes it
/// twice, which shrinks the cover on a large window and hides it on a small one.
///
/// The box is the same size whether the artwork has loaded, failed, or does not
/// exist, which is what stops the actions beneath it moving.
pub(crate) fn featured_cover_box(panel_width: f32, budget: f32) -> Option<egui::Vec2> {
    // Capped so a 1440p panel does not turn one thumbnail into a poster, and
    // clamped to fit across the panel at its portrait ratio.
    let by_width = (panel_width - 2.0 * theme::PAGE_GUTTER).max(0.0) / FEATURED_COVER_ASPECT;
    let height = budget.min(by_width).min(FEATURED_COVER_MAX_HEIGHT);
    if height < FEATURED_COVER_MIN_HEIGHT {
        return None;
    }
    Some(egui::vec2(height * FEATURED_COVER_ASPECT, height))
}

/// Fits an image inside the reserved box, preserving its aspect ratio.
///
/// A portrait cover fills the box's height; a landscape or unusually shaped one
/// fills its width and is letterboxed top and bottom. Nothing is ever stretched,
/// and nothing is cropped away.
pub(crate) fn fit_within(box_size: egui::Vec2, image: egui::Vec2) -> egui::Vec2 {
    if image.x <= 0.0 || image.y <= 0.0 {
        return egui::Vec2::ZERO;
    }
    let scale = (box_size.x / image.x).min(box_size.y / image.y);
    image * scale
}

/// Extends a visible row range with look-ahead, clamped to the list.
///
/// Kept separate from the list rendering so the bound can be asserted directly:
/// this is what stops a 13,891-record library from being asked about.
pub(crate) fn look_ahead_range(
    visible: std::ops::Range<usize>,
    total: usize,
) -> std::ops::Range<usize> {
    let start = visible.start.saturating_sub(LOOK_AHEAD_ROWS);
    let end = visible.end.saturating_add(LOOK_AHEAD_ROWS).min(total);
    start..end.max(start)
}

// --- Resolving one record ------------------------------------------------

/// What may be done for one record, decided before anything is asked for.
///
/// Split out of [`RommCoverSource::resolve`] so the rule that matters most can be
/// asserted without a RomM instance: a record whose only artwork is a public
/// scraper URL has no plan that reaches a fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CoverPlan {
    /// Look in the cache, and request RomM's own `path_cover_small` if it misses.
    /// The only variant from which any request is possible.
    UseRommHostedCover,
    /// Draw the placeholder. No request is made.
    Placeholder(NoCover),
}

/// Decides what one record allows.
///
/// Delegates the artwork question to [`crate::romm_game::availability_of`], the
/// same classification the Details panel and the record browser use, so the three
/// cannot drift apart about what counts as fetchable.
pub(crate) fn plan_for(
    record: &archivefs_core::identity_source::model::ExternalIdentityRecord,
) -> CoverPlan {
    match crate::romm_game::availability_of(record) {
        crate::romm_game::ArtworkAvailability::Fetchable => CoverPlan::UseRommHostedCover,
        crate::romm_game::ArtworkAvailability::None => CoverPlan::Placeholder(NoCover::NoArtwork),
        crate::romm_game::ArtworkAvailability::PublicOnly => {
            CoverPlan::Placeholder(NoCover::PublicOnly)
        }
    }
}

/// Turns a local path into a cover, using the core's cache for everything.
///
/// Held by the worker thread and built once, because opening the identity cache and
/// indexing 13,891 records by path is work that must happen once per library rather
/// than once per row - `IdentityCache::record_for_path` is a linear scan, and one
/// per visible row per frame is a scan storm.
pub(crate) struct RommCoverSource {
    settings: archivefs_core::identity_source::settings::ProviderSettings,
    server_id: String,
    /// Path to the record that claims it. Built once, so a lookup is a hash probe.
    by_path: HashMap<PathBuf, archivefs_core::identity_source::model::ExternalIdentityRecord>,
    artwork: archivefs_core::identity_source::artwork::ArtworkCache,
    /// Validated lazily and at most once. A failure here degrades the list to
    /// cache-only rather than disabling it: covers already on disk still draw.
    source: Option<Result<archivefs_core::identity_source::romm::config::ValidatedRommSource, ()>>,
    transport: archivefs_core::identity_source::romm::client::UreqTransport,
    trusted_roots: Option<Vec<PathBuf>>,
}

/// The page size the catalogue is walked in.
///
/// [`IdentityCache::page`] deliberately clamps its limit, so there is no "give me
/// everything" call: asking for one enormous page silently returns only the first.
const CATALOGUE_PAGE: usize = 1_000;

/// Indexes every record in the catalogue by the local path it claims.
///
/// `IdentityCache::record_for_path` is a linear scan, and one per visible row per
/// frame over 36,259 records is a scan storm; this pays for it once per library.
///
/// Walked page by page on purpose. `page(0, usize::MAX)` looks like it reads the
/// whole catalogue and does not - it clamps, returns the first thousand records,
/// and leaves every game past them looking as though RomM had never heard of it.
pub(crate) fn index_by_path(
    cache: &archivefs_core::identity_source::cache::IdentityCache,
) -> HashMap<PathBuf, archivefs_core::identity_source::model::ExternalIdentityRecord> {
    let mut by_path = HashMap::new();
    let mut offset = 0;
    loop {
        let page = cache.page(offset, CATALOGUE_PAGE);
        if page.is_empty() {
            break;
        }
        for record in page {
            if let Some(path) = record.archivefs_path.as_deref() {
                by_path.insert(path.to_path_buf(), record.clone());
            }
        }
        offset += page.len();
    }
    by_path
}

impl RommCoverSource {
    /// Opens the published cache and indexes it. Touches no network.
    pub(crate) fn open(trusted_roots: Option<Vec<PathBuf>>) -> Result<Self, String> {
        use archivefs_core::identity_source::artwork::ArtworkCache;
        use archivefs_core::identity_source::hashing::LocalHashCache;
        use archivefs_core::identity_source::model::IdentityProvider;
        use archivefs_core::identity_source::settings::{SettingsLocation, default_identity_root};
        use archivefs_core::identity_source::status::IdentitySourceApi;

        let identity_root = default_identity_root()?;
        let settings = SettingsLocation::new(&identity_root, IdentityProvider::Romm)
            .load()
            .map_err(|error| error.detail())?;
        let api = IdentitySourceApi::new(&identity_root, IdentityProvider::Romm);
        let cache = api.open_cache(None).map_err(|refusal| refusal.detail())?;
        let status = api.status(&settings.source, &LocalHashCache::new(), false);
        let server_id = status
            .server_id
            .clone()
            .unwrap_or_else(|| settings.source.url.clone());

        Ok(Self {
            settings,
            server_id,
            by_path: index_by_path(&cache),
            artwork: ArtworkCache::new(&identity_root, IdentityProvider::Romm),
            source: None,
            transport: archivefs_core::identity_source::romm::client::UreqTransport::new(),
            trusted_roots,
        })
    }

    /// Resolves one record, reading the cache first and requesting only when it
    /// must.
    ///
    /// `held_key` is the cover key the caller already has decoded. When the record
    /// still resolves to it, the answer is [`CoverAnswer::Unchanged`] and no
    /// thumbnail is read or decoded at all.
    pub(crate) fn resolve(
        &mut self,
        generation: u64,
        local_path: &Path,
        held_key: Option<&str>,
    ) -> CoverReply {
        use archivefs_core::identity_source::artwork::{ArtworkCache, ArtworkRequest};

        let reply = |provider_game_id: Option<String>, answer: CoverAnswer| CoverReply {
            generation,
            local_path: local_path.to_path_buf(),
            provider_game_id,
            answer,
        };

        let Some(record) = self.by_path.get(local_path).cloned() else {
            return reply(None, CoverAnswer::None(NoCover::NoRommIdentity));
        };
        let game_id = record.provider_game_id.clone();

        // The same rule the core enforces, checked before anything is asked for: a
        // record whose only artwork is a public scraper URL produces a placeholder
        // and no request at all.
        match plan_for(&record) {
            CoverPlan::Placeholder(reason) => {
                return reply(Some(game_id), CoverAnswer::None(reason));
            }
            CoverPlan::UseRommHostedCover => {}
        }

        let request = ArtworkRequest::from_record(&record);
        let key = ArtworkCache::key_for(&self.server_id, &request);
        if held_key == Some(key.as_str()) {
            // The key is a digest of the server, the provider game id and RomM's own
            // artwork identity, so a match is proof that neither the record nor its
            // cover moved. The caller's pixels are still the right pixels.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs() as i64)
                .unwrap_or_default();
            // Kept warm in the eviction order even though nothing was read.
            self.artwork.touch(&self.server_id, &key, now);
            return reply(Some(game_id), CoverAnswer::Unchanged { key });
        }
        if let Some(thumbnail) = self.artwork.lookup(&self.server_id, &request) {
            return match crate::romm_game::decode_thumbnail(&thumbnail, true) {
                Ok(image) => reply(Some(game_id), CoverAnswer::Ready(Box::new(image))),
                Err(_) => reply(Some(game_id), CoverAnswer::None(NoCover::Failed)),
            };
        }

        if self.validated_source().is_none() {
            return reply(Some(game_id), CoverAnswer::None(NoCover::Unavailable));
        }
        // Re-borrowed immutably now that validation is settled, so the fetch can
        // read the source alongside the cache and the transport.
        let Some(Ok(source)) = self.source.as_ref() else {
            return reply(Some(game_id), CoverAnswer::None(NoCover::Unavailable));
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs() as i64)
            .unwrap_or_default();
        match self
            .artwork
            .fetch(source, &self.transport, &request, now, None)
        {
            Ok(thumbnail) => match crate::romm_game::decode_thumbnail(&thumbnail, false) {
                Ok(image) => reply(Some(game_id), CoverAnswer::Ready(Box::new(image))),
                Err(_) => reply(Some(game_id), CoverAnswer::None(NoCover::Failed)),
            },
            Err(refusal) => {
                use archivefs_core::identity_source::artwork::ArtworkRefusal;
                let reason = match refusal {
                    ArtworkRefusal::Request(_) | ArtworkRefusal::Cancelled => NoCover::Unavailable,
                    _ => NoCover::Failed,
                };
                reply(Some(game_id), CoverAnswer::None(reason))
            }
        }
    }

    /// Rebuilds the path index from the published catalogue as it is now.
    ///
    /// Returns whether it was replaced. A catalogue that cannot be reopened - a
    /// failed import that left no new cache, or an unreadable one - leaves the
    /// existing index in place: a refresh that cannot find anything better must not
    /// throw away something that works.
    pub(crate) fn reindex(&mut self) -> bool {
        use archivefs_core::identity_source::model::IdentityProvider;
        use archivefs_core::identity_source::settings::{SettingsLocation, default_identity_root};
        use archivefs_core::identity_source::status::IdentitySourceApi;

        let Ok(identity_root) = default_identity_root() else {
            return false;
        };
        let api = IdentitySourceApi::new(&identity_root, IdentityProvider::Romm);
        let Ok(cache) = api.open_cache(None) else {
            return false;
        };
        self.by_path = index_by_path(&cache);
        // Settings can have moved with the import - a changed mapping is exactly the
        // kind of thing a re-import follows - so the server identity is re-read too.
        // A failure here leaves the previous one, which is still the one the cached
        // thumbnails are keyed by.
        if let Ok(settings) = SettingsLocation::new(&identity_root, IdentityProvider::Romm).load() {
            let status = api.status(
                &settings.source,
                &archivefs_core::identity_source::hashing::LocalHashCache::new(),
                false,
            );
            self.server_id = status
                .server_id
                .clone()
                .unwrap_or_else(|| settings.source.url.clone());
            self.settings = settings;
            // The validated source is derived from those settings, so it is dropped
            // and re-established lazily rather than left describing the old ones.
            self.source = None;
        }
        true
    }

    /// Validates the source once. `None` means cache-only from here on.
    fn validated_source(
        &mut self,
    ) -> Option<&archivefs_core::identity_source::romm::config::ValidatedRommSource> {
        if self.source.is_none() {
            self.source = Some(self.validate());
        }
        self.source.as_ref().and_then(|result| result.as_ref().ok())
    }

    fn validate(
        &self,
    ) -> Result<archivefs_core::identity_source::romm::config::ValidatedRommSource, ()> {
        use archivefs_core::identity_source::settings::load_token_file;

        if !self.settings.source.enabled {
            return Err(());
        }
        let token = load_token_file(self.settings.source.token_path.as_deref()).map_err(|_| ())?;
        let roots = self.trusted_roots.as_deref().ok_or(())?;
        archivefs_core::identity_source::romm::config::ValidatedRommSource::validate(
            &self.settings.source,
            &token,
            roots,
            &archivefs_core::identity_source::net_policy::SystemResolver,
        )
        .map_err(|_| ())
    }
}

// --- The worker ----------------------------------------------------------

/// The thread covers are resolved on, and the channels to it.
///
/// One thread, deliberately. Covers are small and the artwork index is a single
/// shared file, so more threads would contend on it rather than finish sooner; one
/// thread is also a hard ceiling on how much work a fast scroll can cause. It is
/// entirely separate from `RommOperation`'s single slot, so a cover can never delay
/// a mount and a running import can never stop covers from drawing.
pub(crate) struct CoverWorker {
    requests: std::sync::mpsc::Sender<WorkerMessage>,
    replies: std::sync::mpsc::Receiver<CoverReply>,
}

/// One job waiting on the worker, with what it needs to be ordered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedJob {
    pub(crate) generation: u64,
    pub(crate) job: CoverJob,
    /// Arrival order, so ties break oldest-first and the choice is deterministic.
    pub(crate) sequence: u64,
}

/// Chooses the next job to resolve, with bounded fairness.
///
/// Normally the highest-priority job wins, oldest first among equals - that is what
/// puts a freshly selected game ahead of a look-ahead backlog already queued, which
/// plain FIFO delivery cannot do.
///
/// `served_high` counts how many jobs have been taken ahead of something lower.
/// Once it reaches [`FAIRNESS_RUN`], the oldest *lower*-priority job is taken
/// instead and the count resets. Without that, a person holding a key down to move
/// the selection would keep a selected-priority job at the head of the queue
/// forever and the visible rows behind it would never be read.
pub(crate) fn next_job(queue: &mut Vec<QueuedJob>, served_high: &mut u32) -> Option<QueuedJob> {
    if queue.is_empty() {
        return None;
    }
    let best = queue
        .iter()
        .map(|queued| queued.job.priority)
        .min()
        .expect("the queue is not empty");
    // The fairness release: something lower is waiting and the run has gone on long
    // enough, so it goes next whatever is at the head.
    let wanted =
        if *served_high >= FAIRNESS_RUN && queue.iter().any(|queued| queued.job.priority > best) {
            *served_high = 0;
            queue
                .iter()
                .map(|queued| queued.job.priority)
                .filter(|priority| *priority > best)
                .min()
                .expect("a lower priority was just observed")
        } else {
            if queue.iter().any(|queued| queued.job.priority > best) {
                *served_high += 1;
            } else {
                // Nothing is being held back, so nothing is being unfair to.
                *served_high = 0;
            }
            best
        };
    let index = queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| queued.job.priority == wanted)
        .min_by_key(|(_, queued)| queued.sequence)
        .map(|(index, _)| index)
        .expect("a job with the chosen priority");
    Some(queue.remove(index))
}

/// Drops the least valuable work once the queue is over its bound.
///
/// Lowest priority and oldest first: those describe rows that have most likely
/// scrolled away, and the UI re-asks for anything it still wants on the next frame
/// it draws.
pub(crate) fn trim_queue(queue: &mut Vec<QueuedJob>) {
    if queue.len() <= MAX_QUEUED_JOBS {
        return;
    }
    queue.sort_by(|left, right| {
        left.job
            .priority
            .cmp(&right.job.priority)
            .then_with(|| right.sequence.cmp(&left.sequence))
    });
    queue.truncate(MAX_QUEUED_JOBS);
}

/// What the UI thread asks the worker to do.
enum WorkerMessage {
    Resolve {
        generation: u64,
        job: CoverJob,
    },
    /// Reopen the catalogue and rebuild the path index.
    ///
    /// Sent when a RomM import or cache replacement succeeded, and only then - the
    /// index is not rebuilt on a timer or per frame, because on this library it is
    /// 36,259 records and rebuilding it speculatively would be the reload storm this
    /// message exists to avoid.
    Reindex,
}

impl CoverWorker {
    /// Starts the worker. Opening the catalogue happens on the thread, so a large
    /// library never delays the first frame.
    pub(crate) fn start(context: egui::Context, trusted_roots: Option<Vec<PathBuf>>) -> Self {
        let (request_sender, request_receiver) = std::sync::mpsc::channel::<WorkerMessage>();
        let (reply_sender, reply_receiver) = std::sync::mpsc::channel::<CoverReply>();
        std::thread::spawn(move || {
            let mut source: Option<RommCoverSource> = None;
            let mut opened = false;
            let mut queue: Vec<QueuedJob> = Vec::new();
            let mut served_high = 0_u32;
            let mut sequence = 0_u64;
            loop {
                // Block for work, then take everything else that has arrived. The
                // drain is what makes prioritising possible at all: with one job
                // read at a time the order is whatever the channel delivered, and a
                // freshly selected game would sit behind a look-ahead backlog.
                let mut messages = Vec::new();
                if queue.is_empty() {
                    match request_receiver.recv() {
                        Ok(message) => messages.push(message),
                        Err(_) => return,
                    }
                }
                messages.extend(request_receiver.try_iter());

                for message in messages {
                    match message {
                        WorkerMessage::Reindex => {
                            match source.as_mut() {
                                // Reindexed in place, keeping the current index if
                                // the new catalogue cannot be read.
                                Some(source) => {
                                    source.reindex();
                                }
                                // Never opened, or the first open failed. An import
                                // may have created what was missing, so try again.
                                None => {
                                    opened = true;
                                    source = RommCoverSource::open(trusted_roots.clone()).ok();
                                }
                            }
                            // Everything queued was resolved against the previous
                            // catalogue's generation and has been superseded. The UI
                            // has already moved its slots to `Revalidating` and
                            // re-asks for what is on screen.
                            queue.clear();
                            context.request_repaint();
                        }
                        WorkerMessage::Resolve { generation, job } => {
                            sequence = sequence.wrapping_add(1);
                            queue.push(QueuedJob {
                                generation,
                                job,
                                sequence,
                            });
                        }
                    }
                }
                trim_queue(&mut queue);

                let Some(queued) = next_job(&mut queue, &mut served_high) else {
                    continue;
                };
                if !opened {
                    opened = true;
                    source = RommCoverSource::open(trusted_roots.clone()).ok();
                }
                let reply = match source.as_mut() {
                    Some(source) => source.resolve(
                        queued.generation,
                        &queued.job.local_path,
                        queued.job.held_key.as_deref(),
                    ),
                    // The catalogue itself could not be opened - RomM has never been
                    // imported, or the published cache is unreadable. That is not the
                    // same as a record having no identity, and saying so would send
                    // someone looking for the wrong problem.
                    None => CoverReply {
                        generation: queued.generation,
                        local_path: queued.job.local_path,
                        provider_game_id: None,
                        answer: CoverAnswer::None(NoCover::Unavailable),
                    },
                };
                if reply_sender.send(reply).is_err() {
                    return;
                }
                context.request_repaint();
            }
        });
        Self {
            requests: request_sender,
            replies: reply_receiver,
        }
    }

    /// Asks about one record. Dropped silently if the worker has gone.
    pub(crate) fn request(&self, generation: u64, job: CoverJob) {
        let _ = self
            .requests
            .send(WorkerMessage::Resolve { generation, job });
    }

    /// Asks the worker to reopen the catalogue after a successful import.
    pub(crate) fn reindex(&self) {
        let _ = self.requests.send(WorkerMessage::Reindex);
    }

    /// Every answer that has arrived. Never blocks, so the UI thread never waits on
    /// a decode or a request.
    pub(crate) fn drain(&self) -> Vec<CoverReply> {
        self.replies.try_iter().collect()
    }
}

#[cfg(test)]
#[path = "gamer_artwork/tests.rs"]
mod tests;
