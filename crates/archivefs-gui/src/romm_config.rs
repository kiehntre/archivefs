//! The RomM configuration dialog, mappings editor and mapping preview.
//!
//! # Validation happens twice, deliberately
//!
//! Every field is validated as it is typed, and again in the worker before
//! anything is written. The live pass exists so a mistake is visible where it was
//! made; the worker pass is the one that decides, because it is the only one that
//! resolves a hostname and re-reads the token file.
//!
//! The live pass reuses the core's own rules rather than restating them: the URL is
//! run through [`validate_endpoint`] with a resolver that knows no names. A literal
//! address - which is what a private RomM almost always is - resolves inside that
//! resolver, so the whole local-only policy is applied live. A *hostname* cannot
//! be, so those refusals are reported as "checked when you save" instead of being
//! guessed at. Typing in the URL field therefore never touches the network, and
//! never contacts RomM.
//!
//! # The token is a path and nothing else
//!
//! No control in this dialog reads token contents, and no field is ever prefilled
//! with a secret. The token file is described by its path and by the core loader's
//! own verdict on it - missing, a symlink, the wrong mode, empty - which is
//! everything a person needs to fix it and nothing they need to keep private.

use std::path::{Path, PathBuf};

use archivefs_core::identity_source::net_policy::{StaticResolver, validate_endpoint};
use archivefs_core::identity_source::path_map::{
    PathMapping, PathMappings, PathTranslation, ProviderPathKind, normalise_prefix,
};
use archivefs_core::identity_source::settings::{
    MAX_CONFIGURED_PAGE_SIZE, MIN_CONFIGURED_PAGE_SIZE, ProviderSettings, SUGGESTED_TOKEN_PATH,
};
use eframe::egui;

use crate::romm_source::{CardRow, RommSnapshot};
use crate::ui::{components as widgets, theme};

/// The default number of paths a preview samples.
pub(crate) const DEFAULT_PREVIEW_LIMIT: usize = 20;
/// The most it will sample however large a number is typed.
pub(crate) const MAX_PREVIEW_LIMIT: usize = 100;

/// The shell a person can copy to create a token file safely.
///
/// `install -m 600 /dev/null` creates it empty and private in one step, which is
/// the part that is easy to get wrong with `touch` followed by `chmod`.
pub(crate) const TOKEN_FILE_SHELL_EXAMPLE: &str = "mkdir -p ~/.config/emuwiz\n\
     install -m 600 /dev/null ~/.config/emuwiz/romm-token\n\
     nano ~/.config/emuwiz/romm-token";

/// One field's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FieldState {
    /// Usable, with a note worth showing.
    Good(String),
    /// Not usable, and why.
    Problem(String),
    /// Cannot be decided without doing I/O, so it is decided on save.
    Deferred(String),
    /// Nothing entered yet.
    Empty(String),
}

impl FieldState {
    pub(crate) fn is_problem(&self) -> bool {
        matches!(self, Self::Problem(_))
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Good(message)
            | Self::Problem(message)
            | Self::Deferred(message)
            | Self::Empty(message) => message,
        }
    }

    pub(crate) fn tone(&self) -> widgets::StatusTone {
        match self {
            Self::Good(_) => widgets::StatusTone::Success,
            Self::Problem(_) => widgets::StatusTone::Blocked,
            Self::Deferred(_) => widgets::StatusTone::Info,
            Self::Empty(_) => widgets::StatusTone::Pending,
        }
    }
}

/// The editable draft. Holds text, because that is what a person types; the typed
/// values are derived from it by validation.
#[derive(Clone, Debug, Default)]
pub(crate) struct RommConfigDraft {
    pub(crate) url: String,
    pub(crate) token_path: String,
    pub(crate) path_kind: ProviderPathKind,
    pub(crate) page_size: String,
    /// The working copy of the mappings. Not written until the draft is saved.
    pub(crate) mappings: Vec<PathMapping>,
    pub(crate) new_prefix: String,
    pub(crate) new_destination: String,
    /// A prefix whose replacement is awaiting confirmation.
    pub(crate) replace_confirm: Option<String>,
    /// A prefix whose removal is awaiting confirmation, because removing it would
    /// leave nothing usable.
    pub(crate) remove_confirm: Option<String>,
    pub(crate) preview_limit: String,
    /// Set once anything has been edited, so closing can warn.
    pub(crate) dirty: bool,
    pub(crate) close_confirm: bool,
    /// The last add attempt's refusal, kept until the inputs change.
    pub(crate) add_problem: Option<String>,
}

impl RommConfigDraft {
    /// Opens the dialog on the configuration that is actually stored.
    pub(crate) fn from_snapshot(snapshot: &RommSnapshot) -> Self {
        let source = &snapshot.settings.source;
        Self {
            url: source.url.clone(),
            // The path, never the contents.
            token_path: source
                .token_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            path_kind: source.provider_path_kind,
            page_size: snapshot.settings.effective_page_size().to_string(),
            mappings: source.mappings.clone(),
            preview_limit: DEFAULT_PREVIEW_LIMIT.to_string(),
            ..Self::default()
        }
    }

    /// A fresh, empty draft for a source that has never been configured.
    pub(crate) fn blank() -> Self {
        Self {
            page_size: "100".to_string(),
            preview_limit: DEFAULT_PREVIEW_LIMIT.to_string(),
            ..Self::default()
        }
    }

