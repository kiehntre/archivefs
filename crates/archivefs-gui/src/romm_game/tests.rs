//! Selected-game panel, verification and cover tests.
//!
//! Every claim the slice makes about meaning - Strong is never drawn as Confirmed,
//! two claimants produce Ambiguous, a manual platform assignment is not displaced,
//! a directory is not offered for hashing - is a property of a pure function here, so
//! it is settled as data rather than by looking at a screenshot.
//!
//! The local observation is injected, so almost no test touches a filesystem to decide
//! what is at a path - the exceptions are the two that need a fingerprint the hash
//! cache will accept, which is only true of a file that really exists.

use super::*;
use archivefs_core::identity_source::cache::CACHE_FORMAT_VERSION;
use archivefs_core::identity_source::hashing::FileFingerprint;
use archivefs_core::identity_source::model::{
    ArtworkReference, ExternalHash, HashAlgorithm, IdentityProvider,
};
use std::collections::HashMap;

const SERVER: &str = "http://172.19.0.20:8080";
const LOCAL: &str = "/mnt/games/roms/gb/game.gb";
const MD5: &str = "0123456789abcdef0123456789abcdef";
const SHA1: &str = "0123456789abcdef0123456789abcdef01234567";
const CRC: &str = "deadbeef";

// --- Fixtures -------------------------------------------------------------

