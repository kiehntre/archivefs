//! Behavioural tests for candidate loading, individual selection,
//! destination resolution, and staged generation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;
use crate::patch_manager::cheat_candidates::{
    CheatCandidate, CheatCandidateClassification, CheatCandidateEvidence,
};
use crate::patch_manager::cht_document::parse_cht_text;

const CANDIDATE_CHT: &str = "cheats = 3\n\
\n\
cheat0_desc = \"Infinite Health\"\n\
cheat0_code = \"NNVOSPVG\"\n\
cheat0_enable = false\n\
\n\
cheat1_desc = \"Infinite Lives\"\n\
cheat1_code = \"SZNKZOVK\"\n\
cheat1_enable = true\n\
\n\
cheat2_desc = \"Broken entry\"\n";

/// A self-cleaning temporary directory, in the same shape the other
/// filesystem-touching tests in this crate use.
struct Fixture {
    root: PathBuf,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl Fixture {
    fn new(label: &str) -> Self {
        let unique = format!(
            "archivefs-cheat-plan-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).expect("fixture root");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(&path, contents).expect("write fixture file");
        path
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(&path).expect("fixture dir");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn candidate(classification: CheatCandidateClassification) -> CheatCandidate {
    CheatCandidate {
        catalogue_relative_path: "NES/Game.cht".to_string(),
        display_name: "Game".to_string(),
        platform: Some("NES".to_string()),
        region: None,
        revision: None,
        classification,
        confidence_score: 700,
        evidence: Vec::<CheatCandidateEvidence>::new(),
        cheat_count: 3,
        source_file_hash: None,
        auto_selectable: false,
        manually_selectable: classification.is_installable(),
    }
}

fn destination_request(root: &Path) -> CheatDestinationRequest {
    CheatDestinationRequest {
        profile_cheat_root: root.to_path_buf(),
        platform: Some("Nintendo - Nintendo Entertainment System".to_string()),
        content_basename: Some("Chrono Quest (USA)".to_string()),
        playlist_name: None,
        catalogue_name: "Chrono Quest".to_string(),
    }
}

// -----------------------------------------------------------------------
// Loading a candidate
// -----------------------------------------------------------------------

#[test]
fn a_candidate_is_loaded_parsed_and_digest_checked() {
    let fixture = Fixture::new("load");
    let catalogue = fixture.dir("catalogue");
    fixture.write("catalogue/NES/Game.cht", CANDIDATE_CHT);

    let loaded = load_candidate_document(&catalogue, "NES/Game.cht", None).expect("loads");
    assert_eq!(loaded.document.entries.len(), 3);
    assert_eq!(loaded.digest.len(), 64);

    let again = load_candidate_document(&catalogue, "NES/Game.cht", Some(&loaded.digest))
        .expect("digest matches");
    assert_eq!(again.digest, loaded.digest);
}

#[test]
fn a_changed_candidate_is_rejected_rather_than_silently_reloaded() {
    let fixture = Fixture::new("digest");
    let catalogue = fixture.dir("catalogue");
    fixture.write("catalogue/Game.cht", CANDIDATE_CHT);
    let error = load_candidate_document(&catalogue, "Game.cht", Some(&"0".repeat(64)))
        .expect_err("digest mismatch is fatal");
    assert_eq!(
        error.kind,
        CheatInstallPlanErrorKind::CandidateDigestMismatch
    );
}

#[test]
fn a_traversing_candidate_path_is_rejected_before_any_file_is_opened() {
    let fixture = Fixture::new("traversal");
    let catalogue = fixture.dir("catalogue");
    fixture.write("secret.cht", CANDIDATE_CHT);
    for path in ["../secret.cht", "NES/../../secret.cht", "/etc/passwd", ""] {
        let error = load_candidate_document(&catalogue, path, None)
            .expect_err("a traversing or absolute candidate path must be rejected");
        assert_eq!(
            error.kind,
            CheatInstallPlanErrorKind::CandidatePathUnsafe,
            "{path} was rejected for the wrong reason"
        );
    }
}

#[test]
fn a_symlinked_candidate_is_never_followed() {
    let fixture = Fixture::new("symlink");
    let catalogue = fixture.dir("catalogue");
    let outside = fixture.write("outside.cht", CANDIDATE_CHT);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, catalogue.join("Game.cht")).expect("symlink");
    #[cfg(unix)]
    {
        let error =
            load_candidate_document(&catalogue, "Game.cht", None).expect_err("symlink rejected");
        assert_eq!(error.kind, CheatInstallPlanErrorKind::CandidatePathUnsafe);
    }
    let _ = outside;
}

#[test]
fn an_unsupported_encoding_is_reported_as_such() {
    let fixture = Fixture::new("encoding");
    let catalogue = fixture.dir("catalogue");
    fs::write(catalogue.join("Game.cht"), [0xFF, 0xFE, b'c', 0x00]).expect("write");
    let error = load_candidate_document(&catalogue, "Game.cht", None).expect_err("rejected");
    assert_eq!(
        error.kind,
        CheatInstallPlanErrorKind::CandidateUnsupportedEncoding
    );
}

#[test]
fn a_file_that_is_not_a_cheat_file_is_reported_as_malformed() {
    let fixture = Fixture::new("malformed");
    let catalogue = fixture.dir("catalogue");
    fixture.write("catalogue/Game.cht", "not a cheat file at all\n");
    let error = load_candidate_document(&catalogue, "Game.cht", None).expect_err("rejected");
    assert_eq!(error.kind, CheatInstallPlanErrorKind::CandidateMalformed);
}

// -----------------------------------------------------------------------
// Individual selection
// -----------------------------------------------------------------------

fn selection() -> (ChtDocument, CheatSelection) {
    let document = parse_cht_text(CANDIDATE_CHT).expect("parses");
    let selection = CheatSelection::from_document(&document);
    (document, selection)
}

#[test]
fn nothing_is_selected_by_default_and_apply_is_blocked() {
    let (_, selection) = selection();
    assert_eq!(selection.entries.len(), 3);
    assert_eq!(selection.selected_count(), 0);
    assert!(
        !selection.can_apply(),
        "zero selected cheats must block Apply"
    );
}

#[test]
fn the_source_enabled_default_is_preserved_per_entry() {
    let (_, selection) = selection();
    assert!(!selection.entries[0].enabled, "cheat0_enable = false");
    assert!(selection.entries[1].enabled, "cheat1_enable = true");
}

#[test]
fn an_unsafe_entry_can_never_be_selected() {
    let (_, mut selection) = selection();
    let broken = &selection.entries[2];
    assert!(!broken.selectable, "cheat2 has no code");
    assert!(broken.has_blocking_warning);
    assert!(!selection.set_selected(2, true), "the toggle is refused");
    assert_eq!(selection.selected_count(), 0);
}

#[test]
fn select_all_selects_only_the_safe_entries() {
    let (_, mut selection) = selection();
    selection.select_all();
    assert_eq!(selection.selected_count(), 2);
    assert_eq!(selection.selectable_count(), 2);
    assert_eq!(selection.blocked_count(), 1);
    assert!(selection.can_apply());
}

#[test]
fn clear_all_deselects_everything() {
    let (_, mut selection) = selection();
    selection.select_all();
    selection.clear_all();
    assert_eq!(selection.selected_count(), 0);
    assert!(!selection.can_apply());
}

#[test]
fn selection_preserves_catalogue_order() {
    let (_, selection) = selection();
    assert_eq!(
        selection
            .entries
            .iter()
            .map(|entry| entry.description.as_str())
            .collect::<Vec<_>>(),
        vec!["Infinite Health", "Infinite Lives", "Broken entry"]
    );
}

#[test]
fn resolving_an_empty_selection_is_an_error_not_an_empty_file() {
    let (document, selection) = selection();
    let error = selection.resolve(&document).expect_err("blocked");
    assert_eq!(error.kind, CheatInstallPlanErrorKind::NoSelectedCheats);
}

#[test]
fn resolving_returns_only_the_selected_entries_in_order() {
    let (document, mut selection) = selection();
    assert!(selection.set_selected(1, true));
    assert!(selection.set_selected(0, true));
    let resolved = selection.resolve(&document).expect("resolves");
    assert_eq!(
        resolved
            .iter()
            .map(|entry| entry.description.as_str())
            .collect::<Vec<_>>(),
        vec!["Infinite Health", "Infinite Lives"],
        "catalogue order, not click order"
    );
}

#[test]
fn included_and_enabled_are_separate_decisions() {
    let (document, mut selection) = selection();
    assert!(selection.set_selected(1, true));
    assert!(selection.set_enabled(1, false));
    let resolved = selection.resolve(&document).expect("resolves");
    assert_eq!(resolved.len(), 1);
    assert!(
        !resolved[0].enabled,
        "a cheat can be installed without being active"
    );
}

// -----------------------------------------------------------------------
// Destination resolution
// -----------------------------------------------------------------------

#[test]
fn the_destination_uses_the_profile_root_platform_directory_and_content_name() {
    let fixture = Fixture::new("dest");
    let root = fixture.dir("cheats");
    let resolved = resolve_cheat_destination(&destination_request(&root)).expect("resolves");
    assert_eq!(resolved.platform_directory, "NES");
    assert_eq!(resolved.file_name, "Chrono Quest (USA).cht");
    assert_eq!(
        resolved.path,
        root.join("NES").join("Chrono Quest (USA).cht")
    );
    assert_eq!(
        resolved.name_source,
        CheatDestinationNameSource::ContentBasename
    );
    assert!(!resolved.replaces_existing);
}

#[test]
fn the_name_falls_back_through_playlist_then_catalogue_identity() {
    let fixture = Fixture::new("names");
    let root = fixture.dir("cheats");

    let mut request = destination_request(&root);
    request.content_basename = None;
    request.playlist_name = Some("Chrono Quest (Europe)".to_string());
    let resolved = resolve_cheat_destination(&request).expect("resolves");
    assert_eq!(
        resolved.name_source,
        CheatDestinationNameSource::PlaylistName
    );
    assert_eq!(resolved.file_name, "Chrono Quest (Europe).cht");

    request.playlist_name = None;
    let resolved = resolve_cheat_destination(&request).expect("resolves");
    assert_eq!(
        resolved.name_source,
        CheatDestinationNameSource::CatalogueName
    );
    assert_eq!(resolved.file_name, "Chrono Quest.cht");
}

#[test]
fn an_unsafe_name_is_skipped_rather_than_sanitized_into_a_new_path() {
    let fixture = Fixture::new("unsafe-name");
    let root = fixture.dir("cheats");
    let mut request = destination_request(&root);
    request.content_basename = Some("../../etc/passwd".to_string());
    let resolved = resolve_cheat_destination(&request).expect("falls back");
    assert_eq!(
        resolved.name_source,
        CheatDestinationNameSource::CatalogueName,
        "a traversing name is never laundered into a safe-looking one"
    );
    assert_eq!(resolved.file_name, "Chrono Quest.cht");
    assert!(resolved.path.starts_with(&root));
}

#[test]
fn an_unrecognized_platform_blocks_installation_rather_than_guessing_a_directory() {
    let fixture = Fixture::new("platform");
    let root = fixture.dir("cheats");
    let mut request = destination_request(&root);
    request.platform = Some("Some Machine Nobody Knows".to_string());
    let error = resolve_cheat_destination(&request).expect_err("blocked");
    assert_eq!(
        error.kind,
        CheatInstallPlanErrorKind::DestinationPlatformUnresolved
    );
}

#[test]
fn a_missing_platform_blocks_installation() {
    let fixture = Fixture::new("no-platform");
    let root = fixture.dir("cheats");
    let mut request = destination_request(&root);
    request.platform = None;
    assert_eq!(
        resolve_cheat_destination(&request)
            .expect_err("blocked")
            .kind,
        CheatInstallPlanErrorKind::DestinationPlatformUnresolved
    );
}

#[test]
fn a_relative_profile_root_is_rejected() {
    let mut request = destination_request(Path::new("relative/cheats"));
    request.platform = Some("NES".to_string());
    assert_eq!(
        resolve_cheat_destination(&request)
            .expect_err("blocked")
            .kind,
        CheatInstallPlanErrorKind::DestinationRootUnavailable
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_platform_directory_that_escapes_the_root_is_rejected() {
    let fixture = Fixture::new("escape");
    let root = fixture.dir("cheats");
    let outside = fixture.dir("outside");
    std::os::unix::fs::symlink(&outside, root.join("NES")).expect("symlink");
    let error = resolve_cheat_destination(&destination_request(&root)).expect_err("blocked");
    assert_eq!(error.kind, CheatInstallPlanErrorKind::DestinationUnsafe);
}

#[cfg(unix)]
#[test]
fn a_symlinked_destination_file_is_rejected() {
    let fixture = Fixture::new("file-symlink");
    let root = fixture.dir("cheats");
    fixture.dir("cheats/NES");
    let outside = fixture.write("outside.cht", CANDIDATE_CHT);
    std::os::unix::fs::symlink(&outside, root.join("NES").join("Chrono Quest (USA).cht"))
        .expect("symlink");
    let error = resolve_cheat_destination(&destination_request(&root)).expect_err("blocked");
    assert_eq!(error.kind, CheatInstallPlanErrorKind::DestinationUnsafe);
}

#[test]
fn an_existing_destination_file_is_reported_as_a_replacement() {
    let fixture = Fixture::new("replace");
    let root = fixture.dir("cheats");
    fixture.write("cheats/NES/Chrono Quest (USA).cht", "cheats = 0\n");
    let resolved = resolve_cheat_destination(&destination_request(&root)).expect("resolves");
    assert!(resolved.replaces_existing);
    assert_eq!(resolved.state, DestinationState::RegularFile);
}

// -----------------------------------------------------------------------
// Staging and preview
// -----------------------------------------------------------------------

#[test]
fn staging_writes_a_deterministic_file_and_reports_its_digest() {
    let fixture = Fixture::new("stage");
    let staging = fixture.path("staging");
    let (document, mut selection) = selection();
    selection.select_all();
    let entries = selection.resolve(&document).expect("resolves");

    let staged =
        stage_generated_cheat_file(&staging, "Chrono Quest", &entries, &[]).expect("stages");
    assert_eq!(staged.selected_cheat_count, 2);
    assert_eq!(staged.enabled_cheat_count, 1);
    assert!(staged.path.starts_with(&staging));
    let on_disk = fs::read_to_string(&staged.path).expect("staged file exists");
    assert_eq!(on_disk, staged.contents);
    assert!(on_disk.starts_with(&format!("# {GENERATED_FILE_PROVENANCE}\n")));
    assert!(on_disk.contains("cheats = 2"));
    assert!(on_disk.ends_with('\n'));

    let second = Fixture::new("stage2");
    let again = stage_generated_cheat_file(&second.path("staging"), "Chrono Quest", &entries, &[])
        .expect("stages");
    assert_eq!(
        again.digest, staged.digest,
        "the same selection always produces the same bytes"
    );
}

#[test]
fn staging_refuses_an_empty_selection() {
    let fixture = Fixture::new("stage-empty");
    let error = stage_generated_cheat_file(&fixture.path("staging"), "Game", &[], &[])
        .expect_err("blocked");
    assert_eq!(error.kind, CheatInstallPlanErrorKind::NoSelectedCheats);
}

#[test]
fn staging_never_writes_outside_its_own_root() {
    let fixture = Fixture::new("stage-traversal");
    let staging = fixture.path("staging");
    let (document, mut selection) = selection();
    selection.select_all();
    let entries = selection.resolve(&document).expect("resolves");
    let error =
        stage_generated_cheat_file(&staging, "../escape", &entries, &[]).expect_err("blocked");
    assert_eq!(
        error.kind,
        CheatInstallPlanErrorKind::DestinationNameUnresolved
    );
}

#[test]
fn the_trusted_catalogue_file_is_never_modified_by_an_install() {
    let fixture = Fixture::new("catalogue-immutable");
    let catalogue = fixture.dir("catalogue");
    let source = fixture.write("catalogue/Game.cht", CANDIDATE_CHT);
    let before = fs::read(&source).expect("read");

    let loaded = load_candidate_document(&catalogue, "Game.cht", None).expect("loads");
    let mut selection = CheatSelection::from_document(&loaded.document);
    selection.select_all();
    let entries = selection.resolve(&loaded.document).expect("resolves");
    stage_generated_cheat_file(&fixture.path("staging"), "Game", &entries, &[]).expect("stages");

    assert_eq!(
        fs::read(&source).expect("read"),
        before,
        "the catalogue is read-only on the install path"
    );
}

#[test]
fn a_preview_names_the_resolved_destination_and_the_staged_source() {
    let fixture = Fixture::new("preview");
    let root = fixture.dir("cheats");
    let destination = resolve_cheat_destination(&destination_request(&root)).expect("resolves");
    let (document, mut selection) = selection();
    selection.select_all();
    let entries = selection.resolve(&document).expect("resolves");
    let staged =
        stage_generated_cheat_file(&fixture.path("staging"), "Chrono Quest", &entries, &[])
            .expect("stages");

    let preview = build_cheat_install_preview(&CheatInstallPreviewRequest {
        selected_archive: fixture.write("Chrono Quest.zip", "x"),
        platform: Some("NES".to_string()),
        verified_identity: "test-identity".to_string(),
        destination: destination.clone(),
        profile_cheat_root: root.clone(),
        staged: staged.clone(),
        match_strength: PreviewMatchStrength::Strong,
    })
    .expect("preview builds");

    assert_eq!(preview.report.entries.len(), 1);
    let entry = &preview.report.entries[0];
    assert_eq!(entry.destination_root, root);
    assert_eq!(entry.source_path, Some(staged.path));
    assert_eq!(preview.destination.path, destination.path);
}

// -----------------------------------------------------------------------
// Classification gating
// -----------------------------------------------------------------------

#[test]
fn an_installable_candidate_maps_to_a_preview_strength() {
    assert_eq!(
        match_strength_for_candidate(&candidate(CheatCandidateClassification::VerifiedExact))
            .expect("allowed"),
        PreviewMatchStrength::VerifiedExact
    );
    assert_eq!(
        match_strength_for_candidate(&candidate(CheatCandidateClassification::Strong))
            .expect("allowed"),
        PreviewMatchStrength::Strong
    );
    assert_eq!(
        match_strength_for_candidate(&candidate(CheatCandidateClassification::Ambiguous))
            .expect("allowed after an explicit choice"),
        PreviewMatchStrength::Ambiguous
    );
}

#[test]
fn a_cross_platform_or_unsupported_candidate_can_never_reach_a_preview() {
    for classification in [
        CheatCandidateClassification::CrossPlatform,
        CheatCandidateClassification::Unsupported,
    ] {
        let error =
            match_strength_for_candidate(&candidate(classification)).expect_err("must be refused");
        assert_eq!(
            error.kind,
            CheatInstallPlanErrorKind::CandidateNotInstallable
        );
    }
}

// -----------------------------------------------------------------------
// Real-world RetroArch layout
// -----------------------------------------------------------------------

#[test]
fn an_existing_libretro_database_directory_is_used_instead_of_a_new_one() {
    // A real RetroArch cheat root is laid out by libretro database name,
    // not by this project's canonical short name. Installing into a second
    // directory for the same system would put the file somewhere the user
    // does not already look.
    let fixture = Fixture::new("libretro-layout");
    let root = fixture.dir("cheats");
    fixture.dir("cheats/Nintendo - Nintendo Entertainment System");
    fixture.dir("cheats/Sega - Mega Drive - Genesis");

    let resolved = resolve_cheat_destination(&destination_request(&root)).expect("resolves");
    assert_eq!(
        resolved.platform_directory,
        "Nintendo - Nintendo Entertainment System"
    );
    assert_eq!(
        resolved.platform_directory_source,
        CheatPlatformDirectorySource::ExistingProfileDirectory
    );
    assert_eq!(
        resolved.path,
        root.join("Nintendo - Nintendo Entertainment System")
            .join("Chrono Quest (USA).cht")
    );
}

#[test]
fn a_cheat_root_with_no_matching_directory_falls_back_to_the_canonical_name() {
    let fixture = Fixture::new("no-libretro-dir");
    let root = fixture.dir("cheats");
    fixture.dir("cheats/Sega - Mega Drive - Genesis");

    let resolved = resolve_cheat_destination(&destination_request(&root)).expect("resolves");
    assert_eq!(resolved.platform_directory, "NES");
    assert_eq!(
        resolved.platform_directory_source,
        CheatPlatformDirectorySource::CanonicalPlatformName
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_platform_directory_is_never_chosen_as_an_existing_one() {
    let fixture = Fixture::new("symlinked-platform-dir");
    let root = fixture.dir("cheats");
    let outside = fixture.dir("outside");
    std::os::unix::fs::symlink(
        &outside,
        root.join("Nintendo - Nintendo Entertainment System"),
    )
    .expect("symlink");

    let resolved = resolve_cheat_destination(&destination_request(&root)).expect("resolves");
    assert_eq!(
        resolved.platform_directory_source,
        CheatPlatformDirectorySource::CanonicalPlatformName,
        "a symlinked directory is skipped rather than followed"
    );
}
