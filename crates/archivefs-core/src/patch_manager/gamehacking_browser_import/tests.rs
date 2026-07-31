//! Browser-assisted import: validation, identity, cache, and provenance.
//!
//! Every test here is offline by construction. Nothing in this module can
//! reach the network: `import_gamehacking_browser_content` has no
//! transport, no `ureq` agent, and no URL to fetch - the only URL it ever
//! handles is the one it *displays*, and the only process it could start
//! is behind the [`BrowserLauncher`] trait, which is faked below.

use std::sync::Mutex;

use super::*;
use crate::patch_manager::gamehacking_gamecube_provider::{
    GameCubeCodeFormat, GameCubeIdentityState,
};

const SAVED_PAGE: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/gamecube-browser-saved-page-54172.html");
const SAVED_PAGE_OTHER_GAME: &[u8] = include_bytes!(
    "../../../tests/fixtures/gamehacking/gamecube-browser-saved-page-other-game.html"
);
const PS2_SAVED_PAGE: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/ps2-browser-saved-page-70123.html");
const TEXT_EXPORT: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/gamecube-browser-export-54172.txt");
const EXPORT_WRONG_GAME_ID: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/gamecube-browser-export-wrong-game-id.txt");
const EXPORT_HEADERLESS: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/gamecube-browser-export-headerless.txt");
const UNRELATED_PAGE: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/browser-import-unrelated-page.html");
const CHALLENGE_PAGE: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/browser-import-cloudflare-challenge.html");
const PS2_EXPORT: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/ps2-browser-export-70123.pnach");
const PS2_EXPORT_OTHER_GAME: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/ps2-browser-export-other-game.pnach");

const GAME_ID: u64 = 54172;
const DOLPHIN_GAME_ID: &str = "GLME01";

// --- Fixtures -------------------------------------------------------------

/// A private temporary directory removed on drop.
struct TempCache {
    root: PathBuf,
}

impl TempCache {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-browser-import-{}-{}-{:?}",
            label.replace(' ', "_"),
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temporary cache root");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for TempCache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn gamecube_identity() -> BrowserImportLocalIdentity {
    BrowserImportLocalIdentity::GameCube {
        title: "Luigi's Mansion".to_string(),
        dolphin_game_id: DOLPHIN_GAME_ID.to_string(),
        region: Some("E".to_string()),
    }
}

fn ps2_identity() -> BrowserImportLocalIdentity {
    BrowserImportLocalIdentity::PlayStation2 {
        title: "Ratchet & Clank".to_string(),
        executable_crc: "A1B2C3D4".to_string(),
        serial: Some("SLUS-20218".to_string()),
        region: Some("NTSC-U".to_string()),
    }
}

fn gamecube_request(cache: &TempCache, source: BrowserImportSource) -> BrowserImportRequest {
    BrowserImportRequest {
        platform: BrowserImportPlatform::GameCube,
        game_id: GAME_ID,
        source_url: Some("https://gamehacking.org/game/54172/luigis-mansion".to_string()),
        candidate_title: "Luigi's Mansion".to_string(),
        identity: gamecube_identity(),
        cache_root: cache.root.clone(),
        kind: None,
        source,
    }
}

fn ps2_request(cache: &TempCache, source: BrowserImportSource) -> BrowserImportRequest {
    BrowserImportRequest {
        platform: BrowserImportPlatform::PlayStation2,
        game_id: 70123,
        source_url: None,
        candidate_title: "Ratchet & Clank".to_string(),
        identity: ps2_identity(),
        cache_root: cache.root.clone(),
        kind: None,
        source,
    }
}

fn pasted(bytes: &[u8]) -> BrowserImportSource {
    BrowserImportSource::Text {
        text: String::from_utf8(bytes.to_vec()).expect("utf-8 fixture"),
        origin: BrowserImportTextOrigin::PastedText,
    }
}

fn clipboard(bytes: &[u8]) -> BrowserImportSource {
    BrowserImportSource::Text {
        text: String::from_utf8(bytes.to_vec()).expect("utf-8 fixture"),
        origin: BrowserImportTextOrigin::Clipboard,
    }
}

fn saved_file(cache: &TempCache, name: &str, bytes: &[u8]) -> BrowserImportSource {
    let path = cache.path(name);
    fs::write(&path, bytes).expect("write import fixture");
    BrowserImportSource::File(path)
}

// --- 1. Valid GameCube game-page HTML import ------------------------------

#[test]
fn valid_gamecube_game_page_html_import_populates_the_exact_cache_key() {
    let cache = TempCache::new("gc-page");
    let outcome = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("valid saved page imports");

    assert_eq!(outcome.kind, BrowserImportKind::GamePageHtml);
    assert_eq!(outcome.cache_path, cache.path("game-54172.html"));
    assert!(outcome.cache_path.is_file());
    assert_eq!(outcome.imported_title.as_deref(), Some("Luigi's Mansion"));
    assert!(!outcome.replaced_existing_cache);
    assert_eq!(outcome.backup_path, None);
    // The page's own 13 entries: 10 explicitly ARMax, 3 explicitly Gecko.
    assert_eq!(outcome.cheat_count, 13);
    assert_eq!(outcome.action_replay_count, 10);
    assert_eq!(outcome.gecko_count, 3);
    assert_eq!(outcome.raw_unknown_count, 0);
    assert_eq!(outcome.enriched_from_cache, None);
    assert_eq!(outcome.headline(), "Browser import successful");
}

