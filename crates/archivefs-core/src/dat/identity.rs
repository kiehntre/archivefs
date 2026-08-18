//! Pure DAT-source-level platform/machine identity resolution.
//!
//! A DAT catalogue's own platform is a different question from a game's
//! platform: this module answers "what platform does this whole DAT file
//! describe", not "what platform is this ROM". [`crate::platform::identity`]
//! resolves the latter, across RomM and verified-DAT evidence, and already
//! assumes a DAT source's platform is known; this module is what a later
//! chunk will use to derive that fact from a DAT's own content instead of
//! requiring a person to assign it by hand first.
//!
//! # Scope
//!
//! This module defines the identity model, its pure resolver, and pure
//! evidence gathering from an already-parsed [`ParsedDat`] (via
//! [`gather_dat_platform_evidence`]/[`identify_dat_source`]). It is still not
//! wired into DAT validation, audit, repair, source assignment, rename
//! planning/apply, RomM, persistence, or the GUI - it only proves that real
//! parsed DAT facts can reach the resolver and produce an honest answer.
//! Nothing here performs filesystem, database, or network I/O: gathering
//! operates only on the `DatSource`/[`DatGameEntry`] values a parser already
//! produced, including plain string manipulation of `DatSource::file_path`
//! (never a read of the path itself).
//!
//! # Only `Strong` evidence resolves anything
//!
//! In this first chunk, a single [`DatPlatformEvidence`] at
//! [`DatPlatformConfidence::Strong`] is what a `Resolved` or `Ambiguous`
//! outcome is built from. `Corroborated` and `Weak` evidence is always kept
//! in a `Resolved` outcome's `evidence` list for provenance, and never
//! silently downgrades a Strong result, but it can never resolve an identity
//! or create an `Ambiguous` outcome by itself - matching the established
//! rule that a filename or a media extension is never authority on its own.
//! A later chunk, once real evidence-gathering exists, may reconsider
//! promoting agreeing `Corroborated` evidence (for example, a filename and a
//! folder hint that agree with each other); that is deliberately not decided
//! here.
//!
//! # Never a rename authority
//!
//! Nothing in this module may be imported by
//! [`crate::dat::rename_plan`] or [`crate::dat::rename_apply`]. DAT
//! machine/platform identity is corroborating, display-level evidence; the
//! only thing that may authorize a rename is cryptographic hash evidence from
//! [`crate::dat::audit`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::platform::{
    MagicConfidence, equivalent_platform_ids, platform_candidates_for_extension,
    platform_for_alias, platform_magic_confidence_from_bytes, strip_mame_software_list_suffix,
};

use super::model::{DatGameEntry, ParsedDat};

/// Where one piece of DAT-source platform evidence came from.
///
/// Declaration order is also the tie-break order used when sorting evidence
/// for a deterministic result; it carries no other meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatPlatformEvidenceKind {
    /// The DAT's `<header><name>` (Logiqx) or `clrmamepro ( name ... )`
    /// (ClrMamePro) value.
    HeaderName,
    /// The DAT's `<header><description>` / `clrmamepro ( description ... )`
    /// value.
    HeaderDescription,
    /// A bare MAME software-list root's own `name` attribute
    /// (`<softwarelist name="megacd" ...>`).
    SoftwareListName,
    /// A bare MAME software-list root's own `description` attribute.
    SoftwareListDescription,
    /// A `<machine>`/`<software>` entry's own short name, sampled from the
    /// catalogue (for example `sms`, `neocdz`, `c128_flop`).
    MachineShortname,
    /// A registered [`crate::platform::MagicRule`] byte signature matched
    /// against the actual bytes of a file - never a DAT's text. This is
    /// evidence about one *ROM*, not about the catalogue: it exists so a
    /// caller that has both a DAT fact and a real file's bytes can combine
    /// them, but nothing in this module ever reads a file to produce it (see
    /// [`magic_hint_evidence`]). Never to be confused with [`Self::HeaderName`]
    /// or [`Self::HeaderDescription`], which are the DAT's own `<header>`
    /// text and have nothing to do with a file's bytes.
    HeaderMagic,
    /// The DAT file's own filename.
    FilenameCorroboration,
    /// The folder the DAT file lives in.
    FolderHint,
    /// An extension seen among the DAT's own ROM entries.
    MediaExtension,
}

impl DatPlatformEvidenceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::HeaderName => "DAT header name",
            Self::HeaderDescription => "DAT header description",
            Self::SoftwareListName => "MAME software-list name",
            Self::SoftwareListDescription => "MAME software-list description",
            Self::MachineShortname => "MAME machine/software shortname",
            Self::HeaderMagic => "file header/magic signature",
            Self::FilenameCorroboration => "DAT filename",
            Self::FolderHint => "DAT folder name",
            Self::MediaExtension => "ROM entry extension",
        }
    }
}

/// How much one piece of evidence is worth on its own.
///
/// Deliberately three tiers, weakest first. See the module documentation for
/// what each tier is currently allowed to decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatPlatformConfidence {
    /// Never decisive alone: a filename, a folder name, or a media
    /// extension. Corroboration only.
    Weak,
    /// Agrees with something else but has not itself been promoted to decide
    /// an outcome in this chunk.
    Corroborated,
    /// Decisive alone: a resolved header/software-list name or description,
    /// or a machine-shortname sample with a clear majority.
    Strong,
}

impl DatPlatformConfidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Weak => "Weak",
            Self::Corroborated => "Corroborated",
            Self::Strong => "Strong",
        }
    }
}

/// One canonical-platform fact gathered from a DAT source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatPlatformEvidence {
    /// A canonical [`crate::platform::Platform::id`]. Evidence that could not
    /// be resolved to a canonical id is never constructed - there is nothing
    /// useful to carry as "evidence" for an unknown string.
    pub platform: String,
    /// The MAME machine/software shortname this evidence came from, when the
    /// evidence is [`DatPlatformEvidenceKind::MachineShortname`] or
    /// [`DatPlatformEvidenceKind::SoftwareListName`]. `None` for evidence
    /// that has no machine key of its own (a plain No-Intro/TOSEC header).
    pub machine_key: Option<String>,
    pub kind: DatPlatformEvidenceKind,
    pub confidence: DatPlatformConfidence,
    /// What was actually observed, in a person's words.
    pub detail: String,
}

/// The deterministic answer for one DAT source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum DatPlatformIdentity {
    /// No Strong evidence was gathered, or none of it resolved to a
    /// canonical platform. Distinct from `Ambiguous`: here there is nothing
    /// to choose between at all.
    Unknown,
    /// Every piece of Strong evidence agreed (after folding
    /// [`equivalent_platform_ids`] together) on one canonical platform.
    Resolved {
        platform: String,
        /// The machine key carried by the deciding evidence, when any of it
        /// had one.
        machine_key: Option<String>,
        confidence: DatPlatformConfidence,
        /// Every piece of evidence gathered, strongest first, kept for
        /// provenance and display even when it did not participate in the
        /// decision.
        evidence: Vec<DatPlatformEvidence>,
    },
    /// Two or more pieces of Strong evidence named different, non-equivalent
    /// canonical platforms. Fails closed: no platform is selected.
    Ambiguous {
        candidates: Vec<DatPlatformEvidence>,
    },
}

impl DatPlatformIdentity {
    pub fn platform(&self) -> Option<&str> {
        match self {
            Self::Resolved { platform, .. } => Some(platform),
            Self::Unknown | Self::Ambiguous { .. } => None,
        }
    }

    pub fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous { .. })
    }

    /// Every piece of evidence behind a `Resolved` outcome, empty otherwise.
    pub fn evidence(&self) -> &[DatPlatformEvidence] {
        match self {
            Self::Resolved { evidence, .. } => evidence,
            Self::Unknown | Self::Ambiguous { .. } => &[],
        }
    }
}

