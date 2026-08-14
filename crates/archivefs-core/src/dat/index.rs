//! In-memory collision-aware hash indexes over DAT entries.
//!
//! Every hash from every DAT entry is indexed once, so an audit lookup is a
//! map access rather than a linear scan. Collisions are retained: when two
//! DAT entries share a CRC32 (or any other hash), both are kept, and the
//! audit reports `ExactMultipleCandidates` rather than silently picking one.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::classification::{DatContentClassification, DatOriginalMetadata};
use super::model::{ChecksumAlgorithm, DatChecksum, ParsedDat};

/// The position of a ROM declaration within its game.
///
/// Names are deliberately absent: duplicate ROM, part, and data-area names
/// are legal catalogue data and are not stable identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MemberLocation {
    TopLevel {
        rom_index: usize,
    },
    DataArea {
        part_index: usize,
        data_area_index: usize,
        member_index: usize,
    },
}

/// Positional identity for one declared ROM slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DatMemberKey {
    pub game_index: usize,
    pub location: MemberLocation,
}

/// A reference to one ROM in one game entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatRomRef {
    pub game_index: usize,
    pub game_name: String,
    pub rom_index: usize,
    /// Exact declaration position. `rom_index` remains above for source and
    /// diagnostic compatibility; identity always comes from this key.
    pub member_key: DatMemberKey,
    pub rom_name: String,
    pub size_bytes: Option<u64>,
    pub checksums: Vec<DatChecksum>,
    pub status: Option<String>,
    pub merge: Option<String>,
    pub content_classification: DatContentClassification,
    pub original_metadata: DatOriginalMetadata,
}

impl DatRomRef {
    /// Returns the exact declaration key without involving display names.
    pub fn key(&self) -> DatMemberKey {
        self.member_key
    }
}

/// The position of a disk declaration within its game.
///
/// Deliberately separate from [`MemberLocation`] even though the shapes
/// mirror each other: a disk and a ROM are different kinds of catalogue
/// member with different identity fields (SHA-1 only, never CRC32/MD5/
/// SHA-256), and keeping the enums distinct means a disk key can never be
/// compared equal to, or accidentally substituted for, a ROM key even though
/// both carry a `usize`-shaped position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiskLocation {
    TopLevel {
        disk_index: usize,
    },
    DiskArea {
        part_index: usize,
        disk_area_index: usize,
        member_index: usize,
    },
}

/// Positional identity for one declared disk slot. Never derived from
/// `disk_name`: duplicate disk/diskarea names are legal catalogue data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DatDiskKey {
    pub game_index: usize,
    pub location: DiskLocation,
}

/// A reference to one disk in one game entry.
///
/// `sha1` is the *only* lookup identity here - deliberately never CRC32/MD5/
/// SHA-256 (a CHD v5 header exposes no such fields for the disk's own
/// identity) and never `disk_name` (display metadata only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatDiskRef {
    pub game_index: usize,
    pub game_name: String,
    pub disk_key: DatDiskKey,
    pub disk_name: String,
    /// Normalised lowercase 40-hex SHA-1, the DAT's declared disk identity.
    pub sha1: String,
    pub status: Option<String>,
    pub merge: Option<String>,
    pub optional: Option<String>,
}

impl DatDiskRef {
    /// Returns the exact declaration key without involving display names.
    pub fn key(&self) -> DatDiskKey {
        self.disk_key
    }
}

/// Index into a parsed DAT file's disk declarations, keyed by SHA-1 only.
///
/// Structurally separate from [`DatIndex`]: this type has no CRC32/MD5/
/// SHA-256/filename map at all, so a disk SHA-1 has nowhere to accidentally
/// land in the ROM namespace, and a ROM SHA-1 has nowhere to land here. A CHD
/// v5 header's `overall_sha1` is looked up only against
/// [`DatDiskIndex::by_disk_sha1`], never against [`DatIndex::by_sha1`].
#[derive(Debug, Clone, Default)]
pub struct DatDiskIndex {
    pub by_disk_sha1: HashMap<String, Vec<DatDiskRef>>,
}

