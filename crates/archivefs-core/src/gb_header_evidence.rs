//! Pure, read-only Game Boy / Game Boy Color cartridge header evidence.
//!
//! # Format verified, not assumed
//!
//! Verified against Pan Docs' Cartridge Header page
//! (`https://gbdev.io/pandocs/The_Cartridge_Header.html`), the community-
//! maintained primary reference nearly every real Game Boy emulator's
//! loader is written against:
//!
//! ```text
//! [0x0104..0x0134]  nintendo_logo    48 bytes (fixed bitmap)
//! [0x0134..0x0144]  title            16 bytes, ASCII (CGB carts may reuse
//!                                    the tail of this field for the
//!                                    manufacturer code / CGB flag - this
//!                                    module reads the full 16 bytes as
//!                                    title, matching the DMG convention,
//!                                    and separately reads byte 0x0143 as
//!                                    the CGB flag)
//! [0x0143]          cgb_flag         0x80 = CGB-enhanced, 0xC0 = CGB-only
//! [0x0144..0x0146]  new_licensee     2 bytes, ASCII (only meaningful when
//!                                    old_licensee == 0x33)
//! [0x0146]          sgb_flag         0x03 = SGB-enhanced
//! [0x0147]          cartridge_type
//! [0x0148]          rom_size         0x01 << N banks convention (see
//!                                    [`GbHeaderFact::rom_size_bytes`])
//! [0x0149]          ram_size
//! [0x014A]          destination_code 0x00 = Japan, 0x01 = overseas
//! [0x014B]          old_licensee
//! [0x014C]          mask_rom_version
//! [0x014D]          header_checksum
//! [0x014E..0x0150]  global_checksum  2 bytes, big-endian
//! ```
//!
//! Header checksum algorithm, quoted from the same source: covers bytes
//! `0x0134..=0x014C` (title through mask ROM version, exclusive of the
//! checksum byte itself), `checksum = checksum.wrapping_sub(byte).wrapping_sub(1)`
//! starting from `0`, for each byte in that range in order - see
//! [`compute_header_checksum`], which implements exactly this loop (not
//! [`crate::cartridge_header::wrapping_sum_u8`], whose plain additive sum is
//! a different, unrelated algorithm this format does not use).
//!
//! # Collision safety
//!
//! The Nintendo logo is Nintendo's own copyrighted bitmap, present in every
//! genuine Game Boy/Game Boy Color cartridge (the boot ROM refuses to run
//! anything else) - a real, structural signature, not a coincidence. Paired
//! with a valid header checksum (which covers the title/type/size fields,
//! not the logo itself), the two together are treated as `Strong`
//! structural evidence; the logo bytes matching but the checksum not
//! validating is downgraded rather than ignored - see
//! [`observe_gb_evidence`].

use crate::cartridge_header::ascii_field;
use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const GB_HEADER_BYTES: usize = 0x150;

const NINTENDO_LOGO_OFFSET: usize = 0x104;
/// The Nintendo logo's exact 48 bytes, verified against Pan Docs (widely
/// reproduced - every real Game Boy boot ROM compares against this exact
/// bitmap before running the cartridge).
#[rustfmt::skip]
const NINTENDO_LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83,
    0x00, 0x0C, 0x00, 0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E,
    0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63,
    0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

const TITLE_OFFSET: usize = 0x134;
const TITLE_LEN: usize = 16;
const CGB_FLAG_OFFSET: usize = 0x143;
const NEW_LICENSEE_OFFSET: usize = 0x144;
const SGB_FLAG_OFFSET: usize = 0x146;
const CARTRIDGE_TYPE_OFFSET: usize = 0x147;
const ROM_SIZE_OFFSET: usize = 0x148;
const RAM_SIZE_OFFSET: usize = 0x149;
const DESTINATION_CODE_OFFSET: usize = 0x14A;
const OLD_LICENSEE_OFFSET: usize = 0x14B;
const MASK_ROM_VERSION_OFFSET: usize = 0x14C;
const HEADER_CHECKSUM_OFFSET: usize = 0x14D;
const GLOBAL_CHECKSUM_OFFSET: usize = 0x14E;

