use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub enum ArchiveFsError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Config(String),
    CommandFailed {
        program: String,
        status: Option<i32>,
        stderr: String,
    },
}

impl fmt::Display for ArchiveFsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {}", path.display(), source),
            Self::Config(message) => write!(f, "config error: {message}"),
            Self::CommandFailed {
                program,
                status,
                stderr,
            } => {
                write!(f, "{program} failed")?;
                if let Some(code) = status {
                    write!(f, " with exit code {code}")?;
                }
                if !stderr.trim().is_empty() {
                    write!(f, ": {}", stderr.trim())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ArchiveFsError {}

pub type Result<T> = std::result::Result<T, ArchiveFsError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub source_folders: Vec<PathBuf>,
    pub mount_root: PathBuf,
    pub ratarmount_bin: String,
}

impl Config {
    pub fn load_default() -> Result<Self> {
        Self::load_from(default_config_path()?)
    }

    pub fn load_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path).map_err(|source| ArchiveFsError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        parse_config(&contents)
    }
}

pub fn default_config_path() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| ArchiveFsError::Config("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("archivefs")
        .join("config.toml"))
}

pub fn parse_config(contents: &str) -> Result<Config> {
    let mut source_folders = None;
    let mut mount_root = None;
    let mut ratarmount_bin = None;

    for (line_number, raw_line) in contents.lines().enumerate() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(ArchiveFsError::Config(format!(
                "line {} is not a key/value pair",
                line_number + 1
            )));
        };

        match key.trim() {
            "source_folders" | "sources" => {
                source_folders = Some(parse_string_array(value.trim(), line_number + 1)?);
            }
            "mount_root" => {
                mount_root = Some(PathBuf::from(parse_string(value.trim(), line_number + 1)?));
            }
            "ratarmount_bin" | "ratarmount" => {
                ratarmount_bin = Some(parse_string(value.trim(), line_number + 1)?);
            }
            _ => {}
        }
    }

    let source_folders = source_folders
        .ok_or_else(|| ArchiveFsError::Config("missing source_folders".to_string()))?;
    if source_folders.is_empty() {
        return Err(ArchiveFsError::Config(
            "source_folders must contain at least one path".to_string(),
        ));
    }

    Ok(Config {
        source_folders: source_folders.into_iter().map(PathBuf::from).collect(),
        mount_root: mount_root
            .ok_or_else(|| ArchiveFsError::Config("missing mount_root".to_string()))?,
        ratarmount_bin: ratarmount_bin.unwrap_or_else(|| "ratarmount".to_string()),
    })
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut previous_was_escape = false;

    for (index, ch) in line.char_indices() {
        match ch {
            '"' if !previous_was_escape => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
        previous_was_escape = ch == '\\' && !previous_was_escape;
        if ch != '\\' {
            previous_was_escape = false;
        }
    }

    line
}

fn parse_string(value: &str, line_number: usize) -> Result<String> {
    let value = value.trim();
    if !value.starts_with('"') || !value.ends_with('"') || value.len() < 2 {
        return Err(ArchiveFsError::Config(format!(
            "line {line_number} expected a quoted string"
        )));
    }

    Ok(value[1..value.len() - 1]
        .replace("\\\"", "\"")
        .replace("\\\\", "\\"))
}

