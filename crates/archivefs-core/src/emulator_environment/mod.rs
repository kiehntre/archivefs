//! Read-only emulator environment discovery.
//!
//! This is a sibling to [`crate::patch_manager`], not part of it.
//! `patch_manager` matches patch metadata to catalogue games and derives
//! hypothetical patch destinations; there is no "game" or "patch" concept
//! here. This module only discovers and reports what already exists on
//! disk for a given emulator - installation profiles, configured paths,
//! and installed cores - never matching, planning, or mutating anything.
//! See `docs/RETROARCH_ENVIRONMENT.md` for the full design record.
//!
//! Nothing here is imported by, or imports from, `patch_manager`. The
//! read-only filesystem abstraction below intentionally duplicates the
//! *pattern* already proven by `patch_manager::pcsx2`'s
//! `ReadOnlyFilesystem` (probe without following the final path
//! component) rather than sharing code with it - only a second
//! environment-discovery target would justify extracting a shared trait.
//!
//! Only one emulator (RetroArch, in [`retroarch`]) is implemented. No
//! generic `EmulatorEnvironmentAdapter` trait exists yet - see the module
//! doc comment on `patch_manager::adapter` for why guessing a second
//! implementation's shape ahead of time is exactly the mistake this
//! project avoids repeating.

pub mod retroarch;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use serde::Serialize;

/// A filesystem path rendered for stable JSON output. Unlike deriving
/// `Serialize` directly on `PathBuf` (which several existing types in
/// this crate do, e.g. `ArchiveStatus::archive_path` and
/// `patch_manager::adapter::InstallationCandidate::data_root`), this
/// never fails to serialize: a non-UTF-8 path renders as its lossy
/// (replacement-character) form with `lossy: true`, rather than
/// producing a `serde_json` error. No byte-preserving path JSON format
/// already existed in this codebase to reuse; this is a new, minimal one
/// introduced for this module. Ordering/equality for internal sorting
/// purposes is done on the original `PathBuf`/`OsString` before
/// conversion to this type, not on `EncodedPath` itself - see
/// `retroarch::assemble_report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EncodedPath {
    /// Lossy UTF-8 rendering of the path, always present and always
    /// valid JSON text.
    pub display: String,
    /// `true` if `display` is a lossy, non-reversible rendering (the
    /// real path contains bytes that are not valid UTF-8).
    pub lossy: bool,
}

impl EncodedPath {
    pub fn from_path(path: &Path) -> Self {
        match path.to_str() {
            Some(text) => Self {
                display: text.to_string(),
                lossy: false,
            },
            None => Self {
                display: path.to_string_lossy().into_owned(),
                lossy: true,
            },
        }
    }

    pub fn from_os_string(value: &std::ffi::OsStr) -> Self {
        Self::from_path(Path::new(value))
    }
}

/// Classification of one path's final component, without following it if
/// it is itself a symlink. Produced by `symlink_metadata`, never `metadata`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FsProbe {
    PresentFile,
    PresentDirectory,
    Missing,
    /// The final path component is itself a symlink. Never followed by
    /// this module for configured paths, core files, or `.info` files -
    /// see the module-level symlink policy note on
    /// [`ReadOnlyHostFilesystem`].
    Symlink,
    /// Exists, is not a symlink, but is neither a regular file nor a
    /// directory (FIFO, socket, device node, ...).
    WrongType,
    Inaccessible,
    IoError,
}

