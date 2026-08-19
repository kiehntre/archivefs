//! Pure, read-only Atari 7800 `.a78` header field decoding - deeper than
//! [`crate::header_normalization`]'s magic-only recognition (which only
//! answers "does this carry the `ATARI7800` header, and can it be
//! reversibly stripped").
//!
//! # Format verified, not assumed
//!
//! Verified against the 8BitDev.org A78 Header Specification page
//! (`http://7800.8bitdev.org/index.php/A78_Header_Specification`), the
//! dedicated primary reference this format's own homebrew/flashcart tooling
//! (Concerto, cc7800) is written against - and cross-checked against
//! [`crate::header_normalization::ATARI7800_MAGIC`]'s own already-reviewed
//! offset (`0x01`), which matches exactly:
//!
//! ```text
//! [0x00]         header_version
//! [0x01..0x11]   "ATARI7800"        16 bytes
//! [0x11..0x31]   cart_title         32 bytes, ASCII
//! [0x31..0x35]   rom_size           4 bytes, big-endian (payload size,
//!                                   excluding this 128-byte header)
//! [0x35..0x37]   cart_type          2 bytes, big-endian bitfield
//! [0x37]         controller1        0=None, 1=Joystick, 2=Light Gun, ...
//! [0x38]         controller2        same value space as controller1
//! [0x39]         tv_type            bit 0: 0=NTSC, 1=PAL
//! [0x3A]         save_device        version >= 2 only (HSC/SaveKey/AtariVox)
//! ```
//!
//! # What this module does not decode
//!
//! The full `cart_type` bitfield (documented with entries for POKEY-at-
//! `$4000`, SuperGame bank-switching, SuperGame RAM, and several more
//! specialised hardware bits) is exposed as a raw value plus only the two
//! bits this research pass could corroborate with confidence (`bit 0`
//! POKEY-at-`$4000`, `bit 1` SuperGame bank-switched); the remaining bits
//! are real per the source but this pass does not decode all of them. The
//! version-4-and-later mapper/audio/interrupt extension fields (bytes
//! `0x40` onward) and the 28-byte header-end magic are likewise out of
//! scope - version 1-3 fields (the overwhelming majority of real `.a78`
//! files) are what this pass covers.
//!
//! # Physical header facts vs. normalized payload - kept separate
//!
//! Exactly like [`crate::nes_header_evidence`]'s own note: an `.a78` header
//! is commonly a generator-attached convenience (a raw 7800 cartridge dump
//! never carries one), not preservation truth about the payload - this
//! module reports only what the header bytes themselves declare.

use crate::cartridge_header::ascii_field;
use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::header_normalization::recognize_atari7800;

/// The `ATARI7800` magic's byte offset - matches
/// [`crate::header_normalization`]'s own (private) `ATARI7800_MAGIC_OFFSET`
/// exactly; duplicated here as a `const` only because that constant is not
/// exported, not because this module re-derives or re-verifies it.
const ATARI7800_MAGIC_OFFSET: usize = 1;

pub const A78_HEADER_BYTES: usize = 128;

const HEADER_VERSION_OFFSET: usize = 0x00;
const CART_TITLE_OFFSET: usize = 0x11;
const CART_TITLE_LEN: usize = 32;
const ROM_SIZE_OFFSET: usize = 0x31;
const CART_TYPE_OFFSET: usize = 0x35;
const CONTROLLER1_OFFSET: usize = 0x37;
const CONTROLLER2_OFFSET: usize = 0x38;
const TV_TYPE_OFFSET: usize = 0x39;
const SAVE_DEVICE_OFFSET: usize = 0x3A;

const CART_TYPE_POKEY_AT_4000: u16 = 1 << 0;
const CART_TYPE_SUPERGAME_BANKSWITCHED: u16 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Atari7800TvType {
    Ntsc,
    Pal,
}

