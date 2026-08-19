//! Pure, read-only SNES/Super Famicom internal-header candidate evidence.
//!
//! Unlike a fixed-offset header (NES, Game Boy, GBA), the SNES internal
//! header can legitimately live at one of three different offsets depending
//! on the cartridge's memory map (`LoROM`/`HiROM`/`ExHiROM`) - there is no
//! single "the" header offset to check. This module never assumes ASCII
//! title bytes at one guessed offset prove anything; it checks every
//! candidate offset and only trusts one whose checksum/complement pair
//! actually validates - see [`best_snes_header_candidate`].
//!
//! # Format verified, not assumed
//!
//! Verified against the SNESdev wiki's ROM header page
//! (`https://snes.nesdev.org/wiki/ROM_header`), the same community-
//! maintained reference family as the NESdev wiki this crate's
//! [`crate::nes_header_evidence`] already cites:
//!
//! ```text
//! Relative to a candidate base offset B ($7FC0 LoROM / $FFC0 HiROM /
//! $40FFC0 ExHiROM):
//! [B+0x00..0x15]  title                21 bytes, ASCII
//! [B+0x15]        map_mode             %001smmmm: s=speed (1=fast),
//!                                       mmmm: 0=LoROM, 1=HiROM, 5=ExHiROM
//! [B+0x16]        cartridge_type       chipset/coprocessor + battery flag
//! [B+0x17]        rom_size             1 << N KiB
//! [B+0x18]        ram_size             1 << N KiB
//! [B+0x19]        destination_code     region
//! [B+0x1A]        developer_id
//! [B+0x1B]        version
//! [B+0x1C..0x1E]  checksum_complement  2 bytes, LE
//! [B+0x1E..0x20]  checksum             2 bytes, LE
//! ```
//!
//! `checksum ^ checksum_complement == 0xFFFF` for a genuine SNES header -
//! the SNESdev wiki's own stated invariant ("the checksum and complement sum
//! to $FFFF").
//!
//! # Copier headers are a separate, already-handled concern
//!
//! A 512-byte SNES copier header (recognised only by
//! [`crate::header_normalization::recognize_snes_copier_candidate`]) is not
//! this module's concern - strip it first (via
//! [`crate::header_normalization::strip_known_header`]) and hand this
//! module the *payload* bytes. A copier-headered dump and its headerless
//! twin both then produce the identical [`SnesHeaderFact`] from this module,
//! because both are being read at the same SNES-CPU-relative offsets within
//! the payload - this is what keeps "copier-headered" and "headerless"
//! mapping to the same normalized content view while their physical byte
//! identity (with or without the 512-byte header) stays separate.
//!
//! # Never "proven" from title bytes alone
//!
//! A candidate offset with a plausible-looking ASCII title but an invalid
//! checksum/complement pair is not promoted to any evidence at all -
//! matching this crate's collision-safety discipline
//! ([`crate::header_normalization::HeaderNormalizationKind::SnesCopier512`]'s
//! own "size rule alone is Weak, never proof" precedent). See
//! [`best_snes_header_candidate`].

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

/// The 21-byte title field plus every fixed field through the 2-byte
/// checksum - `0x20` (32) bytes total, relative to a candidate base offset.
pub const SNES_HEADER_LEN: usize = 0x20;

const TITLE_OFFSET: usize = 0x00;
const TITLE_LEN: usize = 21;
const MAP_MODE_OFFSET: usize = 0x15;
const CARTRIDGE_TYPE_OFFSET: usize = 0x16;
const ROM_SIZE_OFFSET: usize = 0x17;
const RAM_SIZE_OFFSET: usize = 0x18;
const DESTINATION_CODE_OFFSET: usize = 0x19;
const DEVELOPER_ID_OFFSET: usize = 0x1A;
const VERSION_OFFSET: usize = 0x1B;
const CHECKSUM_COMPLEMENT_OFFSET: usize = 0x1C;
const CHECKSUM_OFFSET: usize = 0x1E;

/// Which SNES memory map a candidate header offset corresponds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnesMapMode {
    LoRom,
    HiRom,
    ExHiRom,
}

impl SnesMapMode {
    /// The candidate header's base byte offset within the ROM payload
    /// (copier header already stripped, if any).
    pub const fn base_offset(self) -> usize {
        match self {
            Self::LoRom => 0x7FC0,
            Self::HiRom => 0xFFC0,
            Self::ExHiRom => 0x40FFC0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LoRom => "LoROM",
            Self::HiRom => "HiROM",
            Self::ExHiRom => "ExHiROM",
        }
    }

    /// Every candidate mode, in the fixed preference order
    /// [`best_snes_header_candidate`] tries them in.
    pub const ALL: [Self; 3] = [Self::LoRom, Self::HiRom, Self::ExHiRom];
}

