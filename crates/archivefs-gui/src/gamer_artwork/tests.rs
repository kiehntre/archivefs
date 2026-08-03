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
    let asked = cache.visible(&window, &[]);
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

    let first = cache.visible(&window, &[]);
    assert_eq!(first.len(), 3);
    for id in ["1", "2", "3"] {
        assert!(cache.absorb(&context, ready(cache.generation(), id)));
    }

    // Scrolled away...
    let elsewhere: Vec<PathBuf> = ["4", "5"].iter().map(|id| path(id)).collect();
    cache.visible(&elsewhere, &[]);
    // ...and back. Nothing is asked for a second time.
    assert!(
        cache.visible(&window, &[]).is_empty(),
        "returning to loaded rows caused fresh requests"
    );
}

#[test]
fn a_record_in_flight_is_not_asked_for_twice() {
    let mut cache = GamerCoverCache::default();
    let window = vec![path("1")];
    assert_eq!(cache.visible(&window, &[]).len(), 1);
    // Nothing has answered yet; the second frame must stay quiet.
    assert!(
        cache.visible(&window, &[]).is_empty(),
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
        cache.visible(&window, &[]);
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
    cache.visible(&[path("1")], &[]);
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
    cache.visible(&[path("1")], &[]);
    let in_flight = ready(cache.generation(), "1");

    // Pushed out by a long scroll before the answer arrived.
    for start in (0..3_000).step_by(10) {
        let window: Vec<PathBuf> = (start..start + 10)
            .map(|id| path(&format!("far{id}")))
            .collect();
        cache.visible(&window, &[]);
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
    cache.visible(&[path("1")], &[]);
    assert!(cache.absorb(&context, ready(cache.generation(), "1")));
    // ...and after a scroll the same position holds record "2", which has not
    // answered yet. Nothing is keyed by position, so "2" has no cover at all
    // rather than "1"'s.
    cache.visible(&[path("2")], &[]);
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
    cache.visible(&[path("1")], &[]);
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
    cache.visible(&[path("1")], &[]);
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
    cache.visible(&all, &[]);
    for id in ["1", "2", "3"] {
        assert!(cache.absorb(&context, ready(cache.generation(), id)));
    }

    // A search, or a platform card, narrows it to one row. That row is record
    // "3", and it draws "3"'s cover - not the one that used to occupy row 0.
    cache.visible(&[path("3")], &[]);
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
    cache.visible(&all, &[]);
    for id in ["1", "2", "3"] {
        assert!(cache.absorb(&context, ready(cache.generation(), id)));
    }
    // Narrow, then widen again. Neither costs a request: the covers describe
    // records, and no record changed.
    assert!(cache.visible(&[path("2")], &[]).is_empty());
    assert!(cache.visible(&all, &[]).is_empty());
}

// --- Failure ------------------------------------------------------------

#[test]
fn a_failed_load_settles_into_a_placeholder_rather_than_retrying() {
    let context = context();
    let mut cache = GamerCoverCache::default();
    cache.visible(&[path("1")], &[]);
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
        cache.visible(&[path("1")], &[]).is_empty(),
        "a failed record was requested again on the next frame"
    );
}

#[test]
fn a_pending_slot_exists_from_the_moment_a_record_is_asked_about() {
    // What keeps a row's height stable: there is never a gap between asking and
    // having something to draw, because Loading and the placeholder occupy the
    // same box.
    let mut cache = GamerCoverCache::default();
    cache.visible(&[path("1")], &[]);
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
