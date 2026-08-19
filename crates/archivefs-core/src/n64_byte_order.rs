//! Pure, read-only Nintendo 64 byte-order detection and normalization.
//!
//! This is a small, isolated prototype, not a general `ContentIdentity`
//! implementation. It exists to prove one thing: that `z64`, `v64`, and
//! `n64` dumps of the *same ROM* can each keep their own distinct physical
//! identity (their own bytes, their own hash) while all producing the
//! *same* normalized identity once their byte order is reduced to one
//! canonical form. Nothing in this module writes a file, renames anything,
//! or decides a canonical platform - see the module-level safety notes on
//! [`normalize_to_z64`] and [`N64ByteOrderDetector`] for exactly where the
//! boundaries are.
//!
//! # Why three byte orders exist
//!
//! Real N64 dumps circulate in three interleavings of the same ROM data,
//! distinguished by the first four bytes:
//!
//! | Order | First 4 bytes | Convention |
//! |---|---|---|
//! | `Z64` | `80 37 12 40` | native big-endian - the canonical order this module normalizes to |
//! | `V64` | `37 80 40 12` | adjacent bytes swapped in pairs |
//! | `N64` | `40 12 37 80` | each 4-byte word reversed |
//!
//! # Physical identity is never replaced
//!
//! [`normalize_to_z64`] and [`denormalize_from_z64`] both return a *new*
//! `Vec<u8>`; the `data: &[u8]` they are given is an immutable borrow and is
//! never touched. A caller that wants both identities keeps both buffers -
//! this module never decides which one "is" the file.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

/// Which of the three known N64 dump byte orders a header matches.
///
/// `Z64` is the canonical order every [`normalize_to_z64`] call reduces to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum N64ByteOrder {
    /// Native big-endian: `80 37 12 40`. Canonical.
    Z64,
    /// Byte-swapped: `37 80 40 12`. Adjacent bytes exchanged in pairs.
    V64,
    /// Word-swapped: `40 12 37 80`. Each 4-byte word reversed.
    N64,
}

impl N64ByteOrder {
    /// The literal four magic bytes for this order, in file order.
    pub const fn magic(self) -> [u8; 4] {
        match self {
            Self::Z64 => [0x80, 0x37, 0x12, 0x40],
            Self::V64 => [0x37, 0x80, 0x40, 0x12],
            Self::N64 => [0x40, 0x12, 0x37, 0x80],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Z64 => "z64",
            Self::V64 => "v64",
            Self::N64 => "n64",
        }
    }
}

/// Detects which N64 byte order `data`'s first four bytes match, if any.
///
/// Pure: reads only `data[0..4]`, performs no I/O, and never infers
/// anything from a filename or extension - the byte-order magic is the only
/// evidence this function considers. `data` shorter than four bytes, or a
/// four-byte prefix that matches none of the three known orders, both
/// return `None` - "not enough to tell" and "not this format at all" are
/// deliberately the same safe outcome here (the sibling [`normalize_to_z64`]
/// is where a *different* kind of failure - malformed body length - gets
/// its own, separate error).
pub fn detect_n64_byte_order(data: &[u8]) -> Option<N64ByteOrder> {
    let header: [u8; 4] = data.get(0..4)?.try_into().ok()?;
    match header {
        [0x80, 0x37, 0x12, 0x40] => Some(N64ByteOrder::Z64),
        [0x37, 0x80, 0x40, 0x12] => Some(N64ByteOrder::V64),
        [0x40, 0x12, 0x37, 0x80] => Some(N64ByteOrder::N64),
        _ => None,
    }
}

/// Why a byte-order transform could not be applied.
///
/// Both variants exist because this module refuses to guess: a `V64` file
/// must be a whole number of 2-byte pairs and an `N64` file must be a whole
/// number of 4-byte words, or the transform does not know what to do with
/// the leftover bytes. Neither case is padded or truncated to make it fit -
/// that would silently invent or discard ROM data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum N64ByteOrderTransformError {
    /// A `V64` transform needs a whole number of 2-byte pairs.
    OddV64Length { length: usize },
    /// An `N64` transform needs a whole number of 4-byte words.
    NonMultipleOfFourN64Length { length: usize },
}

impl N64ByteOrderTransformError {
    pub fn detail(&self) -> String {
        match self {
            Self::OddV64Length { length } => format!(
                "v64 byte order requires a whole number of 2-byte pairs; {length} bytes is odd"
            ),
            Self::NonMultipleOfFourN64Length { length } => format!(
                "n64 byte order requires a whole number of 4-byte words; {length} is not a \
                 multiple of 4"
            ),
        }
    }
}

