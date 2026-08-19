//! Shared, pure, read-only executable/container header observers: ELF,
//! PS3 SELF, Xbox XBE, Xbox 360 XEX2.
//!
//! Each function here inspects a bounded header and reports a format
//! signature - never platform. ELF in particular is ubiquitous (PS2, PS3's
//! unsigned form, and many non-Sony systems all use it), so
//! [`ElfDetector`] is deliberately the weakest kind of evidence this module
//! produces: a format fact, not even circumstantial platform evidence on
//! its own.
//!
//! # Format facts verified, not assumed
//!
//! - ELF magic (`0x7F 'E' 'L' 'F'`) is the well-known, ISO/IEEE-documented
//!   ELF identification field.
//! - PS3 SELF magic (`0x53 0x43 0x45 0x00`, `"SCE\0"`) is cross-checked
//!   against Wikipedia's community-maintained "List of file signatures"
//!   and multiple independent PS3 homebrew/reverse-engineering sources.
//! - XBE header layout (magic `"XBEH"` at 0, `base_address` at `0x104`,
//!   `certificate_addr` at `0x118`) and the certificate structure
//!   (`size`, `timedate`, `title_id`, then a 40-`u16` UTF-16LE
//!   `title_name`) are verified against `xemu`'s own `xemu-xbe.h`
//!   (`https://github.com/xemu-project/xemu/blob/master/xemu-xbe.h`), a
//!   real, actively-maintained Xbox emulator.
//! - XEX2 magic (`"XEX2"`), the optional-header table layout, and the
//!   execution-info key/offsets (`media_id` at `+0x0`, `title_id` at
//!   `+0xC` within the execution-info block) are the exact values already
//!   verified and reviewed in [`crate::game_identity`]'s own XEX parser -
//!   reused here as constants, not re-derived.
//!
//! XBE addresses are **virtual addresses**: the on-disk certificate offset
//! is `certificate_addr - base_address` (the standard, universally-used
//! XBE convention). This module performs that subtraction with a checked
//! operation and fails closed on underflow rather than wrapping.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

// ---------------------------------------------------------------------
// ELF
// ---------------------------------------------------------------------

pub const ELF_MAGIC: &[u8; 4] = &[0x7f, b'E', b'L', b'F'];

pub fn looks_like_elf(header: &[u8]) -> bool {
    header.len() >= ELF_MAGIC.len() && &header[..ELF_MAGIC.len()] == ELF_MAGIC.as_slice()
}

/// Generic ELF magic detector. Deliberately the weakest fact this module
/// produces: ELF is a cross-platform, non-Sony-specific format, so this
/// alone is a `Weak` executable-format signature, never platform evidence.
pub struct ElfDetector;

impl ContentDetector for ElfDetector {
    fn id(&self) -> &'static str {
        "elf_magic"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_elf(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized {
            evidence: vec![ContentEvidence::new(
                ContentEvidenceKind::ContentSignature,
                "ELF",
                ContentEvidenceConfidence::Weak,
                "ELF magic present - a generic, cross-platform executable format signature, not platform evidence on its own",
            )],
        }
    }
}

// ---------------------------------------------------------------------
// PS3 SELF
// ---------------------------------------------------------------------

pub const SELF_MAGIC: &[u8; 4] = &[0x53, 0x43, 0x45, 0x00];

pub fn looks_like_self(header: &[u8]) -> bool {
    header.len() >= SELF_MAGIC.len() && &header[..SELF_MAGIC.len()] == SELF_MAGIC.as_slice()
}

/// PS3 SELF ("Signed ELF") container magic. Never decrypts or interprets
/// the signed/encrypted body - only the leading magic is inspected.
pub struct SelfDetector;

impl ContentDetector for SelfDetector {
    fn id(&self) -> &'static str {
        "ps3_self_magic"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_self(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized {
            evidence: vec![ContentEvidence::new(
                ContentEvidenceKind::ContentSignature,
                "SELF",
                ContentEvidenceConfidence::Strong,
                "PS3 SELF (Signed ELF) container magic present",
            )],
        }
    }
}

