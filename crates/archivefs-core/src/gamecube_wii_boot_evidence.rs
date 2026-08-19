//! Pure, read-only GameCube/Wii disc-structure evidence, backed by the
//! [`nod`](https://docs.rs/nod) crate rather than a hand-written optical
//! partition/filesystem stack.
//!
//! # Why `nod`, not our own parser
//!
//! GameCube/Wii disc structure (the 0x400-byte disc header, the apploader,
//! the FST, and - for Wii - the encrypted partition table) is real,
//! nontrivial format knowledge already correctly implemented, reviewed,
//! and maintained in `nod` (MIT/Apache-2.0, pure Rust with default
//! features disabled here - no native/C dependency). Hand-writing this
//! from scratch would duplicate work `nod` already does safely, which
//! this crate's own dependency policy says to avoid. The disc-header
//! **field offsets** this module relies on (`game_id` at 0, `disc_num` at
//! 6, `disc_version` at 7, magics at `0x18`/`0x1C`) already match what
//! [`crate::game_identity`]'s own independently-reviewed magic check uses
//! (see that module's `GAMECUBE_MAGIC`/`WII_MAGIC`/`WII_MAGIC_OFFSET`/
//! `GAMECUBE_MAGIC_OFFSET` constants) - two independent implementations
//! agreeing is itself a form of verification.
//!
//! # I/O model - path-based, not in-memory bytes
//!
//! Unlike every other reader in this arc, this module takes a
//! [`std::path::Path`], not `&[u8]`. `nod::Disc::new_stream` *can* accept
//! an in-memory stream, but only an owned, `'static` one - wrapping our
//! already-loaded buffer would mean cloning the entire disc image a second
//! time (GameCube images run ~1.4 GB, Wii ~4.7 GB), which this module
//! avoids by opening the path directly instead, exactly the same tradeoff
//! [`crate::chd_optical_specialist`] already made for the same reason.
//!
//! # No Wii decryption performed or required
//!
//! `nod` can decrypt Wii partition data when keys are available, but this
//! module never asks it to: only structural metadata (disc header,
//! partition table entries, apploader header, FST, `main.dol` presence) is
//! read, all of which `nod` exposes without needing any key material at
//! all. A caller wanting actual *file contents* from an encrypted Wii
//! partition is out of scope here - see [`GcWiiDiscError`] for how that
//! would surface if ever attempted.
//!
//! # Collision safety
//!
//! `main.dol`, the FST, and the apploader are shared concepts across both
//! GameCube and Wii - none of them alone distinguish the two. The disc
//! header's own magic field (`is_gamecube()`/`is_wii()`) is what actually
//! distinguishes them, and even that is a *disc-structure* fact, not a
//! platform claim on its own - see the crate-level architecture principle
//! this whole arc follows.

use std::path::Path;

use nod::{Disc, OpenOptions, PartitionKind};

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

#[derive(Debug)]
pub enum GcWiiDiscError {
    /// `nod` could not open or parse the disc image at all. `detail` is
    /// its own error, rendered as text - this module deliberately does not
    /// re-export `nod` types in its own public API.
    Backend { detail: String },
    /// The disc header matched neither the GameCube nor the Wii magic.
    NotGcOrWii,
}

impl std::fmt::Display for GcWiiDiscError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend { detail } => {
                write!(formatter, "GameCube/Wii disc backend error: {detail}")
            }
            Self::NotGcOrWii => {
                formatter.write_str("disc header matches neither GameCube nor Wii magic")
            }
        }
    }
}

impl std::error::Error for GcWiiDiscError {}

/// One partition table entry - Wii only (a GameCube disc has none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiiPartitionFact {
    pub index: usize,
    /// `"Data"`, `"Update"`, `"Channel"`, or `"Unknown(<raw>)"` - the
    /// partition kind exactly as `nod` reports it.
    pub kind: String,
}

/// Structural facts read from the data partition's metadata (`nod`'s
/// `PartitionMeta`) - apploader/FST/`main.dol` presence, never decrypted
/// file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPartitionMetaFact {
    pub apploader_present: bool,
    pub apploader_date: Option<String>,
    pub main_dol_present: bool,
    pub fst_present: bool,
    pub fst_root_entry_count: Option<usize>,
}