/// Resolves DAT-source platform identity from gathered evidence.
///
/// Pure and deterministic: evidence is sorted before it is folded, so the
/// order the caller happened to gather it in can never change the result.
/// Reads nothing from disk, a database, or a config - it only operates on the
/// [`DatPlatformEvidence`] values it is given.
pub fn resolve_dat_platform_identity(
    evidence: impl IntoIterator<Item = DatPlatformEvidence>,
) -> DatPlatformIdentity {
    let mut evidence: Vec<DatPlatformEvidence> = evidence.into_iter().collect();
    evidence.sort_by(|left, right| {
        right
            .confidence
            .cmp(&left.confidence)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.platform.cmp(&right.platform))
            .then_with(|| left.machine_key.cmp(&right.machine_key))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    evidence.dedup();

    let strong: Vec<DatPlatformEvidence> = evidence
        .iter()
        .filter(|item| item.confidence == DatPlatformConfidence::Strong)
        .cloned()
        .collect();

    if strong.is_empty() {
        return DatPlatformIdentity::Unknown;
    }

    let mut platforms: Vec<&str> = strong.iter().map(|item| item.platform.as_str()).collect();
    platforms.sort_unstable();
    platforms.dedup();

    let platforms = if platforms.len() > 1 && all_mutually_equivalent(&platforms) {
        vec![platforms[0]]
    } else {
        platforms
    };

    if platforms.len() > 1 {
        return DatPlatformIdentity::Ambiguous { candidates: strong };
    }

    let platform = platforms[0].to_string();
    let machine_key = strong.iter().find_map(|item| item.machine_key.clone());

    DatPlatformIdentity::Resolved {
        platform,
        machine_key,
        confidence: DatPlatformConfidence::Strong,
        evidence,
    }
}

/// Whether every identifier in `ids` is declared equivalent to every other -
/// the same machine stored under more than one canonical id (`PC Engine` /
/// `TurboGrafx-16`, `PC-98` / `NEC PC-9801`). Mirrors
/// `platform::detect::all_mutually_equivalent` exactly, so a DAT that names
/// two equivalent identifiers is resolved rather than reported as a conflict
/// between two spellings of the same hardware.
fn all_mutually_equivalent(ids: &[&str]) -> bool {
    ids.iter().all(|left| {
        let equivalents = equivalent_platform_ids(left);
        ids.iter()
            .all(|right| right == left || equivalents.contains(right))
    })
}

// -- Evidence gathering from an already-parsed DAT -------------------------
//
// Everything below turns [`ParsedDat`] facts a parser already produced into
// [`DatPlatformEvidence`]. It reads no file, opens no archive, and touches no
// database: `DatSource::file_path` is treated as a string only, never as a
// path to open.

/// How many trailing whitespace-delimited words a compound header/description
/// segment may have trimmed off before giving up. Deliberately small: this
/// only exists to turn `"Commodore 128 Software"` into `"Commodore 128"`, not
/// to hunt for a platform name buried anywhere in a sentence.
const MAX_TRAILING_WORD_TRIMS: usize = 2;

/// How many game/machine/software entries [`machine_shortname_evidence`]
/// samples at most. A generic Logiqx or TOSEC DAT has one entry per game and
/// gains nothing from this tier; a MAME software-list DAT is usually a few
/// hundred entries at most. This bound exists so a pathological or full MAME
/// arcade listxml DAT (hundreds of thousands of `<machine>` entries) cannot
/// make gathering unbounded.
const MAX_MACHINE_SHORTNAME_SAMPLE: usize = 512;

/// Literal separators a compound header/description string is split on
/// before each piece is matched on its own. Deliberately not a general
/// tokeniser: these are exactly the punctuation TOSEC/No-Intro/MAME headers
/// use to join more than one named thing (`"Sega Mark III & Master System -
/// Games"`, `"Sega Mega-CD / Sega CD"`).
const HEADER_SEGMENT_SEPARATORS: &[&str] = &[" - ", "/", "&", ","];

/// Gathers every piece of platform evidence an already-parsed DAT carries.
///
/// Pure and deterministic: iterates `dat.source` and `dat.games` in the order
/// the parser produced them and performs no I/O. The caller is expected to
/// pass the result to [`resolve_dat_platform_identity`] (see
/// [`identify_dat_source`] for the combined convenience call).
pub fn gather_dat_platform_evidence(dat: &ParsedDat) -> Vec<DatPlatformEvidence> {
    // A bare MAME software-list root's `name`/`description` land in exactly
    // the same `DatSource` fields a `<header>` would populate (see the
    // `dat::parsers::logiqx` fix that preserves them). The two cases are
    // distinguished here only for the evidence *label*, using the presence of
    // software-list `<part>` structure as the signal - it never changes
    // whether or how strongly the text resolves.
    let is_software_list = dat.games.iter().any(|game| !game.parts.is_empty());
    let (name_kind, description_kind) = if is_software_list {
        (
            DatPlatformEvidenceKind::SoftwareListName,
            DatPlatformEvidenceKind::SoftwareListDescription,
        )
    } else {
        (
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformEvidenceKind::HeaderDescription,
        )
    };

    let mut evidence = Vec::new();
    if let Some(name) = dat.source.name.as_deref() {
        evidence.extend(strong_evidence_from_text(name, name_kind, is_software_list));
    }
    if let Some(description) = dat.source.description.as_deref() {
        evidence.extend(strong_evidence_from_text(
            description,
            description_kind,
            false,
        ));
    }
    evidence.extend(machine_shortname_evidence(&dat.games));
    evidence.extend(filename_hint_evidence_from_path(&dat.source.file_path));
    evidence.extend(folder_hint_evidence_from_path(&dat.source.file_path));
    evidence
}

/// Gathers evidence and resolves it in one call.
pub fn identify_dat_source(dat: &ParsedDat) -> DatPlatformIdentity {
    resolve_dat_platform_identity(gather_dat_platform_evidence(dat))
}

/// Strong evidence from one piece of header/software-list text: the whole
/// string first, then each conservatively split segment. `carries_machine_key`
/// attaches `text` itself as the [`DatPlatformEvidence::machine_key`] of every
/// fact produced - true only for a software-list's own `name`, which really
/// is the DAT's machine/system shortname, never for free-text description or
/// header prose.
fn strong_evidence_from_text(
    text: &str,
    kind: DatPlatformEvidenceKind,
    carries_machine_key: bool,
) -> Vec<DatPlatformEvidence> {
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut evidence = Vec::new();
    for segment in header_segments(text) {
        let Some(platform) = resolve_segment(&segment) else {
            continue;
        };
        if !seen.insert(platform) {
            continue;
        }
        evidence.push(DatPlatformEvidence {
            platform: platform.to_string(),
            machine_key: carries_machine_key.then(|| text.to_string()),
            kind,
            confidence: DatPlatformConfidence::Strong,
            detail: format!(
                "{} `{text}` names this platform via `{segment}`",
                kind.label()
            ),
        });
    }
    evidence
}

/// Splits `text` on [`HEADER_SEGMENT_SEPARATORS`] and trims whitespace,
/// dropping empty pieces. The whole trimmed `text` is always included as the
/// first candidate segment too, so a single-token value (`"C128"`, `"neocd"`)
/// is tried as-is before any splitting.
fn header_segments(text: &str) -> Vec<String> {
    let mut segments = vec![text.to_string()];
    for separator in HEADER_SEGMENT_SEPARATORS {
        segments = segments
            .into_iter()
            .flat_map(|segment| {
                segment
                    .split(separator)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    let mut segments: Vec<String> = segments
        .into_iter()
        .map(|segment| segment.trim().to_string())
        .filter(|segment| !segment.is_empty())
        .collect();
    segments.dedup();
    segments
}

/// Resolves one segment or machine-shortname token to a canonical platform,
/// conservatively:
///
/// 1. An exact registry alias match on the whole token.
/// 2. Failing that, the fixed MAME software-list suffix stripped, then an
///    exact alias match on what remains (`"c128_flop"` -> `"c128"`;
///    `"c128_weirdthing"` is left alone by the suffix helper and so still
///    fails here).
/// 3. Failing that, up to [`MAX_TRAILING_WORD_TRIMS`] trailing
///    whitespace-delimited words dropped, trying the shortened phrase after
///    each trim - so `"Commodore 128 Software"` reaches `"Commodore 128"`,
///    while a single generic word (`"Commodore"`, `"Software"`) is never
///    trimmed down to nothing and invented into a platform.
fn resolve_segment(segment: &str) -> Option<&'static str> {
    if let Some(platform) = platform_for_alias(segment) {
        return Some(platform.id);
    }
    let stripped = strip_mame_software_list_suffix(segment);
    if stripped != segment
        && let Some(platform) = platform_for_alias(stripped)
    {
        return Some(platform.id);
    }
    let words: Vec<&str> = segment.split_whitespace().collect();
    let max_trim = MAX_TRAILING_WORD_TRIMS.min(words.len().saturating_sub(1));
    for trim in 1..=max_trim {
        let candidate = words[..words.len() - trim].join(" ");
        if let Some(platform) = platform_for_alias(&candidate) {
            return Some(platform.id);
        }
    }
    None
}

/// A sampled population this size or smaller is "tiny": one resolvable entry
/// with nothing to contradict it is treated as real signal rather than noise.
/// This is what lets a genuinely small DAT containing only `<machine
/// name="neocd">` still resolve, without requiring a large-sample majority a
/// tiny catalogue could never produce.
const SMALL_MACHINE_SAMPLE_LIMIT: usize = 3;

/// Once the sampled population is larger than [`SMALL_MACHINE_SAMPLE_LIMIT`],
/// the winning platform group must carry at least this many *distinct*
/// resolved entry names before agreement counts as real signal. Without this,
/// one stray alias collision inside hundreds of unrelated, unresolved MAME
/// arcade machine names would look like "100% agreement" simply because
/// nothing else in the sample resolved to anything at all.
const MIN_RESOLVED_FOR_LARGE_SAMPLE_MAJORITY: usize = 3;

/// The winning group must carry at least this fraction of every *resolved*
/// (not merely sampled) entry once the population is larger than
/// [`SMALL_MACHINE_SAMPLE_LIMIT`]. `9 / 10` = at least 90%: overwhelming, not
/// merely "more common than the others" - a near-even split (as a
/// heterogeneous C128/C64 mixture would produce) never reaches it.
const LARGE_SAMPLE_MAJORITY_NUM: usize = 9;
const LARGE_SAMPLE_MAJORITY_DEN: usize = 10;

/// The canonical id used to group `id` with any platform
/// [`equivalent_platform_ids`] declares equivalent to it, so `PC Engine` and
/// `TurboGrafx-16` machine names count as agreement rather than splitting the
/// vote. Deterministic: the lexicographically smallest id in the equivalence
/// set, so the same group always gets the same representative regardless of
/// which member was seen first.
fn equivalence_representative(id: &'static str) -> &'static str {
    let mut group = equivalent_platform_ids(id);
    group.push(id);
    group.sort_unstable();
    group[0]
}

/// Evidence from individual `<machine>`/`<software>` entry names - MAME's
/// actual machine/software-list shortnames, as opposed to the DAT's own
/// header identity.
///
/// Device-only entries (`isdevice="yes"`) are excluded: they are shared
/// sub-components (a CPU, a BIOS chip) that appear across many unrelated
/// drivers and would otherwise manufacture false agreement or false
/// conflict.
///
/// A single resolvable machine name is never enough evidence by itself once
/// the sampled population is more than a handful of entries: it is
/// indistinguishable from a coincidental alias collision inside an otherwise
/// heterogeneous MAME/arcade set. Only a platform (or equivalence group of
/// platforms) that an overwhelming share of every *resolved* name in the
/// sample agrees on is promoted to [`DatPlatformConfidence::Strong`]; every
/// other resolved name - the losing side of a conflict, or the only tier that
/// exists when nothing reaches that bar - is still returned, but only at
/// [`DatPlatformConfidence::Weak`], so it can never itself decide or conflict
/// with a genuinely strong fact from elsewhere.
fn machine_shortname_evidence(games: &[DatGameEntry]) -> Vec<DatPlatformEvidence> {
    let sample: Vec<&DatGameEntry> = games
        .iter()
        .filter(|game| game.is_device.as_deref() != Some("yes"))
        .take(MAX_MACHINE_SHORTNAME_SAMPLE)
        .collect();

    // Distinct raw names only: a catalogue that repeats one machine name
    // across several ROM/disk variants must not let that repetition count as
    // several independent votes.
    let mut seen_raw: BTreeSet<&str> = BTreeSet::new();
    let mut resolved: Vec<(&'static str, &str)> = Vec::new();
    for game in &sample {
        let raw = game.name.as_str();
        if !seen_raw.insert(raw) {
            continue;
        }
        if let Some(platform) = resolve_segment(raw) {
            resolved.push((platform, raw));
        }
    }

    if resolved.is_empty() {
        return Vec::new();
    }

    let mut groups: BTreeMap<&'static str, Vec<(&'static str, &str)>> = BTreeMap::new();
    for (platform, raw) in &resolved {
        groups
            .entry(equivalence_representative(platform))
            .or_default()
            .push((*platform, *raw));
    }

    // Largest distinct-name count first; ties broken by representative id so
    // the "winner" considered below is always the same regardless of
    // iteration or insertion order. A tie can never itself qualify as an
    // overwhelming majority (see the ratio check), so this ordering only
    // affects which group a genuine tie is *attributed* to for the purpose of
    // computing the ratio, never which group wins one it should not.
    let mut counts: Vec<(&'static str, usize)> = groups
        .iter()
        .map(|(representative, members)| (*representative, members.len()))
        .collect();
    counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));

    let considered = sample.len();
    let total_resolved = resolved.len();
    let winning_representative = counts.first().and_then(|(representative, count)| {
        let qualifies = if considered <= SMALL_MACHINE_SAMPLE_LIMIT {
            // Tiny sample: only a single, uncontradicted resolvable group
            // counts as signal. Two tiny but conflicting groups (a C128 name
            // and a C64 name in a two-entry DAT) must not let insertion order
            // arbitrarily pick one.
            groups.len() == 1
        } else {
            *count >= MIN_RESOLVED_FOR_LARGE_SAMPLE_MAJORITY
                && count.saturating_mul(LARGE_SAMPLE_MAJORITY_DEN)
                    >= total_resolved.saturating_mul(LARGE_SAMPLE_MAJORITY_NUM)
        };
        qualifies.then_some(*representative)
    });

    let mut seen_evidence: BTreeSet<(&'static str, &str)> = BTreeSet::new();
    let mut evidence = Vec::new();
    for (representative, members) in &groups {
        let confidence = if winning_representative == Some(*representative) {
            DatPlatformConfidence::Strong
        } else {
            DatPlatformConfidence::Weak
        };
        for &(platform, raw) in members {
            if !seen_evidence.insert((platform, raw)) {
                continue;
            }
            let detail = if confidence == DatPlatformConfidence::Strong {
                format!(
                    "the machine/software entry name `{raw}` resolves to this platform, and \
                     {}/{total_resolved} resolved entries agree",
                    members.len()
                )
            } else {
                format!(
                    "the machine/software entry name `{raw}` resolves to this platform, but the \
                     sampled population does not agree strongly enough for this to decide alone"
                )
            };
            evidence.push(DatPlatformEvidence {
                platform: platform.to_string(),
                machine_key: Some(raw.to_string()),
                kind: DatPlatformEvidenceKind::MachineShortname,
                confidence,
                detail,
            });
        }
    }
    evidence
}

/// Weak evidence from the DAT file's own filename (its stem, extension
/// dropped). A string operation on the already-known `DatSource::file_path`,
/// never a filesystem read. Per the established rule, a filename is never
/// authority on its own - this can only ever produce [`DatPlatformConfidence::Weak`].
fn filename_hint_evidence_from_path(file_path: &str) -> Vec<DatPlatformEvidence> {
    let Some(stem) = Path::new(file_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
    else {
        return Vec::new();
    };
    weak_evidence_from_text(stem, DatPlatformEvidenceKind::FilenameCorroboration)
}

/// Weak evidence from the name of the folder the DAT file lives in. Same
/// string-only treatment as [`filename_hint_evidence_from_path`].
fn folder_hint_evidence_from_path(file_path: &str) -> Vec<DatPlatformEvidence> {
    let Some(folder) = Path::new(file_path)
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
    else {
        return Vec::new();
    };
    weak_evidence_from_text(folder, DatPlatformEvidenceKind::FolderHint)
}

/// Weak evidence from one extension seen among the DAT's own ROM entries.
/// Never used as authority on its own; exposed as a standalone helper because
/// scanning every ROM entry across a large catalogue is a real cost a caller
/// should opt into rather than pay on every [`gather_dat_platform_evidence`]
/// call.
pub fn media_extension_hint_evidence<'a>(
    extensions: impl IntoIterator<Item = &'a str>,
) -> Vec<DatPlatformEvidence> {
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut evidence = Vec::new();
    for extension in extensions {
        let displayed = extension.trim_start_matches('.');
        for platform in platform_candidates_for_extension(extension) {
            if !seen.insert(platform) {
                continue;
            }
            evidence.push(DatPlatformEvidence {
                platform: platform.to_string(),
                machine_key: None,
                kind: DatPlatformEvidenceKind::MediaExtension,
                confidence: DatPlatformConfidence::Weak,
                detail: format!(
                    "`.{displayed}` is consistent with this platform, but an extension is never authority alone"
                ),
            });
        }
    }
    evidence
}

/// Evidence from a registered byte signature matched against `data`.
///
/// Opt-in, like [`media_extension_hint_evidence`]: not part of
/// [`gather_dat_platform_evidence`]'s default pipeline, and not called by
/// anything in this chunk. A DAT file's own bytes are never a ROM's bytes -
/// this exists for a future caller that has *both* a parsed DAT and a real
/// ROM's bytes (from an audit or similar) and wants to combine them; nothing
/// here reads a file itself, and this chunk wires it into nothing.
///
/// Confidence comes directly from each matching rule's own reviewed
/// [`crate::platform::MagicConfidence`] - never inferred from how many
/// platforms the actual bytes happened to match. Candidate-count is not a
/// safe stand-in for distinctiveness: registry magic coverage is incomplete,
/// so a byte pattern matching only one *currently registered* platform does
/// not prove it is unique to that platform (Sega 32X shares Mega Drive's
/// `SEGA` cartridge header, but 32X has no rule of its own yet - see
/// [`crate::platform::MagicConfidence`]'s own documentation for the full
/// reasoning). A rule reviewed and found genuinely distinctive - a literal
/// platform name, a documented format-specific magic number - is
/// [`DatPlatformConfidence::Strong`]. A rule known to be shared with another
/// platform (the `TMR SEGA` header shared by the Master System and Game
/// Gear; the `PLAYSTATION` system identifier shared by the PS1 and PS2) or
/// judged to be a family/base-hardware convention a related, unregistered
/// platform plausibly shares too (Mega Drive's `SEGA` header) is
/// [`DatPlatformConfidence::Corroborated`] instead: real, but never enough to
/// decide or conflict on its own. When nothing matches, this returns
/// nothing.
pub fn magic_hint_evidence(data: &[u8]) -> Vec<DatPlatformEvidence> {
    platform_magic_confidence_from_bytes(data)
        .into_iter()
        .map(|(platform, confidence)| {
            let confidence = match confidence {
                MagicConfidence::Strong => DatPlatformConfidence::Strong,
                MagicConfidence::Corroborated => DatPlatformConfidence::Corroborated,
            };
            DatPlatformEvidence {
                platform: platform.to_string(),
                machine_key: None,
                kind: DatPlatformEvidenceKind::HeaderMagic,
                confidence,
                detail: if confidence == DatPlatformConfidence::Strong {
                    "a reviewed, distinctive byte signature for this platform matched these bytes"
                        .to_string()
                } else {
                    "a byte signature matched these bytes, but it is shared or family-level and \
                     cannot decide alone"
                        .to_string()
                },
            }
        })
        .collect()
}

fn weak_evidence_from_text(text: &str, kind: DatPlatformEvidenceKind) -> Vec<DatPlatformEvidence> {
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    let mut evidence = Vec::new();
    for segment in header_segments(text) {
        let Some(platform) = resolve_segment(&segment) else {
            continue;
        };
        if !seen.insert(platform) {
            continue;
        }
        evidence.push(DatPlatformEvidence {
            platform: platform.to_string(),
            machine_key: None,
            kind,
            confidence: DatPlatformConfidence::Weak,
            detail: format!(
                "{} `{text}` names this platform via `{segment}`, but {} alone is never authority",
                kind.label(),
                kind.label().to_lowercase()
            ),
        });
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(
        platform: &str,
        kind: DatPlatformEvidenceKind,
        confidence: DatPlatformConfidence,
        machine_key: Option<&str>,
        detail: &str,
    ) -> DatPlatformEvidence {
        DatPlatformEvidence {
            platform: platform.to_string(),
            machine_key: machine_key.map(str::to_string),
            kind,
            confidence,
            detail: detail.to_string(),
        }
    }

    #[test]
    fn tosec_sega_mark_iii_and_master_system_header_resolves_to_master_system() {
        // "Sega Mark III & Master System - Games" tokenises into two segments
        // that both resolve, via the registry's own aliases, to the same
        // canonical `MasterSystem` id - not two different platforms.
        let result = resolve_dat_platform_identity(vec![
            evidence(
                "MasterSystem",
                DatPlatformEvidenceKind::HeaderName,
                DatPlatformConfidence::Strong,
                None,
                "header name segment `Sega Mark III` names this platform",
            ),
            evidence(
                "MasterSystem",
                DatPlatformEvidenceKind::HeaderName,
                DatPlatformConfidence::Strong,
                None,
                "header name segment `Master System` names this platform",
            ),
        ]);
        assert_eq!(result.platform(), Some("MasterSystem"));
        assert!(!result.is_ambiguous());
    }

    #[test]
    fn commodore_c128_header_resolves_to_c128_never_c64() {
        let result = resolve_dat_platform_identity(vec![evidence(
            "Commodore 128",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name `Commodore C128` names this platform",
        )]);
        assert_eq!(result.platform(), Some("Commodore 128"));
        assert_ne!(result.platform(), Some("Commodore 64"));
    }

    #[test]
    fn neocd_machine_shortname_resolves_to_neo_geo_cd() {
        let result = resolve_dat_platform_identity(vec![evidence(
            "Neo Geo CD",
            DatPlatformEvidenceKind::MachineShortname,
            DatPlatformConfidence::Strong,
            Some("neocd"),
            "sampled machine shortname `neocd` names this platform",
        )]);
        assert_eq!(result.platform(), Some("Neo Geo CD"));
        match result {
            DatPlatformIdentity::Resolved { machine_key, .. } => {
                assert_eq!(machine_key.as_deref(), Some("neocd"));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn conflicting_strong_c128_and_c64_evidence_is_ambiguous() {
        let result = resolve_dat_platform_identity(vec![
            evidence(
                "Commodore 128",
                DatPlatformEvidenceKind::HeaderName,
                DatPlatformConfidence::Strong,
                None,
                "header name names Commodore 128",
            ),
            evidence(
                "Commodore 64",
                DatPlatformEvidenceKind::MachineShortname,
                DatPlatformConfidence::Strong,
                Some("c64"),
                "sampled machine shortname names Commodore 64",
            ),
        ]);
        assert!(result.is_ambiguous());
        assert_eq!(result.platform(), None);
        match result {
            DatPlatformIdentity::Ambiguous { candidates } => {
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn filename_only_evidence_resolves_unknown() {
        let result = resolve_dat_platform_identity(vec![evidence(
            "MegaDrive",
            DatPlatformEvidenceKind::FilenameCorroboration,
            DatPlatformConfidence::Weak,
            None,
            "the DAT's own filename names this platform",
        )]);
        assert_eq!(result, DatPlatformIdentity::Unknown);
    }

    #[test]
    fn media_extension_only_evidence_resolves_unknown() {
        let result = resolve_dat_platform_identity(vec![evidence(
            "PSX",
            DatPlatformEvidenceKind::MediaExtension,
            DatPlatformConfidence::Weak,
            None,
            "`.bin` is shared with several disc platforms",
        )]);
        assert_eq!(result, DatPlatformIdentity::Unknown);
    }

    #[test]
    fn weak_evidence_agreeing_with_strong_evidence_does_not_downgrade_it() {
        let result = resolve_dat_platform_identity(vec![
            evidence(
                "MegaDrive",
                DatPlatformEvidenceKind::HeaderName,
                DatPlatformConfidence::Strong,
                None,
                "header name names Mega Drive",
            ),
            evidence(
                "MegaDrive",
                DatPlatformEvidenceKind::MediaExtension,
                DatPlatformConfidence::Weak,
                None,
                "`.md` extension is consistent with this platform",
            ),
        ]);
        match result {
            DatPlatformIdentity::Resolved {
                platform,
                confidence,
                evidence,
                ..
            } => {
                assert_eq!(platform, "MegaDrive");
                assert_eq!(confidence, DatPlatformConfidence::Strong);
                // The weak evidence is retained for provenance, not dropped.
                assert_eq!(evidence.len(), 2);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn strong_evidence_wins_over_conflicting_weak_evidence() {
        let result = resolve_dat_platform_identity(vec![
            evidence(
                "Commodore 128",
                DatPlatformEvidenceKind::HeaderName,
                DatPlatformConfidence::Strong,
                None,
                "header name names Commodore 128",
            ),
            evidence(
                "Commodore 64",
                DatPlatformEvidenceKind::FilenameCorroboration,
                DatPlatformConfidence::Weak,
                None,
                "the filename happens to also say Commodore 64",
            ),
        ]);
        assert_eq!(result.platform(), Some("Commodore 128"));
        assert!(!result.is_ambiguous());
    }

    #[test]
    fn evidence_order_does_not_affect_the_result() {
        let a = evidence(
            "Neo Geo CD",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names Neo Geo CD",
        );
        let b = evidence(
            "Neo Geo CD",
            DatPlatformEvidenceKind::SoftwareListName,
            DatPlatformConfidence::Strong,
            Some("neocdz"),
            "software-list name names Neo Geo CD",
        );
        let c = evidence(
            "Neo Geo CD",
            DatPlatformEvidenceKind::MediaExtension,
            DatPlatformConfidence::Weak,
            None,
            "shared disc extension",
        );

        let forward = resolve_dat_platform_identity(vec![a.clone(), b.clone(), c.clone()]);
        let reversed = resolve_dat_platform_identity(vec![c, b, a]);
        assert_eq!(forward, reversed);
    }

    #[test]
    fn equivalent_canonical_platforms_agree_rather_than_conflict() {
        // `PC Engine` and `TurboGrafx-16` are two stored identifiers for the
        // same hardware (see `platform::EQUIVALENT_PLATFORM_IDS`); a DAT that
        // names both must resolve, not conflict.
        let result = resolve_dat_platform_identity(vec![
            evidence(
                "PC Engine",
                DatPlatformEvidenceKind::HeaderName,
                DatPlatformConfidence::Strong,
                None,
                "header name names PC Engine",
            ),
            evidence(
                "TurboGrafx-16",
                DatPlatformEvidenceKind::HeaderDescription,
                DatPlatformConfidence::Strong,
                None,
                "header description names TurboGrafx-16",
            ),
        ]);
        assert!(!result.is_ambiguous());
        assert!(matches!(
            result.platform(),
            Some("PC Engine") | Some("TurboGrafx-16")
        ));
    }

    #[test]
    fn no_evidence_resolves_unknown() {
        assert_eq!(
            resolve_dat_platform_identity(Vec::new()),
            DatPlatformIdentity::Unknown
        );
    }

    // ------------------------------------------------------------------
    // Extension-candidate evidence: always Weak, never authoritative alone.
    // ------------------------------------------------------------------

    #[test]
    fn extension_only_nes_evidence_resolves_unknown() {
        let evidence = media_extension_hint_evidence(["nes"]);
        assert!(
            evidence
                .iter()
                .all(|item| item.confidence == DatPlatformConfidence::Weak)
        );
        assert_eq!(
            resolve_dat_platform_identity(evidence),
            DatPlatformIdentity::Unknown
        );
    }

    #[test]
    fn extension_only_d64_evidence_resolves_unknown() {
        let evidence = media_extension_hint_evidence(["d64"]);
        assert_eq!(
            resolve_dat_platform_identity(evidence),
            DatPlatformIdentity::Unknown
        );
    }

    #[test]
    fn strong_c128_header_evidence_wins_over_weak_d64_extension_candidates() {
        let mut evidence = vec![evidence(
            "Commodore 128",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names Commodore 128",
        )];
        evidence.extend(media_extension_hint_evidence(["d64"]));
        let result = resolve_dat_platform_identity(evidence);
        assert_eq!(result.platform(), Some("Commodore 128"));
        assert!(!result.is_ambiguous());
    }

    #[test]
    fn strong_neo_geo_cd_evidence_wins_over_weak_cue_extension_candidates() {
        let mut evidence = vec![evidence(
            "Neo Geo CD",
            DatPlatformEvidenceKind::SoftwareListName,
            DatPlatformConfidence::Strong,
            Some("neocd"),
            "software-list name names Neo Geo CD",
        )];
        evidence.extend(media_extension_hint_evidence(["cue"]));
        let result = resolve_dat_platform_identity(evidence);
        assert_eq!(result.platform(), Some("Neo Geo CD"));
        assert!(!result.is_ambiguous());
    }

    #[test]
    fn conflicting_weak_extension_candidates_never_create_ambiguity() {
        // `.d64` alone already spans several Weak candidates (Commodore 64
        // and Commodore 128 among them); with no Strong evidence anywhere,
        // that spread must never be reported as a conflict - it is simply
        // unresolved.
        let evidence = media_extension_hint_evidence(["d64"]);
        let result = resolve_dat_platform_identity(evidence);
        assert!(!result.is_ambiguous());
        assert_eq!(result, DatPlatformIdentity::Unknown);
    }

    #[test]
    fn archive_container_extensions_never_become_platform_authority() {
        for extension in ["zip", "7z"] {
            let evidence = media_extension_hint_evidence([extension]);
            assert!(
                evidence
                    .iter()
                    .all(|item| item.confidence == DatPlatformConfidence::Weak),
                "`.{extension}` evidence must stay Weak"
            );
            assert_eq!(
                resolve_dat_platform_identity(evidence),
                DatPlatformIdentity::Unknown
            );
        }
    }

    // ------------------------------------------------------------------
    // Magic/header-byte evidence: always about a real ROM's bytes, never a
    // DAT's text, and never Strong when the signature is shared.
    // ------------------------------------------------------------------

    fn bytes_with_signature_at(offset: usize, pattern: &[u8]) -> Vec<u8> {
        let mut buffer = vec![0u8; offset + pattern.len()];
        buffer[offset..offset + pattern.len()].copy_from_slice(pattern);
        buffer
    }

    #[test]
    fn distinctive_magic_alone_is_strong_and_resolves() {
        let nes_header = bytes_with_signature_at(0, b"NES\x1a");
        let evidence = magic_hint_evidence(&nes_header);
        assert!(
            evidence
                .iter()
                .all(|item| item.confidence == DatPlatformConfidence::Strong)
        );
        assert_eq!(
            resolve_dat_platform_identity(evidence).platform(),
            Some("NES")
        );
    }

    #[test]
    fn shared_magic_alone_never_resolves() {
        // `TMR SEGA` is shared by the Master System and Game Gear; on its
        // own it must never pick either.
        let sega_header = bytes_with_signature_at(0x7ff0, b"TMR SEGA");
        let evidence = magic_hint_evidence(&sega_header);
        assert!(
            evidence
                .iter()
                .all(|item| item.confidence == DatPlatformConfidence::Corroborated)
        );
        assert_eq!(
            resolve_dat_platform_identity(evidence),
            DatPlatformIdentity::Unknown
        );
    }

    #[test]
    fn unknown_bytes_produce_no_magic_evidence() {
        assert!(magic_hint_evidence(&[1, 2, 3, 4, 5]).is_empty());
    }

    #[test]
    fn strong_dat_identity_and_agreeing_magic_remain_resolved_strong() {
        let mut all_evidence = vec![evidence(
            "NES",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names NES",
        )];
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(0, b"NES\x1a")));
        let result = resolve_dat_platform_identity(all_evidence);
        assert_eq!(result.platform(), Some("NES"));
        match result {
            DatPlatformIdentity::Resolved { confidence, .. } => {
                assert_eq!(confidence, DatPlatformConfidence::Strong);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn strong_dat_identity_and_conflicting_strong_magic_is_ambiguous() {
        let mut all_evidence = vec![evidence(
            "NES",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names NES",
        )];
        // Distinctive on its own for a different platform entirely.
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0,
            b"SEGA SEGAKATANA",
        )));
        let result = resolve_dat_platform_identity(all_evidence);
        assert!(result.is_ambiguous());
    }

    #[test]
    fn weak_extension_and_strong_magic_resolves_only_because_magic_is_strong() {
        let mut all_evidence = media_extension_hint_evidence(["bin"]);
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0,
            &[0x80, 0x37, 0x12, 0x40],
        )));
        assert_eq!(
            resolve_dat_platform_identity(all_evidence).platform(),
            Some("N64")
        );
    }

    #[test]
    fn weak_extension_and_only_corroborated_magic_does_not_resolve() {
        let mut all_evidence = media_extension_hint_evidence(["bin"]);
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0x8008,
            b"PLAYSTATION",
        )));
        assert!(
            all_evidence
                .iter()
                .filter(|item| item.kind == DatPlatformEvidenceKind::HeaderMagic)
                .all(|item| item.confidence == DatPlatformConfidence::Corroborated)
        );
        assert_eq!(
            resolve_dat_platform_identity(all_evidence),
            DatPlatformIdentity::Unknown
        );
    }

    #[test]
    fn magic_candidate_ordering_does_not_affect_the_resolved_identity() {
        let sega_header = bytes_with_signature_at(0x7ff0, b"TMR SEGA");
        let forward = magic_hint_evidence(&sega_header);
        let mut reversed = forward.clone();
        reversed.reverse();
        assert_eq!(
            resolve_dat_platform_identity(forward),
            resolve_dat_platform_identity(reversed)
        );
    }

    // ------------------------------------------------------------------
    // Explicit, reviewed magic confidence (not candidate-count).
    // ------------------------------------------------------------------

    #[test]
    fn candidate_count_alone_does_not_determine_confidence() {
        // The Mega Drive `SEGA` header is the *only* currently-registered
        // match for these bytes (Sega 32X shares the header but has no rule
        // yet), so the old candidate-count rule would have called this
        // Strong. It must not be.
        let mega_drive_header = bytes_with_signature_at(0x100, b"SEGA");
        let evidence = magic_hint_evidence(&mega_drive_header);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].platform, "MegaDrive");
        assert_eq!(evidence[0].confidence, DatPlatformConfidence::Corroborated);
    }

    #[test]
    fn mega_drive_sega_header_alone_never_resolves() {
        let mega_drive_header = bytes_with_signature_at(0x100, b"SEGA");
        assert_eq!(
            resolve_dat_platform_identity(magic_hint_evidence(&mega_drive_header)),
            DatPlatformIdentity::Unknown
        );
    }

    #[test]
    fn tmr_sega_remains_corroborated() {
        let sega_header = bytes_with_signature_at(0x7ff0, b"TMR SEGA");
        let evidence = magic_hint_evidence(&sega_header);
        assert!(!evidence.is_empty());
        assert!(
            evidence
                .iter()
                .all(|item| item.confidence == DatPlatformConfidence::Corroborated)
        );
    }

    #[test]
    fn playstation_identifier_remains_corroborated() {
        let playstation_header = bytes_with_signature_at(0x8008, b"PLAYSTATION");
        let evidence = magic_hint_evidence(&playstation_header);
        assert!(!evidence.is_empty());
        assert!(
            evidence
                .iter()
                .all(|item| item.confidence == DatPlatformConfidence::Corroborated)
        );
    }

    #[test]
    fn strong_dat_identity_and_agreeing_corroborated_magic_remains_resolved_strong() {
        let mut all_evidence = vec![evidence(
            "MegaDrive",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names Mega Drive",
        )];
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0x100, b"SEGA",
        )));
        let result = resolve_dat_platform_identity(all_evidence);
        assert_eq!(result.platform(), Some("MegaDrive"));
        match result {
            DatPlatformIdentity::Resolved { confidence, .. } => {
                assert_eq!(confidence, DatPlatformConfidence::Strong);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn strong_dat_identity_wins_over_conflicting_corroborated_magic() {
        let mut all_evidence = vec![evidence(
            "NES",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names NES",
        )];
        // Corroborated-only (shared) magic for a different platform entirely.
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0x100, b"SEGA",
        )));
        let result = resolve_dat_platform_identity(all_evidence);
        assert_eq!(result.platform(), Some("NES"));
        assert!(!result.is_ambiguous());
    }

    #[test]
    fn zxtape_magic_alone_resolves_unknown() {
        let tzx_header = bytes_with_signature_at(0, b"ZXTape!\x1a");
        assert_eq!(
            resolve_dat_platform_identity(magic_hint_evidence(&tzx_header)),
            DatPlatformIdentity::Unknown
        );
    }

    #[test]
    fn strong_zx_spectrum_dat_and_agreeing_corroborated_tzx_remains_resolved_strong() {
        let mut all_evidence = vec![evidence(
            "ZX Spectrum",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names ZX Spectrum",
        )];
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0,
            b"ZXTape!\x1a",
        )));
        let result = resolve_dat_platform_identity(all_evidence);
        assert_eq!(result.platform(), Some("ZX Spectrum"));
        match result {
            DatPlatformIdentity::Resolved { confidence, .. } => {
                assert_eq!(confidence, DatPlatformConfidence::Strong);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn strong_non_zx_dat_wins_over_conflicting_corroborated_tzx() {
        let mut all_evidence = vec![evidence(
            "Amstrad CPC",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names Amstrad CPC",
        )];
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0,
            b"ZXTape!\x1a",
        )));
        let result = resolve_dat_platform_identity(all_evidence);
        assert_eq!(result.platform(), Some("Amstrad CPC"));
        assert!(!result.is_ambiguous());
    }

    #[test]
    fn segadiscsystem_magic_alone_resolves_unknown() {
        let sega_cd_header = bytes_with_signature_at(0, b"SEGADISCSYSTEM");
        assert_eq!(
            resolve_dat_platform_identity(magic_hint_evidence(&sega_cd_header)),
            DatPlatformIdentity::Unknown
        );
        let sega_cd_header_alt_offset = bytes_with_signature_at(0x10, b"SEGADISCSYSTEM");
        assert_eq!(
            resolve_dat_platform_identity(magic_hint_evidence(&sega_cd_header_alt_offset)),
            DatPlatformIdentity::Unknown
        );
    }

    #[test]
    fn strong_sega_cd_dat_and_agreeing_segadiscsystem_remains_resolved_strong() {
        let mut all_evidence = vec![evidence(
            "Sega CD",
            DatPlatformEvidenceKind::SoftwareListName,
            DatPlatformConfidence::Strong,
            Some("megacd"),
            "software-list name names Sega CD",
        )];
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0,
            b"SEGADISCSYSTEM",
        )));
        let result = resolve_dat_platform_identity(all_evidence);
        assert_eq!(result.platform(), Some("Sega CD"));
        match result {
            DatPlatformIdentity::Resolved { confidence, .. } => {
                assert_eq!(confidence, DatPlatformConfidence::Strong);
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn strong_conflicting_dat_wins_over_corroborated_segadiscsystem() {
        let mut all_evidence = vec![evidence(
            "PSX",
            DatPlatformEvidenceKind::HeaderName,
            DatPlatformConfidence::Strong,
            None,
            "header name names PSX",
        )];
        all_evidence.extend(magic_hint_evidence(&bytes_with_signature_at(
            0,
            b"SEGADISCSYSTEM",
        )));
        let result = resolve_dat_platform_identity(all_evidence);
        assert_eq!(result.platform(), Some("PSX"));
        assert!(!result.is_ambiguous());
    }

    // ------------------------------------------------------------------
    // End-to-end: real parsed DAT -> gathered evidence -> resolved identity.
    // ------------------------------------------------------------------

    mod end_to_end {
        use super::super::*;
        use crate::dat::limits::DatLimits;
        use crate::dat::parsers::logiqx::parse_logiqx;

        /// Parses `xml` from a real temporary file at `relative_path`, so
        /// filename/folder evidence has something real to read from
        /// `ParsedDat::source::file_path` - a string, never reopened.
        fn parse_at(xml: &str, relative_path: &str) -> ParsedDat {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(relative_path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, xml).unwrap();
            parse_logiqx(&path, DatLimits::default()).unwrap().dat
        }

        fn parse(xml: &str) -> ParsedDat {
            parse_at(xml, "test.dat")
        }

        #[test]
        fn tosec_style_header_naming_mark_iii_and_master_system_resolves_to_master_system() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>TOSEC - Sega Mark III &amp; Master System - Games</name>
    </header>
    <game name="Alex Kidd">
        <rom name="alex.sms" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#,
            );
            assert_eq!(identify_dat_source(&dat).platform(), Some("MasterSystem"));
        }

        #[test]
        fn commodore_128_header_resolves_to_c128_never_c64() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Commodore 128</name>
    </header>
    <game name="Some Game">
        <rom name="game.d71" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#,
            );
            let identity = identify_dat_source(&dat);
            assert_eq!(identity.platform(), Some("Commodore 128"));
            assert_ne!(identity.platform(), Some("Commodore 64"));
        }

        #[test]
        fn bare_mame_software_list_c128_flop_resolves_to_c128() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<softwarelist name="c128_flop" description="Commodore 128 (Floppy)">
    <software name="game1">
        <part name="flop1" interface="c128_flop">
            <dataarea name="flop">
                <rom name="game1.d71" size="1" crc="AAAAAAAA"/>
            </dataarea>
        </part>
    </software>
