//! Local hashing, for comparing against a provider's published checksums.
//!
//! RomM publishes CRC32, MD5 and SHA-1. To reach
//! [`ExternalVerification::ConfirmedExternal`](super::model::ExternalVerification::ConfirmedExternal)
//! EmuWiz has to compute the same thing over the same bytes and find it
//! agrees. That is the only reason this module exists, and it shapes every
//! decision in it.
//!
//! # Never automatic
//!
//! Hashing a library is expensive - a single Wii image is several gigabytes - so
//! nothing here runs during startup, during a scan, or during ordinary browsing.
//! A hash is computed when a person asks for verification, or lazily for one file
//! they are looking at. An import that has not verified anything reports
//! [`StrongExternal`](super::model::ExternalVerification::StrongExternal) at
//! best, which is honest, rather than pretending to a hash comparison that never
//! happened.
//!
//! # Bounded and cancellable
//!
//! Files are streamed in fixed chunks, so memory does not scale with file size,
//! and the cancellation flag is checked every chunk - which matters when the file
//! is 4 GB and the person has changed their mind.
//!
//! # Reads go through the shared policy
//!
//! Every byte comes from [`crate::safe_read::open_bounded_read`], so a symlinked
//! library works exactly as it does elsewhere and a target outside the configured
//! roots is refused. This module adds no policy of its own.
//!
//! # These are identifiers, not security
//!
//! MD5 and SHA-1 are used here to compare against numbers a provider published.
//! Nothing is authenticated with them and no security decision rests on them, so
//! their cryptographic weakness is irrelevant to this use.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::model::{ExternalHash, HashAlgorithm};
use crate::safe_read::{SafeReadRefusal, TrustedRoots, open_bounded_read};

/// How much is read at a time. Large enough to keep the syscall count sane,
/// small enough that memory is flat regardless of file size.
pub const HASH_CHUNK_BYTES: usize = 256 * 1024;

/// The largest file this will hash without being asked twice.
///
/// Not a safety bound - hashing is streamed - but a courtesy one: silently
/// reading 60 GB because a record pointed at it would be a surprise, so anything
/// above this is refused with a reason and left to an explicit request.
pub const MAX_AUTOMATIC_HASH_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Why a hash could not be computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum HashRefusal {
    /// The bounded-read policy refused the file - outside the trusted roots, a
    /// broken symlink, a directory, a device.
    NotReadable {
        code: &'static str,
        detail: String,
    },
    TooLarge {
        bytes: u64,
        maximum: u64,
    },
    /// The file changed while it was being read, so the result would describe
    /// neither the old nor the new contents.
    ChangedWhileReading,
    ReadFailed {
        detail: String,
    },
    Cancelled,
}

impl HashRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::NotReadable { detail, .. } => detail.clone(),
            Self::TooLarge { bytes, maximum } => format!(
                "that file is {bytes} bytes, above the {maximum}-byte limit for automatic \
                 hashing; verify it explicitly if you want it hashed"
            ),
            Self::ChangedWhileReading => {
                "the file changed while it was being read, so no hash is reported".to_string()
            }
            Self::ReadFailed { detail } => format!("the file could not be read: {detail}"),
            Self::Cancelled => "hashing was cancelled".to_string(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NotReadable { code, .. } => code,
            Self::TooLarge { .. } => "too_large",
            Self::ChangedWhileReading => "changed_while_reading",
            Self::ReadFailed { .. } => "read_failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// The identity of a file's contents, for cache invalidation.
///
/// Path, size and modification time - the same triple the rest of EmuWiz
/// already uses to decide whether a file it saw before is still the same file. Not
/// a guarantee (a change within one timestamp tick is possible), so it is used
/// only to *invalidate*: a hash is discarded when any of these differs, never
/// trusted merely because they match a record from a different file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub path: PathBuf,
    pub size_bytes: u64,
    /// Modification time in whole seconds since the epoch, or `None` when the
    /// filesystem does not report one.
    pub modified_unix_seconds: Option<i64>,
}

