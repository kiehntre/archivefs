//! Records browser, conflicts and stale-summary tests.
//!
//! Assertions are on the page and view models. Building them as pure functions over
//! a cache is what lets "filters compose", "a directory is never called
//! nonexistent" and "a superseded page is discarded" be settled as data questions.
//!
//! The presence probe is injected everywhere, so no test needs a filesystem to
//! decide what is at a path - and no test can accidentally read a file.

use super::*;
use archivefs_core::identity_source::cache::CACHE_FORMAT_VERSION;
use archivefs_core::identity_source::model::{
    ArtworkReference, ConflictField, ExternalHash, HashAlgorithm, IdentityConflict,
    IdentityProvider, MetadataProviderId,
};
use std::collections::HashMap;

const SERVER: &str = "http://172.19.0.20:8080";

// --- Fixtures -------------------------------------------------------------

/// One record, with everything a row might show left at a sensible default.
fn record(id: &str, title: &str, path: &str) -> ExternalIdentityRecord {
    ExternalIdentityRecord {
        provider: IdentityProvider::Romm,
        server_id: SERVER.to_string(),
        provider_platform_id: Some("7".to_string()),
        provider_game_id: id.to_string(),
        provider_file_id: None,
        provider_path: format!("roms/gb/{path}"),
        archivefs_path: Some(PathBuf::from(format!("/mnt/games/roms/gb/{path}"))),
        title: Some(title.to_string()),
        platform_candidate: Some("Game Boy".to_string()),
        provider_platform_name: Some("gb".to_string()),
        regions: vec!["USA".to_string()],
        revision: None,
        hashes: vec![ExternalHash::parse(HashAlgorithm::Md5, &"a".repeat(32)).expect("hash")],
        file_size_bytes: Some(131_072),
        metadata_provider_ids: vec![MetadataProviderId {
            provider: "igdb".to_string(),
            id: "4242".to_string(),
        }],
        artwork: None,
        related_files: Vec::new(),
        sibling_game_ids: Vec::new(),
        imported_at_unix_seconds: 1_785_595_944,
        provider_updated_at: Some("2026-07-30T12:00:00Z".to_string()),
        verification: ExternalVerification::StrongExternal,
        conflicts: Vec::new(),
        evidence: Vec::new(),
    }
}

fn cache(records: Vec<ExternalIdentityRecord>) -> IdentityCache {
    IdentityCache {
        format_version: CACHE_FORMAT_VERSION,
        provider: IdentityProvider::Romm,
        server_id: SERVER.to_string(),
        server_version: Some("5.1.0".to_string()),
        source_fingerprint: "abcd1234".to_string(),
        imported_at_unix_seconds: 1_785_595_944,
        platforms: Vec::new(),
        records,
        rejected_hashes: Vec::new(),
        unknown_platforms: Vec::new(),
        server_reported_total: Some(0),
    }
}

/// A catalogue with one record per verdict, plus variety to filter on.
fn varied_cache() -> IdentityCache {
    let mut records = Vec::new();
    for (index, verdict) in ALL_VERDICTS.iter().enumerate() {
        let mut row = record(
            &format!("{}", index + 1),
            &format!("Game {}", index + 1),
            &format!("game-{index}.gb"),
        );
        row.verification = *verdict;
        records.push(row);
    }
    // Something on another platform, with a region and artwork.
    let mut snes = record("100", "Super Title", "super.sfc");
    snes.platform_candidate = Some("SNES".to_string());
    snes.provider_platform_name = Some("snes".to_string());
    snes.provider_path = "roms/snes/super.sfc".to_string();
    snes.archivefs_path = Some(PathBuf::from("/mnt/games/roms/snes/super.sfc"));
    snes.regions = vec!["Europe".to_string()];
    snes.artwork = Some(ArtworkReference {
        reference: "https://images.igdb.com/x.jpg".to_string(),
        small_reference: Some("/assets/small.png".to_string()),
    });
    records.push(snes);
    // A multi-file record.
    let mut multi = record("101", "Disc Set", "Shenmue");
    multi.related_files = vec![
        "a.cdi".to_string(),
        "b.cdi".to_string(),
        "c.cdi".to_string(),
    ];
    multi.sibling_game_ids = vec!["102".to_string()];
    records.push(multi);
    // One with no canonical platform.
    let mut unknown = record("103", "Odd Console Game", "odd.bin");
    unknown.platform_candidate = None;
    unknown.provider_platform_name = Some("made-up-console".to_string());
    records.push(unknown);
    // One whose file list was omitted.
    let mut omitted = record("104", "Huge File List", "huge.pkg");
    omitted.evidence = vec![
        "RomM's file list for this record was too large to read, so its per-file detail was not \
         imported"
            .to_string(),
    ];
    records.push(omitted);
    cache(records)
}

/// A probe the test decides, keyed by file name.
fn probe(
    map: &'static [(&'static str, LocalPresence)],
) -> impl Fn(&Path) -> LocalPresence + 'static {
    let lookup: HashMap<&str, LocalPresence> = map.iter().copied().collect();
    move |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| lookup.get(name).copied())
            .unwrap_or(LocalPresence::Absent)
    }
}

fn all_absent(_: &Path) -> LocalPresence {
    LocalPresence::Absent
}

fn page_of(
    cache: &IdentityCache,
    filters: &RecordFilters,
    offset: usize,
    limit: usize,
) -> RecordPageView {
    build_record_page(cache, filters, offset, limit, &all_absent)
}

// --- Paging ---------------------------------------------------------------

#[test]
fn the_first_page_reports_its_position_and_the_whole_catalogues_counts() {
    let cache = varied_cache();
    let page = page_of(&cache, &RecordFilters::default(), 0, 4);
    assert_eq!(page.rows.len(), 4);
    assert_eq!(page.matching, cache.records.len());
    assert_eq!(page.total_in_cache, cache.records.len());
    assert_eq!(page.offset, 0);
    assert_eq!(page.page_number(), 1);
    assert_eq!(page.page_count(), cache.records.len().div_ceil(4));
    assert!(page.has_next());
    assert!(!page.has_previous());
    // The counts describe the catalogue, not the page.
    assert_eq!(page.counts.total, cache.records.len());
    // One Strong from the per-verdict set, plus the four extra records which all
    // default to Strong.
    assert_eq!(page.counts.strong, 5);
}

