//! Configuration dialog, mappings editor and preview tests.
//!
//! Assertions are on the validation and view models. The point of building those as
//! pure functions is that "a public URL is refused", "the token is never rendered"
//! and "a stranded mapping refuses the save" become questions about data.
//!
//! Two tests render headlessly, because "the token file's contents are not drawn"
//! is a claim about drawing.

use super::*;
use archivefs_core::identity_source::artwork::ArtworkCacheStats;
use archivefs_core::identity_source::model::IdentityProvider;
use archivefs_core::identity_source::romm::config::RommSourceConfig;
use archivefs_core::identity_source::status::ProviderStatus;
use std::fs;

/// A value that must never appear in any field, label or drawn glyph.
const SECRET: &str = "romm-token-contents-must-never-be-read-into-the-gui";

// --- Fixtures -------------------------------------------------------------

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-romm-config-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("library")).expect("fixture");
        Self { root }
    }

    fn library(&self) -> PathBuf {
        self.root.join("library")
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![self.library()]
    }

    /// A token file with the given contents and mode.
    fn token(&self, contents: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.root.join("token");
        fs::write(&path, contents).expect("fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("fixture");
        path
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn snapshot(configured: bool, kind: ProviderPathKind, mappings: Vec<PathMapping>) -> RommSnapshot {
    RommSnapshot {
        settings: ProviderSettings {
            source: RommSourceConfig {
                enabled: true,
                url: if configured {
                    "http://172.19.0.20:8080".to_string()
                } else {
                    String::new()
                },
                mappings,
                provider_path_kind: kind,
                token_path: configured
                    .then(|| PathBuf::from("/home/user/.config/archivefs/romm-token")),
            },
            page_size: Some(100),
        },
        status: ProviderStatus::not_configured(IdentityProvider::Romm),
        artwork: ArtworkCacheStats {
            items: 0,
            bytes: 0,
            maximum_bytes: 1024 * 1024 * 1024,
            last_cleanup_unix_seconds: None,
            directory: PathBuf::from("/tmp/artwork"),
            format_version: 1,
        },
        token_available: configured,
        token_problem: None,
        cache_format_version: None,
    }
}

fn mapping(prefix: &str, destination: &Path) -> PathMapping {
    PathMapping {
        provider_prefix: prefix.to_string(),
        archivefs_prefix: destination.to_path_buf(),
    }
}

fn token_verdict(path: &Path) -> FieldState {
    token_field_state(
        archivefs_core::identity_source::settings::load_token_file(Some(path)).map(|_| ()),
        &path.display().to_string(),
    )
}

// --- Opening the dialog ---------------------------------------------------

#[test]
fn opening_on_an_existing_configuration_shows_what_is_stored() {
    let tree = Tree::new("open-existing");
    let snapshot = snapshot(
        true,
        ProviderPathKind::ProviderRelative,
        vec![mapping("roms", &tree.library())],
    );
    let draft = RommConfigDraft::from_snapshot(&snapshot);
    assert_eq!(draft.url, "http://172.19.0.20:8080");
    assert_eq!(
        draft.token_path, "/home/user/.config/archivefs/romm-token",
        "the path, and only the path"
    );
    assert_eq!(draft.path_kind, ProviderPathKind::ProviderRelative);
    assert_eq!(draft.page_size, "100");
    assert_eq!(draft.mappings.len(), 1);
    assert!(!draft.dirty, "opening is not an edit");
    assert_eq!(draft.preview_limit, DEFAULT_PREVIEW_LIMIT.to_string());
}

#[test]
fn opening_a_source_that_was_never_configured_starts_blank_but_usable() {
    let draft = RommConfigDraft::blank();
    assert!(draft.url.is_empty());
    assert!(draft.token_path.is_empty());
    assert!(draft.mappings.is_empty());
    // A sensible default rather than an empty box that reads as "no limit".
    assert_eq!(draft.page_size, "100");
    assert_eq!(
        draft.path_kind,
        ProviderPathKind::AbsoluteProviderPath,
        "the historical default, which is what a config without the field means"
    );
}

// --- URL validation -------------------------------------------------------

#[test]
fn a_literal_private_address_is_fully_validated_without_any_network() {
    // A literal address needs no resolution, so the whole local-only policy applies
    // as it is typed.
    let state = validate_url_text("http://172.19.0.20:8080");
    assert!(matches!(state, FieldState::Good(_)), "{state:?}");
    assert!(
        state.message().contains("172.19.0.20"),
        "{}",
        state.message()
    );
    assert!(
        state.message().contains("local address"),
        "{}",
        state.message()
    );
}

#[test]
fn a_public_address_is_refused_as_it_is_typed() {
    let state = validate_url_text("http://93.184.216.34:8080");
    assert!(state.is_problem(), "{state:?}");
    assert!(
        state
            .message()
            .contains("not on a local or private network"),
        "{}",
        state.message()
    );
}

#[test]
fn embedded_credentials_are_refused() {
    let state = validate_url_text("http://user:secret@172.19.0.20:8080");
    assert!(state.is_problem());
    assert!(
        state.message().contains("username or password"),
        "{}",
        state.message()
    );
    assert!(
        !state.message().contains("secret"),
        "the refusal must not echo the credential: {}",
        state.message()
    );
}

#[test]
fn a_malformed_url_and_an_unsupported_scheme_are_both_refused() {
    for text in ["not a url at all", "file:///etc/passwd", "unix:/var/run/x"] {
        let state = validate_url_text(text);
        assert!(state.is_problem(), "{text} produced {state:?}");
    }
}

#[test]
fn an_empty_url_is_not_yet_a_problem_but_cannot_be_saved() {
    let state = validate_url_text("   ");
    assert!(matches!(state, FieldState::Empty(_)), "{state:?}");
    assert!(
        !state.is_problem(),
        "an empty field is not an error to shout about"
    );
    assert!(
        state.message().contains("172.19.0.20"),
        "it should show the shape wanted"
    );
}

#[test]
fn a_hostname_defers_to_the_save_rather_than_guessing() {
    // Resolving is I/O, so it does not happen while typing. The field says so
    // instead of pretending to know.
    let state = validate_url_text("http://romm.local:8080");
    assert!(matches!(state, FieldState::Deferred(_)), "{state:?}");
    assert!(
        state.message().contains("checked when you save"),
        "{}",
        state.message()
    );
    assert!(
        state.message().contains("no request is made"),
        "and it should say nothing is contacted: {}",
        state.message()
    );
    assert!(
        !state.is_problem(),
        "a hostname must not block the save button"
    );
}

#[test]
fn a_metadata_endpoint_is_refused() {
    let state = validate_url_text("http://169.254.169.254:80");
    assert!(state.is_problem(), "{state:?}");
}

// --- Page size ------------------------------------------------------------

#[test]
fn the_page_size_is_bounded_and_explains_why() {
    assert!(matches!(validate_page_size("100"), FieldState::Good(_)));
    assert!(matches!(
        validate_page_size(&MIN_CONFIGURED_PAGE_SIZE.to_string()),
        FieldState::Good(_)
    ));
    assert!(matches!(
        validate_page_size(&MAX_CONFIGURED_PAGE_SIZE.to_string()),
        FieldState::Good(_)
    ));
    let too_big = validate_page_size(&(MAX_CONFIGURED_PAGE_SIZE + 1).to_string());
    assert!(too_big.is_problem());
    assert!(
        too_big.message().contains("size ceiling"),
        "the reason a large page matters should be given: {}",
        too_big.message()
    );
    assert!(validate_page_size("1").is_problem());
    assert!(validate_page_size("many").is_problem());
    assert!(matches!(validate_page_size(""), FieldState::Empty(_)));
}

// --- Token file -----------------------------------------------------------

#[test]
fn a_usable_token_file_is_described_by_its_path_only() {
    let tree = Tree::new("token-good");
    let path = tree.token(SECRET, 0o600);
    let state = token_verdict(&path);
    assert!(matches!(state, FieldState::Good(_)), "{state:?}");
    assert!(state.message().contains(&path.display().to_string()));
    assert!(
        !state.message().contains(SECRET),
        "the contents must never appear: {}",
        state.message()
    );
}

#[test]
fn every_token_problem_is_reported_with_its_remedy_and_no_contents() {
    let tree = Tree::new("token-problems");

    // Permissions too open.
    let open = tree.token(SECRET, 0o644);
    let state = token_verdict(&open);
    assert!(state.is_problem());
    assert!(state.message().contains("chmod 600"), "{}", state.message());
    assert!(!state.message().contains(SECRET));

    // Empty.
    let empty = tree.token("", 0o600);
    let state = token_verdict(&empty);
    assert!(state.is_problem());
    assert!(
        state.message().contains("usable token"),
        "{}",
        state.message()
    );

    // Missing.
    let missing = tree.root.join("no-such-token");
    let state = token_verdict(&missing);
    assert!(state.is_problem());
    assert!(
        state.message().contains("does not exist"),
        "{}",
        state.message()
    );

    // A symlink, refused because a credential's location must be where it was said.
    let real = tree.token(SECRET, 0o600);
    let link = tree.root.join("token-link");
    std::os::unix::fs::symlink(&real, &link).expect("fixture");
    let state = token_verdict(&link);
    assert!(state.is_problem());
    assert!(state.message().contains("symlink"), "{}", state.message());
    assert!(!state.message().contains(SECRET));

    // A directory.
    let state = token_verdict(&tree.library());
    assert!(state.is_problem());
    assert!(
        state.message().contains("regular file"),
        "{}",
        state.message()
    );
}

#[test]
fn no_token_file_yet_offers_the_suggested_location() {
    let draft = RommConfigDraft::blank();
    let validation = validate_draft(&draft, None, &[]);
    assert!(matches!(validation.token, FieldState::Empty(_)));
    assert!(
        validation.token.message().contains(SUGGESTED_TOKEN_PATH),
        "{}",
        validation.token.message()
    );
    // And the copyable example creates it privately in one step.
    assert!(TOKEN_FILE_SHELL_EXAMPLE.contains("install -m 600 /dev/null"));
    assert!(TOKEN_FILE_SHELL_EXAMPLE.contains(SUGGESTED_TOKEN_PATH.trim_start_matches("~/")));
}

// --- Saving ---------------------------------------------------------------

#[test]
fn a_complete_valid_draft_can_be_saved() {
    let tree = Tree::new("save-valid");
    let path = tree.token(SECRET, 0o600);
    let draft = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        token_path: path.display().to_string(),
        path_kind: ProviderPathKind::ProviderRelative,
        page_size: "100".to_string(),
        mappings: vec![mapping("roms", &tree.library())],
        ..RommConfigDraft::blank()
    };
    let validation = validate_draft(&draft, Some(&token_verdict(&path)), &tree.roots());
    assert!(validation.can_save, "{validation:?}");
    let settings = draft.to_settings(None);
    assert_eq!(settings.source.url, "http://172.19.0.20:8080");
    assert_eq!(settings.source.token_path, Some(path));
    assert_eq!(settings.page_size, Some(100));
    assert_eq!(settings.source.mappings.len(), 1);
}

#[test]
fn any_field_problem_refuses_the_save() {
    let tree = Tree::new("save-refused");
    let path = tree.token(SECRET, 0o600);
    let good = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        token_path: path.display().to_string(),
        page_size: "100".to_string(),
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        ..RommConfigDraft::blank()
    };
    assert!(validate_draft(&good, Some(&token_verdict(&path)), &tree.roots()).can_save);

    // A public URL.
    let mut draft = good.clone();
    draft.url = "http://93.184.216.34:8080".to_string();
    assert!(!validate_draft(&draft, Some(&token_verdict(&path)), &tree.roots()).can_save);

    // An out-of-range page size.
    let mut draft = good.clone();
    draft.page_size = "9999".to_string();
    assert!(!validate_draft(&draft, Some(&token_verdict(&path)), &tree.roots()).can_save);

    // An unusable token file.
    let open = tree.token(SECRET, 0o644);
    let mut draft = good.clone();
    draft.token_path = open.display().to_string();
    assert!(!validate_draft(&draft, Some(&token_verdict(&open)), &tree.roots()).can_save);
}

