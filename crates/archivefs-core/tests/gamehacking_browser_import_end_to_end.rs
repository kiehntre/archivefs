//! End-to-end behaviour for browser-assisted GameHacking.org imports:
//! an imported cache is consumed by the *normal* GameCube provider with
//! no network at all, and the cheats it yields install through the
//! existing Dolphin GameSettings flow with every existing safety rule
//! still in force.
//!
//! No test in this file can make a live GameHacking.org request, and that
//! is enforced rather than assumed: every fixture seeds an *active*
//! Cloudflare cooldown marker (see `seed_offline_cache`). The provider's
//! own `cached_request` refuses to attempt any live request while that
//! marker is fresh, so the only thing it can possibly serve is the
//! imported cache - which is exactly the claim under test.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use archivefs_core::patch_manager::{
    BrowserImportKind, BrowserImportLocalIdentity, BrowserImportPlatform, BrowserImportRequest,
    BrowserImportSource, BrowserImportTextOrigin, GameCubeCheatSelection, GameCubeCodeFormat,
    GameCubeGameHackingInstallPreviewRequest, GameCubeGameIdentity, GameCubeIdentityState,
    GameHackingErrorKind, GameHackingGameCubeFetchOptions, GameHackingGameCubeGame,
    GameHackingGameCubeProvider, MANUAL_BROWSER_IMPORT_SOURCE, SharedApplyConfirmation,
    SharedApplyOptions, SharedApplyStatus, build_gamecube_gamehacking_install_preview,
    build_shared_transaction_plan, execute_shared_apply, import_gamehacking_browser_content,
    load_dolphin_destination, managed_names, parse_dolphin_ini, read_browser_import_provenance,
    require_dolphin_managed_gamehacking_verification, stage_gamecube_gamehacking_install,
};

const SAVED_PAGE: &[u8] =
    include_bytes!("fixtures/gamehacking/gamecube-browser-saved-page-54172.html");
const TEXT_EXPORT: &[u8] = include_bytes!("fixtures/gamehacking/gamecube-browser-export-54172.txt");
const GAME_ID: u64 = 54172;
const DOLPHIN_GAME_ID: &str = "GLME01";
const EXISTING_INI: &str = "[Core]\nFastDiscSpeed = True\n";

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-browser-import-e2e-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("fixture root");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("fixture dir");
        path
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(&path, contents).expect("write");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn local_identity(archive: &Path) -> GameCubeGameIdentity {
    GameCubeGameIdentity {
        archive_path: archive.to_path_buf(),
        title: "Luigi's Mansion".to_string(),
        dolphin_game_id: Some(DOLPHIN_GAME_ID.to_string()),
        region: Some("E".to_string()),
        revision: None,
        loose_rom_sha256: None,
        state: GameCubeIdentityState::Verified,
        evidence: vec!["disc header".to_string()],
        plain_failure_reason: None,
    }
}

fn catalogue_game() -> GameHackingGameCubeGame {
    GameHackingGameCubeGame {
        game_id: GAME_ID,
        title: "Luigi's Mansion".to_string(),
        system: "GameCube".to_string(),
        region: Some("USA".to_string()),
        dolphin_game_id: Some(DOLPHIN_GAME_ID.to_string()),
        revision: None,
        hash: None,
        source_url: format!("https://gamehacking.org/game/{GAME_ID}"),
    }
}

fn import_request(
    cache_root: &Path,
    identity: &GameCubeGameIdentity,
    bytes: &[u8],
) -> BrowserImportRequest {
    BrowserImportRequest {
        platform: BrowserImportPlatform::GameCube,
        game_id: GAME_ID,
        source_url: Some(format!("https://gamehacking.org/game/{GAME_ID}")),
        candidate_title: "Luigi's Mansion".to_string(),
        identity: BrowserImportLocalIdentity::from_gamecube(identity)
            .expect("a verified local identity"),
        cache_root: cache_root.to_path_buf(),
        kind: None,
        source: BrowserImportSource::Text {
            text: String::from_utf8(bytes.to_vec()).expect("utf-8 fixture"),
            origin: BrowserImportTextOrigin::PastedText,
        },
    }
}

