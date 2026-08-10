//! Verification-store tests.
//!
//! The store's whole purpose is that verifying a file does not touch the identity
//! cache, so several of these assert what is *not* modified.

use super::*;
use crate::identity_source::hashing::{FileFingerprint, hash_file};
use crate::safe_read::TrustedRoots;
use std::fs;

const SERVER: &str = "http://172.19.0.20:8080";

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-verify-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("identity")).expect("fixture");
        fs::create_dir_all(root.join("library")).expect("fixture");
        Self { root }
    }

    fn identity(&self) -> PathBuf {
        self.root.join("identity")
    }

    fn library(&self) -> PathBuf {
        self.root.join("library")
    }

    fn store(&self) -> VerificationStore {
        VerificationStore::new(&self.identity(), IdentityProvider::Romm)
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.library().join(name);
        fs::write(&path, contents).expect("fixture");
        path
    }

    fn hash(&self, path: &Path) -> LocalHashes {
        let trusted = TrustedRoots::from_paths(&[self.library()]);
        hash_file(path, &trusted, None).expect("the fixture file should hash")
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn an_empty_store_loads_as_an_empty_cache() {
    let tree = Tree::new("empty");
    let store = tree.store();
    assert!(store.load().is_empty());
    assert_eq!(store.count(), 0);
    assert!(!store.path().exists(), "loading must not create anything");
}

#[test]
fn a_recorded_verification_survives_a_reload() {
    let tree = Tree::new("round-trip");
    let store = tree.store();
    let file = tree.file("game.gb", b"the real bytes");
    let hashes = tree.hash(&file);

    store.record(SERVER, hashes.clone()).expect("recorded");
    let reloaded = store.load();
    assert_eq!(reloaded.len(), 1);
    let stored = reloaded.get(&file).expect("still valid for this file");
    assert_eq!(stored.crc32, hashes.crc32);
    assert_eq!(stored.md5, hashes.md5);
    assert_eq!(stored.sha1, hashes.sha1);
    assert_eq!(stored.bytes_hashed, hashes.bytes_hashed);
}

#[test]
fn two_verifications_in_a_row_both_survive() {
    let tree = Tree::new("two");
    let store = tree.store();
    let first = tree.file("one.gb", b"first");
    let second = tree.file("two.gb", b"second");
    store.record(SERVER, tree.hash(&first)).expect("recorded");
    store.record(SERVER, tree.hash(&second)).expect("recorded");
    let loaded = store.load();
    assert_eq!(
        loaded.len(),
        2,
        "recording must not replace the whole store"
    );
    assert!(loaded.get(&first).is_some());
    assert!(loaded.get(&second).is_some());
}

#[test]
fn re_verifying_one_file_replaces_its_entry_rather_than_duplicating_it() {
    let tree = Tree::new("replace");
    let store = tree.store();
    let file = tree.file("game.gb", b"before");
    store.record(SERVER, tree.hash(&file)).expect("recorded");

    // The file is rebuilt, so its old hash no longer describes it.
    fs::write(&file, b"after the change").expect("fixture");
    let updated = tree.hash(&file);
    store.record(SERVER, updated.clone()).expect("recorded");

    let loaded = store.load();
    assert_eq!(loaded.len(), 1, "one entry per path");
    assert_eq!(loaded.get(&file).expect("valid").md5, updated.md5);
}

#[test]
fn a_hash_for_a_file_that_has_since_changed_is_dropped_on_load() {
    let tree = Tree::new("stale-entry");
    let store = tree.store();
    let file = tree.file("game.gb", b"original");
    store.record(SERVER, tree.hash(&file)).expect("recorded");
    assert_eq!(store.count(), 1);

    // Changing the file makes the stored hash describe something that is gone. A
    // stale hash must never be offered as current evidence.
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(&file, b"different contents entirely").expect("fixture");
    let loaded = store.load();
    assert!(
        loaded.get(&file).is_none(),
        "the fingerprint no longer matches, so the hash is not evidence"
    );
    assert_eq!(
        loaded.len(),
        0,
        "and it is pruned rather than kept as a trap"
    );
}

#[test]
fn a_hash_for_a_file_that_has_gone_is_dropped_on_load() {
    let tree = Tree::new("removed-file");
    let store = tree.store();
    let file = tree.file("game.gb", b"here for now");
    store.record(SERVER, tree.hash(&file)).expect("recorded");
    fs::remove_file(&file).expect("fixture");
    assert_eq!(store.load().len(), 0);
}

#[test]
fn the_store_is_published_atomically_and_leaves_no_temporary_file() {
    let tree = Tree::new("atomic");
    let store = tree.store();
    let file = tree.file("game.gb", b"bytes");
    store.record(SERVER, tree.hash(&file)).expect("recorded");
    assert!(store.path().is_file());
    let strays: Vec<String> = fs::read_dir(store.path().parent().expect("parent"))
        .expect("directory")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "left behind: {strays:?}");
}