#[test]
fn a_path_kind_that_would_strand_a_mapping_refuses_the_save_and_names_it() {
    let tree = Tree::new("strand");
    let path = tree.token(SECRET, 0o600);
    // An absolute prefix, with the source set to relative.
    let draft = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        token_path: path.display().to_string(),
        page_size: "100".to_string(),
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("/romm/library", &tree.library())],
        ..RommConfigDraft::blank()
    };
    let validation = validate_draft(&draft, Some(&token_verdict(&path)), &tree.roots());
    assert!(
        !validation.can_save,
        "a mapping that can never match must block the save"
    );
    assert_eq!(
        validation.stranded_mappings,
        vec!["/romm/library".to_string()],
        "and the offending prefix must be named so it can be removed"
    );

    // Switching the kind back makes it savable again.
    let mut absolute = draft.clone();
    absolute.path_kind = ProviderPathKind::AbsoluteProviderPath;
    let validation = validate_draft(&absolute, Some(&token_verdict(&path)), &tree.roots());
    assert!(validation.stranded_mappings.is_empty());
    assert!(validation.can_save);
}

#[test]
fn a_mapping_set_that_cannot_be_used_together_refuses_the_save() {
    let tree = Tree::new("set-conflict");
    let path = tree.token(SECRET, 0o600);
    // Two prefixes landing on one directory: which describes a file would depend on
    // ordering, so the set is refused.
    let draft = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        token_path: path.display().to_string(),
        page_size: "100".to_string(),
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![
            mapping("roms", &tree.library()),
            mapping("other", &tree.library()),
        ],
        ..RommConfigDraft::blank()
    };
    assert!(!validate_draft(&draft, Some(&token_verdict(&path)), &tree.roots()).can_save);
}