</softwarelist>"#,
            );
            let identity = identify_dat_source(&dat);
            assert_eq!(identity.platform(), Some("Commodore 128"));
            match identity {
                DatPlatformIdentity::Resolved { machine_key, .. } => {
                    assert_eq!(machine_key.as_deref(), Some("c128_flop"));
                }
                other => panic!("expected Resolved, got {other:?}"),
            }
        }

        #[test]
        fn bare_mame_software_list_megacd_resolves_to_sega_cd() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<softwarelist name="megacd" description="Sega Mega-CD / Sega CD">
    <software name="sonic">
        <part name="cdrom" interface="megacd_cdrom">
            <dataarea name="cdrom">
                <rom name="sonic.bin" size="1" crc="AAAAAAAA"/>
            </dataarea>
        </part>
    </software>
</softwarelist>"#,
            );
            assert_eq!(identify_dat_source(&dat).platform(), Some("Sega CD"));
        }

        #[test]
        fn neo_geo_cd_header_resolves_and_never_becomes_cartridge_neo_geo() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Neo Geo CD</name>
    </header>
    <game name="King of Fighters">
        <rom name="kof.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#,
            );
            let identity = identify_dat_source(&dat);
            assert_eq!(identity.platform(), Some("Neo Geo CD"));
            assert_ne!(identity.platform(), Some("NeoGeo"));
        }

        #[test]
        fn neocd_machine_shortname_resolves_to_neo_geo_cd_without_any_header() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <machine name="neocd">
        <rom name="neocd.bin" size="1" crc="AAAAAAAA"/>
    </machine>
