//! What Gamer View's cover scheduling must never get wrong.
//!
//! None of these open a catalogue, touch a disk cache or contact anything: the
//! scheduling is a pure state machine driven through `visible` and `absorb`, and
//! the one rule that decides whether a request is even possible is
//! [`plan_for`], which reads a record and nothing else.

use super::*;
use archivefs_core::identity_source::cache::IdentityCache;
use archivefs_core::identity_source::model::{
    ArtworkReference, ExternalIdentityRecord, ExternalVerification, IdentityProvider,
};

const SERVER: &str = "https://romm.example";

fn record(id: &str, artwork: Option<ArtworkReference>) -> ExternalIdentityRecord {
    ExternalIdentityRecord {
        provider: IdentityProvider::Romm,
        server_id: SERVER.to_string(),
        provider_platform_id: Some("7".to_string()),
        provider_game_id: id.to_string(),
        provider_file_id: None,
        provider_path: format!("roms/snes/{id}.sfc"),
        archivefs_path: Some(PathBuf::from(format!("/roms/{id}.sfc"))),
        title: Some(format!("Game {id}")),
        platform_candidate: Some("SNES".to_string()),
        provider_platform_name: Some("Super Nintendo".to_string()),
        regions: Vec::new(),
        revision: None,
        hashes: Vec::new(),
        file_size_bytes: Some(1024),
        metadata_provider_ids: Vec::new(),
        artwork,
        related_files: Vec::new(),
        sibling_game_ids: Vec::new(),
        imported_at_unix_seconds: 0,
        provider_updated_at: None,
        evidence: Vec::new(),
        verification: ExternalVerification::Unmatched,
        conflicts: Vec::new(),
    }
}

/// A cover RomM hosts itself: `path_cover_small` is set.
fn romm_hosted() -> ArtworkReference {
    ArtworkReference {
        reference: "https://images.igdb.com/igdb/image/upload/t_cover_big/co1234.png".to_string(),
        small_reference: Some(
            "/assets/romm/resources/roms/149/1/cover/small.png?ts=17".to_string(),
        ),
    }
}

/// A record scraped from a public host and nothing else: `url_cover` only.
fn public_only() -> ArtworkReference {
    ArtworkReference {
        reference: "https://images.igdb.com/igdb/image/upload/t_cover_big/co1234.png".to_string(),
        small_reference: None,
    }
}

fn path(id: &str) -> PathBuf {
    PathBuf::from(format!("/roms/{id}.sfc"))
}

/// A decoded cover, as the worker would hand one over. Tiny: these tests are
/// about which record it lands on, never about its pixels.
fn image(key: &str) -> Box<crate::romm_game::CoverImage> {
    Box::new(crate::romm_game::CoverImage {
        key: key.to_string(),
        width: 2,
        height: 3,
        bytes: 24,
        image: egui::ColorImage::new([2, 3], vec![egui::Color32::from_rgb(10, 20, 30); 6]),
        from_cache: true,
    })
}

fn job_paths(jobs: &[CoverJob]) -> Vec<PathBuf> {
    jobs.iter().map(|job| job.local_path.clone()).collect()
}

fn ready(generation: u64, id: &str) -> CoverReply {
    CoverReply {
        generation,
        local_path: path(id),
        provider_game_id: Some(id.to_string()),
        answer: CoverAnswer::Ready(image(id)),
    }
}

fn placeholder(generation: u64, id: &str, reason: NoCover) -> CoverReply {
    CoverReply {
        generation,
        local_path: path(id),
        provider_game_id: Some(id.to_string()),
        answer: CoverAnswer::None(reason),
    }
}

fn context() -> egui::Context {
    egui::Context::default()
}

// --- Which source is allowed ---------------------------------------------

#[test]
fn a_matched_romm_record_uses_its_approved_cover_source() {
    // `path_cover_small` is present, so the only variant that can lead to a
    // request is the one chosen.
    assert_eq!(
        plan_for(&record("101", Some(romm_hosted()))),
        CoverPlan::UseRommHostedCover
    );
}

#[test]
fn a_public_url_cover_is_never_fetched() {
    // The record carries a perfectly usable IGDB URL. It is still not a fetch
    // target: the plan is a placeholder, and `resolve` returns before it reaches
    // the cache or the transport. This is the rule the whole module exists under.
    assert_eq!(
        plan_for(&record("102", Some(public_only()))),
        CoverPlan::Placeholder(NoCover::PublicOnly)
    );
}

#[test]
fn a_record_without_artwork_uses_the_placeholder() {
    assert_eq!(
        plan_for(&record("103", None)),
        CoverPlan::Placeholder(NoCover::NoArtwork)
    );
}

#[test]
fn every_placeholder_reason_explains_itself_without_leaking_a_reference() {
    for reason in [
        NoCover::NoRommIdentity,
        NoCover::NoArtwork,
        NoCover::PublicOnly,
        NoCover::Unavailable,
        NoCover::Failed,
    ] {
        let text = reason.describe();
        assert!(!text.is_empty(), "{reason:?} explains nothing");
        // The wording rule the core holds itself to: no URL, no path, no token.
        assert!(!text.contains("http"), "{reason:?} leaked a URL");
        assert!(!text.contains('/'), "{reason:?} leaked a path");
    }
}