impl DatDiskIndex {
    /// Builds a disk index from a parsed DAT file.
    ///
    /// Every disk in every game is indexed once by its normalised SHA-1.
    /// A disk with no SHA-1, or one that is not well-formed hex of the right
    /// length, is not indexed at all - it can never be looked up, so it can
    /// never be silently promoted to "present" (fail closed, R5-equivalent
    /// for disks).
    pub fn build(dat: &ParsedDat) -> Self {
        let mut index = Self::default();

        for (game_index, game) in dat.games.iter().enumerate() {
            for (disk_index, disk) in game.disks.iter().enumerate() {
                let key = DatDiskKey {
                    game_index,
                    location: DiskLocation::TopLevel { disk_index },
                };
                index.insert_disk(game_index, &game.name, key, disk);
            }

            for (part_index, part) in game.parts.iter().enumerate() {
                for (disk_area_index, area) in part.disk_areas.iter().enumerate() {
                    for (member_index, disk) in area.disks.iter().enumerate() {
                        let key = DatDiskKey {
                            game_index,
                            location: DiskLocation::DiskArea {
                                part_index,
                                disk_area_index,
                                member_index,
                            },
                        };
                        index.insert_disk(game_index, &game.name, key, disk);
                    }
                }
            }
        }

        index
    }

    fn insert_disk(
        &mut self,
        game_index: usize,
        game_name: &str,
        key: DatDiskKey,
        disk: &super::model::DatDiskEntry,
    ) {
        let Some(raw_sha1) = disk.sha1.as_deref() else {
            return;
        };
        // Reuses the same validated hex normalisation ROM checksums use,
        // rather than hand-rolling a second, weaker SHA-1 check.
        let Some(checksum) = DatChecksum::parse(ChecksumAlgorithm::Sha1, raw_sha1) else {
            return;
        };

        let disk_ref = DatDiskRef {
            game_index,
            game_name: game_name.to_string(),
            disk_key: key,
            disk_name: disk.name.clone().unwrap_or_default(),
            sha1: checksum.value.clone(),
            status: disk.status.clone(),
            merge: disk.merge.clone(),
            optional: disk.optional.clone(),
        };

        self.by_disk_sha1
            .entry(checksum.value)
            .or_default()
            .push(disk_ref);
    }

    /// Look up by disk SHA-1 (already-normalised lowercase 40-hex expected;
    /// non-matching case or malformed input simply misses). Returns
    /// candidates (empty if none).
    pub fn lookup_disk_sha1(&self, sha1: &str) -> &[DatDiskRef] {
        self.by_disk_sha1
            .get(sha1)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
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
                    member_key: DatMemberKey {
                        game_index,
                        location: MemberLocation::TopLevel { rom_index },
                    },
                    rom_name: rom.name.clone(),
                    size_bytes: rom.size_bytes,
                    checksums: rom.checksums(),
                    status: rom.status.clone(),
                    merge: rom.merge.clone(),
                    content_classification: game.content_classification.clone(),
                    original_metadata: game.original_metadata.clone(),
                };

