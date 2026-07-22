//! Bounded, read-only inventory of an existing RetroArch cheat directory.
//!
//! This does not parse, install, copy, delete, hash, or claim compatibility
//! for any file. It counts regular files with a `.cht` extension without
//! following symlinks and reports incomplete or unsafe observations
//! conservatively.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{DestinationRootState, DestinationSafetyFailureReason, validate_destination_root};

pub const RETROARCH_CHEAT_LIBRARY_MAX_ENTRIES: usize = 10_000;
pub const RETROARCH_CHEAT_LIBRARY_MAX_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetroArchCheatLibraryState {
    Missing,
    Available,
    Inaccessible,
    UnsafePath,
    LimitReached,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetroArchCheatLibraryInspection {
    pub path: PathBuf,
    pub state: RetroArchCheatLibraryState,
    pub approximate_cheat_file_count: usize,
    pub entries_examined: usize,
    pub complete: bool,
    pub warning: Option<String>,
}

impl RetroArchCheatLibraryInspection {
    fn terminal(path: &Path, state: RetroArchCheatLibraryState, warning: String) -> Self {
        Self {
            path: path.to_path_buf(),
            state,
            approximate_cheat_file_count: 0,
            entries_examined: 0,
            complete: false,
            warning: Some(warning),
        }
    }
}

/// Inspect an existing cheat directory using fixed entry and depth bounds.
/// Missing paths are a successful observation and are never created.
pub fn inspect_retroarch_cheat_library(path: &Path) -> RetroArchCheatLibraryInspection {
    inspect_retroarch_cheat_library_with_limits(
        path,
        RETROARCH_CHEAT_LIBRARY_MAX_ENTRIES,
        RETROARCH_CHEAT_LIBRARY_MAX_DEPTH,
    )
}

fn inspect_retroarch_cheat_library_with_limits(
    path: &Path,
    max_entries: usize,
    max_depth: usize,
) -> RetroArchCheatLibraryInspection {
    if path.parent().is_none() {
        return RetroArchCheatLibraryInspection::terminal(
            path,
            RetroArchCheatLibraryState::UnsafePath,
            "filesystem-root cheat directories are refused".to_string(),
        );
    }
    let validated = match validate_destination_root(path) {
        Ok(validated) => validated,
        Err(error) => {
            let state = if error.reason == DestinationSafetyFailureReason::InspectionFailed {
                RetroArchCheatLibraryState::Inaccessible
            } else {
                RetroArchCheatLibraryState::UnsafePath
            };
            return RetroArchCheatLibraryInspection::terminal(path, state, error.to_string());
        }
    };
    if validated.state() == DestinationRootState::Absent {
        return RetroArchCheatLibraryInspection {
            path: path.to_path_buf(),
            state: RetroArchCheatLibraryState::Missing,
            approximate_cheat_file_count: 0,
            entries_examined: 0,
            complete: true,
            warning: None,
        };
    }

    let mut pending = vec![(path.to_path_buf(), 0_usize)];
    let mut entries_examined = 0_usize;
    let mut approximate_cheat_file_count = 0_usize;
    while let Some((directory, depth)) = pending.pop() {
        if depth > max_depth {
            return RetroArchCheatLibraryInspection {
                path: path.to_path_buf(),
                state: RetroArchCheatLibraryState::LimitReached,
                approximate_cheat_file_count,
                entries_examined,
                complete: false,
                warning: Some(format!("directory depth exceeded the limit of {max_depth}")),
            };
        }
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return RetroArchCheatLibraryInspection::terminal(
                    path,
                    RetroArchCheatLibraryState::UnsafePath,
                    format!("symlinked directory refused: {}", directory.display()),
                );
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return RetroArchCheatLibraryInspection::terminal(
                    path,
                    RetroArchCheatLibraryState::UnsafePath,
                    format!(
                        "directory was replaced by another file type: {}",
                        directory.display()
                    ),
                );
            }
            Err(error) => return inaccessible(path, &directory, error),
        }

        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => return inaccessible(path, &directory, error),
        };
        let mut children = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => return inaccessible(path, &directory, error),
            };
            entries_examined = match entries_examined.checked_add(1) {
                Some(count) if count <= max_entries => count,
                _ => {
                    return RetroArchCheatLibraryInspection {
                        path: path.to_path_buf(),
                        state: RetroArchCheatLibraryState::LimitReached,
                        approximate_cheat_file_count,
                        entries_examined: max_entries,
                        complete: false,
                        warning: Some(format!("entry count exceeded the limit of {max_entries}")),
                    };
                }
            };
            let entry_path = entry.path();
            let metadata = match fs::symlink_metadata(&entry_path) {
                Ok(metadata) => metadata,
                Err(error) => return inaccessible(path, &entry_path, error),
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                children.push((entry_path, depth + 1));
            } else if metadata.is_file()
                && entry_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("cht"))
            {
                approximate_cheat_file_count = approximate_cheat_file_count.saturating_add(1);
            }
        }
        children.sort_by(|left, right| right.0.cmp(&left.0));
        pending.extend(children);
    }

    RetroArchCheatLibraryInspection {
        path: path.to_path_buf(),
        state: RetroArchCheatLibraryState::Available,
        approximate_cheat_file_count,
        entries_examined,
        complete: true,
        warning: None,
    }
}

