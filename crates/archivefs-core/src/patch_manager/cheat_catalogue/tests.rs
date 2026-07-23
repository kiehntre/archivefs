use std::fs;

use super::*;
use crate::emulator_environment::HostReadOnlyFilesystem;
use crate::emulator_environment::retroarch::{
    ContentPathKind, PlaylistCrc, ProfileKind, ProfileRef, ProfileScope, RetroArchEnvironmentReport,
};
use crate::patch_manager::{
    ArtifactAssociation, ArtifactAssociationConfidence, ArtifactCatalogueGame, CheatFileSummary,
    CoreAssociation, CoreMatchDisposition, DestinationKind, PlaylistEvidence, ProposedDestination,
    RetroArchAdvisoryEntry, RetroArchAdvisorySummary, RetroArchArtifactFinding,
    RetroArchArtifactInventory, RetroArchArtifactSummary, RetroArchProfileOutcome,
};

// ---------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------

fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "archivefs-cheat-catalogue-test-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn evidence(
    archive_id: i64,
    display_name: &str,
    platform: Option<&str>,
    region: Option<&str>,
    serial: Option<&str>,
    executable_crc: Option<&str>,
) -> CatalogueGameEvidence {
    CatalogueGameEvidence {
        archive_id,
        is_present: true,
        display_name: display_name.to_string(),
        normalized_name: display_name.to_ascii_lowercase(),
        platform: platform.map(str::to_string),
        region: region.map(str::to_string),
        serial: serial.map(str::to_string),
        executable_crc: executable_crc.map(str::to_string),
    }
}

fn write_manifest(root: &Path, json: &str) -> PathBuf {
    let path = root.join("manifest.json");
    fs::write(&path, json).unwrap();
    path
}

fn profile_ref() -> ProfileRef {
    ProfileRef {
        profile_kind: ProfileKind::Native,
        scope: ProfileScope::User,
    }
}

fn empty_environment() -> RetroArchEnvironmentReport {
    RetroArchEnvironmentReport {
        format_version: 1,
        profiles: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn unsupported_destination() -> ProposedDestination {
    ProposedDestination {
        kind: DestinationKind::Unsupported,
        path: None,
        file_name: None,
        derivation: "unsupported",
        parent_exists: None,
        destination_exists: None,
        conflict: false,
        unsupported_reason: Some("test fixture"),
    }
}

fn cheat_destination(
    archive_id: i64,
    display_name: &str,
    path: &Path,
    probe: FsProbe,
    state: ArtifactConflictState,
) -> RetroArchArtifactDestination {
    RetroArchArtifactDestination {
        profile: Some(profile_ref()),
        artifact_kind: ArtifactKind::Cheat,
        path: EncodedPath::from_path(path),
        catalogue_games: vec![ArtifactCatalogueGame {
            archive_id,
            display_name: display_name.to_string(),
            platform: None,
        }],
        probe,
        size_bytes: None,
        state,
    }
}

fn empty_summary() -> RetroArchArtifactSummary {
    RetroArchArtifactSummary {
        artifacts_found: 0,
        cheat_files: 0,
        soft_patch_files: 0,
        expected_destinations: 0,
        empty_destinations: 0,
        occupied_destinations: 0,
        duplicate_artifacts: 0,
        conflicting_artifacts: 0,
        orphaned_artifacts: 0,
        ambiguous_artifacts: 0,
        unsupported_artifacts: 0,
    }
}

fn inventory_with(
    destinations: Vec<RetroArchArtifactDestination>,
    findings: Vec<RetroArchArtifactFinding>,
) -> RetroArchArtifactInventory {
    RetroArchArtifactInventory {
        format_version: 1,
        read_only: true,
        complete: true,
        findings,
        destinations,
        diagnostics: Vec::new(),
        summary: empty_summary(),
    }
}

fn plan_with(
    entries: Vec<RetroArchAdvisoryEntry>,
    inventory: RetroArchArtifactInventory,
) -> RetroArchAdvisoryPlan {
    RetroArchAdvisoryPlan {
        format_version: 1,
        plan_id: "test-plan".to_string(),
        executable: false,
        environment: empty_environment(),
        entries,
        artifact_inventory: inventory,
        summary: RetroArchAdvisorySummary {
            catalogue_archives: 0,
            exact_core_profile_outcomes: 0,
            ambiguous_core_profile_outcomes: 0,
            unsupported_profile_outcomes: 0,
        },
    }
}

fn advisory_entry(
    archive_id: i64,
    display_name: &str,
    normalized_name: &str,
    platform: Option<&str>,
    playlist_evidence: Vec<PlaylistEvidence>,
) -> RetroArchAdvisoryEntry {
    RetroArchAdvisoryEntry {
        archive_id,
        display_name: display_name.to_string(),
        normalized_name: normalized_name.to_string(),
        platform: platform.map(str::to_string),
        content_extension: None,
        soft_patch_candidates: Vec::new(),
        profile_outcomes: vec![RetroArchProfileOutcome {
            profile: profile_ref(),
            disposition: CoreMatchDisposition::UnsupportedNoCore,
            matched_core_stem: None,
            candidate_core_stems: Vec::new(),
            selected_core_source: None,
            playlist_evidence,
            cheat_database_root: unsupported_destination(),
            per_game_cheat_file: unsupported_destination(),
            reasons: Vec::new(),
        }],
    }
}

fn exact_playlist_evidence(archive_id: i64) -> PlaylistEvidence {
    PlaylistEvidence {
        playlist_file: EncodedPath::from_path(Path::new("/config/playlists/test.lpl")),
        playlist_name: "test".to_string(),
        entry_index: 0,
        entry_label: None,
        matched_archive_id: Some(archive_id),
        ambiguous_archive_ids: Vec::new(),
        confidence: PlaylistMatchConfidence::Exact,
        evidence_basis: "exact_content_path",
        content_path_kind: ContentPathKind::Filesystem,
        database_name: None,
        crc: PlaylistCrc::Missing,
        core_association: CoreAssociation::NoCoreEvidence,
    }
}

fn one_game_manifest(name: &str, platform: &str, region: &str, serial: &str) -> String {
    format!(
        r#"{{
            "source_name": "Fixture",
            "games": [
                {{
                    "game_name": "{name}",
                    "platform": "{platform}",
                    "region": "{region}",
                    "serial": "{serial}",
                    "cheats": [
                        {{"description": "Infinite Lives", "enabled_by_default": false}},
                        {{"description": "Moon Jump", "enabled_by_default": true}}
                    ]
                }}
            ]
        }}"#
    )
}

