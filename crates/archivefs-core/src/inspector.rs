//! Archive Inspector: read-only listing of one ZIP archive's internal
//! entries - never extracts, never writes, never mutates the archive or
//! the filesystem. See `inspect_archive`, the single production entry
//! point.
//!
//! Scope for this first milestone: ZIP only (see `is_inspectable`).
//! Adding 7z/RAR support later belongs in this same module, behind the
//! same `InspectorReport`/`InspectorError` surface, not a parallel one.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use zip::ZipArchive;

use crate::{ArchiveKind, archive_kind};

/// A safety cap, not a design constraint: without it, a hostile or simply
/// enormous ZIP (some real-world ROM-collection archives run into the
/// hundreds of thousands of entries) could make one inspection consume
/// unbounded memory. Reaching it stops entry collection cleanly - see
/// `InspectorReport::truncated` - it is never silently exceeded.
pub const INSPECTOR_ENTRY_LIMIT: usize = 100_000;

/// Whether a directory or a regular file entry - `ZipFile::is_dir`
/// already excludes ambiguity (a trailing `/` in the stored name), so
/// this is a direct, truthful copy of that, not a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum InspectorEntryKind {
    File,
    Directory,
}

/// A first-pass, deliberately simple classification of one entry - never
/// a claim of definitive game/ROM detection. Every rule here is a plain
/// extension match against a small, fixed, documented list; nothing here
/// inspects file contents, magic bytes, or archive structure beyond the
/// entry's own stored name. See `classify_entry`.
///
/// `PartialOrd`/`Ord` follow declaration order below - a stable, sensible
/// grouping order for the GUI's "sort by classification" (see
/// `InspectorSortField::Classification`), not a claim that one kind of
/// entry matters more than another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum InspectorEntryClassification {
    /// A plausible ROM/disc-image/executable extension - "likely content",
    /// deliberately not "game file": this is an extension guess, nothing
    /// more. A `.md` file is never classified here even though it could
    /// coincidentally be a Sega Mega Drive dump, because `.md` is also a
    /// common Markdown extension and `Documentation` is checked first -
    /// see `classify_entry`'s doc comment for the full precedence order.
    LikelyContent,
    Artwork,
    Documentation,
    NestedArchive,
    Directory,
    Other,
}

impl InspectorEntryClassification {
    pub fn label(self) -> &'static str {
        match self {
            Self::LikelyContent => "Likely content",
            Self::Artwork => "Artwork",
            Self::Documentation => "Documentation / metadata",
            Self::NestedArchive => "Nested archive",
            Self::Directory => "Directory",
            Self::Other => "Other",
        }
    }

    /// Every variant, in a stable display order - used to build the
    /// Inspector's summary counts and classification filter without
    /// either side of the GUI needing its own separate copy of this list.
    pub const ALL: [Self; 6] = [
        Self::LikelyContent,
        Self::Artwork,
        Self::Documentation,
        Self::NestedArchive,
        Self::Directory,
        Self::Other,
    ];
}

/// Artwork extensions - exactly the milestone's fixed list, checked
/// before `LIKELY_CONTENT_EXTENSIONS` (see `classify_entry`).
const ARTWORK_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];

/// Documentation/metadata extensions - exactly the milestone's fixed
/// list. Checked *first* of every extension-based rule: `cue`/`m3u` are
/// disc-image companion files that could otherwise look like "content",
/// and `md` collides with the Sega Mega Drive ROM extension - the
/// milestone's own list settles both in favour of Documentation.
const DOCUMENTATION_EXTENSIONS: &[&str] = &[
    "nfo", "txt", "md", "pdf", "xml", "json", "cue", "m3u", "sha1", "md5", "sfv",
];

/// Nested-archive extensions - exactly the milestone's fixed list.
const NESTED_ARCHIVE_EXTENSIONS: &[&str] = &["zip", "7z", "rar", "tar", "gz", "bz2", "xz"];