#[test]
fn a_page_size_that_was_never_set_stays_unset_when_it_is_not_changed() {
    // The field is prefilled with the effective size, so saving without touching it
    // must not turn "use the default" into "explicitly 100" - a no-change save has
    // to leave the file alone.
    let previous = ProviderSettings {
        source: RommSourceConfig {
            enabled: true,
            url: "http://172.19.0.20:8080".to_string(),
            mappings: Vec::new(),
            provider_path_kind: ProviderPathKind::ProviderRelative,
            token_path: None,
        },
        page_size: None,
    };
    let draft = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        page_size: previous.effective_page_size().to_string(),
        path_kind: ProviderPathKind::ProviderRelative,
        ..RommConfigDraft::blank()
    };
    assert_eq!(
        draft.to_settings(Some(&previous)).page_size,
        None,
        "an untouched default must stay unset"
    );

    // Actually changing it does write it.
    let mut changed = draft.clone();
    changed.page_size = "50".to_string();
    assert_eq!(changed.to_settings(Some(&previous)).page_size, Some(50));

    // And a size that was already explicit stays explicit.
    let mut explicit = previous.clone();
    explicit.page_size = Some(100);
    assert_eq!(draft.to_settings(Some(&explicit)).page_size, Some(100));
}

#[test]
fn to_settings_keeps_fields_the_dialog_does_not_edit() {
    let tree = Tree::new("preserve");
    let previous = ProviderSettings {
        source: RommSourceConfig {
            // Enabled is not a field of this dialog, so it must survive a save.
            enabled: true,
            url: "http://old:8080".to_string(),
            mappings: Vec::new(),
            provider_path_kind: ProviderPathKind::AbsoluteProviderPath,
            token_path: None,
        },
        page_size: Some(50),
    };
    let draft = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        token_path: String::new(),
        path_kind: ProviderPathKind::ProviderRelative,
        page_size: "100".to_string(),
        mappings: vec![mapping("roms", &tree.library())],
        ..RommConfigDraft::blank()
    };
    let saved = draft.to_settings(Some(&previous));
    assert!(saved.source.enabled, "enabled must not be silently cleared");
    assert_eq!(saved.source.url, "http://172.19.0.20:8080");
    assert_eq!(
        saved.source.provider_path_kind,
        ProviderPathKind::ProviderRelative
    );
    assert_eq!(saved.page_size, Some(100));
    assert_eq!(saved.source.token_path, None, "an emptied path clears it");
}