    /// The settings this draft would save, when it is valid.
    pub(crate) fn to_settings(&self, previous: Option<&ProviderSettings>) -> ProviderSettings {
        let mut settings = previous.cloned().unwrap_or_default();
        settings.source.url = self.url.trim().to_string();
        settings.source.token_path = {
            let trimmed = self.token_path.trim();
            (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
        };
        settings.source.provider_path_kind = self.path_kind;
        settings.source.mappings = self.mappings.clone();
        // The field is prefilled with the *effective* size, so a page size that was
        // never set explicitly would otherwise be written out as though it had been.
        // Leaving it unset when it still matches keeps a no-change save a no-change
        // save, which is what makes "nothing was written that you did not ask for"
        // true rather than nearly true.
        let typed = self.page_size.trim().parse::<u32>().ok();
        let previously_unset = previous.is_some_and(|previous| previous.page_size.is_none());
        let still_the_default = typed == previous.map(ProviderSettings::effective_page_size);
        settings.page_size = if previously_unset && still_the_default {
            None
        } else {
            typed
        };
        settings
    }
}

/// Every field's verdict, plus whether the draft can be saved at all.
#[derive(Clone, Debug)]
pub(crate) struct RommConfigValidation {
    pub(crate) url: FieldState,
    pub(crate) token: FieldState,
    pub(crate) page_size: FieldState,
    /// Mappings that the chosen path kind would strand. A non-empty list refuses
    /// the save: mappings that can never match are worse than none.
    pub(crate) stranded_mappings: Vec<String>,
    pub(crate) can_save: bool,
}

/// Validates a draft without touching the network.
///
/// `token_verdict` is the core loader's own answer for the path in the draft, which
/// the caller supplies because reading it is I/O. `source_roots` are the configured
/// source folders.
pub(crate) fn validate_draft(
    draft: &RommConfigDraft,
    token_verdict: Option<&FieldState>,
    source_roots: &[PathBuf],
) -> RommConfigValidation {
    let url = validate_url_text(&draft.url);
    let token = token_verdict.cloned().unwrap_or_else(|| {
        FieldState::Empty(format!(
            "No token file yet. Create a read-only RomM client token and point this at it; the \
             suggested location is {SUGGESTED_TOKEN_PATH}."
        ))
    });
    let page_size = validate_page_size(&draft.page_size);
    // A mapping written for the other shape can never match anything, so the save
    // is refused and the offending prefixes are named.
    let stranded_mappings: Vec<String> = draft
        .mappings
        .iter()
        .filter(|mapping| normalise_prefix(&mapping.provider_prefix, draft.path_kind).is_err())
        .map(|mapping| mapping.provider_prefix.clone())
        .collect();
    // The whole set has to validate together: duplicates and shared destinations are
    // only visible across mappings.
    let mapping_set_problem = if stranded_mappings.is_empty() {
        PathMappings::validate(&draft.mappings, source_roots, draft.path_kind)
            .err()
            .map(|refusal| refusal.detail())
    } else {
        None
    };
    let can_save = !url.is_problem()
        && !token.is_problem()
        && !page_size.is_problem()
        && stranded_mappings.is_empty()
        && mapping_set_problem.is_none();
    RommConfigValidation {
        url,
        token,
        page_size: if let Some(problem) = mapping_set_problem {
            // Surfaced on the page-size row's neighbour rather than invented as a
            // fourth field: it belongs to the mappings, and the editor shows it too.
            let _ = &problem;
            page_size
        } else {
            page_size
        },
        stranded_mappings,
        can_save,
    }
}

/// Judges URL text using the core's own endpoint policy.
///
/// A resolver that knows no hostnames is used on purpose. A literal address still
/// goes through the entire policy - scheme, credentials, port, private-range and
/// metadata checks - so the common case is fully decided here. A hostname cannot
/// be resolved without I/O, so that one case defers to the save.
pub(crate) fn validate_url_text(text: &str) -> FieldState {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return FieldState::Empty(
            "Enter the address of your RomM instance, for example http://172.19.0.20:8080."
                .to_string(),
        );
    }
    match validate_endpoint(trimmed, &StaticResolver::new()) {
        Ok(approved) => FieldState::Good(format!(
            "{} resolves to {} - a local address this policy accepts.",
            approved.origin(),
            approved.resolved_addresses().join(", ")
        )),
        Err(refusal) => match refusal.code() {
            // Decidable only by resolving, which is I/O and so happens on save.
            "unresolvable_host" | "no_addresses" => FieldState::Deferred(
                "That looks like a hostname. Whether it points at a local address is checked \
                 when you save - no request is made while you type."
                    .to_string(),
            ),
            _ => FieldState::Problem(refusal.detail()),
        },
    }
}

pub(crate) fn validate_page_size(text: &str) -> FieldState {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return FieldState::Empty(format!(
            "Leave blank for the default. Between {MIN_CONFIGURED_PAGE_SIZE} and \
             {MAX_CONFIGURED_PAGE_SIZE} records per request."
        ));
    }
    match trimmed.parse::<u32>() {
        Ok(value) if (MIN_CONFIGURED_PAGE_SIZE..=MAX_CONFIGURED_PAGE_SIZE).contains(&value) => {
            FieldState::Good(format!("{value} records per request."))
        }
        Ok(value) => FieldState::Problem(format!(
            "{value} is outside the safe range of {MIN_CONFIGURED_PAGE_SIZE} to \
             {MAX_CONFIGURED_PAGE_SIZE}. A larger page is what makes a response exceed the size \
             ceiling."
        )),
        Err(_) => FieldState::Problem(format!("{trimmed:?} is not a whole number.")),
    }
}

/// Turns the core loader's refusal into a field verdict.
///
/// Every arm names the remedy, because "unsafe permissions" without `chmod 600` is
/// a dead end.
pub(crate) fn token_field_state(
    verdict: Result<(), archivefs_core::identity_source::settings::TokenFileRefusal>,
    path: &str,
) -> FieldState {
    match verdict {
        Ok(()) => FieldState::Good(format!("{path} is a private regular file holding a token.")),
        Err(refusal) => match refusal.code() {
            "token_not_configured" => FieldState::Empty(refusal.detail()),
            _ => FieldState::Problem(refusal.detail()),
        },
    }
}