// ---------------------------------------------------------------------
// Catalogue loading
// ---------------------------------------------------------------------

#[test]
fn empty_catalogue_json_manifest() {
    let root = temp_root("empty-manifest");
    let path = write_manifest(&root, r#"{"source_name": "Empty", "games": []}"#);
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Empty", &path);
    assert!(snapshot.complete);
    assert!(snapshot.games.is_empty());
    assert!(snapshot.diagnostics.is_empty());
}

#[test]
fn empty_catalogue_cht_directory() {
    let root = temp_root("empty-cht-dir");
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Empty", &root);
    assert!(snapshot.complete);
    assert!(snapshot.games.is_empty());
}

#[test]
fn one_matching_game_json_manifest() {
    let root = temp_root("one-matching-game");
    let path = write_manifest(
        &root,
        &one_game_manifest("Example Adventure", "SNES", "USA", "SNS-EX-USA"),
    );
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &path);
    assert_eq!(snapshot.games.len(), 1);
    let game = &snapshot.games[0];
    assert_eq!(game.source_game_name, "Example Adventure");
    assert_eq!(game.cheat_count, 2);
    assert_eq!(game.enabled_by_default_count, 1);
    assert!(game.parsing_complete);
    // Cheat *code* bodies are never parsed/stored - only description and
    // enabled-by-default state are present on each definition.
    for cheat in &game.cheats {
        assert!(cheat.description.is_some());
    }
}

#[test]
fn retroarch_cht_folder_platform_alias_matches_canonical_catalogue_platform() {
    let root = temp_root("atari-2600-folder-alias");
    let platform_root = root.join("Atari - 2600");
    fs::create_dir_all(&platform_root).unwrap();
    fs::write(platform_root.join("Frogger (USA).cht"), "cheats = 0\n").unwrap();

    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    let games = vec![evidence(
        2600,
        "Frogger (USA)",
        Some("Atari2600"),
        None,
        None,
        None,
    )];
    let report =
        build_cheat_availability_report(&HostReadOnlyFilesystem, &snapshot, &games, None, None);

    assert_eq!(report.entries.len(), 1);
    assert_eq!(
        report.entries[0].game.source_platform.as_deref(),
        Some("Atari - 2600")
    );
    assert_eq!(report.entries[0].game.source_game_name, "Frogger (USA)");
    assert_eq!(
        report.entries[0].game_match.confidence,
        CheatMatchConfidence::Strong
    );
    assert_eq!(
        report.entries[0].game_match.evidence[0].detail,
        "normalized title and canonical platform match (Atari - 2600 -> Atari2600)"
    );
}

#[test]
fn malformed_cht_is_a_bounded_non_fatal_exclusion() {
    let root = temp_root("malformed-cht");
    fs::write(
        root.join("Game.cht"),
        "cheats = 2\nnot an assignment\ncheat0_desc = \"A\"\n",
    )
    .unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    assert!(snapshot.complete);
    assert_eq!(snapshot.index_state, CatalogueIndexState::UsablePartial);
    assert!(snapshot.games.is_empty());
    assert_eq!(snapshot.excluded_entries.len(), 1);
    assert_eq!(
        snapshot.excluded_entries[0].kind,
        CatalogueEntryExclusionKind::MalformedCht
    );
    assert_eq!(
        snapshot.excluded_entries[0].source_game_name.as_deref(),
        Some("Game")
    );
}

#[test]
fn oversized_cht_file_is_skipped_with_diagnostic() {
    let root = temp_root("oversized-cht");
    let oversized = vec![b'#'; MAX_CATALOGUE_FILE_BYTES + 1];
    fs::write(root.join("Huge.cht"), oversized).unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    assert!(snapshot.games.is_empty());
    assert!(!snapshot.complete);
    assert!(
        snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "catalogue_file_too_large")
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_filename_is_skipped_with_diagnostic() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let root = temp_root("non-utf8-filename");
    let name = OsString::from_vec(b"bad-\xFF-name.cht".to_vec());
    fs::write(root.join(&name), "cheats = 0\n").unwrap();
    fs::write(root.join("Good.cht"), "cheats = 0\n").unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    assert_eq!(snapshot.games.len(), 1);
    assert!(snapshot.complete);
    assert_eq!(snapshot.index_state, CatalogueIndexState::UsablePartial);
    assert_eq!(snapshot.excluded_path_encoding_count(), 1);
    assert!(snapshot.excluded_entries[0].path.lossy);
    assert!(snapshot.diagnostics.is_empty());
}

#[test]
fn invalid_utf8_content_is_excluded_without_poisoning_valid_entries() {
    let root = temp_root("invalid-content-encoding");
    fs::write(root.join("Good.cht"), "cheats = 0\n").unwrap();
    fs::write(root.join("Bad.cht"), b"cheats = 1\n\xff").unwrap();

    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);

    assert!(snapshot.complete);
    assert_eq!(snapshot.index_state, CatalogueIndexState::UsablePartial);
    assert_eq!(snapshot.games.len(), 1);
    assert_eq!(snapshot.excluded_unsupported_count(), 1);
    assert_eq!(
        snapshot.excluded_entries[0].kind,
        CatalogueEntryExclusionKind::UnsupportedContentEncoding
    );
}

#[test]
fn exclusion_examples_are_deterministic_and_bounded() {
    let root = temp_root("deterministic-exclusions");
    fs::write(root.join("Zed.cht"), b"\xff").unwrap();
    fs::write(root.join("Alpha.cht"), "not an assignment\n").unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    let paths = snapshot
        .excluded_entries
        .iter()
        .take(MAX_CATALOGUE_EXCLUSION_EXAMPLES)
        .map(|entry| entry.path.display.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            root.join("Alpha.cht").display().to_string(),
            root.join("Zed.cht").display().to_string(),
        ]
    );

    let mut exclusions = Vec::new();
    let mut diagnostics = Vec::new();
    for index in 0..MAX_CATALOGUE_EXCLUDED_ENTRIES {
        assert!(push_excluded_entry(
            &mut exclusions,
            CatalogueExcludedEntry {
                kind: CatalogueEntryExclusionKind::MalformedCht,
                path: EncodedPath::from_path(&root.join(format!("{index}.cht"))),
                source_game_name: Some(index.to_string()),
                source_platform: Some("SNES".into()),
            },
            &root,
            &mut diagnostics,
        ));
    }
    assert!(!push_excluded_entry(
        &mut exclusions,
        CatalogueExcludedEntry {
            kind: CatalogueEntryExclusionKind::MalformedCht,
            path: EncodedPath::from_path(&root.join("overflow.cht")),
            source_game_name: Some("overflow".into()),
            source_platform: Some("SNES".into()),
        },
        &root,
        &mut diagnostics,
    ));
    assert_eq!(exclusions.len(), MAX_CATALOGUE_EXCLUDED_ENTRIES);
    assert_eq!(diagnostics[0].code, "catalogue_exclusion_limit_reached");
}

