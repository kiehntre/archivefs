//! Batch 8: which byte representation produced a DAT hash match.
//!
//! [`crate::dat::audit`] already compares caller-supplied hashes
//! ([`crate::dat::audit::KnownFileEvidence`]) against a [`DatIndex`] - this
//! module does not duplicate that comparison logic (`audit_one` is reused
//! directly, unchanged). What it adds is the missing piece Batch 7 reported
//! as a real gap: computing hashes from **more than one byte
//! representation** of the same file (physical, and - only where a
//! reversible normalization actually exists - the normalized/canonical
//! view) and recording, explicitly, which one produced a given match.
//!
//! # Reused, not duplicated
//!
//! - Hash *comparison*: [`crate::dat::audit::audit_one`]/[`AuditVerdict`],
//!   completely unchanged.
//! - Hash *values*: [`crate::dat::audit::KnownFileEvidence`] is the exact
//!   struct carried per representation here - no parallel CRC/MD5/SHA type.
//! - CRC32 *computation*: [`crate::identity_source::hashing::Crc32::of`],
//!   called directly rather than re-implemented.
//! - Normalization *transforms*: [`crate::n64_byte_order`],
//!   [`crate::header_normalization`], [`crate::smd_normalization`] - this
//!   module calls them, it does not reimplement byte-order swapping or
//!   header stripping.
//!
//! MD5/SHA-1/SHA-256 computation over an in-memory buffer has no existing
//! bytes-only helper to reuse ([`crate::identity_source::hashing::hash_file`]
//! is path/[`crate::safe_read::TrustedRoots`]-bound, for a different,
//! persisted-cache concern) - [`hash_bytes`] calls the already-dependency
//! `md5`/`sha1`/`sha2` crates the same way that module does internally.

use crate::dat::audit::{AuditVerdict, KnownFileEvidence, audit_one};
use crate::dat::index::DatIndex;
use crate::identity_source::hashing::Crc32;

/// Which bytes a hash was computed from - milestone section 3. A small,
/// typed model rather than prose in a detail string, so a caller can match
/// on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ByteRepresentation {
    /// The file's own bytes, exactly as stored.
    Physical,
    /// A reversible transform applied first - `transform` names it (e.g.
    /// `"n64_byte_order"`, matching
    /// [`crate::n64_byte_order::N64_BYTE_ORDER_TRANSFORM_ID`]/
    /// [`crate::header_normalization::HeaderNormalizationResult::transform_id`]/
    /// [`crate::smd_normalization::SmdNormalizationResult::transform_id`] -
    /// the same stable ids those modules already define, never a new
    /// string invented here).
    Normalized { transform: &'static str },
    /// One member's bytes inside an archive - `member_name` is the
    /// archive-internal path/name, never a filesystem path.
    ArchiveMember { member_name: String },
}

impl ByteRepresentation {
    pub fn is_physical(&self) -> bool {
        matches!(self, Self::Physical)
    }

    pub fn is_normalized(&self) -> bool {
        matches!(self, Self::Normalized { .. })
    }
}

/// One representation's computed hashes, ready to audit - milestone
/// section 4. Deliberately wraps the existing [`KnownFileEvidence`] rather
/// than inventing a parallel hash struct.
#[derive(Debug, Clone)]
pub struct RepresentationHashes {
    pub representation: ByteRepresentation,
    pub evidence: KnownFileEvidence,
}

