//! A bounded, provider-owned thumbnail cache.
//!
//! RomM stays the owner of cover art. EmuWiz caches only what a card needs to
//! draw: a thumbnail fitting inside [`THUMBNAIL_MAX_WIDTH`] x
//! [`THUMBNAIL_MAX_HEIGHT`], fetched lazily when artwork becomes visible, never in
//! bulk during a catalogue import.
//!
//! # Only RomM's own artwork is fetchable
//!
//! This is the part worth reading twice. A RomM record carries two artwork
//! references, and they are not equivalent:
//!
//! - `url_cover` points at whatever scraper supplied the metadata. On a real
//!   instance those were `https://images.igdb.com/...` and
//!   `https://retroachievements.org/...` - public hosts on the open internet.
//! - `path_cover_small` is a path on the RomM instance itself, e.g.
//!   `/assets/romm/resources/roms/149/1/cover/small.png?ts=...`.
//!
//! Only the second is ever fetched. Fetching the first would drive outbound
//! requests to arbitrary public hosts from a subsystem whose entire premise is
//! that it talks to one approved private address - so `url_cover` is recorded for
//! provenance and refused as a fetch target, by
//! [`ArtworkRefusal::RemoteHostNotAllowed`].
//!
//! A reference is resolved against the approved origin and then the *result* is
//! checked to still be that origin. Resolving first and checking after is what
//! makes `//evil.example/x` and `http://evil.example/x` refusals rather than
//! requests: both are legal relative-URL forms that change the host.
//!
//! # Bounded at every step
//!
//! Response bytes, source dimensions, decode allocation, thumbnail dimensions and
//! total cache size all have ceilings. A cover is ~55 KB and 162x216 on the
//! instance this was written against; the limits are set well above that and far
//! below anything that could exhaust memory.
//!
//! # Nothing secret is stored
//!
//! Cache keys are derived from the server identity, the record id and RomM's own
//! artwork timestamp. No token is used in a key, a filename, or the index; the
//! index holds the approved origin, record ids and digests, and no cover path or
//! URL at all. Artwork on the tested instance needs no authentication, so the token
//! is only offered if a request is actually refused without it.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::model::IdentityProvider;
use super::net_policy::EndpointRefusal;
use super::romm::client::{RommRequestError, RommTransport};
use super::romm::config::ValidatedRommSource;

/// The box a thumbnail must fit inside. Matches the GUI's cover aspect closely
/// enough that nothing is stretched: an image is scaled to fit, never to fill.
pub const THUMBNAIL_MAX_WIDTH: u32 = 200;
pub const THUMBNAIL_MAX_HEIGHT: u32 = 280;

/// The most one artwork response may be. A real small cover is about 55 KB.
pub const MAX_ARTWORK_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// The largest source image this will decode, per side and in total allocation.
/// A decompression bomb is a small file that decodes to an enormous buffer, so the
/// allocation ceiling is the one that actually matters.
pub const MAX_SOURCE_DIMENSION: u32 = 2048;
pub const MAX_SOURCE_DECODE_BYTES: u64 = 32 * 1024 * 1024;

/// The whole cache's ceiling, enforced by least-recently-used eviction.
///
/// One gibibyte. At the ~10 KB a 200x280 thumbnail actually encodes to, that is
/// room for far more covers than a 36,259-record catalogue has, so eviction is a
/// backstop rather than something a normal library ever reaches.
pub const MAX_CACHE_BYTES: u64 = 1024 * 1024 * 1024;

/// Bumped only if the on-disk layout changes. An index written by a newer version
/// is discarded rather than misread - thumbnails are derived data and refetching
/// them costs nothing but time.
pub const ARTWORK_FORMAT_VERSION: u32 = 1;

pub const ARTWORK_DIRECTORY_NAME: &str = "artwork";
pub const THUMBNAIL_DIRECTORY_NAME: &str = "thumbnails";
pub const INDEX_FILE_NAME: &str = "index.json";

/// How stale a recorded "last used" may get before a hit rewrites the index.
///
/// Strict LRU would write the index on every cache hit, which for a scrolling grid
/// means a write per frame. Recording the time only when it has drifted by more
/// than this keeps eviction order meaningful while leaving reads almost always
/// write-free.
pub const LAST_USED_WRITE_INTERVAL_SECONDS: i64 = 3600;

/// What a caller wants a thumbnail for.
#[derive(Debug, Clone, Copy)]
pub struct ArtworkRequest<'a> {
    pub provider_game_id: &'a str,
    /// RomM's own small-cover path. The only fetchable reference.
    pub small_reference: Option<&'a str>,
    /// Whatever scraper URL RomM recorded. Kept for provenance, never fetched.
    pub public_reference: Option<&'a str>,
}

impl<'a> ArtworkRequest<'a> {
    /// Builds a request from a cached record.
    pub fn from_record(record: &'a super::model::ExternalIdentityRecord) -> Self {
        let artwork = record.artwork.as_ref();
        Self {
            provider_game_id: &record.provider_game_id,
            small_reference: artwork.and_then(|art| art.small_reference.as_deref()),
            public_reference: artwork.map(|art| art.reference.as_str()),
        }
    }
}