#[test]
fn imported_page_html_is_stored_with_scripts_styles_and_frames_stripped() {
    let cache = TempCache::new("gc-sanitize");
    let outcome = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("valid saved page imports");
    let stored = fs::read_to_string(&outcome.cache_path).expect("stored cache");
    let lowered = stored.to_ascii_lowercase();

    for element in ["<script", "<style", "<noscript", "<iframe"] {
        assert!(
            !lowered.contains(element),
            "stored cache must not contain {element}"
        );
    }
    assert!(
        !stored.contains("GTM-TESTING"),
        "a tracking payload inside <script> is removed with its element"
    );
    assert!(
        !stored.contains("A tracking comment"),
        "HTML comments are removed"
    );
    // Everything the existing provider parser needs survives.
    assert!(stored.contains("class=\"codID"));
    assert!(stored.contains("sub.exportCodes.php"));
    assert!(stored.contains("GLME01"));
    assert_ne!(
        outcome.provenance.supplied_sha256, outcome.provenance.stored_sha256,
        "sanitization is recorded as two distinct digests, never silently"
    );
}

#[test]
fn sanitizer_leaves_ordinary_markup_and_attributes_untouched() {
    let sanitized = sanitize_imported_html(
        "<div class=\"codID\"><label>Cheat</label></div><script>bad()</script><pre>04 05</pre>",
    );
    assert_eq!(
        sanitized,
        "<div class=\"codID\"><label>Cheat</label></div><pre>04 05</pre>"
    );
}

// --- 2. Valid GameCube Text export import ---------------------------------

#[test]
fn valid_gamecube_text_export_import_populates_the_exact_cache_key() {
    let cache = TempCache::new("gc-export");
    let outcome =
        import_gamehacking_browser_content(&gamecube_request(&cache, pasted(TEXT_EXPORT)))
            .expect("valid text export imports");

    assert_eq!(outcome.kind, BrowserImportKind::TextExport);
    assert_eq!(outcome.cache_path, cache.path("export-54172.txt"));
    assert_eq!(
        outcome.imported_title.as_deref(),
        Some("Luigi's Mansion (USA)")
    );
    assert_eq!(outcome.cheat_count, 6);
    // With no page imported yet, the flat Text export carries no format
    // label at all - every cheat stays RawUnknown, exactly as the live
    // provider would leave it.
    assert_eq!(outcome.raw_unknown_count, 6);
    assert_eq!(outcome.action_replay_count, 0);
    assert_eq!(outcome.gecko_count, 0);
    assert_eq!(outcome.enriched_from_cache, None);
}

// --- 3, 4, 5. Page + export enrichment -----------------------------------

#[test]
fn importing_the_page_then_the_export_enriches_formats_and_keeps_ambiguity() {
    let cache = TempCache::new("gc-staged");
    import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("page imports first");
    let outcome =
        import_gamehacking_browser_content(&gamecube_request(&cache, pasted(TEXT_EXPORT)))
            .expect("export imports second");

    assert_eq!(
        outcome.enriched_from_cache,
        Some(cache.path("game-54172.html")),
        "the already-imported page is the enrichment source"
    );
    assert_eq!(outcome.cheat_count, 6);
    // Explicit ARMax and Gecko labels are preserved from the page.
    assert_eq!(outcome.action_replay_count, 4);
    assert_eq!(outcome.gecko_count, 1);
    // The one export cheat with no page entry stays RawUnknown.
    assert_eq!(outcome.raw_unknown_count, 1);
    assert!(
        outcome
            .verified_evidence
            .iter()
            .any(|note| note.contains("format label(s) taken from the already-imported game page")),
        "{:?}",
        outcome.verified_evidence
    );
}

#[test]
fn importing_the_export_then_the_page_reports_the_enriched_counts() {
    let cache = TempCache::new("gc-staged-reverse");
    import_gamehacking_browser_content(&gamecube_request(&cache, pasted(TEXT_EXPORT)))
        .expect("export imports first");
    let outcome = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("page imports second");

    assert_eq!(
        outcome.enriched_from_cache,
        Some(cache.path("export-54172.txt"))
    );
    assert_eq!(outcome.cheat_count, 6);
    assert_eq!(outcome.action_replay_count, 4);
    assert_eq!(outcome.gecko_count, 1);
    assert_eq!(outcome.raw_unknown_count, 1);
}

#[test]
fn enrichment_never_promotes_a_cheat_the_page_does_not_label() {
    let cache = TempCache::new("gc-ambiguous");
    import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("page imports");
    import_gamehacking_browser_content(&gamecube_request(&cache, pasted(TEXT_EXPORT)))
        .expect("export imports");

    let game = GameHackingGameCubeGame {
        game_id: GAME_ID,
        title: "Luigi's Mansion".to_string(),
        system: "GameCube".to_string(),
        region: Some("USA".to_string()),
        dolphin_game_id: Some(DOLPHIN_GAME_ID.to_string()),
        revision: None,
        hash: None,
        source_url: gamehacking_game_page_url(GAME_ID),
    };
    let export = fs::read(cache.path("export-54172.txt")).expect("imported export");
    let page = fs::read(cache.path("game-54172.html")).expect("imported page");
    let mut cheats = parse_gamehacking_gamecube_export(&game, &export).expect("parses");
    apply_gamecube_page_format_labels(&mut cheats, &page);

    let unmatched = cheats
        .iter()
        .find(|cheat| cheat.name == "Not On The Page")
        .expect("the deliberately unmatched cheat");
    assert_eq!(unmatched.code_format, GameCubeCodeFormat::RawUnknown);
    let gecko = cheats
        .iter()
        .find(|cheat| cheat.name == "Element Modifier")
        .expect("the Gecko-labelled cheat");
    assert_eq!(gecko.code_format, GameCubeCodeFormat::Gecko);
}