#[cfg(unix)]
#[test]
fn symlink_cht_file_not_followed() {
    let root = temp_root("symlink-cht-file");
    let real = root.join("real.cht");
    fs::write(&real, "cheats = 0\n").unwrap();
    let link = root.join("link.cht");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    // Only `real.cht` becomes a record; `link.cht` is reported as a
    // diagnostic, never opened.
    assert_eq!(snapshot.games.len(), 1);
    assert!(
        snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "catalogue_file_symlink_not_followed")
    );
}

#[cfg(unix)]
#[test]
fn symlink_directory_escape_blocked() {
    let root = temp_root("symlink-dir-escape-root");
    let outside = temp_root("symlink-dir-escape-outside");
    fs::write(outside.join("Secret.cht"), "cheats = 0\n").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    // The symlinked directory is never traversed, so the file outside the
    // catalogue root is never discovered.
    assert!(snapshot.games.is_empty());
}

#[test]
fn multiple_cht_files_for_one_game_remain_separate_records() {
    let root = temp_root("multiple-files-one-game");
    fs::create_dir_all(root.join("SNES")).unwrap();
    fs::create_dir_all(root.join("SNES Alt")).unwrap();
    fs::write(root.join("SNES").join("Game.cht"), "cheats = 0\n").unwrap();
    fs::write(root.join("SNES Alt").join("Game.cht"), "cheats = 0\n").unwrap();

    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    assert_eq!(snapshot.games.len(), 2);
    assert_ne!(
        snapshot.games[0].source_file_path.display,
        snapshot.games[1].source_file_path.display
    );
}

#[test]
fn deterministic_ordering() {
    let root = temp_root("deterministic-ordering");
    fs::write(root.join("Zeta.cht"), "cheats = 0\n").unwrap();
    fs::write(root.join("Alpha.cht"), "cheats = 0\n").unwrap();
    fs::write(root.join("Mu.cht"), "cheats = 0\n").unwrap();

    let first = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    let second = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    let names = |snapshot: &CheatCatalogueSnapshot| {
        snapshot
            .games
            .iter()
            .map(|game| game.source_file_path.display.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&first), names(&second));
    let mut sorted = names(&first).clone();
    sorted.sort();
    assert_eq!(names(&first), sorted);
}

#[test]
fn stable_json_serialization() {
    let root = temp_root("stable-json");
    let path = write_manifest(
        &root,
        &one_game_manifest("Example Adventure", "SNES", "USA", "SNS-EX-USA"),
    );
    let games = vec![evidence(
        1,
        "Example Adventure",
        Some("SNES"),
        Some("USA"),
        Some("SNS-EX-USA"),
        None,
    )];
    let first_snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &path);
    let second_snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &path);
    let first_report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &first_snapshot,
        &games,
        None,
        None,
    );
    let second_report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &second_snapshot,
        &games,
        None,
        None,
    );
    assert_eq!(
        serde_json::to_string(&first_report).unwrap(),
        serde_json::to_string(&second_report).unwrap()
    );
}

// ---------------------------------------------------------------------
// Matching tiers
// ---------------------------------------------------------------------

fn record_with(
    name: &str,
    platform: Option<&str>,
    region: Option<&str>,
    identifier: Option<&str>,
    content_hash: Option<&str>,
) -> CheatGameRecord {
    CheatGameRecord {
        source_game_name: name.to_string(),
        source_platform: platform.map(str::to_string),
        source_region: region.map(str::to_string),
        source_revision: None,
        source_identifier: identifier.map(str::to_string),
        source_content_hash: content_hash.map(str::to_string),
        target_emulator: Some("retroarch".to_string()),
        cheat_count: 0,
        cheats: Vec::new(),
        enabled_by_default_count: 0,
        source_file_path: EncodedPath::from_path(Path::new("/fixture/game.cht")),
        source_file_hash: Some("deadbeef".to_string()),
        format: CheatCatalogueFormat::JsonManifest,
        parsing_complete: true,
        parsing_diagnostics: Vec::new(),
    }
}

#[test]
fn exact_serial_match() {
    let record = record_with("Any Title", Some("SNES"), None, Some("SNS-EX-USA"), None);
    let games = vec![evidence(
        1,
        "Completely Different Title",
        Some("SNES"),
        None,
        Some("sns-ex-usa"), // case-insensitive
        None,
    )];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Exact);
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(outcome.candidates[0].archive_id, 1);
    assert_eq!(outcome.evidence[0].tier, "exact_serial");
}

#[test]
fn exact_content_hash_match() {
    let record = record_with("Any Title", None, None, None, Some("ABCD1234"));
    let games = vec![evidence(
        7,
        "Unrelated Name",
        None,
        None,
        None,
        Some("abcd1234"),
    )];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Exact);
    assert_eq!(outcome.candidates[0].archive_id, 7);
    assert_eq!(outcome.evidence[0].tier, "exact_content_hash");
}

#[test]
fn exact_playlist_identity_match() {
    let record = record_with("Chrono Quest", Some("SNES"), None, None, None);
    let games = vec![evidence(3, "Chrono Quest", Some("SNES"), None, None, None)];
    let entry = advisory_entry(
        3,
        "Chrono Quest",
        "chrono quest",
        Some("SNES"),
        vec![exact_playlist_evidence(3)],
    );
    let plan = plan_with(vec![entry], inventory_with(Vec::new(), Vec::new()));
    let outcome = match_cheat_game_record(&record, &games, Some(&plan));
    assert_eq!(outcome.confidence, CheatMatchConfidence::Exact);
    assert_eq!(outcome.evidence[0].tier, "exact_playlist_identity");
}

#[test]
fn title_platform_region_match_is_strong() {
    let record = record_with("Chrono Quest", Some("SNES"), Some("USA"), None, None);
    let games = vec![evidence(
        4,
        "Chrono Quest",
        Some("SNES"),
        Some("USA"),
        None,
        None,
    )];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Strong);
    assert_eq!(outcome.evidence[0].tier, "exact_title_platform_region");
}

#[test]
fn title_platform_match_without_region_is_strong() {
    let record = record_with("Chrono Quest", Some("SNES"), None, None, None);
    let games = vec![evidence(5, "Chrono Quest", Some("SNES"), None, None, None)];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Strong);
    assert_eq!(outcome.evidence[0].tier, "exact_title_platform");
}

