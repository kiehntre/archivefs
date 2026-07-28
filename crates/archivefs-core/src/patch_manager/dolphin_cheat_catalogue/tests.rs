use std::cell::Cell;
use std::io::{Cursor, Write as _};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::*;
use crate::patch_manager::{
    CheatSourceError, CheatSourceErrorStage, CheatSourceHttpResponse,
    fetch_dolphin_upstream_gecko_with_transport,
};

static CACHE_COUNTER: AtomicU64 = AtomicU64::new(0);

struct CacheFixture(PathBuf);

impl CacheFixture {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        Self(std::env::temp_dir().join(format!(
            "archivefs-dolphin-catalogue-{label}-{}-{nonce}-{}",
            std::process::id(),
            CACHE_COUNTER.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for CacheFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const GAFE01: &str = "# GAFE01 - Animal Crossing\n\n[Gecko]\n$16:9 Widescreen\n040037A0 3C608000\n040037A4 C38337AC\n";
const NO_GECKO_INI: &str = "# GALE01 - Super Smash Bros. Melee\n\n[Core]\nCPUThread = True\n";
const MALFORMED_INI: &str = "# GAFE02 - Bad File\n\n[Gecko]\n$Bad\nnot code\n$Same\n040037A0 3C608000\n$Same\n040037A4 C38337AC\n";

fn zip_with_entries(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o100644);
    for (name, bytes) in entries {
        writer.start_file(*name, options).expect("start_file");
        writer.write_all(bytes).expect("write entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

fn dir_entry_zip(entries: &[(&str, &[u8])], directory: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let dir_options = SimpleFileOptions::default().unix_permissions(0o040755);
    writer
        .add_directory(directory, dir_options)
        .expect("add_directory");
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o100644);
    for (name, bytes) in entries {
        writer.start_file(*name, options).expect("start_file");
        writer.write_all(bytes).expect("write entry");
    }
    writer.finish().expect("finish zip").into_inner()
}

struct FakeTransport {
    revision_body: Vec<u8>,
    archive_body: Vec<u8>,
    calls: Cell<usize>,
}

impl FakeTransport {
    fn new(commit: &str, archive_body: Vec<u8>) -> Self {
        Self {
            revision_body: format!("{{\"sha\": \"{commit}\"}}").into_bytes(),
            archive_body,
            calls: Cell::new(0),
        }
    }
}

impl CheatSourceTransport for FakeTransport {
    fn get(
        &self,
        url: &str,
        _maximum_bytes: u64,
        destination: &mut dyn Write,
        _context: CheatSourceTransferContext<'_>,
    ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
        self.calls.set(self.calls.get() + 1);
        let body = if url.contains("api.github.com") {
            &self.revision_body
        } else {
            &self.archive_body
        };
        destination.write_all(body).expect("fixture write");
        Ok(CheatSourceHttpResponse {
            status: 200,
            content_type: None,
            content_encoding: None,
            content_length: Some(body.len() as u64),
            location: None,
            etag: None,
            last_modified: None,
            downloaded_bytes: body.len() as u64,
            retry_after_seconds: None,
        })
    }
}

struct FailingTransport(&'static str);

impl CheatSourceTransport for FailingTransport {
    fn get(
        &self,
        _url: &str,
        _maximum_bytes: u64,
        _destination: &mut dyn Write,
        _context: CheatSourceTransferContext<'_>,
    ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
        Err(CheatSourceError::new(
            CheatSourceErrorStage::Network,
            self.0,
            "recorded transport failure",
        ))
    }
}

const COMMIT: &str = "d742aa8b4c4d052f7dceaa39022b1fe3996f1781";

fn options(root: &Path) -> DolphinCatalogueFetchOptions {
    DolphinCatalogueFetchOptions {
        cache_root: root.to_path_buf(),
        cancellation: None,
        progress: None,
    }
}

#[test]
fn full_archive_download_parses_indexes_and_activates_atomically() {
    let cache = CacheFixture::new("download");
    let archive = zip_with_entries(&[
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
            GAFE01.as_bytes(),
        ),
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GALE01.ini"),
            NO_GECKO_INI.as_bytes(),
        ),
        (
            &format!("dolphin-{COMMIT}/Source/Core/Main.cpp"),
            b"int main() { return 0; }",
        ),
    ]);
    let transport = FakeTransport::new(COMMIT, archive);
    let result = fetch_dolphin_catalogue_with_transport(&options(&cache.0), &transport)
        .expect("catalogue fetch succeeds");

    assert_eq!(result.catalogue.metadata.resolved_commit, COMMIT);
    assert_eq!(result.catalogue.metadata.games_with_usable_gecko, 1);
    assert_eq!(result.catalogue.metadata.total_usable_gecko_entries, 1);
    assert_eq!(result.catalogue.games.len(), 2);

    let found = result.catalogue.find("GAFE01").expect("GAFE01 indexed");
    assert!(found.has_usable_gecko());
    assert_eq!(found.title.as_deref(), Some("Animal Crossing"));

    let no_gecko = result.catalogue.find("GALE01").expect("GALE01 indexed");
    assert!(!no_gecko.has_usable_gecko());

    let loaded = load_dolphin_catalogue(&cache.0).expect("catalogue loads back");
    match loaded {
        DolphinCatalogueLoad::Ready(catalogue) => {
            assert_eq!(catalogue.metadata.resolved_commit, COMMIT);
        }
        DolphinCatalogueLoad::NotInstalled => panic!("catalogue must be installed after fetch"),
    }
}

#[test]
fn non_gamesettings_paths_are_never_extracted_and_wildcard_names_are_skipped_honestly() {
    let cache = CacheFixture::new("filter");
    let archive = zip_with_entries(&[
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
            GAFE01.as_bytes(),
        ),
        // Shorter-than-six-character wildcard filenames are intentionally
        // out of scope for exact-Game-ID indexing.
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAF.ini"),
            b"# wildcard\n[Gecko]\n$Should not be indexed\n00000000 00000000\n",
        ),
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/nested/GAFE03.ini"),
            b"# nested, must not match\n",
        ),
    ]);
    let transport = FakeTransport::new(COMMIT, archive);
    let result = fetch_dolphin_catalogue_with_transport(&options(&cache.0), &transport)
        .expect("catalogue fetch succeeds");

    assert_eq!(result.catalogue.games.len(), 1);
    assert!(result.catalogue.find("GAFE01").is_some());
    assert!(result.catalogue.metadata.non_matching_files_skipped >= 2);
}

#[test]
fn malformed_and_duplicate_gecko_bodies_are_blocked_not_repaired() {
    let cache = CacheFixture::new("malformed");
    let archive = zip_with_entries(&[(
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE02.ini"),
        MALFORMED_INI.as_bytes(),
    )]);
    let transport = FakeTransport::new(COMMIT, archive);
    let result = fetch_dolphin_catalogue_with_transport(&options(&cache.0), &transport)
        .expect("catalogue fetch succeeds");

    let game = result.catalogue.find("GAFE02").expect("GAFE02 indexed");
    assert!(!game.has_usable_gecko());
    assert!(game.codes.iter().all(|code| !code.safe_to_offer));
    assert_eq!(result.catalogue.metadata.games_with_usable_gecko, 0);
}

#[test]
fn unsafe_archive_traversal_entries_are_rejected() {
    let cache = CacheFixture::new("traversal");
    let traversal = zip_with_entries(&[(
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/../../../etc/passwd"),
        b"hostile",
    )]);
    let transport = FakeTransport::new(COMMIT, traversal);
    let error = fetch_dolphin_catalogue_with_transport(&options(&cache.0), &transport)
        .expect_err("traversal entry must be rejected");
    assert_eq!(error.kind, DolphinCatalogueErrorKind::Archive);
}

/// `extract_and_parse_game_settings` calls the same `validate_unix_entry_mode`
/// that `cheat_sources.rs` already unit-tests exhaustively against every
/// POSIX special-file bit pattern (symlink, device, FIFO, socket) - the zip
/// write crate used to build fixtures here masks entries down to permission
/// bits only (`unix_permissions() & 0o777`), so a real symlink-moded zip
/// entry cannot be constructed through its public API for an end-to-end
/// fixture. This asserts the same shared validator the extraction loop
/// calls still rejects a symlink entry.
#[test]
fn symlink_moded_archive_entries_are_rejected_by_the_shared_validator() {
    let error =
        validate_unix_entry_mode(0o120_777, "entry").expect_err("symlink mode must be rejected");
    assert_eq!(error.code, "special_entry_rejected");
}

#[test]
fn interrupted_and_failed_updates_preserve_the_previous_catalogue() {
    let cache = CacheFixture::new("preserve");
    let archive = zip_with_entries(&[(
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
        GAFE01.as_bytes(),
    )]);
    fetch_dolphin_catalogue_with_transport(
        &options(&cache.0),
        &FakeTransport::new(COMMIT, archive),
    )
    .expect("initial catalogue install");

    let failure = FailingTransport("overall_timeout");
    let error = fetch_dolphin_catalogue_with_transport(&options(&cache.0), &failure)
        .expect_err("network failure during update must surface");
    assert_eq!(error.kind, DolphinCatalogueErrorKind::Network);

    let loaded = load_dolphin_catalogue(&cache.0).expect("previous catalogue still loads");
    match loaded {
        DolphinCatalogueLoad::Ready(catalogue) => {
            assert_eq!(catalogue.metadata.resolved_commit, COMMIT);
            assert!(catalogue.find("GAFE01").is_some());
        }
        DolphinCatalogueLoad::NotInstalled => panic!("previous catalogue must remain usable"),
    }
}

#[test]
fn no_catalogue_lookup_and_ready_lookup_states_are_distinguished() {
    let cache = CacheFixture::new("lookup-states");
    assert_eq!(
        load_dolphin_catalogue(&cache.0).expect("missing catalogue is not an error"),
        DolphinCatalogueLoad::NotInstalled
    );

    let archive = zip_with_entries(&[
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
            GAFE01.as_bytes(),
        ),
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GALE01.ini"),
            NO_GECKO_INI.as_bytes(),
        ),
    ]);
    let result = fetch_dolphin_catalogue_with_transport(
        &options(&cache.0),
        &FakeTransport::new(COMMIT, archive),
    )
    .expect("catalogue install");

    assert_eq!(
        lookup_dolphin_catalogue(&result.catalogue, "GAFE01", &GeckoRegion::Usa),
        DolphinCatalogueLookup::Found(result.catalogue.find("GAFE01").unwrap())
    );
    assert!(matches!(
        lookup_dolphin_catalogue(&result.catalogue, "GALE01", &GeckoRegion::Usa),
        DolphinCatalogueLookup::NoUsableGecko { .. }
    ));
    assert_eq!(
        lookup_dolphin_catalogue(&result.catalogue, "ZZZZZZ", &GeckoRegion::Usa),
        DolphinCatalogueLookup::NotFound
    );
    assert_eq!(
        lookup_dolphin_catalogue(&result.catalogue, "GAFE01", &GeckoRegion::Europe),
        DolphinCatalogueLookup::RegionMismatch
    );
}