fn record(id: &str, title: &str) -> ExternalIdentityRecord {
    ExternalIdentityRecord {
        provider: IdentityProvider::Romm,
        server_id: SERVER.to_string(),
        provider_platform_id: Some("7".to_string()),
        provider_game_id: id.to_string(),
        provider_file_id: None,
        provider_path: "roms/gb/game.gb".to_string(),
        archivefs_path: Some(PathBuf::from(LOCAL)),
        title: Some(title.to_string()),
        platform_candidate: Some("Game Boy".to_string()),
        provider_platform_name: Some("gb".to_string()),
        regions: vec!["USA".to_string()],
        revision: None,
        hashes: vec![ExternalHash::parse(HashAlgorithm::Md5, MD5).expect("hash")],
        file_size_bytes: Some(131_072),
        metadata_provider_ids: Vec::new(),
        artwork: None,
        related_files: Vec::new(),
        sibling_game_ids: Vec::new(),
        imported_at_unix_seconds: 1_785_595_944,
        provider_updated_at: None,
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

/// An observer that answers from a table, so nothing reads the disk.
///
/// A fingerprint is produced only for a regular file, exactly as `observe` does - which
/// is what makes "a folder is present but has no size to compare" reproducible here.
fn probe(answers: HashMap<PathBuf, LocalPresence>) -> impl Fn(&Path) -> LocalFileFacts {
    move |path: &Path| {
        let presence = answers.get(path).copied().unwrap_or(LocalPresence::Absent);
        LocalFileFacts {
            fingerprint: (presence == LocalPresence::File).then(|| FileFingerprint {
                path: path.to_path_buf(),
                size_bytes: 131_072,
                modified_unix_seconds: Some(1_785_000_000),
            }),
            presence,
            local_platform: None,
            local_strength: archivefs_core::identity_source::model::LocalEvidenceStrength::None,
        }
    }
}

fn present() -> impl Fn(&Path) -> LocalFileFacts {
    probe(HashMap::from([(PathBuf::from(LOCAL), LocalPresence::File)]))
}

/// A real file on disk, for the few tests that need a fingerprint the hash cache will
/// actually accept - `LocalHashCache::get` re-checks the file, by design, so a stored
/// hash for a path that does not exist is correctly treated as no evidence at all.
struct RealFile {
    root: PathBuf,
    path: PathBuf,
}

impl RealFile {
    fn new(label: &str, contents: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-romm-game-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).expect("fixture");
        let path = root.join("game.gb");
        std::fs::write(&path, contents).expect("fixture");
        Self { root, path }
    }

    /// The same observation production makes, on a file that really is there.
    fn facts(&self) -> impl Fn(&Path) -> LocalFileFacts + use<> {
        |path: &Path| LocalFileFacts::observe(path)
    }

    fn stored(&self, md5: &str) -> LocalHashCache {
        let mut cache = LocalHashCache::new();
        cache.insert(LocalHashes {
            fingerprint: FileFingerprint::observe(&self.path).expect("fixture"),
            crc32: CRC.to_string(),
            md5: md5.to_string(),
            sha1: SHA1.to_string(),
            bytes_hashed: std::fs::metadata(&self.path).expect("fixture").len(),
        });
        cache
    }

    fn record(&self, id: &str, title: &str) -> ExternalIdentityRecord {
        let mut built = record(id, title);
        built.archivefs_path = Some(self.path.clone());
        built.file_size_bytes = Some(std::fs::metadata(&self.path).expect("fixture").len());
        built
    }
}

impl Drop for RealFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn hashes_for(path: &str, md5: &str) -> LocalHashCache {
    let mut cache = LocalHashCache::new();
    cache.insert(LocalHashes {
        fingerprint: FileFingerprint {
            path: PathBuf::from(path),
            size_bytes: 131_072,
            modified_unix_seconds: Some(1_785_000_000),
        },
        crc32: CRC.to_string(),
        md5: md5.to_string(),
        sha1: SHA1.to_string(),
        bytes_hashed: 131_072,
    });
    cache
}

fn resolve(
    records: Vec<ExternalIdentityRecord>,
    verified: &LocalHashCache,
    platform: &LocalPlatformClaim,
    chosen: Option<&str>,
) -> GameIdentityPanel {
    let cache = cache(records);
    resolve_selected_game(
        &cache,
        Path::new(LOCAL),
        verified,
        platform,
        chosen,
        &present(),
    )
}

// --- Nothing matched ------------------------------------------------------

#[test]
fn a_file_no_record_maps_to_is_unmatched_and_says_so_plainly() {
    let mut other = record("1", "Somewhere Else");
    other.archivefs_path = Some(PathBuf::from("/mnt/games/roms/gb/other.gb"));
    let panel = resolve(
        vec![other],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    assert_eq!(panel.verdict, ExternalVerification::Unmatched);
    assert!(panel.candidates.is_empty());
    assert_eq!(panel.claimants, 0);
    assert!(
        panel
            .summary
            .contains("No record in the imported RomM catalogue"),
        "{}",
        panel.summary
    );
}

#[test]
fn an_empty_catalogue_produces_a_panel_rather_than_a_failure() {
    let panel = resolve(
        Vec::new(),
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    assert_eq!(panel.verdict, ExternalVerification::Unmatched);
    assert!(panel.verify_blocker.is_some(), "nothing to verify against");
}

// --- The six verdicts keep their meanings --------------------------------

#[test]
fn a_record_with_a_published_hash_and_no_local_hash_is_strong_not_confirmed() {
    let panel = resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim {
            platform: Some("Game Boy".to_string()),
            manual: false,
        },
        None,
    );
    assert_eq!(
        panel.verdict,
        ExternalVerification::StrongExternal,
        "RomM published hashes but the file has not been read"
    );
    assert_eq!(verdict_label(panel.verdict), "Strong");
    assert_ne!(verdict_label(panel.verdict), "Confirmed");
    assert!(panel.stored_hashes.is_none());
}

#[test]
fn a_stored_agreeing_hash_is_what_makes_a_record_confirmed() {
    let file = RealFile::new("agree", b"the bytes RomM described");
    let cached = cache(vec![file.record("1", "Game")]);
    let panel = resolve_selected_game(
        &cached,
        &file.path,
        &file.stored(MD5),
        &LocalPlatformClaim {
            platform: Some("Game Boy".to_string()),
            manual: false,
        },
        None,
        &file.facts(),
    );
    assert_eq!(panel.verdict, ExternalVerification::ConfirmedExternal);
    assert!(panel.stored_hashes.is_some(), "and it is shown");
    assert!(
        panel
            .chosen_candidate()
            .expect("one candidate")
            .hash_compared
    );
}

#[test]
fn a_stored_disagreeing_hash_never_produces_confirmed() {
    let file = RealFile::new("disagree", b"different bytes entirely");
    let cached = cache(vec![file.record("1", "Game")]);
    let panel = resolve_selected_game(
        &cached,
        &file.path,
        &file.stored(&"b".repeat(32)),
        &LocalPlatformClaim::default(),
        None,
        &file.facts(),
    );
    assert_ne!(panel.verdict, ExternalVerification::ConfirmedExternal);
    assert_eq!(panel.verdict, ExternalVerification::Ambiguous);
    let candidate = panel.chosen_candidate().expect("one candidate");
    assert!(
        candidate
            .conflicts
            .iter()
            .any(|conflict| conflict.field == "Hash"),
        "{:?}",
        candidate.conflicts
    );
}

#[test]
fn a_stored_hash_for_a_path_with_no_file_is_not_treated_as_evidence() {
    // The hash cache re-checks the file before offering an entry, so a fingerprint
    // that no longer describes anything is correctly no evidence at all.
    let panel = resolve(
        vec![record("1", "Game")],
        &hashes_for("/mnt/games/roms/gb/vanished.gb", MD5),
        &LocalPlatformClaim::default(),
        None,
    );
    assert!(panel.stored_hashes.is_none());
    assert_ne!(panel.verdict, ExternalVerification::ConfirmedExternal);
}

#[test]
fn a_record_with_no_comparable_size_or_platform_is_probable() {
    let mut only_title = record("1", "Game");
    only_title.file_size_bytes = None;
    only_title.platform_candidate = None;
    only_title.hashes = Vec::new();
    let panel = resolve(
        vec![only_title],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    assert_eq!(panel.verdict, ExternalVerification::ProbableExternal);
}

#[test]
fn a_mapped_path_with_no_file_is_stale_and_not_offered_for_hashing() {
    let cache = cache(vec![record("1", "Game")]);
    let panel = resolve_selected_game(
        &cache,
        Path::new(LOCAL),
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
        &probe(HashMap::from([(
            PathBuf::from(LOCAL),
            LocalPresence::Absent,
        )])),
    );
    assert_eq!(panel.verdict, ExternalVerification::Stale);
    assert_eq!(panel.presence, LocalPresence::Absent);
    assert!(panel.verify_blocker.is_some());
}

#[test]
fn each_verdict_carries_its_own_wording_rather_than_a_shared_one() {
    let mut seen: Vec<&str> = Vec::new();
    for verdict in crate::romm_browse::ALL_VERDICTS {
        let explanation = verdict_explanation(verdict);
        assert!(!explanation.is_empty());
        assert!(
            !seen.contains(&explanation),
            "{verdict:?} reuses another verdict's explanation"
        );
        seen.push(explanation);
    }
}

// --- A present directory is never called nonexistent ---------------------

#[test]
fn a_folder_based_game_is_reported_as_a_present_directory() {
    let cache = cache(vec![record("1", "Shenmue")]);
    let panel = resolve_selected_game(
        &cache,
        Path::new(LOCAL),
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
        &probe(HashMap::from([(
            PathBuf::from(LOCAL),
            LocalPresence::Directory,
        )])),
    );
    assert_eq!(presence_label(panel.presence), "Present directory");
    let blocker = panel.verify_blocker.expect("a folder cannot be hashed");
    assert!(blocker.contains("folder"), "{blocker}");
    assert!(
        !blocker.contains("does not exist"),
        "a present folder is not missing: {blocker}"
    );
}

#[test]
fn a_dangling_symlink_is_refused_for_what_it_is() {
    let cache = cache(vec![record("1", "Game")]);
    let panel = resolve_selected_game(
        &cache,
        Path::new(LOCAL),
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
        &probe(HashMap::from([(
            PathBuf::from(LOCAL),
            LocalPresence::DanglingSymlink,
        )])),
    );
    let blocker = panel.verify_blocker.expect("refused");
    assert!(blocker.contains("symbolic link"), "{blocker}");
}

#[test]
fn a_device_or_socket_is_refused_as_not_a_regular_file() {
    let cache = cache(vec![record("1", "Game")]);
    let panel = resolve_selected_game(
        &cache,
        Path::new(LOCAL),
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
        &probe(HashMap::from([(
            PathBuf::from(LOCAL),
            LocalPresence::Other,
        )])),
    );
    let blocker = panel.verify_blocker.expect("refused");
    assert!(blocker.contains("not a regular file"), "{blocker}");
}

#[test]
fn a_record_with_no_published_hash_cannot_be_verified_and_says_why() {
    let mut no_hashes = record("1", "Game");
    no_hashes.hashes = Vec::new();
    let panel = resolve(
        vec![no_hashes],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    let blocker = panel.verify_blocker.expect("refused");
    assert!(blocker.contains("no hash"), "{blocker}");
}

// --- Ambiguity -----------------------------------------------------------

#[test]
fn two_records_claiming_one_file_are_ambiguous_and_neither_is_picked() {
    let panel = resolve(
        vec![record("1", "First Claim"), record("2", "Second Claim")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    assert_eq!(panel.verdict, ExternalVerification::Ambiguous);
    assert!(panel.is_ambiguous());
    assert_eq!(panel.claimants, 2);
    assert_eq!(panel.candidates.len(), 2, "both are kept");
    assert!(
        panel.chosen.is_none(),
        "nothing is selected until someone chooses"
    );
    assert!(
        panel.summary.contains("2 RomM records"),
        "{}",
        panel.summary
    );
}

#[test]
fn choosing_a_claimant_shows_its_evidence_without_promoting_it() {
    let panel = resolve(
        vec![record("1", "First Claim"), record("2", "Second Claim")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        Some("2"),
    );
    let chosen = panel.chosen_candidate().expect("chosen");
    assert_eq!(chosen.romm_game_id, "2");
    // Still not Confirmed: choosing is not evidence about the bytes.
    assert_ne!(panel.verdict, ExternalVerification::ConfirmedExternal);
    // And the contest itself is still recorded on the candidate.
    assert!(
        chosen
            .conflicts
            .iter()
            .any(|conflict| conflict.field == "File state"),
        "{:?}",
        chosen.conflicts
    );
}

#[test]
fn every_claimant_keeps_its_own_evidence_rather_than_being_merged() {
    let mut second = record("2", "Second Claim");
    second.platform_candidate = Some("SNES".to_string());
    second.provider_path = "roms/snes/game.sfc".to_string();
    let panel = resolve(
        vec![record("1", "First Claim"), second],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    let paths: Vec<&str> = panel
        .candidates
        .iter()
        .map(|candidate| candidate.romm_path.as_str())
        .collect();
    assert!(paths.contains(&"roms/gb/game.gb"), "{paths:?}");
    assert!(paths.contains(&"roms/snes/game.sfc"), "{paths:?}");
}

#[test]
fn the_claimant_list_is_bounded_and_the_full_count_is_still_reported() {
    let records: Vec<ExternalIdentityRecord> = (1..=MAX_CANDIDATES + 5)
        .map(|index| record(&index.to_string(), &format!("Claim {index}")))
        .collect();
    let panel = resolve(
        records,
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    assert_eq!(panel.candidates.len(), MAX_CANDIDATES);
    assert_eq!(panel.claimants, MAX_CANDIDATES + 5);
    assert_eq!(panel.claimants_not_listed, 5);
}

#[test]
fn contested_candidates_are_all_ambiguous_for_the_same_reason() {
    // Not "the strongest wins": a contested path makes every claimant Ambiguous,
    // because the contest is itself the reason none of them can be relied on.
    let panel = resolve(
        vec![record("1", "Zeta"), record("2", "Alpha")],
        &LocalHashCache::new(),
        &LocalPlatformClaim {
            platform: Some("Game Boy".to_string()),
            manual: false,
        },
        None,
    );
    assert_eq!(panel.candidates.len(), 2);
    assert!(
        panel
            .candidates
            .iter()
            .all(|candidate| candidate.verdict == ExternalVerification::Ambiguous)
    );
}

#[test]
fn candidates_are_ordered_the_same_way_every_time() {
    let panel = resolve(
        vec![
            record("9", "Zeta"),
            record("2", "Alpha"),
            record("1", "Alpha"),
        ],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    let order: Vec<&str> = panel
        .candidates
        .iter()
        .map(|candidate| candidate.romm_game_id.as_str())
        .collect();
    assert_eq!(order, vec!["1", "2", "9"], "title then RomM id");
}

#[test]
fn a_single_claimant_needs_no_choice() {
    let panel = resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    assert!(!panel.is_ambiguous());
    assert_eq!(panel.chosen, Some(0));
}

// --- A manual platform assignment is never displaced ---------------------

#[test]
fn a_manual_platform_that_romm_disagrees_with_produces_ambiguous_not_a_correction() {
    let mut romm = record("1", "Game");
    romm.platform_candidate = Some("Game Boy".to_string());
    let panel = resolve(
        vec![romm],
        &LocalHashCache::new(),
        &LocalPlatformClaim {
            platform: Some("Game Boy Color".to_string()),
            manual: true,
        },
        None,
    );
    assert_eq!(panel.verdict, ExternalVerification::Ambiguous);
    assert!(panel.manual_platform);
    let candidate = panel.chosen_candidate().expect("one");
    assert!(
        candidate
            .evidence
            .iter()
            .any(|line| line.contains("verified a different platform")),
        "{:?}",
        candidate.evidence
    );
}

#[test]
fn a_manual_assignment_that_agrees_is_recorded_as_leading() {
    let panel = resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim {
            platform: Some("Game Boy".to_string()),
            manual: true,
        },
        None,
    );
    assert_eq!(panel.verdict, ExternalVerification::StrongExternal);
    let candidate = panel.chosen_candidate().expect("one");
    assert!(
        candidate
            .evidence
            .iter()
            .any(|line| line.contains("is not displaced by this record")),
        "{:?}",
        candidate.evidence
    );
}

#[test]
fn a_manual_assignment_is_named_as_such_in_the_panel_rows() {
    let panel = resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim {
            platform: Some("Game Boy".to_string()),
            manual: true,
        },
        None,
    );
    assert_eq!(
        panel.local_platform.as_deref(),
        Some("Game Boy, assigned by hand")
    );
}

#[test]
fn an_automatic_platform_is_distinguished_from_a_manual_one() {
    let claim = LocalPlatformClaim {
        platform: Some("Game Boy".to_string()),
        manual: false,
    };
    assert_eq!(
        claim.strength(),
        archivefs_core::identity_source::model::LocalEvidenceStrength::Weak
    );
    let manual = LocalPlatformClaim {
        platform: Some("Game Boy".to_string()),
        manual: true,
    };
    assert_eq!(
        manual.strength(),
        archivefs_core::identity_source::model::LocalEvidenceStrength::Verified
    );
    assert_eq!(
        LocalPlatformClaim::default().strength(),
        archivefs_core::identity_source::model::LocalEvidenceStrength::None
    );
}

// --- The panel is bound to what produced it -----------------------------

#[test]
fn a_panel_for_a_different_file_is_discarded() {
    let panel = resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new("/mnt/games/roms/gb/other.gb")));
    assert!(
        !state.accepts_panel(&panel),
        "a panel about another file must not be drawn here"
    );
    state.focus(Some(Path::new(LOCAL)));
    assert!(state.accepts_panel(&panel));
}

#[test]
fn a_panel_records_the_cache_it_came_from() {
    let panel = resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    assert_eq!(panel.cache.server_id, SERVER);
    assert_eq!(panel.cache.records, 1);
    assert_eq!(panel.cache.format_version, CACHE_FORMAT_VERSION);
}

#[test]
fn moving_the_selection_discards_the_previous_games_panel_cover_and_verification() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    )));
    state.chosen_game_id = Some("1".to_string());
    state.cover = CoverState::Unavailable(ArtworkAvailability::None);
    state.cover_key = Some("a-key".to_string());
    state.cover_cache = Some((3, 300));
    state.dismissed = true;

    assert!(state.focus(Some(Path::new("/mnt/games/roms/gb/other.gb"))));
    assert!(state.panel.is_none());
    assert!(state.chosen_game_id.is_none());
    assert!(state.verification.is_none());
    assert_eq!(state.cover, CoverState::Idle);
    assert!(state.cover_key.is_none());
    assert!(state.cover_cache.is_none());
    assert!(!state.dismissed, "closing one game does not hide the next");
}

#[test]
fn re_focusing_the_same_file_changes_nothing() {
    let mut state = GamePanelState::default();
    assert!(state.focus(Some(Path::new(LOCAL))));
    state.chosen_game_id = Some("1".to_string());
    assert!(!state.focus(Some(Path::new(LOCAL))), "no change reported");
    assert_eq!(state.chosen_game_id.as_deref(), Some("1"));
}

#[test]
fn a_cover_for_a_record_that_is_no_longer_chosen_is_discarded() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    )));
    let outcome = CoverOutcome {
        local_path: PathBuf::from(LOCAL),
        romm_game_id: "1".to_string(),
        state: CoverState::Unavailable(ArtworkAvailability::None),
        cached_items: 0,
        cached_bytes: 0,
    };
    assert!(state.accepts_cover(&outcome));
    let other = CoverOutcome {
        romm_game_id: "999".to_string(),
        ..outcome.clone()
    };
    assert!(
        !state.accepts_cover(&other),
        "a cover for another record must not be drawn beside this one"
    );
    let elsewhere = CoverOutcome {
        local_path: PathBuf::from("/mnt/games/roms/gb/other.gb"),
        ..outcome
    };
    assert!(!state.accepts_cover(&elsewhere));
}

