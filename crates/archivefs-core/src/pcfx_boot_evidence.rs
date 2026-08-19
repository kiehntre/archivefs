//! Pure, read-only PC-FX boot-sector evidence: the two magic strings
//! Mednafen itself checks to recognize a PC-FX disc.
//!
//! # Format verified, not assumed
//!
//! Taken directly from Mednafen's own PC-FX core, `TestMagicCD()`
//! (`https://github.com/OpenEmu/Mednafen-Core/blob/master/mednafen/pcfx/pcfx.cpp`,
//! a real, actively-used, production-grade PC-FX emulator - exactly the
//! kind of source this task calls for):
//!
//! ```text
//! if(!strncmp("PC-FX:Hu_CD-ROM", sector, strlen("PC-FX:Hu_CD-ROM")))
//!     return true;
//! else if(!strncmp(sector + 64, "PPPPHHHHOOOOTTTTOOOO____CCCCDDDD", 32))
//!     return true;
//! ```
//!
//! Checked against the first 2048-byte sector of the disc's first data
//! track (the same "first sector of the logical data stream" convention
//! [`crate::dreamcast_boot_evidence`]/[`crate::saturn_boot_evidence`]
//! already use for their own boot-area reads).
//!
//! # Collision safety
//!
//! - Neither magic string is disclosed by Mednafen's own source to encode
//!   a serial/catalog/product code, region, or version - this module never
//!   emits a `ProductCode` fact.
//! - **Never conflated with PC Engine CD/TurboGrafx-CD.** All three (PC
//!   Engine CD, TurboGrafx-CD, PC-FX) are NEC optical platforms, but the
//!   `"PC-FX:Hu_CD-ROM"` string is PC-FX-specific - it is not the generic
//!   `"PC Engine CD-ROM SYSTEM"`-style string PC Engine CD/TurboGrafx-CD
//!   discs carry (a different, unrelated boot string this module does not
//!   check for and never matches against). This module makes no claim
//!   about, and shares no evidence value with, PC Engine CD detection.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const PCFX_PRIMARY_MAGIC: &[u8] = b"PC-FX:Hu_CD-ROM";
pub const PCFX_SECONDARY_MAGIC: &[u8] = b"PPPPHHHHOOOOTTTTOOOO____CCCCDDDD";
const PCFX_SECONDARY_MAGIC_OFFSET: usize = 64;

/// Bound on the sector prefix this module ever looks at - a real CD-ROM
/// sector is 2048 bytes; this is exactly that, never more.
pub const PCFX_BOOT_SECTOR_BYTES: usize = 2048;

/// What was observed about a PC-FX boot-sector candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PcfxBootSectorFact {
    pub primary_magic_present: bool,
    pub secondary_magic_present: bool,
}

impl PcfxBootSectorFact {
    pub fn any_magic_present(&self) -> bool {
        self.primary_magic_present || self.secondary_magic_present
    }
}

/// Checks `sector` (the first sector of a disc's first data track) for
/// either magic Mednafen itself checks. Never panics on a short buffer -
/// a magic simply cannot be present if there are not enough bytes.
pub fn parse_pcfx_boot_sector(sector: &[u8]) -> PcfxBootSectorFact {
    let primary_magic_present = sector.len() >= PCFX_PRIMARY_MAGIC.len()
        && &sector[..PCFX_PRIMARY_MAGIC.len()] == PCFX_PRIMARY_MAGIC;
    let secondary_end = PCFX_SECONDARY_MAGIC_OFFSET + PCFX_SECONDARY_MAGIC.len();
    let secondary_magic_present = sector.len() >= secondary_end
        && &sector[PCFX_SECONDARY_MAGIC_OFFSET..secondary_end] == PCFX_SECONDARY_MAGIC;
    PcfxBootSectorFact {
        primary_magic_present,
        secondary_magic_present,
    }
}

