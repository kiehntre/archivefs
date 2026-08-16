//! Read-only pairwise duplicate-content proof.
//!
//! This is evidence, never permission. It answers exactly one question -
//! "do these two paths contain identical bytes, and are they two distinct
//! filesystem objects or the same one reached twice?" - and produces a typed
//! [`DuplicateContentProof`] a caller can turn into [`RepairEvidence`]. It
//! never deletes, moves, quarantines, or otherwise mutates anything, and it
//! never itself decides *which* of two duplicates should go: that policy
//! question is deliberately left unanswered here.
//!
//! # Not yet sole mutation authority
//!
//! [`DuplicateContentProof`] is read-only evidence appropriate for the
//! current CRC32+MD5+SHA-1 read-only comparison this module performs. It is
//! **not** yet the sole authority a future quarantine or deletion feature
//! could act on directly from a stored/serialized copy: any future mutation
//! path must re-prove from a *live* call to [`prove_duplicate_content`]
//! immediately before acting, never trust a proof (or its flattened
//! [`RepairEvidence`]) captured earlier. That re-proving requirement, and
//! the actual deletion/quarantine mechanism, are out of scope here.
//!
//! # No second hashing implementation
//!
//! Every byte is read through the existing bounded, cancellable
//! [`crate::identity_source::hashing::hash_file`] - the same primitive the
//! identity/verification pipeline already uses for RomM hash comparison.
//! This module adds no reader, no digest, and no chunking of its own. It
//! does *not* use [`crate::identity_source::hashing::hash_file_cached`] /
//! [`crate::identity_source::hashing::LocalHashCache`] - see
//! [`ProofObjectKey`] for why that cache's key is not strong enough for this
//! proof, and [`DuplicateHashCache`] for the proof-local replacement.
//!
//! # Filename grouping is a candidate source, never the proof
//!
//! [`candidate_pairs`] turns a group of paths (for example
//! [`crate::FilenameDuplicateDetector`]'s or
//! [`crate::catalogue_filename_duplicates`]'s grouped paths) into pairs worth
//! checking. It does not read the filesystem and it proves nothing by
//! itself: two same-named files that happen to differ in content are never
//! classified as duplicates, because the only thing that can prove
//! "duplicate content" is [`prove_duplicate_content`] actually hashing both
//! files and finding they agree.
//!
//! # Strong equality, TOCTOU-checked
//!
//! Same filename is never sufficient. Same size is never sufficient. A proof
//! requires: matching size, matching CRC32 *and* MD5 *and* SHA-1 (not just
//! one), a proof-local [`ProofObjectKey`] (inode/device on unix, size, kind,
//! and a *full-precision* modification time) captured immediately before
//! hashing began to still match immediately after hashing finished, *and* -
//! only on the path about to report success - a final fresh, uncached
//! re-hash of both files whose digest must still match what was already
//! computed. That last check exists because full-precision mtime is still
//! not a sufficient *sole* signal: some filesystems/mounts (overlay, tmpfs)
//! report no observable metadata difference at all between a file's state
//! before and after an in-place rewrite that lands within one clock tick, so
//! a metadata match is never trusted as the final word - only a second,
//! independent read of the actual bytes is. Any disagreement anywhere in
//! that chain refuses the proof rather than reporting a weaker one.
//!
//! # Hard links are not "two copies"
//!
//! Two paths that resolve to the same inode and device are the same object
//! on disk, not two independent copies of it - deleting one deletes both.
//! [`DuplicateContentProof::classification`] is
//! [`DuplicatePairClassification::SameObject`] rather than
//! [`DuplicatePairClassification::DistinctObjects`] for that case, so a
//! caller (or a future repair proposal) can never mistake a hard link for
//! reclaimable duplicate space.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::dat::rename_apply::{ObjectIdentity, ObjectKind, capture_identity};
use crate::identity_source::hashing::{HashRefusal, LocalHashes, hash_file};
use crate::identity_source::model::HashAlgorithm;
use crate::safe_read::TrustedRoots;

use super::proposal::{RepairEvidence, RepairEvidenceKind};