#[test]
fn retroarch_platform_folder_alias_promotes_matching_title_to_strong() {
    let record = record_with("Frogger (USA)", Some("Atari - 2600"), None, None, None);
    let games = vec![evidence(
        26,
        "Frogger (USA)",
        Some("Atari2600"),
        None,
        None,
        None,
    )];

    let outcome = match_cheat_game_record(&record, &games, None);

    assert_eq!(record.source_platform.as_deref(), Some("Atari - 2600"));
    assert_eq!(outcome.confidence, CheatMatchConfidence::Strong);
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(outcome.candidates[0].archive_id, 26);
    assert_eq!(outcome.evidence[0].tier, "exact_title_platform");
    assert_eq!(
        outcome.evidence[0].detail,
        "normalized title and canonical platform match (Atari - 2600 -> Atari2600)"
    );
}

#[test]
fn unknown_source_platform_does_not_promote_a_title_match() {
    let record = record_with(
        "Frogger (USA)",
        Some("Unrelated Mystery System"),
        None,
        None,
        None,
    );
    let games = vec![evidence(
        27,
        "Frogger (USA)",
        Some("Atari2600"),
        None,
        None,
        None,
    )];

    let outcome = match_cheat_game_record(&record, &games, None);

    assert_eq!(outcome.confidence, CheatMatchConfidence::Weak);
    assert_eq!(outcome.evidence[0].tier, "filename_only");
    assert_eq!(outcome.evidence[0].detail, "normalized title match only");
}

#[test]
fn weak_filename_only_match() {
    // No platform declared by the source at all - only the normalized
    // title can be compared.
    let record = record_with("Chrono Quest", None, None, None, None);
    let games = vec![evidence(6, "Chrono Quest", Some("SNES"), None, None, None)];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Weak);
    assert_eq!(outcome.evidence[0].tier, "filename_only");
}

#[test]
fn ambiguous_title_multiple_candidates_tie() {
    let record = record_with("Chrono Quest", Some("SNES"), None, None, None);
    let games = vec![
        evidence(8, "Chrono Quest", Some("SNES"), None, None, None),
        evidence(9, "Chrono Quest", Some("SNES"), None, None, None),
    ];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Ambiguous);
    assert_eq!(outcome.candidates.len(), 2);
}

#[test]
fn sequel_is_never_treated_as_a_match() {
    let record = record_with("Super Example Bros", Some("SNES"), None, None, None);
    let games = vec![evidence(
        10,
        "Super Example Bros 2",
        Some("SNES"),
        None,
        None,
        None,
    )];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Unsupported);
    assert!(outcome.candidates.is_empty());
}

#[test]
fn region_mismatch_remains_visible() {
    let record = record_with("Chrono Quest", Some("SNES"), Some("Europe"), None, None);
    let games = vec![evidence(
        11,
        "Chrono Quest",
        Some("SNES"),
        Some("USA"),
        None,
        None,
    )];
    let outcome = match_cheat_game_record(&record, &games, None);
    // Region differs, so tier 4 cannot fire; tier 5 (title+platform) still
    // matches, but the mismatch is recorded as extra evidence rather than
    // silently ignored.
    assert_eq!(outcome.confidence, CheatMatchConfidence::Strong);
    assert!(
        outcome
            .evidence
            .iter()
            .any(|evidence| evidence.tier == "region_mismatch")
    );
}

#[test]
fn revision_mismatch_remains_visible() {
    let mut record = record_with("Chrono Quest", Some("SNES"), None, None, None);
    record.source_revision = Some("REV1".to_string());
    let games = vec![evidence(
        12,
        "Chrono Quest (Rev 2)",
        Some("SNES"),
        None,
        None,
        None,
    )];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Strong);
    assert!(
        outcome
            .evidence
            .iter()
            .any(|evidence| evidence.tier == "revision_mismatch")
    );
}

#[test]
fn no_evidence_is_unsupported() {
    let record = record_with("Totally Unknown Game", None, None, None, None);
    let games = vec![evidence(
        13,
        "Something Else Entirely",
        None,
        None,
        None,
        None,
    )];
    let outcome = match_cheat_game_record(&record, &games, None);
    assert_eq!(outcome.confidence, CheatMatchConfidence::Unsupported);
    assert!(outcome.candidates.is_empty());
    assert!(outcome.evidence.is_empty());
}

// ---------------------------------------------------------------------
// Installed-state integration
// ---------------------------------------------------------------------

#[test]
fn already_installed_exact_match() {
    let root = temp_root("already-installed-exact");
    let manifest_path = write_manifest(
        &root,
        &one_game_manifest("Chrono Quest", "SNES", "USA", "SNS-CQ-USA"),
    );
    let snapshot =
        load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &manifest_path);
    let record = &snapshot.games[0];

    let destination_path = root.join("installed.cht");
    fs::write(
        &destination_path,
        r#"cheats = 2
cheat0_desc = "Infinite Lives"
cheat0_enable = false
cheat1_desc = "Moon Jump"
cheat1_enable = true
"#,
    )
    .unwrap();
    // The installed file's own bytes must match `record.source_file_hash`
    // exactly for `ExactFilePresent` - reconstruct it identically to what
    // the manifest cheat entries would produce is not required here since
    // installed-state hashes raw bytes, not semantic content. Write the
    // installed file so its bytes equal a second read of the same source
    // is not applicable for a JSON manifest (its "file" is the manifest
    // itself, not a `.cht`). This test therefore targets the `.cht`
    // catalogue format, where the installed artifact and the catalogue
    // source are both real `.cht` byte streams.
    let cht_root = temp_root("already-installed-exact-cht");
    let cht_text = "cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n";
    // Nested under a platform subdirectory so the record carries a
    // platform hint - required for `strong` (title+platform) confidence,
    // and in turn for staging eligibility.
    fs::create_dir_all(cht_root.join("SNES")).unwrap();
    fs::write(cht_root.join("SNES").join("Chrono Quest.cht"), cht_text).unwrap();
    fs::write(&destination_path, cht_text).unwrap();
    let cht_snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &cht_root);
    let cht_record = &cht_snapshot.games[0];

    let games = vec![evidence(
        20,
        "Chrono Quest",
        Some("SNES"),
        None,
        Some("SNS-CQ-USA"),
        None,
    )];
    let entry = advisory_entry(20, "Chrono Quest", "chrono quest", Some("SNES"), Vec::new());
    let destination = cheat_destination(
        20,
        "Chrono Quest",
        &destination_path,
        FsProbe::PresentFile,
        ArtifactConflictState::Occupied,
    );
    let plan = plan_with(vec![entry], inventory_with(vec![destination], Vec::new()));

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &CheatCatalogueSnapshot {
            games: vec![cht_record.clone()],
            ..cht_snapshot.clone()
        },
        &games,
        Some(&plan),
        None,
    );
    assert_eq!(report.entries.len(), 1);
    assert_eq!(
        report.entries[0].installed_state,
        CheatInstalledState::ExactFilePresent
    );
    assert_eq!(report.summary.already_installed, 1);
    let _ = record; // manifest-format record kept only to exercise that path above
}

