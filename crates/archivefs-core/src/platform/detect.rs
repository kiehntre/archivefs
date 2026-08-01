//! Evidence-based platform detection.
//!
//! Detection answers three questions, not one: *which* platform, *how sure*,
//! and *on what evidence*. A caller that only wants the first can use
//! [`PlatformDetectionReport::platform`], but the confidence and the evidence
//! are always computed, because a confident-looking wrong answer is worse than
//! an honest "unknown".
//!
//! # Priority
//!
//! Evidence is ranked by [`DetectionSource`], strongest first:
//!
//! 1. An explicit user assignment.
//! 2. Existing trusted platform metadata.
//! 3. An exact normalised folder alias.
//! 4. A bounded file signature.
//! 5. A distinctive multi-file layout.
//! 6. Emulator or profile context.
//! 7. A strong, platform-specific extension.
//! 8. A weak, shared extension.
//!
//! A weaker source never overrides a stronger contradictory one. That single
//! rule is what fixes the ScummVM `RESOURCE.GEN` misclassification: an
//! extension (rank 8) cannot displace the folder that contains it (rank 3).
//!
//! # Read-only and bounded
//!
//! The only filesystem access is `symlink_metadata`, one `read_dir` of the
//! containing directory when layout evidence is requested, and at most
//! [`MAX_MAGIC_READ_BYTES`](super::MAX_MAGIC_READ_BYTES) bytes read at a known
//! offset from the file itself. Nothing is written, no archive is opened or
//! extracted, no image is parsed or hashed, no process is spawned and no
//! network request is made.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    LayoutRule, MAX_MAGIC_READ_BYTES, PLATFORMS, Platform, extension_of, is_shared_extension,
    normalize_alias, platform_by_id, platform_for_alias,
};

/// How sure detection is. Deliberately four states: "we do not know" and "we
/// cannot choose between these" are different answers and a person needs to
/// see which one they have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionConfidence {
    /// Nothing usable was found. A valid result, not a failure.
    Unknown,
    /// Several platforms fit the evidence equally well and none of them wins.
    /// No platform is selected.
    Ambiguous,
    /// One platform fits best, but the evidence could be wrong.
    Probable,
    /// The evidence is decisive: an explicit assignment, or a signature.
    Confirmed,
}

impl DetectionConfidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Ambiguous => "Ambiguous",
            Self::Probable => "Probable",
            Self::Confirmed => "Confirmed",
        }
    }

    /// Whether a person should be asked before this result is relied on.
    pub fn requires_confirmation(self) -> bool {
        matches!(self, Self::Ambiguous | Self::Unknown | Self::Probable)
    }
}

/// Where one piece of evidence came from. The ordering *is* the priority: a
/// larger value outranks a smaller one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionSource {
    /// A shared extension: `.bin`, `.iso`, `.zip` and friends.
    SharedExtension,
    /// An extension specific enough to mean something on its own.
    StrongExtension,
    /// The emulator or core the surrounding context implies.
    EmulatorContext,
    /// A distinctive arrangement of files in the containing directory.
    Layout,
    /// A bounded magic-byte match at a known offset.
    Signature,
    /// An exact whole-component folder alias.
    FolderAlias,
    /// Platform metadata already recorded and trusted.
    TrustedMetadata,
    /// A person said so.
    ExplicitAssignment,
}

impl DetectionSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::SharedExtension => "shared file extension",
            Self::StrongExtension => "platform-specific file extension",
            Self::EmulatorContext => "emulator context",
            Self::Layout => "file layout",
            Self::Signature => "file signature",
            Self::FolderAlias => "folder name",
            Self::TrustedMetadata => "existing platform metadata",
            Self::ExplicitAssignment => "manual assignment",
        }
    }

    /// Whether evidence from this source can, by itself, confirm a platform.
    fn is_decisive(self) -> bool {
        matches!(self, Self::ExplicitAssignment | Self::Signature)
    }
}

/// One observed fact and what it points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectionEvidence {
    pub source: DetectionSource,
    /// The canonical platform this fact points at.
    pub platform: &'static str,
    /// What was actually observed, in a person's words.
    pub detail: String,
}

/// A platform the evidence allows, with the best evidence for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformCandidate {
    pub platform: &'static str,
    pub display_name: &'static str,
    pub best_source: DetectionSource,
}