// --- Covers --------------------------------------------------------------

#[test]
fn a_record_with_only_a_public_scraper_link_is_never_fetchable() {
    let mut public_only = record("1", "Game");
    public_only.artwork = Some(ArtworkReference {
        reference: "https://images.igdb.com/cover.jpg".to_string(),
        small_reference: None,
    });
    assert_eq!(
        availability_of(&public_only),
        ArtworkAvailability::PublicOnly
    );
    let explanation = ArtworkAvailability::PublicOnly
        .explanation()
        .expect("explained");
    assert!(explanation.contains("IGDB"), "{explanation}");
    assert!(explanation.contains("RomM server only"), "{explanation}");
}

#[test]
fn a_record_with_romms_own_small_cover_is_fetchable() {
    let mut fetchable = record("1", "Game");
    fetchable.artwork = Some(ArtworkReference {
        reference: "https://images.igdb.com/cover.jpg".to_string(),
        small_reference: Some("assets/romm/resources/small.png".to_string()),
    });
    assert_eq!(availability_of(&fetchable), ArtworkAvailability::Fetchable);
    assert!(
        ArtworkAvailability::Fetchable.explanation().is_none(),
        "nothing to explain when it can be shown"
    );
}

#[test]
fn a_record_with_no_artwork_at_all_says_so() {
    assert_eq!(
        availability_of(&record("1", "Game")),
        ArtworkAvailability::None
    );
    assert!(
        ArtworkAvailability::None
            .explanation()
            .expect("explained")
            .contains("no cover")
    );
}