/// Whether two proven-identical-content paths are the same filesystem object
/// or two distinct objects that happen to contain the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePairClassification {
    /// Two distinct filesystem objects (different inode/device on platforms
    /// that report one) whose content matches. The only case where removing
    /// one would actually reclaim space and leave a second, independent copy
    /// on disk.
    DistinctObjects,
    /// The same filesystem object reached by two different paths - a hard
    /// link - or the same path compared with itself. There is only one
    /// object here, never a reclaimable "duplicate".
    SameObject,
}

/// A read-only, hash-proven pairwise duplicate-content result.
///
/// Evidence only: nothing here authorises a mutation, and nothing here says
/// which of the two paths (if either) should be removed. See this module's
/// "Not yet sole mutation authority" doc: a future mutation feature must
/// re-prove live, never act on a stored copy of this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateContentProof {
    pub path_a: PathBuf,
    pub path_b: PathBuf,
    /// The headline algorithm reported (SHA-1, the strongest of the three
    /// [`prove_duplicate_content`] requires to agree - CRC32 and MD5 also
    /// matched, or the proof would have refused).
    pub algorithm: HashAlgorithm,
    pub hash: String,
    pub size_bytes: u64,
    /// Each file's filesystem identity, captured immediately after hashing
    /// and revalidated against the identity captured immediately before -
    /// see [`prove_duplicate_content`]'s TOCTOU handling.
    pub identity_a: ObjectIdentity,
    pub identity_b: ObjectIdentity,
    pub classification: DuplicatePairClassification,
}

impl DuplicateContentProof {
    /// Whether this is a duplicate of two distinct filesystem objects -
    /// never true for a hard-linked or self-compared pair. See
    /// [`DuplicatePairClassification`].
    pub fn is_distinct_object_duplicate(&self) -> bool {
        self.classification == DuplicatePairClassification::DistinctObjects
    }

    /// The typed [`RepairEvidence`] this proof supports.
    ///
    /// This is informational, flattened text, not a re-usable typed proof:
    /// `RepairEvidence` carries only `{ kind, detail: String }` for every
    /// evidence kind, and widening that shared struct with a
    /// duplicate-content-specific field would be schema churn touching every
    /// other evidence kind for one caller's convenience. Whether the pair
    /// was [`DuplicatePairClassification::SameObject`] or `DistinctObjects`
    /// is stated in the detail string for a human/GUI reader, but **must
    /// never be parsed back out of it** to make a decision - a future
    /// mutation-authority caller needs the classification, it must call
    /// [`prove_duplicate_content`] again against the live filesystem and
    /// read [`Self::classification`]/[`Self::is_distinct_object_duplicate`]
    /// directly, never reconstruct it from this string.
    ///
    /// Never itself an action or a proposal: [`RepairEvidenceKind::DuplicateContent`]
    /// is evidence a future proposal could cite, and the only action kind it
    /// could ever support - `DeferredActionKind::DeleteDuplicate` - remains
    /// permanently non-executable (`RepairAction::is_executable` is `true`
    /// only for `RenamePath`/`MovePath`).
    pub fn evidence(&self) -> RepairEvidence {
        let detail = match self.classification {
            DuplicatePairClassification::DistinctObjects => format!(
                "'{}' and '{}' are distinct files with byte-identical content ({} bytes, {} {})",
                self.path_a.display(),
                self.path_b.display(),
                self.size_bytes,
                self.algorithm.label(),
                self.hash
            ),
            DuplicatePairClassification::SameObject => format!(
                "'{}' and '{}' are the same filesystem object (hard-linked, or the same path), \
                 not two independent copies",
                self.path_a.display(),
                self.path_b.display()
            ),
        };
        RepairEvidence::new(RepairEvidenceKind::DuplicateContent, detail)
    }
}

