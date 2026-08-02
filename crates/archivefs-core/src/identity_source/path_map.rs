//! Translating a provider's paths into ArchiveFS's own.
//!
//! A RomM instance describes its files in its own terms; ArchiveFS sees the same
//! files somewhere else. Import is therefore useless without a mapping, and
//! dangerous with a careless one - a mapping is a rule for turning text a remote
//! server sent into a local filesystem path.
//!
//! # Two path shapes, declared rather than guessed
//!
//! RomM 5.1.0 reports **provider-relative** paths:
//!
//! ```text
//! roms/sharp-x68000/_ReadMe_.txt
//! roms/atari-st/'Nam 1965-1975 (Europe).stx
//! ```
//!
//! Other installations expose **absolute** paths (`/romm/library/nes/x.zip`).
//! Both are supported, but which one an instance uses is a declared setting -
//! [`ProviderPathKind`] - and never inferred from the shape of an individual
//! string. Guessing per path is what would let a hostile response switch shapes
//! to slip past a mapping; declaring it once means a path of the wrong shape is a
//! refusal with a clear reason instead of a silent reinterpretation.
//!
//! # Rules
//!
//! - **Whole components only.** `roms` matches `roms/gb/x.gb` but never
//!   `roms-backup/x.gb`. This is a path comparison, not a string prefix.
//! - **Longest prefix wins.** With both `roms` and `roms/retro`, a path under the
//!   latter uses the latter.
//! - **Nothing is repaired.** `..`, `.`, an empty component, a backslash, a drive
//!   letter, a UNC prefix or a control character all *refuse* the path. An
//!   earlier version of this module quietly cleaned some of those up; a path that
//!   needs cleaning is a path whose meaning is not agreed on, and translating it
//!   anyway is how a mapping ends up pointing somewhere nobody intended.
//! - **Trusted roots.** A translation must land inside a configured source root -
//!   checked when the mapping is configured *and* again for every path.
//! - **Provenance is kept.** The exact string the provider sent is never
//!   discarded, so what the mapping did can always be audited against it.
//!
//! Nothing here touches the filesystem: no `canonicalize`, no metadata, no
//! existence check. Translation is text arriving over a network being compared
//! with text a person configured, and Test 35 enforces that by scanning this
//! file's own source.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The longest a provider path may be before it is refused. A real library path
/// is far shorter; this only exists so a hostile response cannot hand over
/// something unbounded.
pub const MAX_PROVIDER_PATH_BYTES: usize = 4096;

/// The most mappings one source may have configured.
pub const MAX_MAPPINGS: usize = 64;

/// Which shape of path an installation reports.
///
/// A property of the instance, set deliberately, not read off individual paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderPathKind {
    /// Paths relative to the provider's own library base, as RomM 5.1.0 reports:
    /// `roms/gb/game.gb`. A leading `/` is refused in this mode.
    ProviderRelative,
    /// Paths absolute on the provider's filesystem: `/romm/library/gb/game.gb`.
    /// A path without a leading `/` is refused in this mode.
    AbsoluteProviderPath,
}

impl Default for ProviderPathKind {
    /// Absolute, because that is what every mapping written before this setting
    /// existed had to be - validation refused anything else. A configuration
    /// deserialised without the field therefore means "absolute", which is a
    /// recorded fact about those files rather than a guess.
    fn default() -> Self {
        Self::AbsoluteProviderPath
    }
}

impl ProviderPathKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::ProviderRelative => "relative",
            Self::AbsoluteProviderPath => "absolute",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ProviderRelative => "provider-relative (e.g. roms/gb/game.gb)",
            Self::AbsoluteProviderPath => "absolute (e.g. /romm/library/gb/game.gb)",
        }
    }

    /// Parses the spelling the CLI accepts and prints.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "relative" | "provider_relative" | "provider-relative" => Some(Self::ProviderRelative),
            "absolute" | "absolute_provider_path" | "absolute-provider-path" => {
                Some(Self::AbsoluteProviderPath)
            }
            _ => None,
        }
    }

    /// The shape a single path appears to have.
    ///
    /// For **advice only** - reporting "your server sends relative paths, run
    /// `configure --path-kind relative`". Never used to decide how to translate:
    /// that is what the declared kind is for.
    pub fn observed_in(path: &str) -> Self {
        if path.trim_start().starts_with('/') {
            Self::AbsoluteProviderPath
        } else {
            Self::ProviderRelative
        }
    }
}

