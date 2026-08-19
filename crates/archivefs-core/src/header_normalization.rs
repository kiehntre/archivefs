//! Pure, read-only recognition and removal of known, reversible fixed-length
//! copier/container headers.
//!
//! This is the second normalization prototype after
//! [`crate::n64_byte_order`], and follows the exact same discipline: a
//! *physical* byte stream (a ROM as it actually sits on disk, header and
//! all) and its *canonical* headerless view coexist as two independently
//! hashable buffers. Nothing here ever writes a file, and the header this
//! module strips is never discarded without a way back - see
//! [`HeaderNormalizationResult::stripped_header`].
//!
//! This module answers exactly two questions, and nothing more:
//!
//! - "does this byte stream carry one of our known reversible headers?"
//!   ([`recognize_header_normalization`])
//! - "what is the canonical headerless view, if so?"
//!   ([`strip_known_header`])
//!
//! It has no notion of a DAT release, a provider match, a rename plan, a
//! RomM library, or a filesystem write - none of those concepts are
//! reachable from anything in this file.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

/// Which known, reversible fixed-length header a recognizer matched.
///
/// Every variant here strips a *constant* number of bytes from the front of
/// the file - see [`HeaderNormalizationKind::header_len`] - which is what
/// makes each one trivially reversible (see [`reconstruct_with_header`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderNormalizationKind {
    /// The 16-byte iNES header (`NES\x1a...`).
    INes16,
    /// The 16-byte fwNES FDS header (`FDS\x1a...`).
    Fds16,
    /// The 64-byte Atari Lynx cartridge header (`LYNX...`).
    Lynx64,
    /// The 128-byte Atari 7800 header (`ATARI7800` at byte offset 1).
    Atari7800_128,
    /// A 512-byte SNES copier header, recognised only by the file's overall
    /// size - see the module documentation and
    /// [`recognize_snes_copier_candidate`] for why this is deliberately the
    /// weakest of the five.
    SnesCopier512,
}

impl HeaderNormalizationKind {
    /// The stable identifier for this transform, safe to persist or log.
    pub const fn transform_id(self) -> &'static str {
        match self {
            Self::INes16 => "ines-header-strip-16",
            Self::Fds16 => "fds-header-strip-16",
            Self::Lynx64 => "lynx-header-strip-64",
            Self::Atari7800_128 => "a7800-header-strip-128",
            Self::SnesCopier512 => "snes-copier-header-strip-512",
        }
    }

    /// The exact number of bytes this transform removes from the front of
    /// the file. Never approximate, never padded to fit.
    pub const fn header_len(self) -> usize {
        match self {
            Self::INes16 | Self::Fds16 => 16,
            Self::Lynx64 => 64,
            Self::Atari7800_128 => 128,
            Self::SnesCopier512 => 512,
        }
    }

    /// Every transform in this module strips a header it can also restore
    /// (see [`reconstruct_with_header`]), so this is always `true`. Exposed
    /// as a named fact anyway - see the module's own tests - rather than
    /// left implicit, so a future non-reversible transform added here could
    /// never accidentally default to claiming this.
    pub const fn reversible(self) -> bool {
        true
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::INes16 => "iNES header",
            Self::Fds16 => "FDS header",
            Self::Lynx64 => "Atari Lynx header",
            Self::Atari7800_128 => "Atari 7800 header",
            Self::SnesCopier512 => "SNES copier header (size-rule candidate)",
        }
    }
}

/// The iNES magic: `NES\x1a`, at offset 0. Matches
/// `platform::PLATFORMS`'s own `NES` [`crate::platform::MagicRule`]
/// (`Strong`) exactly - see this module's
/// `ines_recognition_matches_the_existing_platform_registry` test.
const INES_MAGIC: &[u8; 4] = b"NES\x1a";

/// The fwNES FDS header magic: `FDS\x1a`, at offset 0. The existing
/// platform registry has no dedicated magic rule for this form; this is the
/// well-documented convention emulators (fwNES onward) use for a headered
/// raw FDS dump.
const FDS_MAGIC: &[u8; 4] = b"FDS\x1a";