#[cfg(unix)]
#[test]
fn the_store_is_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt as _;

    let tree = Tree::new("mode");
    let store = tree.store();
    let file = tree.file("game.gb", b"bytes");
    store.record(SERVER, tree.hash(&file)).expect("recorded");
    let mode = fs::metadata(store.path())
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "found {mode:o}");
}

#[test]
fn a_store_written_by_a_newer_version_is_discarded_rather_than_misread() {
    let tree = Tree::new("version");
    let store = tree.store();
    let file = tree.file("game.gb", b"bytes");
    store.record(SERVER, tree.hash(&file)).expect("recorded");

    let text = fs::read_to_string(store.path()).expect("readable");
    fs::write(
        store.path(),
        text.replace(
            &format!("\"format_version\": {VERIFICATION_FORMAT_VERSION}"),
            "\"format_version\": 99",
        ),
    )
    .expect("fixture");
    assert_eq!(
        store.load().len(),
        0,
        "a layout this build does not know is started again, since every entry is \
         recomputable"
    );
}

#[test]
fn a_corrupt_store_is_discarded_rather_than_failing() {
    let tree = Tree::new("corrupt");
    let store = tree.store();
    fs::create_dir_all(store.path().parent().expect("parent")).expect("fixture");
    fs::write(store.path(), b"{ this is not json").expect("fixture");
    // Verification is derived data: the safe response is an empty cache, not an
    // error that blocks the panel.
    assert!(store.load().is_empty());
}

#[test]
fn recording_a_verification_does_not_touch_the_identity_cache() {
    let tree = Tree::new("cache-untouched");
    let store = tree.store();
    // Something standing in for the identity cache, beside the store.
    let identity_cache = tree
        .identity()
        .join(IdentityProvider::Romm.slug())
        .join("identity-cache.json");
    fs::create_dir_all(identity_cache.parent().expect("parent")).expect("fixture");
    fs::write(&identity_cache, b"{\"records\":[]}").expect("fixture");
    let before = fs::read(&identity_cache).expect("readable");

    let file = tree.file("game.gb", b"bytes");
    store.record(SERVER, tree.hash(&file)).expect("recorded");

    assert_eq!(
        fs::read(&identity_cache).expect("still readable"),
        before,
        "verifying a file must not rewrite the identity cache"
    );
}

#[test]
fn recording_a_verification_does_not_touch_the_file_it_hashed() {
    let tree = Tree::new("file-untouched");
    let store = tree.store();
    let contents = b"exactly these bytes".to_vec();
    let file = tree.file("game.gb", &contents);
    let before = fs::metadata(&file).expect("metadata");

    store.record(SERVER, tree.hash(&file)).expect("recorded");

    let after = fs::metadata(&file).expect("metadata");
    assert_eq!(fs::read(&file).expect("readable"), contents);
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().ok(), after.modified().ok());
}

#[test]
fn the_store_is_bounded() {
    let tree = Tree::new("bounded");
    let store = tree.store();
    // More entries than the ceiling, built directly rather than by hashing that many
    // files.
    let mut hashes = LocalHashCache::new();
    for index in 0..(MAX_VERIFIED_ENTRIES + 50) {
        hashes.insert(LocalHashes {
            fingerprint: FileFingerprint {
                path: PathBuf::from(format!("/mnt/games/roms/gb/{index}.gb")),
                size_bytes: 1024,
                modified_unix_seconds: Some(1_785_000_000),
            },
            crc32: "00000000".to_string(),
            md5: "0".repeat(32),
            sha1: "0".repeat(40),
            bytes_hashed: 1024,
        });
    }
    store.save(SERVER, &hashes).expect("saved");
    let text = fs::read_to_string(store.path()).expect("readable");
    let record: VerificationRecord = serde_json::from_str(&text).expect("parses");
    assert!(
        record.hashes.len() <= MAX_VERIFIED_ENTRIES,
        "{} entries stored",
        record.hashes.len()
    );
}

#[test]
fn the_stored_form_carries_no_credential() {
    let tree = Tree::new("no-secret");
    let store = tree.store();
    let file = tree.file("game.gb", b"bytes");
    store.record(SERVER, tree.hash(&file)).expect("recorded");
    let text = fs::read_to_string(store.path()).expect("readable");
    assert!(!text.to_lowercase().contains("bearer"), "{text}");
    assert!(!text.to_lowercase().contains("token"), "{text}");
    // The server origin is recorded for provenance, as it is elsewhere.
    assert!(text.contains(SERVER));
}

#[test]
fn removing_the_store_reports_whether_there_was_one() {
    let tree = Tree::new("remove");
    let store = tree.store();
    assert!(!store.remove().expect("no error"), "nothing to remove");
    let file = tree.file("game.gb", b"bytes");
    store.record(SERVER, tree.hash(&file)).expect("recorded");
    assert!(store.remove().expect("removed"));
    assert!(store.load().is_empty());
}