// ---------------------------------------------------------------------
// Original Xbox XBE
// ---------------------------------------------------------------------

pub const XBE_MAGIC: &[u8; 4] = b"XBEH";
const XBE_BASE_OFFSET: usize = 0x104;
const XBE_CERT_ADDR_OFFSET: usize = 0x118;
const XBE_HEADER_PREFIX_BYTES: usize = 0x11C;
/// Bounded certificate read - the real structure is smaller than this in
/// every known XBE version; this is headroom, not a claim about the exact
/// certificate size.
pub const XBE_CERTIFICATE_READ_BYTES: usize = 512;
const XBE_CERT_TITLE_ID_OFFSET: usize = 0x8;
const XBE_CERT_TITLE_NAME_OFFSET: usize = 0xC;
const XBE_CERT_TITLE_NAME_UTF16_UNITS: usize = 40;

pub fn looks_like_xbe(header: &[u8]) -> bool {
    header.len() >= XBE_MAGIC.len() && &header[..XBE_MAGIC.len()] == XBE_MAGIC.as_slice()
}

/// What a parsed XBE header/certificate directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XbeHeaderFact {
    /// `certificate.title_id`, as an exact 8-hex-digit string - a raw
    /// numeric identifier, not yet a normalized product code.
    pub title_id: Option<String>,
    /// `certificate.title_name`, UTF-16LE, NUL-trimmed.
    pub title_name: Option<String>,
}

/// Parses an XBE header (`header` must include at least
/// [`XBE_HEADER_PREFIX_BYTES`]) plus a separately-supplied, already-read
/// certificate buffer (`certificate`, read by the caller from the
/// computed file offset - see the module documentation on virtual-address
/// translation). Returns `None` only when the magic itself does not
/// match; a present-but-too-short certificate simply yields `None` fields
/// rather than failing the whole parse.
pub fn parse_xbe_header(header: &[u8], certificate: Option<&[u8]>) -> Option<XbeHeaderFact> {
    if !looks_like_xbe(header) || header.len() < XBE_HEADER_PREFIX_BYTES {
        return None;
    }
    let Some(cert) = certificate else {
        return Some(XbeHeaderFact {
            title_id: None,
            title_name: None,
        });
    };
    let title_id = cert
        .get(XBE_CERT_TITLE_ID_OFFSET..XBE_CERT_TITLE_ID_OFFSET + 4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("4-byte slice")))
        .map(|value| format!("{value:08X}"));
    let title_name = cert
        .get(
            XBE_CERT_TITLE_NAME_OFFSET
                ..XBE_CERT_TITLE_NAME_OFFSET + XBE_CERT_TITLE_NAME_UTF16_UNITS * 2,
        )
        .map(decode_utf16le_trimmed);
    Some(XbeHeaderFact {
        title_id,
        title_name,
    })
}