                index.insert_rom(rom, rom_ref);
            }

            for (part_index, part) in game.parts.iter().enumerate() {
                for (data_area_index, area) in part.data_areas.iter().enumerate() {
                    for (member_index, rom) in area.roms.iter().enumerate() {
                        let rom_ref = DatRomRef {
                            game_index,
                            game_name: game.name.clone(),
                            // Retained for source compatibility. Nested identity
                            // always comes from `member_key`.
                            rom_index: member_index,
                            member_key: DatMemberKey {
                                game_index,
                                location: MemberLocation::DataArea {
                                    part_index,
                                    data_area_index,
                                    member_index,
                                },
                            },
                            rom_name: rom.name.clone(),
                            size_bytes: rom.size_bytes,
                            checksums: rom.checksums(),
                            status: rom.status.clone(),
                            merge: rom.merge.clone(),
                            content_classification: game.content_classification.clone(),
                            original_metadata: game.original_metadata.clone(),
                        };
                        index.insert_rom(rom, rom_ref);
                    }
                }
            }
        }

        index
    }

    fn insert_rom(&mut self, rom: &super::model::DatRomEntry, rom_ref: DatRomRef) {
        if let Some(ref crc) = rom.crc32 {
            self.by_crc32
                .entry(crc.clone())
                .or_default()
                .push(rom_ref.clone());
        }
        if let Some(ref md5) = rom.md5 {
            self.by_md5
                .entry(md5.clone())
                .or_default()
                .push(rom_ref.clone());
        }
        if let Some(ref sha1) = rom.sha1 {
            self.by_sha1
                .entry(sha1.clone())
                .or_default()
                .push(rom_ref.clone());
        }
        if let Some(ref sha256) = rom.sha256 {
            self.by_sha256
                .entry(sha256.clone())
                .or_default()
                .push(rom_ref.clone());
        }

        self.by_filename
            .entry(rom.name.to_ascii_lowercase())
            .or_default()
            .push(rom_ref);
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
        DatDataAreaEntry, DatEcosystem, DatFormat, DatGameEntry, DatPartEntry, DatRomEntry,
        DatSource, ParsedDat,
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
        assert_eq!(
            candidates[0].key(),
            DatMemberKey {
                game_index: 0,
                location: MemberLocation::TopLevel { rom_index: 0 },
            }
        );
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

    #[test]
    fn nested_data_area_roms_join_every_existing_rom_map_with_positional_identity() {
        let mut dat = make_dat();
        let nested = DatRomEntry {
            name: "folder/nested.bin".into(),
            size_bytes: Some(4),
            crc32: Some("12345678".into()),
            md5: Some("11111111111111111111111111111111".into()),
            sha1: Some("2222222222222222222222222222222222222222".into()),
            sha256: Some("3333333333333333333333333333333333333333333333333333333333333333".into()),
            ..Default::default()
        };
        dat.games[0].parts = vec![DatPartEntry {
            data_areas: vec![DatDataAreaEntry {
                roms: vec![nested],
                ..Default::default()
            }],
            ..Default::default()
        }];

        let index = DatIndex::build(&dat);
        for candidate in [
            &index.lookup_crc32("12345678")[0],
            &index.lookup_md5("11111111111111111111111111111111")[0],
            &index.lookup_sha1("2222222222222222222222222222222222222222")[0],
            &index
                .lookup_sha256("3333333333333333333333333333333333333333333333333333333333333333")
                [0],
            &index.lookup_filename("FOLDER/NESTED.BIN")[0],
        ] {
            assert_eq!(
                candidate.key(),
                DatMemberKey {
                    game_index: 0,
                    location: MemberLocation::DataArea {
                        part_index: 0,
                        data_area_index: 0,
                        member_index: 0,
                    },
                }
            );
        }
    }

    mod disk_index {
        use super::*;
        use crate::dat::model::{DatDiskAreaEntry, DatDiskEntry, DatPartEntry};

        fn disk(name: &str, sha1: Option<&str>) -> DatDiskEntry {
            DatDiskEntry {
                name: Some(name.to_string()),
                sha1: sha1.map(str::to_string),
                merge: None,
                region: None,
                index: None,
                writable: None,
                status: None,
                optional: None,
            }
        }

        fn dat_with_top_level_disk(sha1: Option<&str>) -> ParsedDat {
            let mut dat = make_dat();
            dat.games[0].disks = vec![disk("disc1.chd", sha1)];
            dat
        }

        #[test]
        fn top_level_dat_disk_indexed_exactly_once() {
            let dat = dat_with_top_level_disk(Some("111111111111111111111111111111111111111a"));
            let index = DatDiskIndex::build(&dat);

            let candidates = index.lookup_disk_sha1("111111111111111111111111111111111111111a");
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].game_name, "Game Alpha");
            assert_eq!(candidates[0].disk_name, "disc1.chd");
            assert_eq!(
                candidates[0].key(),
                DatDiskKey {
                    game_index: 0,
                    location: DiskLocation::TopLevel { disk_index: 0 },
                }
            );
        }

        #[test]
        fn nested_part_diskarea_disk_indexed_exactly_once_with_correct_location() {
            let mut dat = make_dat();
            dat.games[0].parts = vec![DatPartEntry {
                disk_areas: vec![DatDiskAreaEntry {
                    disks: vec![disk(
                        "nested.chd",
                        Some("222222222222222222222222222222222222222b"),
                    )],
                    ..Default::default()
                }],
                ..Default::default()
            }];

            let index = DatDiskIndex::build(&dat);
            let candidates = index.lookup_disk_sha1("222222222222222222222222222222222222222b");

            assert_eq!(candidates.len(), 1);
            assert_eq!(
                candidates[0].key(),
                DatDiskKey {
                    game_index: 0,
                    location: DiskLocation::DiskArea {
                        part_index: 0,
                        disk_area_index: 0,
                        member_index: 0,
                    },
                }
            );
        }

        #[test]
        fn same_disk_name_in_different_diskareas_stays_distinct_by_position_not_name() {
            let mut dat = make_dat();
            dat.games[0].parts = vec![DatPartEntry {
                disk_areas: vec![
                    DatDiskAreaEntry {
                        disks: vec![disk(
                            "shared-name.chd",
                            Some("333333333333333333333333333333333333333c"),
                        )],
                        ..Default::default()
                    },
                    DatDiskAreaEntry {
                        disks: vec![disk(
                            "shared-name.chd",
                            Some("444444444444444444444444444444444444444d"),
                        )],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }];

            let index = DatDiskIndex::build(&dat);
            let first = &index.lookup_disk_sha1("333333333333333333333333333333333333333c")[0];
            let second = &index.lookup_disk_sha1("444444444444444444444444444444444444444d")[0];

            assert_eq!(
                first.disk_name, second.disk_name,
                "names collide on purpose"
            );
            assert_ne!(
                first.key(),
                second.key(),
                "position must disambiguate identically-named disks in different diskareas"
            );
            assert_eq!(
                first.key(),
                DatDiskKey {
                    game_index: 0,
                    location: DiskLocation::DiskArea {
                        part_index: 0,
                        disk_area_index: 0,
                        member_index: 0,
                    },
                }
            );
            assert_eq!(
                second.key(),
                DatDiskKey {
                    game_index: 0,
                    location: DiskLocation::DiskArea {
                        part_index: 0,
                        disk_area_index: 1,
                        member_index: 0,
                    },
                }
            );
        }

        #[test]
        fn malformed_disk_sha1_is_not_indexed() {
            let dat = dat_with_top_level_disk(Some("not-hex-at-all"));
            let index = DatDiskIndex::build(&dat);

            assert!(index.by_disk_sha1.is_empty());
        }

        #[test]
        fn empty_disk_sha1_is_not_indexed() {
            let dat = dat_with_top_level_disk(Some(""));
            let index = DatDiskIndex::build(&dat);

            assert!(index.by_disk_sha1.is_empty());
        }

        #[test]
        fn missing_disk_sha1_is_not_indexed() {
            let dat = dat_with_top_level_disk(None);
            let index = DatDiskIndex::build(&dat);

            assert!(index.by_disk_sha1.is_empty());
        }

        #[test]
        fn uppercase_valid_sha1_is_normalised_to_lowercase() {
            let dat = dat_with_top_level_disk(Some("111111111111111111111111111111111111111A"));
            let index = DatDiskIndex::build(&dat);

            assert!(
                index
                    .lookup_disk_sha1("111111111111111111111111111111111111111a")
                    .len()
                    == 1
            );
            assert_eq!(
                index.by_disk_sha1.keys().next().expect("one key").as_str(),
                "111111111111111111111111111111111111111a"
            );
        }

        #[test]
        fn rom_and_disk_sharing_one_sha1_string_never_cross_namespaces() {
            let shared_sha1 = "555555555555555555555555555555555555555e";
            let mut dat = make_dat();
            dat.games[0].roms[0].sha1 = Some(shared_sha1.to_string());
            dat.games[0].disks = vec![disk("disc1.chd", Some(shared_sha1))];

            let rom_index = DatIndex::build(&dat);
            let disk_index = DatDiskIndex::build(&dat);

            let rom_candidates = rom_index.lookup_sha1(shared_sha1);
            assert_eq!(rom_candidates.len(), 1);
            assert_eq!(rom_candidates[0].rom_name, "alpha.bin");

            let disk_candidates = disk_index.lookup_disk_sha1(shared_sha1);
            assert_eq!(disk_candidates.len(), 1);
            assert_eq!(disk_candidates[0].disk_name, "disc1.chd");

            // The rom index has no disk-shaped map at all to leak into, and
            // vice versa - proven structurally, not just by these two
            // lookups agreeing.
            assert!(!rom_index.by_sha1.is_empty());
            assert!(!disk_index.by_disk_sha1.is_empty());
        }
    }
}
