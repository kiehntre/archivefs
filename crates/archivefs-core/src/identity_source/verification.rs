//! Where an explicit local hash verification is remembered.
//!
//! # Why this is not written into the identity cache
//!
//! Promoting a record to Confirmed needs one fact: the local file's hashes. That
//! fact is about the *file*, not about the provider record, and it stays true when
//! the catalogue is re-imported. Writing it into the identity cache would mean
//! rewriting a 52 MB document to record one verification, and losing every
//! verification the moment a refresh replaced that document.
//!
//! So verification lives in its own small, versioned, provider-owned file, and the
//! verdict is recomputed from the cached record plus these hashes by the same
//! [`crate::identity_source::matching::match_record`] the import uses. The identity
//! cache is never touched by verifying a file, and a re-import cannot undo one.
//!
//! # Derived data, and only derived data
//!
//! Everything here can be recomputed by reading files that are already on disk, so
//! a missing, unreadable or future-version store is discarded rather than repaired.
//! Nothing here is authoritative about anything except "these bytes hashed to this".

use std::fs;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::hashing::{LocalHashCache, LocalHashes};
use super::model::IdentityProvider;

/// Bumped only if the layout changes. A newer version is discarded, because every
/// entry can be recomputed by hashing the file again.
pub const VERIFICATION_FORMAT_VERSION: u32 = 1;

pub const VERIFICATION_FILE_NAME: &str = "verified-hashes.json";

/// The most verifications one store holds.
///
/// Each is about 300 bytes, so this is a few megabytes at worst. It exists because
/// a person clicking Verify thousands of times should still not produce an unbounded
/// file - and because the oldest entries are the least likely to be wanted.
pub const MAX_VERIFIED_ENTRIES: usize = 20_000;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The on-disk form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub format_version: u32,
    /// Which instance's records these verifications were made against.
    ///
    /// Kept for provenance only: a hash is a fact about a local file and stays
    /// valid whichever provider asked for it, so this is not used to reject
    /// entries.
    pub server_id: String,
    pub hashes: LocalHashCache,
}

/// Why a verification could not be stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum VerificationStoreError {
    WriteFailed { detail: String },
}

impl VerificationStoreError {
    pub fn detail(&self) -> String {
        match self {
            Self::WriteFailed { detail } => {
                format!("the verification record could not be written: {detail}")
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::WriteFailed { .. } => "write_failed",
        }
    }
}

/// A provider-owned store of explicit verifications.
#[derive(Debug, Clone)]
pub struct VerificationStore {
    directory: PathBuf,
}

impl VerificationStore {
    pub fn new(identity_root: &Path, provider: IdentityProvider) -> Self {
        Self {
            directory: identity_root.join(provider.slug()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join(VERIFICATION_FILE_NAME)
    }

    /// Loads the stored hashes, or an empty cache.
    ///
    /// A missing, unreadable, malformed or future-version file all yield an empty
    /// cache: an entry that cannot be read can always be recomputed, and guessing at
    /// a layout this build does not know would be worse than starting again.
    ///
    /// Entries whose file has changed are dropped on load, so a stale hash can never
    /// be presented as current evidence.
    pub fn load(&self) -> LocalHashCache {
        let Ok(text) = fs::read_to_string(self.path()) else {
            return LocalHashCache::new();
        };
        let Ok(record) = serde_json::from_str::<VerificationRecord>(&text) else {
            return LocalHashCache::new();
        };
        if record.format_version != VERIFICATION_FORMAT_VERSION {
            return LocalHashCache::new();
        }
        let mut hashes = record.hashes;
        // The fingerprint check is what makes a stored hash safe to trust: a file
        // that has been rebuilt since is no longer described by it.
        hashes.prune();
        hashes
    }

    /// Publishes the store atomically.
    ///
    /// Written to a temporary file, flushed, synced and renamed, so a reader never
    /// sees a half-written document - the same discipline the identity cache uses,
    /// for the same reason.
    pub fn save(
        &self,
        server_id: &str,
        hashes: &LocalHashCache,
    ) -> Result<PathBuf, VerificationStoreError> {
        fs::create_dir_all(&self.directory).map_err(|error| {
            VerificationStoreError::WriteFailed {
                detail: error.to_string(),
            }
        })?;
        // Bounded, and deterministic so the file is byte-stable for the same set.
        let mut trimmed = LocalHashCache::new();
        for entry in hashes
            .sorted()
            .into_iter()
            .rev()
            .take(MAX_VERIFIED_ENTRIES)
            .rev()
        {
            trimmed.insert(entry.clone());
        }
        let record = VerificationRecord {
            format_version: VERIFICATION_FORMAT_VERSION,
            server_id: server_id.to_string(),
            hashes: trimmed,
        };
        let bytes = serde_json::to_vec_pretty(&record).map_err(|error| {
            VerificationStoreError::WriteFailed {
                detail: error.to_string(),
            }
        })?;
        let temporary = self.directory.join(format!(
            "{VERIFICATION_FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let write = || -> std::io::Result<()> {
            // Owner-only, like the rest of EmuWiz's identity data. Nothing here is
            // a secret, but it does list where the library's files are, which is not
            // something to publish to every account on the machine.
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&temporary, self.path())
        };
        if let Err(error) = write() {
            let _ = fs::remove_file(&temporary);
            return Err(VerificationStoreError::WriteFailed {
                detail: error.to_string(),
            });
        }
        Ok(self.path())
    }

    /// Records one verification, keeping everything already stored.
    ///
    /// Loads, inserts and publishes in one step, so two verifications in a row
    /// cannot lose the first.
    pub fn record(
        &self,
        server_id: &str,
        verified: LocalHashes,
    ) -> Result<LocalHashCache, VerificationStoreError> {
        let mut hashes = self.load();
        hashes.insert(verified);
        self.save(server_id, &hashes)?;
        Ok(hashes)
    }

    /// Removes the store. Returns whether there was one.
    pub fn remove(&self) -> std::io::Result<bool> {
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// How many verifications are stored and still valid.
    pub fn count(&self) -> usize {
        self.load().len()
    }
}

#[cfg(test)]
mod tests;
