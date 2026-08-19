//! Pure, read-only Super Magic Drive (SMD) de-interleave normalization for
//! Sega Mega Drive / Genesis ROM images.
//!
//! This is the third normalization prototype, after
//! [`crate::n64_byte_order`] (whole-buffer byte reordering) and
//! [`crate::header_normalization`] (fixed-offset header removal). It proves
//! a third, structurally distinct family: a fixed-length copier header
//! *plus* a block-wise byte de-interleave, both reversible, both leaving
//! the original bytes untouched.
//!
//! # The verified algorithm
//!
//! The Super Magic Drive copier prepends a 512-byte header, then stores the
//! ROM payload in 16,384-byte (16 KiB) blocks. Within each block, bytes at
//! *even* canonical offsets are stored in the block's second half and bytes
//! at *odd* canonical offsets are stored in the first half - this is the
//! opposite of the interleaving convention some other sources casually
//! describe, and it was verified rather than assumed. It was confirmed
//! against two independent, well-established references before writing any
//! code:
//!
//! - uCON64's own FAQ (`https://ucon64.sourceforge.io/ucon64/faq.html`):
//!   "for each block of 16384 bytes of the ROM all bytes at even offsets are
//!   stored in the upper half of the dumped block. The bytes at odd offsets
//!   are stored in the lower half."
//! - `pyrominfo`'s `genesis.py` (an independent, working de-interleave
//!   implementation): strips the 512-byte (`0x200`) header, then for each
//!   16 KiB block, `data[block][0::2], data[block][1::2] = block[0x2000:],
//!   block[:0x2000]` - even canonical positions take the block's second
//!   half, odd positions take the first half.
//!
//! Both agree with each other and with the transform this module
//! implements: for canonical (de-interleaved) offset `2*i` within a block,
//! the SMD byte lives at `8192 + i`; for canonical offset `2*i + 1`, the SMD
//! byte lives at `i`.
//!
//! # What was deliberately *not* used
//!
//! Some secondary sources describe a copier-signature convention at header
//! bytes 8-9 (`0xAA`, `0xBB`) as identifying "a real Super Magic Drive
//! dump." This module does not rely on it: sources disagree on how it's
//! actually checked in practice (the `file(1)` magic database's own SMD
//! rule does not test those header bytes at all - it matches a scrambled
//! `"SEGA GENESIS"/"SEGA MEGA DRIVE"` string deep inside the still-
//! interleaved payload instead), and many real SMD dumps are known to carry
//! an all-zero header regardless of which copier or tool produced them. Per
//! the project's established discipline (see
//! [`crate::platform::MagicConfidence`] and
//! [`crate::header_normalization::HeaderNormalizationKind::SnesCopier512`]),
//! evidence that isn't confirmed reliable is never promoted past `Weak` -
//! see [`detect_smd_candidate`] and [`SmdNormalizationDetector`]. Instead,
//! this module's tests prove a cleaner corroboration: after de-interleaving
//! *and* stripping the header, a canonical Mega Drive ROM's own `SEGA`
//! cartridge header (the existing, already-reviewed
//! [`crate::platform::MagicRule`] on the `MegaDrive` platform entry)
//! becomes visible again - see the `deinterleaved_bytes_reveal_the_existing_mega_drive_signature`
//! test.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

/// The SMD copier header length, in bytes.
pub const SMD_HEADER_LEN: usize = 512;

/// The interleave block size, in bytes (16 KiB).
pub const SMD_BLOCK_LEN: usize = 16384;

/// Half of [`SMD_BLOCK_LEN`] - the size of each of the two byte lanes an SMD
/// block is split into.
const SMD_HALF_LEN: usize = SMD_BLOCK_LEN / 2;

/// The stable identifier for this transform.
pub const SMD_DEINTERLEAVE_TRANSFORM_ID: &str = "smd-deinterleave-to-bin";