#[test]
fn installed_conflict_different_content() {
    let cht_root = temp_root("installed-conflict-cht");
    fs::create_dir_all(cht_root.join("SNES")).unwrap();
    fs::write(
        cht_root.join("SNES").join("Chrono Quest.cht"),
        "cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n",
    )
    .unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &cht_root);

    let destination_path = cht_root.join("installed.cht");
    fs::write(
        &destination_path,
        "cheats = 1\ncheat0_desc = \"Different\"\n",
    )
    .unwrap();

    let games = vec![evidence(21, "Chrono Quest", Some("SNES"), None, None, None)];
    let entry = advisory_entry(21, "Chrono Quest", "chrono quest", Some("SNES"), Vec::new());
    let destination = cheat_destination(
        21,
        "Chrono Quest",
        &destination_path,
        FsProbe::PresentFile,
        ArtifactConflictState::Occupied,
    );
    let plan = plan_with(vec![entry], inventory_with(vec![destination], Vec::new()));

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        Some(&plan),
        None,
    );
    assert_eq!(
        report.entries[0].installed_state,
        CheatInstalledState::DestinationOccupiedDifferentContent
    );
    // Different content at an existing destination is a preview-only
    // `replace_different` staging action, not a `conflict` - it is a
    // staging candidate (destructive if ever applied), never silently
    // treated as unsafe.
    assert_eq!(
        report.entries[0].staging_plan.planned_action,
        CheatStagingAction::ReplaceDifferent
    );
    assert!(report.entries[0].staging_candidate);
    assert!(report.entries[0].destructive_if_applied);
    assert_eq!(report.summary.conflicts, 0);
    assert_eq!(report.summary.staging_candidates, 1);
}

#[test]
fn installed_file_malformed() {
    let cht_root = temp_root("installed-malformed-cht");
    fs::write(
        cht_root.join("Chrono Quest.cht"),
        "cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n",
    )
    .unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &cht_root);

    let destination_path = cht_root.join("installed.cht");
    let games = vec![evidence(22, "Chrono Quest", Some("SNES"), None, None, None)];
    let entry = advisory_entry(22, "Chrono Quest", "chrono quest", Some("SNES"), Vec::new());
    let destination = cheat_destination(
        22,
        "Chrono Quest",
        &destination_path,
        FsProbe::PresentFile,
        ArtifactConflictState::Occupied,
    );
    let malformed_finding = RetroArchArtifactFinding {
        profile: Some(profile_ref()),
        artifact_kind: ArtifactKind::Cheat,
        path: EncodedPath::from_path(&destination_path),
        filename: EncodedPath::from_path(Path::new("installed.cht")),
        size_bytes: Some(0),
        probe: FsProbe::PresentFile,
        symlink: false,
        association: ArtifactAssociation {
            confidence: ArtifactAssociationConfidence::Exact,
            evidence: Vec::new(),
            catalogue_games: Vec::new(),
            playlist_evidence: Vec::new(),
            core_stems: Vec::new(),
            expected_destinations: Vec::new(),
        },
        occupies_expected_destination: true,
        conflict_state: ArtifactConflictState::Matched,
        cheat_summary: Some(CheatFileSummary {
            description: None,
            declared_cheat_count: Some(5),
            parsed_cheat_entries: 1,
            enabled_cheat_entries: 0,
            any_cheats_enabled: false,
            malformed_lines: vec![2],
            complete: false,
        }),
        diagnostics: Vec::new(),
    };
    let plan = plan_with(
        vec![entry],
        inventory_with(vec![destination], vec![malformed_finding]),
    );

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        Some(&plan),
        None,
    );
    assert_eq!(
        report.entries[0].installed_state,
        CheatInstalledState::InstalledFileMalformed
    );
}

#[test]
fn multiple_installed_candidates_never_silently_picked() {
    let cht_root = temp_root("installed-multiple-cht");
    fs::write(cht_root.join("Chrono Quest.cht"), "cheats = 0\n").unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &cht_root);

    let games = vec![evidence(23, "Chrono Quest", Some("SNES"), None, None, None)];
    let entry = advisory_entry(23, "Chrono Quest", "chrono quest", Some("SNES"), Vec::new());
    let destination_a = cheat_destination(
        23,
        "Chrono Quest",
        &cht_root.join("a.cht"),
        FsProbe::PresentFile,
        ArtifactConflictState::Occupied,
    );
    let destination_b = cheat_destination(
        23,
        "Chrono Quest",
        &cht_root.join("b.cht"),
        FsProbe::PresentFile,
        ArtifactConflictState::Occupied,
    );
    let plan = plan_with(
        vec![entry],
        inventory_with(vec![destination_a, destination_b], Vec::new()),
    );

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        Some(&plan),
        None,
    );
    assert_eq!(
        report.entries[0].installed_state,
        CheatInstalledState::MultipleInstalledCandidates
    );
}

#[test]
fn not_installed_when_destination_empty() {
    let cht_root = temp_root("not-installed-cht");
    fs::create_dir_all(cht_root.join("SNES")).unwrap();
    fs::write(
        cht_root.join("SNES").join("Chrono Quest.cht"),
        "cheats = 0\n",
    )
    .unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &cht_root);

    let games = vec![evidence(24, "Chrono Quest", Some("SNES"), None, None, None)];
    let entry = advisory_entry(24, "Chrono Quest", "chrono quest", Some("SNES"), Vec::new());
    let destination = cheat_destination(
        24,
        "Chrono Quest",
        &cht_root.join("missing.cht"),
        FsProbe::Missing,
        ArtifactConflictState::Empty,
    );
    let plan = plan_with(vec![entry], inventory_with(vec![destination], Vec::new()));

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        Some(&plan),
        None,
    );
    assert_eq!(
        report.entries[0].installed_state,
        CheatInstalledState::NotInstalled
    );
    assert_eq!(report.summary.not_installed, 1);
    assert_eq!(
        report.entries[0].staging_plan.planned_action,
        CheatStagingAction::InstallNew
    );
    assert!(report.entries[0].staging_candidate);
    assert!(!report.entries[0].destructive_if_applied);
}

