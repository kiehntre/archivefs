//! Bounded, read-only inventory of an existing RetroArch cheat directory.
//!
//! This does not parse, install, copy, delete, hash, or claim compatibility
//! for any file. It counts regular files with a `.cht` extension without
//! following symlinks and reports incomplete or unsafe observations
//! conservatively.

use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::{DestinationRootState, DestinationSafetyFailureReason, validate_destination_root};
use crate::emulator_environment::EncodedPath;

pub const RETROARCH_CHEAT_LIBRARY_MAX_ENTRIES: usize = 10_000;
pub const RETROARCH_CHEAT_LIBRARY_MAX_DEPTH: usize = 16;
pub const RETROARCH_LOCAL_MAX_DIRECTORIES: usize = 16;
pub const RETROARCH_LOCAL_MAX_FILES: usize = 256;
pub const RETROARCH_LOCAL_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const RETROARCH_LOCAL_MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetroArchCheatLibraryState {
    Missing,
    Available,
    Inaccessible,
    UnsafePath,
    LimitReached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetroArchLocalCheatMatchState {
    NotTargeted,
    NotFound,
    Candidate,
    ExactLocalFile,
    Ambiguous,
    LimitReached,
    Unsafe,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetroArchCheatLibraryInspection {
    pub path: PathBuf,
    pub state: RetroArchCheatLibraryState,
    pub approximate_cheat_file_count: usize,
    pub entries_examined: usize,
    pub complete: bool,
    pub warning: Option<String>,
    pub match_state: RetroArchLocalCheatMatchState,
    pub matching_files: Vec<EncodedPath>,
    pub directories_inspected: usize,
    pub bytes_inspected: u64,
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
            match_state: RetroArchLocalCheatMatchState::Unavailable,
            matching_files: Vec::new(),
            directories_inspected: 0,
            bytes_inspected: 0,
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
            match_state: RetroArchLocalCheatMatchState::NotTargeted,
            matching_files: Vec::new(),
            directories_inspected: 0,
            bytes_inspected: 0,
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
                match_state: RetroArchLocalCheatMatchState::LimitReached,
                matching_files: Vec::new(),
                directories_inspected: 0,
                bytes_inspected: 0,
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
                        match_state: RetroArchLocalCheatMatchState::LimitReached,
                        matching_files: Vec::new(),
                        directories_inspected: 0,
                        bytes_inspected: 0,
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
        match_state: RetroArchLocalCheatMatchState::NotTargeted,
        matching_files: Vec::new(),
        directories_inspected: 0,
        bytes_inspected: 0,
    }
}

/// Inspect only the selected platform's immediate cheat directories and the
/// selected title's plausible `.cht` files. No recursive whole-tree walk is
/// performed and local content remains unverified compatibility evidence.
pub fn inspect_retroarch_cheat_library_for_game(
    root: &Path,
    platform: &str,
    display_title: &str,
) -> RetroArchCheatLibraryInspection {
    inspect_retroarch_cheat_library_for_game_with_limits(
        root,
        platform,
        display_title,
        RETROARCH_LOCAL_MAX_DIRECTORIES,
        RETROARCH_LOCAL_MAX_FILES,
        RETROARCH_LOCAL_MAX_TOTAL_BYTES,
    )
}

fn inspect_retroarch_cheat_library_for_game_with_limits(
    root: &Path,
    platform: &str,
    display_title: &str,
    max_directories: usize,
    max_files: usize,
    max_total_bytes: u64,
) -> RetroArchCheatLibraryInspection {
    let base = inspect_target_root(root);
    if base.state != RetroArchCheatLibraryState::Available {
        return base;
    }
    let aliases = platform_directory_aliases(platform);
    if aliases.is_empty() {
        return targeted_terminal(
            root,
            RetroArchCheatLibraryState::Available,
            RetroArchLocalCheatMatchState::Unavailable,
            "selected platform has no reviewed RetroArch cheat-directory aliases",
        );
    }

    let mut directories = Vec::new();
    if root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| aliases.iter().any(|alias| exact_alias(name, alias)))
    {
        directories.push(root.to_path_buf());
    } else {
        let entries = match sorted_directory_entries(root) {
            Ok(entries) => entries,
            Err(error) => return inaccessible(root, root, error),
        };
        for path in entries {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !aliases.iter().any(|alias| exact_alias(name, alias)) {
                continue;
            }
            if directories.len() >= max_directories {
                return targeted_limit(root, directories.len(), 0, 0, "directory limit reached");
            }
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return targeted_terminal(
                        root,
                        RetroArchCheatLibraryState::UnsafePath,
                        RetroArchLocalCheatMatchState::Unsafe,
                        "symlinked platform directory refused",
                    );
                }
                Ok(metadata) if metadata.is_dir() => directories.push(path),
                Ok(_) => continue,
                Err(error) => return inaccessible(root, &path, error),
            }
        }
    }
    directories.sort();
    if directories.is_empty() {
        let mut result = base;
        result.match_state = RetroArchLocalCheatMatchState::NotFound;
        return result;
    }

    let selected_exact = display_title.trim();
    let selected_normalized = normalize_title(display_title);
    let mut exact = Vec::new();
    let mut candidates = Vec::new();
    let mut files_inspected = 0_usize;
    let mut bytes_inspected = 0_u64;
    for directory in &directories {
        for path in match sorted_directory_entries(directory) {
            Ok(entries) => entries,
            Err(error) => return inaccessible(root, directory, error),
        } {
            if files_inspected >= max_files {
                return targeted_limit(
                    root,
                    directories.len(),
                    files_inspected,
                    bytes_inspected,
                    "file inspection limit reached",
                );
            }
            files_inspected += 1;
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => return inaccessible(root, &path, error),
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if !metadata.is_file() || !extension_is_cht(&path) {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let is_exact = stem.eq_ignore_ascii_case(selected_exact);
            let is_candidate = normalize_title(stem) == selected_normalized;
            if !is_exact && !is_candidate {
                continue;
            }
            if metadata.len() > RETROARCH_LOCAL_MAX_FILE_BYTES {
                return targeted_limit(
                    root,
                    directories.len(),
                    files_inspected,
                    bytes_inspected,
                    "matching local cheat file exceeds the per-file byte limit",
                );
            }
            bytes_inspected = match bytes_inspected.checked_add(metadata.len()) {
                Some(total) if total <= max_total_bytes => total,
                _ => {
                    return targeted_limit(
                        root,
                        directories.len(),
                        files_inspected,
                        bytes_inspected,
                        "local cheat byte limit reached",
                    );
                }
            };
            if let Err(error) = read_regular_file_bounded(&path, metadata.len()) {
                return targeted_terminal(
                    root,
                    RetroArchCheatLibraryState::UnsafePath,
                    RetroArchLocalCheatMatchState::Unsafe,
                    &error,
                );
            }
            if is_exact {
                exact.push(path);
            } else {
                candidates.push(path);
            }
        }
    }
    exact.sort();
    candidates.sort();
    let (match_state, matching_files) = if exact.len() == 1 && candidates.is_empty() {
        (RetroArchLocalCheatMatchState::ExactLocalFile, exact)
    } else if exact.len() + candidates.len() > 1 {
        exact.extend(candidates);
        (RetroArchLocalCheatMatchState::Ambiguous, exact)
    } else if exact.is_empty() && candidates.len() == 1 {
        (RetroArchLocalCheatMatchState::Candidate, candidates)
    } else {
        (RetroArchLocalCheatMatchState::NotFound, Vec::new())
    };
    let matching_files: Vec<EncodedPath> = matching_files
        .iter()
        .map(|path| EncodedPath::from_path(path))
        .collect();
    RetroArchCheatLibraryInspection {
        path: root.to_path_buf(),
        state: RetroArchCheatLibraryState::Available,
        approximate_cheat_file_count: matching_files.len(),
        entries_examined: files_inspected,
        complete: true,
        warning: None,
        match_state,
        matching_files,
        directories_inspected: directories.len(),
        bytes_inspected,
    }
}