/// The point of the whole store: a stored hash makes a record Confirmed, without
/// the identity cache having been written to.
#[test]
fn a_stored_hash_promotes_a_matching_record_to_confirmed() {
    use crate::identity_source::matching::{LocalFileFacts, PathClaims, match_record};
    use crate::identity_source::model::{
        ExternalHash, ExternalIdentityRecord, ExternalVerification, HashAlgorithm,
    };

    let tree = Tree::new("promotion");
    let store = tree.store();
    let contents = b"the bytes RomM described".to_vec();
    let file = tree.file("game.gb", &contents);
    let hashes = tree.hash(&file);

    let record = ExternalIdentityRecord {
        provider: IdentityProvider::Romm,
        server_id: SERVER.to_string(),
        provider_platform_id: Some("7".to_string()),
        provider_game_id: "1".to_string(),
        provider_file_id: None,
        provider_path: "roms/gb/game.gb".to_string(),
        archivefs_path: Some(file.clone()),
        title: Some("Game".to_string()),
        platform_candidate: Some("Game Boy".to_string()),
        provider_platform_name: Some("gb".to_string()),
        regions: Vec::new(),
        revision: None,
        // RomM published exactly what the file hashes to.
        hashes: vec![ExternalHash::parse(HashAlgorithm::Md5, &hashes.md5).expect("valid")],
        file_size_bytes: Some(contents.len() as u64),
        metadata_provider_ids: Vec::new(),
        artwork: None,
        related_files: Vec::new(),
        sibling_game_ids: Vec::new(),
        imported_at_unix_seconds: 1_785_000_000,
        provider_updated_at: None,
        verification: ExternalVerification::StrongExternal,
        conflicts: Vec::new(),
        evidence: Vec::new(),
    };

    // Without a stored hash nothing local has been compared, so the verdict rests on
    // the record alone. (It is Probable rather than Strong here only because a bare
    // fixture file gives EmuWiz no platform of its own to agree with.)
    let facts = LocalFileFacts::observe(&file);
    let claims = PathClaims::of(std::slice::from_ref(&record));
    let before = match_record(&record, &facts, &claims, &LocalHashCache::new());
    assert_eq!(before.verification, ExternalVerification::ProbableExternal);
    assert!(
        !before.hash_compared,
        "no hash may be reported as compared when none is stored"
    );

    // With one, the same record is Confirmed - and the promotion came from the
    // store, not from anything written into the cache.
    store.record(SERVER, hashes).expect("recorded");
    let after = match_record(&record, &facts, &claims, &store.load());
    assert_eq!(after.verification, ExternalVerification::ConfirmedExternal);
    assert!(after.hash_compared);
}

#[test]
fn a_stored_hash_that_disagrees_does_not_promote_and_keeps_both_sides() {
    use crate::identity_source::matching::{LocalFileFacts, PathClaims, match_record};
    use crate::identity_source::model::{
        ExternalHash, ExternalIdentityRecord, ExternalVerification, HashAlgorithm,
    };

    let tree = Tree::new("mismatch");
    let store = tree.store();
    let file = tree.file("game.gb", b"the local bytes");
    let hashes = tree.hash(&file);

    let mut record = ExternalIdentityRecord {
        provider: IdentityProvider::Romm,
        server_id: SERVER.to_string(),
        provider_platform_id: None,
        provider_game_id: "1".to_string(),
        provider_file_id: None,
        provider_path: "roms/gb/game.gb".to_string(),
        archivefs_path: Some(file.clone()),
        title: Some("Game".to_string()),
        platform_candidate: Some("Game Boy".to_string()),
        provider_platform_name: Some("gb".to_string()),
        regions: Vec::new(),
        revision: None,
        // A hash for entirely different bytes.
        hashes: vec![ExternalHash::parse(HashAlgorithm::Md5, &"b".repeat(32)).expect("valid")],
        file_size_bytes: None,
        metadata_provider_ids: Vec::new(),
        artwork: None,
        related_files: Vec::new(),
        sibling_game_ids: Vec::new(),
        imported_at_unix_seconds: 1_785_000_000,
        provider_updated_at: None,
        verification: ExternalVerification::StrongExternal,
        conflicts: Vec::new(),
        evidence: Vec::new(),
    };
    record.file_size_bytes = Some(std::fs::metadata(&file).expect("metadata").len());

    store.record(SERVER, hashes).expect("recorded");
    let facts = LocalFileFacts::observe(&file);
    let claims = PathClaims::of(std::slice::from_ref(&record));
    let outcome = match_record(&record, &facts, &claims, &store.load());

    assert_ne!(
        outcome.verification,
        ExternalVerification::ConfirmedExternal,
        "a disagreeing hash must never promote"
    );
    assert!(outcome.hash_compared, "but the comparison did happen");
    // Both values are retained, so the disagreement is inspectable.
    assert!(
        outcome
            .conflicts
            .iter()
            .any(|conflict| !conflict.external.is_empty() && !conflict.local.is_empty()),
        "{:?}",
        outcome.conflicts
    );
}