// --- What gets asked for -------------------------------------------------

#[test]
fn only_the_visible_window_and_its_look_ahead_are_requested() {
    // The shape of a 13,891-record library: the range `show_rows` reports is the
    // viewport's, and the look-ahead extends it by a fixed few rows - never by a
    // fraction of the library.
    let total = 13_891;
    let wanted = look_ahead_range(400..420, total);
    assert_eq!(wanted, (400 - LOOK_AHEAD_ROWS)..(420 + LOOK_AHEAD_ROWS));
    assert!(
        wanted.len() <= 20 + 2 * LOOK_AHEAD_ROWS,
        "the window grew with the library"
    );
}

#[test]
fn the_look_ahead_is_clamped_at_both_ends_of_the_list() {
    assert_eq!(look_ahead_range(0..5, 5), 0..5);
    assert_eq!(look_ahead_range(0..0, 0), 0..0);
    let near_end = look_ahead_range(90..100, 100);
    assert_eq!(near_end.end, 100, "the look-ahead ran past the last row");
}

#[test]
fn a_single_frame_cannot_queue_a_whole_library() {
    let mut cache = GamerCoverCache::default();
    let window: Vec<PathBuf> = (0..13_891).map(|id| path(&id.to_string())).collect();
    let asked = cache.visible(None, &window, &[]);
    assert_eq!(
        asked.len(),
        MAX_REQUESTS_PER_FRAME,
        "one frame queued more than its share of a large library"
    );
}

#[test]
fn scrolling_away_and_back_does_not_ask_again() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    let window: Vec<PathBuf> = ["1", "2", "3"].iter().map(|id| path(id)).collect();

    let first = cache.visible(None, &window, &[]);
    assert_eq!(first.len(), 3);
    for id in ["1", "2", "3"] {
        assert!(cache.absorb(&context, ready(cache.generation(), id)));
    }

    // Scrolled away...
    let elsewhere: Vec<PathBuf> = ["4", "5"].iter().map(|id| path(id)).collect();
    cache.visible(None, &elsewhere, &[]);
    // ...and back. Nothing is asked for a second time.
    assert!(
        cache.visible(None, &window, &[]).is_empty(),
        "returning to loaded rows caused fresh requests"
    );
}

#[test]
fn a_record_in_flight_is_not_asked_for_twice() {
    let mut cache = GamerCoverCache::default();
    let window = vec![path("1")];
    assert_eq!(cache.visible(None, &window, &[]).len(), 1);
    // Nothing has answered yet; the second frame must stay quiet.
    assert!(
        cache.visible(None, &window, &[]).is_empty(),
        "a request in flight was duplicated"
    );
}

#[test]
fn what_is_held_stays_bounded_for_a_large_library() {
    let mut cache = GamerCoverCache::default();
    // Walk a long way through a library, a screenful at a time.
    for start in (0..6_000).step_by(10) {
        let window: Vec<PathBuf> = (start..start + 10)
            .map(|id| path(&id.to_string()))
            .collect();
        cache.visible(None, &window, &[]);
    }
    assert!(
        cache.tracked() <= MAX_TRACKED_COVERS,
        "held {} covers, above the {MAX_TRACKED_COVERS} bound",
        cache.tracked()
    );
}

// --- Stale and misattributed answers -------------------------------------

#[test]
fn a_stale_artwork_result_is_discarded() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    let in_flight = ready(cache.generation(), "1");

    // The library is replaced while that answer is on its way.
    cache.library_changed();

    assert!(
        !cache.absorb(&context, in_flight),
        "an answer from the previous library was kept"
    );
    assert!(
        cache.slot_for(&path("1"), None).is_none(),
        "a discarded answer still left something to draw"
    );
}

#[test]
fn an_answer_for_an_evicted_record_is_dropped_rather_than_reinstated() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    let in_flight = ready(cache.generation(), "1");

    // Pushed out by a long scroll before the answer arrived.
    for start in (0..3_000).step_by(10) {
        let window: Vec<PathBuf> = (start..start + 10)
            .map(|id| path(&format!("far{id}")))
            .collect();
        cache.visible(None, &window, &[]);
    }
    assert!(cache.slot_for(&path("1"), None).is_none());
    assert!(
        !cache.absorb(&context, in_flight),
        "an evicted record's answer was reinstated past the bound"
    );
}

#[test]
fn a_reused_row_position_cannot_inherit_another_records_cover() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    // Row position 0 first holds record "1"...
    cache.visible(None, &[path("1")], &[]);
    assert!(cache.absorb(&context, ready(cache.generation(), "1")));
    // ...and after a scroll the same position holds record "2", which has not
    // answered yet. Nothing is keyed by position, so "2" has no cover at all
    // rather than "1"'s.
    cache.visible(None, &[path("2")], &[]);
    assert!(
        matches!(cache.slot_for(&path("2"), None), Some(CoverSlot::Loading)),
        "a reused row position produced something other than a pending slot"
    );
    // And "1"'s cover is still "1"'s.
    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::Ready { .. })
    ));
}