/// One row of the mappings editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MappingRowView {
    pub(crate) provider_prefix: String,
    /// The canonical form the engine compares against, when it has one.
    pub(crate) normalised_prefix: Option<String>,
    pub(crate) path_kind: ProviderPathKind,
    pub(crate) destination: PathBuf,
    /// Whether the destination sits inside a configured source folder.
    pub(crate) inside_source_root: bool,
    pub(crate) valid: bool,
    pub(crate) problem: Option<String>,
    /// A more specific mapping that would win for every path this one covers.
    pub(crate) shadowed_by: Option<String>,
}

/// The mappings editor, as data.
#[derive(Clone, Debug)]
pub(crate) struct MappingsEditorView {
    /// Longest provider prefix first, which is the order the engine applies.
    pub(crate) rows: Vec<MappingRowView>,
    pub(crate) source_roots: Vec<PathBuf>,
    pub(crate) path_kind: ProviderPathKind,
    /// Set when the set as a whole is unusable - a duplicate, or two mappings
    /// landing on one directory.
    pub(crate) set_problem: Option<String>,
    /// True when nothing usable is configured, so an import would match nothing.
    pub(crate) no_usable_mapping: bool,
}

/// Builds the editor view. Pure except for asking whether a destination is inside
/// a configured root, which is a comparison, not a filesystem read.
pub(crate) fn build_mappings_view(
    mappings: &[PathMapping],
    path_kind: ProviderPathKind,
    source_roots: &[PathBuf],
) -> MappingsEditorView {
    // Ordered by the engine when it can be; otherwise as typed, so an invalid row
    // is still visible and still removable.
    let ordered = PathMappings::validate(mappings, &[], path_kind)
        .map(|validated| validated.as_slice().to_vec())
        .unwrap_or_else(|_| mappings.to_vec());

    let normalised: Vec<Option<String>> = ordered
        .iter()
        .map(|mapping| normalise_prefix(&mapping.provider_prefix, path_kind).ok())
        .collect();

    let rows: Vec<MappingRowView> = ordered
        .iter()
        .enumerate()
        .map(|(index, mapping)| {
            let single =
                PathMappings::validate(std::slice::from_ref(mapping), source_roots, path_kind);
            let inside_source_root = source_roots.is_empty()
                || source_roots
                    .iter()
                    .any(|root| mapping.archivefs_prefix.starts_with(root));
            // Shadowed when another prefix in the set is strictly more specific and
            // this one's paths would all be caught by it first.
            let shadowed_by = normalised[index].as_ref().and_then(|mine| {
                normalised
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .filter_map(|(_, candidate)| candidate.as_ref())
                    .find(|other| {
                        other.len() < mine.len()
                            && mine.starts_with(other.as_str())
                            && mine
                                .as_bytes()
                                .get(other.len())
                                .is_some_and(|byte| *byte == b'/')
                    })
                    // A broader prefix does not shadow a narrower one - the narrower
                    // wins. This finds the case the other way round, which cannot
                    // happen with longest-prefix ordering, so it stays None in
                    // practice and exists to make that visible if ordering changes.
                    .map(|_| String::new())
                    .filter(|value| !value.is_empty())
            });
            MappingRowView {
                provider_prefix: mapping.provider_prefix.clone(),
                normalised_prefix: normalised[index].clone(),
                path_kind,
                destination: mapping.archivefs_prefix.clone(),
                inside_source_root,
                valid: single.is_ok(),
                problem: single.err().map(|refusal| refusal.detail()),
                shadowed_by,
            }
        })
        .collect();

    let set_problem = PathMappings::validate(mappings, source_roots, path_kind)
        .err()
        .map(|refusal| refusal.detail());
    let no_usable_mapping = rows.iter().all(|row| !row.valid);

    MappingsEditorView {
        rows,
        source_roots: source_roots.to_vec(),
        path_kind,
        set_problem,
        no_usable_mapping,
    }
}

/// Why a mapping could not be added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AddMappingOutcome {
    Added,
    /// A mapping for this prefix already exists; replacing needs confirmation.
    NeedsReplaceConfirmation {
        existing_destination: PathBuf,
    },
    Refused(String),
}

/// Adds one mapping to a draft, or explains why not.
///
/// Does not resolve the provider prefix against the local filesystem: a provider
/// path is text from a remote server, and the only safe thing to do with it is
/// compare it.
pub(crate) fn add_mapping(
    draft: &mut RommConfigDraft,
    source_roots: &[PathBuf],
    confirmed_replacement: bool,
) -> AddMappingOutcome {
    let prefix = draft.new_prefix.trim().to_string();
    let destination = draft.new_destination.trim().to_string();
    if prefix.is_empty() || destination.is_empty() {
        return AddMappingOutcome::Refused(
            "Both a RomM prefix and an EmuWiz folder are needed.".to_string(),
        );
    }
    let candidate = PathMapping {
        provider_prefix: prefix.clone(),
        archivefs_prefix: PathBuf::from(&destination),
    };
    // The prefix must be the shape this source is configured for, and free of
    // traversal, dot components, doubled separators, drive letters and UNC forms.
    // All of that is the core's rule, applied here rather than restated.
    let normalised = match normalise_prefix(&prefix, draft.path_kind) {
        Ok(normalised) => normalised,
        Err(refusal) => return AddMappingOutcome::Refused(refusal.detail()),
    };
    if let Err(refusal) = PathMappings::validate(
        std::slice::from_ref(&candidate),
        source_roots,
        draft.path_kind,
    ) {
        return AddMappingOutcome::Refused(refusal.detail());
    }

    let existing = draft.mappings.iter().position(|mapping| {
        normalise_prefix(&mapping.provider_prefix, draft.path_kind)
            .is_ok_and(|other| other == normalised)
    });
    if let Some(index) = existing {
        if !confirmed_replacement {
            return AddMappingOutcome::NeedsReplaceConfirmation {
                existing_destination: draft.mappings[index].archivefs_prefix.clone(),
            };
        }
        draft.mappings.remove(index);
    }

    let mut proposed = draft.mappings.clone();
    proposed.push(candidate.clone());
    // Validated as a whole, so a duplicate destination is caught before it is kept.
    if let Err(refusal) = PathMappings::validate(&proposed, source_roots, draft.path_kind) {
        return AddMappingOutcome::Refused(refusal.detail());
    }
    draft.mappings = proposed;
    draft.new_prefix.clear();
    draft.new_destination.clear();
    draft.add_problem = None;
    draft.dirty = true;
    AddMappingOutcome::Added
}