/// What a parsed `.a78` header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atari7800HeaderFact {
    pub header_version: u8,
    pub cart_title: String,
    pub rom_size: u32,
    /// Raw cart-type bitfield - see the module documentation for which
    /// individual bits this module additionally decodes below.
    pub cart_type_raw: u16,
    pub pokey_at_4000: bool,
    pub supergame_bankswitched: bool,
    pub controller1: u8,
    pub controller2: u8,
    pub tv_type: Atari7800TvType,
}

/// Parses `bytes` (must be at least [`A78_HEADER_BYTES`] long, and match the
/// `ATARI7800` magic at its documented offset - see
/// [`crate::header_normalization::recognize_atari7800`]). Fails closed
/// (`None`) on a short buffer or wrong magic.
pub fn parse_a78_header(bytes: &[u8]) -> Option<Atari7800HeaderFact> {
    if bytes.len() < A78_HEADER_BYTES || !recognize_atari7800(bytes) {
        return None;
    }
    let cart_type_raw = u16::from_be_bytes(
        bytes[CART_TYPE_OFFSET..CART_TYPE_OFFSET + 2]
            .try_into()
            .unwrap(),
    );
    let tv_type = if bytes[TV_TYPE_OFFSET] & 0x01 != 0 {
        Atari7800TvType::Pal
    } else {
        Atari7800TvType::Ntsc
    };

    let _ = ATARI7800_MAGIC_OFFSET; // documented cross-check, see module docs
    let _ = SAVE_DEVICE_OFFSET; // version >= 2 only, not surfaced this pass

    Some(Atari7800HeaderFact {
        header_version: bytes[HEADER_VERSION_OFFSET],
        cart_title: ascii_field(bytes, CART_TITLE_OFFSET, CART_TITLE_LEN)?,
        rom_size: u32::from_be_bytes(
            bytes[ROM_SIZE_OFFSET..ROM_SIZE_OFFSET + 4]
                .try_into()
                .unwrap(),
        ),
        cart_type_raw,
        pokey_at_4000: cart_type_raw & CART_TYPE_POKEY_AT_4000 != 0,
        supergame_bankswitched: cart_type_raw & CART_TYPE_SUPERGAME_BANKSWITCHED != 0,
        controller1: bytes[CONTROLLER1_OFFSET],
        controller2: bytes[CONTROLLER2_OFFSET],
        tv_type,
    })
}

/// Neutral evidence: `Strong` `BootStructure` for the `ATARI7800` magic
/// match - matching [`crate::header_normalization::HeaderNormalizationKind::Atari7800_128`]'s
/// own `Strong` rating for the identical signature, plus (when non-empty) a
/// `Corroborated` `ProductCode` for the cart title.
pub fn observe_a78_evidence(fact: &Atari7800HeaderFact) -> Vec<ContentEvidence> {
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        "ATARI7800",
        ContentEvidenceConfidence::Strong,
        format!(
            "ATARI7800 header magic matched; header version {}",
            fact.header_version
        ),
    )];
    if !fact.cart_title.is_empty() {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            fact.cart_title.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate cart title read from the .a78 header - not verified against a canonical \
             release list, and the header itself may be a generator-attached convenience rather \
             than preservation truth",
        ));
    }
    evidence
}

/// A [`ContentDetector`] wrapping [`parse_a78_header`]/[`observe_a78_evidence`].
pub struct Atari7800HeaderDetector;