// --- Mappings editor ------------------------------------------------------

#[test]
fn mappings_are_listed_longest_prefix_first() {
    let tree = Tree::new("order");
    let narrow = tree.library().join("st");
    let broad = tree.library().join("all");
    fs::create_dir_all(&narrow).expect("fixture");
    fs::create_dir_all(&broad).expect("fixture");
    let view = build_mappings_view(
        &[mapping("roms", &broad), mapping("roms/atari-st", &narrow)],
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    let prefixes: Vec<&str> = view
        .rows
        .iter()
        .map(|row| row.provider_prefix.as_str())
        .collect();
    assert_eq!(
        prefixes,
        vec!["roms/atari-st", "roms"],
        "the more specific rule is applied first, so it is listed first"
    );
    assert!(view.rows.iter().all(|row| row.valid));
    assert!(view.rows.iter().all(|row| row.inside_source_root));
    assert!(view.set_problem.is_none());
    assert!(!view.no_usable_mapping);
}

#[test]
fn a_row_reports_its_kind_destination_and_source_root_membership() {
    let tree = Tree::new("row");
    let view = build_mappings_view(
        &[mapping("roms", &tree.library())],
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    let row = &view.rows[0];
    assert_eq!(row.provider_prefix, "roms");
    assert_eq!(row.normalised_prefix.as_deref(), Some("roms"));
    assert_eq!(row.path_kind, ProviderPathKind::ProviderRelative);
    assert_eq!(row.destination, tree.library());
    assert!(row.inside_source_root);
    assert!(row.valid);
    assert!(row.problem.is_none());
}

#[test]
fn a_destination_outside_the_source_roots_is_shown_as_invalid_with_its_reason() {
    let tree = Tree::new("outside");
    let outside = tree.root.join("elsewhere");
    fs::create_dir_all(&outside).expect("fixture");
    let view = build_mappings_view(
        &[mapping("roms", &outside)],
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    let row = &view.rows[0];
    assert!(!row.valid);
    assert!(!row.inside_source_root);
    assert!(
        row.problem
            .as_deref()
            .is_some_and(|problem| problem.contains("not inside any configured source folder")),
        "{:?}",
        row.problem
    );
    assert!(view.no_usable_mapping);
}

#[test]
fn a_mapping_stranded_by_the_path_kind_is_still_listed_and_still_removable() {
    let tree = Tree::new("stranded-row");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("/romm/library", &tree.library())],
        ..RommConfigDraft::blank()
    };
    // Listing must not fail, or a bad state would have no way out.
    let view = build_mappings_view(&draft.mappings, draft.path_kind, &tree.roots());
    assert_eq!(view.rows.len(), 1);
    assert!(!view.rows[0].valid);
    assert!(view.rows[0].normalised_prefix.is_none());
    assert!(
        view.rows[0]
            .problem
            .as_deref()
            .is_some_and(|problem| problem.contains("absolute")),
        "{:?}",
        view.rows[0].problem
    );
    // And removal works by the text as typed, since it cannot be normalised.
    assert!(remove_mapping(&mut draft, "/romm/library"));
    assert!(draft.mappings.is_empty());
    assert!(draft.dirty);
}

#[test]
fn adding_a_mapping_validates_the_prefix_against_the_configured_shape() {
    let tree = Tree::new("add-shape");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        new_prefix: "/romm/library".to_string(),
        new_destination: tree.library().display().to_string(),
        ..RommConfigDraft::blank()
    };
    match add_mapping(&mut draft, &tree.roots(), false) {
        AddMappingOutcome::Refused(reason) => {
            assert!(reason.contains("relative"), "{reason}");
        }
        other => panic!("an absolute prefix must be refused in relative mode: {other:?}"),
    }
    assert!(draft.mappings.is_empty());
}

