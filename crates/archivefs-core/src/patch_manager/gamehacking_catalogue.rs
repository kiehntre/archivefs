//! Shared GameHacking.org catalogue crawl orchestration.
//!
//! This module deliberately knows nothing about local game identity, match
//! tiers, cheat exports, or emulator installation. Platform providers retain
//! their pagination HTML interpretation, row parser, and public catalogue
//! types through the small [`GameHackingCatalogueHooks`] boundary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::Serialize;

use super::gamehacking_shared::{
    GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE, GameHackingClient, GameHackingError,
    GameHackingErrorKind, GameHackingRequestOptions, GameHackingRequestSpec, ProviderResponse,
    UreqGameHackingTransport, atomic_write, cache_retrieved_at,
    cached_bytes_are_cloudflare_challenge, check_cancelled, provider_error, sha256_hex,
    unix_seconds_now,
};

pub(crate) struct GameHackingCatalogueSpec<'a> {
    pub schema_version: u32,
    pub provider: &'a str,
    pub system: &'a str,
    pub index_url: &'a str,
    pub robots_path: &'a str,
    /// Ordered by preference. The first existing entry wins; otherwise the
    /// first entry is the publication target.
    pub root_cache_files: &'a [&'a str],
    pub page_cache_prefix: &'a str,
    pub page_cache_suffix: &'a str,
    pub catalogue_cache_file: &'a str,
    pub maximum_index_bytes: usize,
    pub maximum_pages: usize,
    /// Some live root pages omit a self-link for page zero. The platform
    /// parser decides whether this documented template behavior applies.
    pub insert_root_page_zero: bool,
    pub no_pages_error: &'a str,
    pub page_count_error: &'a str,
    pub incomplete_pagination_error: &'a str,
    pub page_limit_error: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GameHackingCataloguePageMetadata {
    pub page_number: u32,
    pub source_url: String,
    pub retrieved_at_unix_seconds: u64,
    pub sha256: String,
    pub game_count: usize,
}

pub(crate) struct GameHackingCatalogueMetadata<'a> {
    pub schema_version: u32,
    pub provider: &'a str,
    pub system: &'a str,
    pub source_url: &'a str,
    pub retrieved_at_unix_seconds: u64,
}

pub(crate) trait GameHackingCatalogueHooks {
    type Record: Clone;
    type Page;
    type Catalogue: Serialize;

    /// Extracts raw page identifiers from platform-specific pagination HTML.
    /// Ordering, deduplication, zero insertion, and contiguity are shared.
    fn discover_page_numbers(
        &self,
        bytes: &[u8],
        charset: Option<&str>,
    ) -> Result<Vec<u32>, GameHackingError>;

    fn parse_page(
        &self,
        source_url: &str,
        retrieved_at_unix_seconds: u64,
        bytes: &[u8],
        charset: Option<&str>,
    ) -> Result<Vec<Self::Record>, GameHackingError>;

    fn record_id(&self, record: &Self::Record) -> u64;
    fn record_title<'a>(&self, record: &'a Self::Record) -> &'a str;
    fn make_page(&self, metadata: GameHackingCataloguePageMetadata) -> Self::Page;
    fn make_catalogue(
        &self,
        metadata: GameHackingCatalogueMetadata<'_>,
        pages: Vec<Self::Page>,
        games: Vec<Self::Record>,
    ) -> Self::Catalogue;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GameHackingCatalogueProgress {
    pub pages_complete: usize,
    pub pages_total: usize,
    pub page_number: Option<u32>,
    pub downloaded: bool,
    pub games_collected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GameHackingCatalogueCrawlResult {
    pub catalogue_path: PathBuf,
    pub pages_total: usize,
    pub pages_downloaded: usize,
    pub pages_reused: usize,
    pub games: usize,
    pub retrieved_at_unix_seconds: u64,
    pub cached_fallback: bool,
}

struct CatalogueRequestOptions<'a> {
    cache_root: &'a Path,
    force_refresh: bool,
    cancellation: Option<&'a AtomicBool>,
}

impl GameHackingRequestOptions for CatalogueRequestOptions<'_> {
    fn cache_root(&self) -> &Path {
        self.cache_root
    }

    fn force_refresh(&self) -> bool {
        self.force_refresh
    }

    fn delay(&self) -> Duration {
        Duration::from_secs(2)
    }

    fn cancellation(&self) -> Option<&AtomicBool> {
        self.cancellation
    }
}