/// One configured translation rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMapping {
    /// The path as the provider reports it: `roms` when the instance is
    /// relative, `/romm/library` when it is absolute.
    pub provider_prefix: String,
    /// Where those files are for ArchiveFS, e.g. `/mnt/games/roms`.
    pub archivefs_prefix: PathBuf,
}

/// Why a mapping or a path cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum MappingRefusal {
    EmptyPrefix,
    /// An ArchiveFS destination that is not absolute. A local path has to be.
    NotAbsolute {
        side: &'static str,
        value: String,
    },
    /// A relative provider path where the instance is declared absolute.
    UnexpectedlyRelative {
        side: &'static str,
        value: String,
    },
    /// An absolute provider path where the instance is declared relative. The
    /// common cause is a real mismatch between the setting and the server, so the
    /// message says how to change the setting.
    UnexpectedlyAbsolute {
        side: &'static str,
        value: String,
    },
    /// A `..` component: traversal, refused rather than resolved.
    NonNormalComponent {
        side: &'static str,
        value: String,
    },
    /// A `.` component. Meaningless, so its presence means the sender and the
    /// receiver do not agree on what the path is.
    DotComponent {
        side: &'static str,
        value: String,
    },
    /// An empty component, from `//` or a trailing separator in a record path.
    EmptyComponent {
        side: &'static str,
        value: String,
    },
    /// A control character, including NUL.
    ///
    /// Carries no copy of the offending text: echoing raw control bytes into a
    /// terminal or a log is its own problem.
    ControlCharacter {
        side: &'static str,
        /// Byte offset of the first one, so it can still be located.
        offset: usize,
        /// The character, escaped.
        escaped: String,
    },
    /// A backslash. Not treated as a separator: on the Linux filesystems RomM
    /// runs on it is a legal character *in a filename*, so reinterpreting it as a
    /// separator would invent a directory level that does not exist.
    WindowsSeparator {
        side: &'static str,
        value: String,
    },
    /// A `C:`-style drive prefix.
    DrivePrefix {
        side: &'static str,
        value: String,
    },
    /// A `\\server\share` UNC path.
    UncPath {
        side: &'static str,
        value: String,
    },
    TooLong {
        bytes: usize,
        maximum: usize,
    },
    TooMany {
        count: usize,
        maximum: usize,
    },
    /// The destination is not inside any configured source root.
    OutsideTrustedRoots {
        value: String,
    },
    /// Two mappings translate to the same destination, which would make the
    /// result depend on ordering.
    DuplicateDestination {
        value: String,
    },
    /// Two mappings declare the same provider prefix.
    DuplicateSource {
        value: String,
    },
}