#[test]
fn a_cover_is_only_drawn_for_the_record_id_it_was_resolved_for() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    assert!(cache.absorb(&context, ready(cache.generation(), "1")));

    // The caller that knows the record id gets the cover only when it agrees.
    assert!(cache.slot_for(&path("1"), Some("1")).is_some());
    assert!(
        cache.slot_for(&path("1"), Some("999")).is_none(),
        "a cover was offered for a record it does not belong to"
    );
}

#[test]
fn a_cover_with_no_record_to_attach_it_to_is_refused() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    let orphan = CoverReply {
        generation: cache.generation(),
        local_path: path("1"),
        provider_game_id: None,
        answer: CoverAnswer::Ready(image("1")),
    };
    assert!(
        !cache.absorb(&context, orphan),
        "a cover with no record identity was accepted"
    );
}

// --- Search and platform changes -----------------------------------------

#[test]
fn narrowing_the_list_keeps_each_cover_with_its_own_record() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    // The unfiltered list loads three games.
    let all: Vec<PathBuf> = ["1", "2", "3"].iter().map(|id| path(id)).collect();
    cache.visible(None, &all, &[]);
    for id in ["1", "2", "3"] {
        assert!(cache.absorb(&context, ready(cache.generation(), id)));
    }

    // A search, or a platform card, narrows it to one row. That row is record
    // "3", and it draws "3"'s cover - not the one that used to occupy row 0.
    cache.visible(None, &[path("3")], &[]);
    let Some(CoverSlot::Ready {
        provider_game_id, ..
    }) = cache.slot_for(&path("3"), None)
    else {
        panic!("the narrowed row lost its cover");
    };
    assert_eq!(provider_game_id, "3");
}

#[test]
fn a_search_or_platform_change_does_not_refetch_what_is_already_loaded() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    let all: Vec<PathBuf> = ["1", "2", "3"].iter().map(|id| path(id)).collect();
    cache.visible(None, &all, &[]);
    for id in ["1", "2", "3"] {
        assert!(cache.absorb(&context, ready(cache.generation(), id)));
    }
    // Narrow, then widen again. Neither costs a request: the covers describe
    // records, and no record changed.
    assert!(cache.visible(None, &[path("2")], &[]).is_empty());
    assert!(cache.visible(None, &all, &[]).is_empty());
}

// --- Failure ------------------------------------------------------------

#[test]
fn a_failed_load_settles_into_a_placeholder_rather_than_retrying() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    assert!(cache.absorb(
        &context,
        placeholder(cache.generation(), "1", NoCover::Failed)
    ));

    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::None(NoCover::Failed))
    ));
    // A failure is an answer. Redrawing the same row does not ask again, so a
    // record RomM cannot serve does not become a request every frame.
    assert!(
        cache.visible(None, &[path("1")], &[]).is_empty(),
        "a failed record was requested again on the next frame"
    );
}

#[test]
fn a_pending_slot_exists_from_the_moment_a_record_is_asked_about() {
    // What keeps a row's height stable: there is never a gap between asking and
    // having something to draw, because Loading and the placeholder occupy the
    // same box.
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::Loading)
    ));
}

// --- Indexing the catalogue ----------------------------------------------

fn catalogue(records: Vec<ExternalIdentityRecord>) -> IdentityCache {
    IdentityCache {
        format_version: archivefs_core::identity_source::cache::CACHE_FORMAT_VERSION,
        provider: IdentityProvider::Romm,
        server_id: SERVER.to_string(),
        server_version: None,
        source_fingerprint: "fingerprint".to_string(),
        imported_at_unix_seconds: 0,
        platforms: Vec::new(),
        records,
        rejected_hashes: Vec::new(),
        unknown_platforms: Vec::new(),
        server_reported_total: None,
    }
}

#[test]
fn every_record_is_indexed_not_only_the_first_page() {
    // `IdentityCache::page` clamps its limit, so a single "give me everything"
    // call returns one page and quietly loses the rest. On the real 36,259-record
    // catalogue that meant 35,259 games reporting no RomM identity while their
    // covers sat on the server - the whole feature silently doing nothing past
    // the first thousand records.
    let records: Vec<ExternalIdentityRecord> = (0..3_500)
        .map(|id| record(&format!("{id:06}"), Some(romm_hosted())))
        .collect();
    let index = index_by_path(&catalogue(records));

    assert_eq!(index.len(), 3_500, "the catalogue walk stopped early");
    // Specifically past the first page, which is where the bug lived.
    for id in ["000000", "000999", "001000", "002500", "003499"] {
        assert!(
            index.contains_key(&path(id)),
            "record {id} was left out of the index"
        );
    }
}

