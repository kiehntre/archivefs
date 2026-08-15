//! Provider-neutral DAT catalogue models.
//!
//! A DAT file is a catalogue of known ROM dumps, published by preservation
//! communities. This module defines the shape those catalogues take, regardless
//! of whether they arrived as Logiqx XML (No-Intro, Redump) or ClrMamePro text
//! (TOSEC, generic).
//!
//! Every field is deliberately provider-agnostic: a local DAT catalogue fills the
//! same shape as a RomM server, so adding one later means writing an adapter rather
//! than reshaping the model.

use serde::{Deserialize, Serialize};

use super::classification::{DatContentClassification, DatOriginalMetadata};

/// Which ecosystem a DAT file represents.
///
/// Detection is best-effort, from metadata and naming conventions. The `Generic`
/// variants are what a parser returns when it cannot confirm a specific ecosystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatEcosystem {
    GenericLogiqx,
    NoIntro,
    Redump,
    GenericClrMamePro,
    Tosec,
}

impl DatEcosystem {
    pub fn label(self) -> &'static str {
        match self {
            Self::GenericLogiqx => "Generic Logiqx",
            Self::NoIntro => "No-Intro",
            Self::Redump => "Redump",
            Self::GenericClrMamePro => "Generic ClrMamePro",
            Self::Tosec => "TOSEC",
        }
    }
}

/// The file format of a DAT file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatFormat {
    Logiqx,
    ClrMamePro,
}

impl DatFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Logiqx => "Logiqx XML",
            Self::ClrMamePro => "ClrMamePro",
        }
    }
}

/// What a DAT file is and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatSource {
    pub format: DatFormat,
    pub ecosystem: DatEcosystem,
    pub file_path: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub clrmamepro_header: Option<String>,
    pub entry_count: usize,
    pub rom_count: usize,
    pub parse_warnings: Vec<String>,
}

/// A checksum algorithm as it appears in a DAT file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumAlgorithm {
    Crc32,
    Md5,
    Sha1,
    Sha256,
}

impl ChecksumAlgorithm {
    pub fn label(self) -> &'static str {
        match self {
            Self::Crc32 => "CRC32",
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
        }
    }

    pub fn hex_length(self) -> usize {
        match self {
            Self::Crc32 => 8,
            Self::Md5 => 32,
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

/// A single checksum from a DAT entry, with normalised value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatChecksum {
    pub algorithm: ChecksumAlgorithm,
    pub value: String,
}

impl DatChecksum {
    /// Normalises and validates one checksum.
    ///
    /// Returns `None` for anything that is not the right length of lowercase hex.
    pub fn parse(algorithm: ChecksumAlgorithm, raw: &str) -> Option<Self> {
        let trimmed = raw.trim().to_ascii_lowercase();
        if trimmed.len() != algorithm.hex_length()
            || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Self {
            algorithm,
            value: trimmed,
        })
    }
}

/// One ROM entry within a game entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRomEntry {
    pub name: String,
    pub size_bytes: Option<u64>,
    pub crc32: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub status: Option<String>,
    pub merge: Option<String>,
    pub date: Option<String>,
    /// Raw placement and loading attributes retained for later set semantics.
    #[serde(default)]
    pub offset: Option<String>,
    /// Raw `loadflag` value, when the DAT declares one (Logiqx `<rom
    /// loadflag="...">`, ClrMamePro `loadflag value`).
    ///
    /// Not interpreted: this is provenance, not an operational model. MAME
    /// uses `loadflag` to mark ROM entries that are not an ordinary physical
    /// dump at all - `fill`/`reload`/`continue` describe how to synthesize
    /// or reuse bytes rather than a file to locate - and this codebase has
    /// no logic anywhere that understands what to do with any `loadflag`
    /// value. A consumer that needs to know "is this an ordinary physical
    /// ROM" should treat `Some(_)` here as "no", regardless of the value.
    #[serde(default)]
    pub loadflag: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub optional: Option<String>,
    #[serde(default)]
    pub bios: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

impl DatRomEntry {
    pub fn checksums(&self) -> Vec<DatChecksum> {
        let mut result = Vec::with_capacity(4);
        if let Some(ref value) = self.crc32
            && let Some(c) = DatChecksum::parse(ChecksumAlgorithm::Crc32, value)
        {
            result.push(c);
        }
        if let Some(ref value) = self.md5
            && let Some(c) = DatChecksum::parse(ChecksumAlgorithm::Md5, value)
        {
            result.push(c);
        }
        if let Some(ref value) = self.sha1
            && let Some(c) = DatChecksum::parse(ChecksumAlgorithm::Sha1, value)
        {
            result.push(c);
        }
        if let Some(ref value) = self.sha256
            && let Some(c) = DatChecksum::parse(ChecksumAlgorithm::Sha256, value)
        {
            result.push(c);
        }
        result
    }

    pub fn strongest_checksum(&self) -> Option<DatChecksum> {
        self.checksums().into_iter().max_by_key(|c| c.algorithm)
    }
}

