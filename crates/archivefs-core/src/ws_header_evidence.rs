//! Pure, read-only Bandai WonderSwan / WonderSwan Color ROM footer evidence.
//!
//! # Format verified, not assumed
//!
//! Verified against the WSdev wiki's ROM header page
//! (`https://ws.nesdev.org/wiki/ROM_header`), the NESdev-family community
//! reference for this format:
//!
//! ```text
//! Relative to the LAST 10 bytes of the ROM (offset len-10):
//! [0]     developer_id
//! [1]     minimum_system     0x00 = WonderSwan (mono), 0x01 = WonderSwan
//!                            Color
//! [2]     cart_id            developer-assigned catalog number
//! [3]     (undocumented by the source this pass corroborated - not decoded)
//! [4]     rom_size_code
//! [5]     sram_eeprom_size_code
//! [6]     capability_flags
//! [7]     rtc_present
//! [8..10] checksum           2 bytes, little-endian - the cumulative
//!                            8-bit-wrapping sum of every byte in the ROM,
//!                            including the header, EXCEPT these two bytes
//!                            themselves
//! ```
//!
//! Byte 3 is left undecoded rather than guessed: the reference this pass
//! corroborated (a single source, not independently cross-checked to this
//! crate's usual two-source standard for a specific field meaning) did not
//! give its meaning.
//!
//! # Checksum validation is a separate, opt-in, whole-ROM operation
//!
//! Exactly like [`crate::megadrive_header_evidence::verify_megadrive_checksum`]:
//! validating the checksum needs the entire ROM, not the 10-byte footer
//! [`parse_ws_footer`] reads. [`verify_ws_checksum`] is the caller-invoked,
//! whole-ROM counterpart. It reuses
//! [`crate::cartridge_header::wrapping_sum_u16`] rather than a fourth
//! hand-rolled summing loop.

use crate::cartridge_header::wrapping_sum_u16;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const WS_FOOTER_BYTES: usize = 10;

const DEVELOPER_ID_OFFSET: usize = 0;
const MINIMUM_SYSTEM_OFFSET: usize = 1;
const CART_ID_OFFSET: usize = 2;
const ROM_SIZE_CODE_OFFSET: usize = 4;
const SRAM_EEPROM_SIZE_CODE_OFFSET: usize = 5;
const CAPABILITY_FLAGS_OFFSET: usize = 6;
const RTC_PRESENT_OFFSET: usize = 7;
const CHECKSUM_OFFSET: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsSystem {
    Mono,
    Color,
    /// A value this module does not recognise - reported honestly.
    Unknown(u8),
}

/// What a parsed WonderSwan/WonderSwan Color footer directly states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsFooterFact {
    pub developer_id: u8,
    pub minimum_system: WsSystem,
    pub cart_id: u8,
    pub rom_size_code: u8,
    pub sram_eeprom_size_code: u8,
    pub capability_flags: u8,
    pub rtc_present: bool,
    /// Declared checksum, as read - not independently validated by this
    /// function. See [`verify_ws_checksum`].
    pub checksum: u16,
}

/// Parses the last [`WS_FOOTER_BYTES`] bytes of `rom` as a WonderSwan
/// footer. `None` if `rom` is shorter than [`WS_FOOTER_BYTES`] - fails
/// closed, never a partial struct. Unlike a fixed-offset header, there is no
/// magic byte to validate here: the footer's mere presence at the end of
/// any ROM at least this long is not itself claimed as evidence - see
/// [`observe_ws_evidence`], which requires a validated checksum before
/// emitting anything.
pub fn parse_ws_footer(rom: &[u8]) -> Option<WsFooterFact> {
    if rom.len() < WS_FOOTER_BYTES {
        return None;
    }
    let footer = &rom[rom.len() - WS_FOOTER_BYTES..];
    let minimum_system = match footer[MINIMUM_SYSTEM_OFFSET] {
        0x00 => WsSystem::Mono,
        0x01 => WsSystem::Color,
        other => WsSystem::Unknown(other),
    };
    Some(WsFooterFact {
        developer_id: footer[DEVELOPER_ID_OFFSET],
        minimum_system,
        cart_id: footer[CART_ID_OFFSET],
        rom_size_code: footer[ROM_SIZE_CODE_OFFSET],
        sram_eeprom_size_code: footer[SRAM_EEPROM_SIZE_CODE_OFFSET],
        capability_flags: footer[CAPABILITY_FLAGS_OFFSET],
        rtc_present: footer[RTC_PRESENT_OFFSET] != 0,
        checksum: u16::from_le_bytes([footer[CHECKSUM_OFFSET], footer[CHECKSUM_OFFSET + 1]]),
    })
}

/// Computes the WonderSwan checksum: the 16-bit wrapping sum of every byte
/// in `rom` **except** the two checksum bytes themselves (the last two
/// bytes of the file). `None` if `rom` is shorter than [`WS_FOOTER_BYTES`].
pub fn compute_ws_checksum(rom: &[u8]) -> Option<u16> {
    if rom.len() < WS_FOOTER_BYTES {
        return None;
    }
    let checksum_start = rom.len() - 2;
    Some(wrapping_sum_u16(&rom[..checksum_start]))
}

/// Whether `rom`'s computed checksum matches its own declared footer
/// checksum.
pub fn verify_ws_checksum(rom: &[u8]) -> bool {
    let Some(fact) = parse_ws_footer(rom) else {
        return false;
    };
    compute_ws_checksum(rom) == Some(fact.checksum)
}

