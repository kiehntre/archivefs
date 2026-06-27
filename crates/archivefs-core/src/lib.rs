use std::collections::{BTreeMap, HashMap, HashSet};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Pass,
    Warn,
    Fail,
}

impl fmt::Display for DoctorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pass => write!(f, "PASS"),
            Self::Warn => write!(f, "WARN"),
            Self::Fail => write!(f, "FAIL"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub config_path: PathBuf,
    pub checks: Vec<DoctorCheck>,
    pub archives_found: usize,
    pub archives_with_platform: usize,
    pub archives_unknown_platform: usize,
    pub unknown_platform_examples: Vec<PathBuf>,
    pub platform_counts: Vec<(String, usize)>,
    pub pending_archives: usize,
    pub mounted_archives: usize,
}

impl DoctorReport {
    pub fn is_ready(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|check| check.status == DoctorStatus::Fail)
    }

    fn pass(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(DoctorCheck {
            name: name.into(),
            status: DoctorStatus::Pass,
            detail: detail.into(),
        });
    }

    fn warn(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(DoctorCheck {
            name: name.into(),
            status: DoctorStatus::Warn,
            detail: detail.into(),
        });
    }

    fn fail(&mut self, name: impl Into<String>, detail: impl Into<String>) {
        self.checks.push(DoctorCheck {
            name: name.into(),
            status: DoctorStatus::Fail,
            detail: detail.into(),
        });
    }
}

pub fn run_doctor_default() -> DoctorReport {
    match default_config_path() {
        Ok(path) => run_doctor(path),
        Err(error) => DoctorReport {
            config_path: PathBuf::from("~/.config/archivefs/config.toml"),
            checks: vec![DoctorCheck {
                name: "config path".to_string(),
                status: DoctorStatus::Fail,
                detail: error.to_string(),
            }],
            archives_found: 0,
            archives_with_platform: 0,
            archives_unknown_platform: 0,
            unknown_platform_examples: Vec::new(),
            platform_counts: Vec::new(),
            pending_archives: 0,
            mounted_archives: 0,
        },
    }
}

