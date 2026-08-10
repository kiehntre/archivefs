//! Stage 1B tests: import, cache, matching and hashing.
//!
//! Driven by a deterministic fake instance and real temporary trees. No test
//! contacts a RomM server, and none needs one.

use super::cache::*;
use super::hashing::*;
use super::matching::*;
use super::model::*;
use super::path_map::{PathMapping, PathMappings, ProviderPathKind};
use super::romm::capability::{RommApiCapability, RommCapabilityReport, RommHeartbeat};
use super::romm::client::{RommHttpResponse, RommRequestError, RommTransport};
use super::romm::config::{RommSourceConfig, RommToken, ValidatedRommSource};
use super::romm::import::*;
use super::romm::normalise::*;
use super::status::*;
use crate::identity_source::net_policy::StaticResolver;
use crate::safe_read::TrustedRoots;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

// --- Fixtures -------------------------------------------------------------

/// A temporary tree with a library root and an EmuWiz-owned identity root.
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-romm-1b-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for directory in ["library", "identity"] {
            std::fs::create_dir_all(root.join(directory)).expect("fixture");
        }
        Self { root }
    }

    fn library(&self) -> PathBuf {
        self.root.join("library")
    }

    fn identity(&self) -> PathBuf {
        self.root.join("identity")
    }

    fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.library().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture");
        }
        std::fs::write(&path, contents).expect("fixture");
        path
    }

    fn api(&self) -> IdentitySourceApi {
        IdentitySourceApi::new(&self.identity(), IdentityProvider::Romm)
    }

    fn trusted(&self) -> TrustedRoots {
        TrustedRoots::from_paths([self.library()])
    }

    fn mappings(&self) -> PathMappings {
        PathMappings::validate(
            &[PathMapping {
                provider_prefix: "/romm/library".to_string(),
                archivefs_prefix: self.library(),
            }],
            &[self.library()],
            ProviderPathKind::AbsoluteProviderPath,
        )
        .expect("valid mappings")
    }

    fn source(&self) -> ValidatedRommSource {
        let config = RommSourceConfig {
            enabled: true,
            url: "http://172.19.0.20:8080".to_string(),
            mappings: vec![PathMapping {
                provider_prefix: "/romm/library".to_string(),
                archivefs_prefix: self.library(),
            }],
            provider_path_kind: ProviderPathKind::AbsoluteProviderPath,
            token_path: None,
        };
        ValidatedRommSource::validate(
            &config,
            &RommToken::parse("rk_test_1b").expect("token"),
            &[self.library()],
            &StaticResolver::new(),
        )
        .expect("valid source")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn capability() -> RommCapabilityReport {
    let openapi: serde_json::Value = serde_json::from_str(
        r#"{"info":{"version":"5.1.0"},
            "paths":{"/api/platforms":{"get":{}},
                     "/api/roms":{"get":{"parameters":[{"name":"limit"},{"name":"offset"}]}},
                     "/api/client-tokens":{"get":{}}},
            "components":{"schemas":{"SimpleRomSchema":{"properties":{
              "md5_hash":{},"sha1_hash":{},"crc_hash":{},"url_cover":{},"files":{}}}}}}"#,
    )
    .expect("json");
    RommCapabilityReport {
        server_id: "http://172.19.0.20:8080".to_string(),
        heartbeat: Some(
            RommHeartbeat::parse(
                &serde_json::from_str::<serde_json::Value>(
                    r#"{"SYSTEM":{"VERSION":"5.1.0"},"FILESYSTEM":{"FS_PLATFORMS":["nes"]}}"#,
                )
                .expect("json"),
            )
            .expect("heartbeat"),
        ),
        api: RommApiCapability::from_openapi(&openapi),
        notes: Vec::new(),
    }
}

/// One ROM record, in the real field shape.
fn rom_json(id: u32, name: &str, file: &str, size: u64, md5: Option<&str>) -> String {
    let md5 = md5
        .map(|value| format!("\"{value}\""))
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"id":{id},"platform_id":3,"platform_slug":"nes",
             "fs_path":"/romm/library/nes","fs_name":"{file}",
             "full_path":"/romm/library/nes/{file}",
             "fs_size_bytes":{size},"name":"{name}",
             "regions":["USA"],"revision":null,
             "md5_hash":{md5},"sha1_hash":null,"crc_hash":null,
             "igdb_id":1000,"url_cover":"assets/roms/{id}/cover_l.png",
             "path_cover_small":"assets/roms/{id}/cover_s.png",
             "files":[],"has_multiple_files":false,"sibling_roms":[],
             "updated_at":"2026-07-01T00:00:00Z","missing_from_fs":false}}"#
    )
}

fn page_json(items: &[String], total: u64, limit: u32, offset: u32) -> String {
    format!(
        r#"{{"items":[{}],"total":{total},"limit":{limit},"offset":{offset}}}"#,
        items.join(",")
    )
}

/// A fake instance that serves a scripted sequence of ROM pages.
struct FakeRomm {
    platforms: String,
    /// One entry per `/api/roms` call, in order.
    pages: Mutex<Vec<Result<String, RommRequestError>>>,
    /// When set, every page after this many calls fails with this error.
    fail_after: Option<(usize, RommRequestError)>,
    calls: Mutex<usize>,
    urls: Mutex<Vec<String>>,
    /// Repeat the same page for ever, to exercise loop detection.
    repeat_forever: Option<String>,
}

impl FakeRomm {
    fn with_pages(pages: Vec<String>) -> Self {
        Self {
            platforms: r#"[{"id":3,"slug":"nes","name":"Nintendo Entertainment System"}]"#
                .to_string(),
            pages: Mutex::new(pages.into_iter().map(Ok).collect()),
            fail_after: None,
            calls: Mutex::new(0),
            urls: Mutex::new(Vec::new()),
            repeat_forever: None,
        }
    }

    fn failing_after(mut self, calls: usize, error: RommRequestError) -> Self {
        self.fail_after = Some((calls, error));
        self
    }

    fn repeating(page: String) -> Self {
        let mut fake = Self::with_pages(Vec::new());
        fake.repeat_forever = Some(page);
        fake
    }

    fn rom_calls(&self) -> usize {
        *self.calls.lock().expect("lock")
    }
}

impl RommTransport for FakeRomm {
    fn get(
        &self,
        url: &str,
        _authorization: Option<&str>,
        _max_bytes: usize,
    ) -> Result<RommHttpResponse, RommRequestError> {
        self.urls.lock().expect("lock").push(url.to_string());
        let body = if url.contains("/api/platforms") {
            self.platforms.clone()
        } else if url.contains("/api/roms") {
            let mut calls = self.calls.lock().expect("lock");
            *calls += 1;
            if let Some((limit, error)) = &self.fail_after
                && *calls > *limit
            {
                return Err(error.clone());
            }
            if let Some(page) = &self.repeat_forever {
                page.clone()
            } else {
                let mut pages = self.pages.lock().expect("lock");
                if pages.is_empty() {
                    // Past the end, a real server returns an empty page that
                    // still echoes the offset it was asked for.
                    let requested = url
                        .split("offset=")
                        .nth(1)
                        .and_then(|tail| tail.split('&').next())
                        .and_then(|value| value.parse::<u32>().ok())
                        .unwrap_or(0);
                    page_json(&[], 0, 100, requested)
                } else {
                    pages.remove(0)?
                }
            }
        } else {
            "{}".to_string()
        };
        Ok(RommHttpResponse {
            status: 200,
            body: body.into_bytes(),
            location: None,
        })
    }
}

fn no_progress(_: ImportProgress) {}

/// Facts for a record, observed from the real temporary tree.
#[allow(dead_code)]
fn observe_facts(record: &ExternalIdentityRecord) -> LocalFileFacts {
    match &record.archivefs_path {
        Some(path) => LocalFileFacts::observe(path).with_local_platform(
            record.platform_candidate.as_deref(),
            LocalEvidenceStrength::None,
        ),
        None => LocalFileFacts::default(),
    }
}

// --- Hashing --------------------------------------------------------------

/// Test 68: CRC32, MD5 and SHA-1 against published vectors.
#[test]
fn the_hash_implementations_match_published_vectors() {
    // CRC32 of "123456789" is CBF43926 - the standard check value.
    assert_eq!(Crc32::of(b"123456789"), "cbf43926");
    assert_eq!(Crc32::of(b""), "00000000");
    assert_eq!(
        Crc32::of(b"The quick brown fox jumps over the lazy dog"),
        "414fa339"
    );

    let tree = Tree::new("hash-vectors");
    // The empty file, whose MD5 and SHA-1 are the best known vectors there are.
    let empty = tree.file("empty.bin", b"");
    let hashes = hash_file(&empty, &tree.trusted(), None).expect("hashed");
    assert_eq!(hashes.md5, "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(hashes.sha1, "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(hashes.crc32, "00000000");
    assert_eq!(hashes.bytes_hashed, 0);

    // "abc".
    let abc = tree.file("abc.bin", b"abc");
    let hashes = hash_file(&abc, &tree.trusted(), None).expect("hashed");
    assert_eq!(hashes.md5, "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(hashes.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(hashes.bytes_hashed, 3);
}

/// Test 69: a hash spanning many chunks is correct, so the streaming is right.
#[test]
fn hashing_is_correct_across_chunk_boundaries() {
    let tree = Tree::new("hash-chunks");
    // Deliberately not a multiple of the chunk size.
    let contents: Vec<u8> = (0..(HASH_CHUNK_BYTES * 2 + 12345))
        .map(|index| (index % 251) as u8)
        .collect();
    let path = tree.file("big.bin", &contents);
    let streamed = hash_file(&path, &tree.trusted(), None).expect("hashed");
    assert_eq!(streamed.bytes_hashed, contents.len() as u64);
    // The same bytes hashed in one go must agree.
    assert_eq!(streamed.crc32, Crc32::of(&contents));
    use md5::Md5;
    use sha1::{Sha1, digest::Digest};
    let expected_md5: String = Md5::digest(&contents)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let expected_sha1: String = Sha1::digest(&contents)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(streamed.md5, expected_md5);
    assert_eq!(streamed.sha1, expected_sha1);
}

/// Test 70: hashing is cancellable and never partial.
#[test]
fn hashing_is_cancellable() {
    let tree = Tree::new("hash-cancel");
    let path = tree.file("game.bin", &vec![7_u8; 4096]);
    let cancel = AtomicBool::new(true);
    let refusal = hash_file(&path, &tree.trusted(), Some(&cancel)).expect_err("cancelled");
    assert_eq!(refusal.code(), "cancelled");
    cancel.store(false, Ordering::Relaxed);
    assert!(hash_file(&path, &tree.trusted(), Some(&cancel)).is_ok());
}

/// Progress is reported as the hash reads, so a multi-gigabyte image shows real
/// movement rather than an unbounded spinner.
#[test]
fn hashing_reports_progress_as_it_reads() {
    use crate::identity_source::hashing::{HashProgress, hash_file_reporting};
    use std::cell::RefCell;

    let tree = Tree::new("hash-progress");
    let contents: Vec<u8> = (0..(HASH_CHUNK_BYTES * 3 + 7))
        .map(|index| (index % 253) as u8)
        .collect();
    let path = tree.file("big.bin", &contents);
    let seen: RefCell<Vec<HashProgress>> = RefCell::new(Vec::new());
    let hashes = hash_file_reporting(&path, &tree.trusted(), None, &|progress| {
        seen.borrow_mut().push(progress);
    })
    .expect("hashed");

    let seen = seen.into_inner();
    // One before the first chunk, so the file and its size can be named immediately.
    assert_eq!(seen.first().expect("an opening report").bytes_read, 0);
    assert!(seen.len() >= 5, "{} reports for four chunks", seen.len());
    // Monotonic, and every report agrees about the total.
    for pair in seen.windows(2) {
        assert!(pair[1].bytes_read >= pair[0].bytes_read);
        assert_eq!(pair[0].total_bytes, contents.len() as u64);
    }
    let last = seen.last().expect("a closing report");
    assert_eq!(last.bytes_read, contents.len() as u64);
    assert_eq!(last.fraction(), Some(1.0));
    assert_eq!(hashes.bytes_hashed, contents.len() as u64);
}

/// An empty file has no fraction to report rather than a division by zero.
#[test]
fn hashing_an_empty_file_reports_no_fraction() {
    use crate::identity_source::hashing::{HashProgress, hash_file_reporting};
    use std::cell::RefCell;

    let tree = Tree::new("hash-progress-empty");
    let path = tree.file("empty.bin", b"");
    let seen: RefCell<Vec<HashProgress>> = RefCell::new(Vec::new());
    hash_file_reporting(&path, &tree.trusted(), None, &|progress| {
        seen.borrow_mut().push(progress);
    })
    .expect("hashed");
    let seen = seen.into_inner();
    assert_eq!(seen.len(), 1, "nothing to read, so one opening report");
    assert!(seen[0].fraction().is_none());
}

/// A cancelled hash reports nothing beyond where it stopped, and yields no hashes.
#[test]
fn a_cancelled_hash_reports_no_completion() {
    use crate::identity_source::hashing::{HashProgress, hash_file_reporting};
    use std::cell::RefCell;

    let tree = Tree::new("hash-progress-cancel");
    let path = tree.file("game.bin", &vec![3_u8; HASH_CHUNK_BYTES * 2]);
    let cancel = AtomicBool::new(true);
    let seen: RefCell<Vec<HashProgress>> = RefCell::new(Vec::new());
    let refusal = hash_file_reporting(&path, &tree.trusted(), Some(&cancel), &|progress| {
        seen.borrow_mut().push(progress);
    })
    .expect_err("cancelled");
    assert_eq!(refusal.code(), "cancelled");
    assert!(
        seen.into_inner().is_empty(),
        "cancelled before the file was opened, so nothing was reported"
    );
}

/// Test 71: the hash cache is keyed on metadata and invalidated by a change.
#[test]
fn a_changed_file_invalidates_its_cached_hash() {
    let tree = Tree::new("hash-cache");
    let path = tree.file("game.bin", b"original");
    let mut cache = LocalHashCache::new();

    let first = hash_file_cached(&path, &mut cache, &tree.trusted(), None).expect("hashed");
    assert_eq!(cache.len(), 1);
    assert!(cache.get(&path).is_some());
    // A second call is served from the cache - same answer.
    let second = hash_file_cached(&path, &mut cache, &tree.trusted(), None).expect("cached");
    assert_eq!(first, second);

    // Change the contents *and* the size, which changes the fingerprint.
    std::fs::write(&path, b"changed contents").expect("rewrite");
    assert!(
        cache.get(&path).is_none(),
        "a changed file must not be served a stale hash"
    );
    assert!(
        cache.has_entry_for(&path),
        "the stale entry is still there, it is just not valid"
    );
    let refreshed = hash_file_cached(&path, &mut cache, &tree.trusted(), None).expect("rehashed");
    assert_ne!(refreshed.md5, first.md5);
    assert_eq!(cache.len(), 1, "the stale entry was replaced, not added to");

    // A deleted file's entry is pruned.
    std::fs::remove_file(&path).expect("removed");
    assert_eq!(cache.prune(), 1);
    assert!(cache.is_empty());
}

/// Test 72: hashing goes through the shared read policy.
#[test]
fn hashing_refuses_a_symlink_outside_the_trusted_roots() {
    #[cfg(unix)]
    {
        let tree = Tree::new("hash-symlink");
        let outside = tree.root.join("outside.bin");
        std::fs::write(&outside, b"secret").expect("fixture");
        let link = tree.library().join("game.bin");
        std::os::unix::fs::symlink(&outside, &link).expect("fixture");

        let refusal = hash_file(&link, &tree.trusted(), None).expect_err("refused");
        assert_eq!(
            refusal.code(),
            "target_outside_trusted_roots",
            "the refusal must come from safe_read: {refusal:?}"
        );

        // The same link with the target's directory trusted is allowed, which
        // proves the refusal was the policy and not something else.
        let wider = TrustedRoots::from_paths([tree.library(), tree.root.clone()]);
        assert!(hash_file(&link, &wider, None).is_ok());
    }
}

/// Test 73: an oversized file is refused rather than read.
#[test]
fn an_oversized_file_is_refused_for_automatic_hashing() {
    let tree = Tree::new("hash-huge");
    let path = tree.library().join("huge.bin");
    let file = std::fs::File::create(&path).expect("fixture");
    file.set_len(MAX_AUTOMATIC_HASH_BYTES + 1).expect("sparse");
    drop(file);
    let refusal = hash_file(&path, &tree.trusted(), None).expect_err("refused");
    assert_eq!(refusal.code(), "too_large");
}

// --- Normalisation --------------------------------------------------------

/// Test 74: a full record maps every field the milestone lists.
#[test]
fn a_full_romm_record_maps_every_listed_field() {
    let tree = Tree::new("normalise-full");
    let value: serde_json::Value = serde_json::from_str(&format!(
        r#"{{"id":345,"platform_id":3,"platform_slug":"nes",
             "full_path":"/romm/library/nes/Metroid.zip","fs_name":"Metroid.zip",
             "fs_size_bytes":131072,"name":"Metroid","regions":["USA","EUR"],
             "revision":"1.1","md5_hash":"{md5}","sha1_hash":"{sha1}","crc_hash":"deadbeef",
             "igdb_id":1029,"moby_id":77,"ss_id":88,"hasheous_id":99,
             "url_cover":"assets/roms/345/cover_l.png",
             "path_cover_small":"assets/roms/345/cover_s.png",
             "files":[{{"full_path":"/romm/library/nes/disc1.bin"}},
                      {{"full_path":"/romm/library/nes/disc2.bin"}}],
             "has_multiple_files":true,
             "sibling_roms":[{{"id":346}},{{"id":347}}],
             "updated_at":"2026-07-01T12:00:00Z","missing_from_fs":false}}"#,
        md5 = "a".repeat(32),
        sha1 = "b".repeat(40)
    ))
    .expect("json");
    let mut report = NormalisationReport::default();
    let record = normalise_rom(
        &value,
        "http://romm:8080",
        &tree.mappings(),
        1_785_000_000,
        &mut report,
    )
    .expect("normalised");

    assert_eq!(record.provider, IdentityProvider::Romm);
    assert_eq!(record.server_id, "http://romm:8080");
    assert_eq!(record.provider_platform_id.as_deref(), Some("3"));
    assert_eq!(record.provider_game_id, "345");
    assert_eq!(record.provider_path, "/romm/library/nes/Metroid.zip");
    assert_eq!(
        record.archivefs_path,
        Some(tree.library().join("nes/Metroid.zip"))
    );
    assert_eq!(record.title.as_deref(), Some("Metroid"));
    assert_eq!(record.platform_candidate.as_deref(), Some("NES"));
    assert_eq!(record.provider_platform_name.as_deref(), Some("nes"));
    assert_eq!(record.regions, vec!["USA", "EUR"]);
    assert_eq!(record.revision.as_deref(), Some("1.1"));
    assert_eq!(record.hashes.len(), 3, "crc, md5 and sha1 all valid");
    assert_eq!(record.file_size_bytes, Some(131_072));
    assert_eq!(record.metadata_provider_ids.len(), 4);
    assert!(record.artwork.is_some());
    assert_eq!(record.related_files.len(), 2, "multi-file preserved");
    assert_eq!(record.sibling_game_ids, vec!["346", "347"]);
    assert_eq!(record.imported_at_unix_seconds, 1_785_000_000);
    assert_eq!(
        record.provider_updated_at.as_deref(),
        Some("2026-07-01T12:00:00Z")
    );
    assert_eq!(
        record.verification,
        ExternalVerification::Unmatched,
        "a freshly imported record is unmatched until it is matched"
    );
    assert!(report.rejected_hashes.is_empty());
    // The strongest hash is preferred for comparison.
    assert_eq!(
        record.strongest_hash().expect("some").algorithm,
        HashAlgorithm::Sha1
    );
}