/// Why a thumbnail is not available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum ArtworkRefusal {
    /// The record has no artwork of any kind. A placeholder is the right answer,
    /// and asking again will not help.
    NoArtwork,
    /// The record has only a public scraper URL. Deliberately not fetched.
    RemoteHostNotAllowed {
        host: String,
    },
    UnsupportedReference {
        detail: String,
    },
    Endpoint(EndpointRefusal),
    Request(RommRequestError),
    TooLarge {
        bytes: usize,
        maximum: usize,
    },
    /// The bytes are not an image this build decodes.
    NotAnImage {
        detail: String,
    },
    DimensionsTooLarge {
        width: u32,
        height: u32,
        maximum: u32,
    },
    DecodeFailed,
    WriteFailed {
        detail: String,
    },
    Cancelled,
    /// The cache is not readable and could not be created.
    CacheUnusable {
        detail: String,
    },
}

impl ArtworkRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::NoArtwork => "RomM has no cover for this game".to_string(),
            Self::RemoteHostNotAllowed { host } => format!(
                "RomM's only cover for this game is hosted at {host}, on the public internet. \
                 EmuWiz fetches artwork from your RomM instance and nowhere else, so this one \
                 is left as a placeholder. Letting RomM download the cover into its own library \
                 makes it available here."
            ),
            Self::UnsupportedReference { detail } => {
                format!("that cover reference cannot be used: {detail}")
            }
            Self::Endpoint(refusal) => refusal.detail(),
            Self::Request(error) => error.detail(),
            Self::TooLarge { bytes, maximum } => format!(
                "the cover was {bytes} bytes, over the {maximum}-byte ceiling, and was not read"
            ),
            Self::NotAnImage { detail } => {
                format!("what came back was not an image this build can read: {detail}")
            }
            Self::DimensionsTooLarge {
                width,
                height,
                maximum,
            } => format!(
                "the cover is {width}x{height}, and each side must be at most {maximum} pixels; \
                 it was not decoded"
            ),
            Self::DecodeFailed => "the cover could not be decoded".to_string(),
            Self::WriteFailed { detail } => {
                format!("the thumbnail could not be written: {detail}")
            }
            Self::Cancelled => "fetching the cover was cancelled".to_string(),
            Self::CacheUnusable { detail } => {
                format!("the thumbnail cache is not usable: {detail}")
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NoArtwork => "no_artwork",
            Self::RemoteHostNotAllowed { .. } => "remote_host_not_allowed",
            Self::UnsupportedReference { .. } => "unsupported_reference",
            Self::Endpoint(_) => "endpoint_refused",
            Self::Request(error) => error.code(),
            Self::TooLarge { .. } => "too_large",
            Self::NotAnImage { .. } => "not_an_image",
            Self::DimensionsTooLarge { .. } => "dimensions_too_large",
            Self::DecodeFailed => "decode_failed",
            Self::WriteFailed { .. } => "write_failed",
            Self::Cancelled => "cancelled",
            Self::CacheUnusable { .. } => "cache_unusable",
        }
    }

    /// Whether asking again could ever succeed. A placeholder for a permanent
    /// refusal should not turn into a request storm.
    pub fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::NoArtwork
                | Self::RemoteHostNotAllowed { .. }
                | Self::UnsupportedReference { .. }
                | Self::NotAnImage { .. }
                | Self::DimensionsTooLarge { .. }
                | Self::DecodeFailed
        )
    }
}

/// A thumbnail that is on disk now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CachedThumbnail {
    pub key: String,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
}

/// One index entry. No URL, no token, no secret - only what eviction and
/// invalidation need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkEntry {
    pub key: String,
    pub provider_game_id: String,
    /// A digest of RomM's own artwork identity - usually the `ts` query it appends
    /// to the cover path. Stored as a digest rather than the path itself: the cache
    /// key already encodes the identity, so nothing here needs the literal
    /// reference, and an index that holds no paths cannot leak one.
    pub artwork_identity_digest: String,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub stored_at_unix_seconds: i64,
    pub last_used_unix_seconds: i64,
}

/// The on-disk index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkIndex {
    pub format_version: u32,
    /// The instance these thumbnails came from. Another server's covers are not
    /// reused, the same way its identity records are not.
    pub server_id: String,
    pub entries: Vec<ArtworkEntry>,
    pub last_cleanup_unix_seconds: Option<i64>,
}

impl ArtworkIndex {
    fn empty(server_id: &str) -> Self {
        Self {
            format_version: ARTWORK_FORMAT_VERSION,
            server_id: server_id.to_string(),
            entries: Vec::new(),
            last_cleanup_unix_seconds: None,
        }
    }

    fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }
}

/// What the GUI shows about the cache itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtworkCacheStats {
    pub items: usize,
    pub bytes: u64,
    pub maximum_bytes: u64,
    pub last_cleanup_unix_seconds: Option<i64>,
    pub directory: PathBuf,
    pub format_version: u32,
}

/// What a clear removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtworkClearOutcome {
    pub removed_items: usize,
    pub removed_bytes: u64,
    /// Stated because it is the thing a person is really asking about.
    pub identity_cache_touched: bool,
}