/// What to detect, and what is already known about it.
///
/// Built with [`DetectionRequest::new`] and then narrowed: a caller supplies
/// only what it actually has, and anything it does not know stays `None`
/// rather than being guessed.
#[derive(Debug, Clone)]
pub struct DetectionRequest<'a> {
    pub path: &'a Path,
    /// The configured source folder the path was found under. Folder-alias
    /// matching never looks above it.
    pub source_root: &'a Path,
    /// A platform a person explicitly assigned. Outranks everything.
    pub manual_platform: Option<&'a str>,
    /// A platform already recorded and trusted - for example inherited from
    /// the parent game directory during a scan.
    pub trusted_platform: Option<&'a str>,
    /// Emulator or core context, when the surrounding workflow knows it.
    pub emulator_context: Option<&'a str>,
    /// Whether the containing directory may be listed for layout evidence.
    /// One bounded `read_dir`, off by default so a caller opts in.
    pub inspect_layout: bool,
    /// Whether bounded signature reads are allowed. Off by default for the
    /// same reason.
    pub read_signatures: bool,
}

impl<'a> DetectionRequest<'a> {
    pub fn new(path: &'a Path, source_root: &'a Path) -> Self {
        Self {
            path,
            source_root,
            manual_platform: None,
            trusted_platform: None,
            emulator_context: None,
            inspect_layout: false,
            read_signatures: false,
        }
    }

    pub fn with_manual_platform(mut self, platform: Option<&'a str>) -> Self {
        self.manual_platform = platform;
        self
    }

    pub fn with_trusted_platform(mut self, platform: Option<&'a str>) -> Self {
        self.trusted_platform = platform;
        self
    }

    pub fn with_emulator_context(mut self, emulator: Option<&'a str>) -> Self {
        self.emulator_context = emulator;
        self
    }

    /// Enables the two bounded filesystem reads: one directory listing and
    /// one short read at a known offset per signature rule.
    pub fn inspecting_content(mut self) -> Self {
        self.inspect_layout = true;
        self.read_signatures = true;
        self
    }
}

/// The full result: what was chosen, how sure, on what evidence, and what else
/// it could have been.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformDetectionReport {
    /// The selected canonical platform, or `None` for Ambiguous and Unknown.
    pub platform: Option<&'static str>,
    pub display_name: Option<&'static str>,
    pub confidence: DetectionConfidence,
    /// The source that decided the outcome, if one did.
    pub deciding_source: Option<DetectionSource>,
    /// Every fact gathered, strongest first. Deterministic.
    pub evidence: Vec<DetectionEvidence>,
    /// Platforms the evidence also allows, sorted by identifier.
    pub candidates: Vec<PlatformCandidate>,
    /// Why no single platform could be chosen, when none could.
    pub ambiguity_reason: Option<String>,
    pub requires_confirmation: bool,
    /// True when the selected platform came from an explicit assignment, so
    /// the GUI can say "Manually assigned" rather than "Detected".
    pub manually_assigned: bool,
}

impl PlatformDetectionReport {
    fn unknown(evidence: Vec<DetectionEvidence>, reason: Option<String>) -> Self {
        Self {
            platform: None,
            display_name: None,
            confidence: DetectionConfidence::Unknown,
            deciding_source: None,
            evidence,
            candidates: Vec::new(),
            ambiguity_reason: reason,
            requires_confirmation: true,
            manually_assigned: false,
        }
    }