/// One disk/CHD declaration within a DAT entry.
///
/// These fields are provenance only. In particular, the SHA-1 is not treated
/// as an ordinary ROM-file hash and no CHD content is opened or verified here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatDiskEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub merge: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub index: Option<String>,
    #[serde(default)]
    pub writable: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub optional: Option<String>,
}

/// A referenced device set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatDeviceRefEntry {
    #[serde(default)]
    pub name: Option<String>,
}

/// A sample-file declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatSampleEntry {
    #[serde(default)]
    pub name: Option<String>,
}

/// One BIOS variant declaration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatBiosSetEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
}

/// A software-list data area. Member interpretation remains deferred.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatDataAreaEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub roms: Vec<DatRomEntry>,
}

/// A software-list disk area. Member interpretation remains deferred.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatDiskAreaEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub disks: Vec<DatDiskEntry>,
}

/// One software-list part and the structural areas declared inside it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatPartEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub interface: Option<String>,
    #[serde(default)]
    pub data_areas: Vec<DatDataAreaEntry>,
    #[serde(default)]
    pub disk_areas: Vec<DatDiskAreaEntry>,
}

/// One game entry from a DAT file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatGameEntry {
    pub name: String,
    /// Raw `<game id="...">` declaration, when the DAT publishes one.
    ///
    /// No-Intro DATs assign a stable numeric-or-alphanumeric `id` to every
    /// entry and reference it from a clone via `cloneofid` instead of (or
    /// alongside) a name-based `cloneof`. This field preserves that literal
    /// value so a `cloneofid` can be resolved as a second, ID-keyed identity,
    /// never as a name lookup against an ID string (which is what silently
    /// broke that resolution before this field existed).
    #[serde(default)]
    pub id: Option<String>,
    pub description: Option<String>,
    pub roms: Vec<DatRomEntry>,
    pub clone_of: Option<String>,
    #[serde(default)]
    pub rom_of: Option<String>,
    pub sample_of: Option<String>,
    #[serde(default)]
    pub is_bios: Option<String>,
    /// Raw `isdevice` declaration. A `<device_ref>` must resolve to a node
    /// that declares itself a device; without this field a malformed
    /// catalogue could point a device requirement at an ordinary game and
    /// have it silently satisfied by that game's own storage. Captured as the
    /// raw string so an unexpected value stays visible rather than being
    /// coerced to `false`.
    #[serde(default)]
    pub is_device: Option<String>,
    #[serde(default)]
    pub runnable: Option<String>,
    /// Raw software-list support declaration (`yes`, `partial`, or `no`).
    /// Preserved as provenance; no completeness semantics are applied yet.
    #[serde(default)]
    pub supported: Option<String>,
    #[serde(default)]
    pub disks: Vec<DatDiskEntry>,
    #[serde(default)]
    pub device_refs: Vec<DatDeviceRefEntry>,
    #[serde(default)]
    pub samples: Vec<DatSampleEntry>,
    #[serde(default)]
    pub bios_sets: Vec<DatBiosSetEntry>,
    #[serde(default)]
    pub parts: Vec<DatPartEntry>,
    pub board: Option<String>,
    pub rebuild_to: Option<String>,
    pub year: Option<String>,
    pub manufacturer: Option<String>,
    pub source_file: Option<String>,
    pub comment: Option<String>,
    /// Structured upstream fields retained verbatim for technical review.
    #[serde(default)]
    pub original_metadata: DatOriginalMetadata,
    /// Derived EmuWiz annotation. Never changes upstream identity semantics.
    #[serde(default)]
    pub content_classification: DatContentClassification,
    /// Whether the source parser detected structure it could not preserve or
    /// cannot prove the absence of at all.
    ///
    /// This remains a capability signal even when the raw elements are
    /// preserved by the additive Stage 2 model. Fully represented disks,
    /// parts, areas, samples, BIOS declarations, and device references do not
    /// set it; malformed nesting and unrepresented structures still do.
    ///
    /// `false` is a positive claim that the parser preserved every structural
    /// element it recognizes, never a default assumed without evidence. Every
    /// entry the ClrMamePro parser produces sets this
    /// `true` unconditionally: that parser does not currently attempt to
    /// detect any of this structure, so it cannot honestly claim `false`
    /// for anything.
    #[serde(default)]
    pub unsupported_structure: bool,
}

impl DatGameEntry {
    pub fn rom_count(&self) -> usize {
        self.roms.len()
    }
}

/// The complete parsed contents of a DAT file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedDat {
    pub source: DatSource,
    pub games: Vec<DatGameEntry>,
}

impl ParsedDat {
    pub fn total_roms(&self) -> usize {
        self.games.iter().map(|g| g.rom_count()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::{DatDataAreaEntry, DatDiskAreaEntry};

    #[test]
    fn area_member_vectors_default_when_deserializing_older_data() {
        let data_area: DatDataAreaEntry =
            serde_json::from_str(r#"{"name":"prg"}"#).expect("data area should deserialize");
        let disk_area: DatDiskAreaEntry =
            serde_json::from_str(r#"{"name":"cdrom"}"#).expect("disk area should deserialize");

        assert!(data_area.roms.is_empty());
        assert!(disk_area.disks.is_empty());
    }
}