#[test]
fn paging_forward_and_back_covers_the_catalogue_exactly_once() {
    let cache = varied_cache();
    let limit = 3;
    let mut seen: Vec<String> = Vec::new();
    let mut offset = 0;
    loop {
        let page = page_of(&cache, &RecordFilters::default(), offset, limit);
        seen.extend(page.rows.iter().map(|row| row.romm_game_id.clone()));
        if !page.has_next() {
            break;
        }
        offset += limit;
    }
    assert_eq!(seen.len(), cache.records.len());
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "no record appeared twice");

    // And going back gives the previous slice.
    let second = page_of(&cache, &RecordFilters::default(), limit, limit);
    assert!(second.has_previous());
    let first_again = page_of(&cache, &RecordFilters::default(), 0, limit);
    assert_eq!(
        first_again
            .rows
            .iter()
            .map(|row| row.romm_game_id.clone())
            .collect::<Vec<_>>(),
        seen[..limit].to_vec()
    );
}

#[test]
fn the_final_page_is_short_rather_than_wrapping() {
    let cache = varied_cache();
    let total = cache.records.len();
    let limit = 4;
    let last_offset = (total / limit) * limit;
    let page = page_of(&cache, &RecordFilters::default(), last_offset, limit);
    assert_eq!(page.rows.len(), total - last_offset);
    assert!(!page.has_next());
    assert!(page.has_previous());
}

#[test]
fn an_offset_past_the_end_is_empty_rather_than_an_error() {
    let cache = varied_cache();
    let page = page_of(&cache, &RecordFilters::default(), 9_999, 10);
    assert!(page.rows.is_empty());
    assert_eq!(
        page.matching,
        cache.records.len(),
        "the total is still known"
    );
    assert!(!page.has_next());
    assert!(page.has_previous());
}

#[test]
fn a_page_size_is_bounded_however_large_it_is_asked_for() {
    let cache = varied_cache();
    let page = page_of(&cache, &RecordFilters::default(), 0, 100_000);
    assert_eq!(page.limit, MAX_PAGE_SIZE);
    let tiny = page_of(&cache, &RecordFilters::default(), 0, 0);
    assert_eq!(tiny.limit, 1, "zero is not a page size");
}

#[test]
fn a_page_holds_only_its_own_rows_and_never_the_catalogue() {
    // The guard against loading 36,259 records into the GUI: however large the
    // catalogue, a page allocates at most `limit` rows.
    let many: Vec<ExternalIdentityRecord> = (0..5_000)
        .map(|index| {
            record(
                &format!("{index}"),
                &format!("Game {index}"),
                &format!("{index}.gb"),
            )
        })
        .collect();
    let cache = cache(many);
    let page = page_of(&cache, &RecordFilters::default(), 0, DEFAULT_PAGE_SIZE);
    assert_eq!(page.rows.len(), DEFAULT_PAGE_SIZE);
    assert_eq!(page.matching, 5_000, "but it still knows the total");
    assert!(
        page.rows.capacity() <= MAX_PAGE_SIZE,
        "no oversized allocation"
    );
}

#[test]
fn ordering_is_deterministic_across_identical_requests() {
    let cache = varied_cache();
    let first = page_of(&cache, &RecordFilters::default(), 0, 5);
    let again = page_of(&cache, &RecordFilters::default(), 0, 5);
    assert_eq!(
        first
            .rows
            .iter()
            .map(|r| r.romm_game_id.clone())
            .collect::<Vec<_>>(),
        again
            .rows
            .iter()
            .map(|r| r.romm_game_id.clone())
            .collect::<Vec<_>>()
    );
}

// --- Filters --------------------------------------------------------------

#[test]
fn a_verdict_filter_selects_only_that_verdict() {
    let cache = varied_cache();
    for verdict in ALL_VERDICTS {
        let filters = RecordFilters {
            verdict: Some(verdict),
            ..RecordFilters::default()
        };
        let page = page_of(&cache, &filters, 0, 50);
        assert!(page.matching >= 1, "{verdict:?} should match something");
        assert!(
            page.rows.iter().all(|row| row.verdict == verdict),
            "{verdict:?} leaked other verdicts"
        );
    }
}

#[test]
fn platform_filters_use_the_cached_canonical_and_provider_names() {
    let cache = varied_cache();
    let canonical = RecordFilters {
        canonical_platform: Some("SNES".to_string()),
        ..RecordFilters::default()
    };
    let page = page_of(&cache, &canonical, 0, 50);
    assert_eq!(page.matching, 1);
    assert_eq!(page.rows[0].romm_game_id, "100");

    let provider = RecordFilters {
        romm_platform: Some("made-up-console".to_string()),
        ..RecordFilters::default()
    };
    let page = page_of(&cache, &provider, 0, 50);
    assert_eq!(page.matching, 1);
    assert_eq!(page.rows[0].romm_game_id, "103");

    // An exact canonical name, never a substring guess: "SNE" matches nothing.
    let partial = RecordFilters {
        canonical_platform: Some("SNE".to_string()),
        ..RecordFilters::default()
    };
    assert_eq!(page_of(&cache, &partial, 0, 50).matching, 0);
}

#[test]
fn the_title_filter_is_a_plain_case_insensitive_substring() {
    let cache = varied_cache();
    let filters = RecordFilters {
        title: "super".to_string(),
        ..RecordFilters::default()
    };
    let page = page_of(&cache, &filters, 0, 50);
    assert_eq!(page.matching, 1);
    assert_eq!(page.rows[0].title, "Super Title");

    // Regular-expression metacharacters are matched literally, not executed.
    for pattern in [".*", "(", "[a-z]+", "^Game", "a{1000000}"] {
        let filters = RecordFilters {
            title: pattern.to_string(),
            ..RecordFilters::default()
        };
        let page = page_of(&cache, &filters, 0, 50);
        assert_eq!(
            page.matching, 0,
            "{pattern:?} should be a literal substring that matches no title"
        );
    }
}