/// The Atari Lynx magic: `LYNX`, at offset 0. Matches `platform::PLATFORMS`'s
/// own `Atari Lynx` [`crate::platform::MagicRule`] (`Strong`) exactly.
const LYNX_MAGIC: &[u8; 4] = b"LYNX";

/// The Atari 7800 magic: `ATARI7800`, at byte offset 1 (byte 0 is a version
/// field, not part of the magic). Matches `platform::PLATFORMS`'s own
/// `Atari7800` [`crate::platform::MagicRule`] exactly, offset and all.
const ATARI7800_MAGIC: &[u8; 9] = b"ATARI7800";
const ATARI7800_MAGIC_OFFSET: usize = 1;

/// Whether `data` begins with the iNES magic.
pub fn recognize_ines(data: &[u8]) -> bool {
    data.get(0..INES_MAGIC.len()) == Some(INES_MAGIC.as_slice())
}

/// Whether `data` begins with the fwNES FDS magic.
pub fn recognize_fds(data: &[u8]) -> bool {
    data.get(0..FDS_MAGIC.len()) == Some(FDS_MAGIC.as_slice())
}

/// Whether `data` begins with the Atari Lynx magic.
pub fn recognize_lynx(data: &[u8]) -> bool {
    data.get(0..LYNX_MAGIC.len()) == Some(LYNX_MAGIC.as_slice())
}

/// Whether `data` carries the Atari 7800 magic at its documented offset.
pub fn recognize_atari7800(data: &[u8]) -> bool {
    data.get(ATARI7800_MAGIC_OFFSET..ATARI7800_MAGIC_OFFSET + ATARI7800_MAGIC.len())
        == Some(ATARI7800_MAGIC.as_slice())
}

/// Whether `data`'s overall length matches the established SNES copier-header
/// size rule: with a 512-byte header removed, what remains is a whole number
/// of 32 KiB banks.
///
/// This is evidence about *shape*, not content - no magic byte is involved,
/// unlike every other recognizer in this module. It is real, well-established
/// evidence (real SNES ROMs really do come in exact multiples of 32 KiB, and
/// real copier headers really are exactly 512 bytes), but a bare size
/// coincidence is far weaker than a magic-byte match: plenty of non-SNES data
/// could satisfy this modulus by chance. Callers must not treat a `true`
/// result here as platform proof - see
/// [`HeaderNormalizationKind::SnesCopier512`] and the module documentation on
/// [`HeaderNormalizationDetector`].
pub fn recognize_snes_copier_candidate(data_len: usize) -> bool {
    const SNES_BANK_BYTES: usize = 32 * 1024;
    const SNES_COPIER_HEADER_BYTES: usize = 512;
    data_len > SNES_COPIER_HEADER_BYTES && data_len % SNES_BANK_BYTES == SNES_COPIER_HEADER_BYTES
}

/// Tries every recognizer in this module against `data` and returns every
/// [`HeaderNormalizationKind`] that matched, in a fixed, deterministic order
/// (the four magic-based formats first, in declaration order, then the SNES
/// size-rule candidate last). Empty when nothing matched.
///
/// The four magic-based formats are mutually exclusive by construction (each
/// checks a distinct byte pattern), so in practice at most one of them ever
/// matches; the SNES size rule is independent of all of them and could in
/// principle coexist with a magic match on adversarial input, which is why
/// this returns a `Vec` rather than a single `Option`.
pub fn recognize_header_normalization(data: &[u8]) -> Vec<HeaderNormalizationKind> {
    let mut kinds = Vec::new();
    if recognize_ines(data) {
        kinds.push(HeaderNormalizationKind::INes16);
    }
    if recognize_fds(data) {
        kinds.push(HeaderNormalizationKind::Fds16);
    }
    if recognize_lynx(data) {
        kinds.push(HeaderNormalizationKind::Lynx64);
    }
    if recognize_atari7800(data) {
        kinds.push(HeaderNormalizationKind::Atari7800_128);
    }
    if recognize_snes_copier_candidate(data.len()) {
        kinds.push(HeaderNormalizationKind::SnesCopier512);
    }
    kinds
}