impl ContentDetector for Atari7800HeaderDetector {
    fn id(&self) -> &'static str {
        "atari7800_a78_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        match parse_a78_header(data) {
            Some(fact) => ContentDetectionOutcome::Recognized {
                evidence: observe_a78_evidence(&fact),
            },
            None => ContentDetectionOutcome::NotRecognized,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_header(title: &str, rom_size: u32, cart_type: u16, tv_pal: bool) -> Vec<u8> {
        let mut bytes = vec![0u8; A78_HEADER_BYTES];
        bytes[HEADER_VERSION_OFFSET] = 1;
        bytes[ATARI7800_MAGIC_OFFSET..ATARI7800_MAGIC_OFFSET + 9].copy_from_slice(b"ATARI7800");
        let title_bytes = title.as_bytes();
        bytes[CART_TITLE_OFFSET..CART_TITLE_OFFSET + title_bytes.len().min(CART_TITLE_LEN)]
            .copy_from_slice(&title_bytes[..title_bytes.len().min(CART_TITLE_LEN)]);
        bytes[ROM_SIZE_OFFSET..ROM_SIZE_OFFSET + 4].copy_from_slice(&rom_size.to_be_bytes());
        bytes[CART_TYPE_OFFSET..CART_TYPE_OFFSET + 2].copy_from_slice(&cart_type.to_be_bytes());
        bytes[CONTROLLER1_OFFSET] = 1;
        bytes[CONTROLLER2_OFFSET] = 0;
        bytes[TV_TYPE_OFFSET] = if tv_pal { 1 } else { 0 };
        bytes
    }

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header("GAME", 16384, 0, false);
        assert_eq!(parse_a78_header(&header[..64]), None);
    }

    #[test]
    fn wrong_magic_fails_closed() {
        let mut header = synthetic_header("GAME", 16384, 0, false);
        header[ATARI7800_MAGIC_OFFSET] = b'X';
        assert_eq!(parse_a78_header(&header), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_a78_header(&[]), None);
    }

    #[test]
    fn valid_header_parses_every_field() {
        let header = synthetic_header("XEVIOUS", 32768, 0, false);
        let fact = parse_a78_header(&header).unwrap();
        assert_eq!(fact.cart_title, "XEVIOUS");
        assert_eq!(fact.rom_size, 32768);
        assert_eq!(fact.controller1, 1);
        assert_eq!(fact.tv_type, Atari7800TvType::Ntsc);
    }

    #[test]
    fn pal_bit_is_detected() {
        let header = synthetic_header("GAME", 16384, 0, true);
        let fact = parse_a78_header(&header).unwrap();
        assert_eq!(fact.tv_type, Atari7800TvType::Pal);
    }

    #[test]
    fn pokey_bit_is_decoded() {
        let header = synthetic_header("GAME", 16384, 0x0001, false);
        let fact = parse_a78_header(&header).unwrap();
        assert!(fact.pokey_at_4000);
        assert!(!fact.supergame_bankswitched);
    }

    #[test]
    fn supergame_bankswitched_bit_is_decoded() {
        let header = synthetic_header("GAME", 16384, 0x0002, false);
        let fact = parse_a78_header(&header).unwrap();
        assert!(fact.supergame_bankswitched);
        assert!(!fact.pokey_at_4000);
    }

    #[test]
    fn cart_type_raw_preserves_undocumented_bits() {
        let header = synthetic_header("GAME", 16384, 0xFFFF, false);
        let fact = parse_a78_header(&header).unwrap();
        assert_eq!(fact.cart_type_raw, 0xFFFF);
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn evidence_includes_strong_magic_and_product_code() {
        let header = synthetic_header("XEVIOUS", 32768, 0, false);
        let fact = parse_a78_header(&header).unwrap();
        let evidence = observe_a78_evidence(&fact);
        let boot = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::BootStructure)
            .unwrap();
        assert_eq!(boot.confidence, ContentEvidenceConfidence::Strong);
        let product = evidence
            .iter()
            .find(|item| item.kind == ContentEvidenceKind::ProductCode)
            .unwrap();
        assert_eq!(product.value, "XEVIOUS");
    }

    #[test]
    fn empty_title_yields_no_product_code() {
        let header = synthetic_header("", 16384, 0, false);
        let fact = parse_a78_header(&header).unwrap();
        assert!(
            !observe_a78_evidence(&fact)
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header("GAME", 16384, 0, false);
        let fact = parse_a78_header(&header).unwrap();
        for item in observe_a78_evidence(&fact) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn matches_existing_header_normalization_recognition() {
        let header = synthetic_header("GAME", 16384, 0, false);
        assert!(recognize_atari7800(&header));
        assert!(parse_a78_header(&header).is_some());
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header("GAME", 16384, 0, false);
        assert_eq!(parse_a78_header(&header), parse_a78_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header("GAME", 16384, 0, false);
        let before = header.clone();
        let _ = parse_a78_header(&header);
        assert_eq!(header, before);
    }
}