const CHECKSUM_RANGE_START: usize = 0x134;
const CHECKSUM_RANGE_END_INCLUSIVE: usize = 0x14C;

/// Whether a cartridge declares DMG (original Game Boy) support, CGB
/// enhancement, or CGB-exclusivity - read directly from `cgb_flag`
/// (`0x80`/`0xC0`), never inferred from the ROM extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GbColorSupport {
    /// `cgb_flag` is neither `0x80` nor `0xC0` - a plain DMG-only cartridge.
    DmgOnly,
    /// `cgb_flag == 0x80` - runs on DMG, with extra features on CGB.
    CgbEnhanced,
    /// `cgb_flag == 0xC0` - CGB hardware required.
    CgbOnly,
}

/// What a parsed Game Boy/Game Boy Color header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GbHeaderFact {
    pub logo_valid: bool,
    pub title: String,
    pub color_support: GbColorSupport,
    pub sgb_enhanced: bool,
    pub cartridge_type: u8,
    pub rom_size_code: u8,
    pub ram_size_code: u8,
    pub destination_code: u8,
    pub old_licensee: u8,
    pub mask_rom_version: u8,
    pub header_checksum: u8,
    pub global_checksum: u16,
    /// Whether [`compute_header_checksum`] over `bytes` matches
    /// `header_checksum`.
    pub header_checksum_valid: bool,
}

impl GbHeaderFact {
    /// The declared ROM size in bytes: `32 KiB << rom_size_code` - the
    /// documented convention (`rom_size_code` 0 = 32 KiB/no banking, up to
    /// the highest declared code). `None` for a code this convention does
    /// not define a size for (an overflow this module refuses to guess at).
    pub fn rom_size_bytes(&self) -> Option<u64> {
        (32u64 * 1024).checked_shl(u32::from(self.rom_size_code))
    }
}

/// Computes the Game Boy header checksum over `bytes[0x134..=0x14C]` -
/// `bytes` must be at least [`GB_HEADER_BYTES`] long. `None` on a short
/// buffer, never a partial/wrong sum.
pub fn compute_header_checksum(bytes: &[u8]) -> Option<u8> {
    let region = bytes.get(CHECKSUM_RANGE_START..=CHECKSUM_RANGE_END_INCLUSIVE)?;
    let mut checksum: u8 = 0;
    for &byte in region {
        checksum = checksum.wrapping_sub(byte).wrapping_sub(1);
    }
    Some(checksum)
}

/// Parses `bytes` (must be at least [`GB_HEADER_BYTES`] long) into a
/// [`GbHeaderFact`]. Fails closed (`None`) on a short buffer - unlike most
/// parsers in this crate, a *structurally* invalid header (wrong logo,
/// invalid checksum) still parses successfully so [`GbHeaderFact::logo_valid`]/
/// `header_checksum_valid` can report exactly what failed, matching
/// [`crate::threedo_boot_evidence::parse_opera_volume_header`]'s precedent
/// for the same reason.
pub fn parse_gb_header(bytes: &[u8]) -> Option<GbHeaderFact> {
    if bytes.len() < GB_HEADER_BYTES {
        return None;
    }
    let logo_valid =
        bytes[NINTENDO_LOGO_OFFSET..NINTENDO_LOGO_OFFSET + NINTENDO_LOGO.len()] == NINTENDO_LOGO;
    let title = ascii_field(bytes, TITLE_OFFSET, TITLE_LEN)?;
    let cgb_flag = bytes[CGB_FLAG_OFFSET];
    let color_support = match cgb_flag {
        0x80 => GbColorSupport::CgbEnhanced,
        0xC0 => GbColorSupport::CgbOnly,
        _ => GbColorSupport::DmgOnly,
    };
    let header_checksum = bytes[HEADER_CHECKSUM_OFFSET];
    let header_checksum_valid = compute_header_checksum(bytes) == Some(header_checksum);
    let global_checksum = u16::from_be_bytes(
        bytes[GLOBAL_CHECKSUM_OFFSET..GLOBAL_CHECKSUM_OFFSET + 2]
            .try_into()
            .unwrap(),
    );

    let _ = NEW_LICENSEE_OFFSET; // documented field, not surfaced as its own struct field this pass

    Some(GbHeaderFact {
        logo_valid,
        title,
        color_support,
        sgb_enhanced: bytes[SGB_FLAG_OFFSET] == 0x03,
        cartridge_type: bytes[CARTRIDGE_TYPE_OFFSET],
        rom_size_code: bytes[ROM_SIZE_OFFSET],
        ram_size_code: bytes[RAM_SIZE_OFFSET],
        destination_code: bytes[DESTINATION_CODE_OFFSET],
        old_licensee: bytes[OLD_LICENSEE_OFFSET],
        mask_rom_version: bytes[MASK_ROM_VERSION_OFFSET],
        header_checksum,
        global_checksum,
        header_checksum_valid,
    })
}

