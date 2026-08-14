//! In-memory collision-aware hash indexes over DAT entries.
//!
//! Every hash from every DAT entry is indexed once, so an audit lookup is a
//! map access rather than a linear scan. Collisions are retained: when two
//! DAT entries share a CRC32 (or any other hash), both are kept, and the
//! audit reports `ExactMultipleCandidates` rather than silently picking one.

use std::collections::HashMap;

use serde::Serialize;

use super::classification::{DatContentClassification, DatOriginalMetadata};
use super::model::{DatChecksum, ParsedDat};

/// A reference to one ROM in one game entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatRomRef {
    pub game_index: usize,
    pub game_name: String,
    pub rom_index: usize,
    pub rom_name: String,
    pub size_bytes: Option<u64>,
    pub checksums: Vec<DatChecksum>,
    pub status: Option<String>,
    pub merge: Option<String>,
    pub content_classification: DatContentClassification,
    pub original_metadata: DatOriginalMetadata,
}

/// Index into a parsed DAT file, keyed by hash values.
#[derive(Debug, Clone)]
pub struct DatIndex {
    pub by_crc32: HashMap<String, Vec<DatRomRef>>,
    pub by_md5: HashMap<String, Vec<DatRomRef>>,
    pub by_sha1: HashMap<String, Vec<DatRomRef>>,
    pub by_sha256: HashMap<String, Vec<DatRomRef>>,
    pub by_filename: HashMap<String, Vec<DatRomRef>>,
}

impl DatIndex {
    /// Builds an index from a parsed DAT file.
    ///
    /// Every ROM in every game is indexed by each hash it carries and by its
    /// filename (for `FilenameOnly` fallback).
    pub fn build(dat: &ParsedDat) -> Self {
        let mut index = Self {
            by_crc32: HashMap::new(),
            by_md5: HashMap::new(),
            by_sha1: HashMap::new(),
            by_sha256: HashMap::new(),
            by_filename: HashMap::new(),
        };

        for (game_index, game) in dat.games.iter().enumerate() {
            for (rom_index, rom) in game.roms.iter().enumerate() {
                let rom_ref = DatRomRef {
                    game_index,
                    game_name: game.name.clone(),
                    rom_index,
                    rom_name: rom.name.clone(),
                    size_bytes: rom.size_bytes,
                    checksums: rom.checksums(),
                    status: rom.status.clone(),
                    merge: rom.merge.clone(),
                    content_classification: game.content_classification.clone(),
                    original_metadata: game.original_metadata.clone(),
                };

                if let Some(ref crc) = rom.crc32 {
                    index
                        .by_crc32
                        .entry(crc.clone())
                        .or_default()
                        .push(rom_ref.clone());
                }
                if let Some(ref md5) = rom.md5 {
                    index
                        .by_md5
                        .entry(md5.clone())
                        .or_default()
                        .push(rom_ref.clone());
                }
                if let Some(ref sha1) = rom.sha1 {
                    index
                        .by_sha1
                        .entry(sha1.clone())
                        .or_default()
                        .push(rom_ref.clone());
                }
                if let Some(ref sha256) = rom.sha256 {
                    index
                        .by_sha256
                        .entry(sha256.clone())
                        .or_default()
                        .push(rom_ref.clone());
                }

                let fname_lower = rom.name.to_ascii_lowercase();
                index
                    .by_filename
                    .entry(fname_lower)
                    .or_default()
                    .push(rom_ref.clone());
            }
        }

        index
    }