pub(crate) struct GameHackingCatalogueCrawler<'a> {
    client: &'a GameHackingClient,
}

impl<'a> GameHackingCatalogueCrawler<'a> {
    pub(crate) fn new(client: &'a GameHackingClient) -> Self {
        Self { client }
    }

    pub(crate) fn crawl<O, H, R, P>(
        &self,
        spec: &GameHackingCatalogueSpec<'_>,
        options: &O,
        hooks: &H,
        request: R,
        mut progress: P,
    ) -> Result<GameHackingCatalogueCrawlResult, GameHackingError>
    where
        O: GameHackingRequestOptions,
        H: GameHackingCatalogueHooks,
        R: Fn(&UreqGameHackingTransport, &str, usize) -> Result<ProviderResponse, GameHackingError>,
        P: FnMut(GameHackingCatalogueProgress),
    {
        let root_options = CatalogueRequestOptions {
            cache_root: options.cache_root(),
            force_refresh: options.force_refresh(),
            cancellation: options.cancellation(),
        };
        self.client
            .check_robots(&root_options, &[spec.robots_path])?;

        let root_cache_name = spec
            .root_cache_files
            .iter()
            .copied()
            .find(|name| options.cache_root().join(name).is_file())
            .or_else(|| spec.root_cache_files.first().copied())
            .ok_or_else(|| {
                provider_error(
                    GameHackingErrorKind::CacheUnavailable,
                    "GameHacking.org catalogue root cache specification is empty",
                )
            })?;
        let root_path = options.cache_root().join(root_cache_name);
        let root_was_cached = root_path.is_file();
        let root = self.client.cached_request(
            GameHackingRequestSpec {
                cache_file: root_cache_name,
                url: spec.index_url,
                maximum_bytes: spec.maximum_index_bytes,
            },
            &root_options,
            |transport| request(transport, spec.index_url, spec.maximum_index_bytes),
        )?;
        reject_challenge_before_parser(&root.bytes)?;
        let cached_fallback = root.cached_fallback;
        let resume_options = CatalogueRequestOptions {
            cache_root: options.cache_root(),
            force_refresh: false,
            cancellation: options.cancellation(),
        };
        let discovered = hooks.discover_page_numbers(&root.bytes, root.charset.as_deref())?;
        let page_numbers = ordered_contiguous_page_numbers(
            discovered,
            spec.insert_root_page_zero,
            spec.no_pages_error,
            spec.page_count_error,
            spec.incomplete_pagination_error,
        )?;
        if page_numbers.len() > spec.maximum_pages {
            return Err(provider_error(
                GameHackingErrorKind::InvalidResponse,
                spec.page_limit_error,
            ));
        }

        let mut pages = Vec::with_capacity(page_numbers.len());
        let mut games_by_id = BTreeMap::<u64, H::Record>::new();
        let mut downloaded = 0usize;
        let mut reused = 0usize;
        let mut retrieved_at = None::<u64>;
        for (position, page_number) in page_numbers.iter().copied().enumerate() {
            check_cancelled(&resume_options)?;
            let url = format!("{}/{}", spec.index_url, page_number);
            let cache_name = format!(
                "{}{page_number}{}",
                spec.page_cache_prefix, spec.page_cache_suffix
            );
            let cache_path = options.cache_root().join(&cache_name);
            let (response, was_cached, retrieval_path, page_source_url) = if page_number == 0 {
                (
                    root.clone(),
                    root_was_cached,
                    root_path.clone(),
                    spec.index_url.to_string(),
                )
            } else {
                let was_cached = cache_path.is_file();
                let response = self.client.cached_request(
                    GameHackingRequestSpec {
                        cache_file: &cache_name,
                        url: &url,
                        maximum_bytes: spec.maximum_index_bytes,
                    },
                    &resume_options,
                    |transport| request(transport, &url, spec.maximum_index_bytes),
                )?;
                (response, was_cached, cache_path, url.clone())
            };
            reject_challenge_before_parser(&response.bytes)?;
            if was_cached {
                reused += 1;
            } else {
                downloaded += 1;
            }
            let page_retrieved_at = cache_retrieved_at(&retrieval_path)?;
            retrieved_at = Some(
                retrieved_at
                    .unwrap_or(page_retrieved_at)
                    .max(page_retrieved_at),
            );
            let mut page_games = hooks.parse_page(
                &page_source_url,
                page_retrieved_at,
                &response.bytes,
                response.charset.as_deref(),
            )?;
            page_games.sort_by_key(|game| hooks.record_id(game));
            for game in &page_games {
                games_by_id
                    .entry(hooks.record_id(game))
                    .or_insert_with(|| game.clone());
            }
            pages.push((
                page_number,
                hooks.make_page(GameHackingCataloguePageMetadata {
                    page_number,
                    source_url: page_source_url,
                    retrieved_at_unix_seconds: page_retrieved_at,
                    sha256: sha256_hex(&response.bytes),
                    game_count: page_games.len(),
                }),
            ));
            progress(GameHackingCatalogueProgress {
                pages_complete: position + 1,
                pages_total: page_numbers.len(),
                page_number: Some(page_number),
                downloaded: !was_cached,
                games_collected: games_by_id.len(),
            });
        }

        let mut games = games_by_id.into_values().collect::<Vec<_>>();
        games.sort_by(|left, right| {
            hooks
                .record_id(left)
                .cmp(&hooks.record_id(right))
                .then_with(|| hooks.record_title(left).cmp(hooks.record_title(right)))
        });
        pages.sort_by_key(|(page_number, _)| *page_number);
        let pages = pages.into_iter().map(|(_, page)| page).collect::<Vec<_>>();
        let pages_total = pages.len();
        let games_total = games.len();
        let retrieved_at = retrieved_at.unwrap_or_else(unix_seconds_now);
        let catalogue = hooks.make_catalogue(
            GameHackingCatalogueMetadata {
                schema_version: spec.schema_version,
                provider: spec.provider,
                system: spec.system,
                source_url: spec.index_url,
                retrieved_at_unix_seconds: retrieved_at,
            },
            pages,
            games,
        );
        let mut bytes = serde_json::to_vec_pretty(&catalogue).map_err(|failure| {
            provider_error(
                GameHackingErrorKind::CacheUnavailable,
                format!("GameHacking.org catalogue could not be serialized: {failure}"),
            )
        })?;
        bytes.push(b'\n');
        let catalogue_path = options.cache_root().join(spec.catalogue_cache_file);
        atomic_write(&catalogue_path, &bytes)?;
        Ok(GameHackingCatalogueCrawlResult {
            catalogue_path,
            pages_total,
            pages_downloaded: downloaded,
            pages_reused: reused,
            games: games_total,
            retrieved_at_unix_seconds: retrieved_at,
            cached_fallback,
        })
    }
}