fn inspect_target_root(root: &Path) -> RetroArchCheatLibraryInspection {
    if root.parent().is_none() {
        return targeted_terminal(
            root,
            RetroArchCheatLibraryState::UnsafePath,
            RetroArchLocalCheatMatchState::Unsafe,
            "filesystem-root cheat directories are refused",
        );
    }
    match validate_destination_root(root) {
        Ok(validated) if validated.state() == DestinationRootState::Absent => {
            let mut result = RetroArchCheatLibraryInspection::terminal(
                root,
                RetroArchCheatLibraryState::Missing,
                "configured cheat root is absent".to_string(),
            );
            result.complete = true;
            result.match_state = RetroArchLocalCheatMatchState::NotFound;
            result
        }
        Ok(_) => RetroArchCheatLibraryInspection {
            path: root.to_path_buf(),
            state: RetroArchCheatLibraryState::Available,
            approximate_cheat_file_count: 0,
            entries_examined: 0,
            complete: true,
            warning: None,
            match_state: RetroArchLocalCheatMatchState::NotFound,
            matching_files: Vec::new(),
            directories_inspected: 0,
            bytes_inspected: 0,
        },
        Err(error) => targeted_terminal(
            root,
            if error.reason == DestinationSafetyFailureReason::InspectionFailed {
                RetroArchCheatLibraryState::Inaccessible
            } else {
                RetroArchCheatLibraryState::UnsafePath
            },
            RetroArchLocalCheatMatchState::Unsafe,
            &error.to_string(),
        ),
    }
}