/// "Common ROM, disc-image and executable extensions" - the milestone's
/// wording implies such a list already exists somewhere in ArchiveFS.
/// It does not: platform detection (`FOLDER_PLATFORM_ALIASES` and
/// `detect_platform_from_known_heuristics`) is entirely folder-name
/// based and has never matched on file extensions. This list is new,
/// created specifically for the Inspector's first-pass classification,
/// and deliberately modest - a false negative here only falls through to
/// `Other`, never a wrong or overconfident claim. Extensions already
/// claimed by `DOCUMENTATION_EXTENSIONS`/`ARTWORK_EXTENSIONS`/
/// `NESTED_ARCHIVE_EXTENSIONS` are intentionally left out (`classify_entry`
/// checks those lists first regardless, so listing them here too would
/// only ever be dead weight).
const LIKELY_CONTENT_EXTENSIONS: &[&str] = &[
    // Cartridge/ROM dump extensions.
    "nes", "fds", "sfc", "smc", "gb", "gbc", "gba", "n64", "z64", "v64", "nds", "3ds", "cia", "gg",
    "sms", "gen", "32x", "pce", "ws", "wsc", "ngp", "ngc", "a26", "a52", "a78", "j64", "col",
    "int", "vec", "lnx", "adf", "ipf", "dsk", "d64", "t64", "tap", "crt", "rom",
    // Disc-image extensions.
    "iso", "bin", "img", "mdf", "mds", "nrg", "cso", "gcz", "wbfs", "wad", "rvz", "chd", "gdi",
    "cdi", "pbp", "ecm", // Executable/installer extensions.
    "exe", "msi", "bat", "sh", "app", "com",
];

/// Classifies one entry from its stored name and directory-ness alone -
/// pure, deterministic, and never inspects file content. Precedence
/// (most to least specific): `Directory` first (unambiguous - nothing
/// else applies to a directory entry), then `Documentation`, `Artwork`,
/// `NestedArchive`, `LikelyContent`, in that order, then `Other`. See
/// `DOCUMENTATION_EXTENSIONS`'s doc comment for why Documentation is
/// checked before the others.
pub fn classify_entry(name: &str, is_directory: bool) -> InspectorEntryClassification {
    if is_directory {
        return InspectorEntryClassification::Directory;
    }
    let Some(extension) = Path::new(name).extension() else {
        return InspectorEntryClassification::Other;
    };
    let extension = extension.to_string_lossy().to_lowercase();
    let extension = extension.as_str();

    if DOCUMENTATION_EXTENSIONS.contains(&extension) {
        InspectorEntryClassification::Documentation
    } else if ARTWORK_EXTENSIONS.contains(&extension) {
        InspectorEntryClassification::Artwork
    } else if NESTED_ARCHIVE_EXTENSIONS.contains(&extension) {
        InspectorEntryClassification::NestedArchive
    } else if LIKELY_CONTENT_EXTENSIONS.contains(&extension) {
        InspectorEntryClassification::LikelyContent
    } else {
        InspectorEntryClassification::Other
    }
}

/// One archive entry's read-only metadata - never the entry's own data,
/// which this module never reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectorEntry {
    /// The stored entry name exactly as `zip::read::ZipFile::name`
    /// returns it: converted using the ZIP's own general-purpose UTF-8
    /// flag when set, or a safe (never-panicking) CP437 fallback when it
    /// is not - see that method's own documented contract. This is the
    /// closest a `String` can get to "the exact underlying entry name"
    /// while still being safe to render directly; nothing here further
    /// mangles or path-sanitizes it (this module never uses the name to
    /// touch the filesystem).
    pub name: String,
    pub kind: InspectorEntryKind,
    pub uncompressed_size: u64,
    pub compressed_size: Option<u64>,
    pub compression_method: Option<String>,
    pub classification: InspectorEntryClassification,
}

/// The result of one `inspect_archive` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectorReport {
    pub entries: Vec<InspectorEntry>,
    /// `true` iff the archive has more entries than `entries` lists -
    /// `INSPECTOR_ENTRY_LIMIT` (or the caller's own limit, in
    /// `inspect_archive_with_limit`) was reached. Never silently
    /// incomplete: a caller that ignores this field simply has an
    /// accurate list of exactly `entries.len()` items either way.
    pub truncated: bool,
    /// The archive's real total entry count, independent of `truncated`
    /// or how many entries `entries` actually holds - read directly from
    /// the ZIP central directory (`ZipArchive::len`), never approximated.
    pub total_entries_in_archive: usize,
}