#[test]
fn the_multi_file_filter_needs_more_than_one_file() {
    let cache = varied_cache();
    let filters = RecordFilters {
        multi_file_only: true,
        ..RecordFilters::default()
    };
    let page = page_of(&cache, &filters, 0, 50);
    assert_eq!(page.matching, 1, "only the disc set has two or more files");
    assert_eq!(page.rows[0].romm_game_id, "101");
    assert_eq!(page.rows[0].related_files, 3);
    assert_eq!(page.rows[0].siblings, 1);
}

#[test]
fn the_unknown_platform_and_artwork_and_omitted_filters_each_select_their_own() {
    let cache = varied_cache();
    let unknown = page_of(
        &cache,
        &RecordFilters {
            unknown_platform_only: true,
            ..RecordFilters::default()
        },
        0,
        50,
    );
    assert_eq!(unknown.matching, 1);
    assert_eq!(unknown.rows[0].romm_game_id, "103");
    assert!(unknown.rows[0].canonical_platform.is_none());

    let artwork = page_of(
        &cache,
        &RecordFilters {
            has_artwork_only: true,
            ..RecordFilters::default()
        },
        0,
        50,
    );
    assert_eq!(artwork.matching, 1);
    assert!(artwork.rows[0].has_artwork);

    let omitted = page_of(
        &cache,
        &RecordFilters {
            file_detail_omitted_only: true,
            ..RecordFilters::default()
        },
        0,
        50,
    );
    assert_eq!(omitted.matching, 1);
    assert_eq!(omitted.rows[0].romm_game_id, "104");
    assert!(omitted.rows[0].file_detail_omitted);
}

#[test]
fn a_region_filter_matches_one_of_a_records_regions() {
    let cache = varied_cache();
    let page = page_of(
        &cache,
        &RecordFilters {
            region: Some("Europe".to_string()),
            ..RecordFilters::default()
        },
        0,
        50,
    );
    assert_eq!(page.matching, 1);
    assert_eq!(page.rows[0].romm_game_id, "100");
}

#[test]
fn each_presence_filter_selects_only_its_own_state() {
    let cache = varied_cache();
    static MAP: &[(&str, LocalPresence)] = &[
        ("game-0.gb", LocalPresence::File),
        ("game-1.gb", LocalPresence::Directory),
        ("game-2.gb", LocalPresence::DanglingSymlink),
        ("game-3.gb", LocalPresence::ParentAbsent),
        ("game-4.gb", LocalPresence::Other),
    ];
    let presence_for = probe(MAP);
    for (filter, expected) in [
        (PresenceFilter::RegularFile, LocalPresence::File),
        (PresenceFilter::Directory, LocalPresence::Directory),
        (
            PresenceFilter::DanglingSymlink,
            LocalPresence::DanglingSymlink,
        ),
        (PresenceFilter::MissingParent, LocalPresence::ParentAbsent),
        (PresenceFilter::Other, LocalPresence::Other),
    ] {
        let filters = RecordFilters {
            presence: Some(filter),
            ..RecordFilters::default()
        };
        let page = build_record_page(&cache, &filters, 0, 50, &presence_for);
        assert_eq!(
            page.matching, 1,
            "{filter:?} matched {} rows",
            page.matching
        );
        assert_eq!(page.rows[0].presence, Some(expected));
    }
    // Missing catches everything the map does not name.
    let missing = build_record_page(
        &cache,
        &RecordFilters {
            presence: Some(PresenceFilter::Missing),
            ..RecordFilters::default()
        },
        0,
        50,
        &presence_for,
    );
    assert_eq!(missing.matching, cache.records.len() - MAP.len());
}

#[test]
fn a_presence_probe_only_runs_when_a_presence_filter_asks_for_one() {
    let cache = varied_cache();
    let probed = std::cell::Cell::new(0usize);
    let counting = |_: &Path| {
        probed.set(probed.get() + 1);
        LocalPresence::File
    };
    // No presence filter: no syscalls, and no presence on the rows.
    let page = build_record_page(&cache, &RecordFilters::default(), 0, 5, &counting);
    assert_eq!(
        probed.get(),
        0,
        "probing without being asked is wasted work"
    );
    assert!(page.rows.iter().all(|row| row.presence.is_none()));

    // With one, every candidate is probed.
    probed.set(0);
    let filters = RecordFilters {
        presence: Some(PresenceFilter::RegularFile),
        ..RecordFilters::default()
    };
    let _ = build_record_page(&cache, &filters, 0, 5, &counting);
    assert_eq!(probed.get(), cache.records.len());
}

#[test]
fn a_cheap_filter_runs_before_the_expensive_probe() {
    let cache = varied_cache();
    let probed = std::cell::Cell::new(0usize);
    let counting = |_: &Path| {
        probed.set(probed.get() + 1);
        LocalPresence::File
    };
    // A verdict filter excludes most records, so only the survivors are probed.
    let filters = RecordFilters {
        verdict: Some(ExternalVerification::Ambiguous),
        presence: Some(PresenceFilter::RegularFile),
        ..RecordFilters::default()
    };
    let page = build_record_page(&cache, &filters, 0, 50, &counting);
    assert_eq!(page.matching, 1);
    assert_eq!(
        probed.get(),
        1,
        "the probe should only see records the cheap filters kept"
    );
}