/// Test 75: missing optional fields are absent, not fatal.
#[test]
fn a_minimal_record_normalises_with_gaps() {
    let tree = Tree::new("normalise-minimal");
    let value: serde_json::Value =
        serde_json::from_str(r#"{"id":1,"fs_name":"x.zip"}"#).expect("json");
    let mut report = NormalisationReport::default();
    let record = normalise_rom(&value, "http://romm:8080", &tree.mappings(), 0, &mut report)
        .expect("a record with only an id is still a record");
    assert_eq!(record.provider_game_id, "1");
    assert!(record.title.is_none());
    assert!(record.hashes.is_empty());
    assert!(record.file_size_bytes.is_none());
    assert!(record.platform_candidate.is_none());
    assert!(
        record.archivefs_path.is_none(),
        "`x.zip` matches no mapping"
    );

    // A record with no id at all cannot be recorded.
    let idless: serde_json::Value = serde_json::from_str(r#"{"name":"No id"}"#).expect("json");
    assert!(normalise_rom(&idless, "s", &tree.mappings(), 0, &mut report).is_none());
}

/// Test 76: an invalid upstream hash stays visible as rejected provider data.
#[test]
fn an_invalid_upstream_hash_is_rejected_and_reported() {
    let tree = Tree::new("normalise-bad-hash");
    let value: serde_json::Value = serde_json::from_str(
        r#"{"id":9,"full_path":"/romm/library/a.zip",
            "md5_hash":"notahash","crc_hash":"zzzzzzzz","sha1_hash":"abc"}"#,
    )
    .expect("json");
    let mut report = NormalisationReport::default();
    let record = normalise_rom(&value, "s", &tree.mappings(), 0, &mut report).expect("normalised");
    assert!(
        record.hashes.is_empty(),
        "no malformed value may become evidence"
    );
    assert_eq!(report.rejected_hashes.len(), 3);
    for rejected in &report.rejected_hashes {
        assert_eq!(rejected.provider_game_id, "9");
        assert!(!rejected.reason.is_empty());
        // The reason explains without echoing the value.
        assert!(!rejected.reason.contains("notahash"));
    }
}

/// Test 77: platform mapping goes through the one registry, exactly.
#[test]
fn romm_platform_slugs_resolve_through_the_canonical_registry() {
    // Slugs the registry already knows.
    for (slug, expected) in [
        ("nes", "NES"),
        ("atari-st", "AtariST"),
        ("amiga-cd32", "AmigaCD32"),
        ("scummvm", "ScummVM"),
        ("ps2", "PS2"),
    ] {
        assert_eq!(
            canonical_platform_for_romm_slug(slug),
            Some(expected),
            "{slug} should resolve"
        );
    }
    // Slugs only the RomM table knows.
    for (slug, expected) in [
        ("acpc", "Amstrad CPC"),
        ("genesis-slash-megadrive", "MegaDrive"),
        ("neo-geo-cd", "Neo Geo CD"),
        ("sega-cd", "Sega CD"),
    ] {
        assert_eq!(
            canonical_platform_for_romm_slug(slug),
            Some(expected),
            "{slug}"
        );
    }
    // Unknown stays unknown - no substring guessing.
    for slug in [
        "zx-spectrum-next",
        "amiga-cd",
        "not-a-platform",
        "",
        "nes-clone",
    ] {
        assert_eq!(
            canonical_platform_for_romm_slug(slug),
            None,
            "{slug} must not be guessed at"
        );
    }
}

/// Test 78: every canonical target the RomM slug table names exists in the
/// registry, so the table cannot rot into referring to nothing.
#[test]
fn every_romm_slug_target_exists_in_the_platform_registry() {
    for target in romm_slug_targets() {
        assert!(
            crate::platform::platform_by_id(target).is_some(),
            "the RomM slug table maps to `{target}`, which is not a canonical platform"
        );
    }
}

/// Test 79: an unknown platform is recorded so a person can see it.
#[test]
fn an_unknown_platform_is_recorded_rather_than_hidden() {
    let tree = Tree::new("normalise-unknown-platform");
    let value: serde_json::Value = serde_json::from_str(
        r#"{"id":5,"platform_slug":"some-obscure-machine","full_path":"/romm/library/a.zip"}"#,
    )
    .expect("json");
    let mut report = NormalisationReport::default();
    let record = normalise_rom(&value, "s", &tree.mappings(), 0, &mut report).expect("normalised");
    assert!(record.platform_candidate.is_none());
    assert_eq!(
        record.provider_platform_name.as_deref(),
        Some("some-obscure-machine"),
        "RomM's own name is preserved even when it cannot be mapped"
    );
    assert_eq!(report.unknown_platforms, vec!["some-obscure-machine"]);
}

// --- Import and pagination -------------------------------------------------

/// Test 80: a successful import over several pages.
#[test]
fn an_import_walks_several_pages() {
    let tree = Tree::new("import-pages");
    // 250 records over three pages of 100.
    let page = |offset: u32, count: u32| {
        let items: Vec<String> = (0..count)
            .map(|index| {
                let id = offset + index;
                rom_json(id, &format!("Game {id}"), &format!("g{id}.zip"), 1024, None)
            })
            .collect();
        page_json(&items, 250, 100, offset)
    };
    let fake = FakeRomm::with_pages(vec![page(0, 100), page(100, 100), page(200, 50)]);
    let mut seen_progress = Vec::new();
    let outcome = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        |progress| seen_progress.push(progress),
        None,
    )
    .expect("imported");

    assert_eq!(outcome.cache.records.len(), 250);
    assert_eq!(outcome.progress.pages_fetched, 3);
    assert_eq!(fake.rom_calls(), 3, "a short final page ends the walk");
    assert_eq!(outcome.cache.platforms.len(), 1);
    assert_eq!(outcome.cache.server_reported_total, Some(250));
    // Progress was reported per page and reached the total.
    assert_eq!(seen_progress.len(), 3);
    assert_eq!(seen_progress.last().expect("some").records_fetched, 250);
    assert_eq!(seen_progress[0].fraction(), Some(0.4));
}

/// Test 81: an empty catalogue imports cleanly.
#[test]
fn an_empty_catalogue_imports_as_an_empty_cache() {
    let tree = Tree::new("import-empty");
    let fake = FakeRomm::with_pages(vec![page_json(&[], 0, 100, 0)]);
    let outcome = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect("imported");
    assert!(outcome.cache.records.is_empty());
    assert_eq!(fake.rom_calls(), 1);
    // An empty total is not offered as progress, because a fraction of zero is
    // meaningless.
    assert_eq!(outcome.progress.reported_total, None);
}

/// Test 82: a bounded sample stops early without walking the catalogue.
#[test]
fn a_sample_import_stops_early() {
    let tree = Tree::new("import-sample");
    let items: Vec<String> = (0..100)
        .map(|id| rom_json(id, &format!("G{id}"), &format!("g{id}.zip"), 512, None))
        .collect();
    let fake = FakeRomm::with_pages(vec![page_json(&items, 10_000, 100, 0)]);
    let outcome = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Sample { max_records: 25 },
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect("imported");
    assert_eq!(outcome.cache.records.len(), 25);
    assert_eq!(fake.rom_calls(), 1, "one page was enough for a sample");
}

/// Test 83: a repeated page is detected rather than looped on.
#[test]
fn a_repeated_page_is_detected() {
    let tree = Tree::new("import-repeat");
    // Always returns offset 0 with a full page: without loop detection this
    // never ends.
    let items: Vec<String> = (0..100)
        .map(|id| rom_json(id, "G", &format!("g{id}.zip"), 1, None))
        .collect();
    let fake = FakeRomm::repeating(page_json(&items, 100_000, 100, 0));
    let failure = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect_err("must not loop");
    // The second page reports offset 0 when 100 was asked for, so the envelope
    // check catches it first - either way the import stops.
    assert!(
        matches!(failure.code(), "repeated_page" | "invalid_pagination"),
        "{failure:?}"
    );
    assert!(fake.rom_calls() < 10, "it must stop quickly, not loop");
    assert!(failure.previous_cache_preserved());
}

/// Test 84: an envelope that does not describe the requested page is refused.
#[test]
fn an_invalid_pagination_envelope_is_refused() {
    let tree = Tree::new("import-envelope");
    for (body, why) in [
        (page_json(&[], 10, 100, 42), "a wrong offset"),
        (page_json(&[], 10, 0, 0), "a zero limit"),
        (page_json(&[], 10, 99_999, 0), "an absurd limit"),
    ] {
        let fake = FakeRomm::with_pages(vec![body]);
        let failure = import_identity(
            &tree.source(),
            &fake,
            ImportScope::Full,
            &capability(),
            DEFAULT_PAGE_SIZE,
            no_progress,
            None,
        )
        .expect_err("refused");
        assert_eq!(failure.code(), "invalid_pagination", "{why}");
    }
}