/// Classification used only for native executable discovery via `PATH`,
/// which conventionally *does* follow symlinks (many real `retroarch`
/// binaries on disk are symlinks, e.g. via `update-alternatives`). This
/// is a deliberate, narrowly-scoped exception to the no-follow policy
/// used everywhere else in this module - see
/// `retroarch::discover_native_executables`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutableProbe {
    RegularExecutable,
    NotExecutable,
    Missing,
    WrongType,
    Inaccessible,
    IoError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    pub file_name: OsString,
    pub probe: FsProbe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedReadResult {
    Ok(Vec<u8>),
    NotFound,
    WrongType,
    Symlink,
    Inaccessible,
    IoError,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedListResult {
    Ok(Vec<DirEntryInfo>),
    NotFound,
    WrongType,
    Symlink,
    Inaccessible,
    IoError,
    TooLarge,
}

/// Read-only filesystem capability this module needs. No write, create,
/// delete, rename, or execute method exists on this trait, and no
/// implementation of it may add one.
///
/// Symlink policy: [`Self::probe`], [`Self::read_bounded`], and
/// [`Self::list_dir_bounded`] never follow a symlink at the *final*
/// path component (`symlink_metadata`, not `metadata`); a bounded read
/// or listing whose target is itself a symlink returns `Symlink`/
/// `BoundedReadResult::Symlink` without opening it. This module does not
/// use `openat2`/`RESOLVE_NO_SYMLINKS` or any other race-resistant
/// primitive, so there is an inherent, unavoidable gap between a probe
/// and a subsequent read on POSIX filesystems (the same limitation
/// already documented for `patch_manager`'s retrieval/inspection code).
/// The practical exposure is small: content read through this trait is
/// bounded, never executed, and never trusted beyond parsing a handful
/// of known text fields. Ancestor directories are *not* specially
/// guarded against symlinks; the operating system resolves them
/// normally, which is the same behavior every other filesystem-reading
/// command in this codebase already has.
pub trait ReadOnlyHostFilesystem {
    fn probe(&self, path: &Path) -> FsProbe;
    /// Follows symlinks (`metadata`, not `symlink_metadata`) - used only
    /// for native executable discovery via `PATH`. See
    /// [`ExecutableProbe`].
    fn probe_executable(&self, path: &Path) -> ExecutableProbe;
    /// Reads at most `max_bytes` bytes. `BoundedReadResult::TooLarge` if
    /// the file is larger; the oversized content is never partially
    /// trusted as a truncated prefix.
    fn read_bounded(&self, path: &Path, max_bytes: usize) -> BoundedReadResult;
    /// Non-recursive listing bounded to `max_entries`.
    /// `BoundedListResult::TooLarge` if the directory has more entries;
    /// no partial listing is returned in that case.
    fn list_dir_bounded(&self, path: &Path, max_entries: usize) -> BoundedListResult;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HostReadOnlyFilesystem;

impl HostReadOnlyFilesystem {
    fn probe_metadata(metadata: io::Result<fs::Metadata>) -> FsProbe {
        match metadata {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                if file_type.is_symlink() {
                    FsProbe::Symlink
                } else if file_type.is_dir() {
                    FsProbe::PresentDirectory
                } else if file_type.is_file() {
                    FsProbe::PresentFile
                } else {
                    FsProbe::WrongType
                }
            }
            Err(error) => io_error_to_probe(&error),
        }
    }
}

fn io_error_to_probe(error: &io::Error) -> FsProbe {
    match error.kind() {
        io::ErrorKind::NotFound => FsProbe::Missing,
        io::ErrorKind::PermissionDenied => FsProbe::Inaccessible,
        _ => FsProbe::IoError,
    }
}

impl ReadOnlyHostFilesystem for HostReadOnlyFilesystem {
    fn probe(&self, path: &Path) -> FsProbe {
        Self::probe_metadata(fs::symlink_metadata(path))
    }

    fn probe_executable(&self, path: &Path) -> ExecutableProbe {
        use std::os::unix::fs::PermissionsExt;

        match fs::metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return ExecutableProbe::WrongType;
                }
                if metadata.permissions().mode() & 0o111 != 0 {
                    ExecutableProbe::RegularExecutable
                } else {
                    ExecutableProbe::NotExecutable
                }
            }
            Err(error) => match error.kind() {
                io::ErrorKind::NotFound => ExecutableProbe::Missing,
                io::ErrorKind::PermissionDenied => ExecutableProbe::Inaccessible,
                _ => ExecutableProbe::IoError,
            },
        }
    }

    fn read_bounded(&self, path: &Path, max_bytes: usize) -> BoundedReadResult {
        match self.probe(path) {
            FsProbe::Missing => return BoundedReadResult::NotFound,
            FsProbe::Symlink => return BoundedReadResult::Symlink,
            FsProbe::PresentDirectory | FsProbe::WrongType => return BoundedReadResult::WrongType,
            FsProbe::Inaccessible => return BoundedReadResult::Inaccessible,
            FsProbe::IoError => return BoundedReadResult::IoError,
            FsProbe::PresentFile => {}
        }
        use std::io::Read;
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) => return bounded_read_error(&error),
        };
        let mut buffer = Vec::new();
        match file.take(max_bytes as u64 + 1).read_to_end(&mut buffer) {
            Ok(_) => {
                if buffer.len() > max_bytes {
                    BoundedReadResult::TooLarge
                } else {
                    BoundedReadResult::Ok(buffer)
                }
            }
            Err(error) => bounded_read_error(&error),
        }
    }

    fn list_dir_bounded(&self, path: &Path, max_entries: usize) -> BoundedListResult {
        match self.probe(path) {
            FsProbe::Missing => return BoundedListResult::NotFound,
            FsProbe::Symlink => return BoundedListResult::Symlink,
            FsProbe::PresentFile | FsProbe::WrongType => return BoundedListResult::WrongType,
            FsProbe::Inaccessible => return BoundedListResult::Inaccessible,
            FsProbe::IoError => return BoundedListResult::IoError,
            FsProbe::PresentDirectory => {}
        }
        let read_dir = match fs::read_dir(path) {
            Ok(read_dir) => read_dir,
            Err(error) => return bounded_list_error(&error),
        };
        let mut entries = Vec::new();
        for entry in read_dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => return bounded_list_error(&error),
            };
            if entries.len() >= max_entries {
                return BoundedListResult::TooLarge;
            }
            let entry_probe = Self::probe_metadata(
                entry
                    .metadata()
                    .or_else(|_| fs::symlink_metadata(entry.path())),
            );
            entries.push(DirEntryInfo {
                file_name: entry.file_name(),
                probe: entry_probe,
            });
        }
        BoundedListResult::Ok(entries)
    }
}