    /// Look up by CRC32. Returns candidates (empty if none).
    pub fn lookup_crc32(&self, crc: &str) -> &[DatRomRef] {
        self.by_crc32.get(crc).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Look up by MD5.
    pub fn lookup_md5(&self, md5: &str) -> &[DatRomRef] {
        self.by_md5.get(md5).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Look up by SHA-1.
    pub fn lookup_sha1(&self, sha1: &str) -> &[DatRomRef] {
        self.by_sha1.get(sha1).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Look up by SHA-256.
    pub fn lookup_sha256(&self, sha256: &str) -> &[DatRomRef] {
        self.by_sha256
            .get(sha256)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Look up by filename (case-insensitive).
    pub fn lookup_filename(&self, filename: &str) -> &[DatRomRef] {
        self.by_filename
            .get(&filename.to_ascii_lowercase())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// How many distinct CRC32 entries are indexed.
    pub fn crc32_count(&self) -> usize {
        self.by_crc32.len()
    }

    /// How many distinct MD5 entries are indexed.
    pub fn md5_count(&self) -> usize {
        self.by_md5.len()
    }

    /// How many distinct SHA-1 entries are indexed.
    pub fn sha1_count(&self) -> usize {
        self.by_sha1.len()
    }

    /// How many distinct SHA-256 entries are indexed.
    pub fn sha256_count(&self) -> usize {
        self.by_sha256.len()
    }

    /// Collision count: entries with more than one ROM reference.
    pub fn crc32_collisions(&self) -> usize {
        self.by_crc32.values().filter(|v| v.len() > 1).count()
    }

    pub fn md5_collisions(&self) -> usize {
        self.by_md5.values().filter(|v| v.len() > 1).count()
    }

    pub fn sha1_collisions(&self) -> usize {
        self.by_sha1.values().filter(|v| v.len() > 1).count()
    }

    pub fn sha256_collisions(&self) -> usize {
        self.by_sha256.values().filter(|v| v.len() > 1).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::model::{
        DatEcosystem, DatFormat, DatGameEntry, DatRomEntry, DatSource, ParsedDat,
    };

    fn make_dat() -> ParsedDat {
        ParsedDat {
            source: DatSource {
                format: DatFormat::Logiqx,
                ecosystem: DatEcosystem::NoIntro,
                file_path: "test.dat".into(),
                name: Some("Test".into()),
                description: None,
                version: None,
                author: None,
                homepage: None,
                clrmamepro_header: None,
                entry_count: 2,
                rom_count: 2,
                parse_warnings: Vec::new(),
            },
            games: vec![
                DatGameEntry {
                    name: "Game Alpha".into(),
                    description: None,
                    roms: vec![DatRomEntry {
                        name: "alpha.bin".into(),
                        size_bytes: Some(1024),
                        crc32: Some("deadbeef".into()),
                        md5: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
                        sha1: None,
                        sha256: None,
                        status: None,
                        merge: None,
                        date: None,
                        loadflag: None,
                        ..Default::default()
                    }],
                    clone_of: None,
                    sample_of: None,
                    board: None,
                    rebuild_to: None,
                    year: None,
                    manufacturer: None,
                    source_file: None,
                    comment: None,
                    original_metadata: Default::default(),
                    content_classification: Default::default(),
                    unsupported_structure: false,
                    ..Default::default()
                },
                DatGameEntry {
                    name: "Game Beta".into(),
                    description: None,
                    roms: vec![DatRomEntry {
                        name: "beta.bin".into(),
                        size_bytes: Some(2048),
                        crc32: Some("cafebabe".into()),
                        md5: Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()),
                        sha1: None,
                        sha256: None,
                        status: None,
                        merge: None,
                        date: None,
                        loadflag: None,
                        ..Default::default()
                    }],
                    clone_of: None,
                    sample_of: None,
                    board: None,
                    rebuild_to: None,
                    year: None,
                    manufacturer: None,
                    source_file: None,
                    comment: None,
                    original_metadata: Default::default(),
                    content_classification: Default::default(),
                    unsupported_structure: false,
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn index_lookup_by_crc32() {
        let dat = make_dat();
        let index = DatIndex::build(&dat);
        let candidates = index.lookup_crc32("deadbeef");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].game_name, "Game Alpha");
    }

    #[test]
    fn index_preserves_rom_status_and_merge_provenance() {
        let mut dat = make_dat();
        dat.games[0].roms[0].status = Some("baddump".into());
        dat.games[0].roms[0].merge = Some("parent.bin".into());

        let index = DatIndex::build(&dat);
        let candidates = index.lookup_crc32("deadbeef");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].status.as_deref(), Some("baddump"));
        assert_eq!(candidates[0].merge.as_deref(), Some("parent.bin"));
    }

    #[test]
    fn index_miss_returns_empty() {
        let dat = make_dat();
        let index = DatIndex::build(&dat);
        let candidates = index.lookup_crc32("00000000");
        assert!(candidates.is_empty());
    }

    #[test]
    fn index_by_filename() {
        let dat = make_dat();
        let index = DatIndex::build(&dat);
        let candidates = index.lookup_filename("ALPHA.BIN");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].game_name, "Game Alpha");
    }

    #[test]
    fn crc32_collision_is_counted() {
        let mut dat = make_dat();
        // Add a second ROM with the same CRC32
        dat.games[1].roms[0].crc32 = Some("deadbeef".into());
        let index = DatIndex::build(&dat);
        assert_eq!(index.crc32_collisions(), 1);
    }
}