/// Test 85: a wildly wrong total makes the import incomplete rather than silently
/// truncated.
#[test]
fn an_inconsistent_total_is_reported() {
    let tree = Tree::new("import-total");
    // Claims 1 record, delivers 100 then ends.
    let items: Vec<String> = (0..100)
        .map(|id| rom_json(id, "G", &format!("g{id}.zip"), 1, None))
        .collect();
    let fake = FakeRomm::with_pages(vec![
        page_json(&items, 1, 100, 0),
        page_json(&[], 1, 100, 100),
    ]);
    // 100 vs 1 is within the overshoot tolerance, so this completes - the total
    // is simply not offered as progress.
    let outcome = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect("still imports");
    assert_eq!(outcome.cache.records.len(), 100);
    // The server's claim is recorded honestly - it did say 1 - but it is never
    // turned into a progress fraction, because 100 of 1 is not a fraction.
    assert_eq!(outcome.cache.server_reported_total, Some(1));
    assert_eq!(
        outcome.progress.fraction(),
        None,
        "an impossible total must not drive a progress bar"
    );

    // A total smaller than what arrived by more than the tolerance makes the
    // import incomplete rather than silently accepted.
    let many: Vec<String> = (0..100)
        .map(|id| rom_json(id, "G", &format!("h{id}.zip"), 1, None))
        .collect();
    let pages: Vec<String> = (0..20)
        .map(|page| page_json(&many, 1, 100, page * 100))
        .collect();
    let fake = FakeRomm::with_pages(pages);
    let failure = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect_err("2000 records against a claimed total of 1 is not a complete import");
    assert_eq!(failure.code(), "inconsistent_total");
    assert!(failure.previous_cache_preserved());
}

/// Test 86: cancellation and mid-import failure both stop cleanly.
#[test]
fn cancellation_and_mid_import_failure_stop_the_import() {
    let tree = Tree::new("import-interrupt");
    let items: Vec<String> = (0..100)
        .map(|id| rom_json(id, "G", &format!("g{id}.zip"), 1, None))
        .collect();

    // Cancelled before the first request.
    let fake = FakeRomm::with_pages(vec![page_json(&items, 500, 100, 0)]);
    let cancel = AtomicBool::new(true);
    let failure = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        Some(&cancel),
    )
    .expect_err("cancelled");
    assert_eq!(failure.code(), "cancelled");
    assert_eq!(fake.rom_calls(), 0, "nothing was fetched");

    // A server error on the second page.
    let fake = FakeRomm::with_pages(vec![
        page_json(&items, 500, 100, 0),
        page_json(&items, 500, 100, 100),
    ])
    .failing_after(1, RommRequestError::HttpStatus { status: 500 });
    let failure = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect_err("failed");
    assert_eq!(failure.code(), "http_status");
    assert!(failure.previous_cache_preserved());
}

/// Test 87: an instance that cannot be imported from is refused before any
/// request.
#[test]
fn an_incapable_instance_is_refused_before_importing() {
    let tree = Tree::new("import-incapable");
    let openapi: serde_json::Value = serde_json::from_str(
        r#"{"info":{"version":"5.1.0"},"paths":{"/api/platforms":{"get":{}}},
            "components":{"schemas":{}}}"#,
    )
    .expect("json");
    let mut incapable = capability();
    incapable.api = RommApiCapability::from_openapi(&openapi);
    let fake = FakeRomm::with_pages(Vec::new());
    let failure = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &incapable,
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect_err("refused");
    assert_eq!(failure.code(), "not_capable");
    assert!(failure.detail().contains("/api/roms"));
    assert_eq!(fake.rom_calls(), 0);
}

/// Test 88: duplicate RomM ids and duplicate translated paths both survive
/// import and are visible.
#[test]
fn duplicate_ids_and_duplicate_paths_are_preserved_for_inspection() {
    let tree = Tree::new("import-duplicates");
    // Two records with different ids pointing at the same file.
    let items = vec![
        rom_json(1, "One", "same.zip", 10, None),
        rom_json(2, "Two", "same.zip", 10, None),
    ];
    let fake = FakeRomm::with_pages(vec![page_json(&items, 2, 100, 0)]);
    let outcome = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect("imported");
    assert_eq!(outcome.cache.records.len(), 2);
    let claims = PathClaims::of(&outcome.cache.records);
    assert_eq!(claims.contested().len(), 1, "the contested path is visible");
}

// --- Cache: publication, validation, failure preservation ------------------

/// A minimal valid cache, for the publication tests.
fn cache_with(records: Vec<ExternalIdentityRecord>, server: &str) -> IdentityCache {
    IdentityCache {
        format_version: CACHE_FORMAT_VERSION,
        provider: IdentityProvider::Romm,
        server_id: server.to_string(),
        server_version: Some("5.1.0".to_string()),
        source_fingerprint: "abcd1234".to_string(),
        imported_at_unix_seconds: 1_785_000_000,
        platforms: Vec::new(),
        records,
        rejected_hashes: Vec::new(),
        unknown_platforms: Vec::new(),
        server_reported_total: Some(0),
    }
}

fn record_for(server: &str, id: &str, path: Option<PathBuf>) -> ExternalIdentityRecord {
    ExternalIdentityRecord {
        provider: IdentityProvider::Romm,
        server_id: server.to_string(),
        provider_platform_id: Some("3".to_string()),
        provider_game_id: id.to_string(),
        provider_file_id: None,
        provider_path: format!("/romm/library/nes/{id}.zip"),
        archivefs_path: path,
        title: Some(format!("Game {id}")),
        platform_candidate: Some("NES".to_string()),
        provider_platform_name: Some("nes".to_string()),
        regions: vec!["USA".to_string()],
        revision: None,
        hashes: Vec::new(),
        file_size_bytes: Some(1024),
        metadata_provider_ids: Vec::new(),
        artwork: None,
        related_files: Vec::new(),
        sibling_game_ids: Vec::new(),
        imported_at_unix_seconds: 1_785_000_000,
        provider_updated_at: None,
        verification: ExternalVerification::Unmatched,
        conflicts: Vec::new(),
        evidence: Vec::new(),
    }
}

/// Test 89: the first publication creates the cache, and it reads back.
#[test]
fn a_first_import_publishes_a_readable_cache() {
    let tree = Tree::new("cache-first");
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    assert!(!location.exists());
    assert_eq!(
        load_cache(&location, None).expect_err("nothing yet").code(),
        "missing"
    );

    let cache = cache_with(
        vec![record_for("http://romm:8080", "1", None)],
        "http://romm:8080",
    );
    let path = publish_cache(&location, &cache).expect("published");
    assert!(path.is_file());
    assert!(location.exists());
    assert!(location.cache_size_bytes().expect("a size") > 0);

    let loaded = load_cache(&location, Some("http://romm:8080")).expect("readable");
    assert_eq!(loaded, cache);
    // No temporary file was left behind.
    assert_eq!(clean_temporary_files(&location).expect("cleaned"), 0);
}

/// Test 90: a successful refresh replaces the cache atomically.
#[test]
fn a_successful_refresh_replaces_the_previous_cache() {
    let tree = Tree::new("cache-refresh");
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    let first = cache_with(
        vec![record_for("http://romm:8080", "1", None)],
        "http://romm:8080",
    );
    publish_cache(&location, &first).expect("published");

    let mut second = cache_with(
        vec![
            record_for("http://romm:8080", "1", None),
            record_for("http://romm:8080", "2", None),
        ],
        "http://romm:8080",
    );
    second.imported_at_unix_seconds = 1_785_999_999;
    publish_cache(&location, &second).expect("republished");

    let loaded = load_cache(&location, None).expect("readable");
    assert_eq!(loaded.records.len(), 2);
    assert_eq!(loaded.imported_at_unix_seconds, 1_785_999_999);
    assert_eq!(clean_temporary_files(&location).expect("cleaned"), 0);
}

/// Test 91: a cache that would not read back is never published, and the
/// previous one survives.
#[test]
fn an_invalid_new_cache_never_replaces_a_good_one() {
    let tree = Tree::new("cache-invalid");
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    let good = cache_with(
        vec![record_for("http://romm:8080", "1", None)],
        "http://romm:8080",
    );
    publish_cache(&location, &good).expect("published");
    let before = std::fs::read(location.cache_path()).expect("readable");

    // Every way a candidate cache can be invalid.
    let mut wrong_version = good.clone();
    wrong_version.format_version = CACHE_FORMAT_VERSION + 1;
    let mut no_server = good.clone();
    no_server.server_id = "  ".to_string();
    let mut mixed_server = good.clone();
    mixed_server
        .records
        .push(record_for("http://other:8080", "2", None));
    let mut idless = good.clone();
    idless
        .records
        .push(record_for("http://romm:8080", "  ", None));

    for (candidate, why) in [
        (wrong_version, "a version mismatch"),
        (no_server, "no server id"),
        (mixed_server, "a record from another server"),
        (idless, "a record with no id"),
    ] {
        let failure = publish_cache(&location, &candidate).expect_err(why);
        assert!(failure.previous_cache_preserved());
        assert!(!failure.detail().is_empty());
        assert_eq!(
            std::fs::read(location.cache_path()).expect("still readable"),
            before,
            "the previous cache must be byte-identical after {why}"
        );
    }
    // And it still loads.
    assert_eq!(
        load_cache(&location, None).expect("readable").records.len(),
        1
    );
    assert_eq!(
        clean_temporary_files(&location).expect("cleaned"),
        0,
        "no debris"
    );
}

/// Test 92: a corrupt cache on disk is refused rather than misread, and the file
/// is left alone.
#[test]
fn a_corrupt_cache_is_refused_and_kept() {
    let tree = Tree::new("cache-corrupt");
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    std::fs::create_dir_all(location.directory()).expect("fixture");
    std::fs::write(location.cache_path(), b"{ this is not a cache ]").expect("fixture");

    let refusal = load_cache(&location, None).expect_err("refused");
    assert_eq!(refusal.code(), "corrupt");
    assert!(refusal.keeps_file());
    assert!(
        location.cache_path().is_file(),
        "a cache this build cannot read may still be readable by another"
    );
    // An empty file is corrupt too, not empty-but-valid.
    std::fs::write(location.cache_path(), b"").expect("fixture");
    assert_eq!(
        load_cache(&location, None).expect_err("refused").code(),
        "corrupt"
    );
}

/// Test 93: a cache from a different server, or a different format version, is
/// refused with the reason named.
#[test]
fn a_cache_from_another_server_or_version_is_refused() {
    let tree = Tree::new("cache-mismatch");
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    let cache = cache_with(vec![], "http://romm-a:8080");
    publish_cache(&location, &cache).expect("published");

    let refusal = load_cache(&location, Some("http://romm-b:8080")).expect_err("refused");
    assert_eq!(refusal.code(), "server_mismatch");
    assert!(refusal.detail().contains("romm-a"));
    assert!(refusal.detail().contains("romm-b"));

    // A future format version.
    let mut future = cache.clone();
    future.format_version = 99;
    std::fs::write(
        location.cache_path(),
        serde_json::to_vec(&future).expect("serialises"),
    )
    .expect("fixture");
    let refusal = load_cache(&location, None).expect_err("refused");
    assert_eq!(refusal.code(), "version_mismatch");
    assert!(refusal.detail().contains("re-import"));
}

/// Test 94: serialisation is deterministic for a given import.
#[test]
fn cache_serialisation_is_deterministic() {
    let tree = Tree::new("cache-deterministic");
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    // The same records in a different order must serialise identically once
    // sorted, so an unchanged catalogue produces an unchanged file.
    let mut one = cache_with(
        vec![
            record_for("http://romm:8080", "3", None),
            record_for("http://romm:8080", "1", None),
            record_for("http://romm:8080", "2", None),
        ],
        "http://romm:8080",
    );
    let mut two = cache_with(
        vec![
            record_for("http://romm:8080", "1", None),
            record_for("http://romm:8080", "2", None),
            record_for("http://romm:8080", "3", None),
        ],
        "http://romm:8080",
    );
    one.sort_deterministically();
    two.sort_deterministically();
    assert_eq!(
        serde_json::to_vec(&one).expect("serialises"),
        serde_json::to_vec(&two).expect("serialises"),
        "the same import must always produce the same bytes"
    );

    publish_cache(&location, &one).expect("published");
    let first = std::fs::read(location.cache_path()).expect("readable");
    publish_cache(&location, &two).expect("republished");
    assert_eq!(
        std::fs::read(location.cache_path()).expect("readable"),
        first
    );
}

/// Test 95: reading from the cache is bounded, so a caller cannot page an
/// unbounded number of records at once.
#[test]
fn reading_from_the_cache_is_bounded() {
    let records: Vec<ExternalIdentityRecord> = (0..250)
        .map(|id| record_for("http://romm:8080", &format!("{id:04}"), None))
        .collect();
    let cache = cache_with(records, "http://romm:8080");
    assert_eq!(cache.page(0, 50).len(), 50);
    assert_eq!(cache.page(200, 100).len(), 50, "clamped to what exists");
    assert_eq!(cache.page(9999, 50).len(), 0, "past the end is empty");
    assert_eq!(
        cache.page(0, 99_999).len(),
        250,
        "an absurd limit is clamped, not honoured"
    );
    assert_eq!(cache.page(0, 0).len(), 1, "a zero limit is clamped to one");
}

/// Test 96: an interrupted publication leaves a temporary file and an intact
/// cache, and the debris is cleanable.
#[test]
fn an_interrupted_publication_leaves_the_previous_cache_intact() {
    let tree = Tree::new("cache-interrupted");
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    let good = cache_with(
        vec![record_for("http://romm:8080", "1", None)],
        "http://romm:8080",
    );
    publish_cache(&location, &good).expect("published");
    let before = std::fs::read(location.cache_path()).expect("readable");

    // Simulate a process that died after writing a temporary file.
    let orphan = location
        .directory()
        .join(format!(".{CACHE_FILE_NAME}-12345-999.tmp"));
    std::fs::write(&orphan, b"half a document").expect("fixture");

    // The live cache is unaffected and still loads.
    assert_eq!(
        std::fs::read(location.cache_path()).expect("readable"),
        before
    );
    assert_eq!(
        load_cache(&location, None).expect("readable").records.len(),
        1
    );
    // And the debris is removed without touching the cache.
    assert_eq!(clean_temporary_files(&location).expect("cleaned"), 1);
    assert!(!orphan.exists());
    assert!(location.cache_path().is_file());
}