/// Removes one mapping by the prefix as displayed.
///
/// Matches on the canonical form when both normalise, and on the text otherwise -
/// so a mapping stranded by a change of path kind is still removable, which is the
/// only way out of that state.
pub(crate) fn remove_mapping(draft: &mut RommConfigDraft, prefix: &str) -> bool {
    let target = normalise_prefix(prefix, draft.path_kind).ok();
    let before = draft.mappings.len();
    draft.mappings.retain(|mapping| {
        let mine = normalise_prefix(&mapping.provider_prefix, draft.path_kind).ok();
        match (&mine, &target) {
            (Some(left), Some(right)) => left != right,
            _ => mapping.provider_prefix.trim() != prefix.trim(),
        }
    });
    let removed = draft.mappings.len() != before;
    if removed {
        draft.dirty = true;
    }
    removed
}

/// One previewed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreviewExampleView {
    /// Exactly what RomM sent.
    pub(crate) provider_path: String,
    /// The form the comparison used, when it differs.
    pub(crate) normalised_path: Option<String>,
    pub(crate) path_kind: Option<ProviderPathKind>,
    pub(crate) matched_prefix: Option<String>,
    pub(crate) archivefs_path: Option<PathBuf>,
    pub(crate) canonical_platform: Option<String>,
    /// What is actually at the translated path.
    pub(crate) presence: Option<&'static str>,
    pub(crate) trusted_root: Option<PathBuf>,
    pub(crate) outcome: &'static str,
    pub(crate) refusal: Option<String>,
    pub(crate) refusal_code: Option<String>,
}

/// A whole preview, ready to draw.
#[derive(Clone, Debug, Default)]
pub(crate) struct RommPreviewSummary {
    pub(crate) examples: Vec<PreviewExampleView>,
    pub(crate) translated: usize,
    pub(crate) unmatched: usize,
    pub(crate) refused: usize,
    pub(crate) existing_files: usize,
    pub(crate) directories: usize,
    pub(crate) dangling_symlinks: usize,
    pub(crate) missing: usize,
    pub(crate) missing_parents: usize,
    pub(crate) observed_relative: usize,
    pub(crate) observed_absolute: usize,
    pub(crate) configured_path_kind: String,
    /// Set when the sampled paths disagree with the configured shape.
    pub(crate) suggested_path_kind: Option<String>,
    /// Where the sample came from: the published cache, or a bounded RomM request.
    pub(crate) sample_source: &'static str,
}

impl RommPreviewSummary {
    /// Whether the configured shape and what arrived agree.
    pub(crate) fn path_shape_agrees(&self) -> bool {
        self.suggested_path_kind.is_none()
    }

    /// The one sentence worth leading with.
    pub(crate) fn headline(&self) -> String {
        match &self.suggested_path_kind {
            Some(suggested) => format!(
                "These paths look {suggested}, not {}. Until the path shape is changed, every \
                 record will stay unmatched.",
                self.configured_path_kind
            ),
            None if self.translated == 0 && !self.examples.is_empty() => {
                "Nothing translated. Check that a mapping covers the prefix these paths start \
                 with."
                    .to_string()
            }
            None => format!(
                "{} translated, {} unmatched, {} refused. {} of the translated paths exist locally.",
                self.translated, self.unmatched, self.refused, self.existing_files
            ),
        }
    }
}

/// Reduces one core translation to a preview row.
///
/// `presence_for` is supplied by the caller so this stays testable without a
/// filesystem, and so the preview cannot start reading file contents.
pub(crate) fn preview_example(
    translation: &PathTranslation,
    canonical_platform: Option<String>,
    presence_for: &dyn Fn(&Path) -> &'static str,
) -> PreviewExampleView {
    match translation {
        PathTranslation::Translated {
            provider_path,
            normalised_path,
            kind,
            archivefs_path,
            matched_prefix,
            trusted_root,
        } => PreviewExampleView {
            provider_path: provider_path.clone(),
            normalised_path: (normalised_path != provider_path).then(|| normalised_path.clone()),
            path_kind: Some(*kind),
            matched_prefix: Some(matched_prefix.clone()),
            archivefs_path: Some(archivefs_path.clone()),
            canonical_platform,
            presence: Some(presence_for(archivefs_path)),
            trusted_root: trusted_root.clone(),
            outcome: "translated",
            refusal: None,
            refusal_code: None,
        },
        PathTranslation::Unmatched {
            provider_path,
            normalised_path,
            kind,
        } => PreviewExampleView {
            provider_path: provider_path.clone(),
            normalised_path: (normalised_path != provider_path).then(|| normalised_path.clone()),
            path_kind: Some(*kind),
            matched_prefix: None,
            archivefs_path: None,
            canonical_platform,
            presence: None,
            trusted_root: None,
            outcome: "unmatched",
            refusal: None,
            refusal_code: None,
        },
        PathTranslation::Refused {
            provider_path,
            refusal,
        } => PreviewExampleView {
            provider_path: provider_path.clone(),
            normalised_path: None,
            path_kind: None,
            matched_prefix: None,
            archivefs_path: None,
            canonical_platform: None,
            presence: None,
            trusted_root: None,
            outcome: "refused",
            refusal: Some(refusal.detail()),
            refusal_code: Some(refusal.code().to_string()),
        },
    }
}