#[test]
fn every_cover_state_says_what_it_means_in_words() {
    let states = [
        CoverState::Idle,
        CoverState::Loading,
        CoverState::Unavailable(ArtworkAvailability::None),
        CoverState::Refused("the response was not an image".to_string()),
        CoverState::Cancelled,
    ];
    for state in states {
        assert!(!state.line().is_empty(), "{state:?} says nothing");
    }
    assert!(CoverState::Cancelled.line().contains("Nothing was cached"));
}

#[test]
fn a_cover_read_from_the_cache_says_no_request_was_made() {
    let cover = CoverImage {
        key: "abc".to_string(),
        width: 162,
        height: 216,
        bytes: 55_000,
        image: egui::ColorImage::new([1, 1], vec![egui::Color32::BLACK]),
        from_cache: true,
    };
    let line = CoverState::Ready(Box::new(cover)).line();
    assert!(line.contains("162x216"), "{line}");
    assert!(line.contains("no request was made"), "{line}");
}

#[test]
fn a_thumbnail_never_exceeds_the_declared_box() {
    // The panel draws at most this, whatever arrives.
    assert_eq!(THUMBNAIL_MAX_WIDTH, 200);
    assert_eq!(THUMBNAIL_MAX_HEIGHT, 280);
    const { assert!(MAX_THUMBNAIL_READ_BYTES <= 2 * 1024 * 1024) };
}