/// Every way a read-only inspection can truthfully fail - deliberately a
/// separate type from `ArchiveFsError` (the crate's much larger,
/// widely-matched general error type used by scanning/mounting/database
/// code) so adding Inspector-specific cases here can never touch that
/// unrelated, exhaustively-matched surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorError {
    /// The path does not currently exist or is not a regular file -
    /// checked live via `File::open` at inspection time, regardless of
    /// what any cached catalogue row believed. This is what stops a
    /// stale cached row from ever being treated as authorization to read
    /// a file that is no longer there.
    ArchiveMissing(PathBuf),
    /// Not a format this milestone can inspect (only ZIP is supported -
    /// see `is_inspectable`).
    UnsupportedFormat(PathBuf),
    /// At least one entry is encrypted. Rejected outright, before any
    /// entry is added to the report: this module never asks for or
    /// tries a password, and a partially-listed encrypted archive would
    /// be more confusing than a clear refusal.
    Encrypted(PathBuf),
    /// The file exists and has a `.zip` name but its structure could not
    /// be parsed as a valid ZIP archive (corrupt, truncated, or not
    /// actually a ZIP file despite its extension).
    Malformed { path: PathBuf, detail: String },
    /// A filesystem error other than "missing" - permission denied,
    /// I/O error, and the like.
    Io { path: PathBuf, detail: String },
}

impl std::fmt::Display for InspectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ArchiveMissing(path) => {
                write!(f, "{}: archive not found", path.display())
            }
            Self::UnsupportedFormat(path) => {
                write!(
                    f,
                    "{}: Archive Inspector only supports ZIP archives in this version",
                    path.display()
                )
            }
            Self::Encrypted(path) => {
                write!(
                    f,
                    "{}: this archive contains encrypted entries and cannot be inspected read-only",
                    path.display()
                )
            }
            Self::Malformed { path, detail } => {
                write!(
                    f,
                    "{}: not a readable ZIP archive: {detail}",
                    path.display()
                )
            }
            Self::Io { path, detail } => write!(f, "{}: {detail}", path.display()),
        }
    }
}

impl std::error::Error for InspectorError {}

/// Whether `path` is a format the Archive Inspector currently supports -
/// ZIP only. Pure and filename-only (matches `archive_kind`'s own
/// contract): never touches the filesystem, so it is safe to call just
/// to decide whether to show/enable an "Inspect contents" control.
pub fn is_inspectable(path: &Path) -> bool {
    matches!(archive_kind(path), Some(ArchiveKind::Zip))
}

/// Lists `path`'s entries read-only, up to `INSPECTOR_ENTRY_LIMIT`. See
/// `inspect_archive_with_limit` for the full contract; this is simply
/// that function with the production default limit.
pub fn inspect_archive(path: &Path) -> Result<InspectorReport, InspectorError> {
    inspect_archive_with_limit(path, INSPECTOR_ENTRY_LIMIT)
}