#[test]
fn every_hostile_prefix_shape_is_refused_when_adding() {
    let tree = Tree::new("add-hostile");
    for hostile in [
        "../etc",
        "roms/../../etc",
        "./roms",
        "roms//games",
        "C:/roms",
        r"C:\roms",
        r"\\server\share",
        r"roms\..\games",
        "",
    ] {
        let mut draft = RommConfigDraft {
            path_kind: ProviderPathKind::ProviderRelative,
            new_prefix: hostile.to_string(),
            new_destination: tree.library().display().to_string(),
            ..RommConfigDraft::blank()
        };
        let outcome = add_mapping(&mut draft, &tree.roots(), false);
        assert!(
            matches!(outcome, AddMappingOutcome::Refused(_)),
            "{hostile:?} produced {outcome:?}"
        );
        assert!(draft.mappings.is_empty(), "{hostile:?} was kept");
    }
}

#[test]
fn a_relative_destination_and_one_outside_the_roots_are_both_refused() {
    let tree = Tree::new("add-destination");
    for destination in ["relative/path", "/etc"] {
        let mut draft = RommConfigDraft {
            path_kind: ProviderPathKind::ProviderRelative,
            new_prefix: "roms".to_string(),
            new_destination: destination.to_string(),
            ..RommConfigDraft::blank()
        };
        assert!(
            matches!(
                add_mapping(&mut draft, &tree.roots(), false),
                AddMappingOutcome::Refused(_)
            ),
            "{destination} should be refused"
        );
    }
}

#[test]
fn adding_a_valid_mapping_clears_the_inputs_and_marks_the_draft_edited() {
    let tree = Tree::new("add-good");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        new_prefix: "  roms/  ".to_string(),
        new_destination: tree.library().display().to_string(),
        ..RommConfigDraft::blank()
    };
    assert_eq!(
        add_mapping(&mut draft, &tree.roots(), false),
        AddMappingOutcome::Added
    );
    assert_eq!(draft.mappings.len(), 1);
    assert!(
        draft.new_prefix.is_empty(),
        "the inputs should be ready for the next one"
    );
    assert!(draft.new_destination.is_empty());
    assert!(draft.dirty);
    assert!(draft.add_problem.is_none());
}