#[test]
fn an_indexed_record_keeps_its_own_identity_and_artwork() {
    let index = index_by_path(&catalogue(vec![
        record("100", Some(romm_hosted())),
        record("200", Some(public_only())),
    ]));
    assert_eq!(index[&path("100")].provider_game_id, "100");
    assert_eq!(
        plan_for(&index[&path("100")]),
        CoverPlan::UseRommHostedCover
    );
    assert_eq!(
        plan_for(&index[&path("200")]),
        CoverPlan::Placeholder(NoCover::PublicOnly)
    );
}

#[test]
fn a_catalogue_with_no_mapped_paths_indexes_nothing_rather_than_guessing() {
    let mut unmapped = record("100", Some(romm_hosted()));
    unmapped.archivefs_path = None;
    assert!(index_by_path(&catalogue(vec![unmapped])).is_empty());
}

// --- In-session identity refresh -----------------------------------------
//
// The worker's path-to-record index is built once, so before this a RomM import
// during a session was invisible to Gamer View until a restart. The refresh has to
// make newly matched records eligible without discarding what is still valid, and
// without ever letting a path whose provider id moved keep the previous record's
// cover.

/// A reply confirming the caller's own key still applies.
fn unchanged(generation: u64, id: &str, key: &str) -> CoverReply {
    CoverReply {
        generation,
        local_path: path(id),
        provider_game_id: Some(id.to_string()),
        answer: CoverAnswer::Unchanged {
            key: key.to_string(),
        },
    }
}

/// A ready reply naming an explicit cover key.
fn ready_with_key(generation: u64, id: &str, key: &str) -> CoverReply {
    CoverReply {
        generation,
        local_path: path(id),
        provider_game_id: Some(id.to_string()),
        answer: CoverAnswer::Ready(image(key)),
    }
}

fn loaded(context: &egui::Context, cache: &mut GamerCoverCache, id: &str, key: &str) {
    cache.visible(None, &[path(id)], &[]);
    assert!(cache.absorb(context, ready_with_key(cache.generation(), id, key)));
}

#[test]
fn a_row_with_no_identity_becomes_eligible_after_an_import() {
    // The whole point: a game RomM had never heard of at start-up must be able to
    // acquire artwork mid-session, without a restart.
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    assert!(cache.absorb(
        &context,
        placeholder(cache.generation(), "1", NoCover::NoRommIdentity)
    ));
    // Settled: nothing is asked again while the catalogue says the same thing.
    assert!(cache.visible(None, &[path("1")], &[]).is_empty());

    cache.identity_refreshed();

    let asked = cache.visible(None, &[path("1")], &[]);
    assert_eq!(
        job_paths(&asked),
        vec![path("1")],
        "a previously unmatched row was not re-asked after the import"
    );
    // And it can now be answered with a real cover.
    assert!(cache.absorb(&context, ready_with_key(cache.generation(), "1", "k1")));
    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::Ready { .. })
    ));
}

#[test]
fn an_unchanged_record_keeps_its_decoded_thumbnail() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");
    let before = match cache.slot_for(&path("1"), None) {
        Some(CoverSlot::Ready { texture, .. }) => texture.id(),
        other => panic!("expected a ready cover, got {:?}", other.is_some()),
    };

    cache.identity_refreshed();
    // The texture is retained, and the request offers its key back so the worker can
    // confirm without reading or decoding anything.
    let asked = cache.visible(None, &[path("1")], &[]);
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].held_key.as_deref(),
        Some("key-1"),
        "the held key was not offered for revalidation"
    );

    assert!(cache.absorb(&context, unchanged(cache.generation(), "1", "key-1")));
    let after = match cache.slot_for(&path("1"), None) {
        Some(CoverSlot::Ready { texture, .. }) => texture.id(),
        _ => panic!("the confirmed cover was not restored"),
    };
    assert_eq!(
        before, after,
        "the thumbnail was re-uploaded rather than retained"
    );
}

#[test]
fn a_record_being_revalidated_draws_the_placeholder_not_the_old_cover() {
    // The window between the import and the confirmation is exactly when a path
    // whose provider id moved would otherwise still be showing the old game's art.
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");

    cache.identity_refreshed();
    assert!(
        matches!(
            cache.slot_for(&path("1"), None),
            Some(CoverSlot::Revalidating { .. })
        ),
        "a refreshed record stayed Ready, so its old cover would still be drawn"
    );
}

#[test]
fn a_changed_provider_id_cannot_inherit_the_former_cover() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-old");
    let old_texture = match cache.slot_for(&path("1"), None) {
        Some(CoverSlot::Ready { texture, .. }) => texture.id(),
        _ => panic!("expected a ready cover"),
    };

    cache.identity_refreshed();
    cache.visible(None, &[path("1")], &[]);
    // The import moved this path to a different RomM record, so the worker resolves
    // a different key and answers with real pixels rather than `Unchanged`.
    assert!(cache.absorb(&context, ready_with_key(cache.generation(), "1", "key-new")));

    let Some(CoverSlot::Ready { texture, key, .. }) = cache.slot_for(&path("1"), None) else {
        panic!("the record did not resolve to its new cover");
    };
    assert_eq!(key, "key-new");
    assert_ne!(
        texture.id(),
        old_texture,
        "the new record is drawing the former identity's texture"
    );
}