fn bounded_read_error(error: &io::Error) -> BoundedReadResult {
    match error.kind() {
        io::ErrorKind::NotFound => BoundedReadResult::NotFound,
        io::ErrorKind::PermissionDenied => BoundedReadResult::Inaccessible,
        _ => BoundedReadResult::IoError,
    }
}

fn bounded_list_error(error: &io::Error) -> BoundedListResult {
    match error.kind() {
        io::ErrorKind::NotFound => BoundedListResult::NotFound,
        io::ErrorKind::PermissionDenied => BoundedListResult::Inaccessible,
        _ => BoundedListResult::IoError,
    }
}

/// Raw-byte comparison matching `PathBuf`/`OsStr`'s own `Ord` on Unix
/// (component-wise byte comparison) - used as the single sorting
/// primitive throughout `retroarch` so "sort by raw filename/path bytes"
/// never needs a bespoke comparator.
pub(crate) fn os_str_bytes(value: &std::ffi::OsStr) -> &[u8] {
    value.as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn encoded_path_is_not_lossy_for_valid_utf8() {
        let encoded = EncodedPath::from_path(Path::new("/home/dave/.config/retroarch"));
        assert_eq!(encoded.display, "/home/dave/.config/retroarch");
        assert!(!encoded.lossy);
    }

    #[test]
    fn encoded_path_never_fails_for_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let raw = std::ffi::OsString::from_vec(b"/tmp/bad-\x80-name".to_vec());
        let encoded = EncodedPath::from_path(&PathBuf::from(raw));
        assert!(encoded.lossy);
        assert!(encoded.display.contains('\u{FFFD}'));
        // Must serialize successfully - the whole point of this type.
        let json = serde_json::to_string(&encoded).unwrap();
        assert!(json.contains("\"lossy\":true"));
    }

    #[test]
    fn host_filesystem_probe_distinguishes_file_directory_and_missing() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-emulator-env-probe-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("file.txt"), b"hi").unwrap();

        let fs_probe = HostReadOnlyFilesystem;
        assert_eq!(fs_probe.probe(&root.join("dir")), FsProbe::PresentDirectory);
        assert_eq!(fs_probe.probe(&root.join("file.txt")), FsProbe::PresentFile);
        assert_eq!(fs_probe.probe(&root.join("missing")), FsProbe::Missing);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn host_filesystem_probe_reports_symlink_without_following() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "archivefs-emulator-env-symlink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("real.txt"), b"hi").unwrap();
        symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        let fs_probe = HostReadOnlyFilesystem;
        assert_eq!(fs_probe.probe(&root.join("link.txt")), FsProbe::Symlink);
        assert_eq!(
            fs_probe.read_bounded(&root.join("link.txt"), 1024),
            BoundedReadResult::Symlink
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn host_filesystem_read_bounded_reports_too_large_without_trusting_prefix() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-emulator-env-toolarge-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("big.txt"), vec![b'x'; 10]).unwrap();

        let fs_probe = HostReadOnlyFilesystem;
        assert_eq!(
            fs_probe.read_bounded(&root.join("big.txt"), 5),
            BoundedReadResult::TooLarge
        );
        assert_eq!(
            fs_probe.read_bounded(&root.join("big.txt"), 10),
            BoundedReadResult::Ok(vec![b'x'; 10])
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn host_filesystem_list_dir_bounded_reports_too_large() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-emulator-env-listtoolarge-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for index in 0..5 {
            fs::write(root.join(format!("entry-{index}")), b"x").unwrap();
        }

        let fs_probe = HostReadOnlyFilesystem;
        assert_eq!(
            fs_probe.list_dir_bounded(&root, 3),
            BoundedListResult::TooLarge
        );
        match fs_probe.list_dir_bounded(&root, 5) {
            BoundedListResult::Ok(entries) => assert_eq!(entries.len(), 5),
            other => panic!("expected Ok, got {other:?}"),
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn host_filesystem_does_not_create_or_modify_anything() {
        let root = std::env::temp_dir().join(format!(
            "archivefs-emulator-env-readonly-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let fs_probe = HostReadOnlyFilesystem;
        let _ = fs_probe.probe(&root.join("nonexistent"));
        let _ = fs_probe.list_dir_bounded(&root.join("nonexistent-dir"), 10);
        let _ = fs_probe.read_bounded(&root.join("nonexistent-file"), 10);

        let entries: Vec<_> = fs::read_dir(&root).unwrap().collect();
        assert!(
            entries.is_empty(),
            "no file or directory should have been created"
        );

        let _ = fs::remove_dir_all(root);
    }
}