</datafile>"#,
            );
            let identity = identify_dat_source(&dat);
            assert_eq!(identity.platform(), Some("Neo Geo CD"));
            match identity {
                DatPlatformIdentity::Resolved { machine_key, .. } => {
                    assert_eq!(machine_key.as_deref(), Some("neocd"));
                }
                other => panic!("expected Resolved, got {other:?}"),
            }
        }

        #[test]
        fn conflicting_strong_header_and_machine_name_is_ambiguous() {
            // The header says Commodore 128; a real machine entry in the same
            // catalogue is separately, explicitly named `c64`. Two Strong
            // facts, two different canonical platforms.
            let dat = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Commodore 128</name>
    </header>
    <machine name="c64">
        <rom name="game.prg" size="1" crc="AAAAAAAA"/>
    </machine>
</datafile>"#,
            );
            let identity = identify_dat_source(&dat);
            assert!(identity.is_ambiguous());
            assert_eq!(identity.platform(), None);
        }

        #[test]
        fn generic_commodore_wording_never_guesses_a_platform() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Commodore Software</name>
    </header>
    <game name="Some Game">
        <rom name="game.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#,
            );
            assert_eq!(identify_dat_source(&dat), DatPlatformIdentity::Unknown);
        }

        #[test]
        fn filename_only_clue_is_never_enough_on_its_own() {
            // No header at all, and the one game entry's name matches no
            // platform. Only the filename (`Sega Master System.dat`) carries
            // any platform-shaped text, and a filename is Weak-only.
            let dat = parse_at(
                r#"<?xml version="1.0"?>
<datafile>
    <game name="Sonic the Hedgehog">
        <rom name="sonic.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#,
                "Sega Master System.dat",
            );
            let evidence = gather_dat_platform_evidence(&dat);
            assert!(
                evidence.iter().any(|item| item.kind
                    == DatPlatformEvidenceKind::FilenameCorroboration
                    && item.confidence == DatPlatformConfidence::Weak),
                "the filename should still be gathered as weak evidence"
            );
            assert_eq!(identify_dat_source(&dat), DatPlatformIdentity::Unknown);
        }

        #[test]
        fn unknown_mame_suffix_is_never_generically_stripped() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<softwarelist name="c128_weirdthing">
    <software name="game1">
        <part name="cart">
            <dataarea name="rom">
                <rom name="game1.bin" size="1" crc="AAAAAAAA"/>
            </dataarea>
        </part>
    </software>