#[test]
fn an_unchanged_reply_for_a_key_that_no_longer_matches_is_refused() {
    // Defence in depth: the worker only ever answers `Unchanged` for the key it was
    // offered, but a reply claiming a different one must not silently promote the
    // wrong pixels.
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");
    cache.identity_refreshed();
    cache.visible(None, &[path("1")], &[]);

    assert!(
        !cache.absorb(
            &context,
            unchanged(cache.generation(), "1", "some-other-key")
        ),
        "an Unchanged reply naming a different key was accepted"
    );
    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::Revalidating { .. })
    ));
}

#[test]
fn a_refresh_discards_replies_already_in_flight() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(None, &[path("1")], &[]);
    // Resolved against the catalogue as it was before the import.
    let in_flight = ready_with_key(cache.generation(), "1", "stale-key");

    cache.identity_refreshed();

    assert!(
        !cache.absorb(&context, in_flight),
        "a reply resolved against the previous catalogue was kept"
    );
}

#[test]
fn a_refresh_during_revalidation_does_not_lose_the_retained_texture() {
    // Two imports in quick succession. The second must not throw away the texture
    // the first was still waiting to confirm.
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");
    let original = match cache.slot_for(&path("1"), None) {
        Some(CoverSlot::Ready { texture, .. }) => texture.id(),
        _ => panic!("expected a ready cover"),
    };

    cache.identity_refreshed();
    cache.visible(None, &[path("1")], &[]);
    cache.identity_refreshed();

    let asked = cache.visible(None, &[path("1")], &[]);
    assert_eq!(asked[0].held_key.as_deref(), Some("key-1"));
    assert!(cache.absorb(&context, unchanged(cache.generation(), "1", "key-1")));
    let after = match cache.slot_for(&path("1"), None) {
        Some(CoverSlot::Ready { texture, .. }) => texture.id(),
        _ => panic!("the cover was not restored"),
    };
    assert_eq!(original, after, "the retained texture was lost");
}

#[test]
fn repeated_refreshes_do_not_queue_unbounded_work() {
    // A refresh re-asks only what is on screen, and still respects the per-frame
    // ceiling, so a burst of imports cannot turn into a request storm.
    let context = context();
    let mut cache = GamerCoverCache::default();
    let window: Vec<PathBuf> = (0..40).map(|id| path(&id.to_string())).collect();
    for _ in 0..8 {
        cache.visible(None, &window, &[]);
    }
    for id in 0..40 {
        let _ = cache.absorb(
            &context,
            ready_with_key(cache.generation(), &id.to_string(), &format!("k{id}")),
        );
    }

    for _ in 0..10 {
        cache.identity_refreshed();
        let asked = cache.visible(None, &window, &[]);
        assert!(
            asked.len() <= MAX_REQUESTS_PER_FRAME,
            "a refresh asked for {} records in one frame",
            asked.len()
        );
    }
    assert!(
        cache.tracked() <= MAX_TRACKED_COVERS,
        "repeated refreshes grew what is held"
    );
}

#[test]
fn a_refresh_does_not_re_ask_for_records_already_confirmed() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");

    cache.identity_refreshed();
    cache.visible(None, &[path("1")], &[]);
    assert!(cache.absorb(&context, unchanged(cache.generation(), "1", "key-1")));

    assert!(
        cache.visible(None, &[path("1")], &[]).is_empty(),
        "a confirmed record was asked about again"
    );
}

#[test]
fn a_revalidating_row_is_asked_about_once_not_once_per_frame() {
    // A `Revalidating` slot keeps its texture rather than becoming `Loading`, so
    // without an explicit "already asked" mark it would look unanswered on every
    // frame and produce one request per visible row per frame.
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");
    cache.identity_refreshed();

    assert_eq!(
        cache.visible(None, &[path("1")], &[]).len(),
        1,
        "the refreshed row was not asked about at all"
    );
    for frame in 0..30 {
        assert!(
            cache.visible(None, &[path("1")], &[]).is_empty(),
            "frame {frame} asked again while the confirmation was still in flight"
        );
    }
}

// --- Selected-game priority and fairness ---------------------------------

fn queued(priority: CoverPriority, id: &str, sequence: u64) -> QueuedJob {
    QueuedJob {
        generation: 0,
        job: CoverJob {
            local_path: path(id),
            priority,
            held_key: None,
        },
        sequence,
    }
}