#[test]
fn filters_compose_rather_than_replacing_one_another() {
    let cache = varied_cache();
    // Verdict alone.
    let verdict_only = RecordFilters {
        verdict: Some(ExternalVerification::StrongExternal),
        ..RecordFilters::default()
    };
    let broad = page_of(&cache, &verdict_only, 0, 50).matching;
    assert!(broad > 1);

    // Verdict and platform together must be narrower than either alone, and must
    // keep both conditions.
    let both = RecordFilters {
        verdict: Some(ExternalVerification::StrongExternal),
        canonical_platform: Some("SNES".to_string()),
        ..RecordFilters::default()
    };
    let page = page_of(&cache, &both, 0, 50);
    assert_eq!(page.matching, 1);
    assert_eq!(page.rows[0].romm_game_id, "100");

    // Adding a title narrows further, and adding an impossible combination yields
    // nothing rather than falling back to one of the filters.
    let three = RecordFilters {
        verdict: Some(ExternalVerification::StrongExternal),
        canonical_platform: Some("SNES".to_string()),
        title: "nothing like this".to_string(),
        ..RecordFilters::default()
    };
    assert_eq!(page_of(&cache, &three, 0, 50).matching, 0);
}

#[test]
fn an_empty_result_is_reported_as_such() {
    let cache = varied_cache();
    let filters = RecordFilters {
        title: "no such title anywhere".to_string(),
        ..RecordFilters::default()
    };
    let page = page_of(&cache, &filters, 0, 25);
    assert_eq!(page.matching, 0);
    assert!(page.rows.is_empty());
    assert!(!page.has_next());
    assert!(!page.has_previous());
    assert!(!page.filters.is_empty(), "a filter is active");
}

#[test]
fn the_filter_controls_offer_only_values_the_cache_contains() {
    let cache = varied_cache();
    let page = page_of(&cache, &RecordFilters::default(), 0, 5);
    assert!(page.canonical_platforms.contains(&"Game Boy".to_string()));
    assert!(page.canonical_platforms.contains(&"SNES".to_string()));
    assert!(page.romm_platforms.contains(&"made-up-console".to_string()));
    assert!(page.regions.contains(&"USA".to_string()));
    assert!(page.regions.contains(&"Europe".to_string()));
    // Sorted and de-duplicated, so the control is stable frame to frame.
    let mut sorted = page.canonical_platforms.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, page.canonical_platforms);
}

// --- Rows and detail ------------------------------------------------------

#[test]
fn a_row_carries_everything_it_draws() {
    let cache = varied_cache();
    let page = page_of(&cache, &RecordFilters::default(), 0, 1);
    let row = &page.rows[0];
    assert_eq!(row.romm_game_id, "1");
    assert_eq!(row.romm_platform.as_deref(), Some("gb"));
    assert_eq!(row.canonical_platform.as_deref(), Some("Game Boy"));
    assert_eq!(row.romm_path, "roms/gb/game-0.gb");
    assert_eq!(
        row.archivefs_path.as_deref(),
        Some(Path::new("/mnt/games/roms/gb/game-0.gb"))
    );
    assert_eq!(row.regions, vec!["USA".to_string()]);
    assert_eq!(row.file_size_bytes, Some(131_072));
    assert_eq!(row.published_hashes, vec!["MD5".to_string()]);
    assert_eq!(row.imported_at_unix_seconds, 1_785_595_944);
    assert_eq!(row.romm_updated_at.as_deref(), Some("2026-07-30T12:00:00Z"));
    assert_eq!(row.provenance, SERVER);
}

#[test]
fn the_detail_panel_carries_the_full_evidence() {
    let mut records = vec![record("1", "Evidence Game", "e.gb")];
    records[0].evidence = vec![
        "file size agrees at 131072 bytes".to_string(),
        "RomM published a matching MD5".to_string(),
    ];
    records[0].conflicts = vec![IdentityConflict {
        field: ConflictField::Platform,
        external: "gb".to_string(),
        local: "Game Boy Color".to_string(),
        detail: "RomM and the local evidence disagree about the platform".to_string(),
    }];
    records[0].related_files = vec!["one.gb".to_string()];
    records[0].artwork = Some(ArtworkReference {
        reference: "https://images.igdb.com/x.jpg".to_string(),
        small_reference: Some("/assets/small.png".to_string()),
    });
    let cache = cache(records);
    let detail = build_record_detail(&cache, "1", &all_absent).expect("the record exists");

    assert_eq!(detail.evidence.len(), 2, "nothing is dropped");
    assert_eq!(detail.conflicts.len(), 1);
    assert_eq!(detail.conflicts[0].field, "Platform");
    assert_eq!(detail.related_files, vec!["one.gb".to_string()]);
    assert_eq!(
        detail.metadata_ids.first().map(|row| row.label.clone()),
        Some("igdb".to_string())
    );
    // The artwork is recorded for provenance only; slice 4 renders it.
    assert!(detail.artwork_reference.is_some());
    let labels: Vec<String> = detail.rows.iter().map(|row| row.label.clone()).collect();
    for expected in [
        "RomM id",
        "RomM path",
        "Local path",
        "Local presence",
        "Verdict",
        "File size",
        "Regions",
        "Imported",
        "Provenance",
    ] {
        assert!(labels.contains(&expected.to_string()), "{expected} missing");
    }
}

#[test]
fn an_unknown_record_id_yields_no_detail() {
    let cache = varied_cache();
    assert!(build_record_detail(&cache, "no-such-id", &all_absent).is_none());
}

#[test]
fn a_record_with_no_published_hash_says_so_rather_than_showing_nothing() {
    let mut records = vec![record("1", "No Hash", "n.gb")];
    records[0].hashes.clear();
    let cache = cache(records);
    let detail = build_record_detail(&cache, "1", &all_absent).expect("exists");
    assert!(
        detail
            .rows
            .iter()
            .any(|row| row.value.contains("RomM published no hash")),
        "{:?}",
        detail.rows
    );
}

// --- Verdict wording ------------------------------------------------------

#[test]
fn strong_is_never_presented_as_confirmed() {
    assert_eq!(
        verdict_label(ExternalVerification::StrongExternal),
        "Strong"
    );
    let strong = verdict_explanation(ExternalVerification::StrongExternal);
    assert!(strong.contains("not been explicitly verified"), "{strong}");
    assert!(
        strong.contains("Nothing has been hashed"),
        "it should say plainly that no local hash happened: {strong}"
    );
    assert!(
        !strong.to_lowercase().contains("confirmed"),
        "Strong must not be described as confirmed: {strong}"
    );

    let confirmed = verdict_explanation(ExternalVerification::ConfirmedExternal);
    assert!(
        confirmed.contains("actually happened and agreed"),
        "{confirmed}"
    );
}