#[test]
fn a_duplicate_prefix_needs_replacement_confirmed() {
    let tree = Tree::new("duplicate");
    let first = tree.library().join("one");
    let second = tree.library().join("two");
    fs::create_dir_all(&first).expect("fixture");
    fs::create_dir_all(&second).expect("fixture");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &first)],
        new_prefix: "roms".to_string(),
        new_destination: second.display().to_string(),
        ..RommConfigDraft::blank()
    };
    match add_mapping(&mut draft, &tree.roots(), false) {
        AddMappingOutcome::NeedsReplaceConfirmation {
            existing_destination,
        } => assert_eq!(existing_destination, first),
        other => panic!("a duplicate must ask first: {other:?}"),
    }
    assert_eq!(draft.mappings.len(), 1, "nothing changed while unconfirmed");
    assert_eq!(draft.mappings[0].archivefs_prefix, first);

    // Confirmed, it replaces rather than duplicating.
    assert_eq!(
        add_mapping(&mut draft, &tree.roots(), true),
        AddMappingOutcome::Added
    );
    assert_eq!(draft.mappings.len(), 1);
    assert_eq!(draft.mappings[0].archivefs_prefix, second);
}

#[test]
fn two_spellings_of_one_prefix_are_recognised_as_the_same_mapping() {
    let tree = Tree::new("spelling");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        new_prefix: "roms/".to_string(),
        new_destination: tree.library().join("other").display().to_string(),
        ..RommConfigDraft::blank()
    };
    fs::create_dir_all(tree.library().join("other")).expect("fixture");
    assert!(
        matches!(
            add_mapping(&mut draft, &tree.roots(), false),
            AddMappingOutcome::NeedsReplaceConfirmation { .. }
        ),
        "`roms/` and `roms` are one prefix"
    );
}

#[test]
fn adding_a_second_mapping_to_the_same_destination_is_refused() {
    let tree = Tree::new("same-destination");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        new_prefix: "other".to_string(),
        new_destination: tree.library().display().to_string(),
        ..RommConfigDraft::blank()
    };
    assert!(matches!(
        add_mapping(&mut draft, &tree.roots(), false),
        AddMappingOutcome::Refused(_)
    ));
    assert_eq!(draft.mappings.len(), 1);
}

#[test]
fn removing_a_mapping_that_is_not_there_changes_nothing() {
    let tree = Tree::new("remove-missing");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        ..RommConfigDraft::blank()
    };
    assert!(!remove_mapping(&mut draft, "nope"));
    assert_eq!(draft.mappings.len(), 1);
    assert!(!draft.dirty, "a no-op is not an edit");
}

#[test]
fn removing_matches_either_spelling() {
    let tree = Tree::new("remove-spelling");
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        ..RommConfigDraft::blank()
    };
    assert!(remove_mapping(&mut draft, "roms/"));
    assert!(draft.mappings.is_empty());
}

#[test]
fn component_boundaries_decide_what_a_prefix_covers() {
    let tree = Tree::new("boundary");
    let backup = tree.library().join("backup");
    fs::create_dir_all(&backup).expect("fixture");
    // `roms` and `roms-backup` are different prefixes, not one plus a string
    // extension of it - so both may exist together.
    let mut draft = RommConfigDraft {
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        new_prefix: "roms-backup".to_string(),
        new_destination: backup.display().to_string(),
        ..RommConfigDraft::blank()
    };
    assert_eq!(
        add_mapping(&mut draft, &tree.roots(), false),
        AddMappingOutcome::Added,
        "a different prefix is not a duplicate"
    );
    assert_eq!(draft.mappings.len(), 2);
}

// --- Preview --------------------------------------------------------------

fn engine(
    prefix: &str,
    destination: &Path,
    kind: ProviderPathKind,
    roots: &[PathBuf],
) -> PathMappings {
    PathMappings::validate(&[mapping(prefix, destination)], roots, kind)
        .expect("the fixture mapping should validate")
}

fn preview_of(
    engine: &PathMappings,
    samples: &[&str],
    kind: ProviderPathKind,
    presence: &dyn Fn(&Path) -> &'static str,
) -> RommPreviewSummary {
    let owned: Vec<String> = samples.iter().map(|s| s.to_string()).collect();
    let built = archivefs_core::identity_source::path_map::MappingPreview::build(engine, &owned);
    let examples: Vec<PreviewExampleView> = built
        .translations
        .iter()
        .map(|translation| preview_example(translation, None, presence))
        .collect();
    summarise_preview(
        examples,
        kind,
        built.observed_relative,
        built.observed_absolute,
        "a test fixture",
    )
}