impl MappingRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::EmptyPrefix => {
                "a mapping needs both a RomM path and an ArchiveFS path".to_string()
            }
            Self::NotAbsolute { side, value } => {
                format!("the {side} path `{value}` must be absolute")
            }
            Self::UnexpectedlyRelative { side, value } => format!(
                "the {side} path `{value}` is relative, but this source is configured for absolute \
                 provider paths; if your RomM reports paths like `roms/gb/game.gb`, run \
                 `configure --path-kind relative`"
            ),
            Self::UnexpectedlyAbsolute { side, value } => format!(
                "the {side} path `{value}` is absolute, but this source is configured for \
                 provider-relative paths; if your RomM reports paths like \
                 `/romm/library/gb/game.gb`, run `configure --path-kind absolute`"
            ),
            Self::NonNormalComponent { side, value } => {
                format!("the {side} path `{value}` must not contain a `..` component")
            }
            Self::DotComponent { side, value } => format!(
                "the {side} path `{value}` contains a `.` component; it is not repaired, because a \
                 path that needs repairing is one whose meaning is not agreed on"
            ),
            Self::EmptyComponent { side, value } => format!(
                "the {side} path `{value}` contains an empty component, from a doubled or trailing \
                 separator"
            ),
            Self::ControlCharacter {
                side,
                offset,
                escaped,
            } => format!(
                "the {side} path contains the control character {escaped} at byte {offset}; the \
                 path is not shown here because printing it would emit that character"
            ),
            Self::WindowsSeparator { side, value } => format!(
                "the {side} path `{value}` contains a backslash, which is not treated as a \
                 separator: on Linux it is a legal character in a filename, so reading it as one \
                 would invent a directory level"
            ),
            Self::DrivePrefix { side, value } => {
                format!(
                    "the {side} path `{value}` starts with a drive letter, which is not supported"
                )
            }
            Self::UncPath { side, value } => {
                format!("the {side} path `{value}` is a UNC network path, which is not supported")
            }
            Self::TooLong { bytes, maximum } => {
                format!("that path is {bytes} bytes, over the {maximum}-byte limit")
            }
            Self::TooMany { count, maximum } => {
                format!("{count} mappings is over the {maximum} this source allows")
            }
            Self::OutsideTrustedRoots { value } => format!(
                "`{value}` is not inside any configured source folder; an imported identity must \
                 point at a library ArchiveFS already knows about"
            ),
            Self::DuplicateDestination { value } => format!(
                "two mappings both translate to `{value}`, which would make the result depend on \
                 which was applied first"
            ),
            Self::DuplicateSource { value } => {
                format!("two mappings both start from `{value}`")
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::EmptyPrefix => "empty_prefix",
            Self::NotAbsolute { .. } => "not_absolute",
            Self::UnexpectedlyRelative { .. } => "unexpectedly_relative",
            Self::UnexpectedlyAbsolute { .. } => "unexpectedly_absolute",
            Self::NonNormalComponent { .. } => "non_normal_component",
            Self::DotComponent { .. } => "dot_component",
            Self::EmptyComponent { .. } => "empty_component",
            Self::ControlCharacter { .. } => "control_character",
            Self::WindowsSeparator { .. } => "windows_separator",
            Self::DrivePrefix { .. } => "drive_prefix",
            Self::UncPath { .. } => "unc_path",
            Self::TooLong { .. } => "too_long",
            Self::TooMany { .. } => "too_many",
            Self::OutsideTrustedRoots { .. } => "outside_trusted_roots",
            Self::DuplicateDestination { .. } => "duplicate_destination",
            Self::DuplicateSource { .. } => "duplicate_source",
        }
    }
}

/// A validated set of mappings, sorted so the longest provider prefix is tried
/// first. Constructing one is the only way to translate a path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMappings {
    /// Longest provider prefix first, so the first match is the right one.
    ordered: Vec<PathMapping>,
    /// The shape every provider path from this source must have.
    kind: ProviderPathKind,
    /// The configured source folders every translation must land inside. Empty
    /// means "not yet known", which is the case for a preview run before a
    /// library has been configured.
    trusted_roots: Vec<PathBuf>,
}