</softwarelist>"#,
            );
            assert_eq!(identify_dat_source(&dat), DatPlatformIdentity::Unknown);
        }

        #[test]
        fn gathered_evidence_order_does_not_affect_the_resolved_identity() {
            let forward = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Neo Geo CD</name>
    </header>
    <machine name="neocdz">
        <rom name="a.bin" size="1" crc="AAAAAAAA"/>
    </machine>
    <machine name="neocd">
        <rom name="b.bin" size="1" crc="BBBBBBBB"/>
    </machine>
</datafile>"#,
            );
            let reversed = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Neo Geo CD</name>
    </header>
    <machine name="neocd">
        <rom name="b.bin" size="1" crc="BBBBBBBB"/>
    </machine>
    <machine name="neocdz">
        <rom name="a.bin" size="1" crc="AAAAAAAA"/>
    </machine>
</datafile>"#,
            );
            assert_eq!(
                identify_dat_source(&forward),
                identify_dat_source(&reversed)
            );
        }

        #[test]
        fn a_normal_logiqx_dat_with_a_nested_header_still_identifies_correctly() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Nintendo - Game Boy</name>
        <description>Nintendo - Game Boy (No-Intro)</description>
    </header>
    <game name="Tetris">
        <rom name="tetris.gb" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#,
            );
            assert_eq!(identify_dat_source(&dat).platform(), Some("Game Boy"));
        }

        #[test]
        fn a_bare_software_list_root_identifies_correctly_without_any_nested_header() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<softwarelist name="neocd" description="Neo Geo CD">
    <software name="kof">
        <part name="cart">
            <dataarea name="rom">
                <rom name="kof.bin" size="1" crc="AAAAAAAA"/>
            </dataarea>
        </part>
    </software>