impl FileFingerprint {
    /// Observes a file's current fingerprint. Metadata only - no read.
    pub fn observe(path: &Path) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        // A symlink's own metadata is not the target's, so the target is what is
        // fingerprinted; the read policy decides whether following it is allowed.
        let metadata = if metadata.file_type().is_symlink() {
            std::fs::metadata(path).ok()?
        } else {
            metadata
        };
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            path: path.to_path_buf(),
            size_bytes: metadata.len(),
            modified_unix_seconds: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_secs() as i64),
        })
    }

    /// Whether this fingerprint still describes the file on disk.
    pub fn still_current(&self) -> bool {
        Self::observe(&self.path).is_some_and(|current| &current == self)
    }
}

/// A computed set of local hashes, with the fingerprint they describe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalHashes {
    pub fingerprint: FileFingerprint,
    pub crc32: String,
    pub md5: String,
    pub sha1: String,
    /// How many bytes were actually read, so a caller can report cost.
    pub bytes_hashed: u64,
}

impl LocalHashes {
    /// The value for one algorithm.
    pub fn value(&self, algorithm: HashAlgorithm) -> &str {
        match algorithm {
            HashAlgorithm::Crc32 => &self.crc32,
            HashAlgorithm::Md5 => &self.md5,
            HashAlgorithm::Sha1 => &self.sha1,
        }
    }

    /// Whether a provider's published hash agrees with what was computed.
    ///
    /// Comparison is only ever like-with-like: the algorithm is part of
    /// [`ExternalHash`], so a CRC32 is never compared against an MD5.
    pub fn agrees_with(&self, published: &ExternalHash) -> bool {
        self.value(published.algorithm) == published.value
    }
}

/// A cache of computed hashes, keyed by fingerprint.
///
/// Persisted alongside the identity cache, so an explicit verification pass is
/// not repeated every time. An entry is dropped the moment its file's size or
/// modification time changes, which is what "invalidate cached hash when file
/// metadata changes" means in practice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalHashCache {
    entries: Vec<LocalHashes>,
}

impl LocalHashCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A cached result for `path`, if one is still valid.
    ///
    /// Returns `None` when the file's metadata has changed since the hash was
    /// computed, so a stale hash can never be presented as current evidence.
    pub fn get(&self, path: &Path) -> Option<&LocalHashes> {
        let current = FileFingerprint::observe(path)?;
        self.entries
            .iter()
            .find(|entry| entry.fingerprint == current)
    }

    /// Whether an entry exists for this path at all, regardless of validity.
    /// Used to distinguish "never hashed" from "hashed, then changed".
    pub fn has_entry_for(&self, path: &Path) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.fingerprint.path == path)
    }

    /// Stores a result, replacing any previous entry for the same path.
    pub fn insert(&mut self, hashes: LocalHashes) {
        self.entries
            .retain(|entry| entry.fingerprint.path != hashes.fingerprint.path);
        self.entries.push(hashes);
    }

    /// Drops every entry whose file has changed or disappeared.
    ///
    /// Returns how many were dropped, so a status view can say so.
    pub fn prune(&mut self) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.fingerprint.still_current());
        before - self.entries.len()
    }

    /// Deterministic ordering, so the persisted form is byte-stable.
    pub fn sorted(&self) -> Vec<&LocalHashes> {
        let mut entries: Vec<&LocalHashes> = self.entries.iter().collect();
        entries.sort_by(|left, right| left.fingerprint.path.cmp(&right.fingerprint.path));
        entries
    }
}

/// Computes all three hashes in one pass.
///
/// One pass rather than three: the file is read once and fed to every digest, so
/// verifying a 4 GB image costs one read rather than three.
pub fn hash_file(
    path: &Path,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
) -> Result<LocalHashes, HashRefusal> {
    hash_file_reporting(path, trusted, cancel, &|_| {})
}

/// How far through a file a hash has read.
///
/// Reported per chunk so a caller can show real progress for a multi-gigabyte disc
/// image rather than an unbounded spinner. `total_bytes` is the size observed before
/// reading started; if the file changes underneath, the hash is refused rather than
/// reported, so a total that stops matching is a refusal and not a progress bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashProgress {
    pub bytes_read: u64,
    pub total_bytes: u64,
}

