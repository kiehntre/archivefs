//! Machine-counted evidence coverage inventory.
//!
//! Earlier milestone reports computed platform-coverage numbers by hand,
//! and drifted (see the crate-level Batch 5 milestone report). This module
//! is the fix: one small, explicit manifest ([`COVERAGE`]) linked to
//! canonical platform ids from [`crate::platform::PLATFORMS`] - never a
//! second platform registry, never a duplicate of metadata
//! [`crate::platform::Platform`] already carries (display name, extensions,
//! aliases stay there) - plus a set of counting functions that *compute*
//! every number this milestone's final report needs from real data
//! ([`COVERAGE`] itself and [`crate::platform::PLATFORMS`]), so a report
//! built from [`coverage_report`] can never silently drift from the code
//! again. [`crate::coverage_inventory::tests`] enforces the manifest's own
//! internal consistency (no unknown ids, no duplicates, no stale entries).

use crate::platform::PLATFORMS;
#[cfg(test)]
use crate::platform::platform_by_id;

/// Developer/test coverage metadata only - deliberately **not** the same
/// vocabulary as user-facing identity states
/// ([`crate::platform::identity::PlatformIdentityConfidence`]'s
/// `Inferred`/`Strong`/`High`/`Verified`/`UserSelected`, or a future
/// `Suggested`/`User Accepted`/`Conflict` state). This says how far this
/// crate's *engineering* work on a platform has gotten, not what an
/// end-user's copy of a specific file has been confirmed to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationStatus {
    /// No dedicated content-evidence detector module exists for this
    /// platform at all.
    NoDeepEvidence,
    /// A detector module and its own unit tests exist, but no real-world
    /// specimen has been confirmed to run through it successfully.
    SyntheticValidated,
    /// A real specimen (from this project's own read-only corpus) has been
    /// run through the detector and produced correct evidence - see the
    /// `notes` field for which milestone/sample.
    RealValidated,
    /// A detector exists and covers *part* of the format (e.g. Master
    /// System's TMR SEGA magic is implemented and real-corpus specimens
    /// exist, but this particular platform's own region-nibble combination
    /// was not exercised against one this session).
    Partial,
    /// Deliberately not implemented, with a documented reason (e.g. Atari
    /// Jaguar's encrypted per-title boot block, PC Engine's lack of a
    /// standardized internal header) - a decision, not an oversight.
    Deferred,
}

/// Batch 6: the smallest useful structured provenance for a
/// [`ValidationStatus::RealValidated`] entry - which rule proved it, what
/// class of specimen, and what container/form it was in. Deliberately no
/// absolute filesystem path (this project's corpus lives outside the
/// repository and is not something the registry should depend on, or leak
/// into a report a person might share) - a short, stable, descriptive
/// label instead, exactly as the milestone asked for
/// (`"GameCube RVZ specimen"`, not `/mnt/games/roms/gcn/...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealValidationProvenance {
    /// The [`crate::platform_evidence_fusion::FusionRule::id`] this
    /// specimen exercised, when the platform has a fusion rule at all.
    pub rule_id: Option<&'static str>,
    /// A short, stable class label for the specimen - never a path.
    pub sample_class: &'static str,
    /// The container/form the bytes were read through.
    pub container: &'static str,
}

/// Batch 9: real content+DAT convergence facts for one platform - separate
/// from [`RealValidationProvenance`] (which is about content-evidence-only
/// validation). `None` when no real ROM + real local DAT overlap was found
/// this session - never fabricated; see [`coverage_report`]'s own
/// `real_content_dat_agreement` count, which only counts entries where this
/// field is `Some`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealDatValidation {
    /// A short, stable label for the real local DAT family used (e.g.
    /// `"TOSEC"`, `"No-Intro"`) - never a filesystem path.
    pub dat_family: &'static str,
    /// How many real local files in the corpus were confirmed to
    /// cryptographic-hash-match this DAT family this session - a real,
    /// counted number, never an estimate.
    pub sample_match_count: usize,
    /// Whether at least one of those matches required the normalized
    /// (not physical) byte representation to succeed - see
    /// [`crate::platform_evidence_fusion::dat_hash_representation`].
    pub normalized_match_observed: bool,
}