/// Computes the certificate's file offset from the XBE header's own
/// `base_address`/`certificate_addr` virtual addresses. `None` on
/// underflow (a malformed/inconsistent header) rather than a guessed or
/// wrapped offset.
pub fn xbe_certificate_file_offset(header: &[u8]) -> Option<u64> {
    if !looks_like_xbe(header) || header.len() < XBE_HEADER_PREFIX_BYTES {
        return None;
    }
    let base = u32::from_le_bytes(
        header[XBE_BASE_OFFSET..XBE_BASE_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    let cert_addr = u32::from_le_bytes(
        header[XBE_CERT_ADDR_OFFSET..XBE_CERT_ADDR_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    cert_addr.checked_sub(base).map(u64::from)
}

fn decode_utf16le_trimmed(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    text.trim_end_matches('\0').to_string()
}

pub struct XbeDetector;

impl ContentDetector for XbeDetector {
    fn id(&self) -> &'static str {
        "xbox_xbe_magic"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_xbe(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized {
            evidence: vec![ContentEvidence::new(
                ContentEvidenceKind::ContentSignature,
                "XBEH",
                ContentEvidenceConfidence::Strong,
                "Original Xbox XBE header magic present",
            )],
        }
    }
}

// ---------------------------------------------------------------------
// Xbox 360 XEX2
// ---------------------------------------------------------------------

pub const XEX_MAGIC: &[u8; 4] = b"XEX2";
const XEX_BASE_HEADER_BYTES: usize = 0x18;
const XEX_HEADER_COUNT_OFFSET: usize = 0x14;
const XEX_OPT_HEADER_TABLE_OFFSET: usize = 0x18;
const XEX_OPT_HEADER_ENTRY_BYTES: usize = 8;
/// `XEX_HEADER_EXECUTION_INFO` - verified and already reviewed in
/// [`crate::game_identity`].
const XEX_EXECUTION_INFO_KEY: u32 = 0x0004_0006;
const XEX_EXECUTION_INFO_BYTES: usize = 0x18;
const MAX_XEX_OPT_HEADERS: u32 = 512;

pub fn looks_like_xex(header: &[u8]) -> bool {
    header.len() >= XEX_MAGIC.len() && &header[..XEX_MAGIC.len()] == XEX_MAGIC.as_slice()
}

/// What a parsed XEX2 header directly states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Xex2HeaderFact {
    pub media_id: String,
    pub title_id: String,
}

/// Parses the unencrypted, uncompressed XEX2 module header from `data`:
/// magic, the optional-header table, and (when present) the execution-info
/// optional header holding `media_id`/`title_id`. Never reads the
/// compressed/encrypted module body. `data` must be a single contiguous
/// buffer containing at least the header, table, and execution-info region
/// (a caller reading a real file would supply a generously bounded prefix).
///
/// Returns `None` when the magic does not match, the header count is zero
/// or exceeds [`MAX_XEX_OPT_HEADERS`], the table/execution-info region is
/// out of bounds, or no execution-info header is present - fails closed,
/// exactly as the already-reviewed [`crate::game_identity`] implementation
/// does.
pub fn parse_xex2_header(data: &[u8]) -> Option<Xex2HeaderFact> {
    if !looks_like_xex(data) || data.len() < XEX_BASE_HEADER_BYTES {
        return None;
    }
    let header_count = u32::from_be_bytes(
        data[XEX_HEADER_COUNT_OFFSET..XEX_HEADER_COUNT_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    if header_count == 0 || header_count > MAX_XEX_OPT_HEADERS {
        return None;
    }
    let table_end =
        XEX_OPT_HEADER_TABLE_OFFSET + header_count as usize * XEX_OPT_HEADER_ENTRY_BYTES;
    let table = data.get(XEX_OPT_HEADER_TABLE_OFFSET..table_end)?;

    let execution_info_offset =
        table
            .chunks_exact(XEX_OPT_HEADER_ENTRY_BYTES)
            .find_map(|entry| {
                let key = u32::from_be_bytes(entry[0..4].try_into().unwrap());
                (key == XEX_EXECUTION_INFO_KEY)
                    .then(|| u32::from_be_bytes(entry[4..8].try_into().unwrap()))
            })?;

    let start = execution_info_offset as usize;
    let execution_info = data.get(start..start + XEX_EXECUTION_INFO_BYTES)?;
    let media_id = u32::from_be_bytes(execution_info[0x0..0x4].try_into().unwrap());
    let title_id = u32::from_be_bytes(execution_info[0xC..0x10].try_into().unwrap());
    Some(Xex2HeaderFact {
        media_id: format!("{media_id:08X}"),
        title_id: format!("{title_id:08X}"),
    })
}

pub struct XexDetector;

impl ContentDetector for XexDetector {
    fn id(&self) -> &'static str {
        "xbox360_xex_magic"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_xex(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized {
            evidence: vec![ContentEvidence::new(
                ContentEvidenceKind::ContentSignature,
                "XEX2",
                ContentEvidenceConfidence::Strong,
                "Xbox 360 XEX2 module header magic present",
            )],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // ELF
    // ------------------------------------------------------------------

    #[test]
    fn elf_magic_is_detected() {
        let data = [0x7f, b'E', b'L', b'F', 1, 2, 3];
        assert!(looks_like_elf(&data));
        let outcome = ElfDetector.detect(&data);
        assert!(outcome.is_recognized());
        assert_eq!(
            outcome.evidence()[0].confidence,
            ContentEvidenceConfidence::Weak
        );
    }

    #[test]
    fn non_elf_is_not_recognized() {
        assert!(!looks_like_elf(b"not an elf"));
        assert_eq!(
            ElfDetector.detect(b"not an elf"),
            ContentDetectionOutcome::NotRecognized
        );
    }

    // ------------------------------------------------------------------
    // SELF
    // ------------------------------------------------------------------

    #[test]
    fn self_magic_is_detected() {
        let data = [0x53, 0x43, 0x45, 0x00, 1, 2];
        assert!(looks_like_self(&data));
        assert!(SelfDetector.detect(&data).is_recognized());
    }

    #[test]
    fn non_self_is_not_recognized() {
        assert!(!looks_like_self(b"ELF!"));
    }

    // ------------------------------------------------------------------
    // XBE
    // ------------------------------------------------------------------

    fn synthetic_xbe_header(base: u32, cert_addr: u32) -> Vec<u8> {
        let mut header = vec![0u8; XBE_HEADER_PREFIX_BYTES];
        header[0..4].copy_from_slice(XBE_MAGIC);
        header[XBE_BASE_OFFSET..XBE_BASE_OFFSET + 4].copy_from_slice(&base.to_le_bytes());
        header[XBE_CERT_ADDR_OFFSET..XBE_CERT_ADDR_OFFSET + 4]
            .copy_from_slice(&cert_addr.to_le_bytes());
        header
    }

    fn synthetic_xbe_certificate(title_id: u32, title_name: &str) -> Vec<u8> {
        let mut cert = vec![0u8; XBE_CERTIFICATE_READ_BYTES];
        cert[XBE_CERT_TITLE_ID_OFFSET..XBE_CERT_TITLE_ID_OFFSET + 4]
            .copy_from_slice(&title_id.to_le_bytes());
        let utf16: Vec<u8> = title_name
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let end = (XBE_CERT_TITLE_NAME_OFFSET + utf16.len()).min(cert.len());
        cert[XBE_CERT_TITLE_NAME_OFFSET..end]
            .copy_from_slice(&utf16[..end - XBE_CERT_TITLE_NAME_OFFSET]);
        cert
    }

    #[test]
    fn xbe_magic_is_detected() {
        let header = synthetic_xbe_header(0x10000, 0x10120);
        assert!(looks_like_xbe(&header));
        assert!(XbeDetector.detect(&header).is_recognized());
    }

    #[test]
    fn xbe_certificate_offset_is_computed_from_virtual_addresses() {
        let header = synthetic_xbe_header(0x10000, 0x10120);
        assert_eq!(xbe_certificate_file_offset(&header), Some(0x120));
    }

    #[test]
    fn xbe_certificate_offset_underflow_fails_closed() {
        let header = synthetic_xbe_header(0x20000, 0x10120);
        assert_eq!(xbe_certificate_file_offset(&header), None);
    }

    #[test]
    fn xbe_title_id_and_name_are_extracted() {
        let header = synthetic_xbe_header(0x10000, 0x10120);
        let cert = synthetic_xbe_certificate(0x4D5A0058, "Test Game");
        let fact = parse_xbe_header(&header, Some(&cert)).unwrap();
        assert_eq!(fact.title_id.as_deref(), Some("4D5A0058"));
        assert_eq!(fact.title_name.as_deref(), Some("Test Game"));
    }

    #[test]
    fn xbe_without_certificate_yields_no_fields() {
        let header = synthetic_xbe_header(0x10000, 0x10120);
        let fact = parse_xbe_header(&header, None).unwrap();
        assert_eq!(fact.title_id, None);
        assert_eq!(fact.title_name, None);
    }

    #[test]
    fn non_xbe_header_is_not_parsed() {
        assert_eq!(parse_xbe_header(b"not xbe", None), None);
    }

    // ------------------------------------------------------------------
    // XEX2
    // ------------------------------------------------------------------

    fn synthetic_xex2(media_id: u32, title_id: u32) -> Vec<u8> {
        let mut data =
            vec![
                0u8;
                XEX_OPT_HEADER_TABLE_OFFSET + XEX_OPT_HEADER_ENTRY_BYTES + XEX_EXECUTION_INFO_BYTES
            ];
        data[0..4].copy_from_slice(XEX_MAGIC);
        data[XEX_HEADER_COUNT_OFFSET..XEX_HEADER_COUNT_OFFSET + 4]
            .copy_from_slice(&1u32.to_be_bytes());
        let execution_info_offset =
            (XEX_OPT_HEADER_TABLE_OFFSET + XEX_OPT_HEADER_ENTRY_BYTES) as u32;
        data[XEX_OPT_HEADER_TABLE_OFFSET..XEX_OPT_HEADER_TABLE_OFFSET + 4]
            .copy_from_slice(&XEX_EXECUTION_INFO_KEY.to_be_bytes());
        data[XEX_OPT_HEADER_TABLE_OFFSET + 4..XEX_OPT_HEADER_TABLE_OFFSET + 8]
            .copy_from_slice(&execution_info_offset.to_be_bytes());
        let info_start = execution_info_offset as usize;
        data[info_start..info_start + 4].copy_from_slice(&media_id.to_be_bytes());
        data[info_start + 0xC..info_start + 0x10].copy_from_slice(&title_id.to_be_bytes());
        data
    }

    #[test]
    fn xex2_magic_is_detected() {
        let data = synthetic_xex2(0x4141_4141, 0x4242_4242);
        assert!(looks_like_xex(&data));
        assert!(XexDetector.detect(&data).is_recognized());
    }

    #[test]
    fn xex2_media_and_title_id_are_extracted() {
        let data = synthetic_xex2(0x4141_4141, 0x4242_4242);
        let fact = parse_xex2_header(&data).unwrap();
        assert_eq!(fact.media_id, "41414141");
        assert_eq!(fact.title_id, "42424242");
    }

    #[test]
    fn xex2_zero_header_count_fails_closed() {
        let mut data = synthetic_xex2(1, 2);
        data[XEX_HEADER_COUNT_OFFSET..XEX_HEADER_COUNT_OFFSET + 4]
            .copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(parse_xex2_header(&data), None);
    }

    #[test]
    fn xex2_excessive_header_count_fails_closed() {
        let mut data = synthetic_xex2(1, 2);
        data[XEX_HEADER_COUNT_OFFSET..XEX_HEADER_COUNT_OFFSET + 4]
            .copy_from_slice(&(MAX_XEX_OPT_HEADERS + 1).to_be_bytes());
        assert_eq!(parse_xex2_header(&data), None);
    }

    #[test]
    fn xex2_no_execution_info_header_fails_closed() {
        let mut data = synthetic_xex2(1, 2);
        // Corrupt the key so it no longer matches XEX_EXECUTION_INFO_KEY.
        data[XEX_OPT_HEADER_TABLE_OFFSET..XEX_OPT_HEADER_TABLE_OFFSET + 4]
            .copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(parse_xex2_header(&data), None);
    }

    #[test]
    fn xex2_evidence_never_assigns_a_platform() {
        let data = synthetic_xex2(1, 2);
        let outcome = XexDetector.detect(&data);
        for item in outcome.evidence() {
            assert_eq!(item.kind, ContentEvidenceKind::ContentSignature);
        }
    }
}