impl HashProgress {
    /// Fraction read, or `None` for an empty file where there is nothing to divide.
    pub fn fraction(&self) -> Option<f32> {
        (self.total_bytes > 0)
            .then(|| (self.bytes_read as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32)
    }
}

/// [`hash_file`], reporting progress as it reads.
///
/// The callback runs on the hashing thread between chunks, so it must not block; the
/// GUI sends one channel message and returns.
pub fn hash_file_reporting(
    path: &Path,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
    on_progress: &dyn Fn(HashProgress),
) -> Result<LocalHashes, HashRefusal> {
    use md5::Md5;
    use sha1::Sha1;
    use sha1::digest::Digest;

    let before = FileFingerprint::observe(path).ok_or_else(|| HashRefusal::NotReadable {
        code: "unreadable",
        detail: format!("{} is not a readable regular file", path.display()),
    })?;
    if before.size_bytes > MAX_AUTOMATIC_HASH_BYTES {
        return Err(HashRefusal::TooLarge {
            bytes: before.size_bytes,
            maximum: MAX_AUTOMATIC_HASH_BYTES,
        });
    }
    if cancelled(cancel) {
        return Err(HashRefusal::Cancelled);
    }

    // The shared policy decides what may be opened, including whether a symlink
    // may be followed.
    let file = open_bounded_read(path, trusted).map_err(|refusal: SafeReadRefusal| {
        HashRefusal::NotReadable {
            code: refusal.code(),
            detail: refusal.detail(),
        }
    })?;
    let mut reader = file.into_file();

    let mut crc = Crc32::new();
    let mut md5 = Md5::new();
    let mut sha1 = Sha1::new();
    let mut buffer = vec![0_u8; HASH_CHUNK_BYTES];
    let mut total: u64 = 0;
    // Reported before the first chunk so a caller can name the file and its size
    // immediately, rather than showing nothing until 256 KB has been read.
    on_progress(HashProgress {
        bytes_read: 0,
        total_bytes: before.size_bytes,
    });

    loop {
        // Checked every chunk, so cancelling a multi-gigabyte hash takes effect
        // within one chunk rather than at the end.
        if cancelled(cancel) {
            return Err(HashRefusal::Cancelled);
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| HashRefusal::ReadFailed {
                detail: error.kind().to_string(),
            })?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        crc.update(chunk);
        md5.update(chunk);
        sha1.update(chunk);
        total = total.saturating_add(read as u64);
        on_progress(HashProgress {
            bytes_read: total,
            total_bytes: before.size_bytes,
        });
    }

    // If the file changed underneath us the digests describe neither version, so
    // nothing is reported.
    let after = FileFingerprint::observe(path).ok_or(HashRefusal::ChangedWhileReading)?;
    if after != before {
        return Err(HashRefusal::ChangedWhileReading);
    }

    Ok(LocalHashes {
        fingerprint: before,
        crc32: crc.finish_hex(),
        md5: hex(&md5.finalize()),
        sha1: hex(&sha1.finalize()),
        bytes_hashed: total,
    })
}

/// Returns a cached result, or computes and caches one.
pub fn hash_file_cached(
    path: &Path,
    cache: &mut LocalHashCache,
    trusted: &TrustedRoots,
    cancel: Option<&AtomicBool>,
) -> Result<LocalHashes, HashRefusal> {
    if let Some(cached) = cache.get(path) {
        return Ok(cached.clone());
    }
    let hashes = hash_file(path, trusted, cancel)?;
    cache.insert(hashes.clone());
    Ok(hashes)
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// CRC-32 as used by ZIP and by RomM's `crc_hash` - the IEEE 802.3 polynomial,
/// reflected, initialised to all ones and finally inverted.
///
/// Implemented here rather than pulling in a dependency: it is a fixed twenty-line
/// algorithm, and the test asserts it against published vectors.
pub struct Crc32 {
    state: u32,
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub fn new() -> Self {
        Self { state: !0 }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= u32::from(*byte);
            for _ in 0..8 {
                // Reflected polynomial 0x04C11DB7.
                let mask = (self.state & 1).wrapping_neg();
                self.state = (self.state >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }

    pub fn finish(self) -> u32 {
        !self.state
    }

    pub fn finish_hex(self) -> String {
        format!("{:08x}", self.finish())
    }

    /// One-shot, for a slice already in memory.
    pub fn of(bytes: &[u8]) -> String {
        let mut crc = Self::new();
        crc.update(bytes);
        crc.finish_hex()
    }
}
