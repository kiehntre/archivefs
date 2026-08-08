//! The identity cache, and how it is published.
//!
//! # Why a provider-owned file rather than a schema migration
//!
//! The application database is at schema 6 with migrations 0001-0006, and this
//! milestone adds no migration. An imported identity cache does not need to be
//! there: it is derived data belonging entirely to one optional source, it is
//! replaced wholesale rather than mutated row by row, it must survive being
//! deleted without touching the user's library, and its natural lifecycle is "one
//! file, atomically swapped". A migration would give up all of that in exchange
//! for nothing, and would make removing a source a schema-level operation.
//!
//! So the cache is one JSON file next to the other ArchiveFS-owned caches,
//! versioned by [`CACHE_FORMAT_VERSION`], and the application schema is
//! untouched. If a later stage needs to *join* identity against catalogue rows in
//! SQL, that is when a migration becomes justified, and it can read this file to
//! populate itself.
//!
//! # Publication
//!
//! A refresh writes a temporary file, syncs it, validates it by reading it back,
//! and only then renames it over the live one. Every failure path leaves the
//! previous cache exactly where it was:
//!
//! - the import failed, so nothing was written;
//! - the write failed, so the temporary file is removed and the live file is
//!   untouched;
//! - the readback failed validation, so the temporary file is removed;
//! - the process died, so a `.tmp` file is orphaned and the live file is intact.
//!
//! There is no window in which the live path holds a partial document, because
//! the only thing that ever touches it is `rename`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::{ExternalIdentityRecord, IdentityImportCounts, IdentityProvider};
use super::romm::normalise::{NormalisedPlatform, RejectedHash};

/// The cache format version.
///
/// Bumped when the on-disk shape changes incompatibly. A cache written by a
/// different version is refused rather than misread - and refusing means
/// "re-import", not "lose the library".
pub const CACHE_FORMAT_VERSION: u32 = 1;

/// The most records one cache may hold. A bound on the whole import, so a
/// runaway server cannot produce an unbounded file.
pub const MAX_CACHED_RECORDS: usize = 200_000;

/// The file name inside the identity directory.
pub const CACHE_FILE_NAME: &str = "identity-cache.json";

/// Where a provider's cache lives, and how it is opened.
#[derive(Debug, Clone)]
pub struct IdentityCacheLocation {
    directory: PathBuf,
}

impl IdentityCacheLocation {
    /// The cache directory for one provider, beneath an ArchiveFS-owned root.
    pub fn new(identity_root: &Path, provider: IdentityProvider) -> Self {
        Self {
            directory: identity_root.join(provider.slug()),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn cache_path(&self) -> PathBuf {
        self.directory.join(CACHE_FILE_NAME)
    }

    /// The size of the published cache on disk, for a status view.
    pub fn cache_size_bytes(&self) -> Option<u64> {
        fs::metadata(self.cache_path()).ok().map(|meta| meta.len())
    }

    pub fn exists(&self) -> bool {
        self.cache_path().is_file()
    }
}

/// What one successful import produced, as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityCache {
    /// Refused if it is not [`CACHE_FORMAT_VERSION`].
    pub format_version: u32,
    pub provider: IdentityProvider,
    /// The instance this came from - the approved origin, never a token. A cache
    /// from a different server is refused rather than mixed in.
    pub server_id: String,
    /// What the instance said its version was, for provenance.
    pub server_version: Option<String>,
    /// A fingerprint of the configuration that produced this cache, so a changed
    /// mapping is visible as a reason to refresh.
    pub source_fingerprint: String,
    pub imported_at_unix_seconds: i64,
    pub platforms: Vec<NormalisedPlatform>,
    pub records: Vec<ExternalIdentityRecord>,
    pub rejected_hashes: Vec<RejectedHash>,
    /// Provider platform names the registry did not recognise.
    pub unknown_platforms: Vec<String>,
    /// The total the server reported, kept so a later refresh can notice a large
    /// discrepancy.
    pub server_reported_total: Option<u64>,
}

/// Why a cache cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum CacheRefusal {
    Missing,
    Unreadable {
        detail: String,
    },
    /// The bytes are not a cache document at all.
    Corrupt {
        detail: String,
    },
    /// Written by an incompatible version.
    VersionMismatch {
        found: u32,
        expected: u32,
    },
    /// From a different RomM instance than the one now configured.
    ServerMismatch {
        found: String,
        expected: String,
    },
    /// Internally inconsistent - the counts do not describe the records.
    Inconsistent {
        detail: String,
    },
    TooManyRecords {
        count: usize,
        maximum: usize,
    },
}