fn targeted_terminal(
    root: &Path,
    state: RetroArchCheatLibraryState,
    match_state: RetroArchLocalCheatMatchState,
    warning: &str,
) -> RetroArchCheatLibraryInspection {
    let mut result = RetroArchCheatLibraryInspection::terminal(root, state, warning.to_string());
    result.match_state = match_state;
    result
}

fn targeted_limit(
    root: &Path,
    directories: usize,
    files: usize,
    bytes: u64,
    warning: &str,
) -> RetroArchCheatLibraryInspection {
    RetroArchCheatLibraryInspection {
        path: root.to_path_buf(),
        state: RetroArchCheatLibraryState::LimitReached,
        approximate_cheat_file_count: 0,
        entries_examined: files,
        complete: false,
        warning: Some(warning.to_string()),
        match_state: RetroArchLocalCheatMatchState::LimitReached,
        matching_files: Vec::new(),
        directories_inspected: directories,
        bytes_inspected: bytes,
    }
}

fn platform_directory_aliases(platform: &str) -> &'static [&'static str] {
    let normalized: String = platform
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match normalized.as_str() {
        "megadrive" | "genesis" | "segamegadrive" | "segagenesis" => &[
            "Sega - Mega Drive - Genesis",
            "MegaDrive",
            "Mega Drive",
            "Genesis",
        ],
        "snes" | "supernintendo" | "supernintendoentertainmentsystem" => &[
            "Nintendo - Super Nintendo Entertainment System",
            "SNES",
            "Super Nintendo Entertainment System",
            "Super Famicom",
        ],
        _ => &[],
    }
}

fn exact_alias(value: &str, alias: &str) -> bool {
    value.eq_ignore_ascii_case(alias)
}

fn sorted_directory_entries(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

fn extension_is_cht(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cht"))
}

fn normalize_title(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut separator = true;
    for character in value.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
            separator = false;
        } else if !separator {
            normalized.push(' ');
            separator = true;
        }
    }
    normalized.truncate(normalized.trim_end().len());
    normalized
}