impl PathMappings {
    /// Validates and orders a set of mappings for one declared path shape.
    ///
    /// `trusted_roots` are the configured source folders; every destination must
    /// be inside one, and so must every later translation. Pass an empty slice to
    /// skip that check.
    pub fn validate(
        mappings: &[PathMapping],
        trusted_roots: &[PathBuf],
        kind: ProviderPathKind,
    ) -> Result<Self, MappingRefusal> {
        if mappings.len() > MAX_MAPPINGS {
            return Err(MappingRefusal::TooMany {
                count: mappings.len(),
                maximum: MAX_MAPPINGS,
            });
        }
        let mut seen_sources: Vec<String> = Vec::new();
        let mut seen_destinations: Vec<PathBuf> = Vec::new();
        let mut validated: Vec<PathMapping> = Vec::new();

        for mapping in mappings {
            // A prefix is typed by a person, so a trailing separator is tolerated
            // and normalised away. A *record* path gets no such courtesy.
            let provider = normalise_configured_prefix(&mapping.provider_prefix, kind)?;
            let destination = &mapping.archivefs_prefix;
            if destination.as_os_str().is_empty() {
                return Err(MappingRefusal::EmptyPrefix);
            }
            if !destination.is_absolute() {
                return Err(MappingRefusal::NotAbsolute {
                    side: "ArchiveFS",
                    value: destination.display().to_string(),
                });
            }
            if destination
                .components()
                .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
            {
                return Err(MappingRefusal::NonNormalComponent {
                    side: "ArchiveFS",
                    value: destination.display().to_string(),
                });
            }
            if !trusted_roots.is_empty() && !is_inside_any(destination, trusted_roots) {
                return Err(MappingRefusal::OutsideTrustedRoots {
                    value: destination.display().to_string(),
                });
            }
            if seen_sources.iter().any(|seen| seen == &provider) {
                return Err(MappingRefusal::DuplicateSource { value: provider });
            }
            if seen_destinations.iter().any(|seen| seen == destination) {
                return Err(MappingRefusal::DuplicateDestination {
                    value: destination.display().to_string(),
                });
            }
            seen_sources.push(provider.clone());
            seen_destinations.push(destination.clone());
            validated.push(PathMapping {
                provider_prefix: provider,
                archivefs_prefix: destination.clone(),
            });
        }

        // Longest provider prefix first, by component count then by length, so
        // the more specific rule always wins. Ties break on the text so the
        // order is deterministic.
        validated.sort_by(|left, right| {
            component_count(&right.provider_prefix)
                .cmp(&component_count(&left.provider_prefix))
                .then_with(|| right.provider_prefix.len().cmp(&left.provider_prefix.len()))
                .then_with(|| left.provider_prefix.cmp(&right.provider_prefix))
        });
        Ok(Self {
            ordered: validated,
            kind,
            trusted_roots: trusted_roots.to_vec(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ordered.len()
    }

    /// The path shape this set expects.
    pub fn kind(&self) -> ProviderPathKind {
        self.kind
    }

    /// The mappings, longest provider prefix first.
    pub fn as_slice(&self) -> &[PathMapping] {
        &self.ordered
    }

    /// Translates one provider path, or explains why it could not be.
    ///
    /// Pure: no filesystem access at all, so a preview costs nothing and an
    /// import cannot be slowed by a translation.
    pub fn translate(&self, provider_path: &str) -> PathTranslation {
        let normalised = match normalise_provider_path(provider_path, self.kind, "RomM") {
            Ok(path) => path,
            Err(refusal) => {
                return PathTranslation::Refused {
                    provider_path: provider_path.to_string(),
                    refusal,
                };
            }
        };
        for mapping in &self.ordered {
            let Some(relative) = strip_component_prefix(&normalised, &mapping.provider_prefix)
            else {
                continue;
            };
            let mut translated = mapping.archivefs_prefix.clone();
            for component in relative.split('/').filter(|part| !part.is_empty()) {
                translated.push(component);
            }
            // Belt and braces: the result must still be inside the destination it
            // was built from. Nothing above can produce a path that is not, but
            // this is the boundary where remote text becomes a local path, and
            // the check costs nothing.
            if !translated.starts_with(&mapping.archivefs_prefix) {
                return PathTranslation::Refused {
                    provider_path: provider_path.to_string(),
                    refusal: MappingRefusal::NonNormalComponent {
                        side: "RomM",
                        value: normalised,
                    },
                };
            }
            // Re-checked per path, not only per mapping: when the mappings were
            // validated before any source folder was configured, this is the
            // first opportunity to apply the rule at all.
            let trusted_root = if self.trusted_roots.is_empty() {
                None
            } else {
                match containing_root(&translated, &self.trusted_roots) {
                    Some(root) => Some(root),
                    None => {
                        return PathTranslation::Refused {
                            provider_path: provider_path.to_string(),
                            refusal: MappingRefusal::OutsideTrustedRoots {
                                value: translated.display().to_string(),
                            },
                        };
                    }
                }
            };
            return PathTranslation::Translated {
                provider_path: provider_path.to_string(),
                normalised_path: normalised,
                kind: self.kind,
                archivefs_path: translated,
                matched_prefix: mapping.provider_prefix.clone(),
                trusted_root,
            };
        }
        PathTranslation::Unmatched {
            provider_path: provider_path.to_string(),
            normalised_path: normalised,
            kind: self.kind,
        }
    }
}

/// The outcome of translating one path.
///
/// `provider_path` is always the exact string the provider sent, unmodified.
/// `normalised_path` is the form the comparison was made against, so the two can
/// be shown side by side when they differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum PathTranslation {
    Translated {
        provider_path: String,
        normalised_path: String,
        kind: ProviderPathKind,
        archivefs_path: PathBuf,
        /// Which mapping applied, so a preview can show why.
        matched_prefix: String,
        /// The configured source folder the result landed in, when the roots were
        /// known. `None` means the check was not applicable, never that it failed.
        trusted_root: Option<PathBuf>,
    },
    /// No mapping covers this path. Not an error: a RomM library may legitimately
    /// contain platforms ArchiveFS does not have.
    Unmatched {
        provider_path: String,
        normalised_path: String,
        kind: ProviderPathKind,
    },
    /// The provider path itself is unusable.
    Refused {
        provider_path: String,
        refusal: MappingRefusal,
    },
}

impl PathTranslation {
    pub fn archivefs_path(&self) -> Option<&Path> {
        match self {
            Self::Translated { archivefs_path, .. } => Some(archivefs_path),
            _ => None,
        }
    }

    pub fn is_translated(&self) -> bool {
        matches!(self, Self::Translated { .. })
    }

    /// The exact string the provider sent, whatever the outcome.
    pub fn provider_path(&self) -> &str {
        match self {
            Self::Translated { provider_path, .. }
            | Self::Unmatched { provider_path, .. }
            | Self::Refused { provider_path, .. } => provider_path,
        }
    }

    pub fn refusal(&self) -> Option<&MappingRefusal> {
        match self {
            Self::Refused { refusal, .. } => Some(refusal),
            _ => None,
        }
    }
}

/// Normalises one configured provider prefix to its canonical comparison form.
///
/// Public so a caller can ask "what would this prefix become?" without building a
/// whole mapping set - which is what adding, replacing and removing a single
/// mapping all need in order to compare against what is already stored.
pub fn normalise_prefix(prefix: &str, kind: ProviderPathKind) -> Result<String, MappingRefusal> {
    normalise_configured_prefix(prefix, kind)
}

/// Normalises a configured prefix, which a person typed.
///
/// The one courtesy a record path does not get: a trailing separator is trimmed,
/// because `roms/` and `roms` plainly mean the same thing when someone types
/// them. Everything after that is the strict rule.
fn normalise_configured_prefix(
    path: &str,
    kind: ProviderPathKind,
) -> Result<String, MappingRefusal> {
    let trimmed = path.trim();
    let without_trailing = match trimmed.trim_end_matches('/') {
        // An absolute prefix of `/` is the whole provider root, not an empty
        // string: trimming must not turn one into the other.
        "" if trimmed.starts_with('/') => "/",
        other => other,
    };
    normalise_provider_path(without_trailing, kind, "RomM")
}

/// Normalises a provider path for comparison, refusing anything ambiguous.
///
/// Does not resolve symlinks, does not canonicalise, does not touch the
/// filesystem, and does not decode any escaping. This is text arriving over a
/// network: the only safe thing to do with it is compare it, and the only safe
/// response to something unexpected is to refuse.
///
/// Check order matters. The control-character test runs first so that no later
/// refusal can quote raw control bytes back into a log or a terminal.
fn normalise_provider_path(
    path: &str,
    kind: ProviderPathKind,
    side: &'static str,
) -> Result<String, MappingRefusal> {
    if path.len() > MAX_PROVIDER_PATH_BYTES {
        return Err(MappingRefusal::TooLong {
            bytes: path.len(),
            maximum: MAX_PROVIDER_PATH_BYTES,
        });
    }
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(MappingRefusal::EmptyPrefix);
    }
    // First, so every refusal below can safely quote the path.
    if let Some((offset, character)) = trimmed
        .char_indices()
        .find(|(_, character)| character.is_control())
    {
        return Err(MappingRefusal::ControlCharacter {
            side,
            offset,
            escaped: character.escape_debug().to_string(),
        });
    }
    // UNC before the general backslash rule, so `\\server\share` is named for
    // what it is rather than reported as a stray separator.
    if trimmed.starts_with("\\\\") || trimmed.starts_with("//") {
        return Err(MappingRefusal::UncPath {
            side,
            value: trimmed.to_string(),
        });
    }
    if trimmed.contains('\\') {
        return Err(MappingRefusal::WindowsSeparator {
            side,
            value: trimmed.to_string(),
        });
    }
    if has_drive_prefix(trimmed) {
        return Err(MappingRefusal::DrivePrefix {
            side,
            value: trimmed.to_string(),
        });
    }

    let absolute = trimmed.starts_with('/');
    match (kind, absolute) {
        (ProviderPathKind::AbsoluteProviderPath, false) => {
            return Err(MappingRefusal::UnexpectedlyRelative {
                side,
                value: trimmed.to_string(),
            });
        }
        (ProviderPathKind::ProviderRelative, true) => {
            return Err(MappingRefusal::UnexpectedlyAbsolute {
                side,
                value: trimmed.to_string(),
            });
        }
        _ => {}
    }

    // The provider root, as a mapping prefix. Handled before the component scan,
    // which would otherwise see one empty component.
    if trimmed == "/" {
        return Ok("/".to_string());
    }

    let body = if absolute {
        trimmed.strip_prefix('/').unwrap_or(trimmed)
    } else {
        trimmed
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in body.split('/') {
        match part {
            "" => {
                return Err(MappingRefusal::EmptyComponent {
                    side,
                    value: trimmed.to_string(),
                });
            }
            "." => {
                return Err(MappingRefusal::DotComponent {
                    side,
                    value: trimmed.to_string(),
                });
            }
            ".." => {
                return Err(MappingRefusal::NonNormalComponent {
                    side,
                    value: trimmed.to_string(),
                });
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    Ok(if absolute {
        format!("/{joined}")
    } else {
        joined
    })
}

/// Whether the path begins with a `C:`-style drive letter.
///
/// Checked on the first component only: a colon elsewhere is a perfectly legal
/// character in a Linux filename and plenty of ROM sets contain one.
fn has_drive_prefix(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or(path);
    let mut characters = first.chars();
    match (characters.next(), characters.next(), characters.next()) {
        (Some(letter), Some(':'), None) => letter.is_ascii_alphabetic(),
        _ => false,
    }
}

/// Strips `prefix` from `path` on a component boundary, returning the remainder.
///
/// This is what makes `roms-backup` fail to match `roms`: a string prefix would
/// accept it, a component comparison does not.
fn strip_component_prefix(path: &str, prefix: &str) -> Option<String> {
    if prefix == "/" {
        return Some(path.trim_start_matches('/').to_string());
    }
    let remainder = path.strip_prefix(prefix)?;
    if remainder.is_empty() {
        return Some(String::new());
    }
    // The next character has to be a separator, or the prefix ended mid-component.
    remainder.strip_prefix('/').map(|rest| rest.to_string())
}

fn component_count(path: &str) -> usize {
    path.split('/').filter(|part| !part.is_empty()).count()
}

/// Whether `candidate` is inside one of `roots`, on component boundaries.
fn is_inside_any(candidate: &Path, roots: &[PathBuf]) -> bool {
    containing_root(candidate, roots).is_some()
}

/// Which root contains `candidate`, if any.
fn containing_root(candidate: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .find(|root| candidate.starts_with(root))
        .cloned()
}

/// A preview of how a set of mappings would treat some sample paths.
///
/// Built before importing anything, so a person can see the translation is what
/// they meant while the cost of being wrong is still zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MappingPreview {
    pub translations: Vec<PathTranslation>,
    pub translated: usize,
    pub unmatched: usize,
    pub refused: usize,
    /// The shape the samples actually had, counted rather than assumed.
    ///
    /// When this disagrees with the configured kind, that is almost always the
    /// real problem, and it is what the preview should say first.
    pub observed_relative: usize,
    pub observed_absolute: usize,
    pub configured_kind: ProviderPathKind,
}

impl MappingPreview {
    pub fn build(mappings: &PathMappings, sample_paths: &[String]) -> Self {
        let translations: Vec<PathTranslation> = sample_paths
            .iter()
            .map(|path| mappings.translate(path))
            .collect();
        let mut preview = Self {
            translated: 0,
            unmatched: 0,
            refused: 0,
            observed_relative: 0,
            observed_absolute: 0,
            configured_kind: mappings.kind(),
            translations,
        };
        for path in sample_paths {
            match ProviderPathKind::observed_in(path) {
                ProviderPathKind::ProviderRelative => preview.observed_relative += 1,
                ProviderPathKind::AbsoluteProviderPath => preview.observed_absolute += 1,
            }
        }
        for translation in &preview.translations {
            match translation {
                PathTranslation::Translated { .. } => preview.translated += 1,
                PathTranslation::Unmatched { .. } => preview.unmatched += 1,
                PathTranslation::Refused { .. } => preview.refused += 1,
            }
        }
        preview
    }

    /// The kind the samples suggest, when it disagrees with what is configured.
    ///
    /// `None` when they agree or when there is nothing to go on. Advice, offered
    /// only after translation has already been done by the declared rule.
    pub fn suggested_kind(&self) -> Option<ProviderPathKind> {
        let (relative, absolute) = (self.observed_relative, self.observed_absolute);
        let observed = match (relative, absolute) {
            (0, 0) => return None,
            (relative, absolute) if relative > absolute => ProviderPathKind::ProviderRelative,
            (relative, absolute) if absolute > relative => ProviderPathKind::AbsoluteProviderPath,
            _ => return None,
        };
        (observed != self.configured_kind).then_some(observed)
    }
}