#[test]
fn every_verdict_has_the_projects_own_wording() {
    for (verdict, label, needle) in [
        (
            ExternalVerification::ConfirmedExternal,
            "Confirmed",
            "hash comparison",
        ),
        (
            ExternalVerification::StrongExternal,
            "Strong",
            "strong identity",
        ),
        (
            ExternalVerification::ProbableExternal,
            "Probable",
            "without verified hashes",
        ),
        (
            ExternalVerification::Ambiguous,
            "Ambiguous",
            "prevents a unique result",
        ),
        (
            ExternalVerification::Stale,
            "Stale",
            "comparable regular file",
        ),
        (
            ExternalVerification::Unmatched,
            "Unmatched",
            "No safe local match",
        ),
    ] {
        assert_eq!(verdict_label(verdict), label);
        assert!(
            verdict_explanation(verdict).contains(needle),
            "{label} explanation missing {needle:?}: {}",
            verdict_explanation(verdict)
        );
    }
}

// --- Presence wording -----------------------------------------------------

#[test]
fn a_present_directory_is_never_described_as_nonexistent() {
    assert_eq!(
        presence_label(LocalPresence::Directory),
        "Present directory"
    );
    let explanation =
        presence_explanation(LocalPresence::Directory).expect("a directory needs explaining");
    assert!(
        !explanation.to_lowercase().contains("does not exist"),
        "{explanation}"
    );
    assert!(explanation.contains("not missing"), "{explanation}");
    // And it says why it is still stale.
    assert!(
        explanation.contains("single published file size or hash"),
        "{explanation}"
    );
}

#[test]
fn each_presence_state_is_worded_distinctly() {
    let labels: Vec<&str> = [
        LocalPresence::File,
        LocalPresence::Directory,
        LocalPresence::DanglingSymlink,
        LocalPresence::Absent,
        LocalPresence::ParentAbsent,
        LocalPresence::Other,
    ]
    .into_iter()
    .map(presence_label)
    .collect();
    let mut unique = labels.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        labels.len(),
        "two states share a label: {labels:?}"
    );
    assert_eq!(presence_label(LocalPresence::File), "Present regular file");
    assert_eq!(
        presence_label(LocalPresence::DanglingSymlink),
        "Dangling symlink"
    );
    assert_eq!(presence_label(LocalPresence::Absent), "Missing path");
    assert_eq!(
        presence_label(LocalPresence::ParentAbsent),
        "Missing parent folder"
    );
    assert_eq!(
        presence_label(LocalPresence::Other),
        "Refused or unsafe path"
    );

    // A dangling symlink and a missing parent each explain themselves.
    assert!(
        presence_explanation(LocalPresence::DanglingSymlink)
            .expect("explained")
            .contains("target is gone")
    );
    assert!(
        presence_explanation(LocalPresence::ParentAbsent)
            .expect("explained")
            .contains("not on this machine")
    );
    // A plain file needs no explanation.
    assert!(presence_explanation(LocalPresence::File).is_none());
}

#[test]
fn a_stale_records_reason_is_carried_onto_its_row() {
    let mut records = vec![record("1", "Folder Game", "Shenmue")];
    records[0].verification = ExternalVerification::Stale;
    records[0].evidence = vec![
        "/mnt/games/roms/dc/Shenmue exists but is a directory. The game is present as a folder"
            .to_string(),
    ];
    let cache = cache(records);
    let page = page_of(&cache, &RecordFilters::default(), 0, 1);
    assert!(
        page.rows[0]
            .stale_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("is a directory")),
        "{:?}",
        page.rows[0].stale_reason
    );
    // A non-stale record carries no stale reason.
    let strong = varied_cache();
    let page = page_of(&strong, &RecordFilters::default(), 0, 1);
    assert!(page.rows[0].stale_reason.is_none());
}

// --- Cache identity and staleness ----------------------------------------

#[test]
fn a_page_is_only_accepted_for_the_cache_and_filters_that_asked_for_it() {
    let cache = varied_cache();
    let identity = CacheIdentity::of(&cache);
    let mut state = BrowseState::opened_at(BrowseView::Records);
    state.page_size = 5;
    let page = page_of(&cache, &state.filters, 0, 5);
    assert!(state.accepts_page(&page, &identity));

    // A different cache: refused.
    let mut other = identity.clone();
    other.imported_at_unix_seconds += 1;
    assert!(
        !state.accepts_page(&page, &other),
        "a page from a superseded cache must not be drawn"
    );

    // Different filters: refused.
    state.filters.verdict = Some(ExternalVerification::Stale);
    assert!(
        !state.accepts_page(&page, &identity),
        "a page for the previous filters answers a question nobody is asking"
    );

    // Different page size: refused.
    state.filters = RecordFilters::default();
    state.page_size = 10;
    assert!(!state.accepts_page(&page, &identity));
}

#[test]
fn a_cache_identity_changes_whenever_a_new_cache_is_published() {
    let base = varied_cache();
    let identity = CacheIdentity::of(&base);
    let mut later = base.clone();
    later.imported_at_unix_seconds += 1;
    assert_ne!(identity, CacheIdentity::of(&later));

    let mut fewer = base.clone();
    fewer.records.pop();
    assert_ne!(identity, CacheIdentity::of(&fewer));

    let mut elsewhere = base.clone();
    elsewhere.server_id = "http://10.0.0.5:8080".to_string();
    assert_ne!(identity, CacheIdentity::of(&elsewhere));
}

