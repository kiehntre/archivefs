//! Bounded archive-member evidence for read-only DAT audits.
//!
//! The smallest reusable shape needed for member-aware DAT verification
//! across archive formats. It is intentionally narrow: one source trait that
//! enumerates members in deterministic order and yields per-member hashed
//! evidence, plus the evidence/status/error types. It does **not** define a
//! DAT-verification engine — members are hashed here; matching against a DAT
//! stays a separate consumer (`DatIndex`/`audit_one`).
//!
//! # Determinism and safety invariants
//!
//! - Members are enumerated in the archive's own deterministic order; nothing
//!   ever picks a member "by position" as a winner.
//! - Hashing is bounded, chunked, and cancellable. Each format decides whether
//!   it can continue after a member refusal: ZIP members are independent,
//!   while a solid 7z stream may have to stop. The returned pass outcome says
//!   explicitly whether every member was examined.
//! - Nested archives are surfaced (with [`ArchiveMemberStatus::NestedArchive`])
//!   but never recursively opened or hashed.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use serde::Serialize;

pub mod chd;
pub mod external_process;
pub mod hash;
pub mod limits;
pub mod rar;
pub mod sevenz;
pub mod sevenz_preflight;
pub mod zip;
pub mod zip_preflight;

/// One member's cryptographic hashes, computed over its decompressed bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveMemberHashes {
    pub crc32: String,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
}

/// The outcome for one archive member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ArchiveMemberStatus {
    /// The member streamed and was hashed within limits, and the number of
    /// bytes actually hashed matched its declared logical size exactly. A
    /// decode that ended early is [`ArchiveMemberStatus::Corrupt`], never this.
    HashComplete,
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
    /// No single DAT candidate could be selected to verify this member
    /// against (no filename match, or the filename matches several DAT
    /// entries whose hashes disagree). Content is never guessed at and never
    /// hashed speculatively; the member is left unverified rather than
    /// picking a candidate to test it against. Used by formats whose backend
    /// requires an expected hash *before* streaming a member (see
    /// [`super::rar`]), unlike ZIP/7z, which hash every member unconditionally
    /// and match afterward.
    NotVerified { reason: &'static str },
}

/// Format-neutral per-member evidence produced by an [`ArchiveMemberSource`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveMemberEvidence {
    /// Exact operating-system path of the outer archive. This is provenance,
    /// not a member rename target.
    pub archive_path: PathBuf,
    /// Exact bytes stored for the member name. Member index remains the
    /// identity because ZIP permits duplicate names.
    pub member_name_raw: Vec<u8>,
    /// Safe, display-only rendering of `member_name_raw`.
    pub member_name_display: String,
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
    /// Present only when [`ArchiveMemberStatus::HashComplete`].
    pub hashes: Option<ArchiveMemberHashes>,
}

impl ArchiveMemberEvidence {
    pub fn is_hash_complete(&self) -> bool {
        self.status == ArchiveMemberStatus::HashComplete
    }
}

/// Why a pass stopped before every stream-bearing member was examined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ArchivePassStopReason {
    Cancelled,
    MemberRefused {
        index: usize,
        status: ArchiveMemberStatus,
    },
    RunLogicalBudget,
    OuterFileChanged,
    SourceError {
        detail: String,
    },
}

/// Whether the format implementation examined every stream-bearing member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ArchivePassCompletion {
    Complete,
    Incomplete { reason: ArchivePassStopReason },
}

/// Bounded result of one archive pass. Per-member refusals may coexist with a
/// complete ZIP pass because later ZIP members remain independently readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchivePassOutcome {
    pub members: Vec<ArchiveMemberEvidence>,
    pub total_members: usize,
    pub completion: ArchivePassCompletion,
}

impl ArchivePassOutcome {
    pub fn is_complete(&self) -> bool {
        self.completion == ArchivePassCompletion::Complete
    }
}

/// Logical bytes decoded across every archive in one DAT audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveRunBudget {
    maximum: u64,
    consumed: u64,
}

impl ArchiveRunBudget {
    pub fn new(maximum: u64) -> Self {
        Self {
            maximum,
            consumed: 0,
        }
    }

    pub fn remaining(self) -> u64 {
        self.maximum.saturating_sub(self.consumed)
    }

    pub fn consumed(self) -> u64 {
        self.consumed
    }

    pub fn try_charge(&mut self, bytes: u64) -> bool {
        let Some(next) = self.consumed.checked_add(bytes) else {
            return false;
        };
        if next > self.maximum {
            return false;
        }
        self.consumed = next;
        true
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
/// enumerate members deterministically, and stream accepted members into
/// bounded hashes. The implementation owns continuation policy and returns
/// both the member evidence and explicit pass completeness.
///
/// The trait is **object-safe** so a consumer can hold
/// `Box<dyn ArchiveMemberSource>` without specialising on the concrete
/// format.
pub trait ArchiveMemberSource {
    /// A short, stable format name for diagnostics ("7z", "zip", "rar", …).
    fn archive_format(&self) -> &'static str;

    /// Number of stream-bearing members in deterministic order.
    fn member_count(&self) -> usize;

    /// Examine members in deterministic order, hashing each accepted member.
    fn verify_all(
        &mut self,
        cancel: &AtomicBool,
        run_budget: &mut ArchiveRunBudget,
    ) -> ArchivePassOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_complete_marker_only_true_for_hash_complete() {
        assert!(
            ArchiveMemberEvidence {
                archive_path: "a.7z".into(),
                member_name_raw: b"a".to_vec(),
                member_name_display: "a".into(),
                index: 0,
                logical_size: 1,
                is_nested_archive: false,
                status: ArchiveMemberStatus::HashComplete,
                hashes: Some(ArchiveMemberHashes {
                    crc32: "00000000".into(),
                    md5: "00".into(),
                    sha1: "00".into(),
                    sha256: "00".into(),
                }),
            }
            .is_hash_complete()
        );
        assert!(
            !ArchiveMemberEvidence {
                archive_path: "a.7z".into(),
                member_name_raw: b"a".to_vec(),
                member_name_display: "a".into(),
                index: 0,
                logical_size: 1,
                is_nested_archive: false,
                status: ArchiveMemberStatus::RefusedLimits {
                    reason: "member size"
                },
                hashes: None,
            }
            .is_hash_complete()
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
            ArchiveMemberStatus::HashComplete,
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
            ArchiveMemberStatus::NotVerified {
                reason: "ambiguous DAT candidates",
            },
        ];
        let mut saw = Vec::new();
        for s in statuses {
            let label = match s {
                ArchiveMemberStatus::HashComplete => "hash_complete",
                ArchiveMemberStatus::EmptyFile => "empty",
                ArchiveMemberStatus::NestedArchive => "nested",
                ArchiveMemberStatus::Encrypted => "encrypted",
                ArchiveMemberStatus::UnsupportedCodec { .. } => "codec",
                ArchiveMemberStatus::RefusedLimits { .. } => "limits",
                ArchiveMemberStatus::Corrupt { .. } => "corrupt",
                ArchiveMemberStatus::NotVerified { .. } => "not_verified",
            };
            saw.push(label);
        }
        assert_eq!(saw.len(), 8);
    }
}