#[test]
fn installed_state_unknown_without_advisory_plan() {
    let cht_root = temp_root("installed-unknown-no-plan");
    fs::write(cht_root.join("Chrono Quest.cht"), "cheats = 0\n").unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &cht_root);
    let games = vec![evidence(25, "Chrono Quest", Some("SNES"), None, None, None)];

    let report =
        build_cheat_availability_report(&HostReadOnlyFilesystem, &snapshot, &games, None, None);
    assert_eq!(
        report.entries[0].installed_state,
        CheatInstalledState::Unknown
    );
}

// ---------------------------------------------------------------------
// Read-only / no side-effect guarantees
// ---------------------------------------------------------------------

#[test]
fn no_filesystem_writes_or_directory_changes() {
    let root = temp_root("no-writes");
    fs::write(root.join("Game.cht"), "cheats = 0\n").unwrap();
    let before = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();

    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    let games = vec![evidence(30, "Game", None, None, None, None)];
    let _report =
        build_cheat_availability_report(&HostReadOnlyFilesystem, &snapshot, &games, None, None);

    let after = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(before, after);
}

#[test]
fn adversarial_strings_are_inert_data_only() {
    // Shell metacharacters, a URL, and a SQL-looking fragment in the
    // source name/game name/serial must never be interpreted as anything
    // other than plain string data to compare.
    let root = temp_root("adversarial-strings");
    let path = write_manifest(
        &root,
        r#"{
            "source_name": "$(rm -rf /); http://example.invalid/steal",
            "games": [
                {
                    "game_name": "Robert'); DROP TABLE archives;--",
                    "platform": "`touch /tmp/pwned`",
                    "serial": "../../etc/passwd",
                    "cheats": []
                }
            ]
        }"#,
    );
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &path);
    assert_eq!(snapshot.games.len(), 1);
    assert_eq!(
        snapshot.games[0].source_game_name,
        "Robert'); DROP TABLE archives;--"
    );
    assert!(!std::path::Path::new("/tmp/pwned").exists());
}

#[test]
fn module_source_never_touches_network_process_or_database_writes() {
    // Architectural guardrail: grep this module's own source for tokens
    // that would indicate network access, process execution, database
    // writes, or filesystem mutation. This module has no legitimate
    // reason to reference any of them - see the module doc comment's
    // Non-goals section.
    let source = include_str!("../cheat_catalogue.rs");
    for forbidden in [
        "std::process::Command",
        "Command::new",
        "ureq::",
        "reqwest",
        "TcpStream",
        "UdpSocket",
        "Database::open",
        "Database::create",
        "std::fs::write",
        "std::fs::remove",
        "std::fs::create_dir",
        "std::fs::rename",
        "std::fs::set_permissions",
    ] {
        assert!(
            !source.contains(forbidden),
            "cheat_catalogue.rs must never reference `{forbidden}`"
        );
    }
}

#[test]
fn build_availability_report_matches_snapshot_complete_flag() {
    let root = temp_root("complete-flag-propagation");
    let oversized = vec![b'#'; MAX_CATALOGUE_FILE_BYTES + 1];
    fs::write(root.join("Huge.cht"), oversized).unwrap();
    let snapshot = load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &root);
    assert!(!snapshot.complete);
    let report =
        build_cheat_availability_report(&HostReadOnlyFilesystem, &snapshot, &[], None, None);
    assert!(!report.complete);
    assert!(!report.diagnostics.is_empty());
}

// ---------------------------------------------------------------------
// Staging preview (destination planning)
//
// Every test below uses `temp_root`, which is rooted at
// `std::env::temp_dir()` and never at a real `$HOME` - see
// `module_source_never_reads_home_or_xdg_env_directly` below for the
// structural guarantee that this module cannot read the real `$HOME`
// even if a future edit tried to. `Frogger`/`"Atari - 2600"` is used
// throughout to also exercise the canonical-platform-alias resolution
// (`"Atari - 2600"` -> `"Atari2600"`) end to end into a real destination
// path, not just in `match_cheat_game_record`'s own unit tests above.
// ---------------------------------------------------------------------

fn frogger_cht_snapshot(root: &Path) -> CheatCatalogueSnapshot {
    fs::create_dir_all(root.join("Atari - 2600")).unwrap();
    fs::write(
        root.join("Atari - 2600").join("Frogger.cht"),
        "cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n",
    )
    .unwrap();
    load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", root)
}

fn frogger_games() -> Vec<CatalogueGameEvidence> {
    vec![evidence(50, "Frogger", Some("Atari2600"), None, None, None)]
}

#[test]
fn staging_strong_frogger_match_installs_new_with_no_destination_file() {
    let catalogue_root = temp_root("staging-install-new-catalogue");
    let snapshot = frogger_cht_snapshot(&catalogue_root);
    let games = frogger_games();
    let destination_root = temp_root("staging-install-new-dest");

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    assert_eq!(report.entries.len(), 1);
    let entry = &report.entries[0];
    assert_eq!(entry.game_match.confidence, CheatMatchConfidence::Strong);
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::InstallNew
    );
    assert!(entry.staging_candidate);
    assert!(!entry.destructive_if_applied);
    let destination = entry
        .staging_plan
        .proposed_destination_path
        .as_ref()
        .expect("destination resolved");
    assert!(destination.display.contains("Atari2600"));
    assert!(destination.display.ends_with("Frogger.cht"));
    // Preview only - the proposed destination was never actually created.
    assert!(!Path::new(&destination.display).exists());
    assert_eq!(report.summary.not_installed, 1);
    assert_eq!(report.summary.staging_candidates, 1);
}

#[test]
fn staging_strong_frogger_match_with_identical_existing_file_is_already_installed() {
    let catalogue_root = temp_root("staging-already-installed-catalogue");
    let snapshot = frogger_cht_snapshot(&catalogue_root);
    let games = frogger_games();
    let source_hash = snapshot.games[0].source_file_hash.clone();

    let destination_root = temp_root("staging-already-installed-dest");
    let destination_dir = destination_root.join("Atari2600");
    fs::create_dir_all(&destination_dir).unwrap();
    fs::write(
        destination_dir.join("Frogger.cht"),
        "cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n",
    )
    .unwrap();

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::AlreadyInstalled
    );
    assert_eq!(entry.staging_plan.existing_destination_hash, source_hash);
    assert!(entry.staging_candidate);
    assert!(!entry.destructive_if_applied);
    assert_eq!(report.summary.already_installed, 1);
}