/// Why a pairwise duplicate-content proof could not be produced.
///
/// Fail closed: any of these means "not proven", never a best-effort guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateProofRefusal {
    /// The same path was given for both sides; there is nothing to compare.
    SamePath,
    /// A path does not exist, is not a regular file, or could not be opened
    /// (permission, I/O error, or refused by the trusted-roots read policy).
    NotReadable { path: PathBuf, detail: String },
    /// A path is larger than hashing will read automatically.
    TooLarge {
        path: PathBuf,
        bytes: u64,
        maximum: u64,
    },
    /// A file's size or modification time changed while it was being read by
    /// the underlying hashing primitive, so the digest describes neither its
    /// old nor its new content.
    ChangedWhileHashing { path: PathBuf },
    /// Something about a file changed mid-proof: either its proof-local key
    /// ([`ProofObjectKey`]: inode/device, size, kind, and full-precision
    /// modification time) captured immediately before hashing no longer
    /// matches the key captured immediately after - a whole-object
    /// replacement (different inode) or a same-inode, same-size, in-place
    /// content rewrite (different precise mtime) - or, on a filesystem whose
    /// mtime resolution was too coarse to reveal that, a final fresh re-hash
    /// disagreed with the digest already reported. Neither the key nor
    /// `identity_matches` alone would catch every case: `identity_matches`
    /// intentionally ignores mtime for rename semantics, and mtime itself
    /// can be too coarse on some mounts - the final re-hash is what actually
    /// closes the gap.
    ContentModifiedDuringProof { path: PathBuf },
    /// Hashing was cancelled before it could complete.
    Cancelled,
    /// The two files differ in size. Sufficient on its own to refuse: no
    /// read was performed, since same size is necessary but never proof.
    SizeMismatch { size_a: u64, size_b: u64 },
    /// Both files were fully, independently hashed but at least one of
    /// CRC32, MD5, or SHA-1 disagreed.
    HashMismatch,
}

impl std::fmt::Display for DuplicateProofRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SamePath => write!(f, "the same path was given for both sides"),
            Self::NotReadable { path, detail } => {
                write!(f, "'{}' could not be read: {detail}", path.display())
            }
            Self::TooLarge {
                path,
                bytes,
                maximum,
            } => write!(
                f,
                "'{}' is {bytes} bytes, above the {maximum}-byte automatic hashing limit",
                path.display()
            ),
            Self::ChangedWhileHashing { path } => {
                write!(f, "'{}' changed while it was being hashed", path.display())
            }
            Self::ContentModifiedDuringProof { path } => write!(
                f,
                "'{}' no longer matches the object identity/timing captured before hashing began",
                path.display()
            ),
            Self::Cancelled => write!(f, "duplicate-content proof was cancelled"),
            Self::SizeMismatch { size_a, size_b } => {
                write!(f, "sizes differ ({size_a} vs {size_b} bytes); not proven")
            }
            Self::HashMismatch => write!(f, "content hashes disagree; not proven"),
        }
    }
}

impl std::error::Error for DuplicateProofRefusal {}

/// A proof-local key strong enough to safely reuse a computed digest across
/// more than one pair in a run: the filesystem object's inode and device on
/// unix (there is no portable substitute) plus its size, kind, and a
/// *full-precision* modification time.
///
/// Never [`crate::identity_source::hashing::LocalHashCache`]'s own key: that
/// cache is keyed by path, size, and a modification time **truncated to
/// whole seconds**, which is entirely sufficient for its own callers (RomM
/// hash verification, where a stale cache entry just costs a re-hash, never
/// a false result) but is not strong enough for a proof whose entire purpose
/// is asserting that two objects' *current* bytes agree: a path replaced
/// with a same-size object inside the same wall-clock second would satisfy
/// that cache's key without actually being the same object, silently
/// returning a stale digest for a different file's content.
///
/// Also never merely [`crate::dat::rename_apply::identity_matches`]: that
/// comparison intentionally ignores modification time (mtime is not part of
/// the identity contract for a *rename* - renaming preserves the inode, so
/// size+inode+dev are the strong checks there), so it alone cannot detect a
/// same-inode, same-size, in-place content rewrite. This key folds a
/// full-precision mtime into equality specifically to catch that case, as a
/// proof-local addition - `identity_matches` and rename identity semantics
/// elsewhere in the codebase are untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProofObjectKey {
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    dev: u64,
    /// No portable inode/device exists off unix, so a path is the only
    /// available discriminator there. This is strictly weaker than the unix
    /// key: it relies on the full-precision mtime and the whole-hash
    /// agreement check (and the after-hashing revalidation) to catch what
    /// inode/dev would otherwise catch directly.
    #[cfg(not(unix))]
    path: PathBuf,
    size_bytes: u64,
    /// [`ObjectKind`] does not implement `Hash`/`Eq` derives this module
    /// does not want to add to a shared type for one caller's convenience,
    /// so it is folded in as its `Debug` label - stable enough within one
    /// process/run, and this key is never persisted or compared across
    /// versions.
    kind_label: &'static str,
    /// `(seconds, nanoseconds)` since the Unix epoch - never truncated to
    /// whole seconds. `None` only when the platform/filesystem reports no
    /// modification time at all.
    modified: Option<(u64, u32)>,
}