/// Whether `data`'s overall shape matches the SMD size rule: at least the
/// 512-byte header plus one whole 16 KiB block, with nothing left over.
///
/// This is evidence about *shape*, not content - exactly like
/// [`crate::header_normalization::recognize_snes_copier_candidate`], and for
/// the same reason: plenty of non-SMD data of the right length could satisfy
/// this by chance, so a `true` result here is real but weak evidence, never
/// proof. See the module documentation for why no header magic byte is
/// checked either.
pub fn detect_smd_candidate(data: &[u8]) -> bool {
    let Some(payload_len) = data.len().checked_sub(SMD_HEADER_LEN) else {
        return false;
    };
    payload_len > 0 && payload_len % SMD_BLOCK_LEN == 0
}

/// Why an SMD transform could not proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmdNormalizationError {
    /// `data` is shorter than the 512-byte copier header this transform
    /// expects to remove first.
    TooShortForHeader { required: usize, actual: usize },
    /// What remains after the header (for de-interleaving) or the whole
    /// input (for re-interleaving) is not a positive whole number of
    /// [`SMD_BLOCK_LEN`]-byte blocks - either empty (header-only input, or
    /// no canonical bytes at all) or leaves a partial trailing block. Never
    /// padded or truncated to fit.
    PayloadNotWholeBlocks {
        payload_len: usize,
        block_len: usize,
    },
}

impl SmdNormalizationError {
    pub fn detail(&self) -> String {
        match self {
            Self::TooShortForHeader { required, actual } => format!(
                "{actual} bytes is shorter than the {required}-byte SMD copier header this \
                 transform expects to remove"
            ),
            Self::PayloadNotWholeBlocks {
                payload_len,
                block_len,
            } => format!(
                "{payload_len} bytes is not a positive whole number of {block_len}-byte SMD \
                 blocks"
            ),
        }
    }
}

/// An SMD de-interleave result: the canonical (de-interleaved, headerless)
/// bytes, the exact copier header removed, and enough metadata to know what
/// happened.
///
/// `stripped_header` exists specifically so this transform is provably
/// reversible rather than merely asserted to be - see
/// [`reconstruct_smd_from_bin`] and this module's `reversible_*` tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmdNormalizationResult {
    pub transform_id: &'static str,
    /// The exact 512 bytes removed from the front of the file.
    pub stripped_header: Vec<u8>,
    /// The canonical, de-interleaved, headerless payload.
    pub bytes: Vec<u8>,
    pub block_count: usize,
}

/// De-interleaves `data`: strips the 512-byte copier header, then converts
/// every 16 KiB block from SMD byte order to canonical BIN/raw byte order,
/// using the verified rule documented at the top of this module.
///
/// Pure: `data` is read but never mutated, and both `bytes` and
/// `stripped_header` in the result are fresh, independently owned buffers.
/// The transform is applied uniformly across *every* block, not only the
/// first - see this module's multi-block tests, which use a distinctive,
/// non-repeating payload spanning several blocks specifically so a
/// first-block-only bug would be caught. Fails closed with
/// [`SmdNormalizationError`] - never padding, never truncating, never
/// silently processing only the complete blocks of a partially block-aligned
/// input - when `data` does not cleanly fit the header-plus-whole-blocks
/// shape.
pub fn normalize_smd_to_bin(data: &[u8]) -> Result<SmdNormalizationResult, SmdNormalizationError> {
    if data.len() < SMD_HEADER_LEN {
        return Err(SmdNormalizationError::TooShortForHeader {
            required: SMD_HEADER_LEN,
            actual: data.len(),
        });
    }
    let (header, payload) = data.split_at(SMD_HEADER_LEN);
    if payload.is_empty() || !payload.len().is_multiple_of(SMD_BLOCK_LEN) {
        return Err(SmdNormalizationError::PayloadNotWholeBlocks {
            payload_len: payload.len(),
            block_len: SMD_BLOCK_LEN,
        });
    }

    let mut canonical = vec![0u8; payload.len()];
    for (smd_block, canonical_block) in payload
        .chunks_exact(SMD_BLOCK_LEN)
        .zip(canonical.chunks_exact_mut(SMD_BLOCK_LEN))
    {
        let (lower_lane, upper_lane) = smd_block.split_at(SMD_HALF_LEN);
        for i in 0..SMD_HALF_LEN {
            // Verified rule: canonical[2*i] = smd_block[8192 + i] (upper
            // lane -> even canonical offsets); canonical[2*i + 1] =
            // smd_block[i] (lower lane -> odd canonical offsets).
            canonical_block[2 * i] = upper_lane[i];
            canonical_block[2 * i + 1] = lower_lane[i];
        }
    }

    Ok(SmdNormalizationResult {
        transform_id: SMD_DEINTERLEAVE_TRANSFORM_ID,
        stripped_header: header.to_vec(),
        bytes: canonical,
        block_count: payload.len() / SMD_BLOCK_LEN,
    })
}