/// The stable identifier for the transform this module applies. The
/// smallest useful thing to record alongside a normalization result -
/// see [`N64NormalizationResult`].
pub const N64_BYTE_ORDER_TRANSFORM_ID: &str = "n64-byte-order-to-z64";

/// A normalization result: the canonical bytes, which transform produced
/// them, and which physical byte order they came from.
///
/// Deliberately minimal - this is not the eventual `ContentIdentity` model,
/// just enough to record "these bytes are canonical-order copies of a file
/// that was physically `source_order`, via `transform`." `bytes` is always a
/// fresh, independently owned buffer; it never aliases or replaces whatever
/// buffer the caller read the original file into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct N64NormalizationResult {
    pub transform: &'static str,
    pub source_order: N64ByteOrder,
    pub bytes: Vec<u8>,
}

/// Converts `data`, physically stored in `order`, into canonical `Z64`
/// (native big-endian) byte order.
///
/// Pure and allocating: `data` is read but never mutated, and the result is
/// always a new `Vec<u8>` covering every byte of `data` - the whole ROM, not
/// only its header. `Z64` input is copied unchanged (it already is
/// canonical); `V64` input has every adjacent byte pair swapped; `N64` input
/// has every 4-byte word reversed. Both transforms are applied uniformly
/// across the entire length of `data`, so a mismatch anywhere in the body -
/// not only in the first four bytes - would be caught by comparing the
/// result against a real canonical copy (see this module's tests for a
/// payload constructed specifically to expose a header-only transform bug).
///
/// Returns [`N64ByteOrderTransformError`] rather than padding or truncating
/// when `data`'s length does not evenly divide into the unit the transform
/// needs (2 bytes for `V64`, 4 bytes for `N64`).
pub fn normalize_to_z64(
    data: &[u8],
    order: N64ByteOrder,
) -> Result<N64NormalizationResult, N64ByteOrderTransformError> {
    Ok(N64NormalizationResult {
        transform: N64_BYTE_ORDER_TRANSFORM_ID,
        source_order: order,
        bytes: apply_byte_order_transform(data, order)?,
    })
}

/// Converts canonical `Z64` bytes back into physical `order`.
///
/// Every transform this module applies is its own inverse - swapping a byte
/// pair twice, or reversing a 4-byte word twice, reproduces the original
/// bytes - so this is implemented as the *same* transform
/// [`normalize_to_z64`] uses, called in the other direction. It is exposed
/// as its own function so a caller reasoning about "physical -> canonical"
/// versus "canonical -> physical" never has to know that fact themselves.
/// See this module's `reversibility_*` tests for the round-trip proof.
pub fn denormalize_from_z64(
    canonical: &[u8],
    order: N64ByteOrder,
) -> Result<Vec<u8>, N64ByteOrderTransformError> {
    apply_byte_order_transform(canonical, order)
}

/// The shared, self-inverse transform both directions use. `Z64` is the
/// identity transform; `V64` swaps each adjacent byte pair; `N64` reverses
/// each 4-byte word. Never mutates `data` - always returns a fresh buffer.
fn apply_byte_order_transform(
    data: &[u8],
    order: N64ByteOrder,
) -> Result<Vec<u8>, N64ByteOrderTransformError> {
    match order {
        N64ByteOrder::Z64 => Ok(data.to_vec()),
        N64ByteOrder::V64 => {
            if !data.len().is_multiple_of(2) {
                return Err(N64ByteOrderTransformError::OddV64Length { length: data.len() });
            }
            let mut bytes = data.to_vec();
            for pair in bytes.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
            Ok(bytes)
        }
        N64ByteOrder::N64 => {
            if !data.len().is_multiple_of(4) {
                return Err(N64ByteOrderTransformError::NonMultipleOfFourN64Length {
                    length: data.len(),
                });
            }
            let mut bytes = data.to_vec();
            for word in bytes.chunks_exact_mut(4) {
                word.reverse();
            }
            Ok(bytes)
        }
    }
}

/// A [`crate::content_detector::ContentDetector`] that recognises the N64
/// header byte-order signature.
///
/// This is header-signature detection only, exactly matching
/// [`detect_n64_byte_order`]. It never attempts [`normalize_to_z64`] and so
/// never reports [`ContentDetectionOutcome::Malformed`]: a mismatched body
/// length is a normalization-time concern, not a header-detection-time one,
/// and mixing the two would make this detector's outcome depend on how much
/// of the ROM the caller happened to hand it. The emitted evidence is
/// deliberately at [`ContentEvidenceKind::ContentSignature`], a fact about
/// the *bytes*, not a platform: [`crate::platform::PLATFORMS`] only
/// registers a [`crate::platform::MagicRule`] for the `Z64` form today
/// (`0x80371240`), so calling `V64`/`N64` header recognition "N64 platform
/// evidence" would overclaim what the existing canonical registry actually
/// knows. This detector reports the content fact only; whether or how that
/// ever becomes platform evidence is a deliberately deferred, separately
/// reviewed decision - see this module's test suite for a read-only
/// demonstration of today's gap.
pub struct N64ByteOrderDetector;

