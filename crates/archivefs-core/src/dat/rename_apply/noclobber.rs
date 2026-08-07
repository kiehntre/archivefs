//! The no-clobber rename primitive.
//!
//! `std::fs::rename` maps to `rename(2)`, which *replaces* an existing
//! destination - the one behaviour this transaction system must never perform.
//! On Linux the executor therefore uses `renameat2(2)` with
//! `RENAME_NOREPLACE`, which atomically refuses to rename when the destination
//! already exists. This is a true no-clobber primitive, not a race-prone
//! "exists then rename" sequence: the check and the rename are a single
//! syscall, so there is no window in which a destination can appear between
//! the two.
//!
//! On platforms without a verified no-clobber primitive the executor refuses
//! to mutate rather than falling back to a TOCTOU-prone sequence.

use std::path::Path;

/// Why a no-clobber rename failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoClobberError {
    /// The destination exists. Never overwritten.
    DestinationExists,
    /// This platform has no verified no-clobber rename primitive; the
    /// transaction refuses to mutate here.
    UnsupportedPlatform,
    /// Another I/O error.
    Io(String),
}

impl std::fmt::Display for NoClobberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DestinationExists => write!(f, "the destination already exists"),
            Self::UnsupportedPlatform => write!(
                f,
                "no no-clobber rename primitive is available on this platform"
            ),
            Self::Io(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for NoClobberError {}

/// Renames `source` onto `destination`, atomically refusing if `destination`
/// exists. `source` and `destination` must share the same parent directory.
pub fn rename_noreplace(source: &Path, destination: &Path) -> Result<(), NoClobberError> {
    #[cfg(target_os = "linux")]
    {
        rename_noreplace_linux(source, destination)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, destination);
        Err(NoClobberError::UnsupportedPlatform)
    }
}

/// The Linux implementation: `renameat2(AT_FDCWD, src, AT_FDCWD, dst,
/// RENAME_NOREPLACE)`.
#[cfg(target_os = "linux")]
fn rename_noreplace_linux(source: &Path, destination: &Path) -> Result<(), NoClobberError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source_c = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| NoClobberError::Io("source path contains a NUL byte".to_string()))?;
    let destination_c = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| NoClobberError::Io("destination path contains a NUL byte".to_string()))?;

    // SAFETY: both C strings are valid NUL-terminated byte arrays with no
    // interior NUL; the syscall copies the paths and does not retain the
    // pointers; RENAME_NOREPLACE is a constant. The syscall has no other
    // effect when it fails, and on success it performs exactly the atomic
    // no-replace rename this transaction requires.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return Err(NoClobberError::DestinationExists);
    }
    Err(NoClobberError::Io(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_noreplace_rename_moves_a_file() {
        let dir = temp();
        let source = dir.path().join("a.bin");
        let destination = dir.path().join("b.bin");
        std::fs::write(&source, b"hello").unwrap();
        rename_noreplace(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"hello");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_existing_destination_is_never_overwritten() {
        let dir = temp();
        let source = dir.path().join("a.bin");
        let destination = dir.path().join("b.bin");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"old").unwrap();
        let error = rename_noreplace(&source, &destination).unwrap_err();
        assert_eq!(error, NoClobberError::DestinationExists);
        assert_eq!(std::fs::read(&destination).unwrap(), b"old");
        assert_eq!(std::fs::read(&source).unwrap(), b"new");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_destination_that_appears_after_preflight_is_still_refused() {
        // The whole point of RENAME_NOREPLACE: there is no exists()+rename
        // window. Even if the destination appears just before the call, the
        // call refuses atomically.
        let dir = temp();
        let source = dir.path().join("a.bin");
        let destination = dir.path().join("b.bin");
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&destination, b"appeared").unwrap();
        let error = rename_noreplace(&source, &destination).unwrap_err();
        assert_eq!(error, NoClobberError::DestinationExists);
    }
}