#[test]
fn removing_the_catalogue_only_touches_archivefs_cache_files() {
    let cache = CacheFixture::new("remove");
    let archive = zip_with_entries(&[(
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
        GAFE01.as_bytes(),
    )]);
    fetch_dolphin_catalogue_with_transport(
        &options(&cache.0),
        &FakeTransport::new(COMMIT, archive),
    )
    .expect("catalogue install");
    assert!(cache.0.join("catalogue.json").exists());

    remove_dolphin_catalogue(&cache.0).expect("removal succeeds");
    assert!(!cache.0.join("catalogue.json").exists());
    assert_eq!(
        load_dolphin_catalogue(&cache.0).expect("post-removal load"),
        DolphinCatalogueLoad::NotInstalled
    );
}

#[test]
fn update_check_reports_availability_without_downloading_the_archive() {
    let cache = CacheFixture::new("update-check");
    let archive = zip_with_entries(&[(
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
        GAFE01.as_bytes(),
    )]);
    fetch_dolphin_catalogue_with_transport(
        &options(&cache.0),
        &FakeTransport::new(COMMIT, archive),
    )
    .expect("catalogue install");

    let same_commit_transport = FakeTransport::new(COMMIT, Vec::new());
    let check = check_dolphin_catalogue_update_with_transport(&cache.0, &same_commit_transport)
        .expect("update check succeeds");
    assert!(!check.update_available);
    assert_eq!(same_commit_transport.calls.get(), 1);

    let newer_commit: String = "ab".repeat(20);
    assert_eq!(newer_commit.len(), 40);
    let newer_transport = FakeTransport::new(&newer_commit, Vec::new());
    let check2 = check_dolphin_catalogue_update_with_transport(&cache.0, &newer_transport)
        .expect("update check succeeds");
    assert!(check2.update_available);

    let state = load_dolphin_catalogue_update_state(&cache.0).expect("state loads");
    assert!(state.last_check_unix_seconds.is_some());
}