impl ContentDetector for N64ByteOrderDetector {
    fn id(&self) -> &'static str {
        "n64_byte_order_header"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        let Some(order) = detect_n64_byte_order(data) else {
            return ContentDetectionOutcome::NotRecognized;
        };
        ContentDetectionOutcome::Recognized {
            evidence: vec![ContentEvidence::new(
                ContentEvidenceKind::ContentSignature,
                order.label(),
                ContentEvidenceConfidence::Strong,
                format!(
                    "the first 4 bytes match the {} N64 ROM byte-order header",
                    order.label()
                ),
            )],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest possible SHA-256 helper this prototype needs: an
    /// in-memory digest of an in-memory buffer, no file, no path. The
    /// crate's existing hashing helpers
    /// (`identity_source::hashing::hash_file*`) are all file-based and
    /// bounded-read-policy-aware, which is the wrong tool for hashing bytes
    /// already held in memory - this is a deliberately tiny, test-local
    /// stand-in rather than a new production hashing API.
    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// A synthetic 16-byte canonical (`Z64`) N64 ROM: the real header magic
    /// followed by three more words, each with a distinct, recognisable
    /// pattern. Distinct per-word patterns matter - a transform that
    /// mistakenly only reorders the header and copies the rest verbatim
    /// would still pass a test built from a repeating body, but would fail
    /// against this one.
    fn synthetic_canonical_rom() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&N64ByteOrder::Z64.magic());
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0f]); // word 1
        bytes.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]); // word 2
        bytes.extend_from_slice(&[0x01, 0x23, 0x45, 0x67]); // word 3
        bytes
    }

    /// The same ROM, physically stored as `V64` (adjacent bytes swapped in
    /// pairs), derived independently of [`apply_byte_order_transform`] so
    /// the test does not just check the transform against itself.
    fn synthetic_v64_rom() -> Vec<u8> {
        let mut bytes = synthetic_canonical_rom();
        for pair in bytes.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
        bytes
    }

    /// The same ROM, physically stored as `N64` (each 4-byte word reversed),
    /// derived independently of [`apply_byte_order_transform`].
    fn synthetic_n64_rom() -> Vec<u8> {
        let mut bytes = synthetic_canonical_rom();
        for word in bytes.chunks_exact_mut(4) {
            word.reverse();
        }
        bytes
    }

    // ------------------------------------------------------------------
    // 1. Detection
    // ------------------------------------------------------------------

    #[test]
    fn detects_z64_magic() {
        assert_eq!(
            detect_n64_byte_order(&N64ByteOrder::Z64.magic()),
            Some(N64ByteOrder::Z64)
        );
    }

    #[test]
    fn detects_v64_magic() {
        assert_eq!(
            detect_n64_byte_order(&N64ByteOrder::V64.magic()),
            Some(N64ByteOrder::V64)
        );
    }

    #[test]
    fn detects_n64_magic() {
        assert_eq!(
            detect_n64_byte_order(&N64ByteOrder::N64.magic()),
            Some(N64ByteOrder::N64)
        );
    }

    #[test]
    fn unknown_magic_is_not_recognized() {
        assert_eq!(detect_n64_byte_order(&[0x00, 0x11, 0x22, 0x33]), None);
        assert_eq!(detect_n64_byte_order(b"NES\x1a"), None);
    }

    #[test]
    fn truncated_header_is_not_recognized() {
        assert_eq!(detect_n64_byte_order(&[]), None);
        assert_eq!(detect_n64_byte_order(&[0x80]), None);
        assert_eq!(detect_n64_byte_order(&[0x80, 0x37, 0x12]), None);
    }

    // ------------------------------------------------------------------
    // 2. Normalization
    // ------------------------------------------------------------------

    #[test]
    fn z64_normalization_is_unchanged() {
        let rom = synthetic_canonical_rom();
        let result = normalize_to_z64(&rom, N64ByteOrder::Z64).unwrap();
        assert_eq!(result.bytes, rom);
        assert_eq!(result.transform, N64_BYTE_ORDER_TRANSFORM_ID);
        assert_eq!(result.source_order, N64ByteOrder::Z64);
    }

    #[test]
    fn v64_normalizes_to_correct_z64() {
        let v64 = synthetic_v64_rom();
        let result = normalize_to_z64(&v64, N64ByteOrder::V64).unwrap();
        assert_eq!(result.bytes, synthetic_canonical_rom());
    }

    #[test]
    fn n64_normalizes_to_correct_z64() {
        let n64 = synthetic_n64_rom();
        let result = normalize_to_z64(&n64, N64ByteOrder::N64).unwrap();
        assert_eq!(result.bytes, synthetic_canonical_rom());
    }

    #[test]
    fn transformation_covers_every_word_not_only_the_header() {
        // Both fixture builders already transform the whole 16-byte buffer,
        // not just the first 4 bytes; asserting equality against the full
        // canonical buffer (not just its header) is what actually exercises
        // this. A header-only transform bug would leave bytes 4..16 in the
        // wrong order and fail this assertion.
        let canonical = synthetic_canonical_rom();
        let from_v64 = normalize_to_z64(&synthetic_v64_rom(), N64ByteOrder::V64).unwrap();
        let from_n64 = normalize_to_z64(&synthetic_n64_rom(), N64ByteOrder::N64).unwrap();
        assert_eq!(from_v64.bytes, canonical);
        assert_eq!(from_n64.bytes, canonical);
        assert_eq!(from_v64.bytes.len(), 16);
        assert_eq!(from_n64.bytes.len(), 16);
    }

    #[test]
    fn malformed_v64_odd_length_fails_closed() {
        let odd = vec![0x37, 0x80, 0x40, 0x12, 0xff]; // 5 bytes
        let error = normalize_to_z64(&odd, N64ByteOrder::V64).unwrap_err();
        assert_eq!(
            error,
            N64ByteOrderTransformError::OddV64Length { length: 5 }
        );
    }

    #[test]
    fn malformed_n64_non_multiple_of_four_length_fails_closed() {
        let short = vec![0x40, 0x12, 0x37, 0x80, 0x01, 0x02]; // 6 bytes
        let error = normalize_to_z64(&short, N64ByteOrder::N64).unwrap_err();
        assert_eq!(
            error,
            N64ByteOrderTransformError::NonMultipleOfFourN64Length { length: 6 }
        );
    }

    #[test]
    fn original_input_is_never_mutated() {
        let v64 = synthetic_v64_rom();
        let before = v64.clone();
        let _ = normalize_to_z64(&v64, N64ByteOrder::V64).unwrap();
        assert_eq!(v64, before, "normalize_to_z64 must never mutate its input");
    }

    // ------------------------------------------------------------------
    // 3/4. Physical vs. normalized identity
    // ------------------------------------------------------------------

    #[test]
    fn all_three_forms_normalize_to_identical_bytes() {
        let z64_result = normalize_to_z64(&synthetic_canonical_rom(), N64ByteOrder::Z64).unwrap();
        let v64_result = normalize_to_z64(&synthetic_v64_rom(), N64ByteOrder::V64).unwrap();
        let n64_result = normalize_to_z64(&synthetic_n64_rom(), N64ByteOrder::N64).unwrap();
        assert_eq!(z64_result.bytes, v64_result.bytes);
        assert_eq!(v64_result.bytes, n64_result.bytes);
    }

    #[test]
    fn physical_hashes_differ() {
        let z64 = synthetic_canonical_rom();
        let v64 = synthetic_v64_rom();
        let n64 = synthetic_n64_rom();

        let z64_hash = sha256_hex(&z64);
        let v64_hash = sha256_hex(&v64);
        let n64_hash = sha256_hex(&n64);

        assert_ne!(z64_hash, v64_hash);
        assert_ne!(z64_hash, n64_hash);
        assert_ne!(v64_hash, n64_hash);
    }

    #[test]
    fn normalized_hashes_agree() {
        let z64_result = normalize_to_z64(&synthetic_canonical_rom(), N64ByteOrder::Z64).unwrap();
        let v64_result = normalize_to_z64(&synthetic_v64_rom(), N64ByteOrder::V64).unwrap();
        let n64_result = normalize_to_z64(&synthetic_n64_rom(), N64ByteOrder::N64).unwrap();

        let z64_hash = sha256_hex(&z64_result.bytes);
        let v64_hash = sha256_hex(&v64_result.bytes);
        let n64_hash = sha256_hex(&n64_result.bytes);

        assert_eq!(z64_hash, v64_hash);
        assert_eq!(v64_hash, n64_hash);
    }

    #[test]
    fn physical_and_normalized_hashes_differ_for_non_canonical_forms() {
        // The point of the whole prototype, stated as one assertion: a v64
        // dump's own physical hash is not the same as its normalized hash,
        // proving the two identities genuinely coexist rather than one
        // silently standing in for the other.
        let v64 = synthetic_v64_rom();
        let physical_hash = sha256_hex(&v64);
        let normalized_hash = sha256_hex(&normalize_to_z64(&v64, N64ByteOrder::V64).unwrap().bytes);
        assert_ne!(physical_hash, normalized_hash);
    }

    #[test]
    fn repeated_normalization_is_deterministic() {
        let v64 = synthetic_v64_rom();
        let first = normalize_to_z64(&v64, N64ByteOrder::V64).unwrap();
        let second = normalize_to_z64(&v64, N64ByteOrder::V64).unwrap();
        assert_eq!(first, second);
    }

    // ------------------------------------------------------------------
    // 6. Reversibility
    // ------------------------------------------------------------------

    #[test]
    fn reversibility_z64() {
        let original = synthetic_canonical_rom();
        let canonical = normalize_to_z64(&original, N64ByteOrder::Z64).unwrap();
        let restored = denormalize_from_z64(&canonical.bytes, N64ByteOrder::Z64).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn reversibility_v64() {
        let original = synthetic_v64_rom();
        let canonical = normalize_to_z64(&original, N64ByteOrder::V64).unwrap();
        let restored = denormalize_from_z64(&canonical.bytes, N64ByteOrder::V64).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn reversibility_n64() {
        let original = synthetic_n64_rom();
        let canonical = normalize_to_z64(&original, N64ByteOrder::N64).unwrap();
        let restored = denormalize_from_z64(&canonical.bytes, N64ByteOrder::N64).unwrap();
        assert_eq!(restored, original);
    }

    // ------------------------------------------------------------------
    // 5. ContentEvidence integration
    // ------------------------------------------------------------------

    #[test]
    fn detector_recognizes_all_three_orders_as_strong_content_signature_evidence() {
        for (bytes, label) in [
            (N64ByteOrder::Z64.magic().to_vec(), "z64"),
            (N64ByteOrder::V64.magic().to_vec(), "v64"),
            (N64ByteOrder::N64.magic().to_vec(), "n64"),
        ] {
            let outcome = N64ByteOrderDetector.detect(&bytes);
            assert!(outcome.is_recognized());
            let evidence = outcome.evidence();
            assert_eq!(evidence.len(), 1);
            assert_eq!(evidence[0].kind, ContentEvidenceKind::ContentSignature);
            assert_eq!(evidence[0].value, label);
            assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
        }
    }

    #[test]
    fn detector_reports_not_recognized_for_unrelated_bytes() {
        let outcome = N64ByteOrderDetector.detect(b"not an n64 rom at all!!");
        assert_eq!(outcome, ContentDetectionOutcome::NotRecognized);
    }

    #[test]
    fn detector_id_is_stable() {
        assert_eq!(N64ByteOrderDetector.id(), N64ByteOrderDetector.id());
        assert_eq!(N64ByteOrderDetector.id(), "n64_byte_order_header");
    }

    #[test]
    fn existing_platform_registry_only_treats_the_z64_header_as_strong_today() {
        // Read-only demonstration of the gap this module's own docs call
        // out: crate::platform::PLATFORMS registers a MagicRule for the
        // canonical z64 header (0x80371240) at Strong confidence, but has no
        // rule at all for the raw v64/n64 byte patterns. This test touches
        // only the existing, unmodified platform registry - it adds no new
        // MagicRule and changes no platform behaviour.
        use crate::platform::{MagicConfidence, platform_magic_confidence_from_bytes};

        let z64_matches = platform_magic_confidence_from_bytes(&N64ByteOrder::Z64.magic());
        assert!(
            z64_matches
                .iter()
                .any(|(id, confidence)| *id == "N64" && *confidence == MagicConfidence::Strong)
        );

        let v64_matches = platform_magic_confidence_from_bytes(&N64ByteOrder::V64.magic());
        let n64_matches = platform_magic_confidence_from_bytes(&N64ByteOrder::N64.magic());
        assert!(
            !v64_matches.iter().any(|(id, _)| *id == "N64"),
            "the platform registry does not (yet) recognise raw v64 bytes as N64"
        );
        assert!(
            !n64_matches.iter().any(|(id, _)| *id == "N64"),
            "the platform registry does not (yet) recognise raw n64 bytes as N64"
        );
    }
}