// --- 6-11. Rejections -----------------------------------------------------

#[test]
fn wrong_gamehacking_game_id_is_rejected() {
    let cache = TempCache::new("wrong-game");
    let failure = import_gamehacking_browser_content(&gamecube_request(
        &cache,
        pasted(SAVED_PAGE_OTHER_GAME),
    ))
    .expect_err("a page for another game is refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::WrongGame);
    assert!(failure.detail.contains("99001"));
    assert!(
        failure
            .detail
            .contains("Change the selected GameHacking candidate"),
        "the message says how to deliberately import another game: {}",
        failure.detail
    );
    assert!(!cache.path("game-54172.html").exists());
}

#[test]
fn wrong_dolphin_game_id_in_a_text_export_is_rejected() {
    let cache = TempCache::new("wrong-dolphin-id");
    let failure =
        import_gamehacking_browser_content(&gamecube_request(&cache, pasted(EXPORT_WRONG_GAME_ID)))
            .expect_err("an export for another Dolphin Game ID is refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::GameIdMismatch);
    assert!(failure.detail.contains("GALE01"));
    assert!(failure.detail.contains(DOLPHIN_GAME_ID));
    assert!(!cache.path("export-54172.txt").exists());
}

#[test]
fn wrong_dolphin_game_id_on_a_page_is_rejected() {
    let cache = TempCache::new("wrong-dolphin-id-page");
    let mut request = gamecube_request(&cache, pasted(SAVED_PAGE));
    request.identity = BrowserImportLocalIdentity::GameCube {
        title: "Luigi's Mansion".to_string(),
        dolphin_game_id: "GLMP01".to_string(),
        region: Some("P".to_string()),
    };
    let failure =
        import_gamehacking_browser_content(&request).expect_err("a PAL local disc is refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::GameIdMismatch);
    assert!(failure.detail.contains("GLME01"));
    assert!(failure.detail.contains("GLMP01"));
}

#[test]
fn an_export_with_no_game_id_header_is_refused_rather_than_trusted_by_filename() {
    let cache = TempCache::new("headerless");
    let source = saved_file(&cache, "GLME01 luigis mansion.txt", EXPORT_HEADERLESS);
    let failure = import_gamehacking_browser_content(&gamecube_request(&cache, source))
        .expect_err("a file name is never evidence");
    assert_eq!(
        failure.kind,
        BrowserImportErrorKind::MissingIdentityEvidence
    );
    assert!(!cache.path("export-54172.txt").exists());
}

#[test]
fn wrong_platform_is_rejected() {
    let cache = TempCache::new("wrong-platform");
    let failure =
        import_gamehacking_browser_content(&gamecube_request(&cache, pasted(PS2_SAVED_PAGE)))
            .expect_err("a PS2 page is refused for a GameCube game");
    assert_eq!(failure.kind, BrowserImportErrorKind::WrongPlatform);
}

#[test]
fn a_platform_that_disagrees_with_the_verified_local_identity_is_refused_up_front() {
    let cache = TempCache::new("platform-disagreement");
    let mut request = gamecube_request(&cache, pasted(SAVED_PAGE));
    request.platform = BrowserImportPlatform::PlayStation2;
    let failure = import_gamehacking_browser_content(&request).expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::WrongPlatform);
    assert!(failure.detail.contains("GameCube"));
}

#[test]
fn an_unrelated_webpage_is_rejected() {
    let cache = TempCache::new("unrelated");
    let failure =
        import_gamehacking_browser_content(&gamecube_request(&cache, pasted(UNRELATED_PAGE)))
            .expect_err("an encyclopedia article is refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::UnrelatedContent);
    assert!(
        failure.detail.contains("not a GameHacking.org page"),
        "{}",
        failure.detail
    );
    assert!(!cache.path("game-54172.html").exists());
}

#[test]
fn cloudflare_challenge_html_is_rejected() {
    let cache = TempCache::new("challenge");
    let failure =
        import_gamehacking_browser_content(&gamecube_request(&cache, pasted(CHALLENGE_PAGE)))
            .expect_err("a challenge page is refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::ChallengeContent);
    assert!(!cache.path("game-54172.html").exists());
}

/// The same fixture a browser would have saved from an HTTP *200*
/// response: there is no status code to lean on here at all, which is
/// exactly the case the shared body classifier exists for.
#[test]
fn an_http_200_challenge_body_is_still_classified_as_a_challenge() {
    assert!(
        cached_bytes_are_cloudflare_challenge(CHALLENGE_PAGE),
        "the shared classifier recognises the body with no status involved"
    );
    let cache = TempCache::new("challenge-200");
    let source = saved_file(&cache, "gamehacking-page.html", CHALLENGE_PAGE);
    let failure =
        import_gamehacking_browser_content(&gamecube_request(&cache, source)).expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::ChallengeContent);
}

// --- 12, 13, 19. Input handling ------------------------------------------

#[test]
fn oversized_input_is_rejected() {
    let cache = TempCache::new("oversized");
    let oversized = "A".repeat(MAX_BROWSER_IMPORT_BYTES + 1);
    let failure = import_gamehacking_browser_content(&gamecube_request(
        &cache,
        BrowserImportSource::Text {
            text: oversized,
            origin: BrowserImportTextOrigin::PastedText,
        },
    ))
    .expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::InputTooLarge);
    assert!(
        failure
            .detail
            .contains(&MAX_BROWSER_IMPORT_BYTES.to_string())
    );
}

#[test]
fn an_oversized_file_is_rejected_without_being_read() {
    let cache = TempCache::new("oversized-file");
    let path = cache.path("huge page.html");
    fs::write(&path, vec![b'A'; MAX_BROWSER_IMPORT_BYTES + 1]).expect("write");
    let failure = import_gamehacking_browser_content(&gamecube_request(
        &cache,
        BrowserImportSource::File(path),
    ))
    .expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::InputTooLarge);
}

#[test]
fn empty_clipboard_is_rejected_as_an_empty_clipboard_not_a_generic_failure() {
    let cache = TempCache::new("empty-clipboard");
    let failure = import_gamehacking_browser_content(&gamecube_request(
        &cache,
        clipboard("   \n\t ".as_bytes()),
    ))
    .expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::ClipboardEmpty);
    assert_eq!(failure.kind.headline(), "Clipboard is empty");
}