#[test]
fn staging_strong_frogger_match_with_different_existing_file_is_replace_different() {
    let catalogue_root = temp_root("staging-replace-different-catalogue");
    let snapshot = frogger_cht_snapshot(&catalogue_root);
    let games = frogger_games();

    let destination_root = temp_root("staging-replace-different-dest");
    let destination_dir = destination_root.join("Atari2600");
    fs::create_dir_all(&destination_dir).unwrap();
    let original_bytes = "cheats = 1\ncheat0_desc = \"Different\"\n";
    fs::write(destination_dir.join("Frogger.cht"), original_bytes).unwrap();

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::ReplaceDifferent
    );
    assert!(entry.staging_candidate);
    assert!(entry.destructive_if_applied);
    assert_eq!(report.summary.staging_candidates, 1);
    assert_eq!(report.summary.conflicts, 0);
    // Preview only - the existing destination content is byte-for-byte
    // untouched, never overwritten.
    assert_eq!(
        fs::read_to_string(destination_dir.join("Frogger.cht")).unwrap(),
        original_bytes
    );
}

#[test]
fn staging_weak_title_only_match_is_not_eligible() {
    let catalogue_root = temp_root("staging-weak-catalogue");
    // Flat file directly under the root - no platform subdirectory means
    // no platform hint, so only the filename-only (weak) tier can match.
    fs::write(catalogue_root.join("Frogger.cht"), "cheats = 0\n").unwrap();
    let snapshot =
        load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &catalogue_root);
    let games = vec![evidence(51, "Frogger", Some("Atari2600"), None, None, None)];

    let destination_root = temp_root("staging-weak-dest");
    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(entry.game_match.confidence, CheatMatchConfidence::Weak);
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::NotEligible
    );
    assert_eq!(entry.staging_plan.reason, "weak_match_not_eligible");
    assert!(!entry.staging_candidate);
    assert!(entry.staging_plan.proposed_destination_path.is_none());
    assert_eq!(report.summary.staging_candidates, 0);
}

#[test]
fn staging_two_source_entries_resolving_to_same_destination_conflict() {
    let catalogue_root = temp_root("staging-duplicate-dest-catalogue");
    // Two different real source files, spelling the same platform two
    // different ways ("Atari - 2600" vs "Atari2600") - both canonicalize
    // to the identical `Atari2600` directory, so both propose the exact
    // same destination path for the same game name.
    fs::create_dir_all(catalogue_root.join("Atari - 2600")).unwrap();
    fs::create_dir_all(catalogue_root.join("Atari2600")).unwrap();
    fs::write(
        catalogue_root.join("Atari - 2600").join("Frogger.cht"),
        "cheats = 0\n",
    )
    .unwrap();
    fs::write(
        catalogue_root.join("Atari2600").join("Frogger.cht"),
        "cheats = 1\ncheat0_desc = \"Alternate\"\n",
    )
    .unwrap();
    let snapshot =
        load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &catalogue_root);
    assert_eq!(snapshot.games.len(), 2);
    let games = vec![evidence(52, "Frogger", Some("Atari2600"), None, None, None)];

    let destination_root = temp_root("staging-duplicate-dest-dest");
    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    assert_eq!(report.entries.len(), 2);
    assert!(
        report
            .entries
            .iter()
            .all(|entry| entry.staging_plan.planned_action == CheatStagingAction::Conflict)
    );
    assert!(
        report
            .entries
            .iter()
            .all(|entry| entry.staging_plan.reason == "duplicate_destination")
    );
    assert!(report.entries.iter().all(|entry| !entry.staging_candidate));
    assert_eq!(report.summary.conflicts, 2);
    assert_eq!(report.summary.staging_candidates, 0);
}

/// Builds a single-game manifest snapshot with an explicit, independently
/// controlled `game_name`/`platform`/`serial`, for the platform-safety
/// tests below - all matched via the exact-serial tier (1), which needs no
/// platform or title evidence at all, so an attacker-controlled
/// `game_name`/`platform` string can still reach destination resolution
/// even though it would never win a title/platform-based tier on its own.
fn serial_matched_snapshot(
    root: &Path,
    game_name: &str,
    platform: &str,
    serial: &str,
) -> CheatCatalogueSnapshot {
    let manifest_path = write_manifest(
        root,
        &format!(
            r#"{{
                "source_name": "Fixture",
                "games": [
                    {{
                        "game_name": {game_name},
                        "platform": {platform},
                        "serial": "{serial}",
                        "cheats": []
                    }}
                ]
            }}"#,
            game_name = serde_json::to_string(game_name).unwrap(),
            platform = serde_json::to_string(platform).unwrap(),
        ),
    );
    load_cheat_catalogue_snapshot(&HostReadOnlyFilesystem, "Fixture", &manifest_path)
}

#[test]
fn staging_traversal_style_game_name_is_rejected_with_valid_platform() {
    // Platform is a genuine canonical alias (so destination resolution
    // gets past the platform check) - only the game name is hostile, and
    // must still be rejected as an unsafe path component rather than ever
    // being joined onto the destination root.
    let catalogue_root = temp_root("staging-game-name-traversal-catalogue");
    let snapshot = serial_matched_snapshot(
        &catalogue_root,
        "../../../etc/passwd",
        "Atari - 2600",
        "CX2618-TRAVERSAL",
    );
    let games = vec![evidence(
        53,
        "Something Unrelated",
        None,
        None,
        Some("CX2618-TRAVERSAL"),
        None,
    )];

    let destination_root = temp_root("staging-game-name-traversal-dest");
    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(entry.game_match.confidence, CheatMatchConfidence::Exact);
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::NotEligible
    );
    assert_eq!(entry.staging_plan.reason, "destination_traversal_rejected");
    assert!(entry.staging_plan.proposed_destination_path.is_none());
    assert!(!entry.staging_candidate);
    // Nothing was created anywhere, including inside the destination root
    // itself.
    assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
}