#[test]
fn two_covers_with_the_same_key_compare_equal_without_comparing_pixels() {
    let make = |pixel: egui::Color32| CoverImage {
        key: "same".to_string(),
        width: 1,
        height: 1,
        bytes: 10,
        image: egui::ColorImage::new([1, 1], vec![pixel]),
        from_cache: false,
    };
    assert_eq!(make(egui::Color32::BLACK), make(egui::Color32::WHITE));
}

// --- Verification results -----------------------------------------------

fn comparison(algorithm: &str, agrees: bool) -> HashComparisonView {
    HashComparisonView {
        algorithm: algorithm.to_string(),
        romm: MD5.to_string(),
        local: if agrees {
            MD5.to_string()
        } else {
            "b".repeat(32)
        },
        agrees,
    }
}

fn outcome(
    comparisons: Vec<HashComparisonView>,
    after: ExternalVerification,
) -> VerificationOutcomeView {
    let all_agree = !comparisons.is_empty() && comparisons.iter().all(|line| line.agrees);
    let any_disagree = comparisons.iter().any(|line| !line.agrees);
    VerificationOutcomeView {
        local_path: PathBuf::from(LOCAL),
        file_label: "game.gb".to_string(),
        compact_label: "Game (Game Boy)".to_string(),
        romm_game_id: "1".to_string(),
        comparisons,
        all_agree,
        any_disagree,
        verdict_before: ExternalVerification::StrongExternal,
        verdict_after: after,
        bytes_hashed: 131_072,
        elapsed_seconds: 2,
        stored_at: Some(PathBuf::from(
            "/home/user/.local/share/archivefs/identity/romm/verified-hashes.json",
        )),
        panel: Box::new(resolve(
            vec![record("1", "Game")],
            &LocalHashCache::new(),
            &LocalPlatformClaim::default(),
            None,
        )),
    }
}

#[test]
fn a_verification_where_every_hash_agreed_reports_confirmed() {
    let result = outcome(
        vec![comparison("MD5", true), comparison("SHA-1", true)],
        ExternalVerification::ConfirmedExternal,
    );
    assert!(result.all_agree);
    assert!(!result.any_disagree);
    assert!(result.promoted());
    assert!(
        result.conclusion().contains("Confirmed"),
        "{}",
        result.conclusion()
    );
}

#[test]
fn one_agreeing_and_one_disagreeing_hash_is_called_inconsistent_metadata() {
    let result = outcome(
        vec![comparison("MD5", true), comparison("SHA-1", false)],
        ExternalVerification::Ambiguous,
    );
    assert!(!result.all_agree, "one agreement is not agreement");
    assert!(result.any_disagree);
    let conclusion = result.conclusion();
    assert!(conclusion.contains("inconsistent"), "{conclusion}");
    assert!(!conclusion.contains("different dump from"), "{conclusion}");
}

#[test]
fn no_agreeing_hash_at_all_is_called_a_different_dump() {
    let result = outcome(
        vec![comparison("MD5", false)],
        ExternalVerification::Ambiguous,
    );
    let conclusion = result.conclusion();
    assert!(conclusion.contains("different dump"), "{conclusion}");
    assert!(conclusion.contains("nothing was changed"), "{conclusion}");
    assert!(!result.promoted());
}

#[test]
fn hashing_a_file_romm_published_no_hash_for_confirms_nothing() {
    let result = outcome(Vec::new(), ExternalVerification::StrongExternal);
    assert!(!result.all_agree);
    assert!(result.conclusion().contains("nothing was confirmed"));
    assert!(!result.promoted());
}

