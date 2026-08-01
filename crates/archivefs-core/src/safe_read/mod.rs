//! The one bounded, read-only file-open policy this build has.
//!
//! Two subsystems need to read a handful of bytes from a game file to identify
//! it: platform signature detection ([`crate::platform::detect`]) and disc
//! identity ([`crate::game_identity`]). They used to carry *different* symlink
//! policies, and neither was right:
//!
//! - `game_identity` walked every path component and refused if any of them was
//!   a symlink. Strict and safe, but it meant a symlinked library - which is how
//!   real libraries are usually arranged - could never be identified at all.
//! - platform detection only checked the final component, then used
//!   `File::open`, which happily follows symlinks in *parent* directories. So it
//!   was simultaneously more permissive and less deliberate.
//!
//! This module replaces both with one policy, built on the stricter of the two
//! and extended with an explicit, opt-in way to follow a symlink safely.
//!
//! # The policy
//!
//! A read is permitted only when every one of these holds:
//!
//! 1. The path is absolute and contains no `.` or `..` component.
//! 2. If the path is **not** a symlink: no component of it may be a symlink
//!    either, exactly as before. This is the unchanged, default behaviour.
//! 3. If the path **is** a symlink, it may be followed only when:
//!    a. at least one trusted root is configured (absent roots means refuse -
//!       see [`TrustedRoots::none`]);
//!    b. the symlink itself lies inside a trusted root;
//!    c. the target canonicalises successfully - which is what rules out a
//!       broken link and a symlink loop, since `canonicalize` reports both as
//!       errors;
//!    d. the canonical target also lies inside a trusted root;
//!    e. the canonical target is a regular file - so a directory, FIFO, socket,
//!       device or anything else is refused.
//! 4. The file is opened `O_NOFOLLOW | O_CLOEXEC` on the already-canonical
//!    path, and its device and inode are re-checked afterwards, so the thing
//!    that was validated is the thing that was opened.
//!
//! # Fail-closed
//!
//! [`TrustedRoots::none`] refuses every symlink. Every caller that does not
//! deliberately supply roots therefore behaves exactly as this build did
//! before, and gaining the capability requires passing the roots in.
//!
//! # What this module never does
//!
//! No write, no create, no permission or timestamp change, no rename, no
//! delete, no hashing, no mounting, no extraction, no process, no network.
//! Reads are positional and bounded by the caller's own limit. A resolved
//! symlink target is never returned as a write destination - the only thing
//! that leaves here is a read-only [`SafeFile`].

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

/// The configured source roots a symlink target is allowed to resolve into.
///
/// Canonicalised once at construction, because comparing a canonical target
/// against a non-canonical root would be the obvious way to get containment
/// wrong. A root that cannot be canonicalised (it does not exist, or is not
/// readable) is dropped rather than trusted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedRoots {
    roots: Vec<PathBuf>,
}

impl TrustedRoots {
    /// No trusted roots: every symlink is refused. This is the default and the
    /// behaviour every pre-existing caller keeps.
    pub fn none() -> Self {
        Self { roots: Vec::new() }
    }

    /// Builds a trusted set from configured source roots.
    ///
    /// Only absolute, existing, canonicalisable directories are kept. A
    /// relative or missing root is silently dropped: trusting a path that
    /// cannot be resolved would make containment unprovable.
    pub fn from_paths<I, P>(paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut roots: Vec<PathBuf> = paths
            .into_iter()
            .filter_map(|path| {
                let path = path.as_ref();
                if !path.is_absolute() {
                    return None;
                }
                let canonical = std::fs::canonicalize(path).ok()?;
                std::fs::symlink_metadata(&canonical)
                    .ok()
                    .filter(|metadata| metadata.is_dir())
                    .map(|_| canonical)
            })
            .collect();
        roots.sort();
        roots.dedup();
        Self { roots }
    }