#[test]
fn an_empty_pasted_text_area_is_rejected_as_empty_input() {
    let cache = TempCache::new("empty-paste");
    let failure = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(b"")))
        .expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::EmptyInput);
}

#[test]
fn an_empty_file_is_rejected_as_empty_input() {
    let cache = TempCache::new("empty-file");
    let source = saved_file(&cache, "empty.html", b"");
    let failure =
        import_gamehacking_browser_content(&gamecube_request(&cache, source)).expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::EmptyInput);
}

#[test]
fn the_clipboard_paste_path_imports_exactly_like_a_saved_file() {
    let cache_paste = TempCache::new("paste-path");
    let pasted_outcome =
        import_gamehacking_browser_content(&gamecube_request(&cache_paste, clipboard(SAVED_PAGE)))
            .expect("clipboard paste imports");
    let cache_file = TempCache::new("file-path");
    let source = saved_file(&cache_file, "Luigi's Mansion Cheats.html", SAVED_PAGE);
    let file_outcome = import_gamehacking_browser_content(&gamecube_request(&cache_file, source))
        .expect("saved file imports");

    assert_eq!(
        pasted_outcome.provenance.stored_sha256, file_outcome.provenance.stored_sha256,
        "the same bytes land identically whichever way they arrived"
    );
    assert_eq!(pasted_outcome.provenance.original_filename, None);
    assert_eq!(
        file_outcome.provenance.original_filename.as_deref(),
        Some("Luigi's Mansion Cheats.html")
    );
}

// --- 18. Paths containing spaces -----------------------------------------

#[test]
fn a_file_path_containing_spaces_imports_normally() {
    let cache = TempCache::new("spaced path");
    let directory = cache.path("Saved Pages/GameHacking Downloads");
    fs::create_dir_all(&directory).expect("nested directory with spaces");
    let path = directory.join("Luigi's Mansion - Codes (USA).html");
    fs::write(&path, SAVED_PAGE).expect("write");

    let outcome = import_gamehacking_browser_content(&gamecube_request(
        &cache,
        BrowserImportSource::File(path),
    ))
    .expect("a path with spaces is an ordinary path");
    assert_eq!(
        outcome.provenance.original_filename.as_deref(),
        Some("Luigi's Mansion - Codes (USA).html")
    );
    assert!(outcome.cache_path.is_file());
}

#[cfg(unix)]
#[test]
fn a_symlinked_import_source_is_refused() {
    let cache = TempCache::new("symlink");
    let real = cache.path("real page.html");
    fs::write(&real, SAVED_PAGE).expect("write");
    let link = cache.path("link to page.html");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let failure = import_gamehacking_browser_content(&gamecube_request(
        &cache,
        BrowserImportSource::File(link),
    ))
    .expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::SourceUnreadable);
    assert!(failure.detail.contains("symlink"));
}

// --- 14, 15, 26. Cache safety --------------------------------------------

#[test]
fn an_existing_valid_cache_survives_every_kind_of_failed_import() {
    let cache = TempCache::new("cache-survives");
    let good = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("a good page is imported first");
    let good_bytes = fs::read(&good.cache_path).expect("stored cache");
    let good_sha = good.provenance.stored_sha256.clone();

    for bad in [
        CHALLENGE_PAGE,
        UNRELATED_PAGE,
        SAVED_PAGE_OTHER_GAME,
        PS2_SAVED_PAGE,
        b"<html><body>not a page at all</body></html>".as_slice(),
    ] {
        let failure = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(bad)))
            .expect_err("every one of these is refused");
        assert_ne!(failure.kind, BrowserImportErrorKind::CacheWriteFailed);
        assert_eq!(
            fs::read(&good.cache_path).expect("cache still there"),
            good_bytes,
            "a failed import never touches a good cache ({:?})",
            failure.kind
        );
    }
    let provenance = read_browser_import_provenance(&good.cache_path).expect("provenance intact");
    assert_eq!(provenance.stored_sha256, good_sha);
}