#[test]
fn conflict_and_stale_results_are_checked_against_the_cache_too() {
    let cache = varied_cache();
    let identity = CacheIdentity::of(&cache);
    let state = BrowseState::opened_at(BrowseView::Conflicts);

    let conflicts = build_conflict_page(&cache, 0, 10);
    assert!(state.accepts_conflicts(&conflicts, &identity));
    let mut other = identity.clone();
    other.records += 1;
    assert!(!state.accepts_conflicts(&conflicts, &other));

    let stale = StaleSummaryView {
        cache: identity.clone(),
        summary: StaleSummary::build(&cache, &[], 2, |_| LocalPresence::Absent),
    };
    assert!(state.accepts_stale(&stale, &identity));
    assert!(!state.accepts_stale(&stale, &other));
}

// --- Conflicts ------------------------------------------------------------

fn conflicting(
    id: &str,
    field: ConflictField,
    external: &str,
    local: &str,
) -> ExternalIdentityRecord {
    let mut row = record(id, &format!("Conflicted {id}"), &format!("{id}.gb"));
    row.verification = ExternalVerification::Ambiguous;
    row.conflicts = vec![IdentityConflict {
        field,
        external: external.to_string(),
        local: local.to_string(),
        detail: format!("RomM says {external}, locally it is {local}"),
    }];
    row.evidence = vec![format!("{external} versus {local}")];
    row
}

#[test]
fn a_cache_with_no_conflicts_reports_an_empty_page_that_still_knows_the_total() {
    let cache = varied_cache();
    let page = build_conflict_page(&cache, 0, 10);
    assert!(page.is_empty());
    assert_eq!(page.matching, 0);
    assert_eq!(
        page.total_in_cache,
        cache.records.len(),
        "the empty state must be able to say how many records were checked"
    );
    assert!(!page.has_next());
    assert!(!page.has_previous());
}

#[test]
fn each_kind_of_disagreement_is_shown_with_both_sides() {
    for (field, expected) in [
        (ConflictField::Hash, "Hash"),
        (ConflictField::Platform, "Platform"),
        (ConflictField::FileSize, "File size"),
        (ConflictField::Signature, "Format signature"),
        (ConflictField::FileState, "File state"),
    ] {
        let cache = cache(vec![conflicting("1", field, "romm value", "local value")]);
        let page = build_conflict_page(&cache, 0, 10);
        assert_eq!(page.matching, 1);
        let row = &page.rows[0];
        assert_eq!(row.conflicts.len(), 1);
        assert_eq!(row.conflicts[0].field, expected);
        // Both sides are retained: nothing is chosen and nothing is discarded.
        assert_eq!(row.conflicts[0].romm, "romm value");
        assert_eq!(row.conflicts[0].local, "local value");
        assert!(!row.conflicts[0].detail.is_empty());
        assert!(!row.evidence.is_empty(), "evidence must not be dropped");
        assert_eq!(row.provenance, SERVER);
    }
}

#[test]
fn a_duplicate_provider_claim_names_the_competing_records() {
    let mut row = conflicting("1", ConflictField::FileState, "2 RomM records", "one file");
    row.sibling_game_ids = vec!["2".to_string(), "3".to_string()];
    row.evidence
        .push("more than one RomM record claims this file".to_string());
    let cache = cache(vec![row]);
    let page = build_conflict_page(&cache, 0, 10);
    assert_eq!(
        page.rows[0].competing_records,
        vec!["2".to_string(), "3".to_string()]
    );
    assert!(
        page.rows[0]
            .evidence
            .iter()
            .any(|line| line.contains("more than one RomM record"))
    );
}

#[test]
fn stronger_local_evidence_is_shown_as_retained() {
    let mut row = conflicting("1", ConflictField::Platform, "gb", "Game Boy Color");
    row.evidence
        .push("ArchiveFS's own verified identity was stronger and was not displaced".to_string());
    let cache = cache(vec![row]);
    let page = build_conflict_page(&cache, 0, 10);
    assert!(
        page.rows[0]
            .local_evidence_retained
            .as_deref()
            .is_some_and(|line| line.contains("not displaced")),
        "{:?}",
        page.rows[0].local_evidence_retained
    );
    // And the verdict is reported as it stands - nothing was resolved.
    assert_eq!(page.rows[0].verdict, ExternalVerification::Ambiguous);
}

#[test]
fn conflicts_page_in_bounded_deterministic_order() {
    let rows: Vec<ExternalIdentityRecord> = (0..25)
        .map(|index| conflicting(&format!("{index}"), ConflictField::Hash, "romm", "local"))
        .collect();
    let cache = cache(rows);
    let first = build_conflict_page(&cache, 0, 10);
    assert_eq!(first.rows.len(), 10);
    assert_eq!(first.matching, 25);
    assert!(first.has_next());
    assert!(!first.has_previous());

    let last = build_conflict_page(&cache, 20, 10);
    assert_eq!(last.rows.len(), 5, "a short final page");
    assert!(!last.has_next());
    assert!(last.has_previous());

    // Deterministic: the same request twice gives the same ids.
    let again = build_conflict_page(&cache, 0, 10);
    assert_eq!(
        first
            .rows
            .iter()
            .map(|r| r.romm_game_id.clone())
            .collect::<Vec<_>>(),
        again
            .rows
            .iter()
            .map(|r| r.romm_game_id.clone())
            .collect::<Vec<_>>()
    );
    // Bounded however large a limit is asked for.
    assert_eq!(build_conflict_page(&cache, 0, 100_000).limit, MAX_PAGE_SIZE);
}

// --- Stale summary --------------------------------------------------------

/// A catalogue whose stale records cover every presence, in known proportions.
fn stale_cache() -> IdentityCache {
    let mut records = Vec::new();
    // Proportions chosen so the explained share - RomM's own missing flag plus dead
    // links - is comfortably over the nine-tenths the drift conclusion requires.
    let plan = [
        ("absent", 40, true),
        ("dangling", 20, false),
        ("directory", 4, false),
        ("noparent", 4, true),
    ];
    for (kind, count, romm_flagged) in plan {
        for index in 0..count {
            let mut row = record(
                &format!("{kind}-{index}"),
                &format!("{kind} {index}"),
                &format!("{kind}-{index}.gb"),
            );
            row.verification = ExternalVerification::Stale;
            if romm_flagged {
                row.evidence
                    .push("RomM reports this file as missing from its own filesystem".to_string());
            }
            records.push(row);
        }
    }
    // A matched record that must be excluded from the summary entirely.
    records.push(record("matched", "Matched", "matched.gb"));
    cache(records)
}