#[test]
fn agreement_alone_does_not_claim_promotion_when_the_verdict_did_not_move() {
    // The hashes agreed, but the record stayed Ambiguous - a platform disagreement,
    // say. Reporting Confirmed here would be a lie.
    let result = outcome(
        vec![comparison("MD5", true)],
        ExternalVerification::Ambiguous,
    );
    assert!(result.all_agree);
    assert!(!result.promoted());
}

#[test]
fn a_verification_result_for_another_file_is_discarded() {
    let result = outcome(
        vec![comparison("MD5", true)],
        ExternalVerification::ConfirmedExternal,
    );
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new("/mnt/games/roms/gb/other.gb")));
    assert!(!state.accepts_verification(&result));
    state.focus(Some(Path::new(LOCAL)));
    assert!(state.accepts_verification(&result));
}

#[test]
fn a_verification_result_carries_a_compact_label_rather_than_a_path() {
    let result = outcome(
        vec![comparison("MD5", true)],
        ExternalVerification::ConfirmedExternal,
    );
    assert!(
        !result.compact_label.contains('/'),
        "{}",
        result.compact_label
    );
    assert!(
        !result.conclusion().contains(LOCAL),
        "{}",
        result.conclusion()
    );
}

#[test]
fn the_compact_label_names_the_platform_when_there_is_one() {
    let panel = resolve(
        vec![record("1", "Kirby's Dream Land")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    let candidate = panel.chosen_candidate().expect("one");
    assert_eq!(candidate.compact_label(), "Kirby's Dream Land (Game Boy)");
    let mut without = candidate.clone();
    without.canonical_platform = None;
    assert_eq!(without.compact_label(), "Kirby's Dream Land");
}

#[test]
fn compare_hashes_reports_every_algorithm_romm_published() {
    let mut both = record("1", "Game");
    both.hashes = vec![
        ExternalHash::parse(HashAlgorithm::Md5, MD5).expect("md5"),
        ExternalHash::parse(HashAlgorithm::Sha1, SHA1).expect("sha1"),
        ExternalHash::parse(HashAlgorithm::Crc32, CRC).expect("crc"),
    ];
    let local = LocalHashes {
        fingerprint: FileFingerprint {
            path: PathBuf::from(LOCAL),
            size_bytes: 131_072,
            modified_unix_seconds: Some(1),
        },
        crc32: CRC.to_string(),
        md5: MD5.to_string(),
        sha1: "f".repeat(40),
        bytes_hashed: 131_072,
    };
    let lines = compare_hashes(&both, &local);
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| !line.romm.is_empty()));
    // The disagreeing one is reported as disagreeing, and both values are kept.
    let sha = lines
        .iter()
        .find(|line| line.algorithm == "SHA-1")
        .expect("sha-1 line");
    assert!(!sha.agrees);
    assert_eq!(sha.romm, SHA1);
    assert_eq!(sha.local, "f".repeat(40));
}

// --- Hash progress ------------------------------------------------------

#[test]
fn hash_progress_reads_as_text_not_only_as_a_bar() {
    let progress = HashProgressView {
        file_label: "game.gb".to_string(),
        bytes_read: 65_536,
        total_bytes: 131_072,
        elapsed_seconds: 3,
        cancellation_requested: false,
    };
    let line = progress.line();
    assert!(line.contains("game.gb"), "{line}");
    assert!(line.contains("50%"), "{line}");
    assert!(line.contains("3s elapsed"), "{line}");
    assert!(line.contains("CRC32, MD5 and SHA-1"), "{line}");
    assert_eq!(progress.fraction(), Some(0.5));
}

#[test]
fn hash_progress_never_shows_the_full_private_path() {
    let progress = HashProgressView {
        file_label: "game.gb".to_string(),
        bytes_read: 0,
        total_bytes: 10,
        elapsed_seconds: 0,
        cancellation_requested: false,
    };
    assert!(
        !progress.line().contains("/mnt/games"),
        "{}",
        progress.line()
    );
}

#[test]
fn hash_progress_says_when_it_is_stopping() {
    let progress = HashProgressView {
        file_label: "game.gb".to_string(),
        bytes_read: 1,
        total_bytes: 2,
        elapsed_seconds: 90,
        cancellation_requested: true,
    };
    let line = progress.line();
    assert!(line.contains("Stopping"), "{line}");
    assert!(line.contains("1m 30s"), "{line}");
}

#[test]
fn an_empty_file_has_no_fraction_rather_than_a_division_by_zero() {
    let progress = HashProgressView {
        file_label: "empty.gb".to_string(),
        bytes_read: 0,
        total_bytes: 0,
        elapsed_seconds: 0,
        cancellation_requested: false,
    };
    assert!(progress.fraction().is_none());
    assert!(!progress.line().is_empty());
}

// --- Nothing secret reaches the panel -----------------------------------

#[test]
fn no_view_model_can_carry_a_token_or_a_url() {
    let mut with_artwork = record("1", "Game");
    with_artwork.artwork = Some(ArtworkReference {
        reference: "https://images.igdb.com/cover.jpg".to_string(),
        small_reference: Some("assets/romm/resources/small.png?ts=1 2".to_string()),
    });
    let panel = resolve(
        vec![with_artwork],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    );
    let rendered = format!("{panel:?}");
    assert!(!rendered.to_lowercase().contains("bearer"), "{rendered}");
    assert!(!rendered.to_lowercase().contains("token"), "{rendered}");
    // Nor either artwork reference: availability is all the panel needs.
    assert!(!rendered.contains("images.igdb.com"), "{rendered}");
    assert!(!rendered.contains("small.png"), "{rendered}");
}