#[test]
fn a_preview_reports_each_translation_with_its_local_presence() {
    let tree = Tree::new("preview-presence");
    let engine = engine(
        "roms",
        &tree.library(),
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    // A presence probe the test controls, so every outcome can be exercised.
    let presence = |path: &Path| -> &'static str {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("present.gb") => "file",
            Some("Shenmue") => "directory",
            Some("orphan.gb") => "dangling_symlink",
            Some("nowhere.gb") => "parent_absent",
            _ => "absent",
        }
    };
    let summary = preview_of(
        &engine,
        &[
            "roms/gb/present.gb",
            "roms/dc/Shenmue",
            "roms/gb/orphan.gb",
            "roms/none/nowhere.gb",
            "roms/gb/gone.gb",
            "backups/gb/other.gb",
        ],
        ProviderPathKind::ProviderRelative,
        &presence,
    );

    assert_eq!(summary.translated, 5);
    assert_eq!(summary.unmatched, 1, "backups/ is covered by no mapping");
    assert_eq!(summary.refused, 0);
    assert_eq!(summary.existing_files, 1);
    assert_eq!(summary.directories, 1);
    assert_eq!(summary.dangling_symlinks, 1);
    assert_eq!(summary.missing_parents, 1);
    assert_eq!(summary.missing, 1);
    assert_eq!(summary.observed_relative, 6);
    assert_eq!(summary.observed_absolute, 0);
    assert!(summary.path_shape_agrees());

    // The exact string RomM sent is preserved on every row.
    let first = &summary.examples[0];
    assert_eq!(first.provider_path, "roms/gb/present.gb");
    assert_eq!(first.matched_prefix.as_deref(), Some("roms"));
    assert_eq!(
        first.archivefs_path.as_deref(),
        Some(tree.library().join("gb/present.gb").as_path())
    );
    assert_eq!(first.presence, Some("file"));
    assert_eq!(
        first.trusted_root.as_deref(),
        Some(tree.library().as_path())
    );
    assert_eq!(first.outcome, "translated");
}

