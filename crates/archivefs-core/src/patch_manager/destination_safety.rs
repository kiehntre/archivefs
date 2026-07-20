//! Read-only destination-path validation for future installers.
//!
//! This module never creates a directory or file and never opens destination
//! content. It deliberately rejects every symlink used as a destination root,
//! parent directory, or final destination, including symlinks whose targets
//! remain beneath the validated root.
//!
//! These checks reduce path-traversal and symlink risk, but cannot eliminate
//! time-of-check/time-of-use (TOCTOU) races. A future write-capable caller must
//! revalidate immediately before writing and use platform-appropriate atomic or
//! directory-relative filesystem operations where available.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// Whether the validated destination root currently exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationRootState {
    ExistingDirectory,
    Absent,
}

/// A byte-preserving, absolute destination root whose components were
/// inspected with symlink-aware metadata and accepted only when none is a
/// symlink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedDestinationRoot {
    path: PathBuf,
    state: DestinationRootState,
}

impl ValidatedDestinationRoot {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> DestinationRootState {
        self.state
    }
}

/// A proposed file destination built only from a validated root and two safe
/// path components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeDestination {
    root: ValidatedDestinationRoot,
    platform_directory: PathBuf,
    file_name: PathBuf,
    path: PathBuf,
}

impl SafeDestination {
    pub fn root(&self) -> &ValidatedDestinationRoot {
        &self.root
    }

    pub fn platform_directory(&self) -> &OsStr {
        self.platform_directory.as_os_str()
    }

    pub fn file_name(&self) -> &OsStr {
        self.file_name.as_os_str()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// State recorded for each parent component beneath the destination root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectedParentState {
    ExistingDirectory,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedParent {
    pub path: PathBuf,
    pub state: InspectedParentState,
}

/// Safe final-entry states. Rejected states are also represented here and
/// attached to [`DestinationSafetyError`] when final inspection fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationState {
    Absent,
    RegularFile,
    Directory,
    Symlink,
    Unsafe,
}

/// A complete successful assessment. Only `Absent` and `RegularFile` are safe
/// successful final states; all other final states return a typed error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationSafetyAssessment {
    pub validated_root: ValidatedDestinationRoot,
    pub proposed_destination: SafeDestination,
    pub destination_state: DestinationState,
    pub inspected_parents: Vec<InspectedParent>,
}

/// Stable machine-readable failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationSafetyFailureReason {
    RootNotDirectory,
    RootSymlink,
    UnsafeComponent,
    Traversal,
    ParentSymlink,
    ParentSymlinkEscape,
    FinalSymlink,
    FinalSymlinkEscape,
    BrokenSymlink,
    SymlinkLoop,
    NonDirectoryParent,
    DestinationIsDirectory,
    DestinationOutsideRoot,
    UnsafeDestination,
    InspectionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationSafetyError {
    pub reason: DestinationSafetyFailureReason,
    pub path: PathBuf,
    pub destination_state: Option<DestinationState>,
    pub inspected_parents: Vec<InspectedParent>,
}

impl DestinationSafetyError {
    fn new(reason: DestinationSafetyFailureReason, path: PathBuf) -> Self {
        Self {
            reason,
            path,
            destination_state: None,
            inspected_parents: Vec::new(),
        }
    }

    fn final_entry(
        reason: DestinationSafetyFailureReason,
        path: PathBuf,
        destination_state: DestinationState,
        inspected_parents: Vec<InspectedParent>,
    ) -> Self {
        Self {
            reason,
            path,
            destination_state: Some(destination_state),
            inspected_parents,
        }
    }
}

impl fmt::Display for DestinationSafetyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "destination safety failure {:?} at {}",
            self.reason,
            self.path.display()
        )
    }
}

impl std::error::Error for DestinationSafetyError {}

/// Validate an existing directory or an absent root without creating it.
///
/// Every existing component of the root path is inspected with
/// [`fs::symlink_metadata`]. All root-path symlinks are rejected.
pub fn validate_destination_root(
    root: &Path,
) -> Result<ValidatedDestinationRoot, DestinationSafetyError> {
    let absolute = absolute_lexical_path(root)?;
    let mut current = PathBuf::new();

    for component in absolute.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let reason = classify_link_failure(
                    &current,
                    &absolute,
                    DestinationSafetyFailureReason::RootSymlink,
                    DestinationSafetyFailureReason::RootSymlink,
                );
                return Err(DestinationSafetyError::new(reason, current));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(DestinationSafetyError::new(
                    DestinationSafetyFailureReason::RootNotDirectory,
                    current,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ValidatedDestinationRoot {
                    path: absolute,
                    state: DestinationRootState::Absent,
                });
            }
            Err(error) => {
                return Err(DestinationSafetyError::new(
                    io_failure_reason(&error),
                    current,
                ));
            }
        }
    }

    Ok(ValidatedDestinationRoot {
        path: absolute,
        state: DestinationRootState::ExistingDirectory,
    })
}