impl ProofObjectKey {
    /// Captures the current key and the [`ObjectIdentity`] for `path` in one
    /// call - the identity the rest of the proof already needs, plus the
    /// proof-local, full-precision addition this module needs beyond it.
    fn capture(path: &Path) -> Result<(Self, ObjectIdentity), DuplicateProofRefusal> {
        let identity = capture_identity_checked(path)?;
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            DuplicateProofRefusal::NotReadable {
                path: path.to_path_buf(),
                detail: error.to_string(),
            }
        })?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|elapsed| (elapsed.as_secs(), elapsed.subsec_nanos()));
        let key = Self {
            #[cfg(unix)]
            ino: identity.ino,
            #[cfg(unix)]
            dev: identity.dev,
            #[cfg(not(unix))]
            path: path.to_path_buf(),
            size_bytes: identity.size_bytes,
            kind_label: object_kind_label(identity.kind),
            modified,
        };
        Ok((key, identity))
    }
}

/// A stable-within-one-process label for [`ObjectKind`], since the type
/// itself derives neither `Hash` nor an equivalent this module needs and
/// this module does not add derives to a shared type for its own
/// convenience.
fn object_kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::RegularFile => "regular_file",
        ObjectKind::Symlink => "symlink",
        ObjectKind::BrokenSymlink => "broken_symlink",
        ObjectKind::Other => "other",
    }
}

/// A per-run cache of proven-fresh digests, keyed by [`ProofObjectKey`] so a
/// cache hit is always still bound to the exact filesystem object it was
/// computed for - a replaced path (different inode/device on unix) or a
/// same-inode rewrite (different full-precision mtime) is always a cache
/// miss, forcing a fresh read, never a stale reuse.
///
/// Deliberately separate from
/// [`crate::identity_source::hashing::LocalHashCache`] - see
/// [`ProofObjectKey`]'s doc for why that cache's key is not strong enough
/// here - and, like it, never persisted: a fresh, empty cache is the normal
/// way to call this for one duplicate-analysis run. [`prove_duplicate_group`]
/// creates one per run and shares it across every pair in a group.
#[derive(Debug, Default)]
pub struct DuplicateHashCache {
    /// A `Vec`, not a `HashMap`: [`ProofObjectKey`] does not implement
    /// `Hash` (see its doc), and one duplicate-analysis run's candidate
    /// groups are never large enough for a linear scan to matter.
    entries: Vec<(ProofObjectKey, LocalHashes)>,
}