#[test]
fn the_selected_game_is_requested_before_the_rows_around_it() {
    let mut cache = GamerCoverCache::default();
    // The selection is an ordinary visible row here, as it usually is.
    let window: Vec<PathBuf> = ["1", "2", "3"].iter().map(|id| path(id)).collect();
    let ahead: Vec<PathBuf> = ["4", "5"].iter().map(|id| path(id)).collect();

    let asked = cache.visible(Some(&path("3")), &window, &ahead);
    assert_eq!(
        asked[0].local_path,
        path("3"),
        "the selected game was not asked for first"
    );
    assert_eq!(asked[0].priority, CoverPriority::Selected);
    // And it is asked for exactly once, not again as an ordinary visible row.
    assert_eq!(
        asked
            .iter()
            .filter(|job| job.local_path == path("3"))
            .count(),
        1
    );
}

#[test]
fn a_selection_off_screen_is_still_requested_first() {
    let mut cache = GamerCoverCache::default();
    let window: Vec<PathBuf> = ["1", "2"].iter().map(|id| path(id)).collect();
    let asked = cache.visible(Some(&path("99")), &window, &[]);
    assert_eq!(asked[0].local_path, path("99"));
    assert_eq!(asked[0].priority, CoverPriority::Selected);
}

#[test]
fn look_ahead_work_is_requested_last_and_only_with_what_is_left() {
    let mut cache = GamerCoverCache::default();
    let window: Vec<PathBuf> = (0..MAX_REQUESTS_PER_FRAME)
        .map(|id| path(&format!("w{id}")))
        .collect();
    let ahead: Vec<PathBuf> = (0..8).map(|id| path(&format!("a{id}"))).collect();

    let asked = cache.visible(Some(&path("sel")), &window, &ahead);
    assert_eq!(asked.len(), MAX_REQUESTS_PER_FRAME);
    assert_eq!(asked[0].priority, CoverPriority::Selected);
    assert!(
        !asked
            .iter()
            .any(|job| job.priority == CoverPriority::LookAhead),
        "look-ahead work displaced a visible row"
    );
    // Priorities never go backwards through the emitted list.
    let mut previous = CoverPriority::Selected;
    for job in &asked {
        assert!(
            job.priority >= previous,
            "{:?} came after {previous:?}",
            job.priority
        );
        previous = job.priority;
    }
}

#[test]
fn a_burst_of_selection_changes_cannot_take_more_than_one_slot_a_frame() {
    // The starvation argument, stated as a ceiling: however fast the selection
    // moves, it is one request per frame, so the visible rows always keep the rest.
    let mut cache = GamerCoverCache::default();
    let window: Vec<PathBuf> = (0..20).map(|id| path(&format!("w{id}"))).collect();
    for frame in 0..30 {
        let selection = path(&format!("sel{frame}"));
        let asked = cache.visible(Some(&selection), &window, &[]);
        let selected = asked
            .iter()
            .filter(|job| job.priority == CoverPriority::Selected)
            .count();
        assert!(
            selected <= MAX_SELECTED_REQUESTS_PER_FRAME,
            "frame {frame} emitted {selected} selected-priority requests"
        );
    }
    // And the visible rows really did get served throughout.
    assert!(
        window
            .iter()
            .all(|path| cache.slot_for(path, None).is_some()),
        "visible rows were starved by repeated selection changes"
    );
}

#[test]
fn the_worker_serves_the_selected_game_ahead_of_a_queued_backlog() {
    // FIFO alone would make a freshly selected game wait behind every look-ahead
    // job already queued, which on a fast scroll is hundreds of them.
    let mut queue: Vec<QueuedJob> = (0..50)
        .map(|index| queued(CoverPriority::LookAhead, &format!("a{index}"), index))
        .collect();
    queue.push(queued(CoverPriority::Selected, "sel", 500));
    let mut served_high = 0;

    let next = next_job(&mut queue, &mut served_high).expect("a job");
    assert_eq!(next.job.local_path, path("sel"));
    assert_eq!(next.job.priority, CoverPriority::Selected);
}

#[test]
fn equal_priorities_are_served_oldest_first() {
    let mut queue = vec![
        queued(CoverPriority::Visible, "second", 20),
        queued(CoverPriority::Visible, "first", 10),
    ];
    let mut served_high = 0;
    assert_eq!(
        next_job(&mut queue, &mut served_high)
            .expect("a job")
            .job
            .local_path,
        path("first")
    );
}

#[test]
fn a_run_of_high_priority_work_eventually_yields_to_lower_priority_work() {
    // The fairness release. Without it, a person holding an arrow key would keep a
    // selected-priority job at the head forever and the visible rows behind it
    // would never be read at all.
    let mut queue: Vec<QueuedJob> = (0..40)
        .map(|index| queued(CoverPriority::Visible, &format!("v{index}"), 1_000 + index))
        .collect();
    let mut served_high = 0;
    let mut served_lower = 0;

    for round in 0..40 {
        // A fresh selection arrives every round, as it does when the key is held.
        queue.push(queued(CoverPriority::Selected, &format!("s{round}"), round));
        let next = next_job(&mut queue, &mut served_high).expect("a job");
        if next.job.priority == CoverPriority::Visible {
            served_lower += 1;
        }
    }
    assert!(
        served_lower > 0,
        "visible rows were never served across 40 rounds of selection changes"
    );
    // And it happens at a predictable rate rather than once by luck.
    assert!(
        served_lower >= 40 / (FAIRNESS_RUN as usize + 1) - 1,
        "the fairness release fired only {served_lower} times in 40 rounds"
    );
}