#[test]
fn replacing_an_existing_cache_reports_and_backs_up_what_it_replaced() {
    let cache = TempCache::new("replace");
    let first = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("first import");
    let second = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("second import replaces the first");

    assert!(second.replaced_existing_cache);
    let replaced = second.replaced.expect("what was replaced is described");
    assert_eq!(replaced.source, MANUAL_BROWSER_IMPORT_SOURCE);
    assert_eq!(
        replaced.sha256.as_deref(),
        Some(first.provenance.stored_sha256.as_str())
    );
    assert!(replaced.retrieved_at_unix_seconds.is_some());
    let backup = second.backup_path.expect("a rollback copy is kept");
    assert_eq!(
        fs::read(&backup).expect("backup readable"),
        fs::read(&first.cache_path).expect("current cache")
    );
}

#[test]
fn a_live_fetched_cache_is_reported_as_live_fetch_before_being_replaced() {
    let cache = TempCache::new("replace-live");
    // What the live provider writes: content plus its own sidecars, and
    // deliberately no import provenance.
    fs::write(cache.path("game-54172.html"), SAVED_PAGE).expect("seed");
    fs::write(cache.path("game-54172.html.charset"), b"utf-8").expect("seed charset");
    fs::write(cache.path("game-54172.html.retrieved"), b"1700000000").expect("seed retrieved");

    let plan = plan_gamehacking_browser_import(
        BrowserImportPlatform::GameCube,
        GAME_ID,
        None,
        &gamecube_identity(),
        &cache.root,
    )
    .expect("plan");
    let page_destination = plan
        .destinations
        .iter()
        .find(|destination| destination.kind == BrowserImportKind::GamePageHtml)
        .expect("page destination");
    let existing = page_destination
        .existing
        .as_ref()
        .expect("the live cache is reported");
    assert_eq!(existing.source, "live_fetch");
    assert_eq!(existing.retrieved_at_unix_seconds, Some(1_700_000_000));
    assert!(plan.replaces_existing_cache());
}

#[test]
fn a_successful_import_writes_the_cache_atomically_and_leaves_no_partial_file() {
    let cache = TempCache::new("atomic");
    let outcome = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("import");
    let mut names: Vec<String> = fs::read_dir(&cache.root)
        .expect("cache dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "game-54172.html".to_string(),
            "game-54172.html.charset".to_string(),
            "game-54172.html.import.json".to_string(),
            "game-54172.html.retrieved".to_string(),
        ],
        "exactly the provider's own cache key plus its sidecars"
    );
    assert_eq!(outcome.cache_path, cache.path("game-54172.html"));
}

#[test]
fn an_import_never_creates_or_clears_the_cloudflare_cooldown_marker() {
    let cache = TempCache::new("cooldown");
    let marker = cache.path("cloudflare-blocked-at");
    fs::write(&marker, b"1700000000").expect("seed an active cooldown");

    // A blocked origin does not stop a manual import - that is the whole
    // point - and the import must not clear the marker either, so the
    // live provider's own cooldown behaviour stays exactly as it was.
    import_gamehacking_browser_content(&gamecube_request(&cache, pasted(SAVED_PAGE)))
        .expect("a manual import works while live access is blocked");
    assert_eq!(
        fs::read(&marker).expect("marker intact"),
        b"1700000000".to_vec()
    );

    let _ = import_gamehacking_browser_content(&gamecube_request(&cache, pasted(CHALLENGE_PAGE)));
    assert_eq!(
        fs::read(&marker).expect("marker intact"),
        b"1700000000".to_vec(),
        "importing a challenge page does not re-arm or extend the cooldown either"
    );
}

// --- 16, 17. Provenance ---------------------------------------------------

#[test]
fn manual_import_provenance_is_persisted_next_to_the_cache_entry() {
    let cache = TempCache::new("provenance");
    let source = saved_file(&cache, "Luigi's Mansion.html", SAVED_PAGE);
    let outcome =
        import_gamehacking_browser_content(&gamecube_request(&cache, source)).expect("import");

    let stored = read_browser_import_provenance(&outcome.cache_path).expect("provenance on disk");
    assert_eq!(stored, outcome.provenance);
    assert_eq!(stored.source, MANUAL_BROWSER_IMPORT_SOURCE);
    assert_eq!(
        stored.schema_version,
        BROWSER_IMPORT_PROVENANCE_SCHEMA_VERSION
    );
    assert_eq!(
        stored.parser_schema_version,
        BROWSER_IMPORT_PARSER_SCHEMA_VERSION
    );
    assert_eq!(stored.platform, BrowserImportPlatform::GameCube);
    assert_eq!(stored.gamehacking_game_id, GAME_ID);
    assert_eq!(stored.import_kind, BrowserImportKind::GamePageHtml);
    assert_eq!(
        stored.expected_source_url,
        "https://gamehacking.org/game/54172/luigis-mansion"
    );
    assert_eq!(stored.cache_file_name, "game-54172.html");
    assert_eq!(
        stored.original_filename.as_deref(),
        Some("Luigi's Mansion.html")
    );
    assert_eq!(stored.local_identity, gamecube_identity());
    assert!(stored.imported_at_unix_seconds > 0);
    assert_eq!(stored.stored_sha256.len(), 64);
    assert!(!stored.verified_evidence.is_empty());
    assert_eq!(
        outcome.provenance_path,
        cache.path("game-54172.html.import.json")
    );
}

