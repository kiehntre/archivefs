//! Single source of truth for which file extensions EmuWiz recognises as
//! library media, and how each is persisted as an [`ArchiveKind`].
//!
//! Before this module existed, "which extensions does EmuWiz support" was
//! answered independently in at least two places - [`crate::archive_kind`]
//! (directory scanning) and the filesystem watcher's own extension list -
//! and the two had already drifted apart (the watcher never learned about
//! `.gcz`/`.rvz`/`.wbfs`/`.ciso`). This module is now the only place that
//! answer is written down; scanning, rescanning, and watching all consult
//! it, so a new format is added in one place and every caller sees it.
//!
//! # What this is not
//!
//! This registry never assigns a platform. It answers "does EmuWiz
//! recognise this extension as media, and if so how is it persisted" -
//! nothing about *which game system* a file belongs to. Extension-based
//! platform evidence (strong/weak extensions per platform, e.g. `.d64`
//! being weak evidence for both Commodore 64 and Commodore 128) stays
//! entirely in [`crate::platform::PLATFORMS`]. A file can be recognised
//! here and still end up with no platform, or an ambiguous one - that is
//! the platform registry's decision to make, not this one's.
//!
//! It also never decides grouping (multi-disc `.cue`/`.bin` pairs,
//! `.m3u` playlists) - every entry here is a single independent file.
//!
//! # Scope: this is v1
//!
//! This is deliberately the v1 centralized media-extension/[`ArchiveKind`]
//! compatibility registry, and nothing more. It intentionally does not yet
//! model a richer `MediaKind`, a `StandalonePolicy`, or a `CatalogPolicy` -
//! those concepts (multi-file disc images, grouping/merge rules, per-format
//! cataloguing behaviour beyond "recognised or not") are planned for a
//! later descriptor/playlist/grouping slice, not this one. In particular,
//! CUE/BIN pairing, M3U playlists, and GDI multi-track sets are not
//! implemented here - every [`MediaFormat`] entry is still exactly one
//! independent file mapped to one [`ArchiveKind`], and that stays true
//! until that later slice deliberately changes it.

use crate::ArchiveKind;

/// One file extension EmuWiz recognises as library media on its own,
/// without needing corroboration from folder, source-root, or header
/// evidence, and how it is persisted for backward compatibility.
#[derive(Debug, Clone, Copy)]
pub struct MediaFormat {
    /// Lowercase, no leading dot.
    pub extension: &'static str,
    /// The persisted [`ArchiveKind`] this extension maps to. Multiple
    /// extensions may share a kind (every direct-image format persists as
    /// `ArchiveKind::DirectGameImage`) - `ArchiveKind` is a small,
    /// backward-compatible projection of a much larger set of recognised
    /// formats, never a one-to-one encoding of them.
    pub kind: ArchiveKind,
}

/// The whole media registry. Adding a new self-evidencing format is a
/// one-line addition here; nothing else needs to change for it to be
/// discovered by scanning, rescanning, and watching alike.
pub const MEDIA_FORMATS: &[MediaFormat] = &[
    MediaFormat {
        extension: "zip",
        kind: ArchiveKind::Zip,
    },
    MediaFormat {
        extension: "7z",
        kind: ArchiveKind::SevenZip,
    },
    MediaFormat {
        extension: "rar",
        kind: ArchiveKind::Rar,
    },
    // `.smd` (Super Magic Drive dump) is Mega Drive specific and needs no
    // corroboration, unlike `.md`/`.bin`/`.gen` - see
    // `crate::archive_kind_in_root` for the extensions that still require
    // folder/source/header corroboration before they resolve at all.
    MediaFormat {
        extension: "smd",
        kind: ArchiveKind::MegaDriveRom,
    },
    MediaFormat {
        extension: "iso",
        kind: ArchiveKind::DirectGameImage,
    },
    MediaFormat {
        extension: "gcm",
        kind: ArchiveKind::DirectGameImage,
    },
    MediaFormat {
        extension: "gcz",
        kind: ArchiveKind::DirectGameImage,
    },
    MediaFormat {
        extension: "rvz",
        kind: ArchiveKind::DirectGameImage,
    },
    MediaFormat {
        extension: "wbfs",
        kind: ArchiveKind::DirectGameImage,
    },
    MediaFormat {
        extension: "ciso",
        kind: ArchiveKind::DirectGameImage,
    },
    // Loose Commodore floppy-disk images. Neither is a container format
    // EmuWiz can mount or unwrap - catalogued directly, like every other
    // `DirectGameImage` entry. Which Commodore platform a given file
    // belongs to is entirely the platform registry's decision (`.d64`/
    // `.g64` are shared, weak evidence there) - this registry only
    // recognises the file as media at all.
    MediaFormat {
        extension: "d64",
        kind: ArchiveKind::DirectGameImage,
    },
    MediaFormat {
        extension: "g64",
        kind: ArchiveKind::DirectGameImage,
    },
    // CHD (MAME Compressed Hunks of Data). Shared by many disc-based
    // platforms (Neo Geo CD, Sega CD, arcade sets, redump CD/DVD sets...);
    // resolving *which* platform a `.chd` belongs to is, again, the
    // platform registry's job, driven by folder/source evidence - `.chd`
    // is deliberately never strong extension evidence for any platform.
    MediaFormat {
        extension: "chd",
        kind: ArchiveKind::DirectGameImage,
    },
];