fn parse_string_array(value: &str, line_number: usize) -> Result<Vec<String>> {
    let value = value.trim();
    if !value.starts_with('[') || !value.ends_with(']') {
        return Err(ArchiveFsError::Config(format!(
            "line {line_number} expected an array of quoted strings"
        )));
    }

    let mut values = Vec::new();
    let mut rest = value[1..value.len() - 1].trim();
    while !rest.is_empty() {
        if !rest.starts_with('"') {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} expected a quoted string in array"
            )));
        }

        let mut end = None;
        let mut previous_was_escape = false;
        for (index, ch) in rest[1..].char_indices() {
            if ch == '"' && !previous_was_escape {
                end = Some(index + 1);
                break;
            }
            previous_was_escape = ch == '\\' && !previous_was_escape;
            if ch != '\\' {
                previous_was_escape = false;
            }
        }

        let Some(end) = end else {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} has an unterminated string"
            )));
        };

        values.push(parse_string(&rest[..=end], line_number)?);
        rest = rest[end + 1..].trim_start();
        if let Some(after_comma) = rest.strip_prefix(',') {
            rest = after_comma.trim_start();
        } else if !rest.is_empty() {
            return Err(ArchiveFsError::Config(format!(
                "line {line_number} expected ',' between array values"
            )));
        }
    }

    Ok(values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    SevenZip,
    Rar,
}

pub fn archive_kind(path: impl AsRef<Path>) -> Option<ArchiveKind> {
    let filename = path.as_ref().file_name()?.to_string_lossy().to_lowercase();
    if should_skip_split_archive_part(&filename) {
        return None;
    }

    if filename.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if filename.ends_with(".7z") {
        Some(ArchiveKind::SevenZip)
    } else if filename.ends_with(".rar") {
        Some(ArchiveKind::Rar)
    } else {
        None
    }
}

pub fn is_supported_archive(path: impl AsRef<Path>) -> bool {
    archive_kind(path).is_some()
}

pub fn should_skip_split_archive_part(path: impl AsRef<Path>) -> bool {
    let Some(filename) = path.as_ref().file_name() else {
        return false;
    };
    let filename = filename.to_string_lossy().to_lowercase();

    if let Some(part_number) = rar_part_number(&filename) {
        return part_number != 1;
    }

    let Some(extension) = Path::new(filename.as_str()).extension() else {
        return false;
    };
    let extension = extension.to_string_lossy();
    extension.len() == 3
        && extension.starts_with('r')
        && extension[1..].chars().all(|ch| ch.is_ascii_digit())
}

