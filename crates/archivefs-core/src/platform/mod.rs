//! The single authoritative platform registry.
//!
//! Every piece of platform knowledge this build has lives in [`PLATFORMS`]:
//! canonical identifier, display name, folder and filename aliases, strong and
//! shared extensions, bounded magic-byte signatures, multi-file layout
//! evidence, conflicting platforms and the explanation shown to a person. The
//! GUI, the CLI and the core scanner all read from here - none of them keeps a
//! second list that can drift.
//!
//! # Canonical identifiers are stable
//!
//! [`Platform::id`] is what gets written to `platform_assignments.platform` in
//! the database, so these strings are a storage contract and are never renamed
//! casually. Where two identifiers describe what is arguably one machine -
//! `PC Engine` and `TurboGrafx-16`, or `PC-98` and `NEC PC-9801` - both are
//! kept and related through [`equivalent_platform_ids`] rather than merged,
//! because merging them would silently rewrite what users already have stored.
//!
//! # Matching is exact, on whole path components
//!
//! Aliases are stored already normalised (ASCII alphanumerics, lowercased -
//! see [`normalize_alias`]), so `"ZX Spectrum"`, `"zx-spectrum"`, `"zx_spectrum"`
//! and `"zxspectrum"` all key to the same entry without separate rows. Matching
//! is **exact against one whole normalised path component** and never a
//! substring: that is what keeps `bbc`, `cpc`, `mac` and `dos` safe as aliases,
//! and what stops a real library's `zx-spectrum-next`, `segacd32`, `amiga-cd`
//! and `atari-jaguar-cd` folders from being mistaken for the shorter platforms
//! whose names they contain.
//!
//! # Extensions are ranked, never trusted alone
//!
//! [`Platform::strong_extensions`] are format-specific enough to carry a
//! detection on their own. [`Platform::weak_extensions`] are shared - `.bin`,
//! `.iso`, `.cue`, `.dsk`, `.tap`, `.zip` and friends - and can only ever
//! narrow a decision, never make one. See [`SHARED_EXTENSIONS`].

use std::path::Path;

pub mod detect;
pub mod identity;

#[cfg(test)]
mod tests;

pub use detect::{
    DetectionConfidence, DetectionEvidence, DetectionRequest, DetectionSource, PlatformCandidate,
    PlatformDetectionReport, detect_platform_report,
};
pub use identity::{
    PlatformIdentityConfidence, PlatformIdentityEvidence, PlatformIdentityResolution,
    PlatformIdentitySource, resolve_platform_identity,
};

/// How confidently one [`MagicRule`] identifies its platform on its own.
///
/// This is a property of the *rule*, reviewed once by a person against what
/// is actually known about the signature - never inferred from how many
/// platforms in the current registry happen to declare a matching rule.
/// Registry coverage is incomplete, so the absence of a second platform
/// declaring the same bytes does not prove the bytes are unique to this one:
/// Sega 32X cartridges carry the identical `SEGA` header Mega Drive checks at
/// offset `0x100`, but 32X has no registered [`MagicRule`] of its own yet, so
/// counting *currently matching platforms* would wrongly call that header
/// Mega-Drive-unique. Declared weakest first so [`Ord`] gives the more
/// confident value when more than one of a platform's rules matches the same
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MagicConfidence {
    /// Real evidence, but the signature is either known to be shared with
    /// another registered platform, or is a base-hardware/family convention
    /// a closely related platform not yet in this registry plausibly shares
    /// too. Never treated as authority on its own.
    Corroborated,
    /// The signature's own semantics are genuinely, specifically tied to one
    /// platform - a literal platform name, a documented format-specific magic
    /// number, or a boot-sector string with no known collision - reviewed and
    /// found distinctive, not merely unmatched by anything else today.
    Strong,
}

/// One bounded, read-only signature check.
///
/// `offset` is an absolute byte offset. Reads are always exactly
/// `bytes.len()` long and never seek past what the file actually contains, so
/// the largest read any rule in this registry performs is bounded by
/// [`MAX_MAGIC_READ_BYTES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MagicRule {
    pub offset: u64,
    pub bytes: &'static [u8],
    /// Why this signature means what it means, in a person's words.
    pub description: &'static str,
    /// How confidently this specific signature identifies its platform.
    /// Reviewed per rule - see [`MagicConfidence`].
    pub confidence: MagicConfidence,
}

/// Evidence that comes from a game being a *directory of files* rather than a
/// single ROM - the shape a ScummVM or DOS game actually has on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutRule {
    /// Filenames, already lowercased. The rule matches when the directory
    /// being considered contains any one of them.
    pub any_of_files: &'static [&'static str],
    pub description: &'static str,
}

/// Everything this build knows about one platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// Stable canonical identifier. Persisted, so never renamed casually.
    pub id: &'static str,
    /// What a person sees.
    pub display_name: &'static str,
    /// Normalised folder names that identify this platform, matched exactly
    /// against one whole path component.
    pub folder_aliases: &'static [&'static str],
    /// Normalised filenames that identify this platform on their own. Used
    /// only where a filename really is diagnostic.
    pub filename_aliases: &'static [&'static str],
    /// Extensions specific enough to carry a detection alone.
    pub strong_extensions: &'static [&'static str],
    /// Extensions shared with other platforms. Never decisive.
    pub weak_extensions: &'static [&'static str],
    /// Bounded signature checks.
    pub magic: &'static [MagicRule],
    /// Multi-file layout evidence.
    pub layout: &'static [LayoutRule],
    /// Platforms this one is genuinely confusable with. Used to report
    /// candidates when evidence is shared, so ambiguity is named rather than
    /// resolved by guessing.
    pub conflicts_with: &'static [&'static str],
    /// The emulator or core a folder for this platform usually implies.
    pub preferred_emulator: Option<&'static str>,
    /// Shown to users and asserted in tests: what evidence exists for this
    /// platform and, just as importantly, what does not.
    pub explanation: &'static str,
}

impl Platform {
    /// Whether `extension` (already lowercased, no dot) is strong evidence.
    pub fn has_strong_extension(&self, extension: &str) -> bool {
        self.strong_extensions.contains(&extension)
    }

    /// Whether `extension` is shared evidence that may only narrow a result.
    pub fn has_weak_extension(&self, extension: &str) -> bool {
        self.weak_extensions.contains(&extension)
    }

    /// Whether `extension` (lowercased, no dot) is valid evidence for this
    /// platform at all - strong or weak, it makes no difference here. This
    /// is deliberately the platform registry's own answer to "is this
    /// extension plausible for this platform", so callers outside this
    /// module (e.g. gating a `DirectGameImage`'s platform assignment) never
    /// need to duplicate or re-derive `strong_extensions`/`weak_extensions`
    /// knowledge themselves.
    pub fn accepts_extension(&self, extension: &str) -> bool {
        self.has_strong_extension(extension) || self.has_weak_extension(extension)
    }

    /// How much this platform's own evidence is worth relative to others.
    /// A platform with a real signature can be confirmed outright; one with
    /// only folder evidence never claims more than the folder gives it.
    pub fn can_be_confirmed_by_signature(&self) -> bool {
        !self.magic.is_empty()
    }
}

/// The largest number of bytes any signature check in this registry reads
/// from one file. Asserted by `magic_reads_stay_within_the_documented_bound`,
/// so this constant cannot silently become a lie.
pub const MAX_MAGIC_READ_BYTES: usize = 64;

/// The furthest into a file any signature check looks. ISO 9660 system
/// identifiers live in the primary volume descriptor at 0x8008, which is the
/// deepest this milestone reaches - and it is a single bounded read at a known
/// offset, not a parse of the image.
pub const MAX_MAGIC_OFFSET: u64 = 0x8008;