    /// The configured source folders of `config`, as a trusted set.
    pub fn from_config(config: &crate::Config) -> Self {
        Self::from_paths(&config.source_folders)
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Whether `candidate` - which must already be canonical - lies inside one
    /// of the trusted roots.
    ///
    /// Compares whole path components via `starts_with`, so `/mnt/roms-backup`
    /// is not treated as being inside `/mnt/roms`.
    fn contains_canonical(&self, candidate: &Path) -> bool {
        self.roots.iter().any(|root| candidate.starts_with(root))
    }
}

/// Why a read was refused. Each variant is a distinct, explainable reason
/// rather than one opaque failure, because "the target left your library" and
/// "the target is a directory" call for different responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeReadRefusal {
    /// The path was relative, so containment could not be established.
    NotAbsolute,
    /// The path contained a `.` or `..` component.
    NonNormalComponent,
    /// A component of a non-symlink path was itself a symlink.
    SymlinkInPath(PathBuf),
    /// The path, or a component of it, could not be stat'ed.
    Unreadable(String),
    /// The target is not a regular file: a directory, FIFO, socket, device,
    /// or anything else.
    NotRegularFile,
    /// A symlink was found but no trusted root is configured, so following it
    /// could not be justified.
    NoTrustedRoots,
    /// The symlink itself is outside every trusted root.
    SymlinkOutsideTrustedRoots,
    /// The symlink resolved to something outside every trusted root.
    TargetOutsideTrustedRoots,
    /// The target could not be canonicalised: it is missing, it forms a loop,
    /// or it is not permitted. `canonicalize` reports all three the same way,
    /// so the underlying message is carried through verbatim.
    UnresolvableTarget(String),
    /// The file changed between validation and opening.
    ChangedWhileOpening,
    /// The file could not be opened.
    OpenFailed(String),
}

impl SafeReadRefusal {
    /// A short explanation suitable for evidence or a diagnostic.
    pub fn detail(&self) -> String {
        match self {
            Self::NotAbsolute => "the path is not absolute".to_string(),
            Self::NonNormalComponent => "the path contains a `.` or `..` component".to_string(),
            Self::SymlinkInPath(component) => {
                format!("symlink refused: {}", component.display())
            }
            Self::Unreadable(detail) => detail.clone(),
            Self::NotRegularFile => "the target is not a regular file".to_string(),
            // These four keep the historical "symlink refused" wording so a
            // diagnostic that already looked for it still reads correctly, and
            // each adds the reason rather than leaving it unexplained.
            Self::NoTrustedRoots => {
                "symlink refused: no trusted source root is configured".to_string()
            }
            Self::SymlinkOutsideTrustedRoots => {
                "symlink refused: the link is outside every configured source root".to_string()
            }
            Self::TargetOutsideTrustedRoots => {
                "symlink refused: it resolves outside every configured source root".to_string()
            }
            Self::UnresolvableTarget(detail) => {
                format!("symlink refused: the target could not be resolved: {detail}")
            }
            Self::ChangedWhileOpening => "the file changed while it was being opened".to_string(),
            Self::OpenFailed(detail) => detail.clone(),
        }
    }

    /// A stable, machine-readable reason, for audit counting.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAbsolute => "not_absolute",
            Self::NonNormalComponent => "non_normal_component",
            Self::SymlinkInPath(_) => "symlink_in_path",
            Self::Unreadable(_) => "unreadable",
            Self::NotRegularFile => "not_regular_file",
            Self::NoTrustedRoots => "no_trusted_roots",
            Self::SymlinkOutsideTrustedRoots => "symlink_outside_trusted_roots",
            Self::TargetOutsideTrustedRoots => "target_outside_trusted_roots",
            Self::UnresolvableTarget(_) => "unresolvable_target",
            Self::ChangedWhileOpening => "changed_while_opening",
            Self::OpenFailed(_) => "open_failed",
        }
    }
}

/// A validated, read-only handle. The only thing this module hands out.
#[derive(Debug)]
pub struct SafeFile {
    file: File,
    length: u64,
    resolved_via_symlink: bool,
}

impl SafeFile {
    /// The file's length, as observed during validation.
    pub fn len(&self) -> u64 {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Whether the path given was a symlink that resolved safely. Used to say
    /// so in evidence, without exposing where it pointed.
    pub fn resolved_via_symlink(&self) -> bool {
        self.resolved_via_symlink
    }

    /// The underlying handle, for callers that do their own bounded reading
    /// (`game_identity` reads structured records, not fixed-offset magic).
    /// Read-only: the handle was opened without write access.
    pub fn into_file(self) -> File {
        self.file
    }

    /// Reads exactly `length` bytes at `offset`.
    ///
    /// Returns `None` rather than reading anything when `length` exceeds
    /// `max_length`, or when the range would run past the end of the file, so
    /// a bound is enforced before any I/O happens.
    pub fn read_exact_at(
        &mut self,
        offset: u64,
        length: usize,
        max_length: usize,
    ) -> Option<Vec<u8>> {
        if length == 0 || length > max_length {
            return None;
        }
        if offset.saturating_add(length as u64) > self.length {
            return None;
        }
        self.file.seek(SeekFrom::Start(offset)).ok()?;
        let mut buffer = vec![0_u8; length];
        self.file.read_exact(&mut buffer).ok()?;
        Some(buffer)
    }
}

/// Opens `path` for bounded reading under the policy documented on this module.
///
/// `trusted` decides whether a symlink may be followed at all. Pass
/// [`TrustedRoots::none`] to keep the historical behaviour of refusing every
/// symlink.
pub fn open_bounded_read(path: &Path, trusted: &TrustedRoots) -> Result<SafeFile, SafeReadRefusal> {
    if !path.is_absolute() {
        return Err(SafeReadRefusal::NotAbsolute);
    }
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => {}
            _ => return Err(SafeReadRefusal::NonNormalComponent),
        }
    }

    let link_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| SafeReadRefusal::Unreadable(format!("{}: {error}", path.display())))?;

    let target = if link_metadata.file_type().is_symlink() {
        resolve_trusted_symlink(path, trusted)?
    } else {
        // Unchanged behaviour: every component must be a real directory, so
        // nothing was reached through a link the caller did not see.
        refuse_symlinked_components(path)?;
        if !link_metadata.is_file() {
            return Err(SafeReadRefusal::NotRegularFile);
        }
        path.to_path_buf()
    };

    open_validated(&target, link_metadata.file_type().is_symlink())
}