/// Lists `path`'s entries read-only. Never extracts entry data, never
/// writes to the archive or the filesystem, and never requires or tries
/// a password. `limit` exists as its own parameter (rather than always
/// using `INSPECTOR_ENTRY_LIMIT`) purely so tests can exercise the
/// truncation behaviour without constructing a 100,000-entry fixture.
///
/// Every failure mode maps to a specific `InspectorError` variant - see
/// its own doc comments - rather than one generic message, so a caller
/// (the GUI) can show a truthful, specific explanation instead of a bare
/// "something went wrong".
pub fn inspect_archive_with_limit(
    path: &Path,
    limit: usize,
) -> Result<InspectorReport, InspectorError> {
    if !is_inspectable(path) {
        return Err(InspectorError::UnsupportedFormat(path.to_path_buf()));
    }

    let file = File::open(path).map_err(|source| classify_open_error(path, source))?;
    // `ZipArchive::new` reads and parses the whole central directory up
    // front - every entry's metadata (name, sizes, compression method,
    // encryption flag) is already known after this call succeeds, before
    // any per-entry access.
    let mut archive = ZipArchive::new(file).map_err(|error| InspectorError::Malformed {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;

    let total_entries_in_archive = archive.len();
    let mut entries = Vec::with_capacity(total_entries_in_archive.min(limit));
    let mut truncated = false;

    for index in 0..total_entries_in_archive {
        if entries.len() >= limit {
            truncated = true;
            break;
        }
        // `by_index_raw` reads this entry's metadata without preparing a
        // decompressor for it - the only reason this module never needs
        // to support (or reject for being unsupported) any particular
        // compression codec: it never decodes entry data at all.
        let raw = archive
            .by_index_raw(index)
            .map_err(|error| InspectorError::Malformed {
                path: path.to_path_buf(),
                detail: error.to_string(),
            })?;
        if raw.encrypted() {
            return Err(InspectorError::Encrypted(path.to_path_buf()));
        }

        let name = raw.name().to_string();
        let is_directory = raw.is_dir();
        let classification = classify_entry(&name, is_directory);
        entries.push(InspectorEntry {
            name,
            kind: if is_directory {
                InspectorEntryKind::Directory
            } else {
                InspectorEntryKind::File
            },
            uncompressed_size: raw.size(),
            compressed_size: Some(raw.compressed_size()),
            compression_method: Some(raw.compression().to_string()),
            classification,
        });
    }

    Ok(InspectorReport {
        entries,
        truncated,
        total_entries_in_archive,
    })
}

fn classify_open_error(path: &Path, source: io::Error) -> InspectorError {
    if source.kind() == io::ErrorKind::NotFound {
        InspectorError::ArchiveMissing(path.to_path_buf())
    } else {
        InspectorError::Io {
            path: path.to_path_buf(),
            detail: source.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};
    use zip::write::SimpleFileOptions;
    use zip::{AesMode, CompressionMethod, ZipWriter};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "archivefs-core-inspector-test-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// One entry to write into a test fixture ZIP.
    struct FixtureEntry {
        name: &'static str,
        content: &'static [u8],
        method: CompressionMethod,
    }

    fn write_test_zip(dir: &Path, file_name: &str, entries: &[FixtureEntry]) -> PathBuf {
        let path = dir.join(file_name);
        let file = fs::File::create(&path).unwrap();
        let mut writer = ZipWriter::new(file);
        for entry in entries {
            let options = SimpleFileOptions::default().compression_method(entry.method);
            writer.start_file(entry.name, options).unwrap();
            writer.write_all(entry.content).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    fn write_test_zip_with_directory(dir: &Path, file_name: &str, dir_name: &str) -> PathBuf {
        let path = dir.join(file_name);
        let file = fs::File::create(&path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .add_directory(dir_name, SimpleFileOptions::default())
            .unwrap();
        writer
            .start_file(
                format!("{dir_name}inner.txt"),
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"inside").unwrap();
        writer.finish().unwrap();
        path
    }

    fn write_encrypted_test_zip(dir: &Path, file_name: &str) -> PathBuf {
        let path = dir.join(file_name);
        let file = fs::File::create(&path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .with_aes_encryption(AesMode::Aes256, "correct horse battery staple");
        writer.start_file("secret.txt", options).unwrap();
        writer.write_all(b"top secret").unwrap();
        writer.finish().unwrap();
        path
    }

    /// Hand-builds the smallest possible valid ZIP - one zero-byte STORED
    /// entry - with `name_bytes` as its stored name and the UTF-8
    /// general-purpose flag bit (bit 11) deliberately left *clear*, so
    /// the `zip` crate must decode `name_bytes` via its CP437 fallback
    /// rather than as UTF-8. CP437 maps every byte 0-255 to some
    /// character, so this is valid input regardless of whether
    /// `name_bytes` would be valid UTF-8 - exactly the scenario
    /// `ZipFile::name`'s "never panics" contract exists for. Built by
    /// hand (not via `ZipWriter`, whose `start_file` only accepts `&str`
    /// and so can never produce a non-UTF-8 stored name in the first
    /// place) - zero-byte content sidesteps needing a real CRC-32
    /// implementation (CRC-32 of no bytes is 0).
    fn build_minimal_zip_with_raw_name(name_bytes: &[u8]) -> Vec<u8> {
        let name_len = name_bytes.len() as u16;
        let mut out = Vec::new();

        let local_header_start = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // general purpose flag: UTF-8 bit clear
        out.extend_from_slice(&0u16.to_le_bytes()); // compression: Stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x0021u16.to_le_bytes()); // mod date: 1980-01-01
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32 (0 bytes of content)
        out.extend_from_slice(&0u32.to_le_bytes()); // compressed size
        out.extend_from_slice(&0u32.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        out.extend_from_slice(name_bytes);
        let local_header_len = out.len() as u32 - local_header_start;

        let central_dir_start = out.len() as u32;
        out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // general purpose flag
        out.extend_from_slice(&0u16.to_le_bytes()); // compression: Stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x0021u16.to_le_bytes()); // mod date
        out.extend_from_slice(&0u32.to_le_bytes()); // crc32
        out.extend_from_slice(&0u32.to_le_bytes()); // compressed size
        out.extend_from_slice(&0u32.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        out.extend_from_slice(&0u16.to_le_bytes()); // file comment length
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal file attributes
        out.extend_from_slice(&0u32.to_le_bytes()); // external file attributes
        out.extend_from_slice(&local_header_start.to_le_bytes());
        out.extend_from_slice(name_bytes);
        let central_dir_len = out.len() as u32 - central_dir_start;

        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // this disk
        out.extend_from_slice(&0u16.to_le_bytes()); // central dir start disk
        out.extend_from_slice(&1u16.to_le_bytes()); // records on this disk
        out.extend_from_slice(&1u16.to_le_bytes()); // total records
        out.extend_from_slice(&central_dir_len.to_le_bytes());
        out.extend_from_slice(&local_header_len.to_le_bytes()); // offset of central dir
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length

        out
    }

    #[test]
    fn lists_entries_without_extracting_them() {
        let dir = temp_dir("list-basic");
        let path = write_test_zip(
            &dir,
            "roms.zip",
            &[
                FixtureEntry {
                    name: "a.txt",
                    content: b"hello world",
                    method: CompressionMethod::Stored,
                },
                FixtureEntry {
                    name: "b.txt",
                    content: b"second entry",
                    method: CompressionMethod::Stored,
                },
            ],
        );

        let report = inspect_archive(&path).unwrap();

        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.total_entries_in_archive, 2);
        assert!(!report.truncated);
        let names: Vec<&str> = report.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.txt"]);
        // Nothing was ever extracted - no sibling files exist beyond the
        // ZIP itself.
        let siblings: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(
            siblings.len(),
            1,
            "inspecting must never extract entries onto disk"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directories_and_files_are_distinguished() {
        let dir = temp_dir("dir-vs-file");
        let path = write_test_zip_with_directory(&dir, "with-dir.zip", "folder/");

        let report = inspect_archive(&path).unwrap();

        let folder = report.entries.iter().find(|e| e.name == "folder/").unwrap();
        assert_eq!(folder.kind, InspectorEntryKind::Directory);
        assert_eq!(
            folder.classification,
            InspectorEntryClassification::Directory
        );

        let inner = report
            .entries
            .iter()
            .find(|e| e.name == "folder/inner.txt")
            .unwrap();
        assert_eq!(inner.kind, InspectorEntryKind::File);
        assert_ne!(
            inner.classification,
            InspectorEntryClassification::Directory
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_and_compression_metadata_are_truthful() {
        let dir = temp_dir("sizes");
        let content = b"a payload long enough to actually compress: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let path = write_test_zip(
            &dir,
            "sizes.zip",
            &[
                FixtureEntry {
                    name: "stored.bin",
                    content,
                    method: CompressionMethod::Stored,
                },
                FixtureEntry {
                    name: "deflated.bin",
                    content,
                    method: CompressionMethod::Deflated,
                },
            ],
        );

        let report = inspect_archive(&path).unwrap();

        let stored = report
            .entries
            .iter()
            .find(|e| e.name == "stored.bin")
            .unwrap();
        assert_eq!(stored.uncompressed_size, content.len() as u64);
        assert_eq!(stored.compressed_size, Some(content.len() as u64));
        assert_eq!(
            stored.compression_method,
            Some(CompressionMethod::Stored.to_string())
        );

        let deflated = report
            .entries
            .iter()
            .find(|e| e.name == "deflated.bin")
            .unwrap();
        assert_eq!(deflated.uncompressed_size, content.len() as u64);
        assert!(
            deflated.compressed_size.unwrap() < content.len() as u64,
            "a real repeated-byte payload must actually shrink under Deflate"
        );
        assert_eq!(
            deflated.compression_method,
            Some(CompressionMethod::Deflated.to_string())
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_classification_bucket_is_reachable() {
        assert_eq!(
            classify_entry("game.nes", false),
            InspectorEntryClassification::LikelyContent
        );
        assert_eq!(
            classify_entry("disc.iso", false),
            InspectorEntryClassification::LikelyContent
        );
        assert_eq!(
            classify_entry("setup.exe", false),
            InspectorEntryClassification::LikelyContent
        );
        assert_eq!(
            classify_entry("cover.png", false),
            InspectorEntryClassification::Artwork
        );
        assert_eq!(
            classify_entry("readme.nfo", false),
            InspectorEntryClassification::Documentation
        );
        assert_eq!(
            classify_entry("game.cue", false),
            InspectorEntryClassification::Documentation
        );
        assert_eq!(
            classify_entry("bundled.zip", false),
            InspectorEntryClassification::NestedArchive
        );
        assert_eq!(
            classify_entry("folder/", true),
            InspectorEntryClassification::Directory
        );
        assert_eq!(
            classify_entry("mystery.xyz123", false),
            InspectorEntryClassification::Other
        );
        assert_eq!(
            classify_entry("no_extension_at_all", false),
            InspectorEntryClassification::Other
        );
        // The Mega Drive ROM / Markdown collision - Documentation must
        // win, per `DOCUMENTATION_EXTENSIONS`'s own doc comment.
        assert_eq!(
            classify_entry("notes.md", false),
            InspectorEntryClassification::Documentation
        );
    }

    #[test]
    fn nested_archive_extensions_all_classify_correctly() {
        for extension in ["zip", "7z", "rar", "tar", "gz", "bz2", "xz"] {
            assert_eq!(
                classify_entry(&format!("inner.{extension}"), false),
                InspectorEntryClassification::NestedArchive,
                "extension {extension} must classify as a nested archive"
            );
        }
    }

    #[test]
    fn malformed_zip_produces_a_clear_error() {
        let dir = temp_dir("malformed");
        let path = dir.join("broken.zip");
        fs::write(&path, b"this is not a zip file at all, just plain text").unwrap();

        let error = inspect_archive(&path).unwrap_err();
        assert!(matches!(error, InspectorError::Malformed { .. }));
        assert!(error.to_string().contains("not a readable ZIP archive"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_archive_is_rejected() {
        let dir = temp_dir("missing");
        let path = dir.join("does-not-exist.zip");

        let error = inspect_archive(&path).unwrap_err();
        assert_eq!(error, InspectorError::ArchiveMissing(path.clone()));
        assert!(error.to_string().contains("not found"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inspection_performs_no_filesystem_writes() {
        let dir = temp_dir("no-writes");
        let path = write_test_zip(
            &dir,
            "readonly.zip",
            &[FixtureEntry {
                name: "a.txt",
                content: b"content",
                method: CompressionMethod::Stored,
            }],
        );
        let before_bytes = fs::read(&path).unwrap();
        let before_entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();

        let _ = inspect_archive(&path).unwrap();

        let after_bytes = fs::read(&path).unwrap();
        let after_entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            before_bytes, after_bytes,
            "the archive's own bytes must never change"
        );
        assert_eq!(
            before_entries, after_entries,
            "inspecting must never create, remove, or rename any file"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_limit_truncates_cleanly() {
        let dir = temp_dir("limit");
        let entries: Vec<FixtureEntry> = (0..5)
            .map(|index| FixtureEntry {
                name: match index {
                    0 => "a.txt",
                    1 => "b.txt",
                    2 => "c.txt",
                    3 => "d.txt",
                    _ => "e.txt",
                },
                content: b"x",
                method: CompressionMethod::Stored,
            })
            .collect();
        let path = write_test_zip(&dir, "many.zip", &entries);

        let report = inspect_archive_with_limit(&path, 3).unwrap();

        assert_eq!(report.entries.len(), 3);
        assert!(report.truncated);
        assert_eq!(report.total_entries_in_archive, 5);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_utf8_entry_name_does_not_panic() {
        // 0x80 alone is never valid UTF-8 (it is a continuation byte with
        // no lead byte), but is a perfectly ordinary single CP437
        // character - exactly the case `ZipFile::name`'s CP437 fallback
        // exists for.
        let bytes = build_minimal_zip_with_raw_name(&[0x80, b'.', b't', b'x', b't']);
        let dir = temp_dir("non-utf8-name");
        let path = dir.join("raw-name.zip");
        fs::write(&path, &bytes).unwrap();

        let report = inspect_archive(&path).unwrap();

        assert_eq!(report.entries.len(), 1);
        assert!(
            !report.entries[0].name.is_empty(),
            "a CP437-decoded name must still be a real, non-empty string"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn encrypted_archive_is_rejected() {
        let dir = temp_dir("encrypted");
        let path = write_encrypted_test_zip(&dir, "locked.zip");

        let error = inspect_archive(&path).unwrap_err();
        assert_eq!(error, InspectorError::Encrypted(path.clone()));
        assert!(error.to_string().contains("encrypted"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_inspectable_accepts_only_zip() {
        assert!(is_inspectable(Path::new("/roms/game.zip")));
        assert!(!is_inspectable(Path::new("/roms/game.7z")));
        assert!(!is_inspectable(Path::new("/roms/game.rar")));
        assert!(!is_inspectable(Path::new("/roms/game.txt")));
    }
}