#[test]
fn nothing_is_held_back_when_only_one_priority_is_queued() {
    // The counter must not build up while there is nothing being treated unfairly,
    // or the first lower-priority job to arrive would jump the whole queue.
    let mut queue: Vec<QueuedJob> = (0..10)
        .map(|index| queued(CoverPriority::Selected, &format!("s{index}"), index))
        .collect();
    let mut served_high = 0;
    for _ in 0..10 {
        next_job(&mut queue, &mut served_high).expect("a job");
    }
    assert_eq!(
        served_high, 0,
        "the fairness counter rose with nothing to be fair to"
    );
}

#[test]
fn the_queue_drops_the_least_valuable_work_when_it_overflows() {
    let mut queue: Vec<QueuedJob> = (0..MAX_QUEUED_JOBS as u64 + 100)
        .map(|index| queued(CoverPriority::LookAhead, &format!("a{index}"), index))
        .collect();
    queue.push(queued(CoverPriority::Selected, "sel", 9_999));
    trim_queue(&mut queue);

    assert_eq!(queue.len(), MAX_QUEUED_JOBS);
    assert!(
        queue.iter().any(|q| q.job.local_path == path("sel")),
        "the selected game was dropped in favour of look-ahead work"
    );
}

#[test]
fn a_queue_inside_its_bound_is_left_alone() {
    let mut queue = vec![queued(CoverPriority::Visible, "a", 1)];
    let before = queue.clone();
    trim_queue(&mut queue);
    assert_eq!(queue, before);
}

// --- Selection changes and identity --------------------------------------

#[test]
fn changing_selection_never_offers_the_previous_games_cover() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");

    // The selection moves to a game nothing has answered for yet. The panel reads
    // the new record's own path, which holds nothing ready - so there is no way for
    // it to draw the previous game's art.
    cache.visible(Some(&path("2")), &[path("2")], &[]);
    assert!(
        !matches!(
            cache.slot_for(&path("2"), None),
            Some(CoverSlot::Ready { .. })
        ),
        "the newly selected game had a ready cover it was never given"
    );
    // The old one is untouched and still belongs to the game it was resolved for.
    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::Ready { .. })
    ));
}

#[test]
fn a_late_reply_for_a_previous_selection_cannot_reach_the_current_one() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(Some(&path("1")), &[path("1")], &[]);
    let in_flight = ready_with_key(cache.generation(), "1", "key-1");

    // Selection moves on before that reply lands.
    cache.visible(Some(&path("2")), &[path("2")], &[]);
    assert!(cache.absorb(&context, in_flight));

    // It was stored against the record it answered for, not the one now selected.
    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::Ready { .. })
    ));
    assert!(
        matches!(cache.slot_for(&path("2"), None), Some(CoverSlot::Loading)),
        "the current selection inherited the previous one's reply"
    );
}

#[test]
fn a_selected_record_whose_provider_id_changed_cannot_keep_its_cover() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-old");
    let old = match cache.slot_for(&path("1"), None) {
        Some(CoverSlot::Ready { texture, .. }) => texture.id(),
        _ => panic!("expected a ready cover"),
    };

    cache.identity_refreshed();
    cache.visible(Some(&path("1")), &[path("1")], &[]);
    // While revalidating, the panel draws the placeholder rather than the old art.
    assert!(matches!(
        cache.slot_for(&path("1"), None),
        Some(CoverSlot::Revalidating { .. })
    ));
    assert!(cache.absorb(&context, ready_with_key(cache.generation(), "1", "key-new")));

    let Some(CoverSlot::Ready { texture, key, .. }) = cache.slot_for(&path("1"), None) else {
        panic!("the record did not resolve to its new cover");
    };
    assert_eq!(key, "key-new");
    assert_ne!(texture.id(), old);
}

#[test]
fn reselecting_the_same_game_reuses_the_decoded_artwork() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    loaded(&context, &mut cache, "1", "key-1");
    let texture = match cache.slot_for(&path("1"), None) {
        Some(CoverSlot::Ready { texture, .. }) => texture.id(),
        _ => panic!("expected a ready cover"),
    };

    // Away and back, several times over.
    for _ in 0..5 {
        cache.visible(Some(&path("2")), &[path("2")], &[]);
        assert!(
            cache
                .visible(Some(&path("1")), &[path("1")], &[])
                .is_empty(),
            "reselecting an already loaded game asked for it again"
        );
    }
    assert_eq!(
        match cache.slot_for(&path("1"), None) {
            Some(CoverSlot::Ready { texture, .. }) => texture.id(),
            _ => panic!("the cover was lost"),
        },
        texture,
        "the decoded artwork was replaced rather than reused"
    );
}

// --- Featured cover sizing ------------------------------------------------