</softwarelist>"#,
            );
            let identity = identify_dat_source(&dat);
            assert_eq!(identity.platform(), Some("Neo Geo CD"));
            assert!(
                identity
                    .evidence()
                    .iter()
                    .any(|item| item.kind == DatPlatformEvidenceKind::SoftwareListName)
            );
        }

        // --------------------------------------------------------------
        // MachineShortname agreement gating.
        // --------------------------------------------------------------

        /// Builds a header-less, description-less `<datafile>` with one
        /// `<machine>` per name, so only `MachineShortname` evidence (plus an
        /// uninformative generic filename) is in play.
        fn machine_dat_xml(names: &[&str]) -> String {
            let mut xml = String::from("<?xml version=\"1.0\"?>\n<datafile>\n");
            for (index, name) in names.iter().enumerate() {
                xml.push_str(&format!(
                    "    <machine name=\"{name}\">\n        \
                     <rom name=\"r{index}.bin\" size=\"1\" crc=\"AAAAAAAA\"/>\n    </machine>\n"
                ));
            }
            xml.push_str("</datafile>");
            xml
        }

        #[test]
        fn a_single_explicit_machine_shortname_still_resolves_neo_geo_cd() {
            let dat = parse(&machine_dat_xml(&["neocd"]));
            assert_eq!(identify_dat_source(&dat).platform(), Some("Neo Geo CD"));
        }

        #[test]
        fn many_unresolved_machine_names_plus_one_neocd_do_not_resolve_from_shortname_alone() {
            // 19 distinct, unresolved names plus one real `neocd` match: a
            // large, heterogeneous population with a single stray hit.
            let mut names: Vec<String> = (0..19)
                .map(|index| format!("unrelated-arcade-machine-{index}"))
                .collect();
            names.push("neocd".to_string());
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let dat = parse(&machine_dat_xml(&refs));

            let evidence = gather_dat_platform_evidence(&dat);
            assert!(
                evidence.iter().any(|item| {
                    item.kind == DatPlatformEvidenceKind::MachineShortname
                        && item.platform == "Neo Geo CD"
                        && item.confidence == DatPlatformConfidence::Weak
                }),
                "the lone `neocd` match should still be recorded, but only as weak provenance"
            );
            assert!(
                !evidence.iter().any(|item| {
                    item.kind == DatPlatformEvidenceKind::MachineShortname
                        && item.confidence == DatPlatformConfidence::Strong
                }),
                "one match among many unrelated, unresolved entries must never be Strong"
            );
            assert_eq!(identify_dat_source(&dat), DatPlatformIdentity::Unknown);
        }

        #[test]
        fn a_strong_majority_of_resolvable_shortnames_produces_strong_evidence() {
            let dat = parse(&machine_dat_xml(&[
                "neocd",
                "neogeocd",
                "ngcd",
                "snkneogeocd",
                "some-unrelated-game",
                "another-unrelated-game",
            ]));
            let identity = identify_dat_source(&dat);
            assert_eq!(identity.platform(), Some("Neo Geo CD"));
            assert!(
                identity
                    .evidence()
                    .iter()
                    .filter(|item| item.kind == DatPlatformEvidenceKind::MachineShortname)
                    .all(|item| item.platform == "Neo Geo CD"
                        || item.confidence == DatPlatformConfidence::Weak)
            );
        }

        #[test]
        fn a_tiny_mixed_c128_and_c64_population_resolves_neither() {
            let dat = parse(&machine_dat_xml(&["c128", "c64"]));
            assert_eq!(identify_dat_source(&dat), DatPlatformIdentity::Unknown);
        }

        #[test]
        fn a_large_evenly_mixed_c128_and_c64_population_resolves_neither_and_is_order_independent()
        {
            let forward = [
                "c128",
                "c128d",
                "Commodore 128",
                "Commodore C128",
                "c64",
                "c64gs",
                "Commodore 64",
                "Commodore C64",
            ];
            let mut reversed = forward;
            reversed.reverse();

            let forward_dat = parse(&machine_dat_xml(&forward));
            let reversed_dat = parse(&machine_dat_xml(&reversed));

            let forward_identity = identify_dat_source(&forward_dat);
            let reversed_identity = identify_dat_source(&reversed_dat);

            assert_eq!(forward_identity, DatPlatformIdentity::Unknown);
            assert_eq!(forward_identity, reversed_identity);
        }

        #[test]
        fn equivalent_machine_shortname_aliases_count_together_toward_a_majority() {
            let dat = parse(&machine_dat_xml(&[
                "PC Engine",
                "pce",
                "necpcengine",
                "TurboGrafx-16",
                "tg16",
            ]));
            let identity = identify_dat_source(&dat);
            assert!(!identity.is_ambiguous());
            assert!(matches!(
                identity.platform(),
                Some("PC Engine") | Some("TurboGrafx-16")
            ));
        }

        #[test]
        fn bare_software_list_c128_flop_resolves_despite_a_stray_conflicting_machine_name() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<softwarelist name="c128_flop" description="Commodore 128 (Floppy)">
    <software name="c64">
        <part name="flop1" interface="c128_flop">
            <dataarea name="flop">
                <rom name="stray.d71" size="1" crc="AAAAAAAA"/>
            </dataarea>
        </part>
    </software>
    <software name="sonic">
        <rom name="sonic.bin" size="1" crc="BBBBBBBB"/>
    </software>
    <software name="mario">
        <rom name="mario.bin" size="1" crc="CCCCCCCC"/>
    </software>
    <software name="zelda">
        <rom name="zelda.bin" size="1" crc="DDDDDDDD"/>
    </software>