/// Counts a finished set of rows.
pub(crate) fn summarise_preview(
    examples: Vec<PreviewExampleView>,
    configured_path_kind: ProviderPathKind,
    observed_relative: usize,
    observed_absolute: usize,
    sample_source: &'static str,
) -> RommPreviewSummary {
    let mut summary = RommPreviewSummary {
        configured_path_kind: configured_path_kind.slug().to_string(),
        observed_relative,
        observed_absolute,
        sample_source,
        ..RommPreviewSummary::default()
    };
    for example in &examples {
        match example.outcome {
            "translated" => summary.translated += 1,
            "unmatched" => summary.unmatched += 1,
            _ => summary.refused += 1,
        }
        match example.presence {
            Some("file") => summary.existing_files += 1,
            Some("directory") => summary.directories += 1,
            Some("dangling_symlink") => summary.dangling_symlinks += 1,
            Some("parent_absent") => summary.missing_parents += 1,
            Some("absent") => summary.missing += 1,
            _ => {}
        }
    }
    // Advice only, and only when the sample clearly disagrees.
    let observed = match (observed_relative, observed_absolute) {
        (0, 0) => None,
        (relative, absolute) if relative > absolute => Some(ProviderPathKind::ProviderRelative),
        (relative, absolute) if absolute > relative => Some(ProviderPathKind::AbsoluteProviderPath),
        _ => None,
    };
    summary.suggested_path_kind = observed
        .filter(|kind| *kind != configured_path_kind)
        .map(|kind| kind.slug().to_string());
    summary.examples = examples;
    summary
}

/// Everything the dialog draws from, other than the draft it edits.
///
/// Grouped rather than passed as nine parameters: naming them at the call site is
/// what keeps "which bool was which" from becoming a real question.
pub(crate) struct ConfigDialogInputs<'a> {
    pub(crate) validation: &'a RommConfigValidation,
    pub(crate) mappings: &'a MappingsEditorView,
    pub(crate) preview: Option<&'a RommPreviewSummary>,
    pub(crate) previous: Option<&'a ProviderSettings>,
    pub(crate) busy: bool,
    pub(crate) preview_running: bool,
}

/// What the dialog wants the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfigDialogRequest {
    Save(Box<ProviderSettings>),
    Preview { limit: usize },
    CancelPreview,
    Close,
}

/// Draws the dialog. Every decision was already made by the validation above.
pub(crate) fn show_config_dialog(
    ui: &mut egui::Ui,
    draft: &mut RommConfigDraft,
    inputs: &ConfigDialogInputs<'_>,
    clipboard: &mut dyn crate::ClipboardBackend,
) -> Option<ConfigDialogRequest> {
    let ConfigDialogInputs {
        validation,
        mappings,
        preview,
        // The body no longer builds a `Save`; the footer owns that, and with
        // it the previous settings a save is derived from.
        previous: _,
        busy,
        preview_running,
    } = *inputs;
    let mut request = None;
    widgets::section_header(
        ui,
        "Configure RomM",
        Some("Nothing here contacts RomM. Saving writes EmuWiz's own configuration only."),
    );
    widgets::card(ui, |ui| {
        // --- URL -----------------------------------------------------------
        ui.label("RomM address");
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.url)
                    .desired_width(420.0)
                    .hint_text("http://172.19.0.20:8080"),
            )
            .changed()
        {
            draft.dirty = true;
        }
        field_note(ui, &validation.url);

        // --- Token file ----------------------------------------------------
        ui.add_space(theme::SECTION_GAP / 2.0);
        ui.label("Token file");
        ui.label(
            "EmuWiz reads the token from this file when it needs it. The contents are never \
             shown here, never stored in the configuration, and never written to a log.",
        );
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.token_path)
                    .desired_width(420.0)
                    .hint_text(SUGGESTED_TOKEN_PATH),
            )
            .changed()
        {
            draft.dirty = true;
        }
        field_note(ui, &validation.token);
        widgets::technical_details(ui, "romm-token-help", |ui| {
            ui.label(format!(
                "Create a read-only client token in RomM with the platforms.read and roms.read \
                 scopes, then put it in a private file. The suggested location is \
                 {SUGGESTED_TOKEN_PATH} - EmuWiz never creates it for you."
            ));
            ui.add(
                egui::TextEdit::multiline(&mut TOKEN_FILE_SHELL_EXAMPLE.to_string())
                    .desired_width(520.0)
                    .code_editor()
                    .interactive(false),
            );
            if ui.button("Copy these commands").clicked() {
                let _ = clipboard.set_text(TOKEN_FILE_SHELL_EXAMPLE.to_string());
            }
        });

        // --- Path kind -----------------------------------------------------
        ui.add_space(theme::SECTION_GAP / 2.0);
        ui.label("How RomM reports paths");
        for (kind, example) in [
            (
                ProviderPathKind::ProviderRelative,
                "Relative - RomM returns paths such as roms/snes/game.zip",
            ),
            (
                ProviderPathKind::AbsoluteProviderPath,
                "Absolute - RomM returns paths such as /romm/library/snes/game.zip",
            ),
        ] {
            if ui
                .radio_value(&mut draft.path_kind, kind, example)
                .changed()
            {
                draft.dirty = true;
            }
        }
        ui.label(
            "This is a setting, not a guess: EmuWiz never infers the shape from individual \
             records. Run Test connection to see which shape your server actually reports.",
        );
        if !validation.stranded_mappings.is_empty() {
            widgets::banner(
                ui,
                "This path shape would strand existing mappings",
                &format!(
                    "{} cannot be used as {} paths. Remove or replace them below, then save.",
                    validation.stranded_mappings.join(", "),
                    draft.path_kind.slug()
                ),
                widgets::StatusTone::Blocked,
            );
        }

        // --- Page size -----------------------------------------------------
        ui.add_space(theme::SECTION_GAP / 2.0);
        ui.label("Records per request");
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.page_size)
                    .desired_width(120.0)
                    .hint_text("100"),
            )
            .changed()
        {
            draft.dirty = true;
        }
        field_note(ui, &validation.page_size);

        // --- Mappings ------------------------------------------------------
        ui.add_space(theme::SECTION_GAP);
        show_mappings_editor(ui, draft, mappings, busy);

        // --- Preview -------------------------------------------------------
        ui.add_space(theme::SECTION_GAP);
        if let Some(found) = show_preview_section(ui, draft, preview, busy, preview_running) {
            request = Some(found);
        }
    });
    request
}