fn stale_probe(path: &Path) -> LocalPresence {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if name.starts_with("dangling") {
        LocalPresence::DanglingSymlink
    } else if name.starts_with("directory") {
        LocalPresence::Directory
    } else if name.starts_with("noparent") {
        LocalPresence::ParentAbsent
    } else {
        LocalPresence::Absent
    }
}

fn stale_view() -> StaleSummaryView {
    let cache = stale_cache();
    StaleSummaryView {
        cache: CacheIdentity::of(&cache),
        summary: StaleSummary::build(
            &cache,
            &[("roms".to_string(), "/mnt/games/roms".to_string())],
            3,
            stale_probe,
        ),
    }
}

#[test]
fn the_stale_partition_is_exact_and_excludes_matched_records() {
    let view = stale_view();
    let summary = &view.summary;
    assert_eq!(summary.stale, 68, "40 + 20 + 4 + 4");
    assert_eq!(
        summary.total_in_cache, 69,
        "the matched record is not stale"
    );
    assert_eq!(
        summary.by_reason.iter().map(|r| r.count).sum::<usize>(),
        summary.stale,
        "the reasons must partition the population exactly"
    );
    let by_code: HashMap<&str, usize> = summary
        .by_reason
        .iter()
        .map(|reason| (reason.code, reason.count))
        .collect();
    assert_eq!(by_code.get("absent"), Some(&40));
    assert_eq!(by_code.get("dangling_symlink"), Some(&20));
    assert_eq!(by_code.get("directory"), Some(&4));
    assert_eq!(by_code.get("parent_absent"), Some(&4));
    // The named aggregates agree with the groups.
    assert_eq!(summary.dangling_symlinks, 20);
    assert_eq!(summary.present_as_directory, 4);
    assert_eq!(summary.romm_reports_missing, 44, "the two flagged kinds");
}

#[test]
fn percentages_are_of_the_stale_population() {
    let view = stale_view();
    assert_eq!(view.share(68), "100.0%");
    assert_eq!(view.share(34), "50.0%");
    assert_eq!(view.share(0), "0.0%");
    let by_code: HashMap<&str, usize> = view
        .summary
        .by_reason
        .iter()
        .map(|reason| (reason.code, reason.count))
        .collect();
    // 40 of 68 is 58.8%.
    assert_eq!(view.share(by_code["absent"]), "58.8%");
}

#[test]
fn each_group_reports_how_many_romm_already_flags_missing() {
    let view = stale_view();
    for reason in &view.summary.by_reason {
        match reason.code {
            // The two kinds the fixture flagged.
            "absent" | "parent_absent" => assert_eq!(
                reason.romm_reports_missing, reason.count,
                "{} should be entirely flagged",
                reason.code
            ),
            _ => assert_eq!(
                reason.romm_reports_missing, 0,
                "{} should not be flagged",
                reason.code
            ),
        }
    }
}

#[test]
fn every_grouping_dimension_is_populated_and_bounded() {
    let view = stale_view();
    let summary = &view.summary;
    assert!(!summary.by_platform.is_empty());
    assert!(!summary.by_romm_prefix.is_empty());
    assert!(!summary.by_local_prefix.is_empty());
    assert!(!summary.by_extension.is_empty());
    // Every stale record came through the one configured mapping.
    assert_eq!(summary.by_mapping.len(), 1);
    assert!(summary.by_mapping[0].key.starts_with("roms ->"));
    assert_eq!(summary.by_mapping[0].count, summary.stale);
    // Examples are bounded by what the caller asked for.
    for reason in &summary.by_reason {
        assert!(reason.examples.len() <= 3, "{}", reason.examples.len());
    }
}

#[test]
fn the_remaining_example_count_is_the_group_minus_what_is_shown() {
    let view = stale_view();
    for reason in &view.summary.by_reason {
        let remaining = reason.count.saturating_sub(reason.examples.len());
        // What the renderer says, computed the same way.
        if reason.count > 3 {
            assert_eq!(remaining, reason.count - 3);
            assert!(remaining > 0);
        }
    }
}

#[test]
fn the_interpretation_appears_only_when_the_evidence_supports_it() {
    // Almost everything flagged by RomM or a dead link: drift.
    let view = stale_view();
    assert!(view.summary.looks_like_library_drift);
    let interpretation = view.interpretation().expect("an interpretation");
    assert!(
        interpretation.contains("ordinary library drift or broken links"),
        "{interpretation}"
    );
    assert!(
        interpretation.contains("not a path-mapping failure"),
        "{interpretation}"
    );

    // A population that is mostly unexplained: no conclusion offered.
    let mut records = Vec::new();
    for index in 0..20 {
        let mut row = record(&format!("{index}"), "Unexplained", &format!("{index}.gb"));
        row.verification = ExternalVerification::Stale;
        records.push(row);
    }
    let unexplained = cache(records);
    let view = StaleSummaryView {
        cache: CacheIdentity::of(&unexplained),
        summary: StaleSummary::build(&unexplained, &[], 3, |_| LocalPresence::Absent),
    };
    assert!(!view.summary.looks_like_library_drift);
    assert!(
        view.interpretation().is_none(),
        "a conclusion the evidence does not support must not be shown"
    );
}

#[test]
fn a_summary_of_an_empty_stale_population_is_harmless() {
    let cache = cache(vec![record("1", "Matched", "m.gb")]);
    let view = StaleSummaryView {
        cache: CacheIdentity::of(&cache),
        summary: StaleSummary::build(&cache, &[], 3, |_| LocalPresence::File),
    };
    assert_eq!(view.summary.stale, 0);
    assert!(view.summary.by_reason.is_empty());
    assert_eq!(view.share(0), "0%");
    // Nothing stale is not a problem to report.
    assert!(view.interpretation().is_some());
}