#[test]
fn oversized_declared_download_is_rejected_without_writing_a_partial_catalogue() {
    let cache = CacheFixture::new("oversized");
    struct OversizedTransport;
    impl CheatSourceTransport for OversizedTransport {
        fn get(
            &self,
            url: &str,
            _maximum_bytes: u64,
            destination: &mut dyn Write,
            _context: CheatSourceTransferContext<'_>,
        ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
            if url.contains("api.github.com") {
                let body = format!("{{\"sha\": \"{COMMIT}\"}}").into_bytes();
                destination.write_all(&body).unwrap();
                return Ok(CheatSourceHttpResponse {
                    status: 200,
                    content_type: None,
                    content_encoding: None,
                    content_length: Some(body.len() as u64),
                    location: None,
                    etag: None,
                    last_modified: None,
                    downloaded_bytes: body.len() as u64,
                    retry_after_seconds: None,
                });
            }
            Ok(CheatSourceHttpResponse {
                status: 200,
                content_type: None,
                content_encoding: None,
                content_length: Some(DOLPHIN_CATALOGUE_MAX_DOWNLOAD_BYTES + 1),
                location: None,
                etag: None,
                last_modified: None,
                downloaded_bytes: DOLPHIN_CATALOGUE_MAX_DOWNLOAD_BYTES + 1,
                retry_after_seconds: None,
            })
        }
    }
    let error = fetch_dolphin_catalogue_with_transport(&options(&cache.0), &OversizedTransport)
        .expect_err("oversized download must fail");
    assert_eq!(error.kind, DolphinCatalogueErrorKind::DownloadTooLarge);
    assert!(load_dolphin_catalogue(&cache.0).unwrap() == DolphinCatalogueLoad::NotInstalled);
}