/// Computes CRC32/MD5/SHA-1/SHA-256 over `bytes` in memory - no file I/O,
/// no path, no [`crate::safe_read::TrustedRoots`] involved (the caller
/// already has `bytes`, however it obtained them: a physical file read, a
/// normalized transform's output, or a bounded archive-member prefix/full
/// read). `filepath`/`filename` on the returned [`KnownFileEvidence`] are
/// display-only labels the caller supplies - never derived from `bytes`
/// itself.
pub fn hash_bytes(bytes: &[u8], filepath: &str, filename: &str) -> KnownFileEvidence {
    use md5::Md5;
    use sha1::Sha1;
    use sha1::digest::Digest as _;
    use sha2::Sha256;

    let mut md5 = Md5::new();
    md5.update(bytes);
    let mut sha1 = Sha1::new();
    sha1.update(bytes);
    let mut sha256 = Sha256::new();
    sha256.update(bytes);

    KnownFileEvidence::new(filepath, filename)
        .with_size(bytes.len() as u64)
        .with_crc32(Crc32::of(bytes))
        .with_md5(hex(&md5.finalize()))
        .with_sha1(hex(&sha1.finalize()))
        .with_sha256(hex(&sha256.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Builds one [`RepresentationHashes`] for `bytes` under `representation`.
pub fn observe_representation(
    representation: ByteRepresentation,
    bytes: &[u8],
    filepath: &str,
    filename: &str,
) -> RepresentationHashes {
    RepresentationHashes {
        representation,
        evidence: hash_bytes(bytes, filepath, filename),
    }
}

/// Whether physical `bytes` and `other` are literally the same buffer -
/// the byte-identity check [`RepresentationMatchOutcome::BothAgree`] needs
/// (milestone section 13), computed once here so every
/// `normalized_*_representation` function below reports it consistently.
fn bytes_identical(bytes: &[u8], other: &[u8]) -> bool {
    bytes == other
}

/// Computes the normalized representation for a physically-N64 image, when
/// [`crate::n64_byte_order::detect_n64_byte_order`] recognizes `bytes` at
/// all (`z64`/`v64`/`n64`). Milestone section 5's N64 row. Still computed
/// (never skipped) even when the physical order is already canonical `Z64`,
/// since that is the "both agree, byte-identical" case section 13 asks for,
/// not a reason to omit the normalized hash.
///
/// Returns `(RepresentationHashes, identical_to_physical)`.
pub fn normalized_n64_representation(
    bytes: &[u8],
    filepath: &str,
    filename: &str,
) -> Option<(RepresentationHashes, bool)> {
    use crate::n64_byte_order::{
        N64_BYTE_ORDER_TRANSFORM_ID, detect_n64_byte_order, normalize_to_z64,
    };

    let order = detect_n64_byte_order(bytes)?;
    let result = normalize_to_z64(bytes, order).ok()?;
    let identical = bytes_identical(bytes, &result.bytes);
    Some((
        observe_representation(
            ByteRepresentation::Normalized {
                transform: N64_BYTE_ORDER_TRANSFORM_ID,
            },
            &result.bytes,
            filepath,
            filename,
        ),
        identical,
    ))
}

/// Computes the normalized (headerless) representation for a cartridge
/// format with a known, reversible header - Lynx, Atari 7800, and the
/// (weakly-recognized, see
/// [`crate::header_normalization::recognize_snes_copier_candidate`])
/// SNES copier-header case. Milestone section 5's Lynx/Atari7800/SNES/
/// NES/FDS rows all go through the same
/// [`crate::header_normalization::strip_known_header`] entry point, so one
/// function covers all of them rather than one per format.
pub fn normalized_header_stripped_representation(
    bytes: &[u8],
    filepath: &str,
    filename: &str,
) -> Option<(RepresentationHashes, bool)> {
    use crate::header_normalization::{recognize_header_normalization, strip_known_header};

    let kind = *recognize_header_normalization(bytes).first()?;
    let result = strip_known_header(bytes, kind).ok()?;
    let identical = bytes_identical(bytes, &result.bytes);
    Some((
        observe_representation(
            ByteRepresentation::Normalized {
                transform: result.transform_id,
            },
            &result.bytes,
            filepath,
            filename,
        ),
        identical,
    ))
}

/// Computes the de-interleaved (canonical BIN) representation for an SMD
/// (Mega Drive copier-interleaved) image. Milestone section 5's SMD row.
/// [`crate::smd_normalization::detect_smd_candidate`] is deliberately weak
/// evidence on its own (see that function's own documentation) - this
/// function still only *computes* the normalized hash when the candidate
/// shape is recognized; it never asserts platform identity from that alone
/// (identity is fused separately, exactly as every other detector in this
/// crate already works).
pub fn normalized_smd_representation(
    bytes: &[u8],
    filepath: &str,
    filename: &str,
) -> Option<(RepresentationHashes, bool)> {
    use crate::smd_normalization::{
        SMD_DEINTERLEAVE_TRANSFORM_ID, detect_smd_candidate, normalize_smd_to_bin,
    };

    if !detect_smd_candidate(bytes) {
        return None;
    }
    let result = normalize_smd_to_bin(bytes).ok()?;
    let identical = bytes_identical(bytes, &result.bytes);
    Some((
        observe_representation(
            ByteRepresentation::Normalized {
                transform: SMD_DEINTERLEAVE_TRANSFORM_ID,
            },
            &result.bytes,
            filepath,
            filename,
        ),
        identical,
    ))
}

/// Audits one [`RepresentationHashes`] against `index`, returning the same
/// [`ByteRepresentation`] alongside the [`AuditVerdict`] it produced - the
/// pairing milestone section 4/12 asks for ("a DAT hash observation must
/// know... byte representation").
pub fn audit_representation(
    hashes: &RepresentationHashes,
    index: &DatIndex,
) -> (ByteRepresentation, AuditVerdict) {
    (
        hashes.representation.clone(),
        audit_one(&hashes.evidence, index),
    )
}

/// How the physical and (optional) normalized representations' own DAT
/// audits relate - milestone section 7. Never silently prefers either
/// representation (section 8): a genuine platform/game disagreement
/// between confident verdicts is always [`Self::Disagree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepresentationMatchOutcome {
    /// Only the physical representation was confidently matched (or no
    /// normalized representation exists/was computed for this format).
    PhysicalOnly { verdict: AuditVerdict },
    /// Only the normalized representation was confidently matched -
    /// physical bytes alone do not match anything in the DAT. This is the
    /// milestone's own headline case (section 12).
    NormalizedOnly { verdict: AuditVerdict },
    /// Both representations were confidently matched, to the *same*
    /// game/release evidence. See [`Self::BothAgree`]'s own doc note on
    /// physical==normalized byte-identity (section 13) - this variant
    /// covers both "genuinely two independent confident matches that
    /// happen to agree" and "physical bytes and normalized bytes were
    /// byte-identical, so there was really only one representation to
    /// begin with" - `identical_bytes` distinguishes the two so a caller
    /// never double-counts the latter as two independent proofs.
    BothAgree {
        verdict: AuditVerdict,
        identical_bytes: bool,
    },
    /// Both representations were confidently matched, to *different*
    /// game/release evidence - fails closed, never "physical wins" or
    /// "normalized wins" (section 7/8, non-negotiable).
    Disagree {
        physical_verdict: AuditVerdict,
        normalized_verdict: AuditVerdict,
    },
    /// Neither representation produced a confident match.
    NoMatch,
}

impl RepresentationMatchOutcome {
    pub fn is_confident(&self) -> bool {
        !matches!(self, Self::NoMatch)
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Disagree { .. })
    }
}

/// Combines a physical audit verdict with an optional normalized audit
/// verdict into one [`RepresentationMatchOutcome`]. `identical_bytes`
/// records whether the physical and normalized byte buffers that produced
/// the two verdicts were literally the same bytes (e.g. an already-canonical
/// z64 file) - see [`RepresentationMatchOutcome::BothAgree`].
///
/// "Confident" here matches [`AuditVerdict::is_confident`] exactly - only
/// cryptographic-hash verdicts (`Exact`/`ExactMultipleCandidates`)
/// participate; a `Probable` CRC32-only match on either side is retained on
/// the returned outcome's own verdict field for provenance but never
/// upgrades a representation to `PhysicalOnly`/`NormalizedOnly`/`BothAgree`/
/// `Disagree` on its own - it falls through to whichever confident branch
/// applies, or `NoMatch` if neither side is confident.
pub fn compare_representations(
    physical_verdict: AuditVerdict,
    normalized: Option<AuditVerdict>,
    identical_bytes: bool,
) -> RepresentationMatchOutcome {
    let physical_confident = physical_verdict.is_confident();
    match normalized {
        None => {
            if physical_confident {
                RepresentationMatchOutcome::PhysicalOnly {
                    verdict: physical_verdict,
                }
            } else {
                RepresentationMatchOutcome::NoMatch
            }
        }
        Some(normalized_verdict) => {
            let normalized_confident = normalized_verdict.is_confident();
            match (physical_confident, normalized_confident) {
                (true, true) => {
                    if verdict_names_same_game(&physical_verdict, &normalized_verdict) {
                        RepresentationMatchOutcome::BothAgree {
                            verdict: physical_verdict,
                            identical_bytes,
                        }
                    } else {
                        RepresentationMatchOutcome::Disagree {
                            physical_verdict,
                            normalized_verdict,
                        }
                    }
                }
                (true, false) => RepresentationMatchOutcome::PhysicalOnly {
                    verdict: physical_verdict,
                },
                (false, true) => RepresentationMatchOutcome::NormalizedOnly {
                    verdict: normalized_verdict,
                },
                (false, false) => RepresentationMatchOutcome::NoMatch,
            }
        }
    }
}

/// Whether two confident verdicts name the same game - by `game_name`
/// where both are `Exact`, since that field is exactly the DAT release
/// identity this comparison cares about. Multi-candidate verdicts are
/// compared by their candidate sets overlapping (any shared name), never
/// silently narrowed to "the first one."
fn verdict_names_same_game(left: &AuditVerdict, right: &AuditVerdict) -> bool {
    fn names(verdict: &AuditVerdict) -> Vec<&str> {
        match verdict {
            AuditVerdict::Exact { game_name, .. } => vec![game_name.as_str()],
            AuditVerdict::ExactMultipleCandidates { game_names, .. } => {
                game_names.iter().map(String::as_str).collect()
            }
            _ => Vec::new(),
        }
    }
    let left_names = names(left);
    let right_names = names(right);
    left_names.iter().any(|name| right_names.contains(name))
}

#[cfg(test)]
mod tests;