/// Construct `<root>/<platform>/<filename>` without sanitizing either input.
pub fn construct_safe_destination(
    root: &ValidatedDestinationRoot,
    platform_directory: &OsStr,
    file_name: &OsStr,
) -> Result<SafeDestination, DestinationSafetyError> {
    validate_single_component(platform_directory, root.path())?;
    validate_single_component(file_name, root.path())?;

    let platform_directory = PathBuf::from(platform_directory);
    let file_name = PathBuf::from(file_name);
    let path = root.path.join(&platform_directory).join(&file_name);
    if !path.starts_with(&root.path) {
        return Err(DestinationSafetyError::new(
            DestinationSafetyFailureReason::DestinationOutsideRoot,
            path,
        ));
    }

    Ok(SafeDestination {
        root: root.clone(),
        platform_directory,
        file_name,
        path,
    })
}

/// Inspect every existing parent beneath the root and then the final entry.
/// Missing parents are reported but never created.
pub fn inspect_safe_destination(
    destination: &SafeDestination,
) -> Result<DestinationSafetyAssessment, DestinationSafetyError> {
    if !destination.path.starts_with(destination.root.path()) {
        return Err(DestinationSafetyError::new(
            DestinationSafetyFailureReason::DestinationOutsideRoot,
            destination.path.clone(),
        ));
    }

    let parent = destination.root.path.join(&destination.platform_directory);
    let mut inspected_parents = Vec::with_capacity(1);
    let parent_missing = match fs::symlink_metadata(&parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let reason = classify_link_failure(
                &parent,
                destination.root.path(),
                DestinationSafetyFailureReason::ParentSymlink,
                DestinationSafetyFailureReason::ParentSymlinkEscape,
            );
            let mut error = DestinationSafetyError::new(reason, parent);
            error.inspected_parents = inspected_parents;
            return Err(error);
        }
        Ok(metadata) if metadata.is_dir() => {
            inspected_parents.push(InspectedParent {
                path: parent,
                state: InspectedParentState::ExistingDirectory,
            });
            false
        }
        Ok(_) => {
            let mut error = DestinationSafetyError::new(
                DestinationSafetyFailureReason::NonDirectoryParent,
                parent,
            );
            error.inspected_parents = inspected_parents;
            return Err(error);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            inspected_parents.push(InspectedParent {
                path: parent,
                state: InspectedParentState::Missing,
            });
            true
        }
        Err(error) => {
            let mut failure = DestinationSafetyError::new(io_failure_reason(&error), parent);
            failure.inspected_parents = inspected_parents;
            return Err(failure);
        }
    };

    let destination_state = if parent_missing {
        DestinationState::Absent
    } else {
        match fs::symlink_metadata(&destination.path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let reason = classify_link_failure(
                    &destination.path,
                    destination.root.path(),
                    DestinationSafetyFailureReason::FinalSymlink,
                    DestinationSafetyFailureReason::FinalSymlinkEscape,
                );
                return Err(DestinationSafetyError::final_entry(
                    reason,
                    destination.path.clone(),
                    DestinationState::Symlink,
                    inspected_parents,
                ));
            }
            Ok(metadata) if metadata.is_file() => DestinationState::RegularFile,
            Ok(metadata) if metadata.is_dir() => {
                return Err(DestinationSafetyError::final_entry(
                    DestinationSafetyFailureReason::DestinationIsDirectory,
                    destination.path.clone(),
                    DestinationState::Directory,
                    inspected_parents,
                ));
            }
            Ok(_) => {
                return Err(DestinationSafetyError::final_entry(
                    DestinationSafetyFailureReason::UnsafeDestination,
                    destination.path.clone(),
                    DestinationState::Unsafe,
                    inspected_parents,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => DestinationState::Absent,
            Err(error) => {
                return Err(DestinationSafetyError::final_entry(
                    io_failure_reason(&error),
                    destination.path.clone(),
                    DestinationState::Unsafe,
                    inspected_parents,
                ));
            }
        }
    };

    Ok(DestinationSafetyAssessment {
        validated_root: destination.root.clone(),
        proposed_destination: destination.clone(),
        destination_state,
        inspected_parents,
    })
}

/// Convenience entry point for the complete read-only validation sequence.
pub fn assess_destination(
    root: &Path,
    platform_directory: &OsStr,
    file_name: &OsStr,
) -> Result<DestinationSafetyAssessment, DestinationSafetyError> {
    let root = validate_destination_root(root)?;
    let destination = construct_safe_destination(&root, platform_directory, file_name)?;
    inspect_safe_destination(&destination)
}

fn validate_single_component(
    value: &OsStr,
    error_path: &Path,
) -> Result<(), DestinationSafetyError> {
    if value.is_empty() {
        return Err(DestinationSafetyError::new(
            DestinationSafetyFailureReason::UnsafeComponent,
            error_path.to_path_buf(),
        ));
    }

    if Path::new(value)
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(DestinationSafetyError::new(
            DestinationSafetyFailureReason::Traversal,
            error_path.join(value),
        ));
    }

    let mut components = Path::new(value).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == value => Ok(()),
        _ => Err(DestinationSafetyError::new(
            DestinationSafetyFailureReason::UnsafeComponent,
            error_path.join(value),
        )),
    }
}