/// Seeds everything the provider needs *besides* the imported content,
/// and makes a live request structurally impossible:
///
/// - a permissive cached `robots.txt`, so the robots check is satisfied
///   from cache;
/// - a fresh `cloudflare-blocked-at` marker, which
///   `cloudflare_cooldown_remaining` reads and `cached_request` honours by
///   never attempting a live request at all.
///
/// Nothing here weakens the real cooldown behaviour - it is used exactly
/// as shipped, as the offline guarantee for these tests.
fn seed_offline_cache(cache_root: &Path) {
    fs::create_dir_all(cache_root).expect("cache root");
    fs::write(cache_root.join("robots.txt"), b"User-agent: *\nDisallow:\n")
        .expect("cached robots.txt");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    fs::write(
        cache_root.join("cloudflare-blocked-at"),
        now.to_string().as_bytes(),
    )
    .expect("active cooldown marker");
}

fn fetch_options(cache_root: &Path) -> GameHackingGameCubeFetchOptions {
    GameHackingGameCubeFetchOptions {
        cache_root: cache_root.to_path_buf(),
        force_refresh: false,
        // Never used: every request below is served from cache. Kept at
        // zero so a regression that *did* reach out would not also hang.
        delay: Duration::from_secs(0),
        cancellation: None,
    }
}

#[test]
fn an_imported_page_and_export_are_consumed_by_the_normal_gamecube_provider() {
    let fixture = Fixture::new("provider");
    let cache_root = fixture.path("cache/gamehacking");
    let archive = fixture.write("library/Luigis Mansion (USA).iso", "iso bytes");
    let identity = local_identity(&archive);

    let page =
        import_gamehacking_browser_content(&import_request(&cache_root, &identity, SAVED_PAGE))
            .expect("page imports");
    let export =
        import_gamehacking_browser_content(&import_request(&cache_root, &identity, TEXT_EXPORT))
            .expect("export imports");
    assert_eq!(page.cache_path, cache_root.join("game-54172.html"));
    assert_eq!(export.cache_path, cache_root.join("export-54172.txt"));

    seed_offline_cache(&cache_root);
    let provider = GameHackingGameCubeProvider::default();
    let cheats = provider
        .fetch_cheats_for_confirmed_candidate(
            &identity,
            &catalogue_game(),
            &fetch_options(&cache_root),
        )
        .expect("the provider reads the imported cache with no network access");

    assert_eq!(cheats.len(), 6);
    let by_name = |name: &str| {
        cheats
            .iter()
            .find(|cheat| cheat.name == name)
            .unwrap_or_else(|| panic!("{name} is present"))
    };
    // The page's own explicit labels reached the provider's own result -
    // this is the whole point of importing both artefacts.
    assert_eq!(
        by_name("Element Modifier").code_format,
        GameCubeCodeFormat::Gecko
    );
    assert_eq!(
        by_name("999 Cash").code_format,
        GameCubeCodeFormat::ActionReplay
    );
    assert_eq!(
        by_name("Not On The Page").code_format,
        GameCubeCodeFormat::RawUnknown,
        "an unlabelled cheat is never promoted, even through the provider"
    );

    // And the imported entries are still distinguishable from live ones.
    assert_eq!(
        read_browser_import_provenance(&export.cache_path)
            .expect("provenance survives being read by the provider")
            .source,
        MANUAL_BROWSER_IMPORT_SOURCE
    );
    assert_eq!(
        read_browser_import_provenance(&page.cache_path)
            .expect("page provenance")
            .import_kind,
        BrowserImportKind::GamePageHtml
    );
}