#[test]
fn no_browser_secret_is_ever_requested_or_persisted() {
    let cache = TempCache::new("no-secrets");
    // A saved page whose markup carries cookie-shaped strings: none may
    // end up in provenance, and none is ever identity evidence.
    let page = String::from_utf8(SAVED_PAGE.to_vec())
        .expect("utf-8")
        .replace(
            "<h1>Luigi's Mansion</h1>",
            "<h1>Luigi's Mansion</h1>\n<script>document.cookie='cf_clearance=SECRET-TOKEN';</script>\n",
        );
    let outcome = import_gamehacking_browser_content(&gamecube_request(
        &cache,
        BrowserImportSource::Text {
            text: page,
            origin: BrowserImportTextOrigin::PastedText,
        },
    ))
    .expect("import");

    let provenance_json =
        fs::read_to_string(&outcome.provenance_path).expect("provenance readable");
    let stored_cache = fs::read_to_string(&outcome.cache_path).expect("cache readable");
    for secret in [
        "cookie",
        "Cookie",
        "cf_clearance",
        "SECRET-TOKEN",
        "authorization",
        "Authorization",
        "set-cookie",
        "user-agent",
        "session",
        "bearer",
    ] {
        assert!(
            !provenance_json.contains(secret),
            "provenance must not record `{secret}`"
        );
    }
    assert!(
        !stored_cache.contains("SECRET-TOKEN"),
        "a cookie assignment inside a <script> is stripped with its element"
    );
    // The provenance record has no field capable of holding one either.
    let value: serde_json::Value = serde_json::from_str(&provenance_json).expect("json");
    let keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    // `serde_json::Value` keeps object keys sorted, so this is the whole
    // key set in one deterministic order.
    assert_eq!(
        keys,
        vec![
            "cache_file_name",
            "expected_source_url",
            "gamehacking_game_id",
            "import_kind",
            "imported_at_unix_seconds",
            "local_identity",
            "original_filename",
            "parser_schema_version",
            "platform",
            "schema_version",
            "source",
            "stored_sha256",
            "supplied_sha256",
            "verified_evidence",
        ],
        "the provenance schema is a closed set with no credential field"
    );
}

// --- 20, 21. Browser launch ----------------------------------------------

/// Records exactly what would have been executed. Nothing is started.
#[derive(Default)]
struct RecordingLauncher {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    failure: Option<String>,
}

impl BrowserLauncher for RecordingLauncher {
    fn launch(&self, program: &str, arguments: &[String]) -> Result<(), String> {
        self.calls
            .lock()
            .expect("lock")
            .push((program.to_string(), arguments.to_vec()));
        match &self.failure {
            Some(failure) => Err(failure.clone()),
            None => Ok(()),
        }
    }
}

#[test]
fn the_browser_launch_uses_one_separate_unquoted_url_argument() {
    let launcher = RecordingLauncher::default();
    let notice = open_gamehacking_url_in_browser(
        "https://gamehacking.org/game/54172?utm_source=archivefs#codes",
        &launcher,
    )
    .expect("launch");

    let calls = launcher.calls.lock().expect("lock");
    assert_eq!(calls.len(), 1);
    let (program, arguments) = &calls[0];
    #[cfg(target_os = "linux")]
    assert_eq!(program, "xdg-open");
    assert!(!program.is_empty());
    assert_eq!(
        arguments,
        &vec!["https://gamehacking.org/game/54172".to_string()],
        "exactly one argument, with the tracking query and the fragment removed"
    );
    assert!(
        !arguments[0].contains(' ') && !arguments[0].contains(';') && !arguments[0].contains('&'),
        "nothing shell-interpretable survives validation"
    );
    assert!(
        notice.contains("Nothing has been imported yet"),
        "opening a browser is never reported as a successful import: {notice}"
    );
}

#[test]
fn browser_launch_failure_is_reported_clearly() {
    let launcher = RecordingLauncher {
        failure: Some("xdg-open could not be started: No such file or directory".to_string()),
        ..Default::default()
    };
    let failure = open_gamehacking_url_in_browser(&gamehacking_game_page_url(GAME_ID), &launcher)
        .expect_err("the failure surfaces");
    assert_eq!(failure.kind, BrowserImportErrorKind::BrowserLaunchFailed);
    assert!(failure.detail.contains("could not open your browser"));
    assert!(
        failure.detail.contains("Copy the URL and open it manually"),
        "{}",
        failure.detail
    );
}

#[test]
fn only_plain_https_gamehacking_urls_can_be_opened() {
    let launcher = RecordingLauncher::default();
    for hostile in [
        "http://gamehacking.org/game/54172",
        "https://gamehacking.org.evil.example/game/54172",
        "https://evil.example/gamehacking.org/game/54172",
        "https://user:pass@gamehacking.org/game/54172",
        "https://gamehacking.org:8443/game/54172",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://gamehacking.org/game/54172 ; rm -rf /",
        "not a url at all",
    ] {
        let failure = open_gamehacking_url_in_browser(hostile, &launcher)
            .expect_err(&format!("`{hostile}` must be refused"));
        assert_eq!(
            failure.kind,
            BrowserImportErrorKind::InvalidUrl,
            "`{hostile}` must fail URL validation, not something later"
        );
    }
    assert!(
        launcher.calls.lock().expect("lock").is_empty(),
        "no hostile URL ever reaches the launcher"
    );
}