/// Resolves a symlink under the trusted-root rules and returns the canonical
/// target. Every refusal here is deliberate and named.
fn resolve_trusted_symlink(
    path: &Path,
    trusted: &TrustedRoots,
) -> Result<PathBuf, SafeReadRefusal> {
    if trusted.is_empty() {
        return Err(SafeReadRefusal::NoTrustedRoots);
    }

    // The symlink itself must be inside the library. Its *parent* is
    // canonicalised rather than the link, because canonicalising the link
    // would resolve it and answer the wrong question.
    let parent = path.parent().ok_or(SafeReadRefusal::NotAbsolute)?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|error| SafeReadRefusal::Unreadable(format!("{}: {error}", parent.display())))?;
    if !trusted.contains_canonical(&canonical_parent) {
        return Err(SafeReadRefusal::SymlinkOutsideTrustedRoots);
    }

    // Canonicalising is what rules out a broken link and a symlink loop: the
    // operating system reports ENOENT and ELOOP respectively, and neither is
    // treated as a resolvable target.
    let canonical_target = std::fs::canonicalize(path)
        .map_err(|error| SafeReadRefusal::UnresolvableTarget(error.to_string()))?;

    if !trusted.contains_canonical(&canonical_target) {
        return Err(SafeReadRefusal::TargetOutsideTrustedRoots);
    }
    // A canonical path contains no symlinks by construction, so this checks
    // what the link actually pointed at.
    let target_metadata = std::fs::symlink_metadata(&canonical_target).map_err(|error| {
        SafeReadRefusal::Unreadable(format!("{}: {error}", canonical_target.display()))
    })?;
    if !target_metadata.is_file() {
        // Covers a directory, FIFO, socket, block or character device, and
        // anything else that is not a plain file.
        return Err(SafeReadRefusal::NotRegularFile);
    }
    Ok(canonical_target)
}

/// Refuses when any component of `path` is itself a symlink.
fn refuse_symlinked_components(path: &Path) -> Result<(), SafeReadRefusal> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::Normal(component) => current.push(component),
            _ => return Err(SafeReadRefusal::NonNormalComponent),
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            SafeReadRefusal::Unreadable(format!("{}: {error}", current.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(SafeReadRefusal::SymlinkInPath(current));
        }
    }
    Ok(())
}

/// Opens an already-validated, already-canonical path read-only, and confirms
/// afterwards that the opened file is the one that was validated.
fn open_validated(target: &Path, resolved_via_symlink: bool) -> Result<SafeFile, SafeReadRefusal> {
    let before = std::fs::symlink_metadata(target)
        .map_err(|error| SafeReadRefusal::Unreadable(format!("{}: {error}", target.display())))?;
    if !before.is_file() {
        return Err(SafeReadRefusal::NotRegularFile);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The path is canonical, so `O_NOFOLLOW` cannot reject a legitimate
        // read here - but it is kept as a hard guarantee that nothing is
        // followed at open time either.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(target)
        .map_err(|error| SafeReadRefusal::OpenFailed(error.to_string()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = file
            .metadata()
            .map_err(|error| SafeReadRefusal::OpenFailed(error.to_string()))?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(SafeReadRefusal::ChangedWhileOpening);
        }
    }

    Ok(SafeFile {
        file,
        length: before.len(),
        resolved_via_symlink,
    })
}

#[cfg(test)]
mod tests;