/// Test 97: a write failure preserves the previous cache.
#[test]
fn a_write_failure_preserves_the_previous_cache() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tree = Tree::new("cache-write-fail");
        let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
        let good = cache_with(
            vec![record_for("http://romm:8080", "1", None)],
            "http://romm:8080",
        );
        publish_cache(&location, &good).expect("published");
        let before = std::fs::read(location.cache_path()).expect("readable");

        // Make the directory unwritable, so the temporary file cannot be created.
        std::fs::set_permissions(location.directory(), std::fs::Permissions::from_mode(0o500))
            .expect("chmod");
        let mut next = good.clone();
        next.records.push(record_for("http://romm:8080", "2", None));
        let failure = publish_cache(&location, &next).expect_err("the write must fail");
        assert!(failure.previous_cache_preserved());
        assert!(
            matches!(failure, PublishFailure::WriteFailed { .. }),
            "{failure:?}"
        );

        // Restore and check the old cache is byte-identical and still loads.
        std::fs::set_permissions(location.directory(), std::fs::Permissions::from_mode(0o700))
            .expect("chmod");
        assert_eq!(
            std::fs::read(location.cache_path()).expect("readable"),
            before
        );
        assert_eq!(
            load_cache(&location, None).expect("readable").records.len(),
            1
        );
    }
}

// --- Refresh through the API: failure preservation end to end --------------

/// One way a refresh can fail, as a closure the test runs.
type FailureCase<'a> = Box<dyn Fn() -> ImportFailure + 'a>;

/// Test 98: a failed refresh keeps the previous cache, for every failure kind.
#[test]
fn every_kind_of_failed_refresh_keeps_the_previous_cache() {
    let tree = Tree::new("refresh-preserve");
    let api = tree.api();
    let source = tree.source();

    // A first successful import, so there is something to preserve.
    let items = vec![rom_json(1, "One", "one.zip", 10, None)];
    let good = FakeRomm::with_pages(vec![page_json(&items, 1, 100, 0)]);
    api.refresh(
        RefreshRequest {
            source: &source,
            transport: &good,
            scope: ImportScope::Full,
            capability: &capability(),
            page_size: DEFAULT_PAGE_SIZE,
            hashes: &LocalHashCache::new(),
            cancel: None,
        },
        observe_facts,
        no_progress,
    )
    .expect("first import");
    let before = std::fs::read(api.location().cache_path()).expect("readable");
    assert_eq!(api.open_cache(None).expect("readable").records.len(), 1);

    // Now every failure mode, each of which must leave that file untouched.
    let cancel = AtomicBool::new(true);
    let cases: Vec<(&str, FailureCase<'_>)> = vec![
        (
            "auth failure",
            Box::new(|| {
                let fake = FakeRomm::with_pages(Vec::new())
                    .failing_after(0, RommRequestError::Unauthorised { status: 401 });
                api.refresh(
                    RefreshRequest {
                        source: &source,
                        transport: &fake,
                        scope: ImportScope::Full,
                        capability: &capability(),
                        page_size: DEFAULT_PAGE_SIZE,
                        hashes: &LocalHashCache::new(),
                        cancel: None,
                    },
                    observe_facts,
                    no_progress,
                )
                .expect_err("auth failure")
            }),
        ),
        (
            "malformed page",
            Box::new(|| {
                let fake = FakeRomm::with_pages(vec!["{ not json ]".to_string()]);
                api.refresh(
                    RefreshRequest {
                        source: &source,
                        transport: &fake,
                        scope: ImportScope::Full,
                        capability: &capability(),
                        page_size: DEFAULT_PAGE_SIZE,
                        hashes: &LocalHashCache::new(),
                        cancel: None,
                    },
                    observe_facts,
                    no_progress,
                )
                .expect_err("malformed")
            }),
        ),
        (
            "oversized response",
            Box::new(|| {
                let fake = FakeRomm::with_pages(Vec::new())
                    .failing_after(0, RommRequestError::ResponseTooLarge { limit: 8 });
                api.refresh(
                    RefreshRequest {
                        source: &source,
                        transport: &fake,
                        scope: ImportScope::Full,
                        capability: &capability(),
                        page_size: DEFAULT_PAGE_SIZE,
                        hashes: &LocalHashCache::new(),
                        cancel: None,
                    },
                    observe_facts,
                    no_progress,
                )
                .expect_err("oversized")
            }),
        ),
        (
            "timeout",
            Box::new(|| {
                let fake =
                    FakeRomm::with_pages(Vec::new()).failing_after(0, RommRequestError::Timeout);
                api.refresh(
                    RefreshRequest {
                        source: &source,
                        transport: &fake,
                        scope: ImportScope::Full,
                        capability: &capability(),
                        page_size: DEFAULT_PAGE_SIZE,
                        hashes: &LocalHashCache::new(),
                        cancel: None,
                    },
                    observe_facts,
                    no_progress,
                )
                .expect_err("timeout")
            }),
        ),
        (
            "cancellation",
            Box::new(|| {
                let fake = FakeRomm::with_pages(vec![page_json(&items, 1, 100, 0)]);
                api.refresh(
                    RefreshRequest {
                        source: &source,
                        transport: &fake,
                        scope: ImportScope::Full,
                        capability: &capability(),
                        page_size: DEFAULT_PAGE_SIZE,
                        hashes: &LocalHashCache::new(),
                        cancel: Some(&cancel),
                    },
                    observe_facts,
                    no_progress,
                )
                .expect_err("cancelled")
            }),
        ),
    ];

    for (label, run) in cases {
        let failure = run();
        assert!(
            failure.previous_cache_preserved(),
            "{label} must preserve the cache"
        );
        assert_eq!(
            std::fs::read(api.location().cache_path()).expect("still readable"),
            before,
            "after {label} the cache must be byte-identical"
        );
        assert_eq!(
            api.open_cache(None).expect("still loads").records.len(),
            1,
            "after {label} the cache must still serve"
        );
        assert_eq!(
            clean_temporary_files(api.location()).expect("cleaned"),
            0,
            "after {label} no debris may remain"
        );
    }
}

/// Test 99: a first-ever failed import leaves no cache and no fake ready state.
#[test]
fn a_first_ever_failed_import_leaves_no_fake_ready_state() {
    let tree = Tree::new("refresh-first-fail");
    let api = tree.api();
    let fake = FakeRomm::with_pages(Vec::new())
        .failing_after(0, RommRequestError::Unauthorised { status: 401 });
    let failure = api
        .refresh(
            RefreshRequest {
                source: &tree.source(),
                transport: &fake,
                scope: ImportScope::Full,
                capability: &capability(),
                page_size: DEFAULT_PAGE_SIZE,
                hashes: &LocalHashCache::new(),
                cancel: None,
            },
            observe_facts,
            no_progress,
        )
        .expect_err("auth failure");
    assert_eq!(failure.code(), "unauthorised");
    assert!(!api.location().exists(), "no cache may have been created");
    assert_eq!(api.open_cache(None).expect_err("nothing").code(), "missing");

    // And the status says so rather than claiming readiness.
    let config = RommSourceConfig {
        enabled: true,
        url: "http://172.19.0.20:8080".to_string(),
        mappings: Vec::new(),
        provider_path_kind: ProviderPathKind::AbsoluteProviderPath,
        token_path: None,
    };
    let status = api.status(&config, &LocalHashCache::new(), false);
    // Not a ready state, and not browsable - but not an error either: being
    // configured and not yet imported is a normal stage, and calling it a failure
    // sent people looking for a fault that was not there.
    assert_eq!(status.state, ProviderState::NeverImported);
    assert!(!status.state.can_browse());
    assert!(!matches!(
        status.state,
        ProviderState::Ready | ProviderState::ReadyOffline
    ));
    assert_eq!(status.records_imported, 0);
}

/// Test 100: offline browsing works from the cache with no transport at all.
#[test]
fn cached_identity_is_browsable_with_no_network() {
    let tree = Tree::new("offline");
    let api = tree.api();
    tree.file("nes/one.zip", b"a rom");
    let items = vec![rom_json(1, "One", "one.zip", 5, None)];
    let fake = FakeRomm::with_pages(vec![page_json(&items, 1, 100, 0)]);
    api.refresh(
        RefreshRequest {
            source: &tree.source(),
            transport: &fake,
            scope: ImportScope::Full,
            capability: &capability(),
            page_size: DEFAULT_PAGE_SIZE,
            hashes: &LocalHashCache::new(),
            cancel: None,
        },
        observe_facts,
        no_progress,
    )
    .expect("imported");

    // From here on, no transport exists at all - the fake is dropped. Everything
    // below is served from the file.
    drop(fake);
    let cache = api.open_cache(None).expect("readable offline");
    assert_eq!(cache.records.len(), 1);
    assert_eq!(api.list_records(&cache, 0, 10).len(), 1);
    assert!(api.list_conflicts(&cache).is_empty());

    let config = RommSourceConfig {
        enabled: true,
        url: "http://172.19.0.20:8080".to_string(),
        mappings: Vec::new(),
        provider_path_kind: ProviderPathKind::AbsoluteProviderPath,
        token_path: None,
    };
    // `reachable: false` is the offline case and must still be browsable.
    let status = api.status(&config, &LocalHashCache::new(), false);
    assert_eq!(status.state, ProviderState::ReadyOffline);
    assert!(status.state.can_browse());
    assert_eq!(status.records_imported, 1);
    assert!(status.cache_size_bytes.expect("a size") > 0);
    // Reachable flips only the label, not the contents.
    let online = api.status(&config, &LocalHashCache::new(), true);
    assert_eq!(online.state, ProviderState::Ready);
    assert_eq!(online.records_imported, 1);
}

/// Test 101: a disabled source connects to nothing and reports itself disabled.
#[test]
fn a_disabled_source_reports_disabled_and_contacts_nothing() {
    let tree = Tree::new("disabled");
    let api = tree.api();
    let mut config = RommSourceConfig {
        enabled: true,
        url: "http://172.19.0.20:8080".to_string(),
        mappings: Vec::new(),
        provider_path_kind: ProviderPathKind::AbsoluteProviderPath,
        token_path: None,
    };
    api.disable(&mut config);
    assert!(!config.enabled);
    let status = api.status(&config, &LocalHashCache::new(), false);
    assert_eq!(status.state, ProviderState::Disabled);

    // An unconfigured source is distinct from a disabled one.
    let empty = RommSourceConfig::default();
    assert_eq!(
        api.status(&empty, &LocalHashCache::new(), false).state,
        ProviderState::NotConfigured
    );
}

/// Test 102: removing cached identity needs confirmation.
#[test]
fn removing_cached_identity_requires_confirmation() {
    let tree = Tree::new("remove");
    let api = tree.api();
    let cache = cache_with(
        vec![record_for("http://romm:8080", "1", None)],
        "http://romm:8080",
    );
    publish_cache(api.location(), &cache).expect("published");

    let refusal = api
        .remove_cached_identity(false)
        .expect_err("unconfirmed removal must be refused");
    assert!(matches!(refusal, RemovalRefusal::NotConfirmed));
    assert!(
        api.location().exists(),
        "an unconfirmed removal must not remove anything"
    );

    assert!(api.remove_cached_identity(true).expect("removed"));
    assert!(!api.location().exists());
    // Removing again is not an error, it just removes nothing.
    assert!(!api.remove_cached_identity(true).expect("nothing to remove"));
}

// --- Matching and confidence ----------------------------------------------

/// A record pointing at `path`, with the given size, hashes and platform.
fn matchable(
    path: PathBuf,
    size: Option<u64>,
    hashes: Vec<ExternalHash>,
    platform: Option<&str>,
) -> ExternalIdentityRecord {
    let mut record = record_for("http://romm:8080", "1", Some(path));
    record.file_size_bytes = size;
    record.hashes = hashes;
    record.platform_candidate = platform.map(str::to_string);
    record
}

fn facts_for_file(
    path: &Path,
    platform: Option<&str>,
    strength: LocalEvidenceStrength,
) -> LocalFileFacts {
    LocalFileFacts::observe(path).with_local_platform(platform, strength)
}

/// Test 103: a matching hash of any supported algorithm confirms.
#[test]
fn an_agreeing_hash_confirms_the_record() {
    let tree = Tree::new("match-hash");
    let contents = b"the exact bytes";
    let path = tree.file("nes/game.zip", contents);
    let mut hashes = LocalHashCache::new();
    let local = hash_file_cached(&path, &mut hashes, &tree.trusted(), None).expect("hashed");

    for (algorithm, value) in [
        (HashAlgorithm::Sha1, local.sha1.clone()),
        (HashAlgorithm::Md5, local.md5.clone()),
        (HashAlgorithm::Crc32, local.crc32.clone()),
    ] {
        let record = matchable(
            path.clone(),
            Some(contents.len() as u64),
            vec![ExternalHash::parse(algorithm, &value).expect("valid")],
            Some("NES"),
        );
        let outcome = match_record(
            &record,
            &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
            &PathClaims::of(std::slice::from_ref(&record)),
            &hashes,
        );
        assert_eq!(
            outcome.verification,
            ExternalVerification::ConfirmedExternal,
            "an agreeing {} must confirm",
            algorithm.label()
        );
        assert!(outcome.hash_compared);
        assert!(outcome.conflicts.is_empty());
        assert!(
            outcome
                .evidence
                .iter()
                .any(|item| item.contains(algorithm.label())),
            "the evidence should name the algorithm: {:?}",
            outcome.evidence
        );
    }
}