impl CacheRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::Missing => "no identity has been imported yet".to_string(),
            Self::Unreadable { detail } => format!("the cache could not be read: {detail}"),
            Self::Corrupt { detail } => {
                format!("the cache is not a valid identity document and was not used: {detail}")
            }
            Self::VersionMismatch { found, expected } => format!(
                "the cache was written in format {found} but this build reads {expected}; \
                 re-import to rebuild it"
            ),
            Self::ServerMismatch { found, expected } => format!(
                "the cache came from {found} but the configured source is {expected}; \
                 re-import to replace it"
            ),
            Self::Inconsistent { detail } => {
                format!("the cache is internally inconsistent and was not used: {detail}")
            }
            Self::TooManyRecords { count, maximum } => {
                format!("the cache holds {count} records, above the {maximum} limit")
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unreadable { .. } => "unreadable",
            Self::Corrupt { .. } => "corrupt",
            Self::VersionMismatch { .. } => "version_mismatch",
            Self::ServerMismatch { .. } => "server_mismatch",
            Self::Inconsistent { .. } => "inconsistent",
            Self::TooManyRecords { .. } => "too_many_records",
        }
    }

    /// Whether this refusal means the file should be left alone rather than
    /// deleted. Everything does: a cache this build cannot read may still be
    /// readable by another, and destroying it would be gratuitous.
    pub fn keeps_file(&self) -> bool {
        true
    }
}

impl IdentityCache {
    /// Checks a cache is internally coherent before it is trusted or published.
    ///
    /// Called on the way in *and* on the way out, so a corrupt document can
    /// neither be read nor written.
    pub fn validate(&self, expected_server: Option<&str>) -> Result<(), CacheRefusal> {
        if self.format_version != CACHE_FORMAT_VERSION {
            return Err(CacheRefusal::VersionMismatch {
                found: self.format_version,
                expected: CACHE_FORMAT_VERSION,
            });
        }
        if self.records.len() > MAX_CACHED_RECORDS {
            return Err(CacheRefusal::TooManyRecords {
                count: self.records.len(),
                maximum: MAX_CACHED_RECORDS,
            });
        }
        if self.server_id.trim().is_empty() {
            return Err(CacheRefusal::Inconsistent {
                detail: "the cache names no server".to_string(),
            });
        }
        if let Some(expected) = expected_server
            && expected != self.server_id
        {
            return Err(CacheRefusal::ServerMismatch {
                found: self.server_id.clone(),
                expected: expected.to_string(),
            });
        }
        // Every record must belong to this cache's provider and server, or the
        // document is describing two things at once.
        for record in &self.records {
            if record.provider != self.provider {
                return Err(CacheRefusal::Inconsistent {
                    detail: "a record names a different provider than the cache".to_string(),
                });
            }
            if record.server_id != self.server_id {
                return Err(CacheRefusal::Inconsistent {
                    detail: "a record names a different server than the cache".to_string(),
                });
            }
            if record.provider_game_id.trim().is_empty() {
                return Err(CacheRefusal::Inconsistent {
                    detail: "a record has no provider id".to_string(),
                });
            }
        }
        Ok(())
    }

    pub fn counts(&self) -> IdentityImportCounts {
        IdentityImportCounts::of(&self.records)
    }

    /// One bounded page of records, for a caller listing them.
    ///
    /// Paginated even though the cache is in memory, so a caller cannot
    /// accidentally format two hundred thousand records into a terminal.
    pub fn page(&self, offset: usize, limit: usize) -> &[ExternalIdentityRecord] {
        let limit = limit.clamp(1, 1000);
        let start = offset.min(self.records.len());
        let end = start.saturating_add(limit).min(self.records.len());
        &self.records[start..end]
    }

    /// The record claiming one ArchiveFS path, if any.
    pub fn record_for_path(&self, path: &Path) -> Option<&ExternalIdentityRecord> {
        self.records
            .iter()
            .find(|record| record.archivefs_path.as_deref() == Some(path))
    }

    /// The RomM platform slug this imported instance uses for a canonical
    /// ArchiveFS platform. This is the safe future directory-organisation seam:
    /// it returns provider data already mapped through the registry, never a
    /// slug guessed from a display label. If an instance reports duplicates,
    /// the lexicographically first slug is chosen deterministically.
    pub fn romm_slug_for_platform(&self, canonical_platform: &str) -> Option<&str> {
        crate::platform::platform_by_id(canonical_platform)?;
        self.platforms
            .iter()
            .filter(|platform| platform.canonical.as_deref() == Some(canonical_platform))
            .map(|platform| platform.provider_slug.as_str())
            .min()
    }

    /// Every record with a conflict.
    pub fn conflicts(&self) -> Vec<&ExternalIdentityRecord> {
        self.records
            .iter()
            .filter(|record| record.has_conflicts())
            .collect()
    }

    /// Sorts the cache into a deterministic order, so the serialised bytes for a
    /// given import are always identical.
    pub fn sort_deterministically(&mut self) {
        self.records.sort_by(|left, right| {
            left.provider_game_id
                .cmp(&right.provider_game_id)
                .then_with(|| left.provider_path.cmp(&right.provider_path))
        });
        self.platforms
            .sort_by(|left, right| left.provider_slug.cmp(&right.provider_slug));
        self.rejected_hashes.sort_by(|left, right| {
            left.provider_game_id
                .cmp(&right.provider_game_id)
                .then_with(|| left.algorithm.cmp(&right.algorithm))
        });
        self.unknown_platforms.sort();
        self.unknown_platforms.dedup();
    }
}

