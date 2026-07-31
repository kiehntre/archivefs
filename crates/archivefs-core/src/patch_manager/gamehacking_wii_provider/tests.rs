use std::fs;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::game_identity::{IdentityEvidence, IdentityImageFormat, IdentityProvenance};
use crate::patch_manager::{managed_names, parse_dolphin_ini};

const CATALOGUE_FIXTURE: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/wii-catalogue-sanitized.html");
const GAME_PAGE_FIXTURE: &[u8] =
    include_bytes!("../../../tests/fixtures/gamehacking/wii-game-page-sanitized.html");
const CHALLENGE: &[u8] = b"<html><title>Just a moment...</title>Cloudflare Ray ID: test</html>";

fn temp_root(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "archivefs-wii-gamehacking-{label}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn evidence(
    kind: IdentityKind,
    status: IdentityStatus,
    value: Option<&str>,
    confidence: IdentityConfidence,
) -> IdentityEvidence {
    IdentityEvidence {
        kind,
        status,
        value: value.map(str::to_string),
        confidence,
        provenance: IdentityProvenance {
            archive_path: PathBuf::from("/games/Agent Hugo.iso"),
            member_path: None,
            member_index: None,
            method: "test disc header".to_string(),
        },
        diagnostic: "fixture evidence".to_string(),
    }
}

fn report(platform: IdentityPlatform, game_id: Option<&str>) -> GameIdentityReport {
    let mut evidence_items = vec![evidence(
        IdentityKind::Platform,
        IdentityStatus::Candidate,
        Some("R3HX6Z"),
        IdentityConfidence::FilenameOnly,
    )];
    if let Some(game_id) = game_id {
        evidence_items.extend([
            evidence(
                IdentityKind::DolphinGameId,
                IdentityStatus::Verified,
                Some(game_id),
                IdentityConfidence::ExactBytes,
            ),
            evidence(
                IdentityKind::DolphinRegion,
                IdentityStatus::Verified,
                Some("X"),
                IdentityConfidence::ExactBytes,
            ),
            evidence(
                IdentityKind::DolphinDiscNumber,
                IdentityStatus::Verified,
                Some("0"),
                IdentityConfidence::ExactBytes,
            ),
            evidence(
                IdentityKind::DolphinRevision,
                IdentityStatus::Candidate,
                Some("1"),
                IdentityConfidence::StructuredMetadata,
            ),
        ]);
    }
    GameIdentityReport {
        archive_path: PathBuf::from("/games/Agent Hugo.iso"),
        platform,
        format: IdentityImageFormat::Iso,
        evidence: evidence_items,
        warnings: Vec::new(),
        bytes_read: 32,
        archive_members_inspected: 0,
        metadata_paths_inspected: 0,
        nested_container_depth: 0,
        complete: game_id.is_some(),
    }
}

fn identity() -> WiiGameIdentity {
    WiiGameIdentity::from_report(
        "Agent Hugo: Hula Holiday",
        &report(IdentityPlatform::Wii, Some("R3HX6Z")),
    )
}

fn game(revision: Option<u16>) -> GameHackingWiiGame {
    GameHackingWiiGame {
        game_id: 131936,
        title: "Agent Hugo: Hula Holiday".to_string(),
        system: "Wii".to_string(),
        region: Some("Europe".to_string()),
        dolphin_game_id: Some("R3HX6Z".to_string()),
        revision,
        disc_number: None,
        crc32: None,
        source_url: "https://gamehacking.org/game/131936".to_string(),
    }
}

#[test]
fn confirmed_wii_url_and_slug_are_exact() {
    assert_eq!(WII_INDEX_URL, "https://gamehacking.org/system/wii/all");
    assert_eq!(wii_page_number_from_href("/system/wii/all/3"), Some(3));
    assert_eq!(wii_page_number_from_href("/system/ngc/all/3"), None);
}

#[test]
fn catalogue_fixture_parses_identity_region_revision_and_disc() {
    let records = parse_gamehacking_wii_index_page(WII_INDEX_URL, 123, CATALOGUE_FIXTURE).unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].dolphin_game_id.as_deref(), Some("R3HX6Z"));
    assert_eq!(records[1].revision, Some(1));
    assert_eq!(records[2].disc_number, Some(2));
}