/// The mathematical inverse of the per-block transform in
/// [`normalize_smd_to_bin`]: converts canonical BIN byte order back into SMD
/// byte order, one block at a time. Exposed on its own (rather than folded
/// silently into [`reconstruct_smd_from_bin`]) so a caller building a
/// synthetic SMD fixture from known-canonical bytes - exactly what this
/// module's own hash-identity tests do - never has to duplicate the index
/// math.
///
/// `canonical` must be a positive whole number of [`SMD_BLOCK_LEN`]-byte
/// blocks, with the same fail-closed behaviour as [`normalize_smd_to_bin`].
pub fn interleave_bin_to_smd(canonical: &[u8]) -> Result<Vec<u8>, SmdNormalizationError> {
    if canonical.is_empty() || !canonical.len().is_multiple_of(SMD_BLOCK_LEN) {
        return Err(SmdNormalizationError::PayloadNotWholeBlocks {
            payload_len: canonical.len(),
            block_len: SMD_BLOCK_LEN,
        });
    }

    let mut smd_payload = vec![0u8; canonical.len()];
    for (canonical_block, smd_block) in canonical
        .chunks_exact(SMD_BLOCK_LEN)
        .zip(smd_payload.chunks_exact_mut(SMD_BLOCK_LEN))
    {
        let (lower_lane, upper_lane) = smd_block.split_at_mut(SMD_HALF_LEN);
        for i in 0..SMD_HALF_LEN {
            upper_lane[i] = canonical_block[2 * i];
            lower_lane[i] = canonical_block[2 * i + 1];
        }
    }
    Ok(smd_payload)
}

/// Reconstructs the original SMD bytes from a [`SmdNormalizationResult`]:
/// re-interleaves `result.bytes` back to SMD byte order (via
/// [`interleave_bin_to_smd`]) and prepends `result.stripped_header`. This is
/// the reversibility proof made concrete - see this module's
/// `full_reconstruction_is_byte_for_byte_exact` test, which calls this and
/// asserts equality with the original input.
pub fn reconstruct_smd_from_bin(
    result: &SmdNormalizationResult,
) -> Result<Vec<u8>, SmdNormalizationError> {
    let payload = interleave_bin_to_smd(&result.bytes)?;
    let mut original = Vec::with_capacity(result.stripped_header.len() + payload.len());
    original.extend_from_slice(&result.stripped_header);
    original.extend_from_slice(&payload);
    Ok(original)
}

/// A [`ContentDetector`] for the SMD size-rule candidate.
///
/// Structural evidence only, at [`ContentEvidenceConfidence::Weak`] -
/// never `Strong`, and never a claim about Sega Mega Drive platform
/// identity. See the module documentation for why no header magic byte is
/// used to strengthen this, and this module's
/// `random_block_aligned_data_is_never_promoted_to_strong_evidence` test for
/// the enforced boundary.
pub struct SmdNormalizationDetector;