fn absolute_lexical_path(root: &Path) -> Result<PathBuf, DestinationSafetyError> {
    if root.as_os_str().is_empty() {
        return Err(DestinationSafetyError::new(
            DestinationSafetyFailureReason::UnsafeComponent,
            root.to_path_buf(),
        ));
    }
    if root.components().any(|part| part == Component::ParentDir) {
        return Err(DestinationSafetyError::new(
            DestinationSafetyFailureReason::Traversal,
            root.to_path_buf(),
        ));
    }

    let joined = if root.is_absolute() {
        root.to_path_buf()
    } else {
        let current = std::env::current_dir().map_err(|_| {
            DestinationSafetyError::new(
                DestinationSafetyFailureReason::InspectionFailed,
                root.to_path_buf(),
            )
        })?;
        current.join(root)
    };

    let mut normalized = PathBuf::new();
    for component in joined.components() {
        if component != Component::CurDir {
            normalized.push(component.as_os_str());
        }
    }
    Ok(normalized)
}

fn classify_link_failure(
    link: &Path,
    destination_root: &Path,
    in_root_reason: DestinationSafetyFailureReason,
    escape_reason: DestinationSafetyFailureReason,
) -> DestinationSafetyFailureReason {
    match fs::metadata(link) {
        Ok(_) => {
            let target = match fs::read_link(link) {
                Ok(target) => target,
                Err(error) => return io_failure_reason(&error),
            };
            let resolved = if target.is_absolute() {
                lexical_normalize(&target)
            } else {
                lexical_normalize(&link.parent().unwrap_or(Path::new("/")).join(target))
            };
            if resolved.starts_with(destination_root) {
                in_root_reason
            } else {
                escape_reason
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DestinationSafetyFailureReason::BrokenSymlink
        }
        Err(error) => io_failure_reason(&error),
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn io_failure_reason(error: &io::Error) -> DestinationSafetyFailureReason {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return DestinationSafetyFailureReason::SymlinkLoop;
    }

    DestinationSafetyFailureReason::InspectionFailed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "archivefs-destination-safety-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn reason(error: DestinationSafetyError) -> DestinationSafetyFailureReason {
        error.reason
    }

    #[test]
    fn existing_normal_root_is_validated() {
        let fixture = Fixture::new("existing-root");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(root.path(), fixture.path);
        assert_eq!(root.state(), DestinationRootState::ExistingDirectory);
    }

    #[test]
    fn absent_root_is_valid_and_remains_absent() {
        let fixture = Fixture::new("absent-root");
        let root_path = fixture.path.join("missing").join("nested");
        let root = validate_destination_root(&root_path).unwrap();
        assert_eq!(root.state(), DestinationRootState::Absent);
        assert!(!root_path.exists());
    }

    #[test]
    fn root_existing_as_file_is_rejected() {
        let fixture = Fixture::new("root-file");
        let root = fixture.path.join("file");
        File::create(&root).unwrap();
        assert_eq!(
            reason(validate_destination_root(&root).unwrap_err()),
            DestinationSafetyFailureReason::RootNotDirectory
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_root_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("root-symlink");
        let target = fixture.path.join("target");
        fs::create_dir(&target).unwrap();
        let root = fixture.path.join("root");
        symlink(target, &root).unwrap();
        assert_eq!(
            reason(validate_destination_root(&root).unwrap_err()),
            DestinationSafetyFailureReason::RootSymlink
        );
    }

    #[cfg(unix)]
    #[test]
    fn broken_root_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("broken-root-symlink");
        let root = fixture.path.join("root");
        symlink("missing", &root).unwrap();
        assert_eq!(
            reason(validate_destination_root(&root).unwrap_err()),
            DestinationSafetyFailureReason::BrokenSymlink
        );
    }

    #[cfg(unix)]
    #[test]
    fn root_symlink_loop_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("root-symlink-loop");
        let root = fixture.path.join("root");
        symlink("root", &root).unwrap();
        assert_eq!(
            reason(validate_destination_root(&root).unwrap_err()),
            DestinationSafetyFailureReason::SymlinkLoop
        );
    }

    #[test]
    fn safe_platform_and_filename_are_preserved() {
        let fixture = Fixture::new("safe-components");
        let root = validate_destination_root(&fixture.path).unwrap();
        let destination =
            construct_safe_destination(&root, OsStr::new("Nintendo"), OsStr::new("Game.cht"))
                .unwrap();
        assert_eq!(
            destination.path(),
            fixture.path.join("Nintendo").join("Game.cht")
        );
    }

    #[test]
    fn absolute_platform_is_rejected() {
        let fixture = Fixture::new("absolute-platform");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(&root, OsStr::new("/platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::UnsafeComponent
        );
    }

    #[test]
    fn absolute_filename_is_rejected() {
        let fixture = Fixture::new("absolute-filename");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(&root, OsStr::new("platform"), OsStr::new("/a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::UnsafeComponent
        );
    }

    #[test]
    fn traversal_component_is_rejected() {
        let fixture = Fixture::new("traversal");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(&root, OsStr::new(".."), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::Traversal
        );
    }

    #[test]
    fn embedded_separator_is_rejected() {
        let fixture = Fixture::new("separator");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(&root, OsStr::new("platform/sub"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::UnsafeComponent
        );
    }

    #[test]
    fn empty_component_is_rejected() {
        let fixture = Fixture::new("empty-component");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(&root, OsStr::new(""), OsStr::new("a.cht")).unwrap_err()
            ),
            DestinationSafetyFailureReason::UnsafeComponent
        );
    }

    #[test]
    fn current_directory_component_is_rejected() {
        let fixture = Fixture::new("dot-component");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(&root, OsStr::new("."), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::UnsafeComponent
        );
    }

    #[test]
    fn filename_traversal_is_rejected() {
        let fixture = Fixture::new("filename-traversal");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(&root, OsStr::new("platform"), OsStr::new(".."))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::Traversal
        );
    }

    #[test]
    fn compound_traversal_is_rejected_as_traversal() {
        let fixture = Fixture::new("compound-traversal");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(
                    &root,
                    OsStr::new("platform/../escape"),
                    OsStr::new("a.cht"),
                )
                .unwrap_err()
            ),
            DestinationSafetyFailureReason::Traversal
        );
    }

    #[test]
    fn filename_with_embedded_separator_is_rejected() {
        let fixture = Fixture::new("filename-separator");
        let root = validate_destination_root(&fixture.path).unwrap();
        assert_eq!(
            reason(
                construct_safe_destination(
                    &root,
                    OsStr::new("platform"),
                    OsStr::new("nested/a.cht"),
                )
                .unwrap_err()
            ),
            DestinationSafetyFailureReason::UnsafeComponent
        );
    }

    #[test]
    fn non_directory_parent_is_rejected() {
        let fixture = Fixture::new("parent-file");
        File::create(fixture.path.join("platform")).unwrap();
        assert_eq!(
            reason(
                assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::NonDirectoryParent
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_symlink_escaping_root_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("parent-escape");
        let external = Fixture::new("parent-escape-target");
        symlink(&external.path, fixture.path.join("platform")).unwrap();
        assert_eq!(
            reason(
                assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::ParentSymlinkEscape
        );
    }

    #[cfg(unix)]
    #[test]
    fn in_root_parent_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("parent-inside");
        fs::create_dir(fixture.path.join("actual")).unwrap();
        symlink("actual", fixture.path.join("platform")).unwrap();
        assert_eq!(
            reason(
                assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::ParentSymlink
        );
    }

    #[cfg(unix)]
    #[test]
    fn final_symlink_escaping_root_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("final-escape");
        let external = Fixture::new("final-escape-target");
        fs::create_dir(fixture.path.join("platform")).unwrap();
        let target = external.path.join("target");
        File::create(&target).unwrap();
        symlink(target, fixture.path.join("platform/a.cht")).unwrap();
        assert_eq!(
            reason(
                assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::FinalSymlinkEscape
        );
    }

    #[cfg(unix)]
    #[test]
    fn in_root_final_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("final-inside");
        fs::create_dir(fixture.path.join("platform")).unwrap();
        File::create(fixture.path.join("target")).unwrap();
        symlink("../target", fixture.path.join("platform/a.cht")).unwrap();
        assert_eq!(
            reason(
                assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::FinalSymlink
        );
    }

    #[cfg(unix)]
    #[test]
    fn broken_parent_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("broken-parent");
        symlink("missing", fixture.path.join("platform")).unwrap();
        assert_eq!(
            reason(
                assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::BrokenSymlink
        );
    }

    #[cfg(unix)]
    #[test]
    fn broken_final_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("broken-final");
        fs::create_dir(fixture.path.join("platform")).unwrap();
        symlink("missing", fixture.path.join("platform/a.cht")).unwrap();
        let error = assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
            .unwrap_err();
        assert_eq!(error.reason, DestinationSafetyFailureReason::BrokenSymlink);
        assert_eq!(error.destination_state, Some(DestinationState::Symlink));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loop_is_rejected() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new("loop");
        symlink("platform", fixture.path.join("platform")).unwrap();
        assert_eq!(
            reason(
                assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
                    .unwrap_err()
            ),
            DestinationSafetyFailureReason::SymlinkLoop
        );
    }

    #[test]
    fn destination_directory_is_rejected() {
        let fixture = Fixture::new("destination-directory");
        fs::create_dir(fixture.path.join("platform")).unwrap();
        fs::create_dir(fixture.path.join("platform/a.cht")).unwrap();
        let error = assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
            .unwrap_err();
        assert_eq!(
            error.reason,
            DestinationSafetyFailureReason::DestinationIsDirectory
        );
        assert_eq!(error.destination_state, Some(DestinationState::Directory));
    }

    #[test]
    fn regular_existing_file_is_detected() {
        let fixture = Fixture::new("regular-file");
        fs::create_dir(fixture.path.join("platform")).unwrap();
        File::create(fixture.path.join("platform/a.cht")).unwrap();
        let assessment =
            assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht")).unwrap();
        assert_eq!(assessment.destination_state, DestinationState::RegularFile);
        assert_eq!(
            assessment.inspected_parents[0].state,
            InspectedParentState::ExistingDirectory
        );
    }

    #[test]
    fn absent_destination_is_detected() {
        let fixture = Fixture::new("absent-destination");
        let assessment =
            assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht")).unwrap();
        assert_eq!(assessment.destination_state, DestinationState::Absent);
        assert_eq!(
            assessment.inspected_parents[0].state,
            InspectedParentState::Missing
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_are_preserved() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let fixture = Fixture::new("non-utf8");
        let platform = OsString::from_vec(vec![b'p', 0xff]);
        let filename = OsString::from_vec(vec![b'f', 0xfe, b'.', b'c', b'h', b't']);
        let assessment = assess_destination(&fixture.path, &platform, &filename).unwrap();
        assert_eq!(
            assessment
                .proposed_destination
                .platform_directory()
                .as_bytes(),
            platform.as_bytes()
        );
        assert_eq!(
            assessment.proposed_destination.file_name().as_bytes(),
            filename.as_bytes()
        );
    }

    #[test]
    fn validation_creates_nothing() {
        let fixture = Fixture::new("creates-nothing");
        let root = fixture.path.join("root");
        let assessment =
            assess_destination(&root, OsStr::new("platform"), OsStr::new("a.cht")).unwrap();
        assert_eq!(assessment.destination_state, DestinationState::Absent);
        assert!(!root.exists());
        assert!(!assessment.proposed_destination.path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn external_symlink_target_content_is_not_opened_or_read() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let fixture = Fixture::new("external-not-read");
        let external = Fixture::new("external-not-read-target");
        let target = external.path.join("unreadable");
        File::create(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o000)).unwrap();
        symlink(target, fixture.path.join("platform")).unwrap();
        let error = assess_destination(&fixture.path, OsStr::new("platform"), OsStr::new("a.cht"))
            .unwrap_err();
        assert_eq!(
            error.reason,
            DestinationSafetyFailureReason::ParentSymlinkEscape
        );
    }
}