/// The artwork budget the panel computes: what is left of `panel_height` once the
/// measured action block has been set aside.
fn budget(panel_height: f32, actions: f32) -> f32 {
    (panel_height - actions).max(0.0)
}

#[test]
fn the_featured_cover_is_a_readable_size_at_1920x1080() {
    let box_size = featured_cover_box(
        GAMER_FEATURED_CONTENT_MAX_WIDTH,
        budget(780.0, FEATURED_RESERVED_BELOW),
    )
    .expect("1080p has room for a featured cover");
    assert!(
        (240.0..=FEATURED_COVER_MAX_HEIGHT).contains(&box_size.y),
        "a 1080p featured cover measured {}",
        box_size.y
    );
    // Portrait, and never stretched past its ratio.
    assert!((box_size.x / box_size.y - FEATURED_COVER_ASPECT).abs() < 0.001);
}

#[test]
fn the_featured_cover_scales_down_rather_than_pushing_the_actions_off() {
    let mut previous = f32::MAX;
    for height in [1100.0_f32, 780.0, 540.0, 430.0, 360.0] {
        let available = budget(height, FEATURED_RESERVED_BELOW);
        let box_size = featured_cover_box(GAMER_FEATURED_CONTENT_MAX_WIDTH, available);
        let drawn = box_size.map(|size| size.y).unwrap_or(0.0);
        assert!(
            drawn <= previous,
            "a shorter panel produced a taller cover ({drawn} after {previous})"
        );
        previous = drawn;
        // Whatever it drew, it fitted inside the budget - so the actions the budget
        // was reserved for are still on screen.
        assert!(
            drawn <= available + 0.5,
            "the cover overran its budget at {height}"
        );
    }
}

#[test]
fn a_panel_too_short_for_artwork_drops_the_artwork_not_the_actions() {
    assert!(
        featured_cover_box(
            GAMER_FEATURED_CONTENT_MAX_WIDTH,
            budget(340.0, FEATURED_RESERVED_BELOW)
        )
        .is_none(),
        "a cramped panel still reserved space for a cover"
    );
}

#[test]
fn a_measured_action_block_taller_than_the_estimate_still_shrinks_the_cover() {
    // A wrapped title and a stacked action column make the block far taller than
    // the first-frame estimate. The cover has to give that space back.
    let tight = featured_cover_box(GAMER_FEATURED_CONTENT_MAX_WIDTH, budget(560.0, 420.0));
    let roomy = featured_cover_box(GAMER_FEATURED_CONTENT_MAX_WIDTH, budget(560.0, 240.0));
    let tight_height = tight.map(|size| size.y).unwrap_or(0.0);
    let roomy_height = roomy.map(|size| size.y).unwrap_or(0.0);
    assert!(
        tight_height < roomy_height,
        "a taller action block did not shrink the cover ({tight_height} vs {roomy_height})"
    );
}

#[test]
fn the_featured_cover_never_exceeds_the_panel_width() {
    for width in [220.0_f32, 300.0, 460.0] {
        if let Some(size) = featured_cover_box(width, 1200.0) {
            assert!(
                size.x <= width,
                "a {size:?} cover overflowed a {width}px panel"
            );
        }
    }
}

#[test]
fn artwork_is_fitted_and_letterboxed_never_stretched_or_cropped() {
    // The box is 2:3. Whichever way an image differs from that, it is scaled to
    // fit inside and the remaining axis is letterboxed - never scaled to fill and
    // never cropped.
    let box_size = egui::vec2(200.0, 300.0);

    // Taller than 2:3, so it fills the height and is letterboxed left and right.
    let tall = fit_within(box_size, egui::vec2(100.0, 200.0));
    assert!((tall.y - 300.0).abs() < 0.5, "{tall:?}");
    assert!(
        tall.x < box_size.x,
        "a tall cover was not letterboxed sideways"
    );
    assert!(
        (tall.x / tall.y - 0.5).abs() < 0.001,
        "the aspect ratio changed"
    );

    // RomM's own 162x216 is *wider* than 2:3, so it fills the width instead.
    let real = fit_within(box_size, egui::vec2(162.0, 216.0));
    assert!((real.x - 200.0).abs() < 0.5, "{real:?}");
    assert!(real.y <= box_size.y + 0.5);
    assert!(
        (real.x / real.y - 162.0 / 216.0).abs() < 0.001,
        "the aspect ratio changed"
    );

    // A landscape one fills the width and is letterboxed top and bottom.
    let landscape = fit_within(box_size, egui::vec2(320.0, 200.0));
    assert!((landscape.x - 200.0).abs() < 0.5);
    assert!(
        landscape.y < box_size.y,
        "a landscape cover was not letterboxed"
    );
    assert!(
        (landscape.x / landscape.y - 320.0 / 200.0).abs() < 0.001,
        "the aspect ratio changed"
    );

    // A degenerate size draws nothing rather than dividing by zero.
    assert_eq!(fit_within(box_size, egui::vec2(0.0, 0.0)), egui::Vec2::ZERO);
}