/// Reads the published cache, validating it before it is trusted.
///
/// This is the offline path: it makes no network request of any kind, which is
/// what makes browsing work with RomM switched off.
pub fn load_cache(
    location: &IdentityCacheLocation,
    expected_server: Option<&str>,
) -> Result<IdentityCache, CacheRefusal> {
    let path = location.cache_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CacheRefusal::Missing);
        }
        Err(error) => {
            return Err(CacheRefusal::Unreadable {
                detail: error.kind().to_string(),
            });
        }
    };
    let cache: IdentityCache =
        serde_json::from_slice(&bytes).map_err(|error| CacheRefusal::Corrupt {
            detail: format!("invalid JSON at line {}", error.line()),
        })?;
    cache.validate(expected_server)?;
    Ok(cache)
}

/// Publishes a cache atomically, or leaves the previous one untouched.
///
/// The sequence is deliberate: validate in memory, write a temporary file, sync
/// it, read it back and validate *that*, then rename. A failure at any step
/// removes the temporary file and returns without touching the live path.
pub fn publish_cache(
    location: &IdentityCacheLocation,
    cache: &IdentityCache,
) -> Result<PathBuf, PublishFailure> {
    // Refuse to publish something that could not be read back.
    cache
        .validate(None)
        .map_err(|refusal| PublishFailure::Invalid { refusal })?;

    fs::create_dir_all(location.directory()).map_err(|error| PublishFailure::WriteFailed {
        detail: format!("the cache directory could not be created: {}", error.kind()),
    })?;

    let final_path = location.cache_path();
    let temporary = location.directory().join(format!(
        ".{CACHE_FILE_NAME}-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));

    let bytes = serde_json::to_vec(cache).map_err(|error| PublishFailure::WriteFailed {
        detail: format!("the cache could not be serialised: {error}"),
    })?;

    // Write, flush and fsync, following the convention the existing cheat-source
    // cache uses: a rename is only atomic with respect to data that has actually
    // reached the disk.
    let write = (|| -> std::io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = fs::remove_file(&temporary);
        return Err(PublishFailure::WriteFailed {
            detail: error.kind().to_string(),
        });
    }

    // Read the temporary file back and validate it. A cache that cannot be
    // re-read is never allowed to replace one that can.
    match fs::read(&temporary)
        .map_err(|error| error.kind().to_string())
        .and_then(|written| {
            serde_json::from_slice::<IdentityCache>(&written)
                .map_err(|error| format!("invalid JSON at line {}", error.line()))
        })
        .and_then(|reread| {
            reread
                .validate(Some(&cache.server_id))
                .map_err(|refusal| refusal.detail())
        }) {
        Ok(()) => {}
        Err(detail) => {
            let _ = fs::remove_file(&temporary);
            return Err(PublishFailure::ReadbackFailed { detail });
        }
    }

    // The only operation that ever touches the live path.
    if let Err(error) = fs::rename(&temporary, &final_path) {
        let _ = fs::remove_file(&temporary);
        return Err(PublishFailure::WriteFailed {
            detail: format!("the cache could not be published: {}", error.kind()),
        });
    }
    Ok(final_path)
}

/// Why publication failed. In every case the previous cache is untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum PublishFailure {
    Invalid { refusal: CacheRefusal },
    WriteFailed { detail: String },
    ReadbackFailed { detail: String },
}

impl PublishFailure {
    pub fn detail(&self) -> String {
        match self {
            Self::Invalid { refusal } => format!(
                "the new cache was not published because it is not valid: {}",
                refusal.detail()
            ),
            Self::WriteFailed { detail } => {
                format!(
                    "the new cache could not be written, so the previous one is unchanged: {detail}"
                )
            }
            Self::ReadbackFailed { detail } => format!(
                "the new cache could not be read back after writing, so it was discarded and the \
                 previous one is unchanged: {detail}"
            ),
        }
    }

    /// Always true: no publication failure is a reason to lose working identity.
    pub fn previous_cache_preserved(&self) -> bool {
        true
    }
}

/// Removes the published cache. The explicit-confirmation boundary for a later
/// CLI or GUI stage: this function performs the removal, and the caller is
/// responsible for having asked.
pub fn remove_cache(location: &IdentityCacheLocation) -> std::io::Result<bool> {
    let path = location.cache_path();
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Removes orphaned temporary files left by an interrupted publication.
///
/// Returns how many were removed. Only files matching the temporary prefix are
/// touched, so a published cache can never be caught by this.
pub fn clean_temporary_files(location: &IdentityCacheLocation) -> std::io::Result<usize> {
    let prefix = format!(".{CACHE_FILE_NAME}-");
    let mut removed = 0;
    let Ok(entries) = fs::read_dir(location.directory()) else {
        return Ok(0);
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".tmp") {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}