pub fn run_doctor(config_path: impl AsRef<Path>) -> DoctorReport {
    let config_path = config_path.as_ref().to_path_buf();
    let mut report = DoctorReport {
        config_path: config_path.clone(),
        checks: Vec::new(),
        archives_found: 0,
        archives_with_platform: 0,
        archives_unknown_platform: 0,
        unknown_platform_examples: Vec::new(),
        platform_counts: Vec::new(),
        pending_archives: 0,
        mounted_archives: 0,
    };

    if config_path.exists() {
        report.pass("config file", format!("found {}", config_path.display()));
    } else {
        report.fail("config file", format!("missing {}", config_path.display()));
        return report;
    }

    let config = match Config::load_from(&config_path) {
        Ok(config) => {
            report.pass("config parses", "configuration parsed successfully");
            config
        }
        Err(error) => {
            report.fail("config parses", error.to_string());
            return report;
        }
    };

    let mut sources_ok = true;
    for source in &config.source_folders {
        if source.is_dir() {
            report.pass("source folder", format!("{} exists", source.display()));
        } else {
            sources_ok = false;
            report.fail(
                "source folder",
                format!("{} does not exist or is not a directory", source.display()),
            );
        }
    }

    if config.mount_root.is_dir() {
        report.pass(
            "mount root",
            format!("{} exists", config.mount_root.display()),
        );
    } else if config.mount_root.exists() {
        report.fail(
            "mount root",
            format!(
                "{} exists but is not a directory",
                config.mount_root.display()
            ),
        );
    } else {
        match fs::create_dir_all(&config.mount_root) {
            Ok(()) => report.pass(
                "mount root",
                format!("{} was created", config.mount_root.display()),
            ),
            Err(error) => report.fail(
                "mount root",
                format!("{} cannot be created: {error}", config.mount_root.display()),
            ),
        }
    }

    if command_available(&config.ratarmount_bin) {
        report.pass(
            "ratarmount",
            format!("{} is available", config.ratarmount_bin),
        );
    } else {
        report.fail(
            "ratarmount",
            format!("{} was not found", config.ratarmount_bin),
        );
    }

    if command_available("fusermount3") || command_available("umount") {
        report.pass("unmount tool", "fusermount3 or umount is available");
    } else {
        report.fail("unmount tool", "neither fusermount3 nor umount was found");
    }

    if sources_ok {
        match scan_archives(&config) {
            Ok(archives) => {
                report.archives_found = archives.len();
                let mut platform_counts = BTreeMap::<String, usize>::new();
                for archive in &archives {
                    if let Some(platform) = &archive.identity.platform {
                        *platform_counts.entry(platform.clone()).or_default() += 1;
                    } else {
                        report.archives_unknown_platform += 1;
                        if report.unknown_platform_examples.len() < 10 {
                            report.unknown_platform_examples.push(archive.path.clone());
                        }
                    }
                }
                report.archives_with_platform = archives.len() - report.archives_unknown_platform;
                report.platform_counts = platform_counts.into_iter().collect();
                report.pass("archive scan", format!("{} archives found", archives.len()));
            }
            Err(error) => report.fail("archive scan", error.to_string()),
        }
    } else {
        report.warn(
            "archive scan",
            "skipped because one or more source folders are unavailable",
        );
    }

    match current_statuses(&config) {
        Ok(statuses) => {
            report.pending_archives = statuses
                .iter()
                .filter(|status| status.state == MountState::Pending)
                .count();
            report.mounted_archives = statuses
                .iter()
                .filter(|status| status.state == MountState::Mounted)
                .count();
            report.pass(
                "mount status",
                format!(
                    "{} pending, {} mounted",
                    report.pending_archives, report.mounted_archives
                ),
            );
        }
        Err(error) if sources_ok => report.fail("mount status", error.to_string()),
        Err(_) => report.warn(
            "mount status",
            "skipped because one or more source folders are unavailable",
        ),
    }

    report
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveKind {
    Zip,
    SevenZip,
    Rar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveHealth {
    Pending,
    Mounted,
    Failed,
    MissingParts,
    Corrupt,
    Unsupported,
    PermissionDenied,
    RetryAvailable,
}

impl ArchiveHealth {
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            Self::Failed | Self::MissingParts | Self::RetryAvailable
        )
    }

    pub fn is_terminal_without_source_change(self) -> bool {
        matches!(
            self,
            Self::Corrupt | Self::Unsupported | Self::PermissionDenied
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArchiveIdentity {
    pub display_name: String,
    pub normalized_name: String,
    pub source_root: PathBuf,
    pub size_bytes: Option<u64>,
    pub modified_time: Option<std::time::SystemTime>,
    pub platform: Option<String>,
    pub region: Option<String>,
    pub content_hash: Option<String>,
    pub archive_hash: Option<String>,
    pub internal_listing_hash: Option<String>,
}

impl ArchiveIdentity {
    pub fn from_path(
        path: &Path,
        source_root: impl Into<PathBuf>,
        metadata: Option<&fs::Metadata>,
    ) -> Self {
        let source_root = source_root.into();
        let platform = detect_platform(path, &source_root);
        Self {
            display_name: archive_title(path),
            normalized_name: normalized_title(path),
            source_root,
            size_bytes: metadata.map(fs::Metadata::len),
            modified_time: metadata.and_then(|metadata| metadata.modified().ok()),
            platform,
            region: None,
            content_hash: None,
            archive_hash: None,
            internal_listing_hash: None,
        }
    }

    fn path_fingerprint(&self, archive_path: &Path) -> String {
        let mut input = self.source_root.clone();
        input.push(archive_path);
        short_path_hash(&input)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub path: PathBuf,
    pub kind: ArchiveKind,
    pub identity: ArchiveIdentity,
    pub health: ArchiveHealth,
}

impl Archive {
    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        Self::from_path_in_root(path, PathBuf::new())
    }

    pub fn from_path_in_root(
        path: impl AsRef<Path>,
        source_root: impl Into<PathBuf>,
    ) -> Option<Self> {
        let path = path.as_ref();
        let kind = archive_kind(path)?;
        let metadata = fs::metadata(path).ok();
        Some(Self {
            path: path.to_path_buf(),
            kind,
            identity: ArchiveIdentity::from_path(path, source_root, metadata.as_ref()),
            health: ArchiveHealth::Pending,
        })
    }
}

impl AsRef<Path> for Archive {
    fn as_ref(&self) -> &Path {
        &self.path
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountState {
    Pending,
    Mounted,
    MountPathExists,
}

impl fmt::Display for MountState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Mounted => write!(f, "Mounted"),
            Self::MountPathExists => write!(f, "MountPathExists"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPlan {
    pub archive: Archive,
    pub mount_path: PathBuf,
    pub state: MountState,
}

impl MountPlan {
    pub fn new(archive: Archive, mount_path: PathBuf) -> Self {
        Self {
            archive,
            mount_path,
            state: MountState::Pending,
        }
    }
}

pub trait MountBackend {
    fn mount(&self, plan: &MountPlan) -> Result<()>;
    fn unmount(&self, mount_path: &Path) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatarmountBackend {
    ratarmount_bin: String,
}

impl RatarmountBackend {
    pub fn new(ratarmount_bin: impl Into<String>) -> Self {
        Self {
            ratarmount_bin: ratarmount_bin.into(),
        }
    }
}

impl MountBackend for RatarmountBackend {
    fn mount(&self, plan: &MountPlan) -> Result<()> {
        run_command(
            &self.ratarmount_bin,
            &[plan.archive.path.as_path(), plan.mount_path.as_path()],
        )
    }

    fn unmount(&self, mount_path: &Path) -> Result<()> {
        unmount_path(mount_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveStatus {
    pub archive_path: PathBuf,
    pub mount_path: PathBuf,
    pub state: MountState,
}

pub fn scan_archives(config: &Config) -> Result<Vec<Archive>> {
    let mut archives = Vec::new();
    for source in &config.source_folders {
        scan_source(source, source, &mut archives)?;
    }
    archives.sort_by(|left, right| left.path.cmp(&right.path));
    archives.dedup_by(|left, right| left.path == right.path);
    Ok(archives)
}

fn scan_source(source_root: &Path, source: &Path, archives: &mut Vec<Archive>) -> Result<()> {
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
            scan_source(source_root, &path, archives)?;
        } else if file_type.is_file() {
            if let Some(archive) = Archive::from_path_in_root(&path, source_root) {
                archives.push(archive);
            }
        }
    }

    Ok(())
}

pub fn plan_mounts(archives: &[Archive], mount_root: impl AsRef<Path>) -> Vec<MountPlan> {
    let mount_root = mount_root.as_ref();
    let mut base_counts = HashMap::<String, usize>::new();
    for archive in archives {
        *base_counts
            .entry(safe_mount_name(&archive.path))
            .or_default() += 1;
    }

    let mut used = HashSet::new();
    archives
        .iter()
        .map(|archive| {
            let base = safe_mount_name(&archive.path);
            let mut name = if base_counts.get(&base).copied().unwrap_or(0) > 1 {
                format!(
                    "{base}--{}",
                    archive.identity.path_fingerprint(&archive.path)
                )
            } else {
                base
            };

            if used.contains(&name) {
                name = format!(
                    "{name}--{}",
                    archive.identity.path_fingerprint(&archive.path)
                );
            }
            let mut suffix = 2;
            while used.contains(&name) {
                name = format!("{}-{suffix}", safe_mount_name(&archive.path));
                suffix += 1;
            }
            used.insert(name.clone());

            MountPlan::new(archive.clone(), mount_root.join(name))
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

fn normalized_title(path: &Path) -> String {
    safe_mount_name(path).to_lowercase()
}

pub fn detect_platform(path: impl AsRef<Path>, source_root: impl AsRef<Path>) -> Option<String> {
    let path = path.as_ref();
    let source_root = source_root.as_ref();

    for segment in source_root.iter().chain(path.iter()) {
        let normalized = normalize_path_segment(&segment.to_string_lossy());
        if normalized.starts_with("microsoftxbox360") || normalized.starts_with("xbox360") {
            return Some("Xbox360".to_string());
        }
        if normalized.starts_with("microsoftxbox") || normalized.starts_with("xbox") {
            return Some("Xbox".to_string());
        }
        match normalized.as_str() {
            "atarist" => return Some("AtariST".to_string()),
            "a2600" | "atari2600" => return Some("Atari2600".to_string()),
            _ => {}
        }
    }

    let normalized_path = normalize_path_segment(&path.to_string_lossy());
    let normalized_root = normalize_path_segment(&source_root.to_string_lossy());
    let searchable = format!("{normalized_root}{normalized_path}");

    if searchable.contains("007legends") || searchable.contains("mortalkombatkompleteedition") {
        return Some("Xbox360".to_string());
    }
    if searchable.contains("fableusaeurope") {
        return Some("Xbox".to_string());
    }
    if searchable.contains("gameboyadvancecias") {
        return Some("Nintendo3DS".to_string());
    }
    if searchable.contains("iamjesuschrist") || searchable.contains("steamrip") {
        return Some("PC".to_string());
    }
    if searchable.contains("metalgearsolidpeacewalker") {
        return Some("PSP".to_string());
    }
    if searchable.contains("atari2600vcsromcollection") {
        return Some("Atari2600".to_string());
    }

    None
}

fn normalize_path_segment(segment: &str) -> String {
    segment
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
    let plans = plan_mounts(&archives, &config.mount_root);
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    Ok(statuses_from_plans(plans, &mounted_paths))
}

pub fn mount_archives(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    mount_archives_with_backend(config, &backend)
}

pub fn mount_archives_with_backend(
    config: &Config,
    backend: &impl MountBackend,
) -> Result<Vec<ArchiveStatus>> {
    let archives = scan_archives(config)?;
    let plans = plan_mounts(&archives, &config.mount_root);
    fs::create_dir_all(&config.mount_root).map_err(|source| ArchiveFsError::Io {
        path: config.mount_root.clone(),
        source,
    })?;

    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    for plan in &plans {
        if mounted_paths.contains(&plan.mount_path) {
            continue;
        }
        fs::create_dir_all(&plan.mount_path).map_err(|source| ArchiveFsError::Io {
            path: plan.mount_path.clone(),
            source,
        })?;
        backend.mount(plan)?;
    }

    current_statuses(config)
}

pub fn unmount_archives(config: &Config) -> Result<Vec<ArchiveStatus>> {
    let backend = RatarmountBackend::new(config.ratarmount_bin.clone());
    unmount_archives_with_backend(config, &backend)
}

pub fn unmount_archives_with_backend(
    config: &Config,
    backend: &impl MountBackend,
) -> Result<Vec<ArchiveStatus>> {
    let mounted_paths = mounted_paths_under(&config.mount_root)?;
    let mut mounted_paths = mounted_paths.into_iter().collect::<Vec<_>>();
    mounted_paths.sort();
    mounted_paths.reverse();

    for mount_path in mounted_paths {
        if path_is_under(&mount_path, &config.mount_root) {
            backend.unmount(&mount_path)?;
        }
    }

    current_statuses(config)
}

fn statuses_from_plans(
    plans: Vec<MountPlan>,
    mounted_paths: &HashSet<PathBuf>,
) -> Vec<ArchiveStatus> {
    plans
        .into_iter()
        .map(|plan| {
            let state = if mounted_paths.contains(&plan.mount_path) {
                MountState::Mounted
            } else if plan.mount_path.exists() {
                MountState::MountPathExists
            } else {
                MountState::Pending
            };

            ArchiveStatus {
                archive_path: plan.archive.path,
                mount_path: plan.mount_path,
                state,
            }
        })
        .collect()
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

pub fn command_available(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file();
    }

    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
        .unwrap_or(false)
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
        let archives = vec![archive("/roms/ps1/game.zip"), archive("/roms/ps2/game.zip")];
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

    #[test]
    fn archive_health_marks_retryable_states() {
        assert!(ArchiveHealth::Failed.is_retryable());
        assert!(ArchiveHealth::MissingParts.is_retryable());
        assert!(ArchiveHealth::RetryAvailable.is_retryable());
        assert!(!ArchiveHealth::Pending.is_retryable());
        assert!(!ArchiveHealth::Mounted.is_retryable());
    }

    #[test]
    fn archive_health_marks_terminal_states() {
        assert!(ArchiveHealth::Corrupt.is_terminal_without_source_change());
        assert!(ArchiveHealth::Unsupported.is_terminal_without_source_change());
        assert!(ArchiveHealth::PermissionDenied.is_terminal_without_source_change());
        assert!(!ArchiveHealth::Failed.is_terminal_without_source_change());
    }

    #[test]
    fn detects_platform_from_known_source_path_segments() {
        assert_eq!(
            detect_platform("/roms/microsoft_xbox/Halo.zip", "/roms"),
            Some("Xbox".to_string())
        );
        assert_eq!(
            detect_platform("/roms/xbox360/Halo 3.zip", "/roms"),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform("/collections/Atari ST/Gem.zip", "/collections"),
            Some("AtariST".to_string())
        );
        assert_eq!(
            detect_platform("/collections/Atari-2600/Pitfall.zip", "/collections"),
            Some("Atari2600".to_string())
        );
        assert_eq!(detect_platform("/roms/unknown/game.zip", "/roms"), None);
    }

    #[test]
    fn detects_platform_from_collection_style_xbox_segments() {
        assert_eq!(
            detect_platform(
                "/collections/microsoft_xbox360_f_part1/Game.zip",
                "/collections"
            ),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform("/collections/microsoft_xbox_f/Game.zip", "/collections"),
            Some("Xbox".to_string())
        );
        assert_eq!(
            detect_platform("/collections/microsoft_xbox_j/Game.zip", "/collections"),
            Some("Xbox".to_string())
        );
    }

    #[test]
    fn detects_platform_from_title_and_release_heuristics() {
        assert_eq!(
            detect_platform("/incoming/007 Legends.zip", "/incoming"),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform(
                "/incoming/Mortal Kombat - Komplete Edition.rar",
                "/incoming",
            ),
            Some("Xbox360".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/Fable (USA, Europe).7z", "/incoming"),
            Some("Xbox".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/Gameboy Advance CIAs/Metroid.zip", "/incoming"),
            Some("Nintendo3DS".to_string())
        );
        assert_eq!(
            detect_platform("/downloads/I.Am.Jesus.Christ.zip", "/downloads"),
            Some("PC".to_string())
        );
        assert_eq!(
            detect_platform("/downloads/SteamRIP/Example.zip", "/downloads"),
            Some("PC".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/Metal Gear Solid - Peace Walker.zip", "/incoming",),
            Some("PSP".to_string())
        );
        assert_eq!(
            detect_platform("/sets/Atari-2600-VCS-ROM-Collection/archive.zip", "/sets",),
            Some("Atari2600".to_string())
        );
        assert_eq!(
            detect_platform("/incoming/random-game.zip", "/incoming"),
            None
        );
    }

    #[test]
    fn archive_identity_stores_detected_platform() {
        let archive =
            Archive::from_path_in_root("/roms/microsoft_xbox360/Halo 3.zip", "/roms").unwrap();

        assert_eq!(archive.identity.platform, Some("Xbox360".to_string()));
    }

    #[test]
    fn mount_plan_generation_carries_archive_identity_and_pending_state() {
        let archives = vec![archive("/roms/ps1/Resident Evil 2.zip")];
        let plans = plan_mounts(&archives, "/mnt/archivefs");

        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].archive.path,
            PathBuf::from("/roms/ps1/Resident Evil 2.zip")
        );
        assert_eq!(plans[0].archive.kind, ArchiveKind::Zip);
        assert_eq!(plans[0].archive.identity.normalized_name, "resident_evil_2");
        assert_eq!(
            plans[0].mount_path,
            PathBuf::from("/mnt/archivefs/Resident_Evil_2")
        );
        assert_eq!(plans[0].state, MountState::Pending);
    }

    #[test]
    fn doctor_reports_missing_config() {
        let root = test_root("doctor_missing_config");
        let config_path = root.join("missing.toml");
        let report = run_doctor(&config_path);

        assert!(!report.is_ready());
        assert_eq!(report.archives_found, 0);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "config file" && check.status == DoctorStatus::Fail)
        );
    }

    #[test]
    fn doctor_counts_archives_platforms_and_pending_mounts() {
        let root = test_root("doctor_counts");
        let source_root = root.join("roms");
        let xbox = source_root.join("microsoft_xbox");
        let unknown = source_root.join("unknown");
        let mount_root = root.join("mounts");
        let ratarmount = root.join("ratarmount");
        fs::create_dir_all(&xbox).unwrap();
        fs::create_dir_all(&unknown).unwrap();
        fs::write(xbox.join("Halo.zip"), b"").unwrap();
        fs::write(unknown.join("Mystery.7z"), b"").unwrap();
        fs::write(&ratarmount, b"").unwrap();
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "source_folders = [\"{}\"]\nmount_root = \"{}\"\nratarmount_bin = \"{}\"\n",
                source_root.display(),
                mount_root.display(),
                ratarmount.display()
            ),
        )
        .unwrap();

        let report = run_doctor(&config_path);

        assert_eq!(report.archives_found, 2);
        assert_eq!(report.archives_with_platform, 1);
        assert_eq!(report.archives_unknown_platform, 1);
        assert_eq!(
            report.unknown_platform_examples,
            vec![unknown.join("Mystery.7z")]
        );
        assert_eq!(report.platform_counts, vec![("Xbox".to_string(), 1)]);
        assert_eq!(report.pending_archives, 2);
        assert_eq!(report.mounted_archives, 0);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "config parses" && check.status == DoctorStatus::Pass)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.name == "archive scan" && check.status == DoctorStatus::Pass)
        );
    }

    fn test_root(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("archivefs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn archive(path: &str) -> Archive {
        let path = PathBuf::from(path);
        Archive {
            kind: archive_kind(&path).unwrap(),
            identity: ArchiveIdentity::from_path(&path, PathBuf::new(), None),
            path,
            health: ArchiveHealth::Pending,
        }
    }
}
