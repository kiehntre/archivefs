//! Pure, read-only Nintendo 64 ROM header field decoding - deeper metadata
//! beyond [`crate::n64_byte_order`]'s byte-order detection/normalization.
//!
//! # Operates on canonical `Z64` byte order only
//!
//! This module does not reimplement byte-order handling: it reads every
//! field at the fixed offsets a canonical (`Z64`, big-endian) N64 header
//! uses, and it is the caller's responsibility to have already normalized a
//! `V64`/`N64` dump via [`crate::n64_byte_order::normalize_to_z64`] first.
//! Reading a still-`V64`/`N64`-ordered buffer with this module produces
//! meaningless field values, not an error - the same "physical vs.
//! normalized identity" boundary [`crate::n64_byte_order`] already draws;
//! this module simply lives on the normalized side of it.
//!
//! # Format verified, not assumed
//!
//! Verified against two independent, mutually-agreeing primary/production-
//! grade sources: N64brew's ROM Header wiki page
//! (`https://n64brew.dev/wiki/ROM_Header`) and `ultra64.ca`'s "Detailed
//! Nintendo N64 memory Map" reference document, both describing the
//! identical field layout:
//!
//! ```text
//! [0x00..0x04]  pi_bsd_dom1_config   initial PI_BSD_DOM1 register values
//! [0x04..0x08]  clock_rate
//! [0x08..0x0C]  boot_address         (entry point)
//! [0x0C..0x10]  release              (libultra release/revision)
//! [0x10..0x14]  crc1
//! [0x14..0x18]  crc2
//! [0x18..0x20]  (unused)
//! [0x20..0x34]  image_name           20 bytes, ASCII
//! [0x34..0x3B]  (unused)
//! [0x3B]        manufacturer_id
//! [0x3C..0x3E]  cartridge_id         2 bytes
//! [0x3E]        country_code
//! [0x3F]        (unused)
//! ```
//!
//! # CRC1/CRC2 are not generic file hashes - and not independently verified
//! by this module
//!
//! `crc1`/`crc2` are two 32-bit values computed by a CIC-boot-chip-specific
//! algorithm over the ROM's boot code, seeded differently per CIC chip -
//! they are read here as plain header fields, exactly as declared, and
//! **never** treated as, or compared against, a whole-file hash of any
//! kind. This module does not attempt to recompute or validate them (doing
//! so correctly requires CIC detection, which this milestone's own scope
//! notes explicitly defer rather than invent) - they are reported as raw
//! facts a resolver could later use, nothing more.

use crate::cartridge_header::ascii_field;
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

pub const N64_HEADER_BYTES: usize = 0x40;

const CLOCK_RATE_OFFSET: usize = 0x04;
const BOOT_ADDRESS_OFFSET: usize = 0x08;
const RELEASE_OFFSET: usize = 0x0C;
const CRC1_OFFSET: usize = 0x10;
const CRC2_OFFSET: usize = 0x14;
const IMAGE_NAME_OFFSET: usize = 0x20;
const IMAGE_NAME_LEN: usize = 20;
const MANUFACTURER_ID_OFFSET: usize = 0x3B;
const CARTRIDGE_ID_OFFSET: usize = 0x3C;
const COUNTRY_CODE_OFFSET: usize = 0x3E;

/// What a parsed, canonical-`Z64`-order N64 header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct N64HeaderFact {
    pub clock_rate: u32,
    pub boot_address: u32,
    pub release: u32,
    pub crc1: u32,
    pub crc2: u32,
    pub image_name: String,
    pub manufacturer_id: u8,
    pub cartridge_id: u16,
    pub country_code: u8,
}

/// Parses `bytes` (must be at least [`N64_HEADER_BYTES`] long, in canonical
/// `Z64` order - see the module documentation) into an [`N64HeaderFact`].
/// `None` on a short buffer.
pub fn parse_n64_header(bytes: &[u8]) -> Option<N64HeaderFact> {
    if bytes.len() < N64_HEADER_BYTES {
        return None;
    }
    let u32_at = |offset: usize| u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
    Some(N64HeaderFact {
        clock_rate: u32_at(CLOCK_RATE_OFFSET),
        boot_address: u32_at(BOOT_ADDRESS_OFFSET),
        release: u32_at(RELEASE_OFFSET),
        crc1: u32_at(CRC1_OFFSET),
        crc2: u32_at(CRC2_OFFSET),
        image_name: ascii_field(bytes, IMAGE_NAME_OFFSET, IMAGE_NAME_LEN)?,
        manufacturer_id: bytes[MANUFACTURER_ID_OFFSET],
        cartridge_id: u16::from_be_bytes(
            bytes[CARTRIDGE_ID_OFFSET..CARTRIDGE_ID_OFFSET + 2]
                .try_into()
                .unwrap(),
        ),
        country_code: bytes[COUNTRY_CODE_OFFSET],
    })
}