/// Neutral evidence for a parsed [`GbHeaderFact`]:
///
/// - Nintendo logo valid **and** header checksum valid: `Strong`
///   `BootStructure` - two independent structural facts agree.
/// - Logo valid but checksum invalid: `Corroborated` - a real, non-random
///   signature match, but the header's own self-check fails, so this stops
///   short of `Strong`.
/// - Logo invalid: no evidence at all, regardless of the checksum -
///   matching [`crate::saturn_boot_evidence::observe_saturn_evidence`]'s
///   "unrecognised signature emits nothing" precedent.
///
/// # CGB disambiguation (Batch 6)
///
/// `cgb_flag` (read into [`GbColorSupport`]) further splits the emitted
/// fact by what the header actually declares - never by filename/extension:
///
/// - [`GbColorSupport::DmgOnly`]: unchanged from before this milestone -
///   `"Nintendo Game Boy logo"`, `PlatformSpecific("Game Boy")`.
/// - [`GbColorSupport::CgbOnly`] (`cgb_flag == 0xC0`): the cartridge
///   physically cannot run on original Game Boy hardware - a genuinely
///   different, platform-specific fact, `"Nintendo Game Boy Color logo
///   (CGB-only)"`, scoped `PlatformSpecific("Game Boy Color")` in
///   [`crate::content_evidence_scope`]. Reaches the same confidence ceiling
///   as the DMG case (`Strong` when the checksum also validates).
/// - [`GbColorSupport::CgbEnhanced`] (`cgb_flag == 0x80`): the cartridge
///   *is* a real, backward-compatible Game Boy cartridge (it runs
///   unmodified on original DMG hardware) that additionally enhances on
///   CGB - both things are simultaneously true, so both are reported: the
///   ordinary `"Nintendo Game Boy logo"` fact at its normal confidence
///   (this is what actually resolves the platform - a dual-mode cart is a
///   real Game Boy cartridge first), plus a second, deliberately capped-at-
///   `Corroborated` `"Nintendo Game Boy Color logo (dual-mode)"` fact
///   (`Family("Game Boy/Game Boy Color")`) that never independently
///   resolves anything on its own - it only ever *corroborates* alongside
///   the Strong DMG leg, honestly representing that this title belongs to
///   both ecosystems without inventing an exclusive platform claim the
///   header itself does not make.
pub fn observe_gb_evidence(fact: &GbHeaderFact) -> Vec<ContentEvidence> {
    if !fact.logo_valid {
        return Vec::new();
    }
    let confidence = if fact.header_checksum_valid {
        ContentEvidenceConfidence::Strong
    } else {
        ContentEvidenceConfidence::Corroborated
    };
    let checksum_detail = if fact.header_checksum_valid {
        "valid"
    } else {
        "did not validate"
    };
    match fact.color_support {
        GbColorSupport::DmgOnly => vec![ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "Nintendo Game Boy logo",
            confidence,
            format!(
                "Nintendo logo bitmap matched (cgb_flag=DMG-only); header checksum {checksum_detail}"
            ),
        )],
        GbColorSupport::CgbOnly => vec![ContentEvidence::new(
            ContentEvidenceKind::BootStructure,
            "Nintendo Game Boy Color logo (CGB-only)",
            confidence,
            format!(
                "Nintendo logo bitmap matched with cgb_flag=0xC0 (CGB-exclusive - will not run on original Game Boy hardware); header checksum {checksum_detail}"
            ),
        )],
        GbColorSupport::CgbEnhanced => vec![
            ContentEvidence::new(
                ContentEvidenceKind::BootStructure,
                "Nintendo Game Boy logo",
                confidence,
                format!(
                    "Nintendo logo bitmap matched with cgb_flag=0x80 (CGB-enhanced, but still a real DMG-compatible Game Boy cartridge); header checksum {checksum_detail}"
                ),
            ),
            ContentEvidence::new(
                ContentEvidenceKind::BootStructure,
                "Nintendo Game Boy Color logo (dual-mode)",
                confidence.min(ContentEvidenceConfidence::Corroborated),
                "cgb_flag=0x80 additionally signals CGB-enhanced dual-mode support - corroborating context only, never independently resolved, since the cartridge is a genuine DMG-compatible Game Boy cartridge first",
            ),
        ],
    }
}