/// Everything this module observed about one GameCube/Wii disc image -
/// never a platform decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcWiiDiscObservation {
    pub is_gamecube: bool,
    pub is_wii: bool,
    /// The 6-character game ID (e.g. `"GALE01"`), read directly from the
    /// disc header.
    pub game_id: String,
    pub disc_num: u8,
    pub disc_version: u8,
    pub game_title: String,
    /// Non-empty only for a Wii disc.
    pub partitions: Vec<WiiPartitionFact>,
    /// `None` if the data partition could not be opened/read at all
    /// (still not an error for the disc-header-level facts above, which
    /// remain valid either way).
    pub data_partition_meta: Option<DataPartitionMetaFact>,
}

/// Opens `path` via `nod` and observes GameCube/Wii disc structure facts.
///
/// Pure and read-only: `nod` opens the file for reading only; this module
/// never calls any conversion/rebuild/write API `nod` exposes. Fails
/// closed (`Err`) when `nod` cannot open the image at all, or when the
/// disc header matches neither the GameCube nor the Wii magic (a
/// structural fact, not a guess).
pub fn observe_gc_wii_disc(path: &Path) -> Result<GcWiiDiscObservation, GcWiiDiscError> {
    let disc = Disc::new_with_options(path, &OpenOptions::default()).map_err(|error| {
        GcWiiDiscError::Backend {
            detail: format!("{error:?}"),
        }
    })?;
    let header = disc.header();
    if !header.is_gamecube() && !header.is_wii() {
        return Err(GcWiiDiscError::NotGcOrWii);
    }

    let partitions = disc
        .partitions()
        .iter()
        .map(|info| WiiPartitionFact {
            index: info.index,
            kind: format!("{:?}", info.kind),
        })
        .collect();

    let data_partition_meta =
        disc.open_partition_kind(PartitionKind::Data)
            .ok()
            .and_then(|mut partition| {
                let meta = partition.meta().ok()?;
                let fst = meta.fst().ok();
                Some(DataPartitionMetaFact {
                    apploader_present: !meta.raw_apploader.is_empty(),
                    apploader_date: (!meta.raw_apploader.is_empty())
                        .then(|| meta.apploader_header().date_str().map(str::to_string))
                        .flatten(),
                    main_dol_present: !meta.raw_dol.is_empty(),
                    fst_present: !meta.raw_fst.is_empty(),
                    fst_root_entry_count: fst.map(|table| table.nodes.len()),
                })
            });

    Ok(GcWiiDiscObservation {
        is_gamecube: header.is_gamecube(),
        is_wii: header.is_wii(),
        game_id: header.game_id_str().to_string(),
        disc_num: header.disc_num,
        disc_version: header.disc_version,
        game_title: header.game_title_str().to_string(),
        partitions,
        data_partition_meta,
    })
}