/// Neutral evidence for a parsed [`N64HeaderFact`]: the image name, when
/// non-empty, as a `Corroborated` `ProductCode` - a candidate identifier,
/// not verified against a canonical release list, and not `Strong` because
/// this module deliberately does not validate CRC1/CRC2 (see the module
/// documentation) - unlike [`crate::gb_header_evidence`]/
/// [`crate::gba_header_evidence`], there is no independently-checkable
/// structural invariant this module actually verifies here.
pub fn observe_n64_evidence(fact: &N64HeaderFact) -> Vec<ContentEvidence> {
    if fact.image_name.is_empty() {
        return Vec::new();
    }
    vec![ContentEvidence::new(
        ContentEvidenceKind::ProductCode,
        fact.image_name.clone(),
        ContentEvidenceConfidence::Corroborated,
        "candidate image name read from the N64 ROM header - not verified against a canonical \
         release list, and CRC1/CRC2 were not independently validated (CIC detection is out of \
         scope for this module)",
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::n64_byte_order::N64ByteOrder;

    fn synthetic_header(name: &str) -> Vec<u8> {
        let mut bytes = vec![0u8; N64_HEADER_BYTES];
        bytes[0..4].copy_from_slice(&N64ByteOrder::Z64.magic());
        bytes[CLOCK_RATE_OFFSET..CLOCK_RATE_OFFSET + 4]
            .copy_from_slice(&0x0000_0F41u32.to_be_bytes());
        bytes[BOOT_ADDRESS_OFFSET..BOOT_ADDRESS_OFFSET + 4]
            .copy_from_slice(&0x8000_0400u32.to_be_bytes());
        bytes[CRC1_OFFSET..CRC1_OFFSET + 4].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        bytes[CRC2_OFFSET..CRC2_OFFSET + 4].copy_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        let name_bytes = name.as_bytes();
        bytes[IMAGE_NAME_OFFSET..IMAGE_NAME_OFFSET + name_bytes.len().min(IMAGE_NAME_LEN)]
            .copy_from_slice(&name_bytes[..name_bytes.len().min(IMAGE_NAME_LEN)]);
        bytes[MANUFACTURER_ID_OFFSET] = b'N';
        bytes[CARTRIDGE_ID_OFFSET..CARTRIDGE_ID_OFFSET + 2].copy_from_slice(b"SM");
        bytes[COUNTRY_CODE_OFFSET] = b'E';
        bytes
    }

    #[test]
    fn truncated_header_fails_closed() {
        let header = synthetic_header("SUPER MARIO 64");
        assert_eq!(parse_n64_header(&header[..0x20]), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_n64_header(&[]), None);
    }

    #[test]
    fn every_field_is_read_correctly() {
        let header = synthetic_header("SUPER MARIO 64");
        let fact = parse_n64_header(&header).unwrap();
        assert_eq!(fact.image_name, "SUPER MARIO 64");
        assert_eq!(fact.crc1, 0xDEAD_BEEF);
        assert_eq!(fact.crc2, 0xCAFE_BABE);
        assert_eq!(fact.boot_address, 0x8000_0400);
        assert_eq!(fact.manufacturer_id, b'N');
        assert_eq!(fact.country_code, b'E');
    }

    #[test]
    fn cartridge_id_is_read_big_endian() {
        let header = synthetic_header("GAME");
        let fact = parse_n64_header(&header).unwrap();
        assert_eq!(fact.cartridge_id, u16::from_be_bytes(*b"SM"));
    }

    #[test]
    fn empty_image_name_is_empty_string() {
        let mut header = synthetic_header("GAME");
        header[IMAGE_NAME_OFFSET..IMAGE_NAME_OFFSET + IMAGE_NAME_LEN].fill(0);
        let fact = parse_n64_header(&header).unwrap();
        assert_eq!(fact.image_name, "");
    }

    #[test]
    fn image_name_is_trimmed_of_trailing_nuls() {
        let header = synthetic_header("ZELDA");
        let fact = parse_n64_header(&header).unwrap();
        assert_eq!(fact.image_name, "ZELDA");
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn nonempty_image_name_yields_corroborated_product_code() {
        let header = synthetic_header("SUPER MARIO 64");
        let fact = parse_n64_header(&header).unwrap();
        let evidence = observe_n64_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, ContentEvidenceKind::ProductCode);
        assert_eq!(evidence[0].value, "SUPER MARIO 64");
        assert_eq!(
            evidence[0].confidence,
            ContentEvidenceConfidence::Corroborated
        );
    }

    #[test]
    fn empty_image_name_yields_no_evidence() {
        let mut header = synthetic_header("GAME");
        header[IMAGE_NAME_OFFSET..IMAGE_NAME_OFFSET + IMAGE_NAME_LEN].fill(0);
        let fact = parse_n64_header(&header).unwrap();
        assert!(observe_n64_evidence(&fact).is_empty());
    }

    #[test]
    fn evidence_never_reaches_strong_confidence() {
        // CRC1/CRC2 are never validated by this module - see its docs -
        // so evidence can never claim Strong.
        let header = synthetic_header("GAME");
        let fact = parse_n64_header(&header).unwrap();
        for item in observe_n64_evidence(&fact) {
            assert_ne!(item.confidence, ContentEvidenceConfidence::Strong);
        }
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header("GAME");
        let fact = parse_n64_header(&header).unwrap();
        for item in observe_n64_evidence(&fact) {
            assert_eq!(item.kind, ContentEvidenceKind::ProductCode);
        }
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let header = synthetic_header("GAME");
        assert_eq!(parse_n64_header(&header), parse_n64_header(&header));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let header = synthetic_header("GAME");
        let before = header.clone();
        let _ = parse_n64_header(&header);
        assert_eq!(header, before);
    }
}