/// A [`ContentDetector`] wrapping [`parse_gb_header`]/[`observe_gb_evidence`].
pub struct GbHeaderDetector;

impl ContentDetector for GbHeaderDetector {
    fn id(&self) -> &'static str {
        "gb_cartridge_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_gb_header(data) {
            Some(fact) if fact.logo_valid => ContentDetectionOutcome::Recognized {
                evidence: observe_gb_evidence(&fact),
            },
            _ => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_header(title: &str, cgb_flag: u8, corrupt_checksum: bool) -> Vec<u8> {
        let mut bytes = vec![0u8; GB_HEADER_BYTES];
        bytes[NINTENDO_LOGO_OFFSET..NINTENDO_LOGO_OFFSET + NINTENDO_LOGO.len()]
            .copy_from_slice(&NINTENDO_LOGO);
        let title_bytes = title.as_bytes();
        bytes[TITLE_OFFSET..TITLE_OFFSET + title_bytes.len().min(TITLE_LEN)]
            .copy_from_slice(&title_bytes[..title_bytes.len().min(TITLE_LEN)]);
        bytes[CGB_FLAG_OFFSET] = cgb_flag;
        bytes[SGB_FLAG_OFFSET] = 0x03;
        bytes[CARTRIDGE_TYPE_OFFSET] = 0x1B;
        bytes[ROM_SIZE_OFFSET] = 0x02;
        bytes[RAM_SIZE_OFFSET] = 0x03;
        let checksum = compute_header_checksum(&bytes).unwrap();
        bytes[HEADER_CHECKSUM_OFFSET] = if corrupt_checksum {
            checksum.wrapping_add(1)
        } else {
            checksum
        };
        bytes
    }