/// Why a header could not be stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderNormalizationError {
    /// `data` is shorter than the header this transform expects to remove.
    /// Recognition (a magic match, or the SNES size rule) can succeed on
    /// data too short to actually contain the full declared header only if
    /// the recognizer itself reads fewer bytes than `header_len` - this is
    /// the fail-closed backstop for that case.
    TooShortForHeader { required: usize, actual: usize },
}

impl HeaderNormalizationError {
    pub fn detail(&self) -> String {
        match self {
            Self::TooShortForHeader { required, actual } => format!(
                "{actual} bytes is shorter than the {required}-byte header this transform \
                 expects to remove"
            ),
        }
    }
}

/// A header-strip result: the canonical (headerless) bytes, the exact header
/// bytes removed, and enough metadata to know what happened.
///
/// `stripped_header` exists specifically so this transform is provably
/// reversible rather than merely asserted to be: `stripped_header` followed
/// by `bytes`, concatenated, reproduces the original input exactly (see
/// [`reconstruct_with_header`] and this module's `reversible_*` tests). A
/// transform that discarded its header without keeping it would not be able
/// to make that claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderNormalizationResult {
    pub transform_id: &'static str,
    pub kind: HeaderNormalizationKind,
    pub header_len: usize,
    /// The canonical, headerless payload. Always a fresh, independently
    /// owned buffer - never the same allocation as the caller's original
    /// bytes.
    pub bytes: Vec<u8>,
    /// The exact bytes removed from the front of the file.
    pub stripped_header: Vec<u8>,
}

/// Strips the header `kind` declares from the front of `data`.
///
/// Pure: `data` is read but never mutated, and both `bytes` and
/// `stripped_header` in the result are fresh buffers. Never pads, never
/// truncates beyond the header itself, and never removes anything from
/// `data` that is not exactly `kind.header_len()` bytes long. Fails closed
/// with [`HeaderNormalizationError::TooShortForHeader`] - rather than
/// stripping a partial or short header - when `data` is not even long
/// enough to contain the declared header.
pub fn strip_known_header(
    data: &[u8],
    kind: HeaderNormalizationKind,
) -> Result<HeaderNormalizationResult, HeaderNormalizationError> {
    let header_len = kind.header_len();
    if data.len() < header_len {
        return Err(HeaderNormalizationError::TooShortForHeader {
            required: header_len,
            actual: data.len(),
        });
    }
    let (header, payload) = data.split_at(header_len);
    Ok(HeaderNormalizationResult {
        transform_id: kind.transform_id(),
        kind,
        header_len,
        bytes: payload.to_vec(),
        stripped_header: header.to_vec(),
    })
}

/// Reconstructs the original bytes from a [`HeaderNormalizationResult`]:
/// `stripped_header` followed by `bytes`, concatenated. This is the
/// reversibility proof made concrete - see this module's `reversible_*`
/// tests, which call this and assert equality with the original input.
pub fn reconstruct_with_header(result: &HeaderNormalizationResult) -> Vec<u8> {
    let mut original = Vec::with_capacity(result.stripped_header.len() + result.bytes.len());
    original.extend_from_slice(&result.stripped_header);
    original.extend_from_slice(&result.bytes);
    original
}