impl DuplicateHashCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Independently hashes two files and proves (or refuses to prove) that they
/// contain identical content.
///
/// `cache` is a per-run [`DuplicateHashCache`] the caller owns. Reusing it
/// across multiple calls (for example every pair [`candidate_pairs`]
/// produces for one group) avoids re-hashing a file that appears in more
/// than one candidate pair *and* is still bound to the exact object it was
/// hashed for - see [`ProofObjectKey`]. Nothing here persists the cache; a
/// fresh, empty one is the normal way to call this for one
/// duplicate-analysis run. The cache only ever shortcuts the *first* hash of
/// each file: the path that is about to report a proven duplicate always
/// pays for one additional, uncached confirmation re-hash per file (see
/// "Fails closed on" below) - caching still eliminates the common case
/// (re-hashing a file that appears in many candidate pairs but matches
/// none), it does not eliminate the cost of confirming an actual match.
///
/// # Fails closed on
///
/// - either path missing, unreadable, or not a regular file;
/// - a size mismatch (checked before any read);
/// - a file changing size/mtime while the underlying hashing primitive is
///   reading it;
/// - a file's proof-local key (inode/device where available, size, kind, and
///   full-precision modification time) differing between
///   immediately-before-hashing and immediately-after-hashing - whether from
///   a whole-object replacement or a same-inode, same-size, in-place rewrite;
/// - a final, fresh, uncached re-hash (paid only once both files already
///   look like a match) disagreeing with the digest already computed - the
///   backstop for a same-tick in-place rewrite that the metadata key above
///   could not observe;
/// - hashing being cancelled;
/// - the three hash algorithms not all agreeing.
pub fn prove_duplicate_content(
    path_a: &Path,
    path_b: &Path,
    cache: &mut DuplicateHashCache,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
) -> Result<DuplicateContentProof, DuplicateProofRefusal> {
    prove_duplicate_content_checkpointed(path_a, path_b, cache, trusted, cancel, &mut |_| {})
}

/// A fixed point inside [`prove_duplicate_content_checkpointed`]'s execution.
///
/// Used only by this module's own tests, to synchronize a mutation
/// deterministically into a specific window of the proof instead of racing a
/// background thread against wall-clock scheduling. Never reachable from
/// outside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProofCheckpoint {
    /// Both "before" keys are captured and the size check has passed;
    /// nothing has been hashed yet.
    BeforeHashingA,
    /// `path_a` has been hashed (or served from the proof-local cache);
    /// `path_b` has not yet been touched.
    AfterHashingA,
    /// Both files have been hashed; the "after" revalidation has not yet
    /// run.
    AfterHashingB,
    /// The proof-local key revalidation and the hash-agreement check have
    /// both passed; the final content-confirmation re-hash has not yet run.
    BeforeConfirmationRehash,
}