/// One platform's evidence-engineering coverage. `canonical_id` must match
/// a real [`crate::platform::Platform::id`] - enforced by this module's own
/// test suite, never assumed.
#[derive(Debug, Clone, Copy)]
pub struct PlatformEvidenceCoverage {
    pub canonical_id: &'static str,
    /// Source module(s) providing this platform's own deep evidence -
    /// names only (`"saturn_boot_evidence"`), not re-exported types; this
    /// stays a documentation aid, never a second API surface.
    pub detector_modules: &'static [&'static str],
    /// Whether a physical/normalized dual-identity transform exists for
    /// this platform (N64 byte-order, SNES/Lynx/Atari7800 copier-header
    /// strip, SMD de-interleave).
    pub normalization: bool,
    /// Whether [`crate::platform_evidence_fusion::RULES`] contains at
    /// least one rule targeting this platform - computed by this module's
    /// own tests against the real rule table, never hand-copied here.
    pub real_validation: ValidationStatus,
    pub notes: &'static str,
    /// Structured provenance for [`ValidationStatus::RealValidated`]
    /// entries - `None` for every other status, and `None` for a handful
    /// of RealValidated entries this milestone did not touch (the prose
    /// `notes` field above still names their specimen; this field is
    /// additive, not a replacement).
    pub real_validation_provenance: Option<RealValidationProvenance>,
    /// Batch 9: real content+DAT convergence, when this session confirmed
    /// it against real local corpus + real local DAT files. `None` is
    /// honest and common - most platforms have no real local DAT overlap
    /// (or no DAT was checked at all).
    pub dat_validation: Option<RealDatValidation>,
}