impl ContentDetector for SmdNormalizationDetector {
    fn id(&self) -> &'static str {
        "smd_deinterleave_candidate"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !detect_smd_candidate(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized {
            evidence: vec![ContentEvidence::new(
                ContentEvidenceKind::ContentSignature,
                SMD_DEINTERLEAVE_TRANSFORM_ID,
                ContentEvidenceConfidence::Weak,
                "file length matches the SMD size rule (512-byte header followed by whole \
                 16384-byte blocks); this is a reversible-canonicalization candidate based on \
                 size alone, never platform proof by itself",
            )],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::archive::hash::hash_member_stream;
    use std::sync::atomic::AtomicBool;

    /// SHA-256 of an in-memory buffer via the crate's existing
    /// [`hash_member_stream`] - no new hashing code, no new dependency.
    fn sha256_hex(data: &[u8]) -> String {
        hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
            .expect("hashing an in-memory buffer never fails")
            .hashes
            .sha256
    }

    /// A distinctive, non-repeating payload so a transform that only
    /// handles the first block, or corrupts anything past it, is caught
    /// rather than masked by a repeating pattern.
    fn distinctive_payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn distinctive_header() -> Vec<u8> {
        (0..SMD_HEADER_LEN)
            .map(|i| ((i * 3 + 7) % 251) as u8)
            .collect()
    }

    fn smd_fixture(header: &[u8], canonical_payload: &[u8]) -> Vec<u8> {
        let smd_payload = interleave_bin_to_smd(canonical_payload).unwrap();
        let mut data = header.to_vec();
        data.extend_from_slice(&smd_payload);
        data
    }

    // ------------------------------------------------------------------
    // Core transform
    // ------------------------------------------------------------------

    #[test]
    fn valid_one_block_smd_normalizes_correctly() {
        let canonical = distinctive_payload(SMD_BLOCK_LEN);
        let data = smd_fixture(&distinctive_header(), &canonical);
        let result = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(result.bytes, canonical);
        assert_eq!(result.block_count, 1);
    }

    #[test]
    fn valid_multi_block_smd_normalizes_every_block() {
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 4);
        let data = smd_fixture(&distinctive_header(), &canonical);
        let result = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(result.block_count, 4);
        assert_eq!(result.bytes, canonical);
        // Explicitly check bytes deep into the later blocks, not only the
        // first, so a first-block-only implementation bug cannot pass.
        assert_eq!(
            result.bytes[SMD_BLOCK_LEN * 3 + 100],
            canonical[SMD_BLOCK_LEN * 3 + 100]
        );
    }

    #[test]
    fn header_is_preserved_exactly() {
        let header = distinctive_header();
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 2);
        let data = smd_fixture(&header, &canonical);
        let result = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(result.stripped_header, header);
        assert_eq!(result.stripped_header.len(), SMD_HEADER_LEN);
    }

    #[test]
    fn canonical_output_excludes_the_copier_header() {
        let header = distinctive_header();
        let canonical = distinctive_payload(SMD_BLOCK_LEN);
        let data = smd_fixture(&header, &canonical);
        let result = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(result.bytes.len(), canonical.len());
        assert!(!result.bytes.starts_with(&header[..4]));
    }

