//! Experimental archive-member evidence abstraction (POST-ALPHA-1.1).
//!
//! The smallest reusable shape needed for member-aware DAT verification
//! across archive formats. It is intentionally narrow: one source trait that
//! enumerates members in deterministic order and yields per-member hashed
//! evidence, plus the evidence/status/error types. It does **not** define a
//! DAT-verification engine — members are hashed here; matching against a DAT
//! stays a separate consumer (`DatIndex`/`audit_one`).
//!
//! # Status
//!
//! Zero production callers. The only implementations today are focused tests
//! (`dat::archive::sevenz`). Nothing in shipped code calls into this module,
//! so it is safe to change without affecting any current behavior. See
//! `docs/research/SEVEN_Z_RAR_ARCHIVE_VERIFICATION_RESEARCH.md` §12 (AR-1).
//!
//! # Determinism and safety invariants
//!
//! - Members are enumerated in the archive's own deterministic order; nothing
//!   ever picks a member "by position" as a winner.
//! - Hashing is bounded, chunked, and cancellable; refusal of a member stops
//!   verification of the rest of the archive (later members are not
//!   evaluated). This is fail-closed: after a refusal the caller must treat
//!   the archive as not fully verified.
//! - Nested archives are surfaced (with [`ArchiveMemberStatus::NestedArchive`])
//!   but never recursively opened or hashed.

use std::sync::atomic::AtomicBool;

pub mod hash;
pub mod limits;
pub mod sevenz;
pub mod sevenz_preflight;

/// One member's cryptographic hashes, computed over its decompressed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMemberHashes {
    pub crc32: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

/// The outcome for one archive member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveMemberStatus {
    /// The member streamed and was hashed within limits, and the number of
    /// bytes actually hashed matched its declared logical size exactly. A
    /// decode that ended early is [`ArchiveMemberStatus::Corrupt`], never this.
    Verified,
    /// An empty stream member (zero logical size); surfaced, not hashed.
    EmptyFile,
    /// A nested-archive member (e.g. a `.zip` inside the `.7z`). Surfaced with
    /// metadata but never recursively opened and never hashed.
    NestedArchive,
    /// The member is encrypted; it is never decrypted.
    Encrypted,
    /// The member uses a compression method this build cannot decode.
    UnsupportedCodec { method: String },
    /// A configured limit was hit (member size, total logical budget, solid
    /// decode budget, dictionary size, compression ratio, member count).
    RefusedLimits { reason: &'static str },
    /// The member or its archive is corrupt (checksum/decode failure).
    Corrupt { detail: String },
}

/// Format-neutral per-member evidence produced by an [`ArchiveMemberSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMemberEvidence {
    /// The outer archive this member belongs to (display path or name), for
    /// provenance. Set by the source at construction; stable across runs for
    /// the same archive.
    pub archive: String,
    /// The member's stored name. Display-oriented: a future ZIP source must
    /// supply a lossless representation of raw (possibly non-UTF-8) member
    /// names rather than forcing a lossy conversion into this `String`.
    pub name: String,
    /// Position of this member in the source's deterministic enumeration.
    pub index: usize,
    /// The member's declared logical (uncompressed) size in bytes.
    pub logical_size: u64,
    /// Whether the member name looks like a nested archive. This is *evidence
    /// about the member*, not a policy decision: the source never recursively
    /// opens a member; consumers must not read a `NestedArchive` member's
    /// content.
    pub is_nested_archive: bool,
    pub status: ArchiveMemberStatus,
    /// Present only when [`ArchiveMemberStatus::Verified`].
    pub hashes: Option<ArchiveMemberHashes>,
}

impl ArchiveMemberEvidence {
    pub fn is_verified(&self) -> bool {
        self.status == ArchiveMemberStatus::Verified
    }
}

/// A source-level failure that prevents opening or fully verifying an archive.
///
/// Member-level problems are reported through
/// [`ArchiveMemberEvidence::status`]; this error type is reserved for
/// everything that stops the whole pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveMemberSourceError {
    /// The operation was cancelled mid-decode/hash.
    Cancelled,
    /// The source could not be opened under the read policy.
    Open { detail: String },
    /// The archive is corrupt (bad signature/header/checksum).
    Corrupt { detail: String },
    /// The archive is encrypted (header or member) and is never decrypted.
    Encrypted,
    /// The archive or a whole folder uses an unsupported feature.
    Unsupported { detail: String },
    /// A configured limit was hit before any member could be decoded.
    RefusedLimits { reason: &'static str },
}