/// Neutral evidence: `Strong` `BootStructure` = `"PC-FX:Hu_CD-ROM"` for the
/// primary magic, or `"PPPPHHHHOOOOTTTTOOOO____CCCCDDDD"` for the
/// secondary one - both, if both are present (mirrors
/// [`crate::observe_content_evidence`]'s "preserve every fact" discipline
/// rather than picking one).
pub fn observe_pcfx_evidence(fact: &PcfxBootSectorFact) -> Vec<ContentEvidence> {
    let mut evidence = Vec::new();
    if fact.primary_magic_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "PC-FX:Hu_CD-ROM",
            ContentEvidenceConfidence::Strong,
            "PC-FX primary boot-sector magic present at sector offset 0 - verified against Mednafen's own PC-FX core",
        ));
    }
    if fact.secondary_magic_present {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "PPPPHHHHOOOOTTTTOOOO____CCCCDDDD",
            ContentEvidenceConfidence::Strong,
            "PC-FX secondary boot-sector magic present at sector offset 64 - verified against Mednafen's own PC-FX core",
        ));
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector_with_primary_magic() -> Vec<u8> {
        let mut sector = vec![0u8; PCFX_BOOT_SECTOR_BYTES];
        sector[..PCFX_PRIMARY_MAGIC.len()].copy_from_slice(PCFX_PRIMARY_MAGIC);
        sector
    }

    fn sector_with_secondary_magic() -> Vec<u8> {
        let mut sector = vec![0u8; PCFX_BOOT_SECTOR_BYTES];
        sector
            [PCFX_SECONDARY_MAGIC_OFFSET..PCFX_SECONDARY_MAGIC_OFFSET + PCFX_SECONDARY_MAGIC.len()]
            .copy_from_slice(PCFX_SECONDARY_MAGIC);
        sector
    }

    #[test]
    fn primary_magic_is_detected() {
        let fact = parse_pcfx_boot_sector(&sector_with_primary_magic());
        assert!(fact.primary_magic_present);
        assert!(!fact.secondary_magic_present);
        assert!(fact.any_magic_present());
    }

    #[test]
    fn secondary_magic_is_detected() {
        let fact = parse_pcfx_boot_sector(&sector_with_secondary_magic());
        assert!(!fact.primary_magic_present);
        assert!(fact.secondary_magic_present);
        assert!(fact.any_magic_present());
    }

    #[test]
    fn neither_magic_present_on_unrelated_bytes() {
        let sector = vec![0u8; PCFX_BOOT_SECTOR_BYTES];
        let fact = parse_pcfx_boot_sector(&sector);
        assert!(!fact.any_magic_present());
    }

    #[test]
    fn short_buffer_fails_closed_not_panic() {
        let fact = parse_pcfx_boot_sector(&[0u8; 4]);
        assert!(!fact.any_magic_present());
    }

    #[test]
    fn empty_buffer_fails_closed_not_panic() {
        let fact = parse_pcfx_boot_sector(&[]);
        assert!(!fact.any_magic_present());
    }

    #[test]
    fn truncated_secondary_magic_is_not_recognized() {
        let mut sector = vec![0u8; PCFX_SECONDARY_MAGIC_OFFSET + 10];
        sector[PCFX_SECONDARY_MAGIC_OFFSET..].copy_from_slice(&PCFX_SECONDARY_MAGIC[..10]);
        let fact = parse_pcfx_boot_sector(&sector);
        assert!(!fact.secondary_magic_present);
    }

    #[test]
    fn primary_magic_yields_strong_boot_structure_evidence() {
        let fact = parse_pcfx_boot_sector(&sector_with_primary_magic());
        let evidence = observe_pcfx_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, ContentEvidenceKind::BootStructure);
        assert_eq!(evidence[0].value, "PC-FX:Hu_CD-ROM");
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn both_magics_present_yields_both_facts() {
        let mut sector = sector_with_primary_magic();
        sector
            [PCFX_SECONDARY_MAGIC_OFFSET..PCFX_SECONDARY_MAGIC_OFFSET + PCFX_SECONDARY_MAGIC.len()]
            .copy_from_slice(PCFX_SECONDARY_MAGIC);
        let fact = parse_pcfx_boot_sector(&sector);
        let evidence = observe_pcfx_evidence(&fact);
        assert_eq!(evidence.len(), 2);
    }

    #[test]
    fn no_magic_yields_no_evidence() {
        let fact = PcfxBootSectorFact::default();
        assert!(observe_pcfx_evidence(&fact).is_empty());
    }

    #[test]
    fn evidence_never_includes_product_code() {
        let fact = parse_pcfx_boot_sector(&sector_with_primary_magic());
        for item in observe_pcfx_evidence(&fact) {
            assert_ne!(item.kind, ContentEvidenceKind::ProductCode);
        }
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let fact = parse_pcfx_boot_sector(&sector_with_primary_magic());
        for item in observe_pcfx_evidence(&fact) {
            assert!(matches!(item.kind, ContentEvidenceKind::BootStructure));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let fact = parse_pcfx_boot_sector(&sector_with_primary_magic());
        assert_eq!(observe_pcfx_evidence(&fact), observe_pcfx_evidence(&fact));
    }
}