#[test]
fn staging_unknown_platform_hint_is_not_eligible() {
    // A platform string that is perfectly safe as a path component but is
    // simply not a platform ArchiveFS's own alias table recognizes must
    // never be laundered into a trusted destination directory just
    // because it contains no unsafe characters. Confidence is `exact`
    // (matched via serial) precisely to prove the platform gate fires
    // independently of match confidence.
    let catalogue_root = temp_root("staging-unknown-platform-catalogue");
    let snapshot = serial_matched_snapshot(
        &catalogue_root,
        "Frogger",
        "TotallyMadeUpPlatformXYZ",
        "CX2618-UNKNOWN",
    );
    let games = vec![evidence(
        54,
        "Something Unrelated",
        None,
        None,
        Some("CX2618-UNKNOWN"),
        None,
    )];

    let destination_root = temp_root("staging-unknown-platform-dest");
    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(entry.game_match.confidence, CheatMatchConfidence::Exact);
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::NotEligible
    );
    assert_eq!(entry.staging_plan.reason, "source_platform_unresolved");
    assert!(entry.staging_plan.proposed_destination_path.is_none());
    assert!(!entry.staging_candidate);
    assert_eq!(report.summary.staging_candidates, 0);
    assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
}

#[test]
fn staging_traversal_style_platform_hint_is_not_eligible() {
    // A `../`-laden platform hint must be rejected at the canonicalization
    // gate - `source_platform_unresolved` - never reaching (and therefore
    // never needing to be caught by) the path-component sanitizer at all.
    let catalogue_root = temp_root("staging-traversal-platform-catalogue");
    let snapshot = serial_matched_snapshot(
        &catalogue_root,
        "Frogger",
        "../../escape",
        "CX2618-PLATTRAV",
    );
    let games = vec![evidence(
        55,
        "Something Unrelated",
        None,
        None,
        Some("CX2618-PLATTRAV"),
        None,
    )];

    let destination_root = temp_root("staging-traversal-platform-dest");
    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::NotEligible
    );
    assert_eq!(entry.staging_plan.reason, "source_platform_unresolved");
    assert!(entry.staging_plan.proposed_destination_path.is_none());
    assert!(!entry.staging_candidate);
    assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
}

#[test]
fn staging_separator_containing_platform_hint_is_not_eligible() {
    // An absolute-looking / separator-containing platform hint must also
    // be rejected at the canonicalization gate, not merely at the
    // sanitizer - it is not a recognized alias either.
    let catalogue_root = temp_root("staging-separator-platform-catalogue");
    let snapshot = serial_matched_snapshot(
        &catalogue_root,
        "Frogger",
        "/etc/passwd",
        "CX2618-SEPARATOR",
    );
    let games = vec![evidence(
        56,
        "Something Unrelated",
        None,
        None,
        Some("CX2618-SEPARATOR"),
        None,
    )];

    let destination_root = temp_root("staging-separator-platform-dest");
    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::NotEligible
    );
    assert_eq!(entry.staging_plan.reason, "source_platform_unresolved");
    assert!(entry.staging_plan.proposed_destination_path.is_none());
    assert!(!entry.staging_candidate);
    assert_eq!(fs::read_dir(&destination_root).unwrap().count(), 0);
}

#[test]
fn staging_canonical_atari_alias_remains_eligible() {
    // The positive case, confirmed end to end: a genuine canonical alias
    // ("Atari - 2600" -> "Atari2600") must still resolve to a real,
    // eligible destination - the platform gate above must reject only
    // unknown/unsafe hints, never a real one.
    let catalogue_root = temp_root("staging-canonical-alias-catalogue");
    let snapshot = frogger_cht_snapshot(&catalogue_root);
    let games = frogger_games();
    let destination_root = temp_root("staging-canonical-alias-dest");

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(entry.game_match.confidence, CheatMatchConfidence::Strong);
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::InstallNew
    );
    assert!(entry.staging_candidate);
    let destination = entry
        .staging_plan
        .proposed_destination_path
        .as_ref()
        .expect("destination resolved");
    assert!(destination.display.contains("Atari2600"));
    assert!(!destination.display.contains("Atari - 2600"));
}

#[cfg(unix)]
#[test]
fn staging_destination_symlink_escaping_root_is_rejected_without_reading_target() {
    let catalogue_root = temp_root("staging-symlink-catalogue");
    let snapshot = frogger_cht_snapshot(&catalogue_root);
    let games = frogger_games();

    let destination_root = temp_root("staging-symlink-dest");
    let outside = temp_root("staging-symlink-outside-target");
    // Deliberately identical content to the real source: if the symlink
    // were ever followed and hashed, this would wrongly report
    // `already_installed` instead of refusing to read it at all.
    fs::write(
        outside.join("secret.cht"),
        "cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n",
    )
    .unwrap();
    let destination_dir = destination_root.join("Atari2600");
    fs::create_dir_all(&destination_dir).unwrap();
    std::os::unix::fs::symlink(
        outside.join("secret.cht"),
        destination_dir.join("Frogger.cht"),
    )
    .unwrap();

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::Conflict
    );
    assert!(entry.staging_plan.existing_destination_hash.is_none());
    assert!(!entry.staging_candidate);
    // The external target file itself was never modified.
    assert_eq!(
        fs::read_to_string(outside.join("secret.cht")).unwrap(),
        "cheats = 1\ncheat0_desc = \"Infinite Lives\"\ncheat0_enable = false\n"
    );
}

#[test]
fn staging_absent_destination_root_reports_install_new_without_creating_it() {
    let catalogue_root = temp_root("staging-absent-root-catalogue");
    let snapshot = frogger_cht_snapshot(&catalogue_root);
    let games = frogger_games();

    let destination_root = temp_root("staging-absent-root-dest");
    fs::remove_dir_all(&destination_root).unwrap();
    assert!(!destination_root.exists());

    let report = build_cheat_availability_report(
        &HostReadOnlyFilesystem,
        &snapshot,
        &games,
        None,
        Some(&destination_root),
    );
    let entry = &report.entries[0];
    assert_eq!(
        entry.staging_plan.planned_action,
        CheatStagingAction::InstallNew
    );
    assert!(report.read_only);
    // Read-only preview never creates the missing root, its platform
    // subdirectory, or the file itself.
    assert!(!destination_root.exists());
}

#[test]
fn module_source_never_reads_home_or_xdg_env_directly() {
    // Structural guarantee: destination-root resolution must come only
    // from an explicitly-supplied `RetroArchAdvisoryPlan.environment` or
    // `destination_root_override` parameter, never by reading the
    // process's real `$HOME`/`$XDG_*` environment directly - see
    // `docs/RETROARCH_CHEAT_CATALOGUE.md` and
    // `emulator_environment::retroarch::DiscoveryEnvironment`'s own doc
    // comment on why tests must never depend on the real environment.
    let source = include_str!("../cheat_catalogue.rs");
    for forbidden in [
        "env::var(\"HOME\")",
        "env::var(\"XDG_",
        "std::env::home_dir",
    ] {
        assert!(
            !source.contains(forbidden),
            "cheat_catalogue.rs must never read {forbidden} directly"
        );
    }
}