/// What a parsed SNES internal header directly states, at one candidate
/// offset - does not by itself mean the candidate is correct; see
/// [`SnesHeaderFact::checksum_valid`] and [`best_snes_header_candidate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnesHeaderFact {
    pub mode: SnesMapMode,
    pub title: String,
    /// The map-mode byte's low nibble, as read - `0` = LoROM, `1` = HiROM,
    /// `5` = ExHiROM, anything else is an unrecognised/exotic mapping this
    /// module does not further interpret.
    pub map_mode_low_nibble: u8,
    pub fast_rom: bool,
    pub cartridge_type: u8,
    pub rom_size_code: u8,
    pub ram_size_code: u8,
    pub destination_code: u8,
    pub developer_id: u8,
    pub version: u8,
    pub checksum_complement: u16,
    pub checksum: u16,
}

impl SnesHeaderFact {
    /// Whether `checksum` and `checksum_complement` are exact bitwise
    /// complements of each other - the real, structural invariant a
    /// genuine SNES header satisfies, and plain title-byte plausibility
    /// does not. See the module documentation.
    pub fn checksum_valid(&self) -> bool {
        self.checksum ^ self.checksum_complement == 0xFFFF
    }

    /// Whether `map_mode_low_nibble` matches the nibble `mode` itself
    /// declares (`0`/`1`/`5` for LoROM/HiROM/ExHiROM) - a second,
    /// independent structural signal alongside the checksum.
    pub fn map_mode_matches(&self) -> bool {
        let expected = match self.mode {
            SnesMapMode::LoRom => 0x0,
            SnesMapMode::HiRom => 0x1,
            SnesMapMode::ExHiRom => 0x5,
        };
        self.map_mode_low_nibble == expected
    }
}