#[test]
fn verified_wii_identity_keeps_disc_region_provenance_and_unverified_revision() {
    let identity = identity();
    assert_eq!(identity.verified_game_id(), Some("R3HX6Z"));
    assert_eq!(identity.region.as_deref(), Some("X"));
    assert_eq!(identity.disc_number, Some(0));
    assert_eq!(identity.verified_revision, None);
    assert_eq!(identity.candidate_revision, Some(1));
    assert!(
        identity
            .evidence
            .iter()
            .any(|line| line.contains("test disc header"))
    );
}

#[test]
fn filename_only_and_gamecube_identity_are_never_trusted_for_wii() {
    let filename_only =
        WiiGameIdentity::from_report("R3HX6Z.iso", &report(IdentityPlatform::Wii, None));
    assert_eq!(filename_only.verified_game_id(), None);
    let gamecube = WiiGameIdentity::from_report(
        "Agent Hugo",
        &report(IdentityPlatform::GameCube, Some("R3HX6Z")),
    );
    assert_eq!(gamecube.state, WiiIdentityState::Unsupported);
    assert_eq!(gamecube.verified_game_id(), None);
}

#[test]
fn persisted_wbfs_header_identity_is_accepted_but_candidate_only_is_blocked() {
    let mut verified = report(IdentityPlatform::Wii, Some("R3HX6Z"));
    verified.format = IdentityImageFormat::Wbfs;
    verified.archive_path = PathBuf::from("/games/Wrong [SMNE01].wbfs");
    for item in &mut verified.evidence {
        item.provenance.archive_path = verified.archive_path.clone();
        item.provenance.method = "WBFS-contained Wii disc-info header copy".to_string();
    }
    let identity = WiiGameIdentity::from_report("Agent Hugo", &verified);
    assert_eq!(identity.verified_game_id(), Some("R3HX6Z"));

    let mut candidate_only = report(IdentityPlatform::Wii, None);
    candidate_only.format = IdentityImageFormat::Wbfs;
    candidate_only.evidence.push(evidence(
        IdentityKind::DolphinGameId,
        IdentityStatus::Candidate,
        Some("R3HX6Z"),
        IdentityConfidence::FilenameOnly,
    ));
    assert_eq!(
        WiiGameIdentity::from_report("Agent Hugo", &candidate_only).verified_game_id(),
        None
    );
}

#[test]
fn exact_id_and_region_matches_but_revision_requires_confirmation() {
    assert_eq!(
        classify_wii_match(&identity(), &game(None)),
        Some((GameHackingWiiMatchStrength::ExactGameIdAndRegion, false))
    );
    assert_eq!(
        classify_wii_match(&identity(), &game(Some(1))),
        Some((
            GameHackingWiiMatchStrength::ExactGameIdRevisionUnverified,
            true
        ))
    );
}

#[test]
fn page_fixture_uses_explicit_labels_and_applies_safety_policy() {
    let cheats = parse_wii_game_page(&identity(), &game(None), GAME_PAGE_FIXTURE).unwrap();
    assert_eq!(cheats.len(), 7);
    assert_eq!(cheats[0].code_format, WiiCodeFormat::Gecko);
    assert_eq!(cheats[0].safety, WiiCheatSafety::Installable);
    assert_eq!(cheats[1].code_format, WiiCodeFormat::ActionReplay);
    assert_eq!(cheats[1].safety, WiiCheatSafety::Installable);
    assert_eq!(cheats[2].code_format, WiiCodeFormat::RawUnknown);
    assert_eq!(cheats[2].safety, WiiCheatSafety::UnverifiedFormatLabel);
    assert_eq!(cheats[3].safety, WiiCheatSafety::UnresolvedPlaceholder);
    assert_eq!(
        cheats[4].safety,
        WiiCheatSafety::UnsupportedMasterCodeRequirement
    );
    assert_eq!(cheats[5].safety, WiiCheatSafety::MalformedCode);
    assert!(
        cheats[6]
            .safety_warnings
            .iter()
            .any(|warning| warning.contains("controller"))
    );
}