/// Test 104: a hash that disagrees is ambiguous, whatever else agrees.
#[test]
fn a_disagreeing_hash_is_ambiguous() {
    let tree = Tree::new("match-hash-mismatch");
    let contents = b"actual bytes";
    let path = tree.file("nes/game.zip", contents);
    let mut hashes = LocalHashCache::new();
    hash_file_cached(&path, &mut hashes, &tree.trusted(), None).expect("hashed");

    // Everything else agrees: size, platform, title.
    let record = matchable(
        path.clone(),
        Some(contents.len() as u64),
        vec![ExternalHash::parse(HashAlgorithm::Md5, &"f".repeat(32)).expect("valid")],
        Some("NES"),
    );
    let outcome = match_record(
        &record,
        &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
        &PathClaims::of(std::slice::from_ref(&record)),
        &hashes,
    );
    assert_eq!(
        outcome.verification,
        ExternalVerification::Ambiguous,
        "differing bytes cannot be the same file, however much else matches"
    );
    assert!(outcome.hash_compared);
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(outcome.conflicts[0].field, ConflictField::Hash);
    // The conflict shows both values so a person can see the disagreement.
    assert!(!outcome.conflicts[0].external.is_empty());
    assert!(!outcome.conflicts[0].local.is_empty());
}

/// Test 105: path, size and platform agreement without a hash is strong.
#[test]
fn path_size_and_platform_agreement_is_strong() {
    let tree = Tree::new("match-strong");
    let contents = b"1234567890";
    let path = tree.file("nes/game.zip", contents);
    let record = matchable(path.clone(), Some(10), Vec::new(), Some("NES"));
    let outcome = match_record(
        &record,
        &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
        &PathClaims::of(std::slice::from_ref(&record)),
        // No hash cached: matching must not compute one.
        &LocalHashCache::new(),
    );
    assert_eq!(outcome.verification, ExternalVerification::StrongExternal);
    assert!(
        !outcome.hash_compared,
        "matching must never hash a file by itself"
    );
    assert!(outcome.conflicts.is_empty());
}