    /// A one-line summary for a list view.
    pub fn summary(&self) -> String {
        match (self.platform, self.confidence) {
            (Some(platform), confidence) => format!(
                "{} ({}, from {})",
                super::display_name_for(platform),
                confidence.label().to_lowercase(),
                self.deciding_source
                    .map(DetectionSource::label)
                    .unwrap_or("no evidence")
            ),
            (None, DetectionConfidence::Ambiguous) => format!(
                "Ambiguous between {}",
                self.candidates
                    .iter()
                    .map(|candidate| candidate.display_name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            (None, _) => "Platform unknown".to_string(),
        }
    }
}

/// Detects the platform for one path.
///
/// Read-only and bounded - see the module documentation for exactly which
/// filesystem calls are made.
pub fn detect_platform_report(request: &DetectionRequest<'_>) -> PlatformDetectionReport {
    let mut evidence: Vec<DetectionEvidence> = Vec::new();

    // 1. An explicit assignment ends the question. Nothing below may contradict
    //    it, and it is never re-derived.
    if let Some(manual) = request.manual_platform.and_then(canonical_id) {
        evidence.push(DetectionEvidence {
            source: DetectionSource::ExplicitAssignment,
            platform: manual,
            detail: "a platform was assigned explicitly for this entry".to_string(),
        });
        return PlatformDetectionReport {
            platform: Some(manual),
            display_name: Some(super::display_name_for(manual)),
            confidence: DetectionConfidence::Confirmed,
            deciding_source: Some(DetectionSource::ExplicitAssignment),
            evidence,
            candidates: Vec::new(),
            ambiguity_reason: None,
            requires_confirmation: false,
            manually_assigned: true,
        };
    }

    // 2. Trusted metadata - including a platform inherited from the containing
    //    game directory during a scan.
    if let Some(trusted) = request.trusted_platform.and_then(canonical_id) {
        evidence.push(DetectionEvidence {
            source: DetectionSource::TrustedMetadata,
            platform: trusted,
            detail: "this entry already carries trusted platform metadata".to_string(),
        });
    }

    // 3. Folder aliases, nearest containing folder first.
    if let Some((platform, folder)) = folder_alias_evidence(request.path, request.source_root) {
        evidence.push(DetectionEvidence {
            source: DetectionSource::FolderAlias,
            platform: platform.id,
            detail: format!("the containing folder `{folder}` names this platform exactly"),
        });
    }

    // 4. Bounded signature reads.
    if request.read_signatures {
        evidence.extend(signature_evidence(request.path));
    }

    // 5. Layout evidence from the containing directory.
    if request.inspect_layout {
        evidence.extend(layout_evidence(request.path));
    }

    // 6. Emulator context.
    if let Some(emulator) = request.emulator_context {
        for platform in PLATFORMS
            .iter()
            .filter(|platform| platform.preferred_emulator == Some(emulator))
        {
            evidence.push(DetectionEvidence {
                source: DetectionSource::EmulatorContext,
                platform: platform.id,
                detail: format!("the surrounding workflow is using {emulator}"),
            });
        }
    }

    // 7 and 8. Extensions, ranked.
    let extension = extension_of(request.path);
    if let Some(extension) = &extension {
        for platform in PLATFORMS {
            if platform.has_strong_extension(extension) && !is_shared_extension(extension) {
                evidence.push(DetectionEvidence {
                    source: DetectionSource::StrongExtension,
                    platform: platform.id,
                    detail: format!("`.{extension}` is specific to this platform"),
                });
            }
        }
        for platform in PLATFORMS {
            if platform.has_weak_extension(extension) {
                evidence.push(DetectionEvidence {
                    source: DetectionSource::SharedExtension,
                    platform: platform.id,
                    detail: format!(
                        "`.{extension}` is shared with other platforms, so it only narrows the result"
                    ),
                });
            }
        }
    }

    // Deterministic order: strongest source first, then by platform id, then
    // by detail - so the same inputs always produce the same report whatever
    // order the filesystem listed anything in.
    evidence.sort_by(|left, right| {
        right
            .source
            .cmp(&left.source)
            .then_with(|| left.platform.cmp(right.platform))
            .then_with(|| left.detail.cmp(&right.detail))
    });

    decide(evidence, extension.as_deref())
}

/// Chooses an outcome from gathered evidence. Pure: no I/O.
fn decide(evidence: Vec<DetectionEvidence>, extension: Option<&str>) -> PlatformDetectionReport {
    let Some(strongest) = evidence.first().map(|item| item.source) else {
        return PlatformDetectionReport::unknown(
            evidence,
            Some("no folder, signature, layout or extension evidence was found".to_string()),
        );
    };

    // Only the strongest tier decides. Weaker contradictory evidence is
    // retained for display but never competes - that is the whole point.
    let leaders: Vec<&'static str> = {
        let mut ids: Vec<&'static str> = evidence
            .iter()
            .filter(|item| item.source == strongest)
            .map(|item| item.platform)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };

    let candidates = candidates_from(&evidence);

    // Several platforms tie at the strongest tier. If they are all the same
    // hardware under different stored identifiers - `PC Engine` and
    // `TurboGrafx-16`, `PC-98` and `NEC PC-9801` - that is not a real
    // ambiguity, so the first identifier in sorted order is chosen
    // deterministically and the equivalence is recorded as evidence. Anything
    // else is genuinely ambiguous and is reported as such rather than guessed.
    let leaders = if leaders.len() > 1 && all_mutually_equivalent(&leaders) {
        vec![leaders[0]]
    } else {
        leaders
    };

    // Several platforms tie at the strongest tier: say so rather than pick.
    if leaders.len() > 1 {
        return PlatformDetectionReport {
            platform: None,
            display_name: None,
            confidence: DetectionConfidence::Ambiguous,
            deciding_source: Some(strongest),
            ambiguity_reason: Some(format!(
                "{} platforms share the strongest available evidence ({}): {}",
                leaders.len(),
                strongest.label(),
                leaders.join(", ")
            )),
            evidence,
            candidates,
            requires_confirmation: true,
            manually_assigned: false,
        };
    }

    let selected = leaders[0];

    // A shared extension on its own is never an answer.
    if strongest == DetectionSource::SharedExtension {
        let shared = extension.unwrap_or("shared");
        return PlatformDetectionReport {
            platform: None,
            display_name: None,
            confidence: if candidates.len() > 1 {
                DetectionConfidence::Ambiguous
            } else {
                DetectionConfidence::Unknown
            },
            deciding_source: Some(strongest),
            ambiguity_reason: Some(format!(
                "the only evidence is the shared `.{shared}` extension, which several platforms use"
            )),
            evidence,
            candidates,
            requires_confirmation: true,
            manually_assigned: false,
        };
    }

    // The priority order exists to settle *conflicts*: when a folder name and a
    // signature disagree, the folder wins. When they agree, that is
    // corroboration, and reporting it as merely Probable would understate what
    // is actually known. So the selected platform is Confirmed whenever any
    // decisive source - a signature, or an explicit assignment - also points at
    // it, whatever tier happened to be strongest.
    let corroborated_by_decisive_source = evidence
        .iter()
        .any(|item| item.source.is_decisive() && item.platform == selected);
    let confidence = if strongest.is_decisive() || corroborated_by_decisive_source {
        DetectionConfidence::Confirmed
    } else {
        DetectionConfidence::Probable
    };
    PlatformDetectionReport {
        platform: Some(selected),
        display_name: Some(super::display_name_for(selected)),
        confidence,
        deciding_source: Some(strongest),
        ambiguity_reason: None,
        evidence,
        candidates,
        requires_confirmation: confidence.requires_confirmation(),
        manually_assigned: false,
    }
}

/// Whether every identifier in `ids` is declared equivalent to every other -
/// the same machine stored under more than one name.
fn all_mutually_equivalent(ids: &[&'static str]) -> bool {
    ids.iter().all(|left| {
        let equivalents = super::equivalent_platform_ids(left);
        ids.iter()
            .all(|right| right == left || equivalents.contains(right))
    })
}

/// Every platform any evidence pointed at, with its best source. Sorted by
/// identifier so the list is stable.
fn candidates_from(evidence: &[DetectionEvidence]) -> Vec<PlatformCandidate> {
    let mut candidates: Vec<PlatformCandidate> = Vec::new();
    for item in evidence {
        match candidates
            .iter_mut()
            .find(|candidate| candidate.platform == item.platform)
        {
            Some(existing) => {
                if item.source > existing.best_source {
                    existing.best_source = item.source;
                }
            }
            None => candidates.push(PlatformCandidate {
                platform: item.platform,
                display_name: super::display_name_for(item.platform),
                best_source: item.source,
            }),
        }
    }
    candidates.sort_by(|left, right| left.platform.cmp(right.platform));
    candidates
}

/// The canonical identifier for a hint, if this build knows it. An unknown
/// stored identifier is deliberately *not* dropped: it is returned as-is only
/// when it exactly matches a registry id, so a typo can never become a
/// platform.
fn canonical_id(hint: &str) -> Option<&'static str> {
    platform_by_id(hint)
        .map(|platform| platform.id)
        .or_else(|| platform_for_alias(hint).map(|platform| platform.id))
}

/// Walks directory components from the file's own folder upward to, and
/// including, `source_root`. The file's own name is never treated as a folder.
/// Nearest folder wins.
fn folder_alias_evidence(path: &Path, source_root: &Path) -> Option<(&'static Platform, String)> {
    let relative = path.strip_prefix(source_root).ok()?;
    let mut components: Vec<_> = relative.components().collect();
    components.pop();

    components
        .iter()
        .rev()
        .find_map(|component| {
            let name = component.as_os_str().to_string_lossy();
            folder_alias_with_suffixes(&name).map(|platform| (platform, name.into_owned()))
        })
        .or_else(|| {
            let name = source_root.file_name()?.to_string_lossy();
            folder_alias_with_suffixes(&name).map(|platform| (platform, name.into_owned()))
        })
}

/// An exact alias match, also tolerating the two suffix conventions real
/// collection folders use: a parenthesised suffix and a trailing date.
/// Matching stays exact after the suffix is removed - never a substring.
fn folder_alias_with_suffixes(segment: &str) -> Option<&'static Platform> {
    if let Some(platform) = platform_for_alias(segment) {
        return Some(platform);
    }
    if let Some((base, _)) = segment.split_once('(')
        && let Some(platform) = platform_for_alias(base.trim_end())
    {
        return Some(platform);
    }
    let bytes = segment.as_bytes();
    let date_start = (0..bytes.len().saturating_sub(9)).find(|&index| {
        bytes[index..]
            .get(0..4)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
            && bytes.get(index + 4) == Some(&b'-')
            && bytes[index + 5..]
                .get(0..2)
                .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
            && bytes.get(index + 7) == Some(&b'-')
            && bytes[index + 8..]
                .get(0..2)
                .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    })?;
    platform_for_alias(segment[..date_start].trim_end_matches([' ', '-', '_']))
}