/// [`prove_duplicate_content`], with a checkpoint callback fired at fixed
/// points in the proof. The callback runs synchronously on the same thread,
/// immediately before the next step, so a test can mutate the filesystem
/// from inside it and be certain the mutation lands exactly where intended -
/// no thread, no sleep, no scheduler-dependent race.
fn prove_duplicate_content_checkpointed(
    path_a: &Path,
    path_b: &Path,
    cache: &mut DuplicateHashCache,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
    checkpoint: &mut dyn FnMut(ProofCheckpoint),
) -> Result<DuplicateContentProof, DuplicateProofRefusal> {
    if path_a == path_b {
        return Err(DuplicateProofRefusal::SamePath);
    }

    let (key_a_before, identity_a_before) = ProofObjectKey::capture(path_a)?;
    let (key_b_before, identity_b_before) = ProofObjectKey::capture(path_b)?;

    // Same size is necessary but never sufficient - checked first so a
    // mismatch never costs a read.
    if identity_a_before.size_bytes != identity_b_before.size_bytes {
        return Err(DuplicateProofRefusal::SizeMismatch {
            size_a: identity_a_before.size_bytes,
            size_b: identity_b_before.size_bytes,
        });
    }

    checkpoint(ProofCheckpoint::BeforeHashingA);
    let hashes_a = hash_with_proof_cache(path_a, &key_a_before, cache, trusted, cancel)?;
    checkpoint(ProofCheckpoint::AfterHashingA);
    let hashes_b = hash_with_proof_cache(path_b, &key_b_before, cache, trusted, cancel)?;
    checkpoint(ProofCheckpoint::AfterHashingB);

    // Revalidate with the same proof-local key hashing was bound to. This is
    // deliberately not `identity_matches`: that comparison intentionally
    // ignores mtime for rename semantics and would miss a same-inode,
    // same-size, in-place rewrite. Any difference - inode/dev, size, kind, or
    // full-precision mtime - refuses.
    let (key_a_after, identity_a_after) = ProofObjectKey::capture(path_a)?;
    if key_a_before != key_a_after {
        return Err(DuplicateProofRefusal::ContentModifiedDuringProof {
            path: path_a.to_path_buf(),
        });
    }
    let (key_b_after, identity_b_after) = ProofObjectKey::capture(path_b)?;
    if key_b_before != key_b_after {
        return Err(DuplicateProofRefusal::ContentModifiedDuringProof {
            path: path_b.to_path_buf(),
        });
    }

    // Strong content equality: every algorithm must agree, not just one.
    if hashes_a.crc32 != hashes_b.crc32
        || hashes_a.md5 != hashes_b.md5
        || hashes_a.sha1 != hashes_b.sha1
    {
        return Err(DuplicateProofRefusal::HashMismatch);
    }

    // Final defense, only paid on the path that is about to report success:
    // on a filesystem/mount whose modification-time resolution is coarser
    // than the gap between two operations (some overlay/tmpfs mounts report
    // no observable difference at all between a file's state before and
    // after an in-place rewrite that happens within one clock tick),
    // `ProofObjectKey`'s mtime comparison above can pass even though the
    // content changed. A metadata match is therefore never trusted as the
    // final word: both files are re-hashed fresh (bypassing the cache, so a
    // stale cache entry cannot confirm itself) and the new digest must
    // exactly match what was already reported. Any disagreement here means
    // the content was not actually stable for the duration of this proof,
    // regardless of what the metadata claimed.
    checkpoint(ProofCheckpoint::BeforeConfirmationRehash);
    let confirm_a =
        hash_file(path_a, trusted, cancel).map_err(|refusal| refusal_for(path_a, refusal))?;
    if confirm_a.crc32 != hashes_a.crc32
        || confirm_a.md5 != hashes_a.md5
        || confirm_a.sha1 != hashes_a.sha1
    {
        return Err(DuplicateProofRefusal::ContentModifiedDuringProof {
            path: path_a.to_path_buf(),
        });
    }
    let confirm_b =
        hash_file(path_b, trusted, cancel).map_err(|refusal| refusal_for(path_b, refusal))?;
    if confirm_b.crc32 != hashes_b.crc32
        || confirm_b.md5 != hashes_b.md5
        || confirm_b.sha1 != hashes_b.sha1
    {
        return Err(DuplicateProofRefusal::ContentModifiedDuringProof {
            path: path_b.to_path_buf(),
        });
    }

    let classification = classify_pair(&identity_a_after, &identity_b_after);

    Ok(DuplicateContentProof {
        path_a: path_a.to_path_buf(),
        path_b: path_b.to_path_buf(),
        algorithm: HashAlgorithm::Sha1,
        hash: hashes_a.sha1.clone(),
        size_bytes: identity_a_after.size_bytes,
        identity_a: identity_a_after,
        identity_b: identity_b_after,
        classification,
    })
}

/// Returns a proof-cached digest for `path` only if `key` (captured
/// immediately before this call, in the caller's "before" step) still
/// matches the key the cached entry was stored under; otherwise hashes fresh
/// through [`hash_file`] (the bounded, cancellable primitive - never a
/// second implementation) and caches the new result under `key`.
///
/// This is the fix for cross-call cache staleness: unlike
/// [`crate::identity_source::hashing::hash_file_cached`], a hit here can
/// never be returned for a *different* filesystem object that merely shares
/// a path, size, and whole-second mtime with what was previously hashed.
fn hash_with_proof_cache(
    path: &Path,
    key: &ProofObjectKey,
    cache: &mut DuplicateHashCache,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
) -> Result<LocalHashes, DuplicateProofRefusal> {
    if let Some((_, cached)) = cache.entries.iter().find(|(entry_key, _)| entry_key == key) {
        return Ok(cached.clone());
    }
    let hashes = hash_file(path, trusted, cancel).map_err(|refusal| refusal_for(path, refusal))?;
    cache.entries.push((key.clone(), hashes.clone()));
    Ok(hashes)
}