fn inaccessible(root: &Path, entry: &Path, error: io::Error) -> RetroArchCheatLibraryInspection {
    RetroArchCheatLibraryInspection::terminal(
        root,
        RetroArchCheatLibraryState::Inaccessible,
        format!("could not inspect {}: {error}", entry.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "archivefs-retroarch-library-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn missing_destination_is_reported_without_creating_it() {
        let root = fixture("missing");
        let result = inspect_retroarch_cheat_library(&root);
        assert_eq!(result.state, RetroArchCheatLibraryState::Missing);
        assert!(result.complete);
        assert!(!root.exists());
    }

    #[test]
    fn filesystem_root_is_refused_without_traversal() {
        let root = Path::new(std::path::MAIN_SEPARATOR_STR);
        let result = inspect_retroarch_cheat_library(root);
        assert_eq!(result.state, RetroArchCheatLibraryState::UnsafePath);
        assert_eq!(result.entries_examined, 0);
    }

    #[test]
    fn existing_destination_counts_only_regular_cheat_like_files() {
        let root = fixture("existing");
        fs::create_dir_all(root.join("Nintendo")).unwrap();
        fs::write(root.join("Nintendo/a.cht"), b"cheats = 0\n").unwrap();
        fs::write(root.join("Nintendo/B.CHT"), b"cheats = 0\n").unwrap();
        fs::write(root.join("Nintendo/readme.txt"), b"notes").unwrap();
        let result = inspect_retroarch_cheat_library(&root);
        assert_eq!(result.state, RetroArchCheatLibraryState::Available);
        assert_eq!(result.approximate_cheat_file_count, 2);
        assert_eq!(result.entries_examined, 4);
        assert!(result.complete);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_destination_is_refused_without_following_it() {
        use std::os::unix::fs::symlink;

        let target = fixture("symlink-target");
        let link = fixture("symlink-link");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("secret.cht"), b"not inspected").unwrap();
        symlink(&target, &link).unwrap();
        let result = inspect_retroarch_cheat_library(&link);
        assert_eq!(result.state, RetroArchCheatLibraryState::UnsafePath);
        assert_eq!(result.entries_examined, 0);
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(target).unwrap();
    }

    #[test]
    fn inspection_is_bounded_by_directory_depth() {
        let root = fixture("depth");
        let mut current = root.clone();
        fs::create_dir_all(&current).unwrap();
        for index in 0..=RETROARCH_CHEAT_LIBRARY_MAX_DEPTH {
            current = current.join(format!("d{index}"));
            fs::create_dir(&current).unwrap();
        }
        let result = inspect_retroarch_cheat_library(&root);
        assert_eq!(result.state, RetroArchCheatLibraryState::LimitReached);
        assert!(!result.complete);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inspection_is_bounded_by_entry_count_without_large_fixtures() {
        let root = fixture("entries");
        fs::create_dir_all(&root).unwrap();
        for name in ["a.cht", "b.cht", "c.cht"] {
            fs::write(root.join(name), b"cheats = 0\n").unwrap();
        }
        let result = inspect_retroarch_cheat_library_with_limits(&root, 2, 4);
        assert_eq!(result.state, RetroArchCheatLibraryState::LimitReached);
        assert_eq!(result.entries_examined, 2);
        assert!(!result.complete);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_destination_is_reported_when_permissions_are_enforced() {
        use std::os::unix::fs::PermissionsExt;

        let root = fixture("unreadable");
        fs::create_dir_all(&root).unwrap();
        let original = fs::metadata(&root).unwrap().permissions();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
        let result = inspect_retroarch_cheat_library(&root);
        fs::set_permissions(&root, original).unwrap();
        fs::remove_dir_all(root).unwrap();

        // Root-capable test runners can bypass Unix mode bits. Everywhere
        // else this exercises the real read_dir failure path.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(result.state, RetroArchCheatLibraryState::Inaccessible);
        }
    }
}