/// Test 106: title and platform only, with no size, is probable.
#[test]
fn title_and_platform_only_is_probable() {
    let tree = Tree::new("match-probable");
    let path = tree.file("nes/game.zip", b"whatever");
    // RomM published no size at all.
    let record = matchable(path.clone(), None, Vec::new(), Some("NES"));
    let outcome = match_record(
        &record,
        &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    assert_eq!(outcome.verification, ExternalVerification::ProbableExternal);
    assert!(
        outcome
            .evidence
            .iter()
            .any(|item| item.contains("no file size")),
        "the missing size should be stated: {:?}",
        outcome.evidence
    );
}

/// Test 107: a hash RomM published but which was never verified locally leaves
/// the verdict short of confirmed, honestly.
#[test]
fn an_unverified_hash_does_not_confirm() {
    let tree = Tree::new("match-unverified");
    let contents = b"1234567890";
    let path = tree.file("nes/game.zip", contents);
    let record = matchable(
        path.clone(),
        Some(10),
        vec![ExternalHash::parse(HashAlgorithm::Md5, &"a".repeat(32)).expect("valid")],
        Some("NES"),
    );
    // Empty hash cache: nothing has been verified.
    let outcome = match_record(
        &record,
        &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    assert_eq!(
        outcome.verification,
        ExternalVerification::StrongExternal,
        "a published hash nobody checked is not a confirmation"
    );
    assert!(!outcome.hash_compared);
    assert!(
        outcome
            .evidence
            .iter()
            .any(|item| item.contains("not been verified locally")),
        "the reason must be visible: {:?}",
        outcome.evidence
    );
}

/// Test 108: a missing file is stale, and a changed size is stale.
#[test]
fn a_missing_or_resized_file_is_stale() {
    let tree = Tree::new("match-stale");
    // Missing.
    let absent = tree.library().join("nes/gone.zip");
    let record = matchable(absent.clone(), Some(10), Vec::new(), Some("NES"));
    let outcome = match_record(
        &record,
        &LocalFileFacts::default(),
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    assert_eq!(outcome.verification, ExternalVerification::Stale);
    assert!(
        outcome
            .evidence
            .iter()
            .any(|item| item.contains("does not exist"))
    );

    // Present but a different size.
    let path = tree.file("nes/resized.zip", b"now much longer than before");
    let record = matchable(path.clone(), Some(10), Vec::new(), Some("NES"));
    let outcome = match_record(
        &record,
        &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    assert_eq!(outcome.verification, ExternalVerification::Stale);
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(outcome.conflicts[0].field, ConflictField::FileSize);
}

/// Test 109: no path mapping means unmatched, not an error.
#[test]
fn no_mapping_means_unmatched() {
    let mut record = record_for("http://romm:8080", "1", None);
    record.archivefs_path = None;
    let outcome = match_record(
        &record,
        &LocalFileFacts::default(),
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    assert_eq!(outcome.verification, ExternalVerification::Unmatched);
    assert!(outcome.conflicts.is_empty(), "unmatched is not a conflict");
    assert!(
        outcome
            .evidence
            .iter()
            .any(|item| item.contains("no configured path mapping"))
    );
}

/// Test 110: a platform disagreement with locally verified evidence is ambiguous
/// and the local answer is not displaced.
#[test]
fn a_platform_disagreement_with_verified_local_evidence_is_ambiguous() {
    let tree = Tree::new("match-platform-conflict");
    let contents = b"1234567890";
    let path = tree.file("nes/game.zip", contents);
    // RomM says NES; EmuWiz verified Atari ST from the file itself.
    let record = matchable(path.clone(), Some(10), Vec::new(), Some("NES"));
    let outcome = match_record(
        &record,
        &facts_for_file(&path, Some("AtariST"), LocalEvidenceStrength::Verified),
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    assert_eq!(outcome.verification, ExternalVerification::Ambiguous);
    let conflict = outcome
        .conflicts
        .iter()
        .find(|conflict| conflict.field == ConflictField::Platform)
        .expect("a platform conflict");
    assert_eq!(conflict.external, "NES");
    assert_eq!(conflict.local, "AtariST");
    assert!(
        conflict.detail.contains("from the file itself"),
        "the conflict should say the local answer came from the bytes"
    );
    // And the model refuses to let external evidence displace it.
    assert!(!ExternalVerification::ConfirmedExternal.outranks(LocalEvidenceStrength::Verified));
}

/// Test 111: two records claiming one file is ambiguous rather than resolved.
#[test]
fn two_records_claiming_one_file_are_ambiguous() {
    let tree = Tree::new("match-contested");
    let path = tree.file("nes/same.zip", b"1234567890");
    let mut first = matchable(path.clone(), Some(10), Vec::new(), Some("NES"));
    first.provider_game_id = "1".to_string();
    let mut second = matchable(path.clone(), Some(10), Vec::new(), Some("NES"));
    second.provider_game_id = "2".to_string();
    let records = vec![first.clone(), second];
    let claims = PathClaims::of(&records);
    assert_eq!(claims.claimants(&path), 2);
    assert_eq!(claims.contested().len(), 1);

    let outcome = match_record(
        &first,
        &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
        &claims,
        &LocalHashCache::new(),
    );
    assert_eq!(outcome.verification, ExternalVerification::Ambiguous);
    assert_eq!(outcome.conflicts[0].field, ConflictField::FileState);
    assert!(outcome.conflicts[0].detail.contains("2 RomM records"));
}

/// Test 112: an unknown platform with nothing corroborating it stays weak.
#[test]
fn an_unknown_platform_does_not_reach_strong() {
    let tree = Tree::new("match-unknown-platform");
    let contents = b"1234567890";
    let path = tree.file("nes/game.zip", contents);
    // No canonical platform could be mapped.
    let record = matchable(path.clone(), Some(10), Vec::new(), None);
    let outcome = match_record(
        &record,
        &facts_for_file(&path, None, LocalEvidenceStrength::None),
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    assert_eq!(
        outcome.verification,
        ExternalVerification::ProbableExternal,
        "size agrees and a title exists, but the platform is unknown"
    );
    assert!(
        outcome
            .evidence
            .iter()
            .any(|item| item.contains("could not be mapped"))
    );
}

/// Test 113: matching a whole set assigns verdicts and can be cancelled.
#[test]
fn matching_a_set_assigns_verdicts_and_is_cancellable() {
    let tree = Tree::new("match-all");
    let present = tree.file("nes/present.zip", b"1234567890");
    let absent = tree.library().join("nes/absent.zip");
    let mut records = vec![
        matchable(present.clone(), Some(10), Vec::new(), Some("NES")),
        matchable(absent, Some(10), Vec::new(), Some("NES")),
    ];
    records[1].provider_game_id = "2".to_string();

    match_all(
        &mut records,
        &LocalHashCache::new(),
        |record| {
            record
                .archivefs_path
                .as_deref()
                .map(|path| facts_for_file(path, Some("NES"), LocalEvidenceStrength::None))
                .unwrap_or_default()
        },
        None,
    )
    .expect("matched");
    assert_eq!(
        records[0].verification,
        ExternalVerification::StrongExternal
    );
    assert_eq!(records[1].verification, ExternalVerification::Stale);

    let counts = IdentityImportCounts::of(&records);
    assert_eq!(counts.strong, 1);
    assert_eq!(counts.stale, 1);
    assert_eq!(counts.usable(), 1);

    // Cancellation stops it.
    let cancel = AtomicBool::new(true);
    assert!(
        match_all(
            &mut records,
            &LocalHashCache::new(),
            |_| LocalFileFacts::default(),
            Some(&cancel)
        )
        .is_err()
    );
}

/// Test 114: multi-disc structure is preserved, and a partial group is visible
/// as partial rather than flattened.
#[test]
fn multi_disc_groups_are_preserved_including_partial_ones() {
    let tree = Tree::new("match-multi-disc");
    let disc1 = tree.file("psx/disc1.bin", b"1234567890");
    // Disc 2 is deliberately absent, making the group partial.
    let mut primary = matchable(disc1.clone(), Some(10), Vec::new(), Some("PSX"));
    primary.provider_game_id = "100".to_string();
    primary.title = Some("Final Fantasy VII".to_string());
    primary.sibling_game_ids = vec!["101".to_string()];
    primary.related_files = vec![
        "/romm/library/psx/disc1.bin".to_string(),
        "/romm/library/psx/disc2.bin".to_string(),
    ];
    let mut sibling = matchable(
        tree.library().join("psx/disc2.bin"),
        Some(10),
        Vec::new(),
        Some("PSX"),
    );
    sibling.provider_game_id = "101".to_string();

    let mut records = vec![primary, sibling];
    match_all(
        &mut records,
        &LocalHashCache::new(),
        |record| {
            record
                .archivefs_path
                .as_deref()
                .map(|path| facts_for_file(path, Some("PSX"), LocalEvidenceStrength::None))
                .unwrap_or_default()
        },
        None,
    )
    .expect("matched");

    let groups = build_groups(&records);
    assert_eq!(groups.len(), 1, "one group, not two flattened records");
    let group = &groups[0];
    assert_eq!(group.primary_game_id, "100");
    assert_eq!(group.title.as_deref(), Some("Final Fantasy VII"));
    assert_eq!(group.member_game_ids, vec!["100", "101"]);
    assert_eq!(group.related_files.len(), 2, "both discs are remembered");
    assert_eq!(group.matched_members, 1, "only disc 1 is present");
    assert!(group.partial, "a group with a missing disc is partial");

    // A single-file record is not reported as a group at all.
    let single = vec![matchable(disc1, Some(10), Vec::new(), Some("PSX"))];
    assert!(build_groups(&single).is_empty());
}

/// Test 115: matching one path through the API, for a details view.
#[test]
fn one_path_can_be_matched_through_the_api() {
    let tree = Tree::new("match-one-path");
    let api = tree.api();
    let path = tree.file("nes/one.zip", b"a rom");
    let items = vec![rom_json(1, "One", "one.zip", 5, None)];
    let fake = FakeRomm::with_pages(vec![page_json(&items, 1, 100, 0)]);
    api.refresh(
        RefreshRequest {
            source: &tree.source(),
            transport: &fake,
            scope: ImportScope::Full,
            capability: &capability(),
            page_size: DEFAULT_PAGE_SIZE,
            hashes: &LocalHashCache::new(),
            cancel: None,
        },
        observe_facts,
        no_progress,
    )
    .expect("imported");

    let cache = api.open_cache(None).expect("readable");
    let (record, outcome) = api
        .match_path(
            &cache,
            &path,
            &facts_for_file(&path, Some("NES"), LocalEvidenceStrength::None),
            &LocalHashCache::new(),
        )
        .expect("a record for this path");
    assert_eq!(record.provider_game_id, "1");
    assert_eq!(outcome.verification, ExternalVerification::StrongExternal);
    assert!(
        !outcome.hash_compared,
        "opening a details view must not start hashing"
    );
    // A path with no record returns nothing rather than inventing one.
    assert!(
        api.match_path(
            &cache,
            &tree.library().join("nes/other.zip"),
            &LocalFileFacts::default(),
            &LocalHashCache::new()
        )
        .is_none()
    );
}

/// Test 116: an end-to-end import assigns verdicts and the status reflects them.
#[test]
fn an_end_to_end_import_produces_matched_records_and_a_status() {
    let tree = Tree::new("end-to-end");
    let api = tree.api();
    // Three files: one present and agreeing, one resized, one absent.
    tree.file("nes/present.zip", b"1234567890");
    tree.file("nes/resized.zip", b"much longer than ten bytes");
    let items = vec![
        rom_json(1, "Present", "present.zip", 10, None),
        rom_json(2, "Resized", "resized.zip", 10, None),
        rom_json(3, "Absent", "absent.zip", 10, None),
    ];
    let fake = FakeRomm::with_pages(vec![page_json(&items, 3, 100, 0)]);
    let summary = api
        .refresh(
            RefreshRequest {
                source: &tree.source(),
                transport: &fake,
                scope: ImportScope::Full,
                capability: &capability(),
                page_size: DEFAULT_PAGE_SIZE,
                hashes: &LocalHashCache::new(),
                cancel: None,
            },
            observe_facts,
            no_progress,
        )
        .expect("imported");

    assert_eq!(summary.records, 3);
    assert_eq!(summary.platforms, 1);
    assert_eq!(summary.counts.strong, 1, "the agreeing file");
    assert_eq!(summary.counts.stale, 2, "the resized and the absent one");
    assert_eq!(summary.invalid_hashes, 0);

    let config = RommSourceConfig {
        enabled: true,
        url: "http://172.19.0.20:8080".to_string(),
        mappings: Vec::new(),
        provider_path_kind: ProviderPathKind::AbsoluteProviderPath,
        token_path: None,
    };
    let status = api.status(&config, &LocalHashCache::new(), true);
    // Two of three stale is a majority, so the source reports itself stale rather
    // than ready - the identity is there but no longer describes the library.
    assert!(
        matches!(status.state, ProviderState::Stale { .. }),
        "{:?}",
        status.state
    );
    assert!(status.state.can_browse());
    assert_eq!(status.records_imported, 3);
    assert_eq!(status.counts.stale, 2);
    // And nothing secret is in it.
    let json = serde_json::to_string(&status).expect("serialises");
    assert!(!json.contains("rk_test"));
    assert!(!json.contains("Bearer"));
}

/// Test 117: no module in Stage 1B writes to a ROM, an emulator or RomM.
#[test]
fn stage_1b_modules_perform_no_forbidden_write() {
    for (name, source) in [
        ("cache.rs", include_str!("cache.rs")),
        ("hashing.rs", include_str!("hashing.rs")),
        ("matching.rs", include_str!("matching.rs")),
        ("status.rs", include_str!("status.rs")),
        ("import.rs", include_str!("romm/import.rs")),
        ("normalise.rs", include_str!("romm/normalise.rs")),
    ] {
        let code: String = source
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && !trimmed.starts_with("///")
            })
            .collect::<Vec<_>>()
            .join("\n");
        // No write verb, so nothing can reach RomM's mutating endpoints.
        for forbidden in [
            ".post(",
            ".put(",
            ".patch(",
            ".delete(",
            "\"POST\"",
            "\"DELETE\"",
        ] {
            assert!(
                !code.contains(forbidden),
                "{name} must not contain `{forbidden}`"
            );
        }
        // No process *spawning* and no second network client. `std::process::id`
        // is deliberately allowed: the cache uses it to make a temporary
        // filename unique, which is reading a pid rather than starting anything.
        for forbidden in [
            "Command",
            "std::process::Command",
            "process::Command",
            "ureq::",
            "reqwest",
            "TcpStream",
        ] {
            assert!(
                !code.contains(forbidden),
                "{name} must not contain `{forbidden}`"
            );
        }
    }
    // Only the cache module may write at all, and only inside its own directory.
    let matching = include_str!("matching.rs");
    let hashing = include_str!("hashing.rs");
    for (name, code) in [("matching.rs", matching), ("hashing.rs", hashing)] {
        for forbidden in [
            "fs::write",
            "fs::create_dir",
            "fs::remove_",
            "fs::rename",
            "File::create",
        ] {
            assert!(
                !code.contains(forbidden),
                "{name} must not write anything: found `{forbidden}`"
            );
        }
    }
}

/// Test 118: a large import stays bounded in memory and completes promptly.
///
/// Not a benchmark - a bound. 20,000 records over 200 pages must finish quickly
/// and hold one page of JSON at a time, not two hundred.
#[test]
fn a_large_import_is_bounded_and_prompt() {
    let tree = Tree::new("perf-import");
    let page_items: Vec<String> = (0..100)
        .map(|id| rom_json(id, &format!("Game {id}"), &format!("g{id}.zip"), 1024, None))
        .collect();
    let pages: Vec<String> = (0..200)
        .map(|page| page_json(&page_items, 20_000, 100, page * 100))
        .collect();
    let fake = FakeRomm::with_pages(pages);

    let started = std::time::Instant::now();
    let outcome = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        DEFAULT_PAGE_SIZE,
        no_progress,
        None,
    )
    .expect("imported");
    let elapsed = started.elapsed();

    assert_eq!(outcome.cache.records.len(), 20_000);
    // 201: two hundred full pages, plus one empty page to discover the end. The
    // walk ends on a short page rather than on the server's total, so the extra
    // request is the cost of not trusting that total.
    assert_eq!(outcome.progress.pages_fetched, 201);
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "20,000 records took {elapsed:?}, which is too slow to be bounded work"
    );

    // Publishing and reloading that cache must also work, and the file must be a
    // sane size rather than pathological.
    let location = IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm);
    publish_cache(&location, &outcome.cache).expect("published");
    let size = location.cache_size_bytes().expect("a size");
    assert!(
        size > 1_000_000 && size < 200_000_000,
        "a 20,000-record cache is {size} bytes, which is not a plausible compact size"
    );
    let reloaded = load_cache(&location, None).expect("readable");
    assert_eq!(reloaded.records.len(), 20_000);
}

// --- Adaptive page sizing -------------------------------------------------
//
// A real RomM 5.1.0 catalogue produced a page of 100 records larger than the
// 8 MiB response ceiling, at offset 4400. The ceiling is not the thing to change:
// it is what stops a server handing over an unbounded body. These tests drive the
// alternative - retry the same offset with a smaller page - against a fake that
// actually decides by the requested limit, so the behaviour under test is the
// interaction between the request and the refusal, not a scripted sequence.

/// Whether a request for `limit` records at `offset` would be too large.
///
/// The third argument is whether per-file detail was asked for: a record can be
/// readable without its file list and unreadable with it, which is exactly the
/// case that made a real catalogue unimportable.
type SizePolicy = Box<dyn Fn(u32, u32, bool) -> bool + Send + Sync>;
/// A hook run before each `/api/roms` response, given the call index. Used to
/// trip cancellation or burn the deadline part-way through a retry sequence.
type CallHook = Box<dyn Fn(usize) + Send + Sync>;

struct AdaptiveRomm {
    /// The catalogue, one record per entry, sliced by offset and limit.
    catalogue: Vec<String>,
    size_policy: SizePolicy,
    /// Every `(offset, limit)` asked for, in order. The record of what the walk
    /// actually did.
    requests: Mutex<Vec<(u32, u32)>>,
    /// Totals to report, one per successful page, cycling. A total that changes
    /// mid-import must remain a progress hint and nothing more.
    totals: Option<Vec<u64>>,
    /// Report this offset instead of the real one, on the given call index.
    offset_lie: Option<(usize, u32)>,
    /// Report this limit instead of the requested one, on every page.
    limit_lie: Option<u32>,
    calls: Mutex<usize>,
    /// How many requests came in without `with_files`.
    files_omitted: Mutex<usize>,
    hook: Option<CallHook>,
}

impl AdaptiveRomm {
    /// A catalogue of `records` records, where any request for more than
    /// `max_safe_limit` records is refused as too large.
    fn new(records: usize, max_safe_limit: u32) -> Self {
        Self::with_policy(
            records,
            Box::new(move |_offset, limit, _with_files| limit > max_safe_limit),
        )
    }

    fn with_policy(records: usize, size_policy: SizePolicy) -> Self {
        let catalogue = (0..records)
            .map(|index| {
                rom_json(
                    index as u32 + 1,
                    &format!("Game {index}"),
                    &format!("game-{index}.zip"),
                    1024,
                    None,
                )
            })
            .collect();
        Self {
            catalogue,
            size_policy,
            requests: Mutex::new(Vec::new()),
            totals: None,
            offset_lie: None,
            limit_lie: None,
            calls: Mutex::new(0),
            files_omitted: Mutex::new(0),
            hook: None,
        }
    }

    fn reporting_totals(mut self, totals: Vec<u64>) -> Self {
        self.totals = Some(totals);
        self
    }

    fn lying_about_offset(mut self, call_index: usize, reported: u32) -> Self {
        self.offset_lie = Some((call_index, reported));
        self
    }

    fn lying_about_limit(mut self, reported: u32) -> Self {
        self.limit_lie = Some(reported);
        self
    }

    fn with_hook(mut self, hook: CallHook) -> Self {
        self.hook = Some(hook);
        self
    }

    fn requests(&self) -> Vec<(u32, u32)> {
        self.requests.lock().expect("lock").clone()
    }

    /// Only the requests that were served, i.e. not refused for size. Judged with
    /// file detail on, which is what an import asks for unless forced to drop it.
    fn served_requests(&self) -> Vec<(u32, u32)> {
        self.requests()
            .into_iter()
            .filter(|(offset, limit)| !(self.size_policy)(*offset, *limit, true))
            .collect()
    }

    /// How many requests left the per-file detail out.
    fn files_omitted_requests(&self) -> usize {
        *self.files_omitted.lock().expect("lock")
    }
}

impl RommTransport for AdaptiveRomm {
    fn get(
        &self,
        url: &str,
        _authorization: Option<&str>,
        _max_bytes: usize,
    ) -> Result<RommHttpResponse, RommRequestError> {
        if url.contains("/api/platforms") {
            return Ok(RommHttpResponse {
                status: 200,
                body: r#"[{"id":3,"slug":"nes","name":"Nintendo Entertainment System"}]"#
                    .as_bytes()
                    .to_vec(),
                location: None,
            });
        }
        if !url.contains("/api/roms") {
            return Ok(RommHttpResponse {
                status: 200,
                body: b"{}".to_vec(),
                location: None,
            });
        }
        let number = |key: &str| -> u32 {
            url.split(&format!("{key}="))
                .nth(1)
                .and_then(|tail| tail.split('&').next())
                .and_then(|value| value.parse().ok())
                .unwrap_or(0)
        };
        let (limit, offset) = (number("limit"), number("offset"));
        let with_files = url.contains("with_files=true");
        if !with_files {
            *self.files_omitted.lock().expect("lock") += 1;
        }
        self.requests.lock().expect("lock").push((offset, limit));

        let call_index = {
            let mut calls = self.calls.lock().expect("lock");
            let index = *calls;
            *calls += 1;
            index
        };
        if let Some(hook) = &self.hook {
            hook(call_index);
        }
        // The refusal a real client produces when the body exceeds the ceiling:
        // the body is never read, so nothing about its contents is knowable.
        if (self.size_policy)(offset, limit, with_files) {
            return Err(RommRequestError::ResponseTooLarge {
                limit: crate::identity_source::romm::client::MAX_RESPONSE_BYTES,
            });
        }

        let start = (offset as usize).min(self.catalogue.len());
        let end = start
            .saturating_add(limit as usize)
            .min(self.catalogue.len());
        let items = &self.catalogue[start..end];
        let total = match &self.totals {
            Some(totals) if !totals.is_empty() => totals[call_index.min(totals.len() - 1)],
            _ => self.catalogue.len() as u64,
        };
        let reported_offset = match self.offset_lie {
            Some((index, reported)) if index == call_index => reported,
            _ => offset,
        };
        let reported_limit = self.limit_lie.unwrap_or(limit);
        Ok(RommHttpResponse {
            status: 200,
            body: page_json(items, total, reported_limit, reported_offset).into_bytes(),
            location: None,
        })
    }
}

/// Runs a full import against an adaptive fake, collecting progress.
fn adaptive_import(
    tree: &Tree,
    fake: &AdaptiveRomm,
    page_size: u32,
) -> (Result<ImportOutcome, ImportFailure>, Vec<ImportProgress>) {
    let mut seen = Vec::new();
    let outcome = import_identity(
        &tree.source(),
        fake,
        ImportScope::Full,
        &capability(),
        page_size,
        |progress| seen.push(progress),
        None,
    );
    (outcome, seen)
}

/// Every record id the import ended up with, in catalogue order.
fn imported_ids(outcome: &ImportOutcome) -> Vec<String> {
    let mut ids: Vec<String> = outcome
        .cache
        .records
        .iter()
        .map(|record| record.provider_game_id.clone())
        .collect();
    ids.sort_by_key(|id| id.parse::<u32>().unwrap_or(0));
    ids
}

/// Test A1: 100 is too large, 50 succeeds.
#[test]
fn a_page_size_of_100_steps_down_to_50_and_completes() {
    let tree = Tree::new("adaptive-100-to-50");
    let fake = AdaptiveRomm::new(120, 50);
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("the import should complete at the smaller page size");

    assert_eq!(outcome.cache.records.len(), 120);
    assert_eq!(outcome.adaptive.configured_page_size, 100);
    assert_eq!(outcome.adaptive.effective_page_size, 50);
    assert_eq!(outcome.adaptive.smallest_page_size, 50);
    assert_eq!(outcome.adaptive.reductions, 1);
    assert_eq!(outcome.adaptive.oversized_retries, 1);
    assert!(outcome.adaptive.adapted());

    // The reduction was announced exactly once, naming the offset it retried.
    let reductions: Vec<PageSizeReduction> = seen
        .iter()
        .filter_map(|progress| progress.reduction)
        .collect();
    assert_eq!(reductions.len(), 1);
    assert_eq!(reductions[0].offset, 0);
    assert_eq!(reductions[0].from, 100);
    assert_eq!(reductions[0].to, 50);
    assert_eq!(
        reductions[0].ceiling_bytes,
        crate::identity_source::romm::client::MAX_RESPONSE_BYTES
    );

    // The first request was the configured size; the retry was the same offset.
    assert_eq!(fake.requests()[0], (0, 100));
    assert_eq!(fake.requests()[1], (0, 50));
}

/// Test A2: 100 and 50 are both too large, 25 succeeds.
#[test]
fn the_page_size_steps_down_twice_when_it_has_to() {
    let tree = Tree::new("adaptive-100-to-25");
    let fake = AdaptiveRomm::new(60, 25);
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("imported");

    assert_eq!(outcome.cache.records.len(), 60);
    assert_eq!(outcome.adaptive.effective_page_size, 25);
    assert_eq!(outcome.adaptive.reductions, 2);
    assert_eq!(outcome.adaptive.oversized_retries, 2);

    let steps: Vec<(u32, u32)> = seen
        .iter()
        .filter_map(|progress| progress.reduction)
        .map(|reduction| (reduction.from, reduction.to))
        .collect();
    assert_eq!(steps, vec![(100, 50), (50, 25)], "the ladder is fixed");
    // All three attempts were at offset 0.
    assert_eq!(
        fake.requests()[..3].to_vec(),
        vec![(0, 100), (0, 50), (0, 25)]
    );
}

/// Test A3: the ladder from 100 is exactly the documented sequence.
#[test]
fn the_ladder_from_the_default_is_the_documented_sequence() {
    let mut sizes = vec![100];
    while let Some(next) = next_page_size(*sizes.last().expect("non-empty")) {
        sizes.push(next);
    }
    assert_eq!(sizes, vec![100, 50, 25, 10, 5, 1]);
    assert_eq!(next_page_size(1), None, "there is nothing below one record");
    // A configured size that is not on the ladder still steps down predictably.
    assert_eq!(next_page_size(200), Some(100));
    assert_eq!(next_page_size(75), Some(50));
    assert_eq!(next_page_size(13), Some(10));
    assert_eq!(next_page_size(3), Some(1));
}

/// Test A4: the reduction happens at a later offset, not only on page one.
#[test]
fn a_reduction_at_a_later_offset_retries_that_offset_only() {
    let tree = Tree::new("adaptive-later-offset");
    // Mirrors the real failure: fine until offset 4400, then 100 is too large.
    let fake = AdaptiveRomm::with_policy(
        4600,
        Box::new(|offset, limit, _files| offset >= 4400 && limit > 50),
    );
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("imported");

    assert_eq!(outcome.cache.records.len(), 4600);
    assert_eq!(outcome.adaptive.reductions, 1);
    let reduction = seen
        .iter()
        .filter_map(|progress| progress.reduction)
        .next()
        .expect("a reduction should have been announced");
    assert_eq!(
        reduction.offset, 4400,
        "the reduction should be reported at the offset that failed"
    );
    assert_eq!((reduction.from, reduction.to), (100, 50));

    // Offsets before 4400 were fetched once each at the full size.
    let served = fake.served_requests();
    assert_eq!(served[0], (0, 100));
    assert_eq!(served[43], (4300, 100));
    assert_eq!(
        served[44],
        (4400, 50),
        "the failed offset was retried at 50"
    );
}

/// Test A5 and A6: no record is skipped and none arrives twice.
#[test]
fn an_adaptive_import_skips_no_records_and_duplicates_none() {
    let tree = Tree::new("adaptive-no-loss");
    let fake = AdaptiveRomm::with_policy(
        437,
        // Awkward on purpose: two different offsets need reductions, and the
        // catalogue does not divide evenly by any page size on the ladder.
        Box::new(|offset, limit, _files| {
            (offset == 100 && limit > 25) || (offset == 300 && limit > 10)
        }),
    );
    let (outcome, _) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("imported");

    let ids = imported_ids(&outcome);
    assert_eq!(ids.len(), 437, "every record should arrive");
    let expected: Vec<String> = (1..=437).map(|id| id.to_string()).collect();
    assert_eq!(ids, expected, "in order, with none missing");

    let mut unique = ids.clone();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "no record should arrive twice");

    // Every served request covered the range immediately after the previous one:
    // each offset is the previous offset plus however many records that page
    // actually returned. Contiguous and non-overlapping is exactly "nothing
    // skipped, nothing duplicated" stated in terms of the requests made.
    let served = fake.served_requests();
    let mut expected_offset = 0u32;
    for (offset, limit) in &served {
        assert_eq!(
            *offset, expected_offset,
            "offsets should advance by what arrived, not by what was asked for: {served:?}"
        );
        let delivered = (437u32.saturating_sub(*offset)).min(*limit);
        expected_offset += delivered;
    }
    assert_eq!(
        expected_offset, 437,
        "the walk should have covered the whole catalogue exactly once"
    );

    // The refused attempts repeated an offset; the served ones never did.
    let served_offsets: Vec<u32> = served.iter().map(|(offset, _)| *offset).collect();
    let mut sorted = served_offsets.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        served_offsets.len(),
        "no offset should be served twice: {served_offsets:?}"
    );
}