/// Extensions that never resolve to an [`ArchiveKind`] on their own - they
/// need folder, source-root, or cartridge-header corroboration first (see
/// `crate::archive_kind_in_root`) - but that a filesystem watcher should
/// still treat as worth a rescan, since a rescan is what re-evaluates that
/// corroboration. Recognising a corroboration candidate here is never a
/// claim that the file *is* library media, only that it might become media
/// once evidence elsewhere confirms it.
const CORROBORATION_CANDIDATE_EXTENSIONS: &[&str] = &["md", "bin", "gen"];

/// The persisted [`ArchiveKind`] for `extension` (lowercase, no dot), if
/// this registry recognises it as media on its own.
pub fn kind_for_extension(extension: &str) -> Option<ArchiveKind> {
    MEDIA_FORMATS
        .iter()
        .find(|format| format.extension == extension)
        .map(|format| format.kind)
}

/// Whether `extension` (lowercase, no dot) is recognised as media on its
/// own, without corroboration.
pub fn is_recognized_extension(extension: &str) -> bool {
    kind_for_extension(extension).is_some()
}

/// Whether `extension` (lowercase, no dot) is a Mega Drive corroboration
/// candidate - see [`CORROBORATION_CANDIDATE_EXTENSIONS`]. This is the one
/// authoritative list; `crate::archive_kind_in_root` must consult it rather
/// than hardcoding its own copy of `"md" | "bin" | "gen"`.
pub fn is_corroboration_candidate(extension: &str) -> bool {
    CORROBORATION_CANDIDATE_EXTENSIONS.contains(&extension)
}

/// Whether a filesystem-watcher event on a file with `extension` (lowercase,
/// no dot) is worth a rescan: either the extension is recognised outright,
/// or it is a corroboration candidate whose eventual kind depends on
/// evidence a rescan re-evaluates. This is the single source of truth the
/// watcher consults - it must never maintain its own extension list.
pub fn is_watch_relevant_extension(extension: &str) -> bool {
    is_recognized_extension(extension) || is_corroboration_candidate(extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_extension_is_lowercase_without_a_dot() {
        for format in MEDIA_FORMATS {
            assert!(!format.extension.starts_with('.'), "{}", format.extension);
            assert_eq!(format.extension, format.extension.to_ascii_lowercase());
        }
    }

    #[test]
    fn no_extension_is_registered_twice() {
        let mut seen = std::collections::HashSet::new();
        for format in MEDIA_FORMATS {
            assert!(
                seen.insert(format.extension),
                "`{}` is registered more than once",
                format.extension
            );
        }
    }

    #[test]
    fn watch_relevant_extensions_include_every_registered_extension() {
        for format in MEDIA_FORMATS {
            assert!(is_watch_relevant_extension(format.extension));
        }
    }

    #[test]
    fn watch_relevant_extensions_include_corroboration_candidates() {
        for extension in CORROBORATION_CANDIDATE_EXTENSIONS {
            assert!(is_watch_relevant_extension(extension));
        }
    }

    #[test]
    fn an_unrecognised_extension_is_neither_a_kind_nor_watch_relevant() {
        assert_eq!(kind_for_extension("nfo"), None);
        assert!(!is_watch_relevant_extension("nfo"));
    }
}
