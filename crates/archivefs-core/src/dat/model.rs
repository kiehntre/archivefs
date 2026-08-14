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

/// One game entry from a DAT file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatGameEntry {
    pub name: String,
    pub description: Option<String>,
    pub roms: Vec<DatRomEntry>,
    pub clone_of: Option<String>,
    pub sample_of: Option<String>,
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
    /// Whether the source parser detected structure or a relationship on
    /// this entry that it does not fully observe - a `<disk>`, `<sample>`,
    /// `<part>`, `<dataarea>`, or device/dependency-style child (Logiqx) -
    /// **or** cannot prove the absence of such structure at all (every
    /// entry the ClrMamePro parser produces).
    ///
    /// This is a capability/provenance signal, not a parsed model: nothing
    /// in this codebase interprets what any of these elements mean, and
    /// this flag never distinguishes which one was seen. It exists so a
    /// consumer that cannot safely reason about structure beyond plain
    /// `<rom>` children - `dat::set`'s Stage 1 completeness classifier,
    /// currently - can tell that `roms` is not proven to be the whole
    /// picture of this entry's real content, and refuse to guess.
    ///
    /// `false` is a positive claim ("this parser looked, and found only
    /// ordinary `<rom>` children"), never a default assumed in the absence
    /// of evidence. Every entry the ClrMamePro parser produces sets this
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