/// Parses a candidate SNES header at `mode`'s declared base offset within
/// `rom` (the payload, with any copier header already stripped). `None` if
/// `rom` is not long enough to contain the full [`SNES_HEADER_LEN`]-byte
/// candidate region - never a partial struct.
pub fn parse_snes_header_candidate(rom: &[u8], mode: SnesMapMode) -> Option<SnesHeaderFact> {
    let base = mode.base_offset();
    let end = base.checked_add(SNES_HEADER_LEN)?;
    let region = rom.get(base..end)?;

    let title = crate::cartridge_header::ascii_field(region, TITLE_OFFSET, TITLE_LEN)?;
    let map_mode_byte = region[MAP_MODE_OFFSET];
    let checksum_complement = u16::from_le_bytes(
        region[CHECKSUM_COMPLEMENT_OFFSET..CHECKSUM_COMPLEMENT_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    let checksum = u16::from_le_bytes(
        region[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2]
            .try_into()
            .unwrap(),
    );

    Some(SnesHeaderFact {
        mode,
        title,
        map_mode_low_nibble: map_mode_byte & 0x0F,
        fast_rom: map_mode_byte & 0x10 != 0,
        cartridge_type: region[CARTRIDGE_TYPE_OFFSET],
        rom_size_code: region[ROM_SIZE_OFFSET],
        ram_size_code: region[RAM_SIZE_OFFSET],
        destination_code: region[DESTINATION_CODE_OFFSET],
        developer_id: region[DEVELOPER_ID_OFFSET],
        version: region[VERSION_OFFSET],
        checksum_complement,
        checksum,
    })
}

/// Tries every [`SnesMapMode::ALL`] candidate against `rom`, in order, and
/// returns the **first one whose checksum/complement pair actually
/// validates** - never the first one that merely parses, and never a
/// candidate chosen by ASCII-title plausibility alone. `None` when no
/// candidate both fits within `rom` and validates - an honest "no confirmed
/// SNES header found," not a guess at the least-bad candidate.
pub fn best_snes_header_candidate(rom: &[u8]) -> Option<SnesHeaderFact> {
    SnesMapMode::ALL
        .into_iter()
        .filter_map(|mode| parse_snes_header_candidate(rom, mode))
        .find(|fact| fact.checksum_valid())
}

/// Neutral evidence for a validated [`SnesHeaderFact`] (`fact.checksum_valid()`
/// must be `true` - this function does not itself check that, so a caller
/// should only call it with [`best_snes_header_candidate`]'s result, never a
/// raw [`parse_snes_header_candidate`] call). `Strong` `ContentSignature`:
/// the checksum/complement invariant is real structural proof, not a magic-
/// byte guess.
pub fn observe_snes_evidence(fact: &SnesHeaderFact) -> Vec<ContentEvidence> {
    vec![ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        fact.mode.label(),
        ContentEvidenceConfidence::Strong,
        format!(
            "SNES {} header validated (checksum {:#06x} ^ complement {:#06x} == 0xFFFF)",
            fact.mode.label(),
            fact.checksum,
            fact.checksum_complement
        ),
    )]
}

/// A [`ContentDetector`] wrapping [`best_snes_header_candidate`]/
/// [`observe_snes_evidence`]. Note that within a bounded prefix (see
/// [`crate::archive_member_content_evidence::MAX_MEMBER_PROBE_BYTES`]),
/// only the `LoROM`/`HiROM` candidates can ever be reached - `ExHiROM`'s
/// `0x40FFC0` base offset is far beyond any bounded prefix this crate
/// reads, so a `data` slice bounded that way will only ever validate
/// against the first two candidates. This is not a special case in the
/// detector itself - [`best_snes_header_candidate`] already returns `None`
/// for any candidate whose region does not fit in `data`.
pub struct SnesHeaderDetector;

impl ContentDetector for SnesHeaderDetector {
    fn id(&self) -> &'static str {
        "snes_internal_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match best_snes_header_candidate(data) {
            Some(fact) => ContentDetectionOutcome::Recognized {
                evidence: observe_snes_evidence(&fact),
            },
            None => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_region(title: &[u8], map_mode: u8, checksum: u16) -> Vec<u8> {
        let mut region = vec![0u8; SNES_HEADER_LEN];
        region[TITLE_OFFSET..TITLE_OFFSET + title.len().min(TITLE_LEN)]
            .copy_from_slice(&title[..title.len().min(TITLE_LEN)]);
        region[MAP_MODE_OFFSET] = map_mode;
        let complement = checksum ^ 0xFFFF;
        region[CHECKSUM_COMPLEMENT_OFFSET..CHECKSUM_COMPLEMENT_OFFSET + 2]
            .copy_from_slice(&complement.to_le_bytes());
        region[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 2].copy_from_slice(&checksum.to_le_bytes());
        region
    }

    fn rom_with_header_at(mode: SnesMapMode, title: &[u8], map_mode: u8, checksum: u16) -> Vec<u8> {
        let base = mode.base_offset();
        let region = build_region(title, map_mode, checksum);
        let mut rom = vec![0u8; base + SNES_HEADER_LEN];
        rom[base..base + SNES_HEADER_LEN].copy_from_slice(&region);
        rom
    }

    // ------------------------------------------------------------------
    // parse_snes_header_candidate
    // ------------------------------------------------------------------

    #[test]
    fn lorom_candidate_parses_at_its_offset() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"SUPER GAME", 0x20, 0x1234);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::LoRom).unwrap();
        assert_eq!(fact.title, "SUPER GAME");
        assert_eq!(fact.checksum, 0x1234);
    }

    #[test]
    fn hirom_candidate_parses_at_its_offset() {
        let rom = rom_with_header_at(SnesMapMode::HiRom, b"HIROM GAME", 0x21, 0xABCD);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::HiRom).unwrap();
        assert_eq!(fact.title, "HIROM GAME");
        assert_eq!(fact.map_mode_low_nibble, 1);
    }

    #[test]
    fn exhirom_candidate_parses_at_its_offset() {
        let rom = rom_with_header_at(SnesMapMode::ExHiRom, b"EXHIROM GAME", 0x25, 0x5555);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::ExHiRom).unwrap();
        assert_eq!(fact.map_mode_low_nibble, 5);
    }

    #[test]
    fn too_short_for_candidate_offset_fails_closed() {
        let short_rom = vec![0u8; 100];
        assert_eq!(
            parse_snes_header_candidate(&short_rom, SnesMapMode::LoRom),
            None
        );
    }

    #[test]
    fn empty_rom_fails_closed_not_panic() {
        assert_eq!(parse_snes_header_candidate(&[], SnesMapMode::ExHiRom), None);
    }

    #[test]
    fn fast_rom_bit_is_read() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"X", 0x30, 0);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::LoRom).unwrap();
        assert!(fact.fast_rom);
    }

    #[test]
    fn slow_rom_bit_is_read() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"X", 0x20, 0);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::LoRom).unwrap();
        assert!(!fact.fast_rom);
    }

    // ------------------------------------------------------------------
    // checksum_valid / map_mode_matches
    // ------------------------------------------------------------------

    #[test]
    fn matching_checksum_and_complement_is_valid() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"OK", 0x20, 0x8842);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::LoRom).unwrap();
        assert!(fact.checksum_valid());
    }

    #[test]
    fn wrong_complement_is_invalid() {
        let mut rom = rom_with_header_at(SnesMapMode::LoRom, b"BAD", 0x20, 0x1111);
        let base = SnesMapMode::LoRom.base_offset();
        // Corrupt the complement so it no longer XORs to 0xFFFF.
        rom[base + CHECKSUM_COMPLEMENT_OFFSET] ^= 0xFF;
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::LoRom).unwrap();
        assert!(!fact.checksum_valid());
    }

    #[test]
    fn map_mode_matches_for_correct_nibble() {
        let rom = rom_with_header_at(SnesMapMode::HiRom, b"X", 0x21, 0);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::HiRom).unwrap();
        assert!(fact.map_mode_matches());
    }

    #[test]
    fn map_mode_mismatches_for_wrong_nibble() {
        // Parsed at the LoROM offset, but the map-mode byte declares HiROM.
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"X", 0x21, 0);
        let fact = parse_snes_header_candidate(&rom, SnesMapMode::LoRom).unwrap();
        assert!(!fact.map_mode_matches());
    }

    // ------------------------------------------------------------------
    // best_snes_header_candidate: never proven from title alone
    // ------------------------------------------------------------------

    #[test]
    fn valid_lorom_checksum_is_selected() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"REAL GAME", 0x20, 0x4242);
        let fact = best_snes_header_candidate(&rom).unwrap();
        assert_eq!(fact.mode, SnesMapMode::LoRom);
        assert_eq!(fact.title, "REAL GAME");
    }

    #[test]
    fn valid_hirom_checksum_is_selected_when_lorom_region_is_garbage() {
        let mut rom = rom_with_header_at(SnesMapMode::HiRom, b"HI GAME", 0x21, 0x9999);
        // Fill what would be the LoROM candidate region with non-validating
        // garbage (checksum/complement that do not XOR to 0xFFFF).
        let lorom_base = SnesMapMode::LoRom.base_offset();
        if rom.len() > lorom_base + SNES_HEADER_LEN {
            rom[lorom_base..lorom_base + SNES_HEADER_LEN].fill(0x41);
        }
        let fact = best_snes_header_candidate(&rom).unwrap();
        assert_eq!(fact.mode, SnesMapMode::HiRom);
    }

    #[test]
    fn plausible_title_with_invalid_checksum_is_never_selected() {
        // A convincing ASCII title, but checksum/complement do not
        // validate - this must never be "proven" SNES.
        let base = SnesMapMode::LoRom.base_offset();
        let mut rom = vec![0u8; base + SNES_HEADER_LEN];
        let title = b"TOTALLY A REAL SNES GAME";
        rom[base + TITLE_OFFSET..base + TITLE_OFFSET + TITLE_LEN.min(title.len())]
            .copy_from_slice(&title[..TITLE_LEN.min(title.len())]);
        rom[base + CHECKSUM_OFFSET..base + CHECKSUM_OFFSET + 2]
            .copy_from_slice(&0x1234u16.to_le_bytes());
        rom[base + CHECKSUM_COMPLEMENT_OFFSET..base + CHECKSUM_COMPLEMENT_OFFSET + 2]
            .copy_from_slice(&0x5678u16.to_le_bytes()); // does not XOR to 0xFFFF
        assert_eq!(best_snes_header_candidate(&rom), None);
    }

    #[test]
    fn no_candidate_fits_yields_none() {
        let tiny_rom = vec![0u8; 64];
        assert_eq!(best_snes_header_candidate(&tiny_rom), None);
    }

    #[test]
    fn candidates_are_tried_in_lorom_hirom_exhirom_order() {
        assert_eq!(
            SnesMapMode::ALL,
            [SnesMapMode::LoRom, SnesMapMode::HiRom, SnesMapMode::ExHiRom]
        );
    }

    // ------------------------------------------------------------------
    // Evidence / safety
    // ------------------------------------------------------------------

    #[test]
    fn evidence_is_strong_for_a_validated_candidate() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"GAME", 0x20, 0x0F0F);
        let fact = best_snes_header_candidate(&rom).unwrap();
        let evidence = observe_snes_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
        assert_eq!(evidence[0].value, "LoROM");
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"GAME", 0x20, 0x0F0F);
        let fact = best_snes_header_candidate(&rom).unwrap();
        for item in observe_snes_evidence(&fact) {
            assert_eq!(item.kind, ContentEvidenceKind::ContentSignature);
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"GAME", 0x20, 0x0F0F);
        assert_eq!(
            best_snes_header_candidate(&rom),
            best_snes_header_candidate(&rom)
        );
    }

    #[test]
    fn parsing_never_mutates_input() {
        let rom = rom_with_header_at(SnesMapMode::LoRom, b"GAME", 0x20, 0x0F0F);
        let before = rom.clone();
        let _ = best_snes_header_candidate(&rom);
        assert_eq!(rom, before);
    }
}