// --- Decoding a cached thumbnail ----------------------------------------

/// Writes a PNG the way the artwork cache does, and hands back a thumbnail record
/// pointing at it.
fn cached_png(
    label: &str,
    width: u32,
    height: u32,
    with_alpha: bool,
) -> (RealFile, CachedThumbnail) {
    use image::ImageEncoder as _;

    let file = RealFile::new(label, b"");
    let path = file.root.join("thumbnail.png");
    let mut png = Vec::new();
    if with_alpha {
        let pixels: Vec<u8> = (0..(width * height))
            .flat_map(|index| [(index % 251) as u8, 12, 34, 128])
            .collect();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgba8)
            .expect("encoded");
    } else {
        let pixels: Vec<u8> = (0..(width * height))
            .flat_map(|index| [(index % 251) as u8, 12, 34])
            .collect();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixels, width, height, image::ExtendedColorType::Rgb8)
            .expect("encoded");
    }
    std::fs::write(&path, &png).expect("fixture");
    let thumbnail = CachedThumbnail {
        key: format!("key-{label}"),
        path,
        width,
        height,
        bytes: png.len() as u64,
    };
    (file, thumbnail)
}

#[test]
fn an_rgb_thumbnail_decodes_which_is_the_shape_romm_actually_serves() {
    // The real cached cover on this machine is 162x216 8-bit RGB, so this is the path
    // that matters rather than the RGBA one.
    let (_file, thumbnail) = cached_png("rgb", 162, 216, false);
    let cover = decode_thumbnail(&thumbnail, true).expect("decoded");
    assert_eq!((cover.width, cover.height), (162, 216));
    assert_eq!(cover.image.size, [162, 216]);
    assert!(cover.from_cache);
}

#[test]
fn an_rgba_thumbnail_decodes_too() {
    let (_file, thumbnail) = cached_png("rgba", 100, 140, true);
    let cover = decode_thumbnail(&thumbnail, false).expect("decoded");
    assert_eq!(cover.image.size, [100, 140]);
    assert!(!cover.from_cache);
}

#[test]
fn a_thumbnail_that_is_not_a_png_is_refused_rather_than_guessed_at() {
    let file = RealFile::new("not-png", b"");
    let path = file.root.join("thumbnail.png");
    std::fs::write(&path, b"\xff\xd8\xffthis claims to be a jpeg").expect("fixture");
    let thumbnail = CachedThumbnail {
        key: "key".to_string(),
        path,
        width: 10,
        height: 10,
        bytes: 26,
    };
    let refusal = decode_thumbnail(&thumbnail, true).expect_err("refused");
    assert!(refusal.contains("not a readable PNG"), "{refusal}");
}

#[test]
fn a_thumbnail_larger_than_a_thumbnail_should_ever_be_is_refused_unread() {
    let file = RealFile::new("too-big", b"");
    let path = file.root.join("thumbnail.png");
    std::fs::write(&path, vec![0_u8; (MAX_THUMBNAIL_READ_BYTES + 1) as usize]).expect("fixture");
    let thumbnail = CachedThumbnail {
        key: "key".to_string(),
        path,
        width: 10,
        height: 10,
        bytes: MAX_THUMBNAIL_READ_BYTES + 1,
    };
    let refusal = decode_thumbnail(&thumbnail, true).expect_err("refused");
    assert!(refusal.contains("larger than a thumbnail"), "{refusal}");
}

#[test]
fn a_thumbnail_that_is_not_there_is_refused_without_panicking() {
    let thumbnail = CachedThumbnail {
        key: "key".to_string(),
        path: PathBuf::from("/nonexistent/thumbnail.png"),
        width: 10,
        height: 10,
        bytes: 10,
    };
    assert!(decode_thumbnail(&thumbnail, true).is_err());
}

// --- Rendering ----------------------------------------------------------

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

fn render(state: &mut GamePanelState, inputs: &GamePanelInputs<'_>) -> egui::FullOutput {
    let context = egui::Context::default();
    context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_game_identity_panel(ui, state, inputs);
        });
    })
}

fn ready_inputs() -> GamePanelInputs<'static> {
    GamePanelInputs {
        busy: false,
        busy_reason: None,
        hash_progress: None,
        cache_present: true,
    }
}

#[test]
fn with_nothing_selected_the_panel_says_what_to_do() {
    let mut state = GamePanelState::default();
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(
        &output,
        "Select an archive in the Library"
    ));
}

#[test]
fn with_no_catalogue_the_panel_says_to_import_first_and_offers_nothing_else() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    let output = render(
        &mut state,
        &GamePanelInputs {
            cache_present: false,
            ..ready_inputs()
        },
    );
    assert!(rendered_text_contains(&output, "No RomM catalogue yet"));
    assert!(!rendered_text_contains(&output, "Verify local file"));
}

#[test]
fn an_unopened_panel_states_that_it_has_looked_nothing_up_and_contacts_nothing() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(&output, "Not looked up yet"));
    assert!(rendered_text_contains(&output, "Nothing is hashed"));
    assert!(rendered_text_contains(&output, "Look up in RomM"));
}

#[test]
fn a_strong_record_is_drawn_as_strong_and_never_as_confirmed() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "Kirby's Dream Land")],
        &LocalHashCache::new(),
        &LocalPlatformClaim {
            platform: Some("Game Boy".to_string()),
            manual: false,
        },
        None,
    )));
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(&output, "Kirby's Dream Land"));
    assert!(rendered_text_contains(&output, "RomM verdict: Strong"));
    assert!(
        !rendered_text_contains(&output, "Confirmed"),
        "an unhashed file must never read as Confirmed"
    );
    assert!(rendered_text_contains(
        &output,
        "This file has not been hashed"
    ));
}