/// Bounded signature checks. One short read per distinct offset, never more
/// than [`MAX_MAGIC_READ_BYTES`] bytes, and only on a real file that is not a
/// symlink.
fn signature_evidence(path: &Path) -> Vec<DetectionEvidence> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Vec::new();
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        // Never follow a symlink to read a signature, and never try to read a
        // directory or a device.
        return Vec::new();
    }
    let length = metadata.len();

    let mut evidence = Vec::new();
    for platform in PLATFORMS {
        for rule in platform.magic {
            debug_assert!(
                rule.bytes.len() <= MAX_MAGIC_READ_BYTES,
                "a signature rule must stay within the documented read bound"
            );
            // Refuse before opening rather than reading and discarding.
            if rule.offset.saturating_add(rule.bytes.len() as u64) > length {
                continue;
            }
            if read_exact_at(path, rule.offset, rule.bytes.len())
                .is_some_and(|actual| actual == rule.bytes)
            {
                evidence.push(DetectionEvidence {
                    source: DetectionSource::Signature,
                    platform: platform.id,
                    detail: rule.description.to_string(),
                });
            }
        }
    }
    evidence
}

/// Reads exactly `length` bytes at `offset`. `length` is bounded by the
/// caller's rule, which is itself bounded by [`MAX_MAGIC_READ_BYTES`].
fn read_exact_at(path: &Path, offset: u64, length: usize) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    if length > MAX_MAGIC_READ_BYTES {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buffer = vec![0_u8; length];
    file.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

/// Layout evidence from the file's containing directory: one bounded
/// `read_dir`, filenames only, no file contents.
fn layout_evidence(path: &Path) -> Vec<DetectionEvidence> {
    let Some(directory) = containing_directory(path) else {
        return Vec::new();
    };
    let Ok(read_dir) = fs::read_dir(&directory) else {
        return Vec::new();
    };
    // Bounded: a game directory with more entries than this is not going to be
    // identified by its layout anyway.
    const MAX_LAYOUT_ENTRIES: usize = 4096;
    let mut names: Vec<String> = Vec::new();
    for entry in read_dir.filter_map(Result::ok).take(MAX_LAYOUT_ENTRIES) {
        names.push(entry.file_name().to_string_lossy().to_ascii_lowercase());
    }
    names.sort();

    let mut evidence = Vec::new();
    for platform in PLATFORMS {
        for rule in platform.layout {
            if let Some(matched) = matching_layout_file(rule, &names) {
                evidence.push(DetectionEvidence {
                    source: DetectionSource::Layout,
                    platform: platform.id,
                    detail: format!("{} (`{matched}`)", rule.description),
                });
            }
        }
    }
    evidence
}

fn matching_layout_file(rule: &LayoutRule, names: &[String]) -> Option<String> {
    rule.any_of_files
        .iter()
        .find(|wanted| names.iter().any(|name| name == *wanted))
        .map(|wanted| (*wanted).to_string())
}

/// The directory that would carry layout evidence for `path`: the path itself
/// when it is a directory, otherwise its parent.
fn containing_directory(path: &Path) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        return Some(path.to_path_buf());
    }
    path.parent().map(Path::to_path_buf)
}

/// The platform a normalised folder name implies, exposed for callers that
/// already hold a folder name rather than a path.
pub fn platform_for_folder_name(name: &str) -> Option<&'static Platform> {
    folder_alias_with_suffixes(name)
}

/// Whether `hint` normalises to something the registry knows.
pub fn is_known_platform_hint(hint: &str) -> bool {
    canonical_id(hint).is_some()
}

/// The normalised form of a hint, for callers comparing two spellings.
pub fn normalized_hint(hint: &str) -> String {
    normalize_alias(hint)
}