fn rar_part_number(filename: &str) -> Option<u32> {
    let without_rar = filename.strip_suffix(".rar")?;
    let (_, part) = without_rar.rsplit_once(".part")?;
    if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMount {
    pub archive_path: PathBuf,
    pub mount_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveState {
    Pending,
    Mounted,
    MountPathExists,
}

impl fmt::Display for ArchiveState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Mounted => write!(f, "Mounted"),
            Self::MountPathExists => write!(f, "MountPathExists"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveStatus {
    pub archive_path: PathBuf,
    pub mount_path: PathBuf,
    pub state: ArchiveState,
}

pub fn scan_archives(config: &Config) -> Result<Vec<PathBuf>> {
    let mut archives = Vec::new();
    for source in &config.source_folders {
        scan_source(source, &mut archives)?;
    }
    archives.sort();
    archives.dedup();
    Ok(archives)
}

fn scan_source(source: &Path, archives: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(source).map_err(|source_error| ArchiveFsError::Io {
        path: source.to_path_buf(),
        source: source_error,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source_error| ArchiveFsError::Io {
            path: source.to_path_buf(),
            source: source_error,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source_error| ArchiveFsError::Io {
                path: path.clone(),
                source: source_error,
            })?;

        if file_type.is_dir() {
            scan_source(&path, archives)?;
        } else if file_type.is_file() && is_supported_archive(&path) {
            archives.push(path);
        }
    }

    Ok(())
}

pub fn plan_mounts(archives: &[PathBuf], mount_root: impl AsRef<Path>) -> Vec<ArchiveMount> {
    let mount_root = mount_root.as_ref();
    let mut base_counts = HashMap::<String, usize>::new();
    for archive in archives {
        *base_counts.entry(safe_mount_name(archive)).or_default() += 1;
    }

    let mut used = HashSet::new();
    archives
        .iter()
        .map(|archive| {
            let base = safe_mount_name(archive);
            let mut name = if base_counts.get(&base).copied().unwrap_or(0) > 1 {
                format!("{base}--{}", short_path_hash(archive))
            } else {
                base
            };

            if used.contains(&name) {
                name = format!("{name}--{}", short_path_hash(archive));
            }
            let mut suffix = 2;
            while used.contains(&name) {
                name = format!("{}-{suffix}", safe_mount_name(archive));
                suffix += 1;
            }
            used.insert(name.clone());

            ArchiveMount {
                archive_path: archive.clone(),
                mount_path: mount_root.join(name),
            }
        })
        .collect()
}

pub fn safe_mount_name(path: impl AsRef<Path>) -> String {
    let base = archive_title(path.as_ref());
    let mut safe = String::new();
    let mut previous_was_separator = false;

    for ch in base.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            ch
        } else {
            '_'
        };

        if mapped == '_' {
            if !previous_was_separator {
                safe.push(mapped);
            }
            previous_was_separator = true;
        } else {
            safe.push(mapped);
            previous_was_separator = false;
        }
    }

    let safe = safe.trim_matches(['.', '-', '_']).to_string();
    if safe.is_empty() {
        "archive".to_string()
    } else {
        safe
    }
}

fn archive_title(path: &Path) -> String {
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive".to_string());
    let lower = filename.to_lowercase();

    if let Some(part_number) = rar_part_number(&lower) {
        if part_number == 1 {
            let suffix_len = ".part1.rar".len();
            let part_digits = lower
                .strip_suffix(".rar")
                .and_then(|name| name.rsplit_once(".part"))
                .map(|(_, digits)| digits.len())
                .unwrap_or(1);
            return filename[..filename.len() - suffix_len + 1 - part_digits].to_string();
        }
    }

    for extension in [".zip", ".7z", ".rar"] {
        if lower.ends_with(extension) {
            return filename[..filename.len() - extension.len()].to_string();
        }
    }

    filename
}

fn short_path_hash(path: &Path) -> String {
    let mut hasher = FnvHasher::default();
    path.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

#[derive(Default)]
struct FnvHasher(u64);

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.0 == 0 {
            self.0 = 0xcbf29ce484222325;
        }
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

pub fn current_statuses(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let archives = scan_archives(config)?;
    let mounts = plan_mounts(&archives, &config.mount_root);
    let mounted_paths = mounted_paths_under(&config.mount_root)?;

    Ok(mounts
        .into_iter()
        .map(|mount| {
            let state = if mounted_paths.contains(&mount.mount_path) {
                ArchiveState::Mounted
            } else if mount.mount_path.exists() {
                ArchiveState::MountPathExists
            } else {
                ArchiveState::Pending
            };

            ArchiveStatus {
                archive_path: mount.archive_path,
                mount_path: mount.mount_path,
                state,
            }
        })
        .collect())
}

pub fn mount_archives(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let archives = scan_archives(config)?;
    let mounts = plan_mounts(&archives, &config.mount_root);
    fs::create_dir_all(&config.mount_root).map_err(|source| ArchiveFsError::Io {
        path: config.mount_root.clone(),
        source,
    })?;

    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    for mount in &mounts {
        if mounted_paths.contains(&mount.mount_path) {
            continue;
        }
        fs::create_dir_all(&mount.mount_path).map_err(|source| ArchiveFsError::Io {
            path: mount.mount_path.clone(),
            source,
        })?;
        run_command(
            &config.ratarmount_bin,
            &[mount.archive_path.as_path(), mount.mount_path.as_path()],
        )?;
    }

    current_statuses(config)
}

pub fn unmount_archives(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    let mut mounted_paths = mounted_paths.into_iter().collect::<Vec<_>>();
    mounted_paths.sort();
    mounted_paths.reverse();

    for mount_path in mounted_paths {
        if path_is_under(&mount_path, &config.mount_root) {
            unmount_path(&mount_path)?;
        }
    }

    current_statuses(config)
}

fn mounted_paths_under(root: &Path) -> Result<HashSet<PathBuf>> {
    let mountinfo =
        fs::read_to_string("/proc/self/mountinfo").map_err(|source| ArchiveFsError::Io {
            path: PathBuf::from("/proc/self/mountinfo"),
            source,
        })?;

    Ok(mountinfo
        .lines()
        .filter_map(mount_path_from_mountinfo_line)
        .filter(|path| path_is_under(path, root))
        .collect())
}

fn mount_path_from_mountinfo_line(line: &str) -> Option<PathBuf> {
    let mut fields = line.split_whitespace();
    fields
        .nth(4)
        .map(unescape_mountinfo_path)
        .map(PathBuf::from)
}

fn unescape_mountinfo_path(path: &str) -> String {
    path.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn unmount_path(path: &Path) -> Result<()> {
    for program in ["fusermount3", "fusermount", "umount"] {
        match run_command(program, &[path]) {
            Ok(()) => return Ok(()),
            Err(ArchiveFsError::CommandFailed { .. }) => continue,
            Err(ArchiveFsError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => return Err(error),
        }
    }

    Err(ArchiveFsError::CommandFailed {
        program: "fusermount3/fusermount/umount".to_string(),
        status: None,
        stderr: format!("failed to unmount {}", path.display()),
    })
}

fn run_command(program: &str, args: &[&Path]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|source| ArchiveFsError::Io {
            path: PathBuf::from(program),
            source,
        })?;

    if output.status.success() {
        Ok(())
    } else {
        Err(ArchiveFsError::CommandFailed {
            program: program.to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_supported_archive_extensions_case_insensitively() {
        assert_eq!(archive_kind("game.zip"), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind("game.7Z"), Some(ArchiveKind::SevenZip));
        assert_eq!(archive_kind("game.RAR"), Some(ArchiveKind::Rar));
        assert_eq!(archive_kind("game.iso"), None);
        assert_eq!(archive_kind("game.zip.tmp"), None);
    }

    #[test]
    fn skips_split_rar_parts_except_main_parts() {
        assert!(!should_skip_split_archive_part("game.rar"));
        assert!(!should_skip_split_archive_part("game.part1.rar"));
        assert!(!should_skip_split_archive_part("game.part01.rar"));
        assert!(should_skip_split_archive_part("game.part2.rar"));
        assert!(should_skip_split_archive_part("game.part10.rar"));
        assert!(should_skip_split_archive_part("game.r00"));
        assert!(should_skip_split_archive_part("game.r99"));
        assert_eq!(archive_kind("game.part2.rar"), None);
        assert_eq!(archive_kind("game.part1.rar"), Some(ArchiveKind::Rar));
    }

    #[test]
    fn generates_safe_mount_names() {
        assert_eq!(
            safe_mount_name("/tmp/Resident Evil 2.zip"),
            "Resident_Evil_2"
        );
        assert_eq!(safe_mount_name("/tmp/../../!!!.7z"), "archive");
        assert_eq!(
            safe_mount_name("/tmp/Metal: Gear? Solid.rar"),
            "Metal_Gear_Solid"
        );
        assert_eq!(safe_mount_name("/tmp/Game.part1.rar"), "Game");
    }

    #[test]
    fn duplicate_filenames_get_distinct_mount_paths() {
        let archives = vec![
            PathBuf::from("/roms/ps1/game.zip"),
            PathBuf::from("/roms/ps2/game.zip"),
        ];
        let mounts = plan_mounts(&archives, "/mnt/archivefs");

        assert_eq!(mounts.len(), 2);
        assert_ne!(mounts[0].mount_path, mounts[1].mount_path);
        assert!(
            mounts[0]
                .mount_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("game--")
        );
        assert!(
            mounts[1]
                .mount_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("game--")
        );
    }
}