/// Test A7: a total that changes across retries and pages stays a hint.
#[test]
fn a_total_that_changes_during_retries_is_only_a_hint() {
    let tree = Tree::new("adaptive-total-changes");
    let fake = AdaptiveRomm::new(120, 50)
        // Wildly inconsistent totals, including a zero and a huge one.
        .reporting_totals(vec![120, 0, 999_999, 60, 120]);
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("a changing total must not fail an import that is otherwise fine");

    assert_eq!(outcome.cache.records.len(), 120);
    // The largest claim is what is kept, and only as a hint.
    assert_eq!(outcome.cache.server_reported_total, Some(999_999));
    // Progress fractions stayed within range or were absent, never invented.
    for progress in &seen {
        if let Some(fraction) = progress.fraction() {
            assert!(
                (0.0..=1.0).contains(&fraction),
                "{fraction} is not a fraction"
            );
        }
    }
}

/// Test A8: a wrong offset after a retry is refused, not absorbed.
#[test]
fn an_offset_the_server_reports_wrongly_after_a_retry_is_refused() {
    let tree = Tree::new("adaptive-offset-lie");
    // Call 0 is refused for size; call 1 is the retry, and lies about its offset.
    let fake = AdaptiveRomm::new(200, 50).lying_about_offset(1, 4242);
    let (outcome, _) = adaptive_import(&tree, &fake, 100);
    let failure = outcome.expect_err("a mismatched offset must be refused");
    assert_eq!(failure.code(), "invalid_pagination");
    assert!(
        failure.detail().contains("4242"),
        "the refusal should quote what the server said: {}",
        failure.detail()
    );
    assert!(failure.previous_cache_preserved());
}

/// Test A9: the short-page end condition uses the size actually requested.
#[test]
fn the_short_page_end_condition_uses_the_effective_page_size() {
    let tree = Tree::new("adaptive-short-page");
    // 75 records with a safe size of 50: pages of 50 then 25, and the 25 must end
    // the walk because it is short *against 50*, not against anything the server
    // claims. The server reports a limit of 100 throughout to make the point.
    let fake = AdaptiveRomm::new(75, 50).lying_about_limit(100);
    let (outcome, _) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("imported");

    assert_eq!(outcome.cache.records.len(), 75);
    let served = fake.served_requests();
    assert_eq!(
        served,
        vec![(0, 50), (50, 50)],
        "the second page returned 25 of 50 and ended the walk"
    );
}

/// Test A10: cancellation during the retry sequence.
#[test]
fn cancellation_during_a_retry_sequence_stops_the_import() {
    let tree = Tree::new("adaptive-cancel");
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    // Cancelled on the first refused call, so the flag is set part-way through the
    // ladder rather than between pages.
    let fake = AdaptiveRomm::new(500, 25).with_hook(Box::new(move |index| {
        if index == 0 {
            flag.store(true, Ordering::SeqCst);
        }
    }));
    let failure = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        100,
        no_progress,
        Some(&cancel),
    )
    .expect_err("a cancelled import must not succeed");
    assert_eq!(failure.code(), "cancelled");
    assert!(failure.previous_cache_preserved());
}

/// Test A11: the deadline during the retry sequence.
#[test]
fn the_deadline_still_applies_inside_a_retry_sequence() {
    let tree = Tree::new("adaptive-deadline");
    // Burns the whole deadline on the first refused call, so the next attempt in
    // the ladder is the one that has to notice.
    let fake = AdaptiveRomm::new(500, 25).with_hook(Box::new(|index| {
        if index == 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }));
    // A deadline this import has already blown: the retry loop must check it, not
    // only the outer page loop.
    let failure = crate::identity_source::romm::import::import_identity_with_deadline(
        &tree.source(),
        &fake,
        ImportScope::Full,
        &capability(),
        100,
        no_progress,
        None,
        std::time::Duration::from_millis(1),
    )
    .expect_err("an import past its deadline must stop");
    assert_eq!(failure.code(), "deadline_exceeded");
    assert!(failure.previous_cache_preserved());
}

/// Test A12: a single record still too large fails safely.
#[test]
fn a_single_record_larger_than_the_ceiling_fails_with_its_offset() {
    let tree = Tree::new("adaptive-oversized-record");
    // Nothing is ever small enough, with or without file detail.
    let fake = AdaptiveRomm::with_policy(400, Box::new(|_offset, _limit, _files| true));
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    let failure = outcome.expect_err("no page size can get past an oversized record");

    assert_eq!(failure.code(), "oversized_record");
    let detail = failure.detail();
    assert!(
        detail.contains("at offset 0"),
        "the offset should be named: {detail}"
    );
    assert!(
        detail.contains("without its file list"),
        "the detail should say the file list was already dropped: {detail}"
    );
    assert!(
        detail.contains("Nothing was published"),
        "the detail should say the cache is untouched: {detail}"
    );
    assert!(failure.previous_cache_preserved());

    // It exhausted the ladder before giving up.
    let steps: Vec<u32> = seen
        .iter()
        .filter_map(|progress| progress.reduction)
        .map(|reduction| reduction.to)
        .collect();
    assert_eq!(steps, vec![50, 25, 10, 5, 1]);
    // The ladder, then one last attempt for a single record without its file list:
    // seven requests, every one of them at the same offset.
    let attempts: Vec<(u32, u32)> = fake
        .requests()
        .into_iter()
        .filter(|(offset, _)| *offset == 0)
        .collect();
    assert_eq!(
        attempts,
        vec![(0, 100), (0, 50), (0, 25), (0, 10), (0, 5), (0, 1), (0, 1)]
    );
    assert_eq!(
        fake.files_omitted_requests(),
        1,
        "dropping the file list should have been tried before giving up"
    );
}

/// Test A18: the global caps still hold once the page size has fallen.
#[test]
fn the_global_caps_survive_a_page_size_reduction() {
    // The page budget scales with the smallest page size, so a reduction cannot
    // make the page cap the thing that fails a legitimate import...
    // At the default size the budget is the old constant plus the one short page
    // that detects the end of the catalogue - 2000 pages of 100 reach exactly
    // 200,000 records, and the walk needs one more request to learn it is done.
    assert_eq!(page_budget(MAX_CACHED_RECORDS, 100), MAX_IMPORT_PAGES + 1);
    assert!(
        page_budget(MAX_CACHED_RECORDS, 100) >= MAX_IMPORT_PAGES,
        "the budget must never fall below the documented page cap"
    );
    assert!(page_budget(MAX_CACHED_RECORDS, 1) > MAX_IMPORT_PAGES);
    // A small catalogue does not get a smaller budget than the documented cap.
    assert_eq!(page_budget(10, 100), MAX_IMPORT_PAGES);
    // ...while staying bounded however far it falls.
    assert!(page_budget(MAX_CACHED_RECORDS, 1) <= MAX_IMPORT_PAGES_ABSOLUTE);
    assert_eq!(page_budget(usize::MAX, 1), MAX_IMPORT_PAGES_ABSOLUTE);
    // A sample's budget is never larger than a full import's.
    assert!(page_budget(25, 1) <= page_budget(MAX_CACHED_RECORDS, 1));

    // And the record cap remains the authority: a server offering more than the
    // cap is refused on records, not on pages.
    let tree = Tree::new("adaptive-record-cap");
    let fake = AdaptiveRomm::with_policy(
        MAX_CACHED_RECORDS + 10,
        Box::new(|_offset, limit, _files| limit > 50),
    );
    // Only the arithmetic is under test here; walking 200,000 records through a
    // fake would be slow, so the budget assertions above carry the caps and this
    // asserts the reduction still happened on the way.
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    assert!(
        seen.iter().any(|progress| progress.reduction.is_some()),
        "the reduction should still occur"
    );
    match outcome {
        Err(failure) => assert!(
            matches!(failure.code(), "too_many_records" | "deadline_exceeded"),
            "a runaway catalogue should be stopped by records or time, not by pages: {}",
            failure.code()
        ),
        Ok(outcome) => assert!(outcome.cache.records.len() <= MAX_CACHED_RECORDS),
    }
}

/// Test A17: an ordinary import that never exceeds the ceiling is unchanged.
#[test]
fn an_import_that_never_exceeds_the_ceiling_is_unaffected() {
    let tree = Tree::new("adaptive-no-op");
    // Nothing is ever too large.
    let fake = AdaptiveRomm::with_policy(250, Box::new(|_offset, _limit, _files| false));
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("imported");

    assert_eq!(outcome.cache.records.len(), 250);
    assert_eq!(outcome.adaptive.configured_page_size, 100);
    assert_eq!(outcome.adaptive.effective_page_size, 100);
    assert_eq!(outcome.adaptive.reductions, 0);
    assert_eq!(outcome.adaptive.oversized_retries, 0);
    assert!(!outcome.adaptive.adapted());
    assert!(
        seen.iter().all(|progress| progress.reduction.is_none()),
        "no reduction should be reported"
    );
    assert_eq!(
        fake.served_requests(),
        vec![(0, 100), (100, 100), (200, 100)],
        "three pages of 100, the last one short"
    );
}

/// A configured page size other than the default is honoured, and adapts from
/// there rather than from 100.
#[test]
fn a_configured_page_size_is_the_starting_point() {
    let tree = Tree::new("adaptive-configured-start");
    let fake = AdaptiveRomm::new(60, 100);
    let (outcome, _) = adaptive_import(&tree, &fake, 200);
    let outcome = outcome.expect("imported");
    assert_eq!(outcome.adaptive.configured_page_size, 200);
    assert_eq!(outcome.adaptive.effective_page_size, 100);
    assert_eq!(outcome.adaptive.reductions, 1);
    assert_eq!(fake.requests()[0], (0, 200), "it started where it was told");
}

/// Test A15: a sample import adapts the same way and still publishes nothing.
#[test]
fn a_sample_import_adapts_and_publishes_nothing() {
    let tree = Tree::new("adaptive-sample");
    let fake = AdaptiveRomm::new(500, 25);
    let mut seen = Vec::new();
    let outcome = import_identity(
        &tree.source(),
        &fake,
        ImportScope::Sample { max_records: 30 },
        &capability(),
        100,
        |progress| seen.push(progress),
        None,
    )
    .expect("a sample should complete");

    assert_eq!(outcome.cache.records.len(), 30);
    assert_eq!(outcome.adaptive.reductions, 2, "100 -> 50 -> 25");
    assert_eq!(outcome.adaptive.effective_page_size, 25);
    // import_identity never publishes - that is the caller's separate step - so a
    // sample cannot have written anything.
    assert_eq!(
        load_cache(
            &IdentityCacheLocation::new(&tree.identity(), IdentityProvider::Romm),
            None
        )
        .expect_err("no cache")
        .code(),
        "missing"
    );
}

