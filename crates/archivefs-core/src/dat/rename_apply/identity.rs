//! Read-only object identity capture and verification.
//!
//! A rename is only allowed when the object at the source path is still the
//! very same object that was reviewed. Identity is captured with
//! `symlink_metadata` (never following a link), and compared at preflight and
//! again after the rename so that a file replaced by a different object, a
//! symlink, or a different inode is detected rather than renamed by mistake.
//!
//! On platforms with inode/device numbers these are part of the identity; on
//! others the identity is size + modification time + kind.

use std::path::Path;

use super::model::{ObjectIdentity, ObjectKind};

/// Captures the identity of `path` without following a symlink.
pub fn capture_identity(path: &Path) -> std::io::Result<ObjectIdentity> {
    let metadata = std::fs::symlink_metadata(path)?;
    let kind = classify_at(path)?;
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0);
    let identity = ObjectIdentity {
        size_bytes: metadata.len(),
        modified_unix,
        kind,
        #[cfg(unix)]
        ino: std::os::unix::fs::MetadataExt::ino(&metadata),
        #[cfg(unix)]
        dev: std::os::unix::fs::MetadataExt::dev(&metadata),
    };
    Ok(identity)
}

/// Whether `current` is the same object as the recorded `expected` identity.
///
/// For a regular file this compares the size, kind and - where supported - the
/// inode/device numbers. A file whose mtime changed but whose inode, size and
/// kind did not is treated as unchanged: mtime is not part of the identity
/// contract for a rename (renaming preserves the inode, so size + inode + dev
/// are the strong checks). mtime is captured so a size-and-mtime-only platform
/// still detects a rewrite.
pub fn identity_matches(expected: &ObjectIdentity, current: &ObjectIdentity) -> bool {
    if expected.kind != current.kind || expected.size_bytes != current.size_bytes {
        return false;
    }
    #[cfg(unix)]
    {
        if expected.ino != current.ino || expected.dev != current.dev {
            return false;
        }
    }
    #[cfg(not(unix))]
    {
        if expected.modified_unix != current.modified_unix {
            return false;
        }
    }
    true
}

/// The identity of a *symlink itself* is deliberately never the identity of
/// its target: [`capture_identity`] uses `symlink_metadata`, so a symlink's
/// inode is its own. A source swapped for a symlink therefore never matches a
/// recorded regular-file identity.
///
/// Classifies `path` into [`ObjectKind`], distinguishing a broken symlink.
pub fn classify_at(path: &Path) -> std::io::Result<ObjectKind> {
    let metadata = std::fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        if std::fs::metadata(path).is_ok() {
            Ok(ObjectKind::Symlink)
        } else {
            Ok(ObjectKind::BrokenSymlink)
        }
    } else if file_type.is_file() {
        Ok(ObjectKind::RegularFile)
    } else {
        Ok(ObjectKind::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_uses_symlink_metadata_not_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.bin");
        std::fs::write(&target, b"hello").unwrap();
        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let target_identity = capture_identity(&target).unwrap();
        let link_identity = capture_identity(&link).unwrap();
        // A symlink's own inode is never the target's.
        #[cfg(unix)]
        {
            assert_ne!(target_identity.ino, link_identity.ino);
        }
        assert_eq!(link_identity.kind, ObjectKind::Symlink);
        assert_eq!(classify_at(&link).unwrap(), ObjectKind::Symlink);
        assert_eq!(classify_at(&target).unwrap(), ObjectKind::RegularFile);
    }

    #[test]
    fn a_broken_symlink_is_classified() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("broken.bin");
        std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
        assert_eq!(classify_at(&link).unwrap(), ObjectKind::BrokenSymlink);
    }

    #[test]
    fn identity_matches_itself_and_rejects_a_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"one").unwrap();
        let first = capture_identity(&path).unwrap();
        assert!(identity_matches(&first, &first));

        // A different-size rewrite changes the identity.
        std::fs::write(&path, b"a much longer payload").unwrap();
        let resized = capture_identity(&path).unwrap();
        assert!(!identity_matches(&first, &resized), "size changed");

        // A different file with the same size and kind is distinguished by its
        // inode/device where supported.
        let other = dir.path().join("other.bin");
        std::fs::write(&other, b"one").unwrap();
        let other_identity = capture_identity(&other).unwrap();
        assert!(
            !identity_matches(&first, &other_identity),
            "a different object must not match even with identical size"
        );
    }

    #[test]
    fn a_symlink_substitution_never_matches_a_regular_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"one").unwrap();
        let regular = capture_identity(&path).unwrap();

        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &path).unwrap();
        let substituted = capture_identity(&path).unwrap();
        assert!(!identity_matches(&regular, &substituted));
    }
}