#[test]
fn the_expected_url_is_always_the_canonical_game_page() {
    assert_eq!(
        gamehacking_game_page_url(54172),
        "https://gamehacking.org/game/54172"
    );
    assert_eq!(
        gamehacking_game_id_from_url("https://gamehacking.org/game/501/test-racer"),
        Some(501)
    );
    assert_eq!(
        gamehacking_game_id_from_url("https://evil.example/game/501"),
        None
    );
    assert_eq!(gamehacking_game_id_from_url("/game/54172"), Some(54172));
    assert_eq!(
        validate_gamehacking_browser_url("https://gamehacking.org/game/54172?x=1#y")
            .expect("valid"),
        "https://gamehacking.org/game/54172"
    );
    let (program, arguments) =
        gamehacking_browser_launch_command("https://gamehacking.org/game/54172").expect("command");
    assert!(!program.contains(' '));
    assert_eq!(arguments.len(), 1);
}

// --- The dialog's plan ----------------------------------------------------

#[test]
fn the_plan_names_every_field_the_dialog_must_show() {
    let cache = TempCache::new("plan");
    let plan = plan_gamehacking_browser_import(
        BrowserImportPlatform::GameCube,
        GAME_ID,
        Some("https://gamehacking.org/game/54172/luigis-mansion"),
        &gamecube_identity(),
        &cache.root,
    )
    .expect("plan");

    assert_eq!(plan.platform_label, "GameCube");
    assert_eq!(plan.local_game_title, "Luigi's Mansion");
    assert_eq!(
        plan.local_identity_summary,
        "Verified Dolphin Game ID GLME01 · region code E"
    );
    assert_eq!(plan.gamehacking_game_id, GAME_ID);
    assert_eq!(
        plan.expected_source_url,
        "https://gamehacking.org/game/54172/luigis-mansion"
    );
    assert_eq!(plan.accepted_formats.len(), 4);
    assert_eq!(
        plan.destinations
            .iter()
            .map(|destination| destination.cache_file_name.as_str())
            .collect::<Vec<_>>(),
        vec!["game-54172.html", "export-54172.txt"]
    );
    assert!(!plan.replaces_existing_cache());
}

#[test]
fn a_source_url_for_another_game_falls_back_to_the_canonical_url() {
    let cache = TempCache::new("plan-url");
    let plan = plan_gamehacking_browser_import(
        BrowserImportPlatform::GameCube,
        GAME_ID,
        Some("https://gamehacking.org/game/99001/some-other-game"),
        &gamecube_identity(),
        &cache.root,
    )
    .expect("plan");
    assert_eq!(
        plan.expected_source_url,
        "https://gamehacking.org/game/54172"
    );
}

#[test]
fn the_plan_refuses_an_identity_from_another_platform() {
    let cache = TempCache::new("plan-platform");
    let failure = plan_gamehacking_browser_import(
        BrowserImportPlatform::GameCube,
        GAME_ID,
        None,
        &ps2_identity(),
        &cache.root,
    )
    .expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::WrongPlatform);
}

#[test]
fn identity_is_only_ever_built_from_a_verified_local_report() {
    let unverified = GameCubeGameIdentity {
        archive_path: PathBuf::from("/library/Luigi's Mansion.rvz"),
        title: "Luigi's Mansion".to_string(),
        dolphin_game_id: None,
        region: Some("E".to_string()),
        revision: None,
        loose_rom_sha256: None,
        state: GameCubeIdentityState::MissingGameId,
        evidence: Vec::new(),
        plain_failure_reason: None,
    };
    let failure = BrowserImportLocalIdentity::from_gamecube(&unverified).expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::IdentityIncomplete);

    let verified = GameCubeGameIdentity {
        dolphin_game_id: Some(DOLPHIN_GAME_ID.to_string()),
        state: GameCubeIdentityState::Verified,
        ..unverified
    };
    assert_eq!(
        BrowserImportLocalIdentity::from_gamecube(&verified).expect("accepted"),
        gamecube_identity()
    );
}

// --- 25. PlayStation 2 ---------------------------------------------------

#[test]
fn a_valid_ps2_export_import_populates_the_providers_own_pnach_cache_key() {
    let cache = TempCache::new("ps2-export");
    let outcome = import_gamehacking_browser_content(&ps2_request(&cache, pasted(PS2_EXPORT)))
        .expect("valid PCSX2 export imports");

    assert_eq!(outcome.kind, BrowserImportKind::TextExport);
    assert_eq!(outcome.cache_path, cache.path("export-70123.pnach"));
    assert_eq!(outcome.cheat_count, 2);
    assert_eq!(
        outcome.imported_title.as_deref(),
        Some("Ratchet & Clank (USA)")
    );
    assert_eq!(
        read_browser_import_provenance(&outcome.cache_path)
            .expect("provenance")
            .platform,
        BrowserImportPlatform::PlayStation2
    );
}

#[test]
fn a_ps2_export_for_another_game_is_rejected() {
    let cache = TempCache::new("ps2-wrong-game");
    let failure =
        import_gamehacking_browser_content(&ps2_request(&cache, pasted(PS2_EXPORT_OTHER_GAME)))
            .expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::WrongGame);
    assert!(failure.detail.contains("Gran Turismo 4"));
    assert!(!cache.path("export-70123.pnach").exists());
}