pub(crate) fn ordered_contiguous_page_numbers(
    discovered: Vec<u32>,
    insert_root_page_zero: bool,
    no_pages_error: &str,
    page_count_error: &str,
    incomplete_pagination_error: &str,
) -> Result<Vec<u32>, GameHackingError> {
    if discovered.is_empty() {
        return Err(provider_error(
            GameHackingErrorKind::InvalidResponse,
            no_pages_error,
        ));
    }
    let mut pages = discovered;
    if insert_root_page_zero {
        pages.push(0);
    }
    pages.sort_unstable();
    pages.dedup();
    let expected_len = u32::try_from(pages.len())
        .map_err(|_| provider_error(GameHackingErrorKind::InvalidResponse, page_count_error))?;
    if pages.first() != Some(&0) || pages.iter().copied().ne(0..expected_len) {
        return Err(provider_error(
            GameHackingErrorKind::InvalidResponse,
            incomplete_pagination_error,
        ));
    }
    Ok(pages)
}

fn reject_challenge_before_parser(bytes: &[u8]) -> Result<(), GameHackingError> {
    if cached_bytes_are_cloudflare_challenge(bytes) {
        return Err(provider_error(
            GameHackingErrorKind::CloudflareBlocked,
            GAMEHACKING_PROVIDER_CHALLENGE_MESSAGE,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde::{Deserialize, Serialize};

    use super::super::gamehacking_shared::{
        charset_cache_path, mark_cloudflare_blocked, retrieved_cache_path,
    };

    #[derive(Debug, Clone)]
    struct Options {
        cache_root: PathBuf,
        force_refresh: bool,
        cancellation: Option<Arc<AtomicBool>>,
    }

    impl GameHackingRequestOptions for Options {
        fn cache_root(&self) -> &Path {
            &self.cache_root
        }

        fn force_refresh(&self) -> bool {
            self.force_refresh
        }

        fn delay(&self) -> Duration {
            Duration::ZERO
        }

        fn cancellation(&self) -> Option<&AtomicBool> {
            self.cancellation.as_deref()
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct Record {
        id: u64,
        title: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct Page {
        page_number: u32,
        source_url: String,
        retrieved_at_unix_seconds: u64,
        sha256: String,
        game_count: usize,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct Catalogue {
        schema_version: u32,
        provider: String,
        system: String,
        source_url: String,
        retrieved_at_unix_seconds: u64,
        pages: Vec<Page>,
        games: Vec<Record>,
    }

    struct Hooks {
        parser_calls: Arc<AtomicUsize>,
    }

    impl GameHackingCatalogueHooks for Hooks {
        type Record = Record;
        type Page = Page;
        type Catalogue = Catalogue;

        fn discover_page_numbers(
            &self,
            bytes: &[u8],
            _charset: Option<&str>,
        ) -> Result<Vec<u32>, GameHackingError> {
            let text = String::from_utf8_lossy(bytes);
            let pages = text
                .lines()
                .find_map(|line| line.strip_prefix("pages="))
                .unwrap_or_default()
                .split(',')
                .filter_map(|value| value.parse::<u32>().ok())
                .collect();
            Ok(pages)
        }

        fn parse_page(
            &self,
            _source_url: &str,
            _retrieved_at_unix_seconds: u64,
            bytes: &[u8],
            _charset: Option<&str>,
        ) -> Result<Vec<Self::Record>, GameHackingError> {
            self.parser_calls.fetch_add(1, Ordering::Relaxed);
            let text = String::from_utf8_lossy(bytes);
            Ok(text
                .lines()
                .find_map(|line| line.strip_prefix("records="))
                .unwrap_or_default()
                .split(',')
                .filter_map(|record| {
                    let (id, title) = record.split_once(':')?;
                    Some(Record {
                        id: id.parse().ok()?,
                        title: title.to_string(),
                    })
                })
                .collect())
        }

        fn record_id(&self, record: &Self::Record) -> u64 {
            record.id
        }

        fn record_title<'a>(&self, record: &'a Self::Record) -> &'a str {
            &record.title
        }

        fn make_page(&self, metadata: GameHackingCataloguePageMetadata) -> Self::Page {
            Page {
                page_number: metadata.page_number,
                source_url: metadata.source_url,
                retrieved_at_unix_seconds: metadata.retrieved_at_unix_seconds,
                sha256: metadata.sha256,
                game_count: metadata.game_count,
            }
        }

        fn make_catalogue(
            &self,
            metadata: GameHackingCatalogueMetadata<'_>,
            pages: Vec<Self::Page>,
            games: Vec<Self::Record>,
        ) -> Self::Catalogue {
            Catalogue {
                schema_version: metadata.schema_version,
                provider: metadata.provider.to_string(),
                system: metadata.system.to_string(),
                source_url: metadata.source_url.to_string(),
                retrieved_at_unix_seconds: metadata.retrieved_at_unix_seconds,
                pages,
                games,
            }
        }
    }

    fn root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "archivefs-gamehacking-catalogue-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        write_cache(&root, "robots.txt", b"User-agent: *\nAllow: /\n", 99);
        root
    }

    fn write_cache(root: &Path, name: &str, bytes: &[u8], retrieved_at: u64) {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        fs::write(retrieved_cache_path(&path), retrieved_at.to_string()).unwrap();
        fs::write(charset_cache_path(&path), b"utf-8").unwrap();
    }

    fn spec<'a>(
        system: &'a str,
        root_files: &'a [&'a str],
        prefix: &'a str,
        catalogue: &'a str,
        insert_zero: bool,
    ) -> GameHackingCatalogueSpec<'a> {
        GameHackingCatalogueSpec {
            schema_version: 1,
            provider: "gamehacking.org",
            system,
            index_url: "https://gamehacking.org/system/test/all",
            robots_path: "/system/test/all",
            root_cache_files: root_files,
            page_cache_prefix: prefix,
            page_cache_suffix: ".html",
            catalogue_cache_file: catalogue,
            maximum_index_bytes: 4096,
            maximum_pages: 16,
            insert_root_page_zero: insert_zero,
            no_pages_error: "no pages",
            page_count_error: "bad count",
            incomplete_pagination_error: "incomplete pages",
            page_limit_error: "too many pages",
        }
    }

    fn cached_options(root: &Path) -> Options {
        Options {
            cache_root: root.to_path_buf(),
            force_refresh: false,
            cancellation: None,
        }
    }

    fn read_catalogue(path: &Path) -> Catalogue {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn one_shared_loop_handles_two_specs_without_platform_branching() {
        for (system, root_name, prefix, catalogue_name) in [
            ("First", "first-root.html", "first-page-", "first.json"),
            ("Second", "second-root.html", "second-page-", "second.json"),
        ] {
            let root = root(system);
            write_cache(&root, root_name, b"pages=0\nrecords=7:Game", 100);
            let root_files = [root_name];
            let hooks = Hooks {
                parser_calls: Arc::new(AtomicUsize::new(0)),
            };
            let result = GameHackingCatalogueCrawler::new(&GameHackingClient::default())
                .crawl(
                    &spec(system, &root_files, prefix, catalogue_name, false),
                    &cached_options(&root),
                    &hooks,
                    |_, _, _| panic!("cached crawl must not use transport"),
                    |_| {},
                )
                .unwrap();
            assert_eq!(result.pages_reused, 1);
            assert_eq!(read_catalogue(&result.catalogue_path).system, system);
            assert!(root.join(catalogue_name).is_file());
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn page_zero_duplicates_and_order_are_normalized_and_processed_once() {
        let root = root("ordering");
        write_cache(
            &root,
            "root.html",
            b"pages=2,1,0,1\nrecords=2:Zulu,1:Alpha",
            100,
        );
        write_cache(&root, "page-1.html", b"records=3:Beta,1:Conflict", 101);
        write_cache(&root, "page-2.html", b"records=4:Delta", 102);
        let root_files = ["root.html"];
        let parser_calls = Arc::new(AtomicUsize::new(0));
        let hooks = Hooks {
            parser_calls: parser_calls.clone(),
        };
        let mut events = Vec::new();
        let result = GameHackingCatalogueCrawler::new(&GameHackingClient::default())
            .crawl(
                &spec("Test", &root_files, "page-", "catalogue.json", true),
                &cached_options(&root),
                &hooks,
                |_, _, _| panic!("cached crawl must not use transport"),
                |event| events.push(event),
            )
            .unwrap();
        assert_eq!(parser_calls.load(Ordering::Relaxed), 3);
        assert_eq!(result.pages_reused, 3);
        assert_eq!(result.pages_downloaded, 0);
        assert_eq!(
            events
                .iter()
                .filter_map(|event| event.page_number)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.games_collected)
                .collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        let catalogue = read_catalogue(&result.catalogue_path);
        assert_eq!(
            catalogue
                .pages
                .iter()
                .map(|page| page.page_number)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            catalogue
                .games
                .iter()
                .map(|game| (game.id, game.title.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, "Alpha"), (2, "Zulu"), (3, "Beta"), (4, "Delta")]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn downloaded_and_cached_counts_and_progress_match_the_existing_contract() {
        let root = root("mixed");
        write_cache(&root, "page-1.html", b"records=2:Two", 101);
        write_cache(&root, "page-2.html", b"records=3:Three", 102);
        let root_files = ["root.html"];
        let calls = Cell::new(0usize);
        let mut events = Vec::new();
        let result = GameHackingCatalogueCrawler::new(&GameHackingClient::default())
            .crawl(
                &spec("Test", &root_files, "page-", "catalogue.json", false),
                &cached_options(&root),
                &Hooks {
                    parser_calls: Arc::new(AtomicUsize::new(0)),
                },
                |_, url, _| {
                    calls.set(calls.get() + 1);
                    assert_eq!(url, "https://gamehacking.org/system/test/all");
                    Ok(ProviderResponse {
                        bytes: b"pages=2,0,1,1\nrecords=1:One".to_vec(),
                        charset: Some("utf-8".to_string()),
                        cached_fallback: false,
                        retrieved_at_unix_seconds: None,
                    })
                },
                |event| events.push(event),
            )
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(result.pages_downloaded, 1);
        assert_eq!(result.pages_reused, 2);
        assert_eq!(
            events
                .iter()
                .map(|event| (event.pages_complete, event.pages_total, event.downloaded))
                .collect::<Vec<_>>(),
            vec![(1, 3, true), (2, 3, false), (3, 3, false)]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_and_failed_pages_never_replace_an_existing_catalogue() {
        for failure in ["cancel", "page-error"] {
            let root = root(failure);
            write_cache(&root, "root.html", b"pages=0,1\nrecords=1:One", 100);
            fs::write(root.join("catalogue.json"), b"previous catalogue\n").unwrap();
            let root_files = ["root.html"];
            let cancelled = Arc::new(AtomicBool::new(false));
            let options = Options {
                cache_root: root.clone(),
                force_refresh: false,
                cancellation: Some(cancelled.clone()),
            };
            let result = GameHackingCatalogueCrawler::new(&GameHackingClient::default()).crawl(
                &spec("Test", &root_files, "page-", "catalogue.json", false),
                &options,
                &Hooks {
                    parser_calls: Arc::new(AtomicUsize::new(0)),
                },
                |_, _, _| {
                    Err(provider_error(
                        GameHackingErrorKind::InvalidResponse,
                        "page failed",
                    ))
                },
                |_| {
                    if failure == "cancel" {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                },
            );
            assert_eq!(
                result.unwrap_err().kind,
                if failure == "cancel" {
                    GameHackingErrorKind::Cancelled
                } else {
                    GameHackingErrorKind::InvalidResponse
                }
            );
            assert_eq!(
                fs::read(root.join("catalogue.json")).unwrap(),
                b"previous catalogue\n"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn challenge_cache_never_reaches_platform_parsers() {
        let root = root("challenge");
        write_cache(
            &root,
            "root.html",
            b"<html><title>Just a moment...</title>Cloudflare Ray ID: test</html>",
            100,
        );
        let root_files = ["root.html"];
        let calls = Arc::new(AtomicUsize::new(0));
        let failure = GameHackingCatalogueCrawler::new(&GameHackingClient::default())
            .crawl(
                &spec("Test", &root_files, "page-", "catalogue.json", false),
                &cached_options(&root),
                &Hooks {
                    parser_calls: calls.clone(),
                },
                |_, _, _| panic!("challenge cache must not use transport"),
                |_| {},
            )
            .unwrap_err();
        assert_eq!(failure.kind, GameHackingErrorKind::CloudflareBlocked);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(!root.join("catalogue.json").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cooldown_cached_fallback_still_feeds_exact_pages_to_parsers() {
        let root = root("fallback");
        write_cache(&root, "root.html", b"pages=0,1\nrecords=1:One", 100);
        write_cache(&root, "page-1.html", b"records=2:Two", 101);
        mark_cloudflare_blocked(&root).unwrap();
        let root_files = ["root.html"];
        let calls = Arc::new(AtomicUsize::new(0));
        let result = GameHackingCatalogueCrawler::new(&GameHackingClient::default())
            .crawl(
                &spec("Test", &root_files, "page-", "catalogue.json", false),
                &Options {
                    cache_root: root.clone(),
                    force_refresh: true,
                    cancellation: None,
                },
                &Hooks {
                    parser_calls: calls.clone(),
                },
                |_, _, _| panic!("cooldown must not use transport"),
                |_| {},
            )
            .unwrap();
        assert!(result.cached_fallback);
        assert_eq!(result.pages_reused, 2);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(read_catalogue(&result.catalogue_path).games.len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pagination_normalizer_sorts_deduplicates_and_requires_contiguity() {
        assert_eq!(
            ordered_contiguous_page_numbers(vec![2, 0, 1, 2], false, "none", "count", "gap")
                .unwrap(),
            vec![0, 1, 2]
        );
        assert_eq!(
            ordered_contiguous_page_numbers(vec![2, 1], true, "none", "count", "gap").unwrap(),
            vec![0, 1, 2]
        );
        assert_eq!(
            ordered_contiguous_page_numbers(vec![0, 2], false, "none", "count", "gap")
                .unwrap_err()
                .detail,
            "gap"
        );
    }
}