    #[test]
    fn original_input_is_never_mutated() {
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 2);
        let data = smd_fixture(&distinctive_header(), &canonical);
        let before = data.clone();
        let _ = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(
            data, before,
            "normalize_smd_to_bin must never mutate its input"
        );
    }

    #[test]
    fn deterministic_repeated_normalization() {
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 3);
        let data = smd_fixture(&distinctive_header(), &canonical);
        let first = normalize_smd_to_bin(&data).unwrap();
        let second = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(first, second);
    }

    // ------------------------------------------------------------------
    // Fail-closed behaviour
    // ------------------------------------------------------------------

    #[test]
    fn malformed_payload_not_divisible_by_block_size_fails_closed() {
        let mut data = distinctive_header();
        data.extend_from_slice(&distinctive_payload(SMD_BLOCK_LEN + 37));
        let error = normalize_smd_to_bin(&data).unwrap_err();
        assert_eq!(
            error,
            SmdNormalizationError::PayloadNotWholeBlocks {
                payload_len: SMD_BLOCK_LEN + 37,
                block_len: SMD_BLOCK_LEN,
            }
        );
    }

    #[test]
    fn input_shorter_than_header_fails_safely() {
        let short = vec![0u8; SMD_HEADER_LEN - 1];
        let error = normalize_smd_to_bin(&short).unwrap_err();
        assert_eq!(
            error,
            SmdNormalizationError::TooShortForHeader {
                required: SMD_HEADER_LEN,
                actual: SMD_HEADER_LEN - 1,
            }
        );
    }

    #[test]
    fn header_only_input_fails_safely() {
        let header_only = distinctive_header();
        assert_eq!(header_only.len(), SMD_HEADER_LEN);
        let error = normalize_smd_to_bin(&header_only).unwrap_err();
        assert_eq!(
            error,
            SmdNormalizationError::PayloadNotWholeBlocks {
                payload_len: 0,
                block_len: SMD_BLOCK_LEN,
            }
        );
    }

    #[test]
    fn no_padding_or_truncation_on_malformed_input() {
        // A payload one byte short of two whole blocks must fail outright,
        // never silently process the one complete block and drop the rest.
        let mut data = distinctive_header();
        data.extend_from_slice(&distinctive_payload(SMD_BLOCK_LEN * 2 - 1));
        assert!(normalize_smd_to_bin(&data).is_err());
    }

    // ------------------------------------------------------------------
    // Reversibility
    // ------------------------------------------------------------------

    #[test]
    fn inverse_interleave_reconstructs_the_exact_canonical_payload() {
        // Exercises `interleave_bin_to_smd` (the standalone inverse) and
        // `normalize_smd_to_bin` (the forward transform) as two genuinely
        // independent functions, round-tripped through a real header, rather
        // than re-deriving the index math by hand in the test itself.
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 3);
        let smd_payload = interleave_bin_to_smd(&canonical).unwrap();
        let mut data = distinctive_header();
        data.extend_from_slice(&smd_payload);

        let result = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(result.bytes, canonical);
    }

    #[test]
    fn full_reconstruction_including_header_is_exact() {
        let header = distinctive_header();
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 3);
        let data = smd_fixture(&header, &canonical);
        let result = normalize_smd_to_bin(&data).unwrap();
        let reconstructed = reconstruct_smd_from_bin(&result).unwrap();
        assert_eq!(reconstructed, data);
    }

    #[test]
    fn reconstruction_fails_closed_for_a_malformed_normalized_buffer() {
        let odd_canonical = SmdNormalizationResult {
            transform_id: SMD_DEINTERLEAVE_TRANSFORM_ID,
            stripped_header: distinctive_header(),
            bytes: distinctive_payload(SMD_BLOCK_LEN + 1),
            block_count: 0,
        };
        assert!(reconstruct_smd_from_bin(&odd_canonical).is_err());
    }

    // ------------------------------------------------------------------
    // Physical vs. normalized identity
    // ------------------------------------------------------------------

    #[test]
    fn physical_hash_differs_from_canonical_bin_hash() {
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 2);
        let data = smd_fixture(&distinctive_header(), &canonical);
        assert_ne!(sha256_hex(&data), sha256_hex(&canonical));
    }

    #[test]
    fn normalized_hash_equals_canonical_bin_hash() {
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 2);
        let data = smd_fixture(&distinctive_header(), &canonical);
        let result = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(sha256_hex(&result.bytes), sha256_hex(&canonical));
    }

    #[test]
    fn deep_body_corruption_would_be_caught_by_equality() {
        // A synthetic fixture built from a distinctive multi-block payload,
        // corrupted deep in the last block after interleaving, must not
        // normalize back to the original canonical bytes - proving the
        // equality-based tests above are not vacuously true.
        let canonical = distinctive_payload(SMD_BLOCK_LEN * 3);
        let mut smd_payload = interleave_bin_to_smd(&canonical).unwrap();
        let corrupt_at = SMD_BLOCK_LEN * 2 + 100;
        smd_payload[corrupt_at] ^= 0xFF;
        let mut data = distinctive_header();
        data.extend_from_slice(&smd_payload);
        let result = normalize_smd_to_bin(&data).unwrap();
        assert_ne!(result.bytes, canonical);
    }

    // ------------------------------------------------------------------
    // Detection / evidence
    // ------------------------------------------------------------------

    #[test]
    fn filename_or_extension_is_never_consulted() {
        // There is no `Path`, `OsStr`, or filename parameter anywhere in
        // this module's public API - structurally, not just by convention.
        // This test exists to document that plainly: every function here
        // takes only `&[u8]`.
        let canonical = distinctive_payload(SMD_BLOCK_LEN);
        let data = smd_fixture(&distinctive_header(), &canonical);
        assert!(detect_smd_candidate(&data));
    }

    #[test]
    fn random_block_aligned_data_is_never_promoted_to_strong_evidence() {
        let random_but_block_aligned = distinctive_payload(SMD_HEADER_LEN + SMD_BLOCK_LEN * 2);
        assert!(detect_smd_candidate(&random_but_block_aligned));
        let outcome = SmdNormalizationDetector.detect(&random_but_block_aligned);
        assert!(outcome.is_recognized());
        let evidence = outcome.evidence();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Weak);
        assert_ne!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn non_block_aligned_data_is_not_recognized() {
        let data = distinctive_payload(SMD_HEADER_LEN + SMD_BLOCK_LEN + 1);
        assert!(!detect_smd_candidate(&data));
        assert_eq!(
            SmdNormalizationDetector.detect(&data),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn transform_id_is_stable() {
        assert_eq!(SMD_DEINTERLEAVE_TRANSFORM_ID, "smd-deinterleave-to-bin");
        let canonical = distinctive_payload(SMD_BLOCK_LEN);
        let data = smd_fixture(&distinctive_header(), &canonical);
        let result = normalize_smd_to_bin(&data).unwrap();
        assert_eq!(result.transform_id, SMD_DEINTERLEAVE_TRANSFORM_ID);
    }

    #[test]
    fn detector_id_is_stable() {
        assert_eq!(SmdNormalizationDetector.id(), SmdNormalizationDetector.id());
        assert_eq!(SmdNormalizationDetector.id(), "smd_deinterleave_candidate");
    }

    /// The corroboration this module's own documentation promises: a
    /// canonical Mega Drive ROM's `SEGA` cartridge header (the existing,
    /// unmodified `platform::PLATFORMS` `MegaDrive` `MagicRule`) is not
    /// visible in the raw SMD bytes, but becomes visible again once this
    /// module de-interleaves and strips the header - proving structural SMD
    /// evidence and real platform-signature evidence are genuinely
    /// different things that can be combined later, not conflated now.
    #[test]
    fn deinterleaved_bytes_reveal_the_existing_mega_drive_signature() {
        use crate::platform::{MagicConfidence, platform_magic_confidence_from_bytes};

        let mut canonical = distinctive_payload(SMD_BLOCK_LEN);
        canonical[0x100..0x104].copy_from_slice(b"SEGA");
        let data = smd_fixture(&distinctive_header(), &canonical);

        // The raw, still-interleaved SMD bytes do not carry the signature
        // at the raw offset the platform registry checks.
        let raw_matches = platform_magic_confidence_from_bytes(&data);
        assert!(!raw_matches.iter().any(|(id, _)| *id == "MegaDrive"));

        // After this module's own de-interleave, it does.
        let result = normalize_smd_to_bin(&data).unwrap();
        let normalized_matches = platform_magic_confidence_from_bytes(&result.bytes);
        assert!(
            normalized_matches
                .iter()
                .any(|(id, confidence)| *id == "MegaDrive"
                    && *confidence == MagicConfidence::Corroborated)
        );
    }
}