#[test]
fn a_ps2_export_with_no_gamehacking_evidence_is_rejected() {
    let cache = TempCache::new("ps2-no-evidence");
    let failure = import_gamehacking_browser_content(&ps2_request(
        &cache,
        pasted(b"[Player Codes\\Infinite Bolts]\npatch=1,EE,2019ABCD,word,0000FFFF\n"),
    ))
    .expect_err("refused");
    assert_eq!(
        failure.kind,
        BrowserImportErrorKind::MissingIdentityEvidence
    );
}

#[test]
fn a_ps2_game_page_is_refused_with_a_specific_reason_not_a_generic_error() {
    let cache = TempCache::new("ps2-page");
    let mut request = ps2_request(&cache, pasted(PS2_SAVED_PAGE));
    request.kind = Some(BrowserImportKind::GamePageHtml);
    let failure = import_gamehacking_browser_content(&request).expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::UnsupportedPageShape);
    assert!(
        failure.detail.contains("reads only the cheat export"),
        "{}",
        failure.detail
    );
    assert_eq!(
        BrowserImportPlatform::PlayStation2.accepted_kinds(),
        &[BrowserImportKind::TextExport]
    );
}

// --- Kind detection and destination safety -------------------------------

#[test]
fn the_import_kind_comes_from_the_content_never_the_extension() {
    // A saved page stored with a .txt extension is still a page.
    let cache = TempCache::new("kind-detection");
    let source = saved_file(&cache, "saved page.txt", SAVED_PAGE);
    let outcome =
        import_gamehacking_browser_content(&gamecube_request(&cache, source)).expect("import");
    assert_eq!(outcome.kind, BrowserImportKind::GamePageHtml);
    assert_eq!(outcome.cache_path, cache.path("game-54172.html"));

    // And an export stored with a .html extension is still an export.
    let cache = TempCache::new("kind-detection-2");
    let source = saved_file(&cache, "export.html", TEXT_EXPORT);
    let outcome =
        import_gamehacking_browser_content(&gamecube_request(&cache, source)).expect("import");
    assert_eq!(outcome.kind, BrowserImportKind::TextExport);
    assert_eq!(outcome.cache_path, cache.path("export-54172.txt"));
}

#[test]
fn an_explicit_kind_is_honoured_but_still_fully_validated() {
    let cache = TempCache::new("explicit-kind");
    let mut request = gamecube_request(&cache, pasted(EXPORT_WRONG_GAME_ID));
    request.kind = Some(BrowserImportKind::TextExport);
    let failure = import_gamehacking_browser_content(&request).expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::GameIdMismatch);
}

#[test]
fn a_cache_destination_is_derived_only_from_the_platform_and_numeric_game_id() {
    for (platform, kind, expected) in [
        (
            BrowserImportPlatform::GameCube,
            BrowserImportKind::GamePageHtml,
            Some("game-7.html"),
        ),
        (
            BrowserImportPlatform::GameCube,
            BrowserImportKind::TextExport,
            Some("export-7.txt"),
        ),
        (
            BrowserImportPlatform::PlayStation2,
            BrowserImportKind::TextExport,
            Some("export-7.pnach"),
        ),
        (
            BrowserImportPlatform::PlayStation2,
            BrowserImportKind::GamePageHtml,
            None,
        ),
    ] {
        assert_eq!(
            kind.cache_file_name(platform, 7).as_deref(),
            expected,
            "no part of the imported content can influence this name"
        );
    }
}

#[test]
fn a_relative_or_root_cache_root_is_refused() {
    let cache = TempCache::new("bad-root");
    let mut request = gamecube_request(&cache, pasted(SAVED_PAGE));
    request.cache_root = PathBuf::from("relative/cache");
    let failure = import_gamehacking_browser_content(&request).expect_err("refused");
    assert_eq!(failure.kind, BrowserImportErrorKind::CacheWriteFailed);
}

#[test]
fn platform_and_kind_slugs_round_trip_for_the_cli() {
    assert_eq!(
        BrowserImportPlatform::parse_slug("gamecube"),
        Some(BrowserImportPlatform::GameCube)
    );
    assert_eq!(
        BrowserImportPlatform::parse_slug("PS2"),
        Some(BrowserImportPlatform::PlayStation2)
    );
    assert_eq!(
        BrowserImportKind::parse_slug("export"),
        Some(BrowserImportKind::TextExport)
    );
    assert_eq!(
        BrowserImportKind::parse_slug("page"),
        Some(BrowserImportKind::GamePageHtml)
    );
    assert_eq!(BrowserImportKind::parse_slug("gct"), None);
}

#[test]
fn wii_is_not_offered_by_this_milestone() {
    assert_eq!(BrowserImportPlatform::parse_slug("wii"), None);
    assert_eq!(
        BrowserImportPlatform::GameCube.gamehacking_system_slug(),
        "ngc"
    );
    assert_eq!(
        BrowserImportPlatform::PlayStation2.gamehacking_system_slug(),
        "ps2"
    );
}

#[test]
fn the_blocked_banner_wording_is_exactly_what_the_gui_must_show() {
    assert_eq!(
        GAMEHACKING_BROWSER_IMPORT_BLOCKED_TITLE,
        "GameHacking.org access blocked"
    );
    assert_eq!(
        GAMEHACKING_BROWSER_IMPORT_BLOCKED_BODY,
        "ArchiveFS cannot fetch this page automatically, but you can open it in your browser and import the page or Text export."
    );
}