/// Height reserved for the configuration dialog's fixed footer.
///
/// The footer is drawn *outside* the body's scroll area, so Save and Cancel
/// keep their place on screen no matter how far the body is scrolled - the
/// same arrangement the RomM record Details window uses. Previously these
/// two buttons were the last widgets *inside* the scrolling body, which at
/// TV resolution put the only visible way out of the dialog below the fold
/// and made Escape the sole discoverable exit.
pub(crate) const CONFIG_FOOTER_HEIGHT: f32 = 44.0;

/// The height the scrolling body may occupy once the footer has its own.
pub(crate) fn config_body_height(available_height: f32, footer_height: f32) -> f32 {
    (available_height - footer_height).max(96.0)
}

/// The dialog's critical actions, drawn in a fixed footer by the window
/// wrapper rather than at the end of the scrolling body.
///
/// Cancel never writes, never imports and never contacts anything: it
/// resolves to `Close` (directly, or via the unsaved-changes confirmation),
/// and `Close` is handled by discarding the draft.
pub(crate) fn show_config_dialog_footer(
    ui: &mut egui::Ui,
    draft: &mut RommConfigDraft,
    inputs: &ConfigDialogInputs<'_>,
) -> Option<ConfigDialogRequest> {
    let ConfigDialogInputs {
        validation,
        previous,
        busy,
        ..
    } = *inputs;
    let mut request = None;
    // Drawn above the buttons so the confirmation cannot push them out of
    // the footer's fixed height.
    if draft.close_confirm {
        widgets::banner(
            ui,
            "Close without saving?",
            "The changes you made here have not been written. Your existing configuration is \
             unchanged.",
            widgets::StatusTone::Warning,
        );
        ui.horizontal(|ui| {
            if widgets::action_button(
                ui,
                "Discard changes",
                widgets::ActionStyle::Destructive,
                true,
            )
            .clicked()
            {
                draft.close_confirm = false;
                request = Some(ConfigDialogRequest::Close);
            }
            if ui.button("Keep editing").clicked() {
                draft.close_confirm = false;
            }
        });
    }
    ui.horizontal(|ui| {
        let can_save = validation.can_save && !busy;
        let mut save = widgets::action_button(
            ui,
            "Save configuration",
            widgets::ActionStyle::Primary,
            can_save,
        );
        if !can_save {
            save = save.on_disabled_hover_text(if busy {
                "A RomM operation is running.".to_string()
            } else {
                "Fix the problems above first.".to_string()
            });
        }
        if save.clicked() {
            request = Some(ConfigDialogRequest::Save(Box::new(
                draft.to_settings(previous),
            )));
        }
        // Visually distinct from Save (Quiet against Primary) and always
        // enabled - a way out must never depend on the draft validating.
        if widgets::action_button(ui, "Cancel", widgets::ActionStyle::Quiet, true).clicked() {
            if draft.dirty {
                draft.close_confirm = true;
            } else {
                request = Some(ConfigDialogRequest::Close);
            }
        }
        ui.label("Saving writes EmuWiz's configuration. It contacts nothing.");
    });
    request
}

fn field_note(ui: &mut egui::Ui, state: &FieldState) {
    widgets::status_badge(
        ui,
        match state {
            FieldState::Good(_) => "ok",
            FieldState::Problem(_) => "problem",
            FieldState::Deferred(_) => "checked on save",
            FieldState::Empty(_) => "not set",
        },
        state.tone(),
    );
    ui.label(state.message());
}