/// What eviction removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtworkEvictionOutcome {
    pub evicted_items: usize,
    pub evicted_bytes: u64,
    pub bytes_after: u64,
}

// --- Serialising index updates -------------------------------------------
//
// Every entry in the index is a read-modify-write of one shared file, and the
// cache has more than one caller: the Gamer View list resolves covers on its own
// worker while the Details panel and the record browser resolve theirs through the
// GUI's operation queue, and the CLI can be doing the same against the same
// directory. Two callers that each read the index, add their own entry and rename
// their own version over the file lose one of the two entries - the rename is
// atomic, so the file is never torn, but the *update* is not, so the second writer
// silently reverts the first.
//
// The fix is to make every modification re-read the index while holding a lock and
// merge into whatever is there now. Reads stay unlocked: a rename is atomic, so a
// reader sees either the whole previous index or the whole next one, never a mix.

/// How long a caller waits for another process to finish its index update.
///
/// Long enough to outlast any write of a file this size, short enough that a stale
/// lock from a killed process cannot wedge the UI. Timing out is not fatal - the
/// caller reports a failed write and the thumbnail is refetched later.
const INDEX_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const INDEX_LOCK_RETRY: Duration = Duration::from_millis(10);

/// Held only while the index is being rewritten, never during a fetch or a decode.
pub const INDEX_LOCK_FILE_NAME: &str = "index.lock";

/// Whether a locked update actually changed anything.
///
/// [`ArtworkCache::touch`] deliberately leaves the index alone most of the time -
/// writing on every cache hit is what this type exists to avoid - so "I looked and
/// there was nothing to do" has to be expressible without writing the file back.
enum IndexChange<T> {
    /// Persist the index as the closure left it.
    Write(T),
    /// Nothing changed. The file is not rewritten.
    Keep(T),
}

/// One mutex per index path, shared by every `ArtworkCache` in this process.
///
/// The mutex cannot live on `ArtworkCache` itself: callers construct their own with
/// `ArtworkCache::new` whenever they need one - the GUI does it in three separate
/// places - so a per-instance mutex would guard nothing. Keying on the path is what
/// makes two independently constructed caches over the same directory serialise,
/// and what lets two caches over *different* directories proceed in parallel.
fn index_mutex(index_path: &Path) -> Arc<Mutex<()>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        registry
            .entry(index_path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(()))),
    )
}

/// An advisory cross-process lock on one artwork index.
///
/// Best effort by design. The process-local mutex above is the guarantee - exact,
/// needing no filesystem support, holding on every platform. This layer covers the
/// case the mutex cannot see: a second EmuWiz, typically the CLI clearing
/// thumbnails while the GUI is browsing. It works on any filesystem that
/// implements `flock` honestly, which a local one does and a network mount may not,
/// so a failure to acquire it is not treated as a failure to update.
struct IndexFileLock {
    file: fs::File,
}

impl IndexFileLock {
    /// Returns `None` when the platform or filesystem cannot lock.
    ///
    /// Deliberately not an error: on a filesystem without working `flock` the
    /// process-local mutex still holds, and refusing every write there would break
    /// the cache for a guarantee it never had.
    fn acquire(directory: &Path, timeout: Duration) -> Option<Self> {
        let path = directory.join(INDEX_LOCK_FILE_NAME);
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .ok()?;
        let deadline = Instant::now().checked_add(timeout);
        loop {
            match try_lock_exclusive(&file) {
                Ok(true) => return Some(Self { file }),
                Ok(false) => {
                    if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                        // Another process is taking an implausibly long time, or was
                        // killed holding the lock. The process-local mutex still
                        // orders this process's own callers, so proceeding is safer
                        // than refusing: the worst case is the lost update this
                        // whole mechanism exists to make rare, and the best case is
                        // a working cache.
                        return None;
                    }
                    std::thread::sleep(INDEX_LOCK_RETRY.min(timeout));
                }
                Err(()) => return None,
            }
        }
    }
}

impl Drop for IndexFileLock {
    fn drop(&mut self) {
        unlock_file(&self.file);
    }
}