/// Extensions that are shared between so many platforms that they must never
/// identify one on their own, in the order the milestone lists them.
///
/// A file whose only evidence is one of these is [`DetectionConfidence::Ambiguous`]
/// at best, with every plausible platform reported as a candidate.
pub const SHARED_EXTENSIONS: &[&str] = &[
    "bin", "cue", "iso", "img", "rom", "zip", "7z", "rar", "dsk", "tap", "cas", "adf", "chd",
    "mdf", "ccd",
];

/// Whether `extension` (lowercased, no dot) is one of the shared extensions
/// that can never identify a platform by itself.
pub fn is_shared_extension(extension: &str) -> bool {
    SHARED_EXTENSIONS.contains(&extension)
}

/// Canonical identifiers that describe the same hardware under different
/// names, or the same family under an older and a newer spelling.
///
/// Deliberately a relation rather than a rename: both identifiers stay valid
/// and stored data is never rewritten, but a comparison between them can be
/// made deliberately. See the module documentation.
pub const EQUIVALENT_PLATFORM_IDS: &[(&str, &str)] = &[
    // The same console, sold as PC Engine in Japan and TurboGrafx-16 in North
    // America. Both identifiers already exist in stored libraries.
    ("PC Engine", "TurboGrafx-16"),
    // The same computer family. `NEC PC-9801` is the identifier this build
    // shipped first; `PC-98` is the spelling real folder names use.
    ("PC-98", "NEC PC-9801"),
];