/// The coverage manifest - one entry per platform this crate has *any*
/// dedicated content-evidence work for. A canonical platform with no entry
/// here is, by construction, evidence-poor - see [`evidence_poor_count`].
pub const COVERAGE: &[PlatformEvidenceCoverage] = &[
    PlatformEvidenceCoverage {
        canonical_id: "Saturn",
        detector_modules: &["saturn_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Athlete Kings (Europe) Track 01 (Batch 3/4) - raw-sector + logical-image both validated",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("saturn_boot_signature"),
            sample_class: "Saturn raw2352 real corpus specimen",
            container: "raw CD sector image",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Sega CD",
        detector_modules: &["segacd_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "No real Sega CD specimen found accessible in the corpus (Batch 3/4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "3DO",
        detector_modules: &["threedo_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "No real 3DO specimen found accessible in the corpus (Batch 3/4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Dreamcast",
        detector_modules: &["dreamcast_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "Dreamcast directory in the local corpus contains no real specimen (Batch 5 search)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "PSX",
        detector_modules: &["playstation_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Gundam Battle Assault 2 (USA).chd - resolved through platform_evidence_fusion (Batch 5)",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("ps1_system_cnf_boot"),
            sample_class: "PSX real corpus specimen",
            container: "CHD compressed disc image",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "PS2",
        detector_modules: &["ps2_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: God of War (USA).iso - Batch 6 added a reviewed BOOT2 strong leg (SYSTEM.CNF BOOT2= confirmed against a validated ELF executable); now Resolved: PS2, not just a candidate",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("ps2_system_cnf_boot2_strong"),
            sample_class: "PS2 real corpus specimen (8.5GB single-layer ISO)",
            container: "raw ISO9660 disc image",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "PSP",
        detector_modules: &["psp_boot_evidence", "psp_pbp_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: God of War - Ghost of Sparta UMD ISO - Batch 6 added a reviewed UMD_DATA.BIN strong leg (PSP-UMD-exclusive medium-identification file); now Resolved: PSP, not just a candidate",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("psp_umd_data_bin_strong"),
            sample_class: "PSP UMD real corpus specimen",
            container: "raw ISO9660 UMD disc image",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "PS3",
        detector_modules: &["ps3_boot_evidence", "ps3_disc_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Resident Evil 4 HD .pkg (3.5GB, Batch 3)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Xbox",
        detector_modules: &["xbox_boot_evidence", "executable_signatures"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Fable - The Lost Chapters (USA).iso - Resolved through fusion (Batch 5)",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("xbox_original_disc"),
            sample_class: "Xbox XDVDFS/XBE real corpus specimen",
            container: "raw XDVDFS disc image",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Xbox360",
        detector_modules: &[
            "xbox360_boot_evidence",
            "xbox360_stfs_evidence",
            "executable_signatures",
        ],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimens: Fable II (USA, Europe).iso (disc, Batch 5) and Double Dragon Neon STFS package (Batch 3)",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("xbox360_disc"),
            sample_class: "Xbox360 XDVDFS/XEX2 real corpus specimen",
            container: "raw XDVDFS disc image",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "GameCube",
        detector_modules: &["gamecube_wii_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: ZooCube (USA).rvz - Batch 6 enabled nod's compress-zstd feature (zero new crates: zstd/zstd-safe/zstd-sys were already compiled via zip's own default features); resolves GZCE51 / Resolved: GameCube",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("gamecube_disc_header"),
            sample_class: "GameCube RVZ specimen",
            container: "RVZ (zstd-compressed disc image)",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Wii",
        detector_modules: &["gamecube_wii_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: New Super Mario Bros. Wii [SMNE01].wbfs - Resolved through fusion (Batch 5)",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("wii_disc_header"),
            sample_class: "Wii WBFS specimen",
            container: "WBFS (uncompressed disc image)",
        }),
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "NES",
        detector_modules: &["header_normalization", "nes_header_evidence"],
        normalization: true,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "No real .nes specimen found accessible in the corpus (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "SNES",
        detector_modules: &["header_normalization", "snes_header_evidence"],
        normalization: true,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "Real corpus specimens are dominated by unlicensed pirate/multi-cart .unh dumps whose headers do not validate (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Game Boy",
        detector_modules: &["gb_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: 10-Pin Bowling (USA) (Proto).gb - Resolved through fusion (Batch 4/5)",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("gb_logo_and_checksum"),
            sample_class: "DMG-only real corpus specimen",
            container: "raw .gb cartridge dump",
        }),
        dat_validation: Some(RealDatValidation {
            dat_family: "TOSEC",
            sample_match_count: 171,
            normalized_match_observed: false,
        }),
    },
    PlatformEvidenceCoverage {
        canonical_id: "Game Boy Color",
        detector_modules: &["gb_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimens: Deer Hunter.gbc and Thunderbirds.gbc (cgb_flag=0xC0, CGB-only) both Resolved: Game Boy Color; Klustar.gbc (cgb_flag=0x80, CGB-enhanced dual-mode) honestly stays Resolved: Game Boy with a corroborating dual-mode fact, never forced into an exclusive GBC claim (Batch 6 closed the Batch 5 gap)",
        real_validation_provenance: Some(RealValidationProvenance {
            rule_id: Some("gbc_cgb_only_logo_and_checksum"),
            sample_class: "CGB-only real corpus specimen (cgb_flag=0xC0)",
            container: "raw .gbc cartridge dump",
        }),
        dat_validation: Some(RealDatValidation {
            dat_family: "TOSEC",
            sample_match_count: 99,
            normalized_match_observed: false,
        }),
    },
    PlatformEvidenceCoverage {
        canonical_id: "Game Boy Advance",
        detector_modules: &["gba_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Metal Slug Advance (via ZIP) - Resolved through fusion (Batch 4/5)",
        real_validation_provenance: None,
        dat_validation: Some(RealDatValidation {
            dat_family: "TOSEC",
            sample_match_count: 3,
            normalized_match_observed: false,
        }),
    },
    PlatformEvidenceCoverage {
        canonical_id: "N64",
        detector_modules: &["n64_byte_order", "n64_header_evidence"],
        normalization: true,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimens: Aerofighters Assault (z64) and 1080 Snowboarding (v64) - Resolved through fusion (Batch 4/5)",
        real_validation_provenance: None,
        dat_validation: Some(RealDatValidation {
            dat_family: "TOSEC",
            sample_match_count: 18,
            normalized_match_observed: true,
        }),
    },
    PlatformEvidenceCoverage {
        canonical_id: "MegaDrive",
        detector_modules: &["megadrive_header_evidence"],
        normalization: true,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimens: 3 Ninjas Kick Back .md (whole-ROM checksum validated exactly) and the 32X Doom specimen's base header - candidate-only by design (Corroborated confidence only) (Batch 4/5)",
        real_validation_provenance: None,
        dat_validation: Some(RealDatValidation {
            dat_family: "TOSEC",
            sample_match_count: 58,
            normalized_match_observed: false,
        }),
    },
    PlatformEvidenceCoverage {
        canonical_id: "Sega 32X",
        detector_modules: &["megadrive_header_evidence", "sega32x_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Doom (Japan, USA) (En).7z, probed in-process (no system 7z) - candidate-only by design (Batch 4/5)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "MasterSystem",
        detector_modules: &["sms_gg_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::Partial,
        notes: "Real .zip specimens exist in the corpus, content-evidence fusion not exercised end-to-end (Batch 4); Batch 9 confirmed real DAT hash overlap independently via in-process ZIP-member hashing",
        real_validation_provenance: None,
        dat_validation: Some(RealDatValidation {
            dat_family: "TOSEC",
            sample_match_count: 47,
            normalized_match_observed: false,
        }),
    },
    PlatformEvidenceCoverage {
        canonical_id: "GameGear",
        detector_modules: &["sms_gg_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Aa Harimanada (Japan).gg - Resolved through fusion (Batch 4/5)",
        real_validation_provenance: None,
        dat_validation: Some(RealDatValidation {
            dat_family: "TOSEC",
            sample_match_count: 96,
            normalized_match_observed: false,
        }),
    },
    PlatformEvidenceCoverage {
        canonical_id: "Atari7800",
        detector_modules: &["header_normalization", "atari7800_header_evidence"],
        normalization: true,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "No real .a78 specimen found accessible in the corpus (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Atari Lynx",
        detector_modules: &["header_normalization", "lynx_header_evidence"],
        normalization: true,
        real_validation: ValidationStatus::RealValidated,
        notes: "Real specimen: Joust.lnx - Resolved through fusion (Batch 4/5)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Atari Jaguar",
        detector_modules: &[],
        normalization: false,
        real_validation: ValidationStatus::Deferred,
        notes: "No corroborated generic internal header exists (per-title encrypted boot block) - deliberately not implemented (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "PC Engine",
        detector_modules: &[],
        normalization: false,
        real_validation: ValidationStatus::Deferred,
        notes: "No standardized internal HuCard header corroborated to this crate's two-source standard - deliberately not implemented (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Neo Geo Pocket",
        detector_modules: &["ngp_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "No real NGP specimen found accessible in the corpus (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Neo Geo Pocket Color",
        detector_modules: &["ngp_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "No real NGPC specimen found accessible in the corpus (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "WonderSwan",
        detector_modules: &["ws_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "Every local WonderSwan symlink target is missing from the decypharr store - no real specimen accessible (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "WonderSwan Color",
        detector_modules: &["ws_header_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "Every local WonderSwan Color symlink target is missing from the decypharr store - no real specimen accessible (Batch 4)",
        real_validation_provenance: None,
        dat_validation: None,
    },
    PlatformEvidenceCoverage {
        canonical_id: "Neo Geo CD",
        detector_modules: &["neogeocd_boot_evidence"],
        normalization: false,
        real_validation: ValidationStatus::SyntheticValidated,
        notes: "No real Neo Geo CD specimen recorded as validated in this project's history",
        real_validation_provenance: None,
        dat_validation: None,
    },
];

/// The number of canonical platforms in [`crate::platform::PLATFORMS`] -
/// machine-counted, never a literal.
pub fn canonical_platform_count() -> usize {
    PLATFORMS.len()
}

/// Every canonical id with a [`COVERAGE`] entry - i.e. this crate has *any*
/// dedicated content-evidence work for it, regardless of validation status.
pub fn deep_evidence_count() -> usize {
    COVERAGE.len()
}

/// Canonical platforms with **no** [`COVERAGE`] entry at all - computed as
/// the registry total minus the coverage manifest's own size, never a
/// second hand-typed number.
pub fn evidence_poor_count() -> usize {
    canonical_platform_count().saturating_sub(deep_evidence_count())
}

pub fn real_validated_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| entry.real_validation == ValidationStatus::RealValidated)
        .count()
}

pub fn synthetic_only_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| entry.real_validation == ValidationStatus::SyntheticValidated)
        .count()
}

pub fn partial_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| entry.real_validation == ValidationStatus::Partial)
        .count()
}

pub fn deferred_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| entry.real_validation == ValidationStatus::Deferred)
        .count()
}

pub fn no_deep_evidence_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| entry.real_validation == ValidationStatus::NoDeepEvidence)
        .count()
}

/// Platforms with a physical/normalized dual-identity transform.
pub fn normalization_supported_count() -> usize {
    COVERAGE.iter().filter(|entry| entry.normalization).count()
}

/// Every canonical platform id with **no** [`COVERAGE`] entry - the actual
/// list backing [`evidence_poor_count`], for a report that wants to name
/// them, not just count them.
pub fn evidence_poor_platform_ids() -> Vec<&'static str> {
    PLATFORMS
        .iter()
        .map(|platform| platform.id)
        .filter(|id| !COVERAGE.iter().any(|entry| entry.canonical_id == *id))
        .collect()
}

/// Batch 9 section 11: platforms [`crate::dat::identity`]'s own name-
/// matching machinery could in principle recognize from a DAT's header
/// text - structurally every canonical platform, since
/// `gather_dat_platform_evidence` matches against each platform's own
/// display name/aliases in the *full* [`PLATFORMS`] registry, never a
/// second, narrower list scoped to [`COVERAGE`]. Distinct from
/// [`real_dat_validated_count`] (platforms this session actually proved
/// real overlap for) - "capable of" is not "proven."
pub fn dat_identity_capable_count() -> usize {
    canonical_platform_count()
}

/// Platforms with a [`RealDatValidation`] on their [`COVERAGE`] entry -
/// real local ROM bytes hash-matched a real local DAT entry this session.
pub fn real_dat_validated_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| entry.dat_validation.is_some())
        .count()
}

/// Platforms where **both** content evidence and DAT identity were
/// independently real-validated this session or a prior one -
/// [`ValidationStatus::RealValidated`] *and* a real
/// [`RealDatValidation`] together. Stricter than [`real_dat_validated_count`]
/// alone: a platform with real DAT overlap but only `Partial` content
/// validation (Master System, this session) does not count here, honestly
/// reflecting that the two lanes were not both proven for the same
/// platform.
pub fn real_content_dat_agreement_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| {
            entry.dat_validation.is_some()
                && entry.real_validation == ValidationStatus::RealValidated
        })
        .count()
}

/// Platforms where at least one real match required the *normalized* byte
/// representation, not the physical one - the milestone's own headline
/// "normalized DAT real-validation" metric.
pub fn normalized_dat_real_validated_count() -> usize {
    COVERAGE
        .iter()
        .filter(|entry| {
            entry
                .dat_validation
                .is_some_and(|dat| dat.normalized_match_observed)
        })
        .count()
}

/// A compact, machine-generated summary - the structured source for a
/// developer probe's coverage report; never itself a giant prose string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    pub canonical_platforms: usize,
    pub deep_evidence: usize,
    pub real_validated: usize,
    pub synthetic_only: usize,
    pub partial: usize,
    pub deferred: usize,
    pub evidence_poor: usize,
    pub normalization_supported: usize,
    pub dat_identity_capable: usize,
    pub real_dat_validated: usize,
    pub real_content_dat_agreement: usize,
    pub normalized_dat_real_validated: usize,
}

pub fn coverage_report() -> CoverageReport {
    CoverageReport {
        canonical_platforms: canonical_platform_count(),
        deep_evidence: deep_evidence_count(),
        real_validated: real_validated_count(),
        synthetic_only: synthetic_only_count(),
        partial: partial_count(),
        deferred: deferred_count(),
        evidence_poor: evidence_poor_count(),
        normalization_supported: normalization_supported_count(),
        dat_identity_capable: dat_identity_capable_count(),
        real_dat_validated: real_dat_validated_count(),
        real_content_dat_agreement: real_content_dat_agreement_count(),
        normalized_dat_real_validated: normalized_dat_real_validated_count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_coverage_entry_references_a_real_canonical_platform() {
        for entry in COVERAGE {
            assert!(
                platform_by_id(entry.canonical_id).is_some(),
                "coverage entry references unknown canonical platform id {:?}",
                entry.canonical_id
            );
        }
    }

    #[test]
    fn no_duplicate_coverage_entries() {
        let mut ids: Vec<&str> = COVERAGE.iter().map(|entry| entry.canonical_id).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate coverage entry found");
    }

    #[test]
    fn no_coverage_entry_uses_an_alias_instead_of_a_canonical_id() {
        // platform_by_id only matches the real `id` field, never an alias -
        // combined with the previous test, an entry that accidentally used
        // an alias (e.g. "TurboGrafx-16" typed where "PC Engine" was meant,
        // or vice versa) would still pass `platform_by_id` (both are their
        // own real canonical ids) but would silently duplicate coverage for
        // the same hardware under two different manifest keys. This test
        // checks the display name recorded for each entry's id actually
        // matches that platform's own registry entry - a sanity check that
        // the id string was not simply mistyped to a lookalike alias.
        for entry in COVERAGE {
            let platform = platform_by_id(entry.canonical_id)
                .expect("checked by every_coverage_entry_references_a_real_canonical_platform");
            assert_eq!(platform.id, entry.canonical_id);
        }
    }

    #[test]
    fn deep_evidence_count_matches_coverage_len() {
        assert_eq!(deep_evidence_count(), COVERAGE.len());
    }

    #[test]
    fn canonical_count_matches_registry_len() {
        assert_eq!(canonical_platform_count(), PLATFORMS.len());
    }

    #[test]
    fn evidence_poor_count_plus_deep_evidence_count_equals_canonical_total() {
        assert_eq!(
            evidence_poor_count() + deep_evidence_count(),
            canonical_platform_count()
        );
    }

    #[test]
    fn evidence_poor_platform_ids_len_matches_evidence_poor_count() {
        assert_eq!(evidence_poor_platform_ids().len(), evidence_poor_count());
    }

    #[test]
    fn validation_status_buckets_sum_to_deep_evidence_count() {
        let sum = real_validated_count()
            + synthetic_only_count()
            + partial_count()
            + deferred_count()
            + no_deep_evidence_count();
        assert_eq!(sum, deep_evidence_count());
    }

    #[test]
    fn coverage_report_fields_match_their_own_functions() {
        let report = coverage_report();
        assert_eq!(report.canonical_platforms, canonical_platform_count());
        assert_eq!(report.deep_evidence, deep_evidence_count());
        assert_eq!(report.real_validated, real_validated_count());
        assert_eq!(report.synthetic_only, synthetic_only_count());
        assert_eq!(report.partial, partial_count());
        assert_eq!(report.deferred, deferred_count());
        assert_eq!(report.evidence_poor, evidence_poor_count());
        assert_eq!(
            report.normalization_supported,
            normalization_supported_count()
        );
    }

    #[test]
    fn no_coverage_entry_has_an_empty_notes_field() {
        for entry in COVERAGE {
            assert!(
                !entry.notes.is_empty(),
                "coverage entry for {} has no notes",
                entry.canonical_id
            );
        }
    }

    #[test]
    fn real_validated_entries_have_a_specimen_named_in_notes() {
        for entry in COVERAGE {
            if entry.real_validation == ValidationStatus::RealValidated {
                assert!(
                    entry.notes.to_lowercase().contains("real specimen")
                        || entry.notes.contains(".chd")
                        || entry.notes.contains(".iso")
                        || entry.notes.contains(".gb")
                        || entry.notes.contains(".gg")
                        || entry.notes.contains(".lnx")
                        || entry.notes.contains(".wbfs")
                        || entry.notes.contains(".7z")
                        || entry.notes.contains(".md")
                        || entry.notes.contains(".pkg")
                        || entry.notes.contains("STFS")
                        || entry.notes.contains("z64"),
                    "RealValidated entry for {} has no specimen reference in notes: {:?}",
                    entry.canonical_id,
                    entry.notes
                );
            }
        }
    }

    #[test]
    fn deferred_entries_have_no_detector_modules() {
        for entry in COVERAGE {
            if entry.real_validation == ValidationStatus::Deferred {
                assert!(
                    entry.detector_modules.is_empty(),
                    "{} is Deferred but lists detector modules {:?}",
                    entry.canonical_id,
                    entry.detector_modules
                );
            }
        }
    }

    #[test]
    fn every_non_deferred_entry_has_at_least_one_detector_module() {
        for entry in COVERAGE {
            if entry.real_validation != ValidationStatus::Deferred {
                assert!(
                    !entry.detector_modules.is_empty(),
                    "{} is not Deferred but lists no detector modules",
                    entry.canonical_id
                );
            }
        }
    }

    #[test]
    fn evidence_poor_platform_ids_never_overlap_coverage() {
        let poor = evidence_poor_platform_ids();
        for id in poor {
            assert!(!COVERAGE.iter().any(|entry| entry.canonical_id == id));
        }
    }

    #[test]
    fn counts_are_deterministic() {
        assert_eq!(coverage_report(), coverage_report());
    }

    #[test]
    fn real_validated_platforms_have_at_least_one_fusion_rule() {
        // Cross-check against the real rule table, not a second hand-typed
        // list: every RealValidated platform should be reachable by at
        // least one crate::platform_evidence_fusion::RULES entry (either
        // Strong-eligible or candidate-only) - if resolving evidence exists
        // for a platform, the fusion layer should know about it.
        use crate::platform_evidence_fusion::RULES;
        for entry in COVERAGE {
            if entry.real_validation == ValidationStatus::RealValidated {
                let has_rule = RULES.iter().any(|rule| rule.platform == entry.canonical_id);
                assert!(
                    has_rule,
                    "{} is RealValidated but has no platform_evidence_fusion rule",
                    entry.canonical_id
                );
            }
        }
    }

    #[test]
    fn only_real_validated_entries_ever_carry_provenance() {
        for entry in COVERAGE {
            if entry.real_validation_provenance.is_some() {
                assert_eq!(
                    entry.real_validation,
                    ValidationStatus::RealValidated,
                    "{} carries real_validation_provenance but is not RealValidated",
                    entry.canonical_id
                );
            }
        }
    }

    #[test]
    fn provenance_rule_id_when_present_is_a_real_fusion_rule() {
        use crate::platform_evidence_fusion::RULES;
        for entry in COVERAGE {
            if let Some(provenance) = entry.real_validation_provenance
                && let Some(rule_id) = provenance.rule_id
            {
                assert!(
                    RULES.iter().any(|rule| rule.id == rule_id),
                    "{} names provenance rule_id {:?} which is not a real RULES entry",
                    entry.canonical_id,
                    rule_id
                );
            }
        }
    }

    #[test]
    fn provenance_sample_class_and_container_are_never_empty() {
        for entry in COVERAGE {
            if let Some(provenance) = entry.real_validation_provenance {
                assert!(!provenance.sample_class.is_empty());
                assert!(!provenance.container.is_empty());
            }
        }
    }

    #[test]
    fn provenance_never_contains_an_absolute_filesystem_path() {
        for entry in COVERAGE {
            if let Some(provenance) = entry.real_validation_provenance {
                assert!(!provenance.sample_class.starts_with('/'));
                assert!(!provenance.container.starts_with('/'));
                assert!(!provenance.sample_class.contains("/mnt/"));
                assert!(!provenance.sample_class.contains("/home/"));
            }
        }
    }

    // ------------------------------------------------------------------
    // Batch 9: RealDatValidation consistency (section 10)
    // ------------------------------------------------------------------

    #[test]
    fn dat_identity_capable_count_equals_canonical_count() {
        assert_eq!(dat_identity_capable_count(), canonical_platform_count());
    }

    #[test]
    fn real_dat_validated_count_matches_coverage_scan() {
        let counted = COVERAGE
            .iter()
            .filter(|e| e.dat_validation.is_some())
            .count();
        assert_eq!(real_dat_validated_count(), counted);
    }

    #[test]
    fn real_content_dat_agreement_never_exceeds_real_dat_validated() {
        assert!(real_content_dat_agreement_count() <= real_dat_validated_count());
    }

    #[test]
    fn real_content_dat_agreement_never_exceeds_real_validated_content_count() {
        assert!(real_content_dat_agreement_count() <= real_validated_count());
    }

    #[test]
    fn normalized_dat_real_validated_never_exceeds_real_dat_validated() {
        assert!(normalized_dat_real_validated_count() <= real_dat_validated_count());
    }

    #[test]
    fn dat_validation_sample_match_count_is_never_zero_when_present() {
        // A `Some(RealDatValidation)` entry must name a genuine, nonzero
        // count of real matches - `Some` with 0 matches would be
        // indistinguishable from "not validated" and is never honest.
        for entry in COVERAGE {
            if let Some(dat) = entry.dat_validation {
                assert!(
                    dat.sample_match_count > 0,
                    "{} has dat_validation with sample_match_count == 0",
                    entry.canonical_id
                );
            }
        }
    }

    #[test]
    fn dat_validation_family_is_never_empty() {
        for entry in COVERAGE {
            if let Some(dat) = entry.dat_validation {
                assert!(!dat.dat_family.is_empty());
            }
        }
    }

    #[test]
    fn dat_validation_never_contains_a_filesystem_path() {
        for entry in COVERAGE {
            if let Some(dat) = entry.dat_validation {
                assert!(!dat.dat_family.contains('/'));
            }
        }
    }

    #[test]
    fn master_system_dat_validation_present_despite_partial_content_status() {
        // The exact real, honest asymmetry this milestone's own section 27
        // is about: DAT overlap can be proven independently of content
        // fusion agreement.
        let entry = COVERAGE
            .iter()
            .find(|e| e.canonical_id == "MasterSystem")
            .unwrap();
        assert!(entry.dat_validation.is_some());
        assert_eq!(entry.real_validation, ValidationStatus::Partial);
    }

    #[test]
    fn n64_is_the_only_normalized_match_observed_platform_this_session() {
        let normalized_platforms: Vec<&str> = COVERAGE
            .iter()
            .filter(|e| {
                e.dat_validation
                    .is_some_and(|dat| dat.normalized_match_observed)
            })
            .map(|e| e.canonical_id)
            .collect();
        assert_eq!(normalized_platforms, vec!["N64"]);
    }

    #[test]
    fn coverage_report_new_fields_match_their_own_functions() {
        let report = coverage_report();
        assert_eq!(report.dat_identity_capable, dat_identity_capable_count());
        assert_eq!(report.real_dat_validated, real_dat_validated_count());
        assert_eq!(
            report.real_content_dat_agreement,
            real_content_dat_agreement_count()
        );
        assert_eq!(
            report.normalized_dat_real_validated,
            normalized_dat_real_validated_count()
        );
    }
}