#[test]
fn manual_page_import_is_bounded_validated_namespaced_and_challenge_safe() {
    let root = temp_root("import");
    let outcome =
        import_wii_game_page_bytes(&root, &identity(), &game(None), GAME_PAGE_FIXTURE).unwrap();
    assert_eq!(outcome.cache_path, root.join("wii-game-131936.html"));
    assert_eq!(outcome.cheats.len(), 7);
    assert!(
        !fs::read_to_string(&outcome.cache_path)
            .unwrap()
            .contains("<script")
    );
    let previous = fs::read(&outcome.cache_path).unwrap();
    let failure =
        import_wii_game_page_bytes(&root, &identity(), &game(None), CHALLENGE).unwrap_err();
    assert_eq!(failure.kind, WiiManualImportErrorKind::ChallengeContent);
    assert_eq!(fs::read(&outcome.cache_path).unwrap(), previous);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn text_export_import_is_explicitly_blocked_without_a_real_wii_fixture() {
    let failure = import_wii_text_export_unverified(b"R3HX6Z\nexample").unwrap_err();
    assert_eq!(
        failure.kind,
        WiiManualImportErrorKind::TextExportShapeUnverified
    );
}

#[test]
fn mixed_action_replay_and_gecko_install_reuses_dolphin_document_pipeline() {
    let root = temp_root("install");
    let destination = parse_dolphin_ini("[Core]\nCPUThread = True\n");
    let cheats = parse_wii_game_page(&identity(), &game(None), GAME_PAGE_FIXTURE).unwrap();
    let staged =
        stage_wii_gamehacking_install(&root, "R3HX6Z.ini", &destination, true, &cheats, &[0, 1])
            .unwrap();
    assert_eq!(staged.path, root.join("R3HX6Z.ini"));
    assert!(staged.contents.contains("[Core]\nCPUThread = True"));
    assert!(staged.contents.contains("[Gecko]"));
    assert!(staged.contents.contains("[ActionReplay]"));
    assert!(staged.contents.contains("[ArchiveFS_Managed_GameHacking]"));
    let parsed = parse_dolphin_ini(&staged.contents);
    assert_eq!(managed_names(&parsed).len(), 2);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cached_shared_crawler_is_deterministic_and_never_uses_live_network() {
    let root = temp_root("crawler");
    fs::write(root.join("robots.txt"), b"User-agent: *\nAllow: /\n").unwrap();
    fs::write(root.join(WII_INDEX_ROOT_CACHE_FILE), CATALOGUE_FIXTURE).unwrap();
    fs::write(root.join("wii-index-root.retrieved"), b"1700000000").unwrap();
    let options = GameHackingWiiFetchOptions {
        cache_root: root.clone(),
        force_refresh: false,
        delay: Duration::ZERO,
        cancellation: Some(std::sync::Arc::new(AtomicBool::new(false))),
    };
    let provider = GameHackingWiiProvider::default();
    let first = provider.refresh_wii_index(&options, |_| {}).unwrap();
    let first_bytes = fs::read(&first.catalogue_path).unwrap();
    let second = provider.refresh_wii_index(&options, |_| {}).unwrap();
    assert_eq!(first_bytes, fs::read(&second.catalogue_path).unwrap());
    assert_eq!(second.pages_reused, 1);
    assert_eq!(load_wii_catalogue(&root).unwrap().games.len(), 3);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cache_contract_is_namespaced_and_does_not_overlap_gamecube() {
    let root = Path::new("/tmp/archivefs-cache-contract");
    assert_eq!(
        root.join(WII_CATALOGUE_FILE),
        root.join("wii-catalogue.json")
    );
    assert_eq!(
        root.join(WII_INDEX_ROOT_CACHE_FILE),
        root.join("wii-index-root.html")
    );
    assert_eq!(
        root.join(format!("{WII_INDEX_PAGE_PREFIX}2.html")),
        root.join("wii-index-page-2.html")
    );
    assert_ne!(root.join("wii-game-42.html"), root.join("game-42.html"));
}

#[test]
fn adapter_source_delegates_transport_and_crawl_instead_of_copying_them() {
    let source = include_str!("../gamehacking_wii_provider.rs");
    assert!(source.contains("GameHackingClient"));
    assert!(source.contains("GameHackingCatalogueCrawler"));
    for forbidden in ["ureq::", "reqwest::", "TcpStream", "ClientBuilder"] {
        assert!(
            !source.contains(forbidden),
            "Wii adapter copied a low-level transport primitive: {forbidden}"
        );
    }
}