#[cfg(unix)]
fn try_lock_exclusive(file: &fs::File) -> Result<bool, ()> {
    use std::os::unix::io::AsRawFd as _;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    // Compared rather than matched: `EWOULDBLOCK` and `EAGAIN` are the same value on
    // Linux and distinct elsewhere, which a match arm cannot express portably.
    let raw = error.raw_os_error();
    if raw == Some(libc::EWOULDBLOCK) || raw == Some(libc::EAGAIN) || raw == Some(libc::EINTR) {
        Ok(false)
    } else {
        Err(())
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &fs::File) -> Result<bool, ()> {
    Err(())
}

#[cfg(unix)]
fn unlock_file(file: &fs::File) {
    use std::os::unix::io::AsRawFd as _;
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

#[cfg(not(unix))]
fn unlock_file(_file: &fs::File) {}

/// A provider-owned thumbnail cache on disk.
///
/// Separate from the identity JSON: clearing thumbnails must never risk the
/// identity cache, and the two have completely different lifetimes.
#[derive(Debug, Clone)]
pub struct ArtworkCache {
    directory: PathBuf,
}

impl ArtworkCache {
    pub fn new(identity_root: &Path, provider: IdentityProvider) -> Self {
        Self {
            directory: identity_root
                .join(provider.slug())
                .join(ARTWORK_DIRECTORY_NAME),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn index_path(&self) -> PathBuf {
        self.directory.join(INDEX_FILE_NAME)
    }

    fn thumbnails_directory(&self) -> PathBuf {
        self.directory.join(THUMBNAIL_DIRECTORY_NAME)
    }

    fn thumbnail_path(&self, key: &str) -> PathBuf {
        self.thumbnails_directory().join(format!("{key}.png"))
    }

    /// Loads the index, or an empty one.
    ///
    /// A missing, unreadable, wrong-version or wrong-server index all yield an
    /// empty index rather than an error: every thumbnail is refetchable, so the
    /// safe response to anything unexpected is to start again.
    fn load_index(&self, server_id: &str) -> ArtworkIndex {
        let Ok(text) = fs::read_to_string(self.index_path()) else {
            return ArtworkIndex::empty(server_id);
        };
        match serde_json::from_str::<ArtworkIndex>(&text) {
            Ok(index)
                if index.format_version == ARTWORK_FORMAT_VERSION
                    && index.server_id == server_id =>
            {
                index
            }
            _ => ArtworkIndex::empty(server_id),
        }
    }

    /// Publishes the index atomically: a torn index would lose the whole cache's
    /// accounting, which is how a bounded cache stops being bounded.
    fn save_index(&self, index: &ArtworkIndex) -> Result<(), ArtworkRefusal> {
        fs::create_dir_all(&self.directory).map_err(|error| ArtworkRefusal::CacheUnusable {
            detail: error.to_string(),
        })?;
        let bytes =
            serde_json::to_vec_pretty(index).map_err(|error| ArtworkRefusal::WriteFailed {
                detail: error.to_string(),
            })?;
        let temporary = self
            .directory
            .join(format!("{INDEX_FILE_NAME}.{}.tmp", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&temporary, self.index_path())
        };
        if let Err(error) = write() {
            let _ = fs::remove_file(&temporary);
            return Err(ArtworkRefusal::WriteFailed {
                detail: error.to_string(),
            });
        }
        Ok(())
    }

    /// Runs one read-modify-write of the index under exclusive access.
    ///
    /// This is the only path by which the index is ever modified. The index is
    /// re-read *inside* the lock and handed to `apply`, so a closure always merges
    /// into whatever the latest state is rather than into a snapshot taken before
    /// some other caller's write.
    ///
    /// Nothing slow belongs in `apply`. A fetch and a decode both happen before this
    /// is called; what runs here is a vector edit and, for eviction, some file
    /// removals - all of which are bounded and local.
    fn update_index<R>(
        &self,
        server_id: &str,
        apply: impl FnOnce(&mut ArtworkIndex) -> IndexChange<R>,
    ) -> Result<R, ArtworkRefusal> {
        let mutex = index_mutex(&self.index_path());
        // A poisoned mutex means another thread panicked mid-update. The file on
        // disk is still whole - it is only ever replaced by rename - so the right
        // answer is to carry on and re-read it, not to refuse every write for the
        // rest of this process's life.
        let _process = mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fs::create_dir_all(&self.directory).map_err(|error| ArtworkRefusal::CacheUnusable {
            detail: error.to_string(),
        })?;
        let _file = IndexFileLock::acquire(&self.directory, INDEX_LOCK_TIMEOUT);

        let mut index = self.load_index(server_id);
        match apply(&mut index) {
            IndexChange::Write(value) => {
                self.save_index(&index)?;
                Ok(value)
            }
            IndexChange::Keep(value) => Ok(value),
        }
    }

    /// The cache key for one request.
    ///
    /// Deterministic, and derived only from non-secret facts: the server identity,
    /// the record id, and RomM's own artwork identity. The same request always
    /// produces the same key, and a replaced cover produces a different one.
    pub fn key_for(server_id: &str, request: &ArtworkRequest<'_>) -> String {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(server_id.as_bytes());
        digest.update(b"\0");
        digest.update(request.provider_game_id.as_bytes());
        digest.update(b"\0");
        digest.update(artwork_identity(request).as_bytes());
        digest
            .finalize()
            .iter()
            .take(16)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Looks a thumbnail up without any network access.
    ///
    /// This is what offline browsing uses. A thumbnail whose file has gone or is no
    /// longer a valid PNG is treated as absent and dropped from the index, so a
    /// corrupted entry is refetched rather than drawn as a broken image.
    pub fn lookup(&self, server_id: &str, request: &ArtworkRequest<'_>) -> Option<CachedThumbnail> {
        let key = Self::key_for(server_id, request);
        let index = self.load_index(server_id);
        let entry = index.entries.iter().find(|entry| entry.key == key)?;
        let path = self.thumbnail_path(&key);
        let bytes = fs::read(&path).ok();
        let valid = bytes
            .as_deref()
            .is_some_and(|bytes| detect_format(bytes) == Some(ImageFormat::Png));
        if !valid {
            // Corrupt or vanished: forget it so the next request refetches. The read
            // above needed no lock - a rename is atomic, so a reader sees one whole
            // index or the other - but removing the entry is a modification, and
            // goes through the one path that merges against the latest state.
            let _ = fs::remove_file(&path);
            let _ = self.update_index(server_id, |index| {
                index.entries.retain(|entry| entry.key != key);
                IndexChange::Write(())
            });
            return None;
        }
        let thumbnail = CachedThumbnail {
            key: key.clone(),
            path,
            width: entry.width,
            height: entry.height,
            bytes: entry.bytes,
        };
        Some(thumbnail)
    }

    /// Records that a thumbnail was used, for eviction order.
    ///
    /// Writes the index only when the recorded time has drifted by more than
    /// [`LAST_USED_WRITE_INTERVAL_SECONDS`], so a scrolling grid does not write on
    /// every frame.
    pub fn touch(&self, server_id: &str, key: &str, now_unix_seconds: i64) {
        // An unlocked pre-check first, so the overwhelmingly common case - a hit
        // whose recorded time is still fresh - costs one read and takes no lock at
        // all. The decision is made again inside the lock, against the real state.
        let fresh = self
            .load_index(server_id)
            .entries
            .iter()
            .find(|entry| entry.key == key)
            .is_none_or(|entry| {
                now_unix_seconds - entry.last_used_unix_seconds <= LAST_USED_WRITE_INTERVAL_SECONDS
            });
        if fresh {
            return;
        }
        let _ = self.update_index(server_id, |index| {
            let Some(entry) = index.entries.iter_mut().find(|entry| entry.key == key) else {
                // Evicted between the pre-check and the lock. Nothing to record.
                return IndexChange::Keep(());
            };
            if now_unix_seconds - entry.last_used_unix_seconds <= LAST_USED_WRITE_INTERVAL_SECONDS {
                return IndexChange::Keep(());
            }
            entry.last_used_unix_seconds = now_unix_seconds;
            IndexChange::Write(())
        });
    }

    /// Fetches, validates, thumbnails and stores one cover.
    ///
    /// Returns a cached thumbnail on success. Makes exactly one request, or none at
    /// all when the reference is not fetchable.
    pub fn fetch<T: RommTransport>(
        &self,
        source: &ValidatedRommSource,
        transport: &T,
        request: &ArtworkRequest<'_>,
        now_unix_seconds: i64,
        cancel: Option<&AtomicBool>,
    ) -> Result<CachedThumbnail, ArtworkRefusal> {
        let server_id = source.server_id().to_string();
        let url = self.resolve(source, request)?;
        if cancelled(cancel) {
            return Err(ArtworkRefusal::Cancelled);
        }

        // Artwork on the tested instance needs no authentication, so the token is
        // not offered first. It is only used if the instance actually refuses -
        // there is no reason to hand a credential to a request that does not want
        // one.
        let response = match transport.get(&url, None, MAX_ARTWORK_RESPONSE_BYTES) {
            Ok(response) if response.status == 401 || response.status == 403 => source
                .token()
                .with_header_value(|header| {
                    transport.get(&url, Some(header), MAX_ARTWORK_RESPONSE_BYTES)
                })
                .map_err(ArtworkRefusal::Request)?,
            Ok(response) => response,
            Err(RommRequestError::Unauthorised { .. }) => source
                .token()
                .with_header_value(|header| {
                    transport.get(&url, Some(header), MAX_ARTWORK_RESPONSE_BYTES)
                })
                .map_err(ArtworkRefusal::Request)?,
            Err(error) => return Err(ArtworkRefusal::Request(error)),
        };
        if cancelled(cancel) {
            return Err(ArtworkRefusal::Cancelled);
        }
        if response.status != 200 {
            return Err(ArtworkRefusal::Request(RommRequestError::HttpStatus {
                status: response.status,
            }));
        }
        if response.body.len() > MAX_ARTWORK_RESPONSE_BYTES {
            return Err(ArtworkRefusal::TooLarge {
                bytes: response.body.len(),
                maximum: MAX_ARTWORK_RESPONSE_BYTES,
            });
        }

        let thumbnail = render_thumbnail(&response.body)?;
        if cancelled(cancel) {
            return Err(ArtworkRefusal::Cancelled);
        }
        self.store(&server_id, request, thumbnail, now_unix_seconds)
    }

    /// Resolves a request to a URL on the approved origin, or refuses.
    fn resolve(
        &self,
        source: &ValidatedRommSource,
        request: &ArtworkRequest<'_>,
    ) -> Result<String, ArtworkRefusal> {
        let Some(reference) = request
            .small_reference
            .map(str::trim)
            .filter(|r| !r.is_empty())
        else {
            // No RomM-hosted cover. If there is a public one, say which host it is
            // and why it is not used; otherwise there is simply no artwork.
            return match request
                .public_reference
                .map(str::trim)
                .filter(|r| !r.is_empty())
            {
                Some(public) => Err(ArtworkRefusal::RemoteHostNotAllowed {
                    host: host_of(public).unwrap_or_else(|| "an external host".to_string()),
                }),
                None => Err(ArtworkRefusal::NoArtwork),
            };
        };
        let origin = source.endpoint().origin();
        let base =
            url::Url::parse(origin).map_err(|error| ArtworkRefusal::UnsupportedReference {
                detail: error.to_string(),
            })?;
        // Resolve first, then check the result. A reference like `//other/x` or
        // `http://other/x` is a legal relative URL that changes the host, so only
        // the resolved origin can be trusted to answer "where would this go".
        let resolved =
            base.join(reference)
                .map_err(|error| ArtworkRefusal::UnsupportedReference {
                    detail: error.to_string(),
                })?;
        if resolved.scheme() != base.scheme() || resolved.host_str() != base.host_str() {
            return Err(ArtworkRefusal::RemoteHostNotAllowed {
                host: resolved
                    .host_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| resolved.scheme().to_string()),
            });
        }
        if resolved.port_or_known_default() != base.port_or_known_default() {
            return Err(ArtworkRefusal::RemoteHostNotAllowed {
                host: format!(
                    "{}:{}",
                    resolved.host_str().unwrap_or("?"),
                    resolved.port_or_known_default().unwrap_or(0)
                ),
            });
        }
        // `Url::join` has already resolved any `..` away, and the origin check above
        // is what guarantees the result is still RomM. This is a belt-and-braces
        // check for a future caller that builds a URL some other way; a surviving
        // `..` would mean the resolver did not normalise, which is worth refusing
        // rather than sending.
        if resolved.path().split('/').any(|segment| segment == "..") {
            return Err(ArtworkRefusal::UnsupportedReference {
                detail: "the reference contains an unresolved `..` segment".to_string(),
            });
        }
        Ok(resolved.to_string())
    }

    /// Writes a rendered thumbnail and updates the index, then evicts if needed.
    fn store(
        &self,
        server_id: &str,
        request: &ArtworkRequest<'_>,
        thumbnail: RenderedThumbnail,
        now_unix_seconds: i64,
    ) -> Result<CachedThumbnail, ArtworkRefusal> {
        let key = Self::key_for(server_id, request);
        let directory = self.thumbnails_directory();
        fs::create_dir_all(&directory).map_err(|error| ArtworkRefusal::CacheUnusable {
            detail: error.to_string(),
        })?;
        let path = self.thumbnail_path(&key);
        // Atomic: a half-written PNG must never be visible as a cache hit.
        let temporary = directory.join(format!("{key}.{}.tmp", std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(&thumbnail.png)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        };
        if let Err(error) = write() {
            let _ = fs::remove_file(&temporary);
            return Err(ArtworkRefusal::WriteFailed {
                detail: error.to_string(),
            });
        }

        // The fetch and the decode are already done, so the lock is taken now and
        // held only for the vector edit and the eviction that follows it. Recording
        // the entry and trimming to the ceiling happen in one locked update: two
        // separate ones would leave a window in which the cache is over its limit,
        // and would take the lock twice for one logical change.
        let entry = ArtworkEntry {
            key: key.clone(),
            provider_game_id: request.provider_game_id.to_string(),
            artwork_identity_digest: digest_of(&artwork_identity(request)),
            width: thumbnail.width,
            height: thumbnail.height,
            bytes: thumbnail.png.len() as u64,
            stored_at_unix_seconds: now_unix_seconds,
            last_used_unix_seconds: now_unix_seconds,
        };
        self.update_index(server_id, |index| {
            index.entries.retain(|held| held.key != key);
            index.entries.push(entry);
            // Kept inside the ceiling on the way in, so the cache cannot grow past
            // it between explicit cleanups.
            self.evict_within(index, MAX_CACHE_BYTES, now_unix_seconds);
            IndexChange::Write(())
        })?;

        Ok(CachedThumbnail {
            key,
            path,
            width: thumbnail.width,
            height: thumbnail.height,
            bytes: thumbnail.png.len() as u64,
        })
    }

    /// Evicts least-recently-used thumbnails until the cache fits `maximum`.
    pub fn evict_to_limit(
        &self,
        server_id: &str,
        maximum: u64,
        now_unix_seconds: i64,
    ) -> ArtworkEvictionOutcome {
        // An eviction that decided what to remove from a stale snapshot would delete
        // thumbnails another caller had just recorded, so the decision is made
        // against the index as it is under the lock.
        self.update_index(server_id, |index| {
            let outcome = self.evict_within(index, maximum, now_unix_seconds);
            if outcome.evicted_items == 0 {
                IndexChange::Keep(outcome)
            } else {
                IndexChange::Write(outcome)
            }
        })
        .unwrap_or_else(|_| ArtworkEvictionOutcome {
            evicted_items: 0,
            evicted_bytes: 0,
            bytes_after: self.load_index(server_id).total_bytes(),
        })
    }

    /// The eviction itself, against an index the caller already holds the lock for.
    ///
    /// Separate from [`Self::evict_to_limit`] so a fetch can record its entry and
    /// trim to the ceiling in one locked update rather than taking the lock twice -
    /// and so neither can deadlock by nesting the lock inside itself.
    fn evict_within(
        &self,
        index: &mut ArtworkIndex,
        maximum: u64,
        now_unix_seconds: i64,
    ) -> ArtworkEvictionOutcome {
        let mut total = index.total_bytes();
        if total <= maximum {
            return ArtworkEvictionOutcome {
                evicted_items: 0,
                evicted_bytes: 0,
                bytes_after: total,
            };
        }
        // Oldest use first; the stored time then the key break ties, so two runs
        // over the same cache evict the same things.
        index.entries.sort_by(|left, right| {
            left.last_used_unix_seconds
                .cmp(&right.last_used_unix_seconds)
                .then_with(|| {
                    left.stored_at_unix_seconds
                        .cmp(&right.stored_at_unix_seconds)
                })
                .then_with(|| left.key.cmp(&right.key))
        });
        let mut evicted_items = 0;
        let mut evicted_bytes = 0;
        let mut keep = Vec::with_capacity(index.entries.len());
        for entry in std::mem::take(&mut index.entries) {
            if total > maximum {
                total = total.saturating_sub(entry.bytes);
                evicted_bytes += entry.bytes;
                evicted_items += 1;
                let _ = fs::remove_file(self.thumbnail_path(&entry.key));
            } else {
                keep.push(entry);
            }
        }
        index.entries = keep;
        index.last_cleanup_unix_seconds = Some(now_unix_seconds);
        ArtworkEvictionOutcome {
            evicted_items,
            evicted_bytes,
            bytes_after: total,
        }
    }

    pub fn stats(&self, server_id: &str) -> ArtworkCacheStats {
        let index = self.load_index(server_id);
        ArtworkCacheStats {
            items: index.entries.len(),
            bytes: index.total_bytes(),
            maximum_bytes: MAX_CACHE_BYTES,
            last_cleanup_unix_seconds: index.last_cleanup_unix_seconds,
            directory: self.directory.clone(),
            format_version: index.format_version,
        }
    }

    /// Removes every thumbnail and the index.
    ///
    /// Touches only this directory, so the identity cache beside it is unaffected -
    /// which is the whole reason artwork lives in its own subtree.
    pub fn clear(
        &self,
        server_id: &str,
        confirmed: bool,
    ) -> Result<ArtworkClearOutcome, ArtworkRefusal> {
        if !confirmed {
            return Err(ArtworkRefusal::WriteFailed {
                detail: "clearing the thumbnail cache needs confirmation".to_string(),
            });
        }
        // Under the same lock every write takes, so a fetch that is midway through
        // recording an entry finishes first and a clear cannot leave an index
        // describing thumbnails it has already deleted. `Keep` because the index
        // file is removed outright rather than rewritten.
        self.update_index(server_id, |index| {
            let outcome = ArtworkClearOutcome {
                removed_items: index.entries.len(),
                removed_bytes: index.total_bytes(),
                identity_cache_touched: false,
            };
            // Only this provider's artwork subtree, never its parent.
            let _ = fs::remove_dir_all(self.thumbnails_directory());
            let _ = fs::remove_file(self.index_path());
            IndexChange::Keep(outcome)
        })
    }

    /// Every key currently held, for tests and diagnostics.
    pub fn keys(&self, server_id: &str) -> Vec<String> {
        let mut keys: Vec<String> = self
            .load_index(server_id)
            .entries
            .into_iter()
            .map(|entry| entry.key)
            .collect();
        keys.sort();
        keys
    }
}

/// RomM's own identity for a cover, used in the cache key so a replaced cover
/// invalidates the thumbnail.
///
/// The `ts` query RomM appends to `path_cover_small` changes whenever it rewrites
/// the file, which makes it exactly the right thing to key on. With no query, the
/// path itself is used.
fn artwork_identity(request: &ArtworkRequest<'_>) -> String {
    match request
        .small_reference
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(reference) => reference.to_string(),
        None => request
            .public_reference
            .map(str::trim)
            .unwrap_or("")
            .to_string(),
    }
}

/// A short, stable digest. Used where a value is needed for comparison but the
/// value itself does not belong on disk.
fn digest_of(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::new_with_prefix(value.as_bytes())
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn host_of(reference: &str) -> Option<String> {
    url::Url::parse(reference)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// The image formats this build recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    /// Recognised so the refusal can say what it was, not decoded.
    Jpeg,
    Gif,
    WebP,
}

impl ImageFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::WebP => "WebP",
        }
    }
}

/// Identifies a format from its magic bytes.
///
/// The declared content type is not consulted: it is a claim by the sender, and
/// the bytes are the fact. Anything unrecognised is refused rather than guessed.
pub fn detect_format(bytes: &[u8]) -> Option<ImageFormat> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(PNG) {
        return Some(ImageFormat::Png);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(ImageFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(ImageFormat::WebP);
    }
    None
}

struct RenderedThumbnail {
    png: Vec<u8>,
    width: u32,
    height: u32,
}

/// Decodes a cover and renders a bounded thumbnail.
///
/// PNG only, which is what RomM serves for `path_cover_small` - 29,473 of the
/// 29,759 covers on the instance this was measured against, with the remainder
/// carrying no extension and resolved by magic bytes. Another format is named in
/// the refusal rather than silently skipped, so the reason is actionable instead of
/// looking like a missing cover.
fn render_thumbnail(bytes: &[u8]) -> Result<RenderedThumbnail, ArtworkRefusal> {
    use image::{ImageDecoder as _, ImageEncoder as _};

    match detect_format(bytes) {
        Some(ImageFormat::Png) => {}
        Some(other) => {
            return Err(ArtworkRefusal::NotAnImage {
                detail: format!(
                    "RomM sent a {} cover; this build reads PNG thumbnails only",
                    other.label()
                ),
            });
        }
        None => {
            return Err(ArtworkRefusal::NotAnImage {
                detail: "the bytes do not begin with any image signature this build knows"
                    .to_string(),
            });
        }
    }

    let mut decoder = image::codecs::png::PngDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|_| ArtworkRefusal::DecodeFailed)?;
    // The allocation ceiling is what actually stops a decompression bomb: a few
    // hundred bytes of PNG can describe an image with billions of pixels.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_SOURCE_DECODE_BYTES);
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(ArtworkRefusal::DecodeFailed);
    }
    if width > MAX_SOURCE_DIMENSION || height > MAX_SOURCE_DIMENSION {
        return Err(ArtworkRefusal::DimensionsTooLarge {
            width,
            height,
            maximum: MAX_SOURCE_DIMENSION,
        });
    }
    decoder
        .set_limits(limits)
        .map_err(|_| ArtworkRefusal::DimensionsTooLarge {
            width,
            height,
            maximum: MAX_SOURCE_DIMENSION,
        })?;
    let decoded = image::DynamicImage::from_decoder(decoder)
        .map_err(|_| ArtworkRefusal::DecodeFailed)?
        .into_rgba8();

    let (target_width, target_height) = thumbnail_size(width, height);
    if target_width == width && target_height == height {
        // Already inside the box, so there is nothing to scale - and re-encoding it
        // would only make it bigger. RomM's own small covers arrive at 162x216 in
        // about 55 KB; a naive RGBA8 re-encode of the same pixels came out at 76 KB,
        // so the cheapest and smallest thumbnail is the bytes that already arrived.
        // They are validated PNG and already inside the response ceiling.
        return Ok(RenderedThumbnail {
            png: bytes.to_vec(),
            width,
            height,
        });
    }

    let resized = image::imageops::resize(
        &decoded,
        target_width,
        target_height,
        // Deterministic, cheap, and good enough at this size. Lanczos would cost
        // more CPU per cover for a difference nobody sees at 200 pixels.
        image::imageops::FilterType::Triangle,
    );

    // Covers are overwhelmingly opaque, and carrying a pointless alpha channel adds
    // a quarter to every one of them.
    let opaque = resized.pixels().all(|pixel| pixel.0[3] == 255);
    let mut png = Vec::new();
    if opaque {
        let rgb: Vec<u8> = resized
            .pixels()
            .flat_map(|pixel| [pixel.0[0], pixel.0[1], pixel.0[2]])
            .collect();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                &rgb,
                target_width,
                target_height,
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|_| ArtworkRefusal::DecodeFailed)?;
    } else {
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                resized.as_raw(),
                target_width,
                target_height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|_| ArtworkRefusal::DecodeFailed)?;
    }
    Ok(RenderedThumbnail {
        png,
        width: target_width,
        height: target_height,
    })
}

