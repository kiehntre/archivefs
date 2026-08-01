//! Translating a provider's paths into ArchiveFS's own.
//!
//! A RomM instance sees its library at `/romm/library`; ArchiveFS sees the same
//! files at `/mnt/games/roms`. Import is therefore useless without a mapping,
//! and dangerous with a careless one - a mapping is a rule for turning text a
//! remote server sent into a local filesystem path.
//!
//! # Rules
//!
//! - **Whole components only.** `/romm/library` matches `/romm/library/nes/x.zip`
//!   but never `/romm/library-backup/x.zip`. This is a path comparison, not a
//!   string prefix.
//! - **Longest prefix wins.** With both `/romm/library` and
//!   `/romm/library/retro`, a path under the latter uses the latter.
//! - **No traversal.** A provider path containing `..` is refused outright, and
//!   so is any translation whose result would leave its own destination.
//! - **Trusted roots.** A translation must land inside a configured source root.
//!   A mapping that points somewhere else is refused when it is configured, not
//!   when it is used.
//! - **Provenance is kept.** The original provider path is never discarded.
//!
//! Nothing here touches the filesystem except an optional containment check
//! against already-canonical trusted roots, and nothing here ever writes.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The longest a provider path may be before it is refused. A real library path
/// is far shorter; this only exists so a hostile response cannot hand over
/// something unbounded.
pub const MAX_PROVIDER_PATH_BYTES: usize = 4096;

/// The most mappings one source may have configured.
pub const MAX_MAPPINGS: usize = 64;

/// One configured translation rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathMapping {
    /// The path as the provider reports it, e.g. `/romm/library`.
    pub provider_prefix: String,
    /// Where those files are for ArchiveFS, e.g. `/mnt/games/roms`.
    pub archivefs_prefix: PathBuf,
}

/// Why a mapping cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum MappingRefusal {
    EmptyPrefix,
    /// Both sides must be absolute, or "longest prefix" and containment have no
    /// meaning.
    NotAbsolute {
        side: &'static str,
        value: String,
    },
    /// A `.` or `..` component in a configured prefix.
    NonNormalComponent {
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
            Self::NonNormalComponent { side, value } => {
                format!("the {side} path `{value}` must not contain a `.` or `..` component")
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
            Self::NonNormalComponent { .. } => "non_normal_component",
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
}

impl PathMappings {
    /// Validates and orders a set of mappings.
    ///
    /// `trusted_roots` are the configured source folders; every destination must
    /// be inside one. Pass an empty slice to skip that check, which is what the
    /// mapping *preview* does before a library has been configured.
    pub fn validate(
        mappings: &[PathMapping],
        trusted_roots: &[PathBuf],
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
            let provider = normalise_provider_path(&mapping.provider_prefix)?;
            if provider == "/" {
                // Mapping the whole provider root is legal but must still be a
                // real absolute path; it is the widest possible rule and simply
                // sorts last.
            }
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
        Ok(Self { ordered: validated })
    }

    pub fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ordered.len()
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
        let normalised = match normalise_provider_path(provider_path) {
            Ok(path) => path,
            Err(refusal) => {
                return PathTranslation::Refused {
                    provider_path: provider_path.to_string(),
                    refusal,
                };
            }
        };
        for mapping in &self.ordered {
            if let Some(relative) = strip_component_prefix(&normalised, &mapping.provider_prefix) {
                let mut translated = mapping.archivefs_prefix.clone();
                for component in relative.split('/').filter(|part| !part.is_empty()) {
                    translated.push(component);
                }
                // Belt and braces: the result must still be inside the
                // destination it was built from. Nothing above can produce a
                // path that is not, but the check is cheap and this is the
                // boundary where remote text becomes a local path.
                if !translated.starts_with(&mapping.archivefs_prefix) {
                    return PathTranslation::Refused {
                        provider_path: provider_path.to_string(),
                        refusal: MappingRefusal::NonNormalComponent {
                            side: "RomM",
                            value: provider_path.to_string(),
                        },
                    };
                }
                return PathTranslation::Translated {
                    provider_path: normalised,
                    archivefs_path: translated,
                    matched_prefix: mapping.provider_prefix.clone(),
                };
            }
        }
        PathTranslation::Unmatched {
            provider_path: normalised,
        }
    }
}

/// The outcome of translating one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum PathTranslation {
    Translated {
        provider_path: String,
        archivefs_path: PathBuf,
        /// Which mapping applied, so a preview can show why.
        matched_prefix: String,
    },
    /// No mapping covers this path. Not an error: a RomM library may legitimately
    /// contain platforms ArchiveFS does not have.
    Unmatched { provider_path: String },
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
}

/// Normalises a provider path for comparison.
///
/// Accepts the Windows-style separators a provider on Windows would report, and
/// refuses traversal and over-long input. Does *not* resolve symlinks or touch
/// the filesystem: this is text arriving over a network, and the only safe thing
/// to do with it is compare it.
fn normalise_provider_path(path: &str) -> Result<String, MappingRefusal> {
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
    // A provider running on Windows reports backslashes; treat them as
    // separators so a mapping written with forward slashes still applies.
    let unified = trimmed.replace('\\', "/");
    if !unified.starts_with('/') {
        return Err(MappingRefusal::NotAbsolute {
            side: "RomM",
            value: trimmed.to_string(),
        });
    }
    // Collapse repeated separators and refuse traversal. `.` is dropped as
    // meaningless; `..` is refused rather than resolved, because resolving it
    // would let a remote path climb out of its own mapping.
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(MappingRefusal::NonNormalComponent {
                    side: "RomM",
                    value: trimmed.to_string(),
                });
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Ok("/".to_string());
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Strips `prefix` from `path` on a component boundary, returning the remainder.
///
/// This is what makes `/romm/library-backup` fail to match `/romm/library`: a
/// string prefix would accept it, a component comparison does not.
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
    roots.iter().any(|root| candidate.starts_with(root))
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
            translations,
        };
        for translation in &preview.translations {
            match translation {
                PathTranslation::Translated { .. } => preview.translated += 1,
                PathTranslation::Unmatched { .. } => preview.unmatched += 1,
                PathTranslation::Refused { .. } => preview.refused += 1,
            }
        }
        preview
    }
}