/// A sequential, bounded source of archive-member evidence.
///
/// Implementations open the outer file through `safe_read`/`TrustedRoots`,
/// enumerate members deterministically, stream each member's decompressed
/// bytes into bounded hashes, and hand the evidence to `visit`. Returning
/// `Ok(false)` from `visit` stops iteration early; `Err` aborts.
///
/// The trait is **object-safe** (`visit` is a `dyn` callback) so a future
/// consumer can hold `Box<dyn ArchiveMemberSource>` without specialising on
/// the concrete format.
pub trait ArchiveMemberSource {
    /// A short, stable format name for diagnostics ("7z", "zip", "rar", …).
    fn archive_format(&self) -> &'static str;

    /// Number of stream-bearing members in deterministic order.
    fn member_count(&self) -> usize;

    /// Visit every member in deterministic order, hashing each within limits.
    ///
    /// `cancel` is checked during decode/hash at useful granularity. On the
    /// first member that cannot be verified, that member's evidence is
    /// emitted with a non-`Verified` status and iteration stops; later
    /// members are not evaluated.
    fn verify_all(
        &mut self,
        cancel: &AtomicBool,
        visit: &mut dyn FnMut(ArchiveMemberEvidence) -> Result<bool, ArchiveMemberSourceError>,
    ) -> Result<(), ArchiveMemberSourceError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn verified_marker_only_true_for_verified() {
        let cancel = AtomicBool::new(false);
        let _ = &cancel;
        assert!(
            ArchiveMemberEvidence {
                archive: "a.7z".into(),
                name: "a".into(),
                index: 0,
                logical_size: 1,
                is_nested_archive: false,
                status: ArchiveMemberStatus::Verified,
                hashes: Some(ArchiveMemberHashes {
                    crc32: "00000000".into(),
                    md5: "00".into(),
                    sha1: "00".into(),
                    sha256: "00".into(),
                }),
            }
            .is_verified()
        );
        assert!(
            !ArchiveMemberEvidence {
                archive: "a.7z".into(),
                name: "a".into(),
                index: 0,
                logical_size: 1,
                is_nested_archive: false,
                status: ArchiveMemberStatus::RefusedLimits {
                    reason: "member size"
                },
                hashes: None,
            }
            .is_verified()
        );
    }

    #[test]
    fn trait_is_object_safe_for_future_dyn_use() {
        // A future ZIP consumer may hold `Box<dyn ArchiveMemberSource>`; this
        // compiles only if the trait is object-safe (no generic methods).
        fn accept(_source: &dyn ArchiveMemberSource) {}
        let _ = accept as fn(&dyn ArchiveMemberSource);
    }

    #[test]
    fn evidence_statuses_cover_the_fail_closed_set() {
        // The refusal taxonomy must be exhaustively matchable by consumers.
        let statuses = [
            ArchiveMemberStatus::Verified,
            ArchiveMemberStatus::EmptyFile,
            ArchiveMemberStatus::NestedArchive,
            ArchiveMemberStatus::Encrypted,
            ArchiveMemberStatus::UnsupportedCodec {
                method: "ZSTD".into(),
            },
            ArchiveMemberStatus::RefusedLimits { reason: "ratio" },
            ArchiveMemberStatus::Corrupt {
                detail: "bad crc".into(),
            },
        ];
        let mut saw = Vec::new();
        for s in statuses {
            let label = match s {
                ArchiveMemberStatus::Verified => "verified",
                ArchiveMemberStatus::EmptyFile => "empty",
                ArchiveMemberStatus::NestedArchive => "nested",
                ArchiveMemberStatus::Encrypted => "encrypted",
                ArchiveMemberStatus::UnsupportedCodec { .. } => "codec",
                ArchiveMemberStatus::RefusedLimits { .. } => "limits",
                ArchiveMemberStatus::Corrupt { .. } => "corrupt",
            };
            saw.push(label);
        }
        assert_eq!(saw.len(), 7);
    }
}