/// The size a cover becomes: scaled to fit the box, aspect ratio preserved, never
/// enlarged.
///
/// Never enlarging matters - RomM's small covers are already 162x216, and
/// upscaling them to fill a 200x280 box would soften every one of them for nothing.
pub fn thumbnail_size(width: u32, height: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width, height);
    }
    if width <= THUMBNAIL_MAX_WIDTH && height <= THUMBNAIL_MAX_HEIGHT {
        return (width, height);
    }
    let by_width = f64::from(THUMBNAIL_MAX_WIDTH) / f64::from(width);
    let by_height = f64::from(THUMBNAIL_MAX_HEIGHT) / f64::from(height);
    let scale = by_width.min(by_height);
    let scaled_width = ((f64::from(width) * scale).round() as u32).max(1);
    let scaled_height = ((f64::from(height) * scale).round() as u32).max(1);
    (
        scaled_width.min(THUMBNAIL_MAX_WIDTH),
        scaled_height.min(THUMBNAIL_MAX_HEIGHT),
    )
}

/// Every entry in the index, for a diagnostic view.
pub fn index_entries(cache: &ArtworkCache, server_id: &str) -> Vec<ArtworkEntry> {
    let mut entries = cache.load_index(server_id).entries;
    entries.sort_by(|left, right| left.key.cmp(&right.key));
    entries
}

/// Removes leftover temporary files from an interrupted write.
pub fn clean_temporary_files(cache: &ArtworkCache) -> usize {
    let mut removed = 0;
    for directory in [
        cache.directory().to_path_buf(),
        cache.thumbnails_directory(),
    ] {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("tmp")
                && fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests;

/// Fixtures shared with the crate's other test modules.
#[cfg(test)]
pub(crate) mod fixtures {
    /// A tiny synthetic PNG of `width` x `height` solid pixels.
    ///
    /// Generated, never a real cover: the tests must not carry copyrighted artwork,
    /// and a generated image is also the only way to test an exact size.
    pub fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder as _;
        let mut image = image::RgbaImage::new(width, height);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(
                image.as_raw(),
                width,
                height,
                image::ExtendedColorType::Rgba8,
            )
            .expect("encoding a synthetic PNG cannot fail");
        png
    }
}