#[test]
fn nested_directory_entries_before_gamesettings_do_not_confuse_extraction() {
    let cache = CacheFixture::new("directory-entries");
    let archive = dir_entry_zip(
        &[(
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
            GAFE01.as_bytes(),
        )],
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/"),
    );
    let result = fetch_dolphin_catalogue_with_transport(
        &options(&cache.0),
        &FakeTransport::new(COMMIT, archive),
    )
    .expect("catalogue fetch succeeds with directory entries present");
    assert_eq!(result.catalogue.games.len(), 1);
}

#[test]
fn gecko_lookup_prefers_catalogue_then_falls_back_to_cached_single_game_then_neither() {
    let catalogue_cache = CacheFixture::new("lookup-catalogue");
    let provider_cache = CacheFixture::new("lookup-provider");

    // No catalogue, no cached single-game result: nothing to show, but the
    // caller can still distinguish "no catalogue" from every other case.
    let outcome = resolve_dolphin_gecko_lookup(
        &catalogue_cache.0,
        &provider_cache.0,
        "GAFE01",
        &GeckoRegion::Usa,
        0,
    )
    .expect("lookup does not error without a catalogue");
    assert_eq!(
        outcome,
        DolphinGeckoLookupResult::NoCatalogueInstalled { cached: None }
    );

    // A validated cached single-game result becomes the fallback once no
    // catalogue is installed.
    let provider_query = GeckoProviderQuery {
        game_id: "GAFE01".to_string(),
        region: GeckoRegion::Usa,
        revision: 0,
    };
    let provider_options = crate::patch_manager::GeckoProviderFetchOptions {
        cache_root: provider_cache.0.clone(),
        force_refresh: false,
        now_unix_seconds: 1_700_000_000,
    };
    // Populate the provider's own cache the same way a prior explicit
    // per-game fetch would have.
    fetch_dolphin_upstream_gecko_with_transport(
        &provider_query,
        &provider_options,
        &SingleGameFakeTransport(GAFE01.as_bytes().to_vec()),
    )
    .expect("seed the single-game provider cache");

    let outcome = resolve_dolphin_gecko_lookup(
        &catalogue_cache.0,
        &provider_cache.0,
        "GAFE01",
        &GeckoRegion::Usa,
        0,
    )
    .expect("lookup does not error");
    assert!(matches!(
        outcome,
        DolphinGeckoLookupResult::NoCatalogueInstalled { cached: Some(_) }
    ));

    // Once a catalogue is installed and has this Game ID, it takes priority
    // over the cached single-game result.
    let archive = zip_with_entries(&[(
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
        GAFE01.as_bytes(),
    )]);
    fetch_dolphin_catalogue_with_transport(
        &options(&catalogue_cache.0),
        &FakeTransport::new(COMMIT, archive),
    )
    .expect("catalogue install");

    let outcome = resolve_dolphin_gecko_lookup(
        &catalogue_cache.0,
        &provider_cache.0,
        "GAFE01",
        &GeckoRegion::Usa,
        0,
    )
    .expect("lookup does not error");
    match outcome {
        DolphinGeckoLookupResult::Found(result) => {
            assert_eq!(result.provider_id, DOLPHIN_CATALOGUE_PROVIDER_ID);
            assert_eq!(result.entries.len(), 1);
        }
        other => panic!("expected the catalogue result to take priority, got {other:?}"),
    }
}