#[test]
fn every_hostile_provider_path_is_refused_by_the_preview() {
    let tree = Tree::new("preview-hostile");
    let engine = engine(
        "roms",
        &tree.library(),
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    let presence = |_: &Path| -> &'static str { "absent" };
    let hostile = [
        "../etc/passwd",
        "roms/../../etc/passwd",
        "./roms/game.zip",
        "roms//game.zip",
        "C:/roms/game.zip",
        r"C:\roms\game.zip",
        r"\\server\share\game.zip",
        r"roms\..\game.zip",
        // Wrong shape for this source.
        "/romm/library/game.zip",
    ];
    let summary = preview_of(
        &engine,
        &hostile,
        ProviderPathKind::ProviderRelative,
        &presence,
    );
    assert_eq!(
        summary.refused,
        hostile.len(),
        "every hostile shape must be refused: {:#?}",
        summary
            .examples
            .iter()
            .map(|e| (&e.provider_path, e.outcome))
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.translated, 0);
    for example in &summary.examples {
        assert!(
            example.archivefs_path.is_none(),
            "{}",
            example.provider_path
        );
        assert!(example.refusal_code.is_some(), "{}", example.provider_path);
    }
}

#[test]
fn a_percent_encoded_component_stays_literal_and_inside_the_library() {
    let tree = Tree::new("preview-percent");
    let engine = engine(
        "roms",
        &tree.library(),
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    let presence = |_: &Path| -> &'static str { "absent" };
    let summary = preview_of(
        &engine,
        &["roms/%2e%2e/%2e%2e/etc/passwd"],
        ProviderPathKind::ProviderRelative,
        &presence,
    );
    // Nothing decodes, so these are ordinary folder names.
    assert_eq!(summary.translated, 1);
    let path = summary.examples[0]
        .archivefs_path
        .clone()
        .expect("a literal path translates");
    assert!(path.starts_with(tree.library()), "{}", path.display());
    assert!(!path.to_string_lossy().contains(".."), "{}", path.display());
}

#[test]
fn a_shape_mismatch_leads_the_preview_summary() {
    let tree = Tree::new("preview-mismatch");
    // Configured absolute, but the paths that arrived are relative.
    let engine = engine(
        "/romm/library",
        &tree.library(),
        ProviderPathKind::AbsoluteProviderPath,
        &tree.roots(),
    );
    let presence = |_: &Path| -> &'static str { "absent" };
    let summary = preview_of(
        &engine,
        &["roms/gb/a.gb", "roms/gb/b.gb", "roms/gb/c.gb"],
        ProviderPathKind::AbsoluteProviderPath,
        &presence,
    );
    assert_eq!(summary.refused, 3);
    assert_eq!(summary.observed_relative, 3);
    assert!(!summary.path_shape_agrees());
    assert_eq!(summary.suggested_path_kind.as_deref(), Some("relative"));
    let headline = summary.headline();
    assert!(headline.contains("look relative"), "{headline}");
    assert!(headline.contains("stay unmatched"), "{headline}");
}

#[test]
fn a_preview_where_nothing_translates_says_to_check_the_prefix() {
    let tree = Tree::new("preview-nothing");
    let engine = engine(
        "roms",
        &tree.library(),
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    let presence = |_: &Path| -> &'static str { "absent" };
    let summary = preview_of(
        &engine,
        &["elsewhere/a.gb", "elsewhere/b.gb"],
        ProviderPathKind::ProviderRelative,
        &presence,
    );
    assert_eq!(summary.unmatched, 2);
    assert!(
        summary.headline().contains("Check that a mapping covers"),
        "{}",
        summary.headline()
    );
}

#[test]
fn the_preview_count_rows_carry_every_aggregate_the_card_shows() {
    let tree = Tree::new("preview-rows");
    let engine = engine(
        "roms",
        &tree.library(),
        ProviderPathKind::ProviderRelative,
        &tree.roots(),
    );
    let presence = |_: &Path| -> &'static str { "file" };
    let summary = preview_of(
        &engine,
        &["roms/a.gb"],
        ProviderPathKind::ProviderRelative,
        &presence,
    );
    let labels: Vec<String> = preview_count_rows(&summary)
        .into_iter()
        .map(|row| row.label)
        .collect();
    for expected in [
        "Existing files",
        "Directories",
        "Dangling symlinks",
        "Missing",
        "Missing parent folder",
        "Observed relative paths",
        "Observed absolute paths",
        "Configured shape",
    ] {
        assert!(labels.contains(&expected.to_string()), "{expected} missing");
    }
}

// --- Rendering ------------------------------------------------------------

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

struct NoClipboard;

impl crate::ClipboardBackend for NoClipboard {
    fn set_text(&mut self, _text: String) -> Result<(), String> {
        Ok(())
    }

    fn get_text_status(&mut self) -> crate::ClipboardTextStatus {
        crate::ClipboardTextStatus::Empty
    }
}

#[test]
fn the_dialog_draws_the_token_path_and_never_its_contents() {
    let tree = Tree::new("render-token");
    let path = tree.token(SECRET, 0o600);
    let mut draft = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        token_path: path.display().to_string(),
        page_size: "100".to_string(),
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        ..RommConfigDraft::blank()
    };
    let validation = validate_draft(&draft, Some(&token_verdict(&path)), &tree.roots());
    let mappings = build_mappings_view(&draft.mappings, draft.path_kind, &tree.roots());
    let context = egui::Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_config_dialog(
                ui,
                &mut draft,
                &ConfigDialogInputs {
                    validation: &validation,
                    mappings: &mappings,
                    preview: None,
                    previous: None,
                    busy: false,
                    preview_running: false,
                },
                &mut NoClipboard,
            );
        });
    });
    assert!(rendered_text_contains(&output, "Configure RomM"));
    assert!(
        rendered_text_contains(&output, &path.display().to_string()),
        "the path is what a person needs"
    );
    assert!(
        !rendered_text_contains(&output, SECRET),
        "the token contents were drawn"
    );
    assert!(!rendered_text_contains(&output, "Bearer"));
    // And it states plainly that saving contacts nothing.
    assert!(rendered_text_contains(&output, "contacts nothing"));
}

#[test]
fn the_dialog_draws_the_mapping_and_the_path_shape_choices() {
    let tree = Tree::new("render-mappings");
    let mut draft = RommConfigDraft {
        url: "http://172.19.0.20:8080".to_string(),
        page_size: "100".to_string(),
        path_kind: ProviderPathKind::ProviderRelative,
        mappings: vec![mapping("roms", &tree.library())],
        ..RommConfigDraft::blank()
    };
    let validation = validate_draft(&draft, None, &tree.roots());
    let mappings = build_mappings_view(&draft.mappings, draft.path_kind, &tree.roots());
    let context = egui::Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            let _ = show_config_dialog(
                ui,
                &mut draft,
                &ConfigDialogInputs {
                    validation: &validation,
                    mappings: &mappings,
                    preview: None,
                    previous: None,
                    busy: false,
                    preview_running: false,
                },
                &mut NoClipboard,
            );
        });
    });
    assert!(rendered_text_contains(&output, "roms"));
    assert!(rendered_text_contains(
        &output,
        &tree.library().display().to_string()
    ));
    // Both shapes are offered with an example, so the choice is explicable.
    assert!(rendered_text_contains(&output, "roms/snes/game.zip"));
    assert!(rendered_text_contains(
        &output,
        "/romm/library/snes/game.zip"
    ));
    assert!(rendered_text_contains(&output, "Path mappings"));
}