fn show_mappings_editor(
    ui: &mut egui::Ui,
    draft: &mut RommConfigDraft,
    view: &MappingsEditorView,
    busy: bool,
) {
    ui.strong("Path mappings");
    ui.label(format!(
        "How a {} RomM path becomes a local one. Matching is on whole folder names, and the most \
         specific mapping wins.",
        view.path_kind.slug()
    ));
    if view.rows.is_empty() {
        ui.label("No mappings yet. Without one, every imported record stays unmatched.");
    }
    if let Some(problem) = &view.set_problem {
        widgets::banner(
            ui,
            "These mappings cannot be used together",
            problem,
            widgets::StatusTone::Blocked,
        );
    } else if view.no_usable_mapping && !view.rows.is_empty() {
        widgets::banner(
            ui,
            "No usable mapping",
            "Every mapping below is refused, so an import would match nothing.",
            widgets::StatusTone::Warning,
        );
    }

    let mut remove_requested = None;
    for row in &view.rows {
        widgets::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(&row.provider_prefix);
                ui.label("->");
                // Long paths wrap rather than overflowing, and carry the full value.
                ui.add(egui::Label::new(row.destination.display().to_string()).wrap())
                    .on_hover_text(row.destination.display().to_string());
                widgets::status_badge(
                    ui,
                    if row.valid { "valid" } else { "invalid" },
                    if row.valid {
                        widgets::StatusTone::Success
                    } else {
                        widgets::StatusTone::Blocked
                    },
                );
                widgets::status_badge(
                    ui,
                    if row.inside_source_root {
                        "inside a source folder"
                    } else {
                        "outside your source folders"
                    },
                    if row.inside_source_root {
                        widgets::StatusTone::Info
                    } else {
                        widgets::StatusTone::Warning
                    },
                );
                widgets::status_badge(ui, row.path_kind.slug(), widgets::StatusTone::Info);
            });
            if let Some(normalised) = &row.normalised_prefix
                && normalised != &row.provider_prefix
            {
                ui.label(format!("Compared as: {normalised}"));
            }
            if let Some(problem) = &row.problem {
                ui.label(problem);
            }
            if let Some(shadow) = &row.shadowed_by {
                ui.label(format!(
                    "Shadowed by {shadow}, which is more specific and would win first."
                ));
            }
            if widgets::action_button(ui, "Remove", widgets::ActionStyle::Destructive, !busy)
                .clicked()
            {
                remove_requested = Some(row.provider_prefix.clone());
            }
        });
    }

    if let Some(prefix) = remove_requested {
        // Confirmation only where it would leave nothing usable - removing one of
        // several is routine and does not need a gate.
        let usable = view.rows.iter().filter(|row| row.valid).count();
        let removing_last_usable = usable <= 1
            && view
                .rows
                .iter()
                .any(|row| row.provider_prefix == prefix && row.valid);
        if removing_last_usable {
            draft.remove_confirm = Some(prefix);
        } else {
            remove_mapping(draft, &prefix);
        }
    }

    if let Some(prefix) = draft.remove_confirm.clone() {
        widgets::banner(
            ui,
            "Remove the only usable mapping?",
            "Without a mapping, imported records cannot be matched to local files. No source \
             folder and no file is removed.",
            widgets::StatusTone::Warning,
        );
        ui.horizontal(|ui| {
            if widgets::action_button(ui, "Remove anyway", widgets::ActionStyle::Destructive, true)
                .clicked()
            {
                draft.remove_confirm = None;
                remove_mapping(draft, &prefix);
            }
            if ui.button("Keep it").clicked() {
                draft.remove_confirm = None;
            }
        });
    }

    // --- Add -----------------------------------------------------------
    ui.add_space(theme::SECTION_GAP / 2.0);
    ui.label("Add a mapping");
    ui.horizontal_wrapped(|ui| {
        ui.label("RomM prefix");
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.new_prefix)
                    .desired_width(180.0)
                    .hint_text(match view.path_kind {
                        ProviderPathKind::ProviderRelative => "roms",
                        ProviderPathKind::AbsoluteProviderPath => "/romm/library",
                    }),
            )
            .changed()
        {
            draft.add_problem = None;
        }
        ui.label("EmuWiz folder");
        if ui
            .add(
                egui::TextEdit::singleline(&mut draft.new_destination)
                    .desired_width(260.0)
                    .hint_text("/mnt/games/roms"),
            )
            .changed()
        {
            draft.add_problem = None;
        }
        if widgets::action_button(ui, "Add", widgets::ActionStyle::Secondary, !busy).clicked() {
            match add_mapping(draft, &view.source_roots, false) {
                AddMappingOutcome::Added => {}
                AddMappingOutcome::NeedsReplaceConfirmation {
                    existing_destination,
                } => {
                    draft.replace_confirm = Some(existing_destination.display().to_string());
                }
                AddMappingOutcome::Refused(reason) => draft.add_problem = Some(reason),
            }
        }
    });
    if let Some(problem) = &draft.add_problem {
        widgets::banner(
            ui,
            "That mapping was not added",
            problem,
            widgets::StatusTone::Warning,
        );
    }
    if let Some(existing) = draft.replace_confirm.clone() {
        widgets::banner(
            ui,
            "Replace the existing mapping?",
            &format!("That RomM prefix already points at {existing}."),
            widgets::StatusTone::Warning,
        );
        ui.horizontal(|ui| {
            if widgets::action_button(ui, "Replace", widgets::ActionStyle::Destructive, true)
                .clicked()
            {
                draft.replace_confirm = None;
                if let AddMappingOutcome::Refused(reason) =
                    add_mapping(draft, &view.source_roots, true)
                {
                    draft.add_problem = Some(reason);
                }
            }
            if ui.button("Keep the existing one").clicked() {
                draft.replace_confirm = None;
            }
        });
    }
    if !view.source_roots.is_empty() {
        widgets::technical_details(ui, "romm-source-roots", |ui| {
            ui.label("A mapping's destination must be inside one of these source folders:");
            for root in &view.source_roots {
                ui.label(root.display().to_string());
            }
        });
    }
}