/// Neutral evidence: `Corroborated` `BootStructure` only when the checksum
/// validates (`verify_ws_checksum`) - since this format has no magic byte
/// to match, the checksum is the only structural fact this module can
/// actually confirm, so it stays at `Corroborated` rather than `Strong` (a
/// validating whole-file additive checksum is real but not as specific as a
/// multi-byte signature match). No evidence at all when it does not
/// validate, or when `rom` was too short to check.
pub fn observe_ws_evidence(rom: &[u8]) -> Vec<ContentEvidence> {
    let Some(fact) = parse_ws_footer(rom) else {
        return Vec::new();
    };
    if !verify_ws_checksum(rom) {
        return Vec::new();
    }
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        "WonderSwan footer checksum",
        ContentEvidenceConfidence::Corroborated,
        "WonderSwan/WonderSwan Color ROM footer checksum validated over the whole file",
    )];
    let system_label = match fact.minimum_system {
        WsSystem::Mono => Some("WonderSwan (mono)"),
        WsSystem::Color => Some("WonderSwan Color"),
        WsSystem::Unknown(_) => None,
    };
    if let Some(label) = system_label {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ContentSignature,
            label,
            ContentEvidenceConfidence::Weak,
            "minimum-system byte read from the WonderSwan footer - real, but not independently \
             corroborated to this crate's own two-source standard, so this stays Weak",
        ));
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_rom(payload_len: usize, minimum_system: u8) -> Vec<u8> {
        let mut rom = vec![0x42u8; payload_len];
        let mut footer = [0u8; WS_FOOTER_BYTES];
        footer[DEVELOPER_ID_OFFSET] = 0x01;
        footer[MINIMUM_SYSTEM_OFFSET] = minimum_system;
        footer[CART_ID_OFFSET] = 0x05;
        footer[ROM_SIZE_CODE_OFFSET] = 0x03;
        footer[SRAM_EEPROM_SIZE_CODE_OFFSET] = 0x01;
        footer[RTC_PRESENT_OFFSET] = 0;
        rom.extend_from_slice(&footer);
        // Now compute and patch in a valid checksum.
        let checksum = compute_ws_checksum(&rom).unwrap();
        let checksum_start = rom.len() - 2;
        rom[checksum_start..].copy_from_slice(&checksum.to_le_bytes());
        rom
    }

    #[test]
    fn too_short_fails_closed() {
        assert_eq!(parse_ws_footer(&[0u8; 5]), None);
    }

    #[test]
    fn empty_rom_fails_closed_not_panic() {
        assert_eq!(parse_ws_footer(&[]), None);
    }

    #[test]
    fn valid_footer_parses_every_field() {
        let rom = synthetic_rom(1024, 0x00);
        let fact = parse_ws_footer(&rom).unwrap();
        assert_eq!(fact.developer_id, 0x01);
        assert_eq!(fact.minimum_system, WsSystem::Mono);
        assert_eq!(fact.cart_id, 0x05);
        assert!(!fact.rtc_present);
    }

    #[test]
    fn color_minimum_system_is_decoded() {
        let rom = synthetic_rom(1024, 0x01);
        let fact = parse_ws_footer(&rom).unwrap();
        assert_eq!(fact.minimum_system, WsSystem::Color);
    }

    #[test]
    fn unrecognised_minimum_system_is_reported_honestly() {
        let rom = synthetic_rom(1024, 0x77);
        let fact = parse_ws_footer(&rom).unwrap();
        assert_eq!(fact.minimum_system, WsSystem::Unknown(0x77));
    }

    // ------------------------------------------------------------------
    // Checksum
    // ------------------------------------------------------------------

    #[test]
    fn synthetic_rom_checksum_is_self_consistent() {
        let rom = synthetic_rom(2048, 0x00);
        assert!(verify_ws_checksum(&rom));
    }

    #[test]
    fn corrupted_payload_invalidates_checksum() {
        let mut rom = synthetic_rom(2048, 0x00);
        rom[100] ^= 0xFF;
        assert!(!verify_ws_checksum(&rom));
    }

    #[test]
    fn corrupted_checksum_bytes_invalidate_checksum() {
        let mut rom = synthetic_rom(2048, 0x00);
        let len = rom.len();
        rom[len - 1] ^= 0xFF;
        assert!(!verify_ws_checksum(&rom));
    }

    #[test]
    fn checksum_computation_too_short_fails_closed() {
        assert_eq!(compute_ws_checksum(&[0u8; 3]), None);
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn valid_checksum_yields_corroborated_evidence() {
        let rom = synthetic_rom(2048, 0x00);
        let evidence = observe_ws_evidence(&rom);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Corroborated);
    }

    #[test]
    fn invalid_checksum_yields_no_evidence() {
        let mut rom = synthetic_rom(2048, 0x00);
        rom[100] ^= 0xFF;
        assert!(observe_ws_evidence(&rom).is_empty());
    }

    #[test]
    fn color_system_flag_yields_weak_content_signature() {
        let rom = synthetic_rom(2048, 0x01);
        let evidence = observe_ws_evidence(&rom);
        let system = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ContentSignature)
            .unwrap();
        assert_eq!(system.value, "WonderSwan Color");
        assert_eq!(system.confidence, ContentEvidenceConfidence::Weak);
    }

    #[test]
    fn too_short_rom_yields_no_evidence_not_panic() {
        assert!(observe_ws_evidence(&[0u8; 3]).is_empty());
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let rom = synthetic_rom(2048, 0x01);
        for item in observe_ws_evidence(&rom) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ContentSignature
            ));
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let rom = synthetic_rom(2048, 0x00);
        assert_eq!(parse_ws_footer(&rom), parse_ws_footer(&rom));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let rom = synthetic_rom(2048, 0x00);
        let before = rom.clone();
        let _ = parse_ws_footer(&rom);
        assert_eq!(rom, before);
    }
}