</softwarelist>"#,
            );
            // A single stray `c64` machine name amid several unrelated,
            // unresolved entries must never win MachineShortname gating, and
            // must never be allowed to conflict with the root's own Strong
            // SoftwareListName evidence either.
            assert_eq!(identify_dat_source(&dat).platform(), Some("Commodore 128"));
        }

        #[test]
        fn bare_software_list_megacd_resolves_despite_heterogeneous_machine_entries() {
            let dat = parse(
                r#"<?xml version="1.0"?>
<softwarelist name="megacd" description="Sega Mega-CD / Sega CD">
    <software name="psx">
        <part name="cdrom" interface="megacd_cdrom">
            <dataarea name="cdrom">
                <rom name="stray.bin" size="1" crc="AAAAAAAA"/>
            </dataarea>
        </part>
    </software>
    <software name="sonic">
        <rom name="sonic.bin" size="1" crc="BBBBBBBB"/>
    </software>
    <software name="streets-of-rage">
        <rom name="sor.bin" size="1" crc="CCCCCCCC"/>
    </software>
    <software name="golden-axe">
        <rom name="ga.bin" size="1" crc="DDDDDDDD"/>
    </software>
</softwarelist>"#,
            );
            assert_eq!(identify_dat_source(&dat).platform(), Some("Sega CD"));
        }

        #[test]
        fn filename_only_remains_unknown_after_shortname_gating() {
            let dat = parse_at(
                r#"<?xml version="1.0"?>
<datafile>
    <game name="Sonic the Hedgehog">
        <rom name="sonic.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#,
                "Sega Master System.dat",
            );
            assert_eq!(identify_dat_source(&dat), DatPlatformIdentity::Unknown);
        }
    }
}