/// Neutral evidence for a [`GcWiiDiscObservation`].
///
/// The disc-structure magic itself (`Strong` `BootStructure` - a real,
/// validated disc header, not a filename) and the game ID (`Corroborated`
/// `ProductCode`) are always emitted. `main.dol` presence is
/// `Corroborated` only, per the crate-level guidance that filenames shared
/// across GameCube/Wii never rise to `Strong` on their own.
pub fn observe_gc_wii_evidence(observation: &GcWiiDiscObservation) -> Vec<ContentEvidence> {
    let mut evidence = Vec::new();
    let disc_kind = if observation.is_wii {
        "Wii"
    } else {
        "GameCube"
    };
    evidence.push(ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        disc_kind,
        ContentEvidenceConfidence::Strong,
        "disc header magic validated by nod - a real disc-structure fact, not a filename or folder convention",
    ));
    if !observation.game_id.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            observation.game_id.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate game ID read from the disc header - not verified against a canonical release list",
        ));
    }
    if let Some(meta) = &observation.data_partition_meta
        && meta.main_dol_present
    {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "main.dol",
            ContentEvidenceConfidence::Corroborated,
            "main.dol present in the data partition - shared between GameCube and Wii, not unique proof of either",
        ));
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonexistent_path_is_rejected() {
        let result = observe_gc_wii_disc(Path::new("/nonexistent/path/does-not-exist.iso"));
        assert!(result.is_err());
    }

    #[test]
    fn non_disc_file_is_rejected() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "gc_wii_boot_evidence_test_{}.bin",
            std::process::id()
        ));
        std::fs::write(
            &path,
            b"this is definitely not a GameCube or Wii disc image",
        )
        .unwrap();
        let result = observe_gc_wii_disc(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn gamecube_evidence_is_strong_boot_structure() {
        let observation = GcWiiDiscObservation {
            is_gamecube: true,
            is_wii: false,
            game_id: "GALE01".to_string(),
            disc_num: 0,
            disc_version: 0,
            game_title: "Test Game".to_string(),
            partitions: Vec::new(),
            data_partition_meta: None,
        };
        let evidence = observe_gc_wii_evidence(&observation);
        let boot = evidence
            .iter()
            .find(|item| item.value == "GameCube")
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn wii_evidence_uses_wii_label() {
        let observation = GcWiiDiscObservation {
            is_gamecube: false,
            is_wii: true,
            game_id: "RMCE01".to_string(),
            disc_num: 0,
            disc_version: 0,
            game_title: "Test Wii Game".to_string(),
            partitions: vec![WiiPartitionFact {
                index: 0,
                kind: "Data".to_string(),
            }],
            data_partition_meta: None,
        };
        let evidence = observe_gc_wii_evidence(&observation);
        assert!(evidence.iter().any(|item| item.value == "Wii"));
    }

    #[test]
    fn game_id_is_corroborated_product_code() {
        let observation = GcWiiDiscObservation {
            is_gamecube: true,
            is_wii: false,
            game_id: "GALE01".to_string(),
            disc_num: 0,
            disc_version: 0,
            game_title: String::new(),
            partitions: Vec::new(),
            data_partition_meta: None,
        };
        let evidence = observe_gc_wii_evidence(&observation);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "GALE01");
        assert_eq!(product.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn main_dol_alone_is_corroborated_not_strong() {
        let observation = GcWiiDiscObservation {
            is_gamecube: true,
            is_wii: false,
            game_id: String::new(),
            disc_num: 0,
            disc_version: 0,
            game_title: String::new(),
            partitions: Vec::new(),
            data_partition_meta: Some(DataPartitionMetaFact {
                apploader_present: true,
                apploader_date: Some("2004/01/01".to_string()),
                main_dol_present: true,
                fst_present: true,
                fst_root_entry_count: Some(10),
            }),
        };
        let evidence = observe_gc_wii_evidence(&observation);
        let dol = evidence
            .iter()
            .find(|item| item.value == "main.dol")
            .unwrap();
        assert_eq!(dol.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn main_dol_and_fst_are_shared_gc_wii_concepts_never_strong() {
        // Neither main.dol nor the FST/apploader alone should ever produce
        // Strong evidence - only the validated disc header magic does.
        let observation = GcWiiDiscObservation {
            is_gamecube: false,
            is_wii: true,
            game_id: String::new(),
            disc_num: 0,
            disc_version: 0,
            game_title: String::new(),
            partitions: Vec::new(),
            data_partition_meta: Some(DataPartitionMetaFact {
                apploader_present: true,
                apploader_date: None,
                main_dol_present: true,
                fst_present: true,
                fst_root_entry_count: Some(5),
            }),
        };
        for item in observe_gc_wii_evidence(&observation) {
            if item.value == "main.dol" {
                assert_ne!(item.confidence, ContentEvidenceConfidence::Strong);
            }
        }
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let observation = GcWiiDiscObservation {
            is_gamecube: true,
            is_wii: false,
            game_id: "GALE01".to_string(),
            disc_num: 0,
            disc_version: 0,
            game_title: String::new(),
            partitions: Vec::new(),
            data_partition_meta: Some(DataPartitionMetaFact {
                apploader_present: true,
                apploader_date: None,
                main_dol_present: true,
                fst_present: true,
                fst_root_entry_count: Some(1),
            }),
        };
        for item in observe_gc_wii_evidence(&observation) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let observation = GcWiiDiscObservation {
            is_gamecube: true,
            is_wii: false,
            game_id: "GALE01".to_string(),
            disc_num: 0,
            disc_version: 0,
            game_title: String::new(),
            partitions: Vec::new(),
            data_partition_meta: None,
        };
        assert_eq!(
            observe_gc_wii_evidence(&observation),
            observe_gc_wii_evidence(&observation)
        );
    }
}