fn read_regular_file_bounded(path: &Path, expected_size: u64) -> Result<(), String> {
    let before = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err("matching local cheat is not a regular non-symlink file".to_string());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    validate_open_identity(&before, &file)?;
    let mut remaining = expected_size;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file
            .read(&mut buffer[..wanted])
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("matching local cheat changed while being inspected".to_string());
        }
        remaining -= read as u64;
    }
    let mut extra = [0_u8; 1];
    if file.read(&mut extra).map_err(|error| error.to_string())? != 0 {
        return Err("matching local cheat changed while being inspected".to_string());
    }
    let after = file.metadata().map_err(|error| error.to_string())?;
    if after.len() != expected_size || after.modified().ok() != before.modified().ok() {
        return Err("matching local cheat changed while being inspected".to_string());
    }
    Ok(())
}

fn validate_open_identity(before: &fs::Metadata, file: &File) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = file.metadata().map_err(|error| error.to_string())?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err("matching local cheat changed while being opened".to_string());
        }
    }
    Ok(())
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
    fn production_local_inspector_has_no_write_execution_or_network_path() {
        let production = include_str!("retroarch_cheat_library.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "File::create",
            "fs::write",
            "create_dir",
            "remove_dir",
            "Command::",
            "std::process",
            "TcpStream",
            "ureq::",
            "http://",
            "https://",
        ] {
            assert!(
                !production.contains(forbidden),
                "forbidden production path: {forbidden}"
            );
        }
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

    #[test]
    fn targeted_inspection_ignores_unrelated_platform_trees() {
        let root = fixture("targeted-platform");
        let mega = root.join("Sega - Mega Drive - Genesis");
        let unrelated = root.join("Nintendo - Super Nintendo Entertainment System");
        fs::create_dir_all(&mega).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(mega.join("Alien 3 (USA, Europe).cht"), b"cheats = 0\n").unwrap();
        for index in 0..32 {
            fs::write(unrelated.join(format!("Unrelated {index}.cht")), b"ignored").unwrap();
        }
        let result =
            inspect_retroarch_cheat_library_for_game(&root, "MegaDrive", "Alien 3 (USA, Europe)");
        assert_eq!(
            result.match_state,
            RetroArchLocalCheatMatchState::ExactLocalFile
        );
        assert_eq!(result.directories_inspected, 1);
        assert_eq!(result.entries_examined, 1);
        assert_eq!(result.matching_files.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn targeted_inspection_reports_candidate_ambiguity_and_limits() {
        let root = fixture("targeted-ambiguity");
        let mega = root.join("Genesis");
        fs::create_dir_all(&mega).unwrap();
        fs::write(mega.join("Alien-3-USA-Europe.cht"), b"one").unwrap();
        let candidate =
            inspect_retroarch_cheat_library_for_game(&root, "MegaDrive", "Alien 3 USA Europe");
        assert_eq!(
            candidate.match_state,
            RetroArchLocalCheatMatchState::Candidate
        );

        fs::write(mega.join("Alien_3_USA_Europe.CHT"), b"two").unwrap();
        let ambiguous =
            inspect_retroarch_cheat_library_for_game(&root, "MegaDrive", "Alien 3 USA Europe");
        assert_eq!(
            ambiguous.match_state,
            RetroArchLocalCheatMatchState::Ambiguous
        );
        assert_eq!(ambiguous.matching_files.len(), 2);

        let limited = inspect_retroarch_cheat_library_for_game_with_limits(
            &root,
            "MegaDrive",
            "Alien 3 USA Europe",
            1,
            1,
            RETROARCH_LOCAL_MAX_TOTAL_BYTES,
        );
        assert_eq!(
            limited.match_state,
            RetroArchLocalCheatMatchState::LimitReached
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn targeted_inspection_refuses_symlinked_platform_directory() {
        use std::os::unix::fs::symlink;

        let root = fixture("targeted-symlink");
        let outside = fixture("targeted-symlink-outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("Alien 3.cht"), b"secret").unwrap();
        symlink(&outside, root.join("Genesis")).unwrap();
        let result = inspect_retroarch_cheat_library_for_game(&root, "MegaDrive", "Alien 3");
        assert_eq!(result.match_state, RetroArchLocalCheatMatchState::Unsafe);
        assert_eq!(result.entries_examined, 0);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
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