#[test]
fn imported_classified_cheats_install_through_the_existing_dolphin_flow() {
    let fixture = Fixture::new("install");
    let cache_root = fixture.path("cache/gamehacking");
    let archive = fixture.write("library/Luigis Mansion (USA).iso", "iso bytes");
    let identity = local_identity(&archive);
    let configuration_path = fixture.dir("dolphin");
    fixture.write("dolphin/GameSettings/GLME01.ini", EXISTING_INI);

    import_gamehacking_browser_content(&import_request(&cache_root, &identity, SAVED_PAGE))
        .expect("page imports");
    import_gamehacking_browser_content(&import_request(&cache_root, &identity, TEXT_EXPORT))
        .expect("export imports");
    seed_offline_cache(&cache_root);

    let cheats = GameHackingGameCubeProvider::default()
        .fetch_cheats_for_confirmed_candidate(
            &identity,
            &catalogue_game(),
            &fetch_options(&cache_root),
        )
        .expect("cheats come from the imported cache");

    let destination =
        load_dolphin_destination(&configuration_path, DOLPHIN_GAME_ID).expect("destination loads");
    let mut selection = GameCubeCheatSelection::from_cheats(&cheats, &destination.document);
    selection.select_all();
    let selectable = cheats
        .iter()
        .filter(|cheat| {
            matches!(
                cheat.code_format,
                GameCubeCodeFormat::ActionReplay | GameCubeCodeFormat::Gecko
            )
        })
        .count();
    assert_eq!(
        selection.selected_count(),
        selectable,
        "only explicitly labelled cheats can ever be selected"
    );
    assert_eq!(selectable, 5, "4 ARMax plus 1 Gecko survive enrichment");

    let staging_root = fixture.path("managed/generated-gamecube-gamehacking");
    let staged = stage_gamecube_gamehacking_install(
        &staging_root,
        "GLME01.ini",
        &destination.document,
        destination.existed,
        &cheats,
        &selection,
    )
    .expect("install stages cleanly");
    let preview =
        build_gamecube_gamehacking_install_preview(&GameCubeGameHackingInstallPreviewRequest {
            selected_archive: archive.clone(),
            configuration_path: configuration_path.clone(),
            game_id: DOLPHIN_GAME_ID.to_string(),
            revision: None,
            staged: staged.clone(),
        })
        .expect("preview builds");

    let mut plan = build_shared_transaction_plan(
        &preview.report,
        "browser-import-e2e-profile",
        "Dolphin GameSettings",
        &staging_root,
    )
    .expect("plan builds");
    let source = preview.report.entries[0]
        .source_path
        .as_ref()
        .expect("staged source path");
    let staged_contents = fs::read_to_string(source).expect("staged source readable");
    require_dolphin_managed_gamehacking_verification(
        &mut plan,
        managed_names(&parse_dolphin_ini(&staged_contents))
            .into_iter()
            .collect(),
    )
    .expect("the same semantic verification contract still attaches");

    let result = execute_shared_apply(
        &plan,
        &SharedApplyOptions {
            dry_run: false,
            confirmation: Some(SharedApplyConfirmation {
                plan_id: plan.plan_id.clone(),
                general_approved: true,
                replacement_approved: true,
            }),
            operation_id: "browser-import-e2e-op".to_string(),
            timestamp_unix_seconds: 1_700_000_500,
            current_context: plan.context.clone(),
            history_root: fixture.dir("managed/history"),
            backup_root: fixture.dir("managed/backups"),
        },
    );
    assert_eq!(result.journal.status, SharedApplyStatus::Success);
    assert!(
        result.journal_path.is_some(),
        "History and Undo get their journal exactly as before"
    );

    let installed = fs::read_to_string(configuration_path.join("GameSettings/GLME01.ini"))
        .expect("installed file");
    assert_eq!(installed, staged.contents, "exactly the previewed bytes");
    assert!(installed.contains("[Core]\nFastDiscSpeed = True\n"));
    assert!(installed.contains("999 Cash [Codejunkies]"));
    assert!(installed.contains("Element Modifier [Link Master]"));
    // 24. RawUnknown remains non-installable, all the way to disk.
    assert!(
        !installed.contains("Not On The Page"),
        "an unlabelled imported cheat is never written to Dolphin's own file"
    );
}

#[test]
fn without_the_import_a_blocked_provider_has_nothing_to_serve() {
    let fixture = Fixture::new("offline");
    let cache_root = fixture.path("cache/gamehacking");
    let archive = fixture.write("library/Luigis Mansion (USA).iso", "iso bytes");
    let identity = local_identity(&archive);

    // The cooldown marker is active, so the provider will not attempt a
    // live request. With no imported cache either, it must fail loudly as
    // blocked rather than invent a result - which is what makes the two
    // passing tests above meaningful rather than accidental.
    seed_offline_cache(&cache_root);
    let before = GameHackingGameCubeProvider::default().fetch_cheats_for_confirmed_candidate(
        &identity,
        &catalogue_game(),
        &fetch_options(&cache_root),
    );
    let failure = before.expect_err("nothing to serve while blocked with no cache");
    assert_eq!(failure.kind, GameHackingErrorKind::CloudflareBlocked);

    import_gamehacking_browser_content(&import_request(&cache_root, &identity, TEXT_EXPORT))
        .expect("export imports");
    let after = GameHackingGameCubeProvider::default()
        .fetch_cheats_for_confirmed_candidate(
            &identity,
            &catalogue_game(),
            &fetch_options(&cache_root),
        )
        .expect("the import alone is enough");
    assert_eq!(after.len(), 6);
    assert!(
        after
            .iter()
            .all(|cheat| cheat.code_format == GameCubeCodeFormat::RawUnknown),
        "with no page imported there is no label to apply, and none is guessed"
    );
}