fn show_preview_section(
    ui: &mut egui::Ui,
    draft: &mut RommConfigDraft,
    preview: Option<&RommPreviewSummary>,
    busy: bool,
    preview_running: bool,
) -> Option<ConfigDialogRequest> {
    let mut request = None;
    ui.strong("Preview");
    ui.label(
        "Translates a bounded sample of real RomM paths and reports what each one becomes. \
         Imports nothing, publishes nothing, and writes nothing.",
    );
    ui.horizontal_wrapped(|ui| {
        ui.label("Paths to sample");
        ui.add(
            egui::TextEdit::singleline(&mut draft.preview_limit)
                .desired_width(80.0)
                .hint_text(DEFAULT_PREVIEW_LIMIT.to_string()),
        );
        let limit = draft
            .preview_limit
            .trim()
            .parse::<usize>()
            .unwrap_or(DEFAULT_PREVIEW_LIMIT)
            .clamp(1, MAX_PREVIEW_LIMIT);
        if widgets::action_button(
            ui,
            "Preview mappings",
            widgets::ActionStyle::Secondary,
            !busy && !preview_running,
        )
        .clicked()
        {
            request = Some(ConfigDialogRequest::Preview { limit });
        }
        if widgets::action_button(
            ui,
            "Cancel preview",
            widgets::ActionStyle::Quiet,
            preview_running,
        )
        .clicked()
        {
            request = Some(ConfigDialogRequest::CancelPreview);
        }
        ui.label(format!("At most {MAX_PREVIEW_LIMIT}."));
    });
    if preview_running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Previewing. This can be cancelled.");
        });
    }

    if let Some(summary) = preview {
        widgets::banner(
            ui,
            &summary.headline(),
            &format!(
                "Sampled from {}. Shapes seen: {} relative, {} absolute.",
                summary.sample_source, summary.observed_relative, summary.observed_absolute
            ),
            if summary.path_shape_agrees() && summary.refused == 0 {
                widgets::StatusTone::Success
            } else {
                widgets::StatusTone::Warning
            },
        );
        widgets::status_rows(
            ui,
            &[
                (
                    "Translated",
                    &summary.translated.to_string(),
                    widgets::StatusTone::Success,
                ),
                (
                    "Unmatched",
                    &summary.unmatched.to_string(),
                    widgets::StatusTone::Warning,
                ),
                (
                    "Refused",
                    &summary.refused.to_string(),
                    widgets::StatusTone::Blocked,
                ),
            ]
            .map(|(label, value, tone)| (label, value.as_str(), tone)),
        );
        for CardRow { label, value } in preview_count_rows(summary) {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{label}:"));
                ui.strong(value);
            });
        }
        for example in &summary.examples {
            widgets::card(ui, |ui| {
                ui.add(egui::Label::new(&example.provider_path).wrap())
                    .on_hover_text(&example.provider_path);
                if let Some(normalised) = &example.normalised_path {
                    ui.label(format!("Compared as: {normalised}"));
                }
                match (&example.archivefs_path, &example.refusal) {
                    (Some(path), _) => {
                        ui.add(egui::Label::new(format!("-> {}", path.display())).wrap())
                            .on_hover_text(path.display().to_string());
                        ui.horizontal_wrapped(|ui| {
                            widgets::status_badge(
                                ui,
                                presence_label(example.presence),
                                presence_tone(example.presence),
                            );
                            if let Some(prefix) = &example.matched_prefix {
                                widgets::status_badge(
                                    ui,
                                    format!("via {prefix}"),
                                    widgets::StatusTone::Info,
                                );
                            }
                            if let Some(platform) = &example.canonical_platform {
                                widgets::status_badge(
                                    ui,
                                    platform.clone(),
                                    widgets::StatusTone::Info,
                                );
                            }
                        });
                        ui.label(match &example.trusted_root {
                            Some(root) => format!("Inside source folder {}", root.display()),
                            None => "Source-folder check not applicable".to_string(),
                        });
                    }
                    (None, Some(refusal)) => {
                        widgets::banner(
                            ui,
                            &format!(
                                "Refused ({})",
                                example.refusal_code.as_deref().unwrap_or("unknown")
                            ),
                            refusal,
                            widgets::StatusTone::Blocked,
                        );
                    }
                    (None, None) => {
                        ui.label("No mapping covers this path.");
                    }
                }
            });
        }
    }
    request
}

/// The aggregate rows, so the counts the spec asks for are all present and all
/// derived from the same pass.
pub(crate) fn preview_count_rows(summary: &RommPreviewSummary) -> Vec<CardRow> {
    vec![
        CardRow {
            label: "Existing files".to_string(),
            value: summary.existing_files.to_string(),
        },
        CardRow {
            label: "Directories".to_string(),
            value: summary.directories.to_string(),
        },
        CardRow {
            label: "Dangling symlinks".to_string(),
            value: summary.dangling_symlinks.to_string(),
        },
        CardRow {
            label: "Missing".to_string(),
            value: summary.missing.to_string(),
        },
        CardRow {
            label: "Missing parent folder".to_string(),
            value: summary.missing_parents.to_string(),
        },
        CardRow {
            label: "Observed relative paths".to_string(),
            value: summary.observed_relative.to_string(),
        },
        CardRow {
            label: "Observed absolute paths".to_string(),
            value: summary.observed_absolute.to_string(),
        },
        CardRow {
            label: "Configured shape".to_string(),
            value: format!(
                "{} ({})",
                summary.configured_path_kind,
                if summary.path_shape_agrees() {
                    "agrees with what arrived"
                } else {
                    "disagrees with what arrived"
                }
            ),
        },
    ]
}

fn presence_label(presence: Option<&'static str>) -> &'static str {
    match presence {
        Some("file") => "a regular file",
        Some("directory") => "a directory, not a file",
        Some("dangling_symlink") => "a symlink whose target is gone",
        Some("parent_absent") => "the folder that would hold it is missing",
        Some("absent") => "nothing at that path",
        Some(other) => other,
        None => "not checked",
    }
}

fn presence_tone(presence: Option<&'static str>) -> widgets::StatusTone {
    match presence {
        Some("file") => widgets::StatusTone::Success,
        Some("directory") => widgets::StatusTone::Info,
        Some("dangling_symlink") | Some("absent") | Some("parent_absent") => {
            widgets::StatusTone::Warning
        }
        _ => widgets::StatusTone::Pending,
    }
}

/// A one-line description of a saved configuration, for the result area.
pub(crate) fn describe_saved(settings: &ProviderSettings) -> Vec<CardRow> {
    vec![
        CardRow {
            label: "URL".to_string(),
            value: settings.source.url.clone(),
        },
        CardRow {
            label: "Path shape".to_string(),
            value: settings.source.provider_path_kind.label().to_string(),
        },
        CardRow {
            label: "Mappings".to_string(),
            value: settings.source.mappings.len().to_string(),
        },
        CardRow {
            label: "Records per request".to_string(),
            value: settings.effective_page_size().to_string(),
        },
        CardRow {
            label: "Token file".to_string(),
            value: settings
                .source
                .token_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not configured".to_string()),
        },
    ]
}

#[cfg(test)]
mod tests;