#[test]
fn stale_progress_reports_a_fraction_only_when_it_has_a_total() {
    let none = StaleProgress::default();
    assert!(none.fraction().is_none(), "no total, no percentage");
    let half = StaleProgress {
        probed: 50,
        total: 100,
    };
    assert_eq!(half.fraction(), Some(0.5));
}

// --- Browse state ---------------------------------------------------------

#[test]
fn opening_a_view_starts_with_no_stale_results_from_another() {
    let state = BrowseState::opened_at(BrowseView::StaleSummary);
    assert_eq!(state.view, BrowseView::StaleSummary);
    assert!(state.page.is_none());
    assert!(state.conflicts.is_none());
    assert!(state.stale.is_none());
    assert!(state.detail.is_none());
    assert!(!state.needs_reload);
    assert_eq!(state.page_size, DEFAULT_PAGE_SIZE);
    assert!(state.filters.is_empty());
}

#[test]
fn each_view_names_itself() {
    assert_eq!(BrowseView::Records.title(), "RomM records");
    assert_eq!(BrowseView::Conflicts.title(), "Identity conflicts");
    assert_eq!(BrowseView::StaleSummary.title(), "Stale records");
}

// --- Rendering ------------------------------------------------------------

fn rendered_text_contains(output: &egui::FullOutput, needle: &str) -> bool {
    fn shape_contains(shape: &egui::Shape, needle: &str) -> bool {
        match shape {
            egui::Shape::Text(text_shape) => text_shape.galley.text().contains(needle),
            egui::Shape::Vec(nested) => nested.iter().any(|shape| shape_contains(shape, needle)),
            _ => false,
        }
    }
    output
        .shapes
        .iter()
        .any(|clipped| shape_contains(&clipped.shape, needle))
}

fn render(state: &mut BrowseState) -> egui::FullOutput {
    let context = egui::Context::default();
    let mut state_ref = state.clone();
    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_browse_panel(ui, &mut state_ref, false, None);
        });
    });
    *state = state_ref;
    output
}

#[test]
fn the_records_view_draws_rows_with_their_verdict_and_presence() {
    let cache = varied_cache();
    static MAP: &[(&str, LocalPresence)] = &[("game-0.gb", LocalPresence::Directory)];
    let filters = RecordFilters {
        presence: Some(PresenceFilter::Directory),
        ..RecordFilters::default()
    };
    let page = build_record_page(&cache, &filters, 0, 5, &probe(MAP));
    let mut state = BrowseState {
        filters,
        page: Some(Box::new(page)),
        ..BrowseState::opened_at(BrowseView::Records)
    };
    let output = render(&mut state);
    assert!(rendered_text_contains(&output, "Game 1"));
    assert!(rendered_text_contains(&output, "roms/gb/game-0.gb"));
    // The corrected presence wording reaches the screen.
    assert!(rendered_text_contains(&output, "Present directory"));
    assert!(!rendered_text_contains(&output, "does not exist"));
    // And it says plainly that browsing contacts nothing.
    assert!(rendered_text_contains(
        &output,
        "No request is made to RomM"
    ));
}

#[test]
fn the_empty_conflicts_state_is_precise_about_what_it_means() {
    let cache = varied_cache();
    let page = build_conflict_page(&cache, 0, 10);
    let mut state = BrowseState {
        conflicts: Some(Box::new(page)),
        ..BrowseState::opened_at(BrowseView::Conflicts)
    };
    let output = render(&mut state);
    assert!(rendered_text_contains(
        &output,
        "No conflicting identity claims were found in the current RomM cache."
    ));
    // It must not imply the catalogue is free of stale or unmatched records.
    assert!(rendered_text_contains(
        &output,
        "says nothing about stale or unmatched records"
    ));
    assert!(rendered_text_contains(&output, "Checked all"));
}

#[test]
fn the_stale_view_draws_the_partition_and_its_interpretation() {
    let view = stale_view();
    let mut state = BrowseState {
        stale: Some(Box::new(view)),
        ..BrowseState::opened_at(BrowseView::StaleSummary)
    };
    let output = render(&mut state);
    assert!(rendered_text_contains(
        &output,
        "68 of 69 cached record(s) are stale"
    ));
    assert!(rendered_text_contains(
        &output,
        "ordinary library drift or broken links"
    ));
    // Every group label, in the corrected wording.
    for expected in [
        "nothing at that path",
        "a symlink whose target is gone",
        "a directory, not a file",
        "the folder that would hold it is missing too",
    ] {
        assert!(
            rendered_text_contains(&output, expected),
            "the summary did not draw {expected:?}"
        );
    }
    // And it states its own safety.
    assert!(rendered_text_contains(&output, "no hashing"));
}

#[test]
fn the_records_view_draws_an_empty_state_when_nothing_matches() {
    let cache = varied_cache();
    let filters = RecordFilters {
        title: "nothing at all".to_string(),
        ..RecordFilters::default()
    };
    let page = page_of(&cache, &filters, 0, 25);
    let mut state = BrowseState {
        filters,
        page: Some(Box::new(page)),
        ..BrowseState::opened_at(BrowseView::Records)
    };
    let output = render(&mut state);
    assert!(rendered_text_contains(
        &output,
        "No records match these filters"
    ));
    assert!(rendered_text_contains(
        &output,
        "The cache itself is unchanged"
    ));
}

#[test]
fn a_stale_cache_warning_is_drawn_when_a_result_was_discarded() {
    let mut state = BrowseState::opened_at(BrowseView::Records);
    state.needs_reload = true;
    state.page = Some(Box::new(page_of(&varied_cache(), &state.filters, 0, 5)));
    let output = render(&mut state);
    assert!(rendered_text_contains(
        &output,
        "The identity cache changed"
    ));
    assert!(rendered_text_contains(&output, "Reload to see it"));
}