/// Every canonical identifier equivalent to `id`, excluding `id` itself.
pub fn equivalent_platform_ids(id: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = EQUIVALENT_PLATFORM_IDS
        .iter()
        .filter_map(|(left, right)| {
            if *left == id {
                Some(*right)
            } else if *right == id {
                Some(*left)
            } else {
                None
            }
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Normalises one alias, path component or hint: ASCII alphanumerics only,
/// lowercased. This is what makes spaces, hyphens, underscores, punctuation
/// and case differences all fold together.
pub fn normalize_alias(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Fixed, conservative suffixes MAME software-list DAT names carry after the
/// machine's base short name - `c128_flop`, `c128_cart`, `c128_cass`,
/// `c128_rom`, `megacd_cd`.
///
/// This is an explicit allowlist and nothing more. It is deliberately *not* a
/// generic "strip the final underscore-delimited token" rule: a suffix this
/// build does not recognise might be part of the machine's real short name
/// rather than a media-kind marker, so an unlisted suffix is left exactly as
/// it is rather than guessed at.
pub const MAME_SOFTWARE_LIST_SUFFIXES: &[&str] =
    &["_flop", "_cart", "_cass", "_rom", "_cd", "_disk"];

/// Strips one known MAME software-list suffix from `name`, if `name` ends in
/// one from [`MAME_SOFTWARE_LIST_SUFFIXES`] and something is left over.
///
/// Returns `name` unchanged when no listed suffix matches - including when a
/// suffix matches but stripping it would leave nothing (`"_cart"` alone stays
/// `"_cart"`, not empty). The returned base is a plain string slice, not yet
/// normalised: callers still run it through [`normalize_alias`] and
/// [`platform_for_alias`] like any other hint.
pub fn strip_mame_software_list_suffix(name: &str) -> &str {
    for suffix in MAME_SOFTWARE_LIST_SUFFIXES {
        if let Some(base) = name.strip_suffix(suffix)
            && !base.is_empty()
        {
            return base;
        }
    }
    name
}

/// The platform whose descriptor carries `id`, if any.
pub fn platform_by_id(id: &str) -> Option<&'static Platform> {
    PLATFORMS.iter().find(|platform| platform.id == id)
}

/// The platform one whole path component or hint names, matched exactly after
/// normalisation.
///
/// Returns `None` when nothing matches, and also when the normalised alias
/// would be claimed by more than one platform - an ambiguous alias is a
/// registry defect, and returning `None` refuses to pick a winner. The
/// `no_alias_is_claimed_by_two_platforms` test keeps that case at zero.
pub fn platform_for_alias(hint: &str) -> Option<&'static Platform> {
    let normalized = normalize_alias(hint);
    if normalized.is_empty() {
        return None;
    }
    let mut matches = PLATFORMS
        .iter()
        .filter(|platform| platform.folder_aliases.contains(&normalized.as_str()));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Every platform that lists `extension` as strong evidence.
pub fn platforms_with_strong_extension(extension: &str) -> Vec<&'static Platform> {
    PLATFORMS
        .iter()
        .filter(|platform| platform.has_strong_extension(extension))
        .collect()
}

/// Every platform that lists `extension` as shared evidence. This is what
/// populates the candidate list a person sees when a `.bin` cannot be placed.
pub fn platforms_with_weak_extension(extension: &str) -> Vec<&'static Platform> {
    PLATFORMS
        .iter()
        .filter(|platform| platform.has_weak_extension(extension))
        .collect()
}

/// Every canonical platform id that treats `extension` as valid evidence at
/// all - the union of [`platforms_with_strong_extension`] and
/// [`platforms_with_weak_extension`], sorted and deduplicated so the result
/// is stable regardless of registry iteration order.
///
/// This is a *candidate* list, never an answer: many real extensions
/// (`.bin`, `.cue`, `.d64`) are valid for a dozen platforms at once, and that
/// breadth is the whole point of returning every one of them rather than
/// picking a winner. An extension no platform in [`PLATFORMS`] declares
/// returns an empty list. `extension` may be given with or without a leading
/// dot and in any case; both are normalised the same way
/// [`extension_of`] already normalises a path's extension, so a caller never
/// needs to pre-clean its input.
pub fn platform_candidates_for_extension(extension: &str) -> Vec<&'static str> {
    let normalized = extension.trim_start_matches('.').to_ascii_lowercase();
    let mut ids: Vec<&'static str> = platforms_with_strong_extension(&normalized)
        .into_iter()
        .chain(platforms_with_weak_extension(&normalized))
        .map(|platform| platform.id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Every canonical platform id whose registered [`MagicRule`] matches `data`.
///
/// Pure: `data` is a byte slice already held in memory, indexed as if it were
/// the start of a file (`data[0]` is offset `0`). No file is opened, no path
/// is resolved, and no symlink policy is consulted - that I/O-bound,
/// bounded-read, symlink-safe variant of the same rules already exists as
/// [`detect::detect_platform_report`] for a caller that has a real path. This
/// is for a caller that already holds the bytes (or, in a test, invented
/// them) and wants the same canonical signatures applied without any I/O of
/// its own.
///
/// A rule whose offset and length would run past the end of `data` simply
/// does not match, exactly as a short real file would not match it either.
/// Multiple matching platforms are returned in full - this never picks a
/// winner - sorted and deduplicated so the result is stable regardless of
/// [`PLATFORMS`] iteration order. Bytes nothing in the registry recognises
/// return an empty list.
pub fn platform_candidates_from_bytes(data: &[u8]) -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = PLATFORMS
        .iter()
        .filter(|platform| {
            platform
                .magic
                .iter()
                .any(|rule| magic_rule_matches(rule, data))
        })
        .map(|platform| platform.id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Every canonical platform id whose registered [`MagicRule`] matches
/// `data`, paired with the reviewed [`MagicConfidence`] of the best rule that
/// matched it.
///
/// Distinct from [`platform_candidates_from_bytes`], which stays a plain,
/// confidence-free candidate list: this is for a caller that needs to know
/// how much a *specific* matching signature is actually worth, per the
/// judgement already recorded on that rule - never inferred from how many
/// platforms this buffer happened to match. A platform whose several rules
/// disagree on confidence for the same buffer (not currently possible in
/// this registry, but not structurally forbidden either) is reported at its
/// best matching rule's confidence, never its worst.
///
/// Sorted by platform id and deduplicated, so the result is stable
/// regardless of [`PLATFORMS`] iteration order. Bytes nothing in the
/// registry recognises return an empty list.
pub fn platform_magic_confidence_from_bytes(data: &[u8]) -> Vec<(&'static str, MagicConfidence)> {
    let mut matches: Vec<(&'static str, MagicConfidence)> = PLATFORMS
        .iter()
        .filter_map(|platform| {
            platform
                .magic
                .iter()
                .filter(|rule| magic_rule_matches(rule, data))
                .map(|rule| rule.confidence)
                .max()
                .map(|confidence| (platform.id, confidence))
        })
        .collect();
    matches.sort_by(|left, right| left.0.cmp(right.0));
    matches
}

/// Whether `data`, read as bytes from the start of a file, carries `rule`'s
/// signature at its offset. This is the same equality [`detect`]'s
/// bounded-read signature check performs on the exact bytes it reads at
/// `rule.offset`; here the comparison is against an in-memory slice instead
/// of a fresh bounded read, so the same [`MagicRule`] table is the only thing
/// shared between the two, never a second copy of it.
fn magic_rule_matches(rule: &MagicRule, data: &[u8]) -> bool {
    data.get(rule.offset as usize..)
        .and_then(|rest| rest.get(..rule.bytes.len()))
        == Some(rule.bytes)
}

/// The lowercased extension of `path` without its dot, if it has one.
pub fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

/// Every canonical platform identifier, sorted and deduplicated.
pub fn canonical_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = PLATFORMS.iter().map(|platform| platform.id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// The display name for a canonical identifier, falling back to the
/// identifier itself for a value this build does not know (a platform stored
/// by an older or newer build, which must still display as something).
pub fn display_name_for(id: &str) -> &str {
    platform_by_id(id).map_or(id, |platform| platform.display_name)
}

/// The registry. Sorted by display name so iteration order is stable and
/// every derived list is deterministic.
pub const PLATFORMS: &[Platform] = &[
    Platform {
        id: "3DO",
        display_name: "3DO Interactive Multiplayer",
        folder_aliases: &[
            "3do",
            "panasonic3do",
            "threedo",
            "panasonicthreedo",
            "3dointeractivemultiplayer",
        ],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "img"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Philips CD-i", "PSX"],
        preferred_emulator: None,
        explanation: "3DO discs use a non-ISO 9660 filesystem that this milestone does not parse, so identification comes from folder evidence rather than a signature.",
    },
    Platform {
        id: "Acorn Archimedes",
        display_name: "Acorn Archimedes",
        folder_aliases: &[
            "archimedes",
            "acornarchimedes",
            "riscos",
            "acornriscos",
            "archie",
        ],
        filename_aliases: &[],
        strong_extensions: &["jfd"],
        weak_extensions: &["adf", "adl", "hfe"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Amiga"],
        preferred_emulator: None,
        explanation: "RISC OS ADFS images use `.adf`, the same extension as Amiga floppies, so `.adf` alone cannot separate them.",
    },
    Platform {
        id: "Acorn Electron",
        display_name: "Acorn Electron",
        folder_aliases: &["electron", "acornelectron", "acornelk", "elk"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["uef", "ssd", "adf", "dsk"],
        magic: &[],
        layout: &[],
        conflicts_with: &["BBC Micro"],
        preferred_emulator: None,
        explanation: "The Electron shares every common file format with the BBC Micro, so it is recognised from folder evidence rather than from a file signature. No extension is treated as proof.",
    },
    Platform {
        id: "AmigaCD32",
        display_name: "Amiga CD32",
        folder_aliases: &["amigacd32", "cd32", "commodorecd32", "amigacd32cd"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "ccd", "mdf"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Amiga", "Commodore CDTV", "PSX"],
        preferred_emulator: None,
        explanation: "A CD32 title is a CD image, so it shares every disc extension with other CD systems. It stays a separate canonical platform from Amiga because its software is not interchangeable.",
    },
    Platform {
        id: "Amstrad CPC",
        display_name: "Amstrad CPC",
        folder_aliases: &[
            "amstrad",
            "amstradcpc",
            "cpc",
            "cpc464",
            "cpc6128",
            "amstradcpc464",
            "amstradcpc6128",
        ],
        filename_aliases: &[],
        strong_extensions: &["cdt"],
        weak_extensions: &["dsk", "sna", "tap", "voc"],
        magic: &[],
        layout: &[],
        conflicts_with: &["ZX Spectrum", "AtariST"],
        preferred_emulator: None,
        explanation: "`.cdt` tapes are CPC-specific. `.dsk` is shared with the BBC Micro, Atari ST, Apple II and PC-98, so a disk image alone is never enough.",
    },
    Platform {
        id: "Apple II",
        display_name: "Apple II",
        folder_aliases: &[
            "appleii",
            "apple2",
            "apple2e",
            "appleiie",
            "apple2c",
            "apple2gs",
            "appleiigs",
        ],
        filename_aliases: &[],
        strong_extensions: &["do", "po", "woz", "2mg", "nib"],
        weak_extensions: &["dsk", "img", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Macintosh"],
        preferred_emulator: None,
        explanation: "`.woz`/`.2mg`/`.nib` are Apple II disk formats. `.dsk` is shared with several other systems and is only weak evidence.",
    },
    Platform {
        id: "Macintosh",
        display_name: "Apple Macintosh",
        folder_aliases: &[
            "mac",
            "macintosh",
            "applemac",
            "applemacintosh",
            "macos",
            "macplus",
            "macse",
        ],
        filename_aliases: &[],
        strong_extensions: &["hfv", "dc42", "sit"],
        weak_extensions: &["img", "dsk", "iso", "toast", "cdr", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Apple II"],
        preferred_emulator: None,
        explanation: "Macintosh disk images mostly use generic extensions, so folder evidence identifies the platform. `.img`/`.iso` are never treated as proof.",
    },
    Platform {
        id: "Arcade",
        display_name: "Arcade",
        folder_aliases: &["arcade", "mame", "fbneo", "finalburnneo", "fba"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["zip", "7z", "chd"],
        magic: &[],
        layout: &[],
        conflicts_with: &["NeoGeo"],
        preferred_emulator: None,
        explanation: "Arcade sets are `.zip`/`.chd` files whose names are the only identification, so folder evidence carries the platform. `.zip` never identifies a platform on its own.",
    },
    Platform {
        id: "Atari2600",
        display_name: "Atari 2600",
        folder_aliases: &["atari2600", "a2600", "atarivcs"],
        filename_aliases: &[],
        strong_extensions: &["a26"],
        weak_extensions: &["bin", "rom", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Atari 8-bit"],
        preferred_emulator: None,
        explanation: "`.a26` is 2600 specific; a bare `.bin` is shared with most cartridge systems.",
    },
    Platform {
        id: "Atari5200",
        display_name: "Atari 5200",
        folder_aliases: &["atari5200", "a5200"],
        filename_aliases: &[],
        strong_extensions: &["a52"],
        weak_extensions: &["bin", "rom", "car", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Atari 8-bit"],
        preferred_emulator: None,
        explanation: "`.a52` is 5200 specific. The 5200 shares cartridge dumps with the Atari 8-bit range.",
    },
    Platform {
        id: "Atari7800",
        display_name: "Atari 7800",
        folder_aliases: &["atari7800", "a7800"],
        filename_aliases: &[],
        strong_extensions: &["a78"],
        weak_extensions: &["bin", "rom", "zip"],
        magic: &[MagicRule {
            offset: 0x1,
            bytes: b"ATARI7800",
            description: "A 7800 header names `ATARI7800` at offset 0x01",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "The `ATARI7800` header confirms a 7800 dump.",
    },
    Platform {
        id: "Atari 8-bit",
        display_name: "Atari 8-bit",
        folder_aliases: &[
            "atari8bit",
            "atari800",
            "atari8",
            "atarixl",
            "atarixe",
            "atari400",
            "atari130xe",
            "atarixegs",
        ],
        filename_aliases: &[],
        strong_extensions: &["atr", "atx", "xex", "xfd"],
        weak_extensions: &["cas", "bin", "rom", "car"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Atari5200", "AtariST"],
        preferred_emulator: None,
        explanation: "`.atr`/`.xex` are Atari 8-bit disk and executable formats. `.bin`/`.rom` are shared with the Atari 5200 cartridge range, so those stay weak.",
    },
    Platform {
        id: "Atari Jaguar",
        display_name: "Atari Jaguar",
        folder_aliases: &["atarijaguar", "jaguar", "jaguar64", "atarijag"],
        filename_aliases: &[],
        strong_extensions: &["j64", "jag"],
        weak_extensions: &["rom", "bin", "abs", "cof"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.j64`/`.jag` are Jaguar cartridge dumps. Jaguar Jaguar CD titles are disc images; this build has no canonical platform for them, so they are not claimed here.",
    },
    Platform {
        id: "Atari Lynx",
        display_name: "Atari Lynx",
        folder_aliases: &["atarilynx", "lynx", "atarilynxlynx", "lynxii"],
        filename_aliases: &[],
        strong_extensions: &["lnx", "lyx"],
        weak_extensions: &["bin", "o"],
        magic: &[MagicRule {
            offset: 0,
            bytes: b"LYNX",
            description: "Lynx cartridge images carry the `LYNX` header magic",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "The `LYNX` header confirms a Lynx dump outright; a bare `.bin` never does.",
    },
    Platform {
        id: "AtariST",
        display_name: "Atari ST",
        folder_aliases: &["atarist", "atariste", "atarifalcon", "atarittu"],
        filename_aliases: &[],
        strong_extensions: &["st", "stx", "msa", "mfm"],
        weak_extensions: &["dsk", "ipf", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Amstrad CPC", "BBC Micro"],
        preferred_emulator: None,
        explanation: "`.st` and `.stx` are validated structurally, not taken on trust: a `.st` file's FAT12 boot-sector geometry must account for its exact length, and a `.stx` file must be a consistent Pasti container. A valid Pasti container settles the platform on its own; a raw `.st` dump has the same boot sector a PC DOS floppy of that geometry would, so it reaches confirmed only alongside folder evidence. `.msa` and `.mfm` are recognised by extension only, and `.dsk` is shared with several other disk systems.",
    },
    Platform {
        id: "WonderSwan",
        display_name: "Bandai WonderSwan",
        folder_aliases: &["wonderswan", "bandaiwonderswan"],
        filename_aliases: &[],
        strong_extensions: &["ws"],
        weak_extensions: &["bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["WonderSwan Color"],
        preferred_emulator: None,
        explanation: "`.ws` is WonderSwan specific.",
    },
    Platform {
        id: "WonderSwan Color",
        display_name: "Bandai WonderSwan Color",
        folder_aliases: &["wonderswancolor", "wsc", "bandaiwonderswancolor"],
        filename_aliases: &[],
        strong_extensions: &["wsc"],
        weak_extensions: &["ws", "bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["WonderSwan"],
        preferred_emulator: None,
        explanation: "`.wsc` is WonderSwan Color specific.",
    },
    Platform {
        id: "BBC Micro",
        display_name: "BBC Micro",
        folder_aliases: &[
            "bbc",
            "bbcmicro",
            "bbcmodelb",
            "bbcb",
            "acornbbc",
            "bbcmaster",
        ],
        filename_aliases: &[],
        strong_extensions: &["ssd", "dsd"],
        weak_extensions: &["uef", "dsk", "adf", "adl"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Acorn Electron"],
        preferred_emulator: None,
        explanation: "Acorn DFS disk images (`.ssd`/`.dsd`) are characteristic, but `.uef` tapes are shared with the Acorn Electron, so tape-only evidence stays ambiguous between the two.",
    },
    Platform {
        id: "ColecoVision",
        display_name: "ColecoVision",
        folder_aliases: &["colecovision", "coleco", "colecovisioncv"],
        filename_aliases: &[],
        strong_extensions: &["col"],
        weak_extensions: &["rom", "bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.col` is ColecoVision specific. `.rom`/`.bin` are shared with too many cartridge systems to be proof.",
    },
    Platform {
        id: "Commodore 128",
        display_name: "Commodore 128",
        folder_aliases: &["commodore128", "c128", "commodorec128", "c128d"],
        filename_aliases: &[],
        strong_extensions: &["d71", "d81"],
        weak_extensions: &["d64", "g64", "prg", "crt", "tap"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Commodore 64"],
        preferred_emulator: None,
        explanation: "1571/1581 images (`.d71`/`.d81`) are C128-era formats. A plain `.d64`/`.g64` is far more often a C64 image, so both are only weak evidence here; a 1571 drive can read GCR `.g64` images too.",
    },
    Platform {
        id: "Commodore 64",
        display_name: "Commodore 64",
        folder_aliases: &["commodore64", "c64", "commodorec64", "c64gs"],
        filename_aliases: &[],
        strong_extensions: &["d64", "t64", "prg", "crt", "g64", "d81ns"],
        weak_extensions: &["tap", "cas", "bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Commodore 128", "VIC-20"],
        preferred_emulator: None,
        explanation: "`.d64`/`.t64`/`.crt` are Commodore-specific but shared across the 8-bit Commodore range, so folder evidence is what separates C64 from C128 and VIC-20.",
    },
    Platform {
        id: "Amiga",
        display_name: "Commodore Amiga",
        folder_aliases: &[
            "amiga",
            "commodoreamiga",
            "commodoreamiga500",
            "amiga500",
            "amigaocs",
            "amigaaga",
            "amigaecs",
        ],
        filename_aliases: &[],
        strong_extensions: &["adz", "ipf", "dms", "hdf", "lha"],
        weak_extensions: &["adf", "zip", "iso", "lzx"],
        magic: &[MagicRule {
            offset: 0,
            bytes: b"DOS\x00",
            description: "OFS/FFS floppy images begin with the `DOS` boot-block identifier",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &["AmigaCD32", "Acorn Archimedes", "Commodore CDTV"],
        preferred_emulator: None,
        explanation: "`.adf` is shared with Acorn ADFS, so the boot-block signature is what confirms an Amiga floppy. CD-based Amiga titles belong to CD32 or CDTV, not here.",
    },
    Platform {
        id: "Commodore CDTV",
        display_name: "Commodore CDTV",
        folder_aliases: &["cdtv", "commodorecdtv", "amigacdtv"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Amiga", "AmigaCD32"],
        preferred_emulator: None,
        explanation: "CDTV discs carry no distinctive extension and are recognised from folder evidence. Kept separate from CD32, which is different hardware.",
    },
    Platform {
        id: "VIC-20",
        display_name: "Commodore VIC-20",
        folder_aliases: &["vic20", "commodorevic20", "vic"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["prg", "crt", "tap", "cas", "d64"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Commodore 64"],
        preferred_emulator: None,
        explanation: "The VIC-20 shares `.prg`/`.crt`/`.tap` with the rest of the Commodore range and has no extension of its own, so it is recognised from folder evidence.",
    },
    Platform {
        id: "FM Towns",
        display_name: "Fujitsu FM Towns",
        folder_aliases: &["fmtowns", "fujitsufmtowns", "towns", "fmtownsmarty"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "img", "d88"],
        magic: &[],
        layout: &[],
        conflicts_with: &["PC-98", "NEC PC-9801"],
        preferred_emulator: None,
        explanation: "FM Towns titles are CD or floppy images with no distinctive extension, so folder evidence identifies them.",
    },
    Platform {
        id: "Vectrex",
        display_name: "GCE Vectrex",
        folder_aliases: &["vectrex", "gcevectrex", "gcevectrexvectrex"],
        filename_aliases: &[],
        strong_extensions: &["vec"],
        weak_extensions: &["bin", "rom", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.vec` is Vectrex specific; a bare `.bin` is shared with many systems.",
    },
    Platform {
        id: "Intellivision",
        display_name: "Mattel Intellivision",
        folder_aliases: &["intellivision", "mattelintellivision", "intv"],
        filename_aliases: &[],
        strong_extensions: &["int", "itv"],
        weak_extensions: &["rom", "bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.int`/`.itv` are Intellivision specific; `.bin` is not.",
    },
    Platform {
        id: "Xbox",
        display_name: "Microsoft Xbox",
        folder_aliases: &["xbox", "xboxoriginal", "microsoftxbox"],
        filename_aliases: &[],
        strong_extensions: &["xbe", "xiso"],
        weak_extensions: &["iso", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Xbox360"],
        preferred_emulator: None,
        explanation: "`.xbe` is an original Xbox executable; `.iso` is shared with the Xbox 360 and every other disc system.",
    },
    Platform {
        id: "Xbox360",
        display_name: "Microsoft Xbox 360",
        folder_aliases: &["xbox360", "x360", "microsoftxbox360"],
        filename_aliases: &[],
        strong_extensions: &["xex"],
        weak_extensions: &["iso", "zip", "god"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Xbox"],
        preferred_emulator: None,
        explanation: "`.xex` is an Xbox 360 executable.",
    },
    Platform {
        id: "DOS",
        display_name: "MS-DOS",
        folder_aliases: &["dos", "msdos", "dosgames", "dosbox", "pcdos", "ibmpcdos"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["exe", "com", "img", "ima", "zip", "7z"],
        magic: &[],
        layout: &[LayoutRule {
            any_of_files: &["dosbox.conf"],
            description: "A DOSBox configuration file sits alongside the game",
        }],
        conflicts_with: &["PC", "ScummVM"],
        preferred_emulator: None,
        explanation: "`.exe`/`.com` are shared with every Windows-era release, so DOS is identified from folder evidence or a DOSBox configuration rather than from an extension.",
    },
    Platform {
        id: "MSX",
        display_name: "MSX",
        folder_aliases: &["msx", "msx1", "microsoftmsx", "msxone"],
        filename_aliases: &[],
        strong_extensions: &["mx1"],
        weak_extensions: &["rom", "dsk", "cas", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["MSX2"],
        preferred_emulator: None,
        explanation: "`.mx1` is MSX1 specific. `.rom`/`.dsk`/`.cas` are shared across the whole MSX range and with other systems.",
    },
    Platform {
        id: "MSX2",
        display_name: "MSX2",
        folder_aliases: &["msx2", "microsoftmsx2", "msx2plus", "msxtwo"],
        filename_aliases: &[],
        strong_extensions: &["mx2"],
        weak_extensions: &["rom", "dsk", "cas", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["MSX"],
        preferred_emulator: None,
        explanation: "`.mx2` is MSX2 specific; every other MSX format is shared with MSX1.",
    },
    Platform {
        id: "NEC PC-8801",
        display_name: "NEC PC-8801",
        folder_aliases: &["necpc8801", "pc8801", "pc88", "necpc88"],
        filename_aliases: &[],
        strong_extensions: &["d88"],
        weak_extensions: &["dsk", "cmt", "t88"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.d88` is shared with the PC-98 range, so folder evidence separates PC-8801 from PC-98.",
    },
    Platform {
        id: "PC-98",
        display_name: "NEC PC-98",
        folder_aliases: &["pc98", "necpc98", "pc9800", "pc9800series", "necpc9800"],
        filename_aliases: &[],
        strong_extensions: &["fdi", "d88", "hdi", "nhd", "thd"],
        weak_extensions: &["dsk", "fdd", "hdm", "img", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["NEC PC-9801", "FM Towns", "Sharp X68000"],
        preferred_emulator: None,
        explanation: "`.fdi`/`.d88`/`.hdi` are PC-98 disk formats. This canonical identifier covers folders named `pc98`; the older `NEC PC-9801` identifier is preserved separately so stored data is never rewritten.",
    },
    Platform {
        id: "NEC PC-9801",
        display_name: "NEC PC-9801",
        folder_aliases: &["necpc9801", "pc9801"],
        filename_aliases: &[],
        strong_extensions: &["fdi", "d88", "hdi"],
        weak_extensions: &["dsk", "fdd", "img"],
        magic: &[],
        layout: &[],
        conflicts_with: &["PC-98"],
        preferred_emulator: None,
        explanation: "Retained unchanged because existing libraries already store this identifier. `PC-98` is the equivalent modern spelling; the two are mapped to each other for comparison but never silently merged.",
    },
    Platform {
        id: "NeoGeo",
        display_name: "Neo Geo",
        folder_aliases: &[
            "neogeo",
            "neogeoaes",
            "neogeomvs",
            "snkneogeo",
            "neogeoarcade",
        ],
        filename_aliases: &[],
        strong_extensions: &["neo"],
        weak_extensions: &["zip", "7z", "chd"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Neo Geo CD", "Arcade"],
        preferred_emulator: None,
        explanation: "Neo Geo cartridge sets are MAME-style `.zip` archives, which prove nothing by themselves, so folder evidence carries the identification.",
    },
    Platform {
        id: "NeoGeo64",
        display_name: "Neo Geo 64",
        folder_aliases: &["neogeo64"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["zip", "bin"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "Recognised from folder evidence only; its dumps use generic containers.",
    },
    Platform {
        id: "Neo Geo CD",
        display_name: "Neo Geo CD",
        folder_aliases: &["neogeocd", "snkneogeocd", "ngcd", "neocd", "neocdz"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "img"],
        magic: &[],
        layout: &[],
        conflicts_with: &["NeoGeo", "Sega CD"],
        preferred_emulator: None,
        explanation: "Neo Geo CD discs share every extension with other CD systems. Kept separate from cartridge Neo Geo, whose software is not interchangeable.",
    },
    Platform {
        id: "Neo Geo Pocket",
        display_name: "Neo Geo Pocket",
        folder_aliases: &["neogeopocket", "ngp", "snkneogeopocket"],
        filename_aliases: &[],
        strong_extensions: &["ngp"],
        weak_extensions: &["bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Neo Geo Pocket Color"],
        preferred_emulator: None,
        explanation: "`.ngp` is Neo Geo Pocket specific.",
    },
    Platform {
        id: "Neo Geo Pocket Color",
        display_name: "Neo Geo Pocket Color",
        folder_aliases: &["neogeopocketcolor", "ngpc", "snkneogeopocketcolor"],
        filename_aliases: &[],
        strong_extensions: &["ngc"],
        weak_extensions: &["ngp", "bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Neo Geo Pocket"],
        preferred_emulator: None,
        explanation: "`.ngc` is Neo Geo Pocket Color specific.",
    },
    Platform {
        id: "Nintendo 3DS",
        display_name: "Nintendo 3DS",
        folder_aliases: &["nintendo3ds", "n3ds", "new3ds"],
        filename_aliases: &[],
        strong_extensions: &["3ds", "cia", "cci", "cxi"],
        weak_extensions: &["zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.3ds`/`.cia` are 3DS specific.",
    },
    Platform {
        id: "N64",
        display_name: "Nintendo 64",
        folder_aliases: &["n64", "nintendo64", "nintendonintendo64"],
        filename_aliases: &[],
        strong_extensions: &["z64", "n64", "v64", "ndd"],
        weak_extensions: &["bin", "rom", "zip"],
        magic: &[MagicRule {
            offset: 0,
            bytes: b"\x80\x37\x12\x40",
            description: "A big-endian Nintendo 64 ROM begins with 0x80371240",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "The byte-order signature confirms a big-endian N64 dump.",
    },
    Platform {
        id: "Nintendo DS",
        display_name: "Nintendo DS",
        folder_aliases: &[
            "nintendods",
            "nds",
            "ds",
            "nintendonintendods",
            "nintendonintendodsi",
        ],
        filename_aliases: &[],
        strong_extensions: &["nds", "dsi"],
        weak_extensions: &["bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.nds` is Nintendo DS specific.",
    },
    Platform {
        id: "NES",
        display_name: "Nintendo Entertainment System",
        folder_aliases: &[
            "nes",
            "nintendoentertainmentsystem",
            "famicom",
            "nintendofamicom",
            "nintendonintendoentertainmentsystem",
            "nintendofamilycomputerdisksystem",
        ],
        filename_aliases: &[],
        strong_extensions: &["nes", "fds", "unf"],
        weak_extensions: &["bin", "rom", "zip"],
        magic: &[MagicRule {
            offset: 0,
            bytes: b"NES\x1a",
            description: "An iNES ROM begins with the `NES` magic",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "The iNES header confirms a NES ROM outright.",
    },
    Platform {
        id: "Game Boy",
        display_name: "Nintendo Game Boy",
        folder_aliases: &["gameboy", "gb", "nintendogameboy"],
        filename_aliases: &[],
        strong_extensions: &["gb"],
        weak_extensions: &["bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Game Boy Color"],
        preferred_emulator: None,
        explanation: "`.gb` is Game Boy specific, though Game Boy Color titles are backwards compatible and sometimes share it.",
    },
    Platform {
        id: "Game Boy Advance",
        display_name: "Nintendo Game Boy Advance",
        folder_aliases: &["gameboyadvance", "gba", "nintendogameboyadvance"],
        filename_aliases: &[],
        strong_extensions: &["gba"],
        weak_extensions: &["bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.gba` is Game Boy Advance specific.",
    },
    Platform {
        id: "Game Boy Color",
        display_name: "Nintendo Game Boy Color",
        folder_aliases: &["gameboycolor", "gbc", "nintendogameboycolor"],
        filename_aliases: &[],
        strong_extensions: &["gbc"],
        weak_extensions: &["gb", "bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["Game Boy"],
        preferred_emulator: None,
        explanation: "`.gbc` is Game Boy Color specific; a `.gb` file may be either machine.",
    },
    Platform {
        id: "GameCube",
        display_name: "Nintendo GameCube",
        folder_aliases: &["gamecube", "nintendogamecube", "gcn", "gc", "ngc"],
        filename_aliases: &[],
        strong_extensions: &["gcm", "gcz", "rvz"],
        weak_extensions: &["iso", "ciso", "zip"],
        magic: &[MagicRule {
            offset: 0x1c,
            bytes: b"\xc2\x33\x9f\x3d",
            description: "A GameCube disc carries the 0xC2339F3D magic word at offset 0x1C",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &["Wii"],
        preferred_emulator: None,
        explanation: "The disc magic word is what separates a GameCube `.iso` from a Wii one. Preserved exactly as the existing header check behaved.",
    },
    Platform {
        id: "Switch",
        display_name: "Nintendo Switch",
        folder_aliases: &["switch", "nintendoswitch"],
        filename_aliases: &[],
        strong_extensions: &["xci", "nsp", "nca"],
        weak_extensions: &["zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.xci`/`.nsp` are Switch specific.",
    },
    Platform {
        id: "Virtual Boy",
        display_name: "Nintendo Virtual Boy",
        folder_aliases: &["virtualboy", "vb", "nintendovirtualboy"],
        filename_aliases: &[],
        strong_extensions: &["vb", "vboy"],
        weak_extensions: &["bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.vb` is Virtual Boy specific.",
    },
    Platform {
        id: "Wii",
        display_name: "Nintendo Wii",
        folder_aliases: &["wii", "nintendowii"],
        filename_aliases: &[],
        strong_extensions: &["wbfs", "wad"],
        weak_extensions: &["iso", "gcz", "rvz", "ciso", "wia", "zip"],
        magic: &[MagicRule {
            offset: 0x18,
            bytes: b"\x5d\x1c\x9e\xa3",
            description: "A Wii disc carries the 0x5D1C9EA3 magic word at offset 0x18",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &["GameCube"],
        preferred_emulator: None,
        explanation: "The disc magic word is what separates a Wii `.iso` from a GameCube one. Preserved exactly as the existing header check behaved.",
    },
    Platform {
        id: "WiiU",
        display_name: "Nintendo Wii U",
        folder_aliases: &["wiiu", "nintendowiiu"],
        filename_aliases: &[],
        strong_extensions: &["wud", "wux", "rpx"],
        weak_extensions: &["iso", "wad", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.wud`/`.wux` are Wii U disc formats.",
    },
    Platform {
        id: "NGage",
        display_name: "Nokia N-Gage",
        folder_aliases: &["ngage", "nokiangage"],
        filename_aliases: &[],
        strong_extensions: &["n-gage", "sis"],
        weak_extensions: &["zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.sis` is a Symbian installer used by N-Gage titles.",
    },
    Platform {
        id: "PC",
        display_name: "PC",
        folder_aliases: &["pc", "pcgames", "windows", "windowsgames"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["exe", "msi", "iso", "zip", "7z", "rar"],
        magic: &[],
        layout: &[],
        conflicts_with: &["DOS"],
        preferred_emulator: None,
        explanation: "PC releases use entirely generic containers, so only folder evidence identifies them.",
    },
    Platform {
        id: "PC Engine",
        display_name: "PC Engine / TurboGrafx-16",
        folder_aliases: &[
            "pcengine",
            "pce",
            "necpcenginesupergrafx",
            "necpcengineturbografx16",
            "necpcengine",
            "pcenginesupergrafx",
            "supergrafx",
            "necsupergrafx",
        ],
        filename_aliases: &[],
        strong_extensions: &["pce", "sgx"],
        weak_extensions: &["bin", "zip", "chd"],
        magic: &[],
        layout: &[],
        conflicts_with: &["PC Engine CD", "TurboGrafx-16"],
        preferred_emulator: None,
        explanation: "`.pce` is PC Engine specific. CD titles belong to PC Engine CD, which stays a separate canonical platform.",
    },
    Platform {
        id: "PC Engine CD",
        display_name: "PC Engine CD / TurboGrafx-CD",
        folder_aliases: &[
            "pcenginecd",
            "turbografxcd",
            "tgcd",
            "necpcenginecd",
            "pcecd",
            "turbografxcdrom",
            "necturbografxcd",
        ],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "img"],
        magic: &[],
        layout: &[],
        conflicts_with: &["PC Engine", "Sega CD", "PSX"],
        preferred_emulator: None,
        explanation: "A CD-ROM² title shares every disc extension with other CD systems, so folder or cue-sheet context is what identifies it. Never merged with cartridge PC Engine.",
    },
    Platform {
        id: "Philips CD-i",
        display_name: "Philips CD-i",
        folder_aliases: &[
            "cdi",
            "philipscdi",
            "philipscd",
            "cdinteractive",
            "philipscdinteractive",
        ],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "img", "mdf", "ccd"],
        magic: &[MagicRule {
            offset: 0x8008,
            bytes: b"CD-RTOS",
            description: "A CD-i disc names `CD-RTOS` as the ISO 9660 system identifier at offset 0x8008",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &["PSX", "Sega CD", "3DO", "PC Engine CD"],
        preferred_emulator: None,
        explanation: "The ISO 9660 system identifier is what separates a CD-i disc from any other ISO: CD-i names `CD-RTOS`, where a PlayStation disc names `PLAYSTATION`. Extension alone is never enough.",
    },
    Platform {
        id: "ScummVM",
        display_name: "ScummVM",
        folder_aliases: &["scummvm", "scumm", "scummvmgames", "sci", "sierrasci"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &[
            "gen", "map", "000", "aud", "cfg", "scr", "hep", "sog", "la0", "lfl", "he0", "she",
            "zip",
        ],
        magic: &[],
        layout: &[
            LayoutRule {
                any_of_files: &["resource.map"],
                description: "A Sierra SCI resource map sits in the game directory",
            },
            LayoutRule {
                any_of_files: &["resource.000"],
                description: "A Sierra SCI resource volume sits in the game directory",
            },
            LayoutRule {
                any_of_files: &["resource.cfg"],
                description: "A Sierra SCI resource configuration sits in the game directory",
            },
            LayoutRule {
                any_of_files: &["000.lfl"],
                description: "A SCUMM index file sits in the game directory",
            },
            LayoutRule {
                any_of_files: &["scummvm.ini"],
                description: "A ScummVM configuration file sits in the game directory",
            },
        ],
        conflicts_with: &["MegaDrive", "DOS"],
        preferred_emulator: None,
        explanation: "A ScummVM game is a directory of engine resource files, not a ROM. Its files use extensions that collide with cartridge formats - RESOURCE.GEN against Mega Drive `.gen` most notably - so ScummVM is identified by directory layout and filename, and its resource files must never be reclassified from their extension alone.",
    },
    Platform {
        id: "Sega 32X",
        display_name: "Sega 32X",
        folder_aliases: &[
            "sega32x",
            "32x",
            "sega32x32x",
            "sega32xgenesis",
            "mega32x",
            "genesis32x",
        ],
        filename_aliases: &[],
        strong_extensions: &["32x"],
        weak_extensions: &["bin", "md", "smd", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["MegaDrive", "Sega CD"],
        preferred_emulator: None,
        explanation: "A 32X cartridge carries the same `SEGA` header as a Mega Drive one, so the header cannot separate them; folder evidence does.",
    },
    Platform {
        id: "Dreamcast",
        display_name: "Sega Dreamcast",
        folder_aliases: &["dreamcast", "segadreamcast"],
        filename_aliases: &[],
        strong_extensions: &["gdi", "cdi"],
        weak_extensions: &["iso", "cue", "bin", "chd", "mdf"],
        magic: &[MagicRule {
            offset: 0,
            bytes: b"SEGA SEGAKATANA",
            description: "A Dreamcast disc boot sector begins with `SEGA SEGAKATANA`",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &["Saturn", "Philips CD-i"],
        preferred_emulator: None,
        explanation: "The boot-sector signature confirms a Dreamcast disc. Note that `.cdi` here is the DiscJuggler image format, not Philips CD-i, which is a different platform entirely.",
    },
    Platform {
        id: "GameGear",
        display_name: "Sega Game Gear",
        folder_aliases: &["gamegear", "segagamegear", "segagg", "gg"],
        filename_aliases: &[],
        strong_extensions: &["gg"],
        weak_extensions: &["bin", "rom", "zip"],
        magic: &[MagicRule {
            offset: 0x7ff0,
            bytes: b"TMR SEGA",
            description: "The Game Gear shares the `TMR SEGA` 8-bit Sega ROM header",
            confidence: MagicConfidence::Corroborated,
        }],
        layout: &[],
        conflicts_with: &["MasterSystem"],
        preferred_emulator: None,
        explanation: "Shares the `TMR SEGA` header with the Master System, so `.gg` or folder evidence is what distinguishes it.",
    },
    Platform {
        id: "MasterSystem",
        display_name: "Sega Master System",
        folder_aliases: &[
            "mastersystem",
            "segamastersystem",
            "sms",
            "segamastersystemmarkiii",
            "segasms",
            "mastersystemmarkiii",
            "segamarkiii",
        ],
        filename_aliases: &[],
        strong_extensions: &["sms"],
        weak_extensions: &["bin", "rom", "zip"],
        magic: &[
            MagicRule {
                offset: 0x7ff0,
                bytes: b"TMR SEGA",
                description: "The `TMR SEGA` ROM header appears at 0x7FF0 in most Master System dumps",
                confidence: MagicConfidence::Corroborated,
            },
            MagicRule {
                offset: 0x3ff0,
                bytes: b"TMR SEGA",
                description: "Smaller dumps carry the same header at 0x3FF0",
                confidence: MagicConfidence::Corroborated,
            },
            MagicRule {
                offset: 0x1ff0,
                bytes: b"TMR SEGA",
                description: "The smallest dumps carry it at 0x1FF0",
                confidence: MagicConfidence::Corroborated,
            },
        ],
        layout: &[],
        conflicts_with: &["GameGear"],
        preferred_emulator: None,
        explanation: "`TMR SEGA` confirms an 8-bit Sega ROM but is shared with the Game Gear, so the header alone leaves the two ambiguous and `.sms`/folder evidence decides.",
    },
    Platform {
        id: "MegaDrive",
        display_name: "Sega Mega Drive / Genesis",
        folder_aliases: &[
            "megadrive",
            "genesis",
            "segamegadrive",
            "segagenesis",
            "segamegadrivegenesis",
            "smd",
            "segamd",
            "megadrivegenesis",
            "md",
        ],
        filename_aliases: &[],
        strong_extensions: &["smd", "68k"],
        weak_extensions: &["md", "bin", "gen", "zip", "chd"],
        magic: &[MagicRule {
            offset: 0x100,
            bytes: b"SEGA",
            description: "A Mega Drive cartridge header begins with `SEGA` at offset 0x100",
            confidence: MagicConfidence::Corroborated,
        }],
        layout: &[],
        conflicts_with: &["Sega CD", "Sega 32X", "ScummVM"],
        preferred_emulator: None,
        explanation: "The cartridge header at 0x100 is the only proof. `.gen` and `.bin` are explicitly weak: `.gen` also names ScummVM RESOURCE.GEN resource files, and `.bin` is shared with almost every CD and cartridge system.",
    },
    Platform {
        id: "Sega CD",
        display_name: "Sega Mega-CD / Sega CD",
        folder_aliases: &[
            "segacd",
            "megacd",
            "segamegacdsegacd",
            "segamegacd",
            "megacdsegacd",
            "megasegacd",
            "scd",
        ],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "ccd", "mdf", "img"],
        // Both `SEGADISCSYSTEM` rules are Corroborated for canonical
        // platform identity: this review did not establish enough evidence
        // that the signature is unique across every related Sega CD /
        // 32X-CD-compatible case. It may be strong family/media evidence
        // once a dedicated media/container model exists; it must not decide
        // canonical Sega CD platform identity by itself yet.
        magic: &[
            MagicRule {
                offset: 0,
                bytes: b"SEGADISCSYSTEM",
                description: "A Mega-CD boot sector begins with `SEGADISCSYSTEM`",
                confidence: MagicConfidence::Corroborated,
            },
            MagicRule {
                offset: 0x10,
                bytes: b"SEGADISCSYSTEM",
                description: "ISO-2048 Mega-CD dumps carry the same signature at offset 0x10",
                confidence: MagicConfidence::Corroborated,
            },
        ],
        layout: &[],
        conflicts_with: &["MegaDrive", "Sega 32X", "PSX", "AmigaCD32"],
        preferred_emulator: None,
        explanation: "The `SEGADISCSYSTEM` boot signature is what separates a Mega-CD image from any other `.bin`/`.iso`. Kept as its own canonical platform, never folded into Mega Drive.",
    },
    Platform {
        id: "Saturn",
        display_name: "Sega Saturn",
        folder_aliases: &["saturn", "segasaturn"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "chd", "mdf", "ccd", "img"],
        magic: &[MagicRule {
            offset: 0,
            bytes: b"SEGA SEGASATURN",
            description: "A Saturn disc boot sector begins with `SEGA SEGASATURN`",
            confidence: MagicConfidence::Strong,
        }],
        layout: &[],
        conflicts_with: &["Sega CD", "Dreamcast"],
        preferred_emulator: None,
        explanation: "The boot-sector signature confirms a Saturn disc, which no disc extension can.",
    },
    Platform {
        id: "Sharp X68000",
        display_name: "Sharp X68000",
        folder_aliases: &[
            "sharpx68000",
            "x68000",
            "x68k",
            "sharpx68k",
            "x68000compact",
        ],
        filename_aliases: &[],
        strong_extensions: &["xdf", "dim", "d88x"],
        weak_extensions: &["dsk", "hdf", "hdm", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["NEC PC-9801"],
        preferred_emulator: None,
        explanation: "`.xdf`/`.dim` are X68000 disk formats; `.dsk` and `.hdf` are shared with other Japanese computers.",
    },
    Platform {
        id: "PSX",
        display_name: "Sony PlayStation",
        folder_aliases: &[
            "psx",
            "ps1",
            "playstation",
            "playstation1",
            "sonyplaystation",
            "sonyplaystation1",
        ],
        filename_aliases: &[],
        strong_extensions: &["pbp", "ecm"],
        weak_extensions: &["iso", "cue", "bin", "img", "chd", "mdf", "ccd", "zip"],
        magic: &[MagicRule {
            offset: 0x8008,
            bytes: b"PLAYSTATION",
            description: "A PlayStation disc names `PLAYSTATION` as the ISO 9660 system identifier at offset 0x8008",
            confidence: MagicConfidence::Corroborated,
        }],
        layout: &[],
        conflicts_with: &["PS2", "Sega CD", "Philips CD-i", "3DO", "PC Engine CD"],
        preferred_emulator: None,
        explanation: "The ISO 9660 system identifier confirms a PlayStation disc. `.iso`/`.bin` alone never do - the same extensions cover Sega CD, CD-i, 3DO and PC Engine CD.",
    },
    Platform {
        id: "PS2",
        display_name: "Sony PlayStation 2",
        folder_aliases: &["ps2", "playstation2", "sonyplaystation2"],
        filename_aliases: &[],
        strong_extensions: &[],
        weak_extensions: &["iso", "cue", "bin", "img", "chd", "mdf", "zip"],
        magic: &[MagicRule {
            offset: 0x8008,
            bytes: b"PLAYSTATION",
            description: "A PlayStation 2 disc also names `PLAYSTATION` as the ISO 9660 system identifier",
            confidence: MagicConfidence::Corroborated,
        }],
        layout: &[],
        conflicts_with: &["PSX"],
        preferred_emulator: None,
        explanation: "Shares the `PLAYSTATION` system identifier with the original PlayStation, so the signature confirms the family but folder evidence separates the generations.",
    },
    Platform {
        id: "PS3",
        display_name: "Sony PlayStation 3",
        folder_aliases: &["ps3", "playstation3", "sonyplaystation3"],
        filename_aliases: &[],
        strong_extensions: &["pkg"],
        weak_extensions: &["iso", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.pkg` is a PS3 package format.",
    },
    Platform {
        id: "PSP",
        display_name: "Sony PlayStation Portable",
        folder_aliases: &[
            "psp",
            "playstationportable",
            "sonypsp",
            "sonyplaystationportable",
        ],
        filename_aliases: &[],
        strong_extensions: &["cso", "pbp"],
        weak_extensions: &["iso", "chd", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.cso` is a PSP-compressed image format; `.iso` is shared with every other disc system.",
    },
    Platform {
        id: "PlayStation Vita",
        display_name: "Sony PlayStation Vita",
        folder_aliases: &["playstationvita", "psvita", "vita"],
        filename_aliases: &[],
        strong_extensions: &["vpk"],
        weak_extensions: &["zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.vpk` is a Vita package format.",
    },
    Platform {
        id: "SNES",
        display_name: "Super Nintendo Entertainment System",
        folder_aliases: &[
            "snes",
            "supernintendo",
            "supernintendoentertainmentsystem",
            "nintendosupernintendoentertainmentsystem",
            "superfamicom",
        ],
        filename_aliases: &[],
        strong_extensions: &["sfc", "smc"],
        weak_extensions: &["bin", "rom", "zip", "fig", "swc"],
        magic: &[],
        layout: &[],
        conflicts_with: &[],
        preferred_emulator: None,
        explanation: "`.sfc`/`.smc` are SNES specific. The SNES has no header at a fixed offset, so no magic-byte rule is claimed.",
    },
    Platform {
        id: "TurboGrafx-16",
        display_name: "TurboGrafx-16",
        folder_aliases: &["turbografx16", "tg16", "necturbografx16", "turbografx"],
        filename_aliases: &[],
        strong_extensions: &["pce"],
        weak_extensions: &["bin", "zip"],
        magic: &[],
        layout: &[],
        conflicts_with: &["PC Engine"],
        preferred_emulator: None,
        explanation: "Retained as its own canonical identifier because existing libraries already store it separately from `PC Engine`; the two are the same hardware under different regional names.",
    },
    Platform {
        id: "ZX Spectrum",
        display_name: "ZX Spectrum",
        folder_aliases: &[
            "zxspectrum",
            "spectrum",
            "zxs",
            "sinclairzxspectrum",
            "speccy",
            "sinclairspectrum",
            "zxspectrum48k",
            "zxspectrum128k",
        ],
        filename_aliases: &[],
        strong_extensions: &["z80", "sna", "szx", "tzx", "scl", "trd"],
        weak_extensions: &["tap", "dsk", "zip"],
        magic: &[MagicRule {
            offset: 0,
            bytes: b"ZXTape!\x1a",
            description: "`.tzx` tape images begin with the literal `ZXTape!` signature",
            // TZX/CDT is a tape-container format used by more than one
            // platform family (ZX Spectrum and Amstrad CPC contexts both
            // appear in real TZX/CDT usage). This signature strongly proves
            // "TZX tape container", not uniquely "ZX Spectrum platform" -
            // that distinction belongs to a future media/container evidence
            // model, not to canonical platform identity.
            confidence: MagicConfidence::Corroborated,
        }],
        layout: &[],
        conflicts_with: &["Commodore 64", "Amstrad CPC"],
        preferred_emulator: None,
        explanation: "Tape and snapshot formats are distinctive, but `.tap` is shared with Commodore tape images and `.dsk` with several disk systems.",
    },
];