/// All unordered pairs within one candidate group worth proving.
///
/// A pure combinatorial helper: it never touches the filesystem and proves
/// nothing itself. Feed it the paths from an existing candidate-generation
/// source - for example one [`crate::DuplicateEntry::archive_paths`] group,
/// or one [`crate::CatalogueDuplicateGroup`]'s paths - and hand every pair it
/// returns to [`prove_duplicate_content`] for the actual classification.
pub fn candidate_pairs(paths: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let mut pairs = Vec::new();
    for i in 0..paths.len() {
        for j in (i + 1)..paths.len() {
            pairs.push((paths[i].clone(), paths[j].clone()));
        }
    }
    pairs
}

/// Proves every pair [`candidate_pairs`] would generate for one group,
/// sharing one [`DuplicateHashCache`] across the whole group so a path that
/// appears in more than one pair is hashed at most once per run - safely,
/// per [`ProofObjectKey`]: a path replaced partway through the group forces
/// a fresh hash (or a refusal) for every pair evaluated after the
/// replacement, never a stale reuse of what it used to contain.
pub fn prove_duplicate_group(
    paths: &[PathBuf],
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
) -> Vec<(
    PathBuf,
    PathBuf,
    Result<DuplicateContentProof, DuplicateProofRefusal>,
)> {
    let mut cache = DuplicateHashCache::new();
    candidate_pairs(paths)
        .into_iter()
        .map(|(a, b)| {
            let result = prove_duplicate_content(&a, &b, &mut cache, trusted, cancel);
            (a, b, result)
        })
        .collect()
}

/// [`capture_identity`], refusing with a [`DuplicateProofRefusal`] and
/// requiring a regular file (never a symlink, directory, or other object -
/// hashing a symlink's target is a different question this proof never
/// answers).
fn capture_identity_checked(path: &Path) -> Result<ObjectIdentity, DuplicateProofRefusal> {
    let identity = capture_identity(path).map_err(|error| DuplicateProofRefusal::NotReadable {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    if identity.kind != ObjectKind::RegularFile {
        return Err(DuplicateProofRefusal::NotReadable {
            path: path.to_path_buf(),
            detail: format!("not a regular file ({:?})", identity.kind),
        });
    }
    Ok(identity)
}

fn refusal_for(path: &Path, refusal: HashRefusal) -> DuplicateProofRefusal {
    match refusal {
        HashRefusal::NotReadable { detail, .. } => DuplicateProofRefusal::NotReadable {
            path: path.to_path_buf(),
            detail,
        },
        HashRefusal::TooLarge { bytes, maximum } => DuplicateProofRefusal::TooLarge {
            path: path.to_path_buf(),
            bytes,
            maximum,
        },
        HashRefusal::ChangedWhileReading => DuplicateProofRefusal::ChangedWhileHashing {
            path: path.to_path_buf(),
        },
        HashRefusal::ReadFailed { detail } => DuplicateProofRefusal::NotReadable {
            path: path.to_path_buf(),
            detail,
        },
        HashRefusal::Cancelled => DuplicateProofRefusal::Cancelled,
    }
}

/// Classifies a proven-identical-content pair as the same filesystem object
/// or two distinct ones.
///
/// Unix only can prove `SameObject`: inode and device numbers are the only
/// portable-in-this-codebase way to know two paths name one object. On a
/// platform without them this always reports `DistinctObjects` - under-
/// detecting a hard link there rather than ever mis-claiming two objects are
/// one when that cannot actually be proven.
#[cfg(unix)]
fn classify_pair(a: &ObjectIdentity, b: &ObjectIdentity) -> DuplicatePairClassification {
    if a.ino == b.ino && a.dev == b.dev {
        DuplicatePairClassification::SameObject
    } else {
        DuplicatePairClassification::DistinctObjects
    }
}

#[cfg(not(unix))]
fn classify_pair(_a: &ObjectIdentity, _b: &ObjectIdentity) -> DuplicatePairClassification {
    DuplicatePairClassification::DistinctObjects
}

#[cfg(test)]
mod tests;