#[test]
fn an_ambiguous_selection_is_drawn_with_both_claimants_and_no_default_choice() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "First Claim"), record("2", "Second Claim")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    )));
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(
        &output,
        "More than one RomM record maps here"
    ));
    assert!(rendered_text_contains(&output, "First Claim"));
    assert!(rendered_text_contains(&output, "Second Claim"));
    // No candidate detail is drawn, because none has been chosen.
    assert!(!rendered_text_contains(&output, "Hash verification"));
}

#[test]
fn a_folder_shows_verify_disabled_with_the_reason_rather_than_hidden() {
    let cache = cache(vec![record("1", "Shenmue")]);
    let panel = resolve_selected_game(
        &cache,
        Path::new(LOCAL),
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
        &probe(HashMap::from([(
            PathBuf::from(LOCAL),
            LocalPresence::Directory,
        )])),
    );
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(panel));
    let output = render(&mut state, &ready_inputs());
    // The button is still there, disabled, with the reason beside it - not a button
    // that looks live and does nothing.
    assert!(rendered_text_contains(&output, "Verify local file"));
    assert!(rendered_text_contains(&output, "Cannot verify this file"));
    assert!(rendered_text_contains(&output, "Present directory"));
}

#[test]
fn hash_progress_is_drawn_as_a_sentence() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    )));
    let progress = HashProgressView {
        file_label: "game.gb".to_string(),
        bytes_read: 32_768,
        total_bytes: 131_072,
        elapsed_seconds: 1,
        cancellation_requested: false,
    };
    let output = render(
        &mut state,
        &GamePanelInputs {
            busy: true,
            busy_reason: Some("Verifying the local file"),
            hash_progress: Some(&progress),
            cache_present: true,
        },
    );
    assert!(rendered_text_contains(&output, "Reading game.gb"));
    assert!(rendered_text_contains(&output, "25%"));
    assert!(rendered_text_contains(&output, "Stop hashing"));
}

#[test]
fn a_public_only_cover_is_explained_rather_than_fetched() {
    let mut public_only = record("1", "Game");
    public_only.artwork = Some(ArtworkReference {
        reference: "https://images.igdb.com/cover.jpg".to_string(),
        small_reference: None,
    });
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![public_only],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    )));
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(
        &output,
        "only a public scraper link"
    ));
    assert!(rendered_text_contains(&output, "RomM server only"));
    assert!(
        !rendered_text_contains(&output, "images.igdb.com"),
        "the URL itself is never drawn"
    );
}

#[test]
fn a_closed_panel_stays_visible_as_a_way_back_in() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.dismissed = true;
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(&output, "Closed for this archive"));
    assert!(rendered_text_contains(&output, "Show RomM identity"));
}

#[test]
fn a_superseded_result_is_announced_rather_than_silently_drawn() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.needs_reload = true;
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(
        &output,
        "The identity cache changed"
    ));
}

#[test]
fn a_manual_platform_assignment_is_stated_on_screen() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim {
            platform: Some("Game Boy".to_string()),
            manual: true,
        },
        None,
    )));
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(
        &output,
        "Your platform assignment stands"
    ));
    assert!(rendered_text_contains(&output, "assigned by hand"));
}

#[test]
fn a_verification_result_is_drawn_with_both_hash_values() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "Game")],
        &hashes_for(LOCAL, MD5),
        &LocalPlatformClaim::default(),
        None,
    )));
    state.verification = Some(Box::new(outcome(
        vec![comparison("MD5", false)],
        ExternalVerification::Ambiguous,
    )));
    let output = render(&mut state, &ready_inputs());
    assert!(rendered_text_contains(&output, "Verification result"));
    assert!(rendered_text_contains(&output, MD5));
    assert!(rendered_text_contains(&output, "differ"));
    assert!(rendered_text_contains(
        &output,
        "The imported catalogue was not rewritten"
    ));
}

#[test]
fn escape_closes_the_panel_which_is_what_a_controller_back_button_sends() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    let context = egui::Context::default();
    let mut input = egui::RawInput::default();
    input.events.push(egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
    let mut request = None;
    let _ = context.run(input, |context| {
        egui::CentralPanel::default().show(context, |ui| {
            request = show_game_identity_panel(ui, &mut state, &ready_inputs());
        });
    });
    assert_eq!(request, Some(GamePanelRequest::Close));
}

#[test]
fn every_action_offered_is_reachable_by_keyboard_and_has_a_label() {
    // egui buttons are focusable and activate on Enter or Space, so the property that
    // matters here is that no action is drawn as an unlabelled icon.
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    state.panel = Some(Box::new(resolve(
        vec![record("1", "Game")],
        &LocalHashCache::new(),
        &LocalPlatformClaim::default(),
        None,
    )));
    let output = render(&mut state, &ready_inputs());
    for label in [
        "Reload identity",
        "Close",
        "Verify local file",
        "Show cover",
    ] {
        assert!(
            rendered_text_contains(&output, label),
            "{label} is not drawn with a readable label"
        );
    }
}

#[test]
fn while_something_runs_the_actions_are_disabled_and_the_reason_is_named() {
    let mut state = GamePanelState::default();
    state.focus(Some(Path::new(LOCAL)));
    let output = render(
        &mut state,
        &GamePanelInputs {
            busy: true,
            busy_reason: Some("Importing the RomM catalogue"),
            hash_progress: None,
            cache_present: true,
        },
    );
    assert!(rendered_text_contains(
        &output,
        "Importing the RomM catalogue"
    ));
    assert!(rendered_text_contains(&output, "Stop"));
}