    // ------------------------------------------------------------------
    // Parsing
    // ------------------------------------------------------------------

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header("GAME", 0x00, false);
        assert_eq!(parse_gb_header(&header[..0x100]), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_gb_header(&[]), None);
    }

    #[test]
    fn valid_logo_and_checksum_parse_correctly() {
        let header = synthetic_header("POKEMON RED", 0x00, false);
        let fact = parse_gb_header(&header).unwrap();
        assert!(fact.logo_valid);
        assert!(fact.header_checksum_valid);
        assert_eq!(fact.title, "POKEMON RED");
    }

    #[test]
    fn wrong_logo_bytes_are_detected_but_still_parse() {
        let mut header = synthetic_header("GAME", 0x00, false);
        header[NINTENDO_LOGO_OFFSET] = 0xFF;
        let fact = parse_gb_header(&header).unwrap();
        assert!(!fact.logo_valid);
    }

    #[test]
    fn corrupted_checksum_is_detected() {
        let header = synthetic_header("GAME", 0x00, true);
        let fact = parse_gb_header(&header).unwrap();
        assert!(!fact.header_checksum_valid);
    }

    #[test]
    fn dmg_only_flag_is_default() {
        let header = synthetic_header("GAME", 0x00, false);
        let fact = parse_gb_header(&header).unwrap();
        assert_eq!(fact.color_support, GbColorSupport::DmgOnly);
    }

    #[test]
    fn cgb_enhanced_flag_is_detected() {
        let header = synthetic_header("GAME", 0x80, false);
        let fact = parse_gb_header(&header).unwrap();
        assert_eq!(fact.color_support, GbColorSupport::CgbEnhanced);
    }

    #[test]
    fn cgb_only_flag_is_detected() {
        let header = synthetic_header("GAME", 0xC0, false);
        let fact = parse_gb_header(&header).unwrap();
        assert_eq!(fact.color_support, GbColorSupport::CgbOnly);
    }

    #[test]
    fn sgb_flag_is_read() {
        let header = synthetic_header("GAME", 0x00, false);
        let fact = parse_gb_header(&header).unwrap();
        assert!(fact.sgb_enhanced);
    }

    #[test]
    fn rom_size_bytes_computes_expected_value() {
        let mut header = synthetic_header("GAME", 0x00, false);
        header[ROM_SIZE_OFFSET] = 0; // 32 KiB
        let fact = parse_gb_header(&header).unwrap();
        assert_eq!(fact.rom_size_bytes(), Some(32 * 1024));
    }

    #[test]
    fn rom_size_bytes_scales_with_code() {
        let mut header = synthetic_header("GAME", 0x00, false);
        header[ROM_SIZE_OFFSET] = 4; // 32 KiB << 4 = 512 KiB
        let fact = parse_gb_header(&header).unwrap();
        assert_eq!(fact.rom_size_bytes(), Some(512 * 1024));
    }

    #[test]
    fn global_checksum_is_read_big_endian() {
        let mut header = synthetic_header("GAME", 0x00, false);
        header[GLOBAL_CHECKSUM_OFFSET] = 0x12;
        header[GLOBAL_CHECKSUM_OFFSET + 1] = 0x34;
        let fact = parse_gb_header(&header).unwrap();
        assert_eq!(fact.global_checksum, 0x1234);
    }

    #[test]
    fn checksum_algorithm_matches_documented_formula() {
        // Hand-computed independently of compute_header_checksum's own loop:
        // an all-zero region from 0x134..=0x14C (25 bytes) - each iteration
        // does checksum = checksum - 0 - 1, so the result is -25 mod 256.
        let bytes = vec![0u8; GB_HEADER_BYTES];
        let expected = 0u8.wrapping_sub(25);
        assert_eq!(compute_header_checksum(&bytes), Some(expected));
    }

    #[test]
    fn checksum_range_is_exactly_title_through_version() {
        // Corrupting a byte just outside the checksummed range must not
        // change the computed checksum.
        let header = synthetic_header("GAME", 0x00, false);
        let mut before_range = header.clone();
        before_range[CHECKSUM_RANGE_START - 1] ^= 0xFF;
        let mut after_range = header.clone();
        after_range[HEADER_CHECKSUM_OFFSET + 1] ^= 0xFF; // inside global checksum, outside range
        assert_eq!(
            compute_header_checksum(&before_range),
            compute_header_checksum(&header)
        );
        assert_eq!(
            compute_header_checksum(&after_range),
            compute_header_checksum(&header)
        );
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn valid_logo_and_checksum_yields_strong_evidence() {
        let header = synthetic_header("GAME", 0x00, false);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
        assert_eq!(evidence[0].kind, ContentEvidenceKind::BootStructure);
    }

    #[test]
    fn valid_logo_with_invalid_checksum_yields_corroborated_only() {
        let header = synthetic_header("GAME", 0x00, true);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            evidence[0].confidence,
            ContentEvidenceConfidence::Corroborated
        );
    }

    #[test]
    fn invalid_logo_yields_no_evidence_regardless_of_checksum() {
        let mut header = synthetic_header("GAME", 0x00, false);
        header[NINTENDO_LOGO_OFFSET] = 0x00;
        let fact = parse_gb_header(&header).unwrap();
        assert!(observe_gb_evidence(&fact).is_empty());
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header("GAME", 0x00, false);
        let fact = parse_gb_header(&header).unwrap();
        for item in observe_gb_evidence(&fact) {
            assert_eq!(item.kind, ContentEvidenceKind::BootStructure);
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header("GAME", 0x00, false);
        assert_eq!(parse_gb_header(&header), parse_gb_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header("GAME", 0x00, false);
        let before = header.clone();
        let _ = parse_gb_header(&header);
        assert_eq!(header, before);
    }

    // ------------------------------------------------------------------
    // CGB disambiguation evidence (Batch 6, section 7) - see
    // observe_gb_evidence's own doc comment for the full design.
    // ------------------------------------------------------------------

    #[test]
    fn cgb_only_valid_header_yields_one_strong_fact_with_the_cgb_only_value() {
        let header = synthetic_header("GAME", 0xC0, false);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].value, "Nintendo Game Boy Color logo (CGB-only)");
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn cgb_only_invalid_checksum_downgrades_to_corroborated() {
        let header = synthetic_header("GAME", 0xC0, true);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        assert_eq!(
            evidence[0].confidence,
            ContentEvidenceConfidence::Corroborated
        );
        assert_eq!(evidence[0].value, "Nintendo Game Boy Color logo (CGB-only)");
    }

    #[test]
    fn cgb_enhanced_valid_header_yields_two_facts() {
        let header = synthetic_header("GAME", 0x80, false);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        assert_eq!(evidence.len(), 2);
        assert!(
            evidence
                .iter()
                .any(|item| item.value == "Nintendo Game Boy logo")
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.value == "Nintendo Game Boy Color logo (dual-mode)")
        );
    }

    #[test]
    fn cgb_enhanced_dmg_leg_is_strong_when_checksum_valid() {
        let header = synthetic_header("GAME", 0x80, false);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        let dmg_leg = evidence
            .iter()
            .find(|item| item.value == "Nintendo Game Boy logo")
            .unwrap();
        assert_eq!(dmg_leg.confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn cgb_enhanced_dual_mode_leg_is_never_strong_even_with_a_valid_checksum() {
        // The dual-mode fact must always cap at Corroborated - even when
        // the header checksum validates - so it can never independently
        // resolve Game Boy Color; see the rule table's own doc comment.
        let header = synthetic_header("GAME", 0x80, false);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        let dual_mode_leg = evidence
            .iter()
            .find(|item| item.value == "Nintendo Game Boy Color logo (dual-mode)")
            .unwrap();
        assert_eq!(
            dual_mode_leg.confidence,
            ContentEvidenceConfidence::Corroborated
        );
    }

    #[test]
    fn cgb_enhanced_dual_mode_leg_is_corroborated_even_with_invalid_checksum() {
        let header = synthetic_header("GAME", 0x80, true);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        let dual_mode_leg = evidence
            .iter()
            .find(|item| item.value == "Nintendo Game Boy Color logo (dual-mode)")
            .unwrap();
        assert_eq!(
            dual_mode_leg.confidence,
            ContentEvidenceConfidence::Corroborated
        );
    }

    #[test]
    fn dmg_only_still_yields_exactly_one_fact() {
        let header = synthetic_header("GAME", 0x00, false);
        let fact = parse_gb_header(&header).unwrap();
        let evidence = observe_gb_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].value, "Nintendo Game Boy logo");
    }

    #[test]
    fn cgb_evidence_is_deterministic() {
        let header = synthetic_header("GAME", 0x80, false);
        let fact = parse_gb_header(&header).unwrap();
        assert_eq!(observe_gb_evidence(&fact), observe_gb_evidence(&fact));
    }
}