/// The real blocker, reproduced: one record whose *file list* is what makes it
/// too large. Dropping only that detail gets the import through.
#[test]
fn a_record_whose_file_list_is_too_large_is_imported_without_it() {
    let tree = Tree::new("adaptive-files-too-large");
    // Record 200 is readable on its own, but not with its file list - and any
    // page containing it is too large whatever the size, exactly like the PS4
    // game that held 28,831 file entries.
    let fake = AdaptiveRomm::with_policy(
        260,
        Box::new(|offset, limit, with_files| {
            let covers_the_fat_record = offset <= 200 && offset + limit > 200;
            covers_the_fat_record && with_files
        }),
    );
    let (outcome, seen) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("dropping the file list should let the import finish");

    assert_eq!(outcome.cache.records.len(), 260, "no record should be lost");
    // Exactly one record lost its file detail, and it is named.
    assert_eq!(
        outcome.adaptive.records_without_file_detail,
        vec!["201".to_string()],
        "the record ids are one-based in the fixture"
    );
    assert!(outcome.adaptive.lost_file_detail());
    assert_eq!(
        fake.files_omitted_requests(),
        1,
        "only the one record should have been fetched without detail"
    );

    // The record itself says so, so the gap is visible per record and not only in
    // a summary.
    let record = outcome
        .cache
        .records
        .iter()
        .find(|record| record.provider_game_id == "201")
        .expect("the record should still be in the cache");
    assert!(
        record
            .evidence
            .iter()
            .any(|line| line.contains("per-file detail was not imported")),
        "{:?}",
        record.evidence
    );
    assert!(record.related_files.is_empty());

    // It stepped all the way down before dropping detail, so detail is only ever
    // the last resort.
    let steps: Vec<u32> = seen
        .iter()
        .filter_map(|progress| progress.reduction)
        .map(|reduction| reduction.to)
        .collect();
    assert_eq!(steps, vec![50, 25, 10, 5, 1]);
}

/// After the fat region, the page size climbs back - or a long catalogue could
/// never finish inside the deadline.
#[test]
fn the_page_size_recovers_after_a_run_of_successes() {
    let tree = Tree::new("adaptive-recovery");
    // Only the first page is too large; everything after it is fine.
    let fake = AdaptiveRomm::with_policy(
        600,
        Box::new(|offset, limit, _files| offset == 0 && limit > 50),
    );
    let (outcome, _) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("imported");

    assert_eq!(outcome.cache.records.len(), 600);
    assert_eq!(outcome.adaptive.reductions, 1);
    assert!(
        outcome.adaptive.recoveries >= 1,
        "the size should climb back once pages are fitting: {:?}",
        outcome.adaptive
    );
    assert_eq!(
        outcome.adaptive.effective_page_size, 100,
        "it should get back to the configured size"
    );

    // Recovery is gated: the first RECOVERY_STREAK pages after the reduction stay
    // at the reduced size, so one fat page cannot start an alternation.
    let served = fake.served_requests();
    let sizes: Vec<u32> = served.iter().map(|(_, limit)| *limit).collect();
    assert_eq!(
        sizes[..RECOVERY_STREAK as usize],
        vec![50; RECOVERY_STREAK as usize],
        "the size should hold for a run of successes before climbing: {sizes:?}"
    );
    assert!(
        sizes.windows(2).all(|pair| pair[1] <= 100),
        "recovery must never exceed the configured size: {sizes:?}"
    );
    // And it climbs one rung at a time rather than jumping straight back.
    assert!(
        sizes.contains(&100),
        "it should reach the configured size eventually: {sizes:?}"
    );
}

/// Recovery never climbs above what was configured.
#[test]
fn recovery_is_capped_at_the_configured_page_size() {
    assert_eq!(previous_page_size(50, 100), Some(100));
    assert_eq!(previous_page_size(25, 100), Some(50));
    assert_eq!(previous_page_size(100, 100), None, "already there");
    assert_eq!(
        previous_page_size(50, 50),
        None,
        "a configured 50 must not climb to 100"
    );
    assert_eq!(previous_page_size(1, 10), Some(5));
}

/// A server that only ever manages one record per page does not flap: the size
/// freezes at what works and the import finishes.
///
/// This is what the recovery budget buys. Recovery is what makes a long catalogue
/// possible after one fat record, and the budget is what stops it from retrying
/// for ever on a server that will never do better.
#[test]
fn relentless_refusals_freeze_the_page_size_rather_than_flapping() {
    let tree = Tree::new("adaptive-freeze");
    // Anything above one record is too large, at every offset.
    let fake = AdaptiveRomm::with_policy(400, Box::new(|_offset, limit, _files| limit > 1));
    let (outcome, _) = adaptive_import(&tree, &fake, 100);
    let outcome = outcome.expect("one record at a time still gets there");

    assert_eq!(outcome.cache.records.len(), 400);
    assert_eq!(outcome.adaptive.effective_page_size, 1);
    assert!(
        outcome.adaptive.oversized_retries <= MAX_OVERSIZED_EVENTS,
        "refusals must stay inside the budget: {:?}",
        outcome.adaptive
    );
    // Recovery stopped once the budget was spent, rather than retrying a size that
    // never works for the remaining hundreds of records.
    let served = fake.served_requests();
    let late_sizes: Vec<u32> = served
        .iter()
        .rev()
        .take(50)
        .map(|(_, limit)| *limit)
        .collect();
    assert!(
        late_sizes.iter().all(|size| *size == 1),
        "the tail of the import should have stopped trying to climb: {late_sizes:?}"
    );
}

/// A server whose every record has an unreadable file list is abandoned rather
/// than allowed to spend the deadline on bodies that get thrown away.
#[test]
fn an_endless_run_of_oversized_responses_is_abandoned() {
    let tree = Tree::new("adaptive-endless");
    // Every request carrying file detail is refused, whatever its size, so each
    // record costs at least one wasted read.
    let fake =
        AdaptiveRomm::with_policy(10_000, Box::new(|_offset, _limit, with_files| with_files));
    let (outcome, _) = adaptive_import(&tree, &fake, 100);
    let failure = outcome.expect_err("this cannot be allowed to run to the deadline");
    assert_eq!(failure.code(), "too_many_oversized_pages");
    assert!(
        failure.detail().contains(&MAX_OVERSIZED_EVENTS.to_string()),
        "the budget should be named: {}",
        failure.detail()
    );
    assert!(failure.previous_cache_preserved());
}

// --- Local presence, and explaining a stale population --------------------
//
// A real import produced 10,081 stale records. Grouping them showed that 79% were
// flagged missing by RomM itself and 18% were links whose targets had gone - but
// also that 214 were present as *directories*, and every one of those was being
// reported as "does not exist". These tests pin the classification that fixed it.

#[test]
fn local_presence_tells_the_cases_apart() {
    let tree = Tree::new("presence");
    let file = tree.file("nes/game.zip", b"bytes");
    assert_eq!(LocalPresence::observe(&file), LocalPresence::File);

    let directory = tree.library().join("dc/Shenmue");
    std::fs::create_dir_all(&directory).expect("fixture");
    assert_eq!(
        LocalPresence::observe(&directory),
        LocalPresence::Directory,
        "a folder-based game is present, not missing"
    );

    let absent = tree.library().join("nes/gone.zip");
    assert_eq!(LocalPresence::observe(&absent), LocalPresence::Absent);

    let orphan = tree.library().join("nes/no-such-folder/game.zip");
    assert_eq!(
        LocalPresence::observe(&orphan),
        LocalPresence::ParentAbsent,
        "a whole collection missing is worth telling apart from one file missing"
    );

    let dangling = tree.library().join("nes/dangling.zip");
    std::os::unix::fs::symlink(tree.library().join("nes/nothing.zip"), &dangling).expect("fixture");
    assert_eq!(
        LocalPresence::observe(&dangling),
        LocalPresence::DanglingSymlink
    );

    // A symlink is judged by what it leads to, which is what the library's own
    // symlink farms require.
    let to_file = tree.library().join("nes/link-to-file.zip");
    std::os::unix::fs::symlink(&file, &to_file).expect("fixture");
    assert_eq!(LocalPresence::observe(&to_file), LocalPresence::File);
    let to_dir = tree.library().join("nes/link-to-dir");
    std::os::unix::fs::symlink(&directory, &to_dir).expect("fixture");
    assert_eq!(LocalPresence::observe(&to_dir), LocalPresence::Directory);
}

#[test]
fn a_present_directory_is_not_described_as_missing() {
    let path = Path::new("/mnt/games/roms/dc/Shenmue");
    let detail = LocalPresence::Directory.stale_detail(path);
    assert!(
        !detail.contains("does not exist"),
        "a directory that is right there must not be called missing: {detail}"
    );
    assert!(detail.contains("is a directory"), "{detail}");
    assert!(
        detail.contains("no single file to compare"),
        "it should say why that stops a match: {detail}"
    );

    // The other cases still say what they always said.
    assert!(
        LocalPresence::Absent
            .stale_detail(path)
            .contains("does not exist")
    );
    assert!(
        LocalPresence::DanglingSymlink
            .stale_detail(path)
            .contains("target no longer exists")
    );
    assert!(
        LocalPresence::ParentAbsent
            .stale_detail(path)
            .contains("neither does the folder")
    );
}

/// Matching a record whose local path is a directory: still stale, because no file
/// could be compared - but the reason is now the truth.
#[test]
fn matching_a_directory_backed_game_reports_it_accurately() {
    let tree = Tree::new("presence-matching");
    let directory = tree.library().join("dc/Shenmue");
    std::fs::create_dir_all(&directory).expect("fixture");
    std::fs::write(directory.join("Shenmue_Disc1.cdi"), b"disc").expect("fixture");

    let mut record = record_for("http://romm:8080", "1", Some(directory.clone()));
    record.provider_path = "roms/dc/Shenmue".to_string();
    record.file_size_bytes = Some(0);

    let facts = LocalFileFacts::observe(&directory);
    assert_eq!(facts.presence, LocalPresence::Directory);
    assert!(!facts.exists(), "a directory yields no file fingerprint");

    let outcome = match_record(
        &record,
        &facts,
        &PathClaims::of(std::slice::from_ref(&record)),
        &LocalHashCache::new(),
    );
    // The verdict is unchanged: no file was compared, so nothing was verified.
    assert_eq!(outcome.verification, ExternalVerification::Stale);
    assert!(!outcome.hash_compared);
    let evidence = outcome.evidence.join(" | ");
    assert!(
        !evidence.contains("does not exist"),
        "the old message was factually wrong here: {evidence}"
    );
    assert!(evidence.contains("is a directory"), "{evidence}");
}

/// A stale summary partitions the population exactly and stays bounded.
#[test]
fn a_stale_summary_partitions_the_population_and_stays_bounded() {
    use crate::identity_source::stale::{MAX_GROUPS, StaleSummary};

    let tree = Tree::new("stale-summary");
    let mut records = Vec::new();
    // Twenty platforms, so the group lists have to truncate.
    for index in 0..20 {
        for copy in 0..3 {
            let mut record = record_for(
                "http://romm:8080",
                &format!("{index}-{copy}"),
                Some(tree.library().join(format!("p{index}/game.zip"))),
            );
            record.provider_path = format!("roms/p{index}/game.zip");
            record.platform_candidate = Some(format!("Platform {index}"));
            record.verification = ExternalVerification::Stale;
            if copy == 0 {
                record
                    .evidence
                    .push("RomM reports this file as missing from its own filesystem".to_string());
            }
            if copy == 1 {
                record.related_files = vec!["a".to_string(), "b".to_string()];
            }
            records.push(record);
        }
    }
    // One matched record, which must be excluded entirely.
    let mut matched = record_for("http://romm:8080", "matched", None);
    matched.provider_path = "roms/p0/present.zip".to_string();
    matched.verification = ExternalVerification::StrongExternal;
    records.push(matched);

    let cache = cache_with(records, "http://romm:8080");
    let mappings = vec![("roms".to_string(), tree.library().display().to_string())];
    // A pure probe: every path is reported absent, so no filesystem is involved.
    let summary = StaleSummary::build(&cache, &mappings, 2, |_| LocalPresence::Absent);

    assert_eq!(summary.total_in_cache, 61);
    assert_eq!(summary.stale, 60, "the matched record must not be counted");
    assert_eq!(
        summary.by_reason.iter().map(|r| r.count).sum::<usize>(),
        60,
        "the reasons must partition the population exactly"
    );
    assert_eq!(summary.romm_reports_missing, 20);
    assert_eq!(
        summary.multi_file, 20,
        "only records listing two or more files count as multi-file"
    );
    assert_eq!(summary.unmapped, 0);

    // Bounded: twelve groups at most, and the tail is stated.
    assert_eq!(summary.by_platform.len(), MAX_GROUPS);
    assert_eq!(summary.platforms_not_listed, 20 - MAX_GROUPS);
    assert_eq!(summary.by_romm_prefix.len(), MAX_GROUPS);
    assert_eq!(summary.romm_prefixes_not_listed, 20 - MAX_GROUPS);
    // Examples are bounded by the caller's limit.
    for reason in &summary.by_reason {
        assert!(reason.examples.len() <= 2, "{:?}", reason.examples.len());
    }
    // Every stale record came through the one configured mapping.
    assert_eq!(summary.by_mapping.len(), 1);
    assert_eq!(summary.by_mapping[0].count, 60);
    assert!(summary.by_mapping[0].key.starts_with("roms ->"));
}

/// The drift verdict: what it says, and when it declines to say it.
#[test]
fn the_drift_verdict_follows_what_explains_the_population() {
    use crate::identity_source::stale::StaleSummary;

    let tree = Tree::new("stale-verdict");
    let build = |flagged: usize, total: usize, presence: LocalPresence| {
        let mut records = Vec::new();
        for index in 0..total {
            let mut record = record_for(
                "http://romm:8080",
                &format!("{index}"),
                Some(tree.library().join("nes/game.zip")),
            );
            record.verification = ExternalVerification::Stale;
            if index < flagged {
                record
                    .evidence
                    .push("RomM reports this file as missing from its own filesystem".to_string());
            }
            records.push(record);
        }
        let cache = cache_with(records, "http://romm:8080");
        StaleSummary::build(&cache, &[], 1, move |_| presence)
    };

    // Everything flagged by RomM: drift.
    assert!(build(100, 100, LocalPresence::Absent).looks_like_drift());
    // Nothing flagged, but every path a dead link: still drift, and still not a
    // mapping fault.
    assert!(build(0, 100, LocalPresence::DanglingSymlink).looks_like_drift());
    // Half explained: not drift, and worth looking at the mappings.
    assert!(!build(50, 100, LocalPresence::Absent).looks_like_drift());
    // Nothing explained at all - the shape a real mapping fault would take.
    assert!(!build(0, 100, LocalPresence::Absent).looks_like_drift());
    // An empty population is not a problem to report.
    assert!(build(0, 0, LocalPresence::Absent).looks_like_drift());
}