/// A [`ContentDetector`] that recognises the five known reversible headers
/// this module handles.
///
/// Header-signature (or, for SNES, size-rule) recognition only - this never
/// calls [`strip_known_header`] and so never reports
/// [`ContentDetectionOutcome::Malformed`], for the same reason
/// [`crate::n64_byte_order::N64ByteOrderDetector`] doesn't: whether a
/// declared header actually fits inside the bytes the caller happened to
/// hand over is a strip-time concern, not a detection-time one.
///
/// Confidence is deliberately not uniform across the five facts this
/// detector can emit. The four magic-based formats (`iNES`, `FDS`, `Lynx`,
/// `Atari 7800`) each matched a specific, documented byte signature, so they
/// are reported at [`ContentEvidenceConfidence::Strong`] - exactly the same
/// tier [`crate::n64_byte_order::N64ByteOrderDetector`] uses for a genuine
/// magic match. The SNES copier-header candidate matched only a size
/// modulus, with no byte content examined at all, so it is reported at
/// [`ContentEvidenceConfidence::Weak`] - real evidence, but nowhere near
/// strong enough to be mistaken for a signature match, and never to be
/// treated as SNES platform proof by anything that consumes it.
pub struct HeaderNormalizationDetector;

impl ContentDetector for HeaderNormalizationDetector {
    fn id(&self) -> &'static str {
        "known_header_normalization"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        let kinds = recognize_header_normalization(data);
        if kinds.is_empty() {
            return ContentDetectionOutcome::NotRecognized;
        }
        let evidence = kinds
            .into_iter()
            .map(|kind| {
                let confidence = match kind {
                    HeaderNormalizationKind::SnesCopier512 => ContentEvidenceConfidence::Weak,
                    _ => ContentEvidenceConfidence::Strong,
                };
                let detail = match kind {
                    HeaderNormalizationKind::SnesCopier512 => {
                        "file length matches the SNES copier-header size rule (length % 32768 \
                         == 512); this is a reversible-canonicalization candidate only, never \
                         platform proof by itself"
                            .to_string()
                    }
                    _ => format!("the {} signature was matched", kind.label()),
                };
                ContentEvidence::new(
                    ContentEvidenceKind::ContentSignature,
                    kind.transform_id(),
                    confidence,
                    detail,
                )
            })
            .collect();
        ContentDetectionOutcome::Recognized { evidence }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::archive::hash::hash_member_stream;
    use std::sync::atomic::AtomicBool;

    /// SHA-256 of an in-memory buffer via the crate's existing
    /// [`hash_member_stream`] (already used elsewhere for archive-member and
    /// N64-probe hashing) - no new hashing code, no new dependency.
    fn sha256_hex(data: &[u8]) -> String {
        hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
            .expect("hashing an in-memory buffer never fails")
            .hashes
            .sha256
    }

    /// A distinctive, non-repeating payload body so a transform that
    /// mistakenly strips the wrong number of bytes, or corrupts anything
    /// past the header, is caught rather than masked by a repeating pattern.
    fn distinctive_payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn headered(header: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(payload);
        bytes
    }

    // ------------------------------------------------------------------
    // General
    // ------------------------------------------------------------------

    #[test]
    fn unknown_input_is_not_recognized() {
        assert_eq!(
            recognize_header_normalization(b"just some ordinary bytes, nothing special"),
            Vec::new()
        );
        assert_eq!(recognize_header_normalization(&[]), Vec::new());
    }

    #[test]
    fn truncated_recognized_header_fails_closed() {
        // The iNES magic is present, but the file is far shorter than the
        // 16-byte header it implies.
        let short = b"NES\x1a\x01\x02";
        assert!(recognize_ines(short));
        let error = strip_known_header(short, HeaderNormalizationKind::INes16).unwrap_err();
        assert_eq!(
            error,
            HeaderNormalizationError::TooShortForHeader {
                required: 16,
                actual: short.len()
            }
        );
    }

    #[test]
    fn original_input_is_never_mutated() {
        let mut full_header = vec![0u8; 16];
        full_header[..4].copy_from_slice(INES_MAGIC);
        let data = headered(&full_header, &distinctive_payload(64));
        let before = data.clone();
        let _ = strip_known_header(&data, HeaderNormalizationKind::INes16).unwrap();
        assert_eq!(
            data, before,
            "strip_known_header must never mutate its input"
        );
    }