#[test]
fn rebuild_index_reuses_the_pinned_commit_without_resolving_master_again() {
    let cache = CacheFixture::new("rebuild");
    let archive = zip_with_entries(&[(
        &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
        GAFE01.as_bytes(),
    )]);
    fetch_dolphin_catalogue_with_transport(
        &options(&cache.0),
        &FakeTransport::new(COMMIT, archive),
    )
    .expect("initial catalogue install");

    struct RebuildOnlyTransport(Vec<u8>);
    impl CheatSourceTransport for RebuildOnlyTransport {
        fn get(
            &self,
            url: &str,
            _maximum_bytes: u64,
            destination: &mut dyn Write,
            _context: CheatSourceTransferContext<'_>,
        ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
            assert!(
                !url.contains("api.github.com"),
                "rebuild must not resolve the moving master reference"
            );
            destination.write_all(&self.0).expect("fixture write");
            Ok(CheatSourceHttpResponse {
                status: 200,
                content_type: None,
                content_encoding: None,
                content_length: Some(self.0.len() as u64),
                location: None,
                etag: None,
                last_modified: None,
                downloaded_bytes: self.0.len() as u64,
                retry_after_seconds: None,
            })
        }
    }
    let updated_archive = zip_with_entries(&[
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GAFE01.ini"),
            GAFE01.as_bytes(),
        ),
        (
            &format!("dolphin-{COMMIT}/Data/Sys/GameSettings/GALE01.ini"),
            NO_GECKO_INI.as_bytes(),
        ),
    ]);
    let result = rebuild_dolphin_catalogue_index_with_transport(
        &options(&cache.0),
        &RebuildOnlyTransport(updated_archive),
    )
    .expect("rebuild succeeds against the pinned commit");
    assert_eq!(result.catalogue.metadata.resolved_commit, COMMIT);
    assert_eq!(result.catalogue.games.len(), 2);
}

#[test]
fn rebuild_index_without_an_installed_catalogue_fails_clearly() {
    let cache = CacheFixture::new("rebuild-missing");
    let error = rebuild_dolphin_catalogue_index_with_transport(
        &options(&cache.0),
        &FakeTransport::new(COMMIT, Vec::new()),
    )
    .expect_err("rebuild without a catalogue must fail");
    assert_eq!(error.kind, DolphinCatalogueErrorKind::CacheInvalid);
}

struct SingleGameFakeTransport(Vec<u8>);
impl CheatSourceTransport for SingleGameFakeTransport {
    fn get(
        &self,
        _url: &str,
        _maximum_bytes: u64,
        destination: &mut dyn Write,
        _context: CheatSourceTransferContext<'_>,
    ) -> Result<CheatSourceHttpResponse, CheatSourceError> {
        destination.write_all(&self.0).expect("fixture write");
        Ok(CheatSourceHttpResponse {
            status: 200,
            content_type: None,
            content_encoding: None,
            content_length: Some(self.0.len() as u64),
            location: None,
            etag: None,
            last_modified: None,
            downloaded_bytes: self.0.len() as u64,
            retry_after_seconds: None,
        })
    }
}