    #[test]
    fn deterministic_normalization() {
        let mut header = vec![0u8; 16];
        header[..4].copy_from_slice(INES_MAGIC);
        let data = headered(&header, &distinctive_payload(64));
        let first = strip_known_header(&data, HeaderNormalizationKind::INes16).unwrap();
        let second = strip_known_header(&data, HeaderNormalizationKind::INes16).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn reversible_reconstruction_general() {
        let mut header = vec![0u8; 16];
        header[..4].copy_from_slice(INES_MAGIC);
        let data = headered(&header, &distinctive_payload(64));
        let result = strip_known_header(&data, HeaderNormalizationKind::INes16).unwrap();
        assert_eq!(reconstruct_with_header(&result), data);
    }

    #[test]
    fn physical_hash_differs_when_header_present() {
        let mut header = vec![0u8; 16];
        header[..4].copy_from_slice(INES_MAGIC);
        let payload = distinctive_payload(64);
        let headered_bytes = headered(&header, &payload);
        assert_ne!(sha256_hex(&headered_bytes), sha256_hex(&payload));
    }

    #[test]
    fn normalized_hash_matches_canonical_headerless_payload() {
        let mut header = vec![0u8; 16];
        header[..4].copy_from_slice(INES_MAGIC);
        let payload = distinctive_payload(64);
        let headered_bytes = headered(&header, &payload);
        let result = strip_known_header(&headered_bytes, HeaderNormalizationKind::INes16).unwrap();
        assert_eq!(sha256_hex(&result.bytes), sha256_hex(&payload));
    }

    // ------------------------------------------------------------------
    // NES / iNES
    // ------------------------------------------------------------------

    fn ines_header() -> Vec<u8> {
        let mut header = vec![0u8; 16];
        header[..4].copy_from_slice(INES_MAGIC);
        header[4] = 2; // arbitrary PRG bank count, unrelated to recognition
        header
    }

    #[test]
    fn recognized_ines_header_strips_exactly_sixteen() {
        let payload = distinctive_payload(128);
        let data = headered(&ines_header(), &payload);
        assert!(recognize_ines(&data));
        let result = strip_known_header(&data, HeaderNormalizationKind::INes16).unwrap();
        assert_eq!(result.header_len, 16);
        assert_eq!(result.stripped_header.len(), 16);
        assert_eq!(result.bytes, payload);
        assert_eq!(reconstruct_with_header(&result), data);
    }

    #[test]
    fn wrong_ines_magic_does_not_strip() {
        let mut header = ines_header();
        header[0] = b'X'; // corrupt the magic
        let data = headered(&header, &distinctive_payload(128));
        assert!(!recognize_ines(&data));
        assert!(recognize_header_normalization(&data).is_empty());
    }

    #[test]
    fn short_ines_header_fails_safely() {
        let short = b"NES\x1a";
        assert!(recognize_ines(short));
        assert!(matches!(
            strip_known_header(short, HeaderNormalizationKind::INes16),
            Err(HeaderNormalizationError::TooShortForHeader { .. })
        ));
    }

    #[test]
    fn ines_recognition_matches_the_existing_platform_registry() {
        use crate::platform::{MagicConfidence, platform_magic_confidence_from_bytes};
        let mut probe = vec![0u8; 16];
        probe[..4].copy_from_slice(INES_MAGIC);
        let matches = platform_magic_confidence_from_bytes(&probe);
        assert!(
            matches
                .iter()
                .any(|(id, confidence)| *id == "NES" && *confidence == MagicConfidence::Strong)
        );
    }

    // ------------------------------------------------------------------
    // FDS
    // ------------------------------------------------------------------

    fn fds_header() -> Vec<u8> {
        let mut header = vec![0u8; 16];
        header[..4].copy_from_slice(FDS_MAGIC);
        header[4] = 1; // one disk side, unrelated to recognition
        header
    }

    #[test]
    fn recognized_fds_header_strips_exactly_sixteen() {
        let payload = distinctive_payload(96);
        let data = headered(&fds_header(), &payload);
        assert!(recognize_fds(&data));
        let result = strip_known_header(&data, HeaderNormalizationKind::Fds16).unwrap();
        assert_eq!(result.header_len, 16);
        assert_eq!(result.bytes, payload);
        assert_eq!(reconstruct_with_header(&result), data);
    }

    #[test]
    fn unheadered_raw_fds_is_not_accidentally_stripped() {
        // A raw, headerless FDS disk side begins with the block marker
        // "\x01*NINTENDO-HVC*", not the fwNES "FDS\x1a" header.
        let raw = b"\x01*NINTENDO-HVC*some disk data follows";
        assert!(!recognize_fds(raw));
        assert!(recognize_header_normalization(raw).is_empty());
    }

    // ------------------------------------------------------------------
    // Lynx
    // ------------------------------------------------------------------

    fn lynx_header() -> Vec<u8> {
        let mut header = vec![0u8; 64];
        header[..4].copy_from_slice(LYNX_MAGIC);
        header
    }

    #[test]
    fn recognized_lynx_header_strips_exactly_sixty_four() {
        let payload = distinctive_payload(200);
        let data = headered(&lynx_header(), &payload);
        assert!(recognize_lynx(&data));
        let result = strip_known_header(&data, HeaderNormalizationKind::Lynx64).unwrap();
        assert_eq!(result.header_len, 64);
        assert_eq!(result.stripped_header.len(), 64);
        assert_eq!(result.bytes, payload);
        assert_eq!(reconstruct_with_header(&result), data);
    }

    #[test]
    fn wrong_lynx_signature_not_stripped() {
        let mut header = lynx_header();
        header[0] = b'X';
        let data = headered(&header, &distinctive_payload(200));
        assert!(!recognize_lynx(&data));
    }

    #[test]
    fn lynx_recognition_matches_the_existing_platform_registry() {
        use crate::platform::{MagicConfidence, platform_magic_confidence_from_bytes};
        let matches = platform_magic_confidence_from_bytes(LYNX_MAGIC);
        assert!(
            matches
                .iter()
                .any(|(id, confidence)| *id == "Atari Lynx"
                    && *confidence == MagicConfidence::Strong)
        );
    }

    // ------------------------------------------------------------------
    // Atari 7800
    // ------------------------------------------------------------------

    fn atari7800_header() -> Vec<u8> {
        let mut header = vec![0u8; 128];
        header[0] = 1; // version byte, unrelated to recognition
        header[ATARI7800_MAGIC_OFFSET..ATARI7800_MAGIC_OFFSET + ATARI7800_MAGIC.len()]
            .copy_from_slice(ATARI7800_MAGIC);
        header
    }

    #[test]
    fn recognized_atari7800_header_strips_exactly_one_hundred_twenty_eight() {
        let payload = distinctive_payload(300);
        let data = headered(&atari7800_header(), &payload);
        assert!(recognize_atari7800(&data));
        let result = strip_known_header(&data, HeaderNormalizationKind::Atari7800_128).unwrap();
        assert_eq!(result.header_len, 128);
        assert_eq!(result.stripped_header.len(), 128);
        assert_eq!(result.bytes, payload);
        assert_eq!(reconstruct_with_header(&result), data);
    }

    #[test]
    fn wrong_atari7800_signature_not_stripped() {
        let mut header = atari7800_header();
        header[ATARI7800_MAGIC_OFFSET] = b'X';
        let data = headered(&header, &distinctive_payload(300));
        assert!(!recognize_atari7800(&data));
    }

    #[test]
    fn atari7800_recognition_matches_the_existing_platform_registry() {
        use crate::platform::{MagicConfidence, platform_magic_confidence_from_bytes};
        let matches = platform_magic_confidence_from_bytes(&atari7800_header());
        assert!(
            matches.iter().any(
                |(id, confidence)| *id == "Atari7800" && *confidence == MagicConfidence::Strong
            )
        );
    }

    // ------------------------------------------------------------------
    // SNES copier header (size rule only)
    // ------------------------------------------------------------------

    #[test]
    fn snes_size_rule_candidate_strips_exactly_five_hundred_twelve() {
        // 512-byte header + 3 banks of 32 KiB = 512 + 98304, satisfying
        // length % 32768 == 512.
        let payload = distinctive_payload(3 * 32 * 1024);
        let header = vec![0u8; 512];
        let data = headered(&header, &payload);
        assert!(recognize_snes_copier_candidate(data.len()));
        let result = strip_known_header(&data, HeaderNormalizationKind::SnesCopier512).unwrap();
        assert_eq!(result.header_len, 512);
        assert_eq!(result.bytes, payload);
        assert_eq!(reconstruct_with_header(&result), data);
    }

    #[test]
    fn snes_exact_bank_multiple_does_not_strip() {
        // No header at all: an exact multiple of 32 KiB, length % 32768 == 0.
        let headerless = distinctive_payload(4 * 32 * 1024);
        assert!(!recognize_snes_copier_candidate(headerless.len()));
        assert!(recognize_header_normalization(&headerless).is_empty());
    }

    #[test]
    fn snes_nonmatching_size_does_not_strip() {
        let odd_len = distinctive_payload(4 * 32 * 1024 + 37);
        assert!(!recognize_snes_copier_candidate(odd_len.len()));
        assert!(recognize_header_normalization(&odd_len).is_empty());
    }

    #[test]
    fn snes_candidate_never_emits_strong_content_evidence() {
        let payload = distinctive_payload(3 * 32 * 1024);
        let header = vec![0u8; 512];
        let data = headered(&header, &payload);

        let outcome = HeaderNormalizationDetector.detect(&data);
        assert!(outcome.is_recognized());
        let evidence = outcome.evidence();
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            evidence[0].value,
            HeaderNormalizationKind::SnesCopier512.transform_id()
        );
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Weak);
    }

    // ------------------------------------------------------------------
    // ContentEvidence integration
    // ------------------------------------------------------------------

    #[test]
    fn magic_based_formats_emit_strong_content_signature_evidence() {
        let cases: &[(Vec<u8>, HeaderNormalizationKind)] = &[
            (
                headered(&ines_header(), &distinctive_payload(32)),
                HeaderNormalizationKind::INes16,
            ),
            (
                headered(&fds_header(), &distinctive_payload(32)),
                HeaderNormalizationKind::Fds16,
            ),
            (
                headered(&lynx_header(), &distinctive_payload(32)),
                HeaderNormalizationKind::Lynx64,
            ),
            (
                headered(&atari7800_header(), &distinctive_payload(32)),
                HeaderNormalizationKind::Atari7800_128,
            ),
        ];
        for (data, kind) in cases {
            let outcome = HeaderNormalizationDetector.detect(data);
            assert!(outcome.is_recognized(), "{kind:?} should be recognized");
            let evidence = outcome.evidence();
            assert!(
                evidence.iter().any(|item| item.value == kind.transform_id()
                    && item.confidence == ContentEvidenceConfidence::Strong
                    && item.kind == ContentEvidenceKind::ContentSignature),
                "{kind:?} should produce Strong ContentSignature evidence, got {evidence:?}"
            );
        }
    }

    #[test]
    fn detector_reports_not_recognized_for_unrelated_bytes() {
        let outcome = HeaderNormalizationDetector.detect(b"nothing special here at all");
        assert_eq!(outcome, ContentDetectionOutcome::NotRecognized);
    }

    #[test]
    fn detector_id_is_stable() {
        assert_eq!(
            HeaderNormalizationDetector.id(),
            HeaderNormalizationDetector.id()
        );
        assert_eq!(
            HeaderNormalizationDetector.id(),
            "known_header_normalization"
        );
    }

    #[test]
    fn every_kind_reports_itself_as_reversible() {
        for kind in [
            HeaderNormalizationKind::INes16,
            HeaderNormalizationKind::Fds16,
            HeaderNormalizationKind::Lynx64,
            HeaderNormalizationKind::Atari7800_128,
            HeaderNormalizationKind::SnesCopier512,
        ] {
            assert!(kind.reversible());
        }
    }
}
