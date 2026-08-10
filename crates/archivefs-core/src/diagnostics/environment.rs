//! Read-only storage and filesystem diagnostics for Doctor.
//!
//! Two questions, answered without writing anything:
//!
//! 1. **How much room is left** on each filesystem EmuWiz depends on
//!    ([`assess_storage`]). Answered with `statvfs(3)`, which reports what
//!    the kernel already knows. Nothing is created, and free space is never
//!    estimated by walking directory contents.
//! 2. **Is that filesystem mounted read-only** ([`mount_table`]). Answered by
//!    reading the mount options the kernel publishes in
//!    `/proc/self/mountinfo`, reusing the same escape decoding the mount code
//!    already relies on. Never by calling `access(2)`, and never by creating
//!    a probe file.
//!
//! ## Why permission bits are not the answer
//!
//! A write can fail for reasons no metadata reveals: a read-only mount, a
//! full filesystem, an immutable attribute, a POSIX ACL, SELinux, or a
//! Flatpak portal that mediates the path. So this module never concludes
//! "writable". It reports [`WritabilityAssessment`], whose most positive
//! verdict is *"appears writable"* - see that type's documentation for why
//! that wording is deliberate.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    DoctorCategory, DoctorSeverity, DoctorSubsystem, Finding, Measurement, NotCheckedCheck,
};
use crate::Config;
use crate::emulator_environment::EncodedPath;

/// Which EmuWiz resource a path is. Drives both severity (a read-only
/// source folder is fine; a read-only database is not) and the free-space
/// floor applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRole {
    /// `~/.local/share/archivefs` and friends.
    DataDirectory,
    CacheDirectory,
    /// The catalogue database.
    Database,
    /// Where install journals and backups live.
    TransactionStorage,
    MountRoot,
    /// A configured library folder. EmuWiz only ever reads these.
    SourceRoot,
    ArchiveIndex,
    /// A destination EmuWiz may write an emulator file into.
    EmulatorProfile,
}

impl ResourceRole {
    pub fn label(self) -> &'static str {
        match self {
            Self::DataDirectory => "EmuWiz data directory",
            Self::CacheDirectory => "EmuWiz cache directory",
            Self::Database => "catalogue database",
            Self::TransactionStorage => "install history and backups",
            Self::MountRoot => "mount root",
            Self::SourceRoot => "source folder",
            Self::ArchiveIndex => "archive index",
            Self::EmulatorProfile => "emulator profile",
        }
    }

    /// Whether EmuWiz ever writes here. A source folder is read-only by
    /// design, so it being on a read-only filesystem is not a fault.
    pub fn archivefs_writes_here(self) -> bool {
        !matches!(self, Self::SourceRoot)
    }

    /// Whether losing room here can interrupt a transaction mid-way -
    /// journals, backups and staged files. These get the strictest floor.
    pub fn holds_transactional_data(self) -> bool {
        matches!(
            self,
            Self::DataDirectory | Self::Database | Self::TransactionStorage
        )
    }

    /// How bad a read-only filesystem is for this role.
    fn read_only_severity(self) -> Option<DoctorSeverity> {
        match self {
            // EmuWiz cannot function: these are written during ordinary use.
            Self::Database
            | Self::CacheDirectory
            | Self::TransactionStorage
            | Self::DataDirectory
            | Self::ArchiveIndex => Some(DoctorSeverity::Error),
            // Mounting needs to create mount-point directories here.
            Self::MountRoot => Some(DoctorSeverity::Error),
            // An install would fail, but nothing else breaks.
            Self::EmulatorProfile => Some(DoctorSeverity::Warning),
            // Perfectly normal: a library on a read-only share still works.
            Self::SourceRoot => None,
        }
    }
}

/// One path Doctor wants to know about, and what it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageResource {
    pub role: ResourceRole,
    pub path: PathBuf,
}

impl StorageResource {
    pub fn new(role: ResourceRole, path: impl Into<PathBuf>) -> Self {
        Self {
            role,
            path: path.into(),
        }
    }
}

// --- Filesystem statistics ------------------------------------------------

/// What the kernel reports about a filesystem's capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FilesystemStat {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

impl FilesystemStat {
    pub fn available_percent(&self) -> Option<f64> {
        (self.total_bytes > 0)
            .then(|| (self.available_bytes as f64 / self.total_bytes as f64) * 100.0)
    }
}

/// Reads a filesystem's capacity with `statvfs(3)`.
///
/// Read-only: `statvfs` reports counters the kernel already maintains. It
/// creates nothing, opens no file for writing, and does not change any
/// timestamp. `f_bavail` is used rather than `f_bfree` because it excludes
/// blocks reserved for root, which is what an unprivileged EmuWiz can
/// actually use.
#[cfg(unix)]
pub fn filesystem_stat(path: &Path) -> Option<FilesystemStat> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let raw = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `raw` is a valid NUL-terminated C string that outlives the
    // call, and `stat` is a correctly sized, zeroed `libc::statvfs` we own.
    // `statvfs` only reads.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(raw.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block_size = if stat.f_frsize > 0 {
        stat.f_frsize as u64
    } else {
        stat.f_bsize as u64
    };
    Some(FilesystemStat {
        available_bytes: (stat.f_bavail as u64).saturating_mul(block_size),
        total_bytes: (stat.f_blocks as u64).saturating_mul(block_size),
    })
}

#[cfg(not(unix))]
pub fn filesystem_stat(_path: &Path) -> Option<FilesystemStat> {
    // No portable read-only equivalent is wired up yet; the caller reports
    // this honestly as "not checked" rather than as healthy.
    None
}

/// The device a path lives on, used to group paths by filesystem.
#[cfg(unix)]
fn device_id(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
fn device_id(_path: &Path) -> Option<u64> {
    None
}

// --- Mount state ----------------------------------------------------------

/// How a filesystem is mounted, as the kernel reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    ReadWrite,
    ReadOnly,
    /// The mount table could not be read, or no entry covers this path.
    /// Never treated as healthy.
    Unknown,
}

impl MountMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadWrite => "mounted read-write",
            Self::ReadOnly => "mounted read-only",
            Self::Unknown => "mount state unknown",
        }
    }
}

/// One line of `/proc/self/mountinfo`, reduced to what Doctor needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub mount_point: PathBuf,
    /// The per-mount options field. `ro` here means writes through *this*
    /// mount point fail, even if the superblock is writable elsewhere.
    pub read_only_mount: bool,
    /// The superblock options after the `-` separator.
    pub read_only_superblock: bool,
    pub filesystem_type: Option<String>,
}

impl MountEntry {
    pub fn mode(&self) -> MountMode {
        if self.read_only_mount || self.read_only_superblock {
            MountMode::ReadOnly
        } else {
            MountMode::ReadWrite
        }
    }
}

/// Parses `/proc/self/mountinfo` content.
///
/// Field layout (`proc(5)`): id, parent, `major:minor`, root, mount point,
/// mount options, zero or more optional fields, a literal `-`, filesystem
/// type, source, superblock options. Mount points are decoded with the same
/// octal unescaping the existing mount code uses, so a path containing a
/// space or a newline is handled identically here.
pub fn parse_mount_table(contents: &[u8]) -> Vec<MountEntry> {
    contents
        .split(|byte| *byte == b'\n')
        .filter_map(parse_mount_line)
        .collect()
}

fn parse_mount_line(line: &[u8]) -> Option<MountEntry> {
    let fields: Vec<&[u8]> = line
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect();
    // Shortest possible valid line: 6 fixed fields, `-`, type, source,
    // superblock options.
    if fields.len() < 10 {
        return None;
    }
    let mount_point = crate::mountinfo_path_for_diagnostics(fields[4])?;
    let read_only_mount = options_contain_read_only(fields[5]);
    let separator = fields.iter().position(|field| *field == b"-")?;
    let filesystem_type = fields
        .get(separator + 1)
        .map(|value| String::from_utf8_lossy(value).into_owned());
    let read_only_superblock = fields
        .get(separator + 3)
        .is_some_and(|options| options_contain_read_only(options));
    Some(MountEntry {
        mount_point,
        read_only_mount,
        read_only_superblock,
        filesystem_type,
    })
}

/// `ro` as a whole comma-separated option, never as a substring: `rootcontext`
/// and `errors=remount-ro` must not be mistaken for it.
fn options_contain_read_only(options: &[u8]) -> bool {
    options
        .split(|byte| *byte == b',')
        .any(|option| option == b"ro")
}

/// Reads the current mount table. `None` when it cannot be read at all, so
/// callers report "unknown" rather than assuming read-write.
pub fn mount_table() -> Option<Vec<MountEntry>> {
    fs::read("/proc/self/mountinfo")
        .ok()
        .map(|contents| parse_mount_table(&contents))
}

/// The mount entry covering `path`: the **longest** matching mount point, so
/// a nested mount wins over its parent.
pub fn mount_entry_for_path<'a>(table: &'a [MountEntry], path: &Path) -> Option<&'a MountEntry> {
    // Resolve first so a symlink cannot attribute a path to the wrong
    // filesystem. If it cannot be resolved, fall back to the literal path
    // rather than guessing.
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    table
        .iter()
        .filter(|entry| resolved.starts_with(&entry.mount_point))
        .max_by_key(|entry| entry.mount_point.as_os_str().len())
}

// --- Free-space policy ----------------------------------------------------

/// Conservative, documented thresholds. One structure so these can become
/// settings later without hunting for constants; no setting and no migration
/// is added now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeSpacePolicy {
    /// Percentage bands only apply while available space is *also* below
    /// this. Without it, a 10 TB filesystem at 8% free - 800 GB, plenty -
    /// would be reported as a problem.
    pub percentage_bands_apply_below_bytes: u64,
    pub info_percent: f64,
    pub warning_percent: f64,
    pub error_percent: f64,
    /// Absolute floor below which any writable destination is an error,
    /// whatever the percentage says.
    pub error_floor_bytes: u64,
    /// Absolute floor below which a destination holding transactional data
    /// is critical: a journal or backup that cannot be written leaves an
    /// operation unrecoverable.
    pub critical_floor_bytes: u64,
}

impl Default for FreeSpacePolicy {
    fn default() -> Self {
        Self {
            percentage_bands_apply_below_bytes: 64 * 1024 * 1024 * 1024,
            info_percent: 15.0,
            warning_percent: 10.0,
            error_percent: 5.0,
            error_floor_bytes: 2 * 1024 * 1024 * 1024,
            critical_floor_bytes: 512 * 1024 * 1024,
        }
    }
}

impl FreeSpacePolicy {
    /// The severity for one filesystem, as the worse of the absolute rule and
    /// the percentage rule. `None` when there is nothing to say.
    fn severity(
        &self,
        stat: FilesystemStat,
        holds_transactional_data: bool,
        archivefs_writes_here: bool,
    ) -> Option<DoctorSeverity> {
        if !archivefs_writes_here {
            // EmuWiz never writes to a source folder, so its free space is
            // not EmuWiz's problem to report.
            return None;
        }
        let mut severity: Option<DoctorSeverity> = None;
        let mut escalate = |candidate: DoctorSeverity| {
            severity = Some(match severity {
                Some(current) if current.rank() <= candidate.rank() => current,
                _ => candidate,
            });
        };

        if stat.available_bytes < self.critical_floor_bytes {
            escalate(if holds_transactional_data {
                DoctorSeverity::Critical
            } else {
                DoctorSeverity::Error
            });
        } else if stat.available_bytes < self.error_floor_bytes {
            escalate(DoctorSeverity::Error);
        }

        // Percentages only once absolute space is small enough to matter.
        if stat.available_bytes < self.percentage_bands_apply_below_bytes
            && let Some(percent) = stat.available_percent()
        {
            if percent < self.error_percent {
                escalate(DoctorSeverity::Error);
            } else if percent < self.warning_percent {
                escalate(DoctorSeverity::Warning);
            } else if percent < self.info_percent {
                escalate(DoctorSeverity::Info);
            }
        }
        severity
    }
}

// --- Assessment -----------------------------------------------------------

/// One filesystem, with every EmuWiz resource that lives on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FilesystemGroup {
    /// The first resource path observed on this filesystem - what findings
    /// point at.
    pub representative_path: EncodedPath,
    /// Device identity, when the platform exposes it safely.
    pub device_id: Option<u64>,
    pub mount_point: Option<EncodedPath>,
    pub filesystem_type: Option<String>,
    pub mount_mode: MountMode,
    pub stat: Option<FilesystemStat>,
    /// Every EmuWiz resource sharing this filesystem, deduplicated.
    pub roles: Vec<ResourceRole>,
    /// Every path that resolved here.
    pub paths: Vec<EncodedPath>,
    /// Where the numbers came from.
    pub evidence_source: &'static str,
}

impl FilesystemGroup {
    fn holds_transactional_data(&self) -> bool {
        self.roles
            .iter()
            .any(|role| role.holds_transactional_data())
    }

    fn archivefs_writes_here(&self) -> bool {
        self.roles.iter().any(|role| role.archivefs_writes_here())
    }

    fn role_summary(&self) -> String {
        self.roles
            .iter()
            .map(|role| role.label())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A resource Doctor could not assess, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnassessedResource {
    pub role: ResourceRole,
    pub path: EncodedPath,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StorageAssessment {
    pub filesystems: Vec<FilesystemGroup>,
    pub unassessed: Vec<UnassessedResource>,
    /// `false` when the mount table could not be read at all.
    pub mount_table_available: bool,
}

/// Groups the supplied resources by filesystem and reads each one's capacity
/// and mount mode.
///
/// Every call is read-only: `fs::metadata` and `fs::canonicalize` for
/// identity, `statvfs` for capacity, and one read of
/// `/proc/self/mountinfo`. Nothing is created and no timestamp changes. A
/// path that does not exist, or whose capacity cannot be read, becomes an
/// [`UnassessedResource`] rather than being assumed healthy.
pub fn assess_storage(resources: &[StorageResource]) -> StorageAssessment {
    let table = mount_table();
    let mut groups: BTreeMap<GroupKey, FilesystemGroup> = BTreeMap::new();
    let mut unassessed = Vec::new();

    for resource in resources {
        if !resource.path.exists() {
            unassessed.push(UnassessedResource {
                role: resource.role,
                path: EncodedPath::from_path(&resource.path),
                reason: "the path does not exist yet, so its filesystem cannot be identified",
            });
            continue;
        }
        let device = device_id(&resource.path);
        let stat = filesystem_stat(&resource.path);
        if stat.is_none() {
            unassessed.push(UnassessedResource {
                role: resource.role,
                path: EncodedPath::from_path(&resource.path),
                reason: "the operating system did not report this filesystem's capacity",
            });
        }
        let entry = table
            .as_deref()
            .and_then(|table| mount_entry_for_path(table, &resource.path));
        // Group by device where available; otherwise by mount point, and
        // finally by the path itself - never merging two filesystems that
        // might be different.
        let key = match (device, entry) {
            (Some(device), _) => GroupKey::Device(device),
            (None, Some(entry)) => GroupKey::MountPoint(entry.mount_point.clone()),
            (None, None) => GroupKey::Path(resource.path.clone()),
        };
        let group = groups.entry(key).or_insert_with(|| FilesystemGroup {
            representative_path: EncodedPath::from_path(&resource.path),
            device_id: device,
            mount_point: entry.map(|entry| EncodedPath::from_path(&entry.mount_point)),
            filesystem_type: entry.and_then(|entry| entry.filesystem_type.clone()),
            mount_mode: match (&table, entry) {
                (Some(_), Some(entry)) => entry.mode(),
                _ => MountMode::Unknown,
            },
            stat,
            roles: Vec::new(),
            paths: Vec::new(),
            evidence_source: if cfg!(unix) {
                "statvfs(3) and /proc/self/mountinfo"
            } else {
                "unavailable on this platform"
            },
        });
        if !group.roles.contains(&resource.role) {
            group.roles.push(resource.role);
        }
        let encoded = EncodedPath::from_path(&resource.path);
        if !group.paths.contains(&encoded) {
            group.paths.push(encoded);
        }
    }

    let mut filesystems: Vec<FilesystemGroup> = groups.into_values().collect();
    for group in &mut filesystems {
        group.roles.sort();
        group
            .paths
            .sort_by(|left, right| left.display.cmp(&right.display));
    }
    filesystems.sort_by(|left, right| {
        left.representative_path
            .display
            .cmp(&right.representative_path.display)
    });
    StorageAssessment {
        filesystems,
        unassessed,
        mount_table_available: table.is_some(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum GroupKey {
    Device(u64),
    MountPoint(PathBuf),
    Path(PathBuf),
}

// --- Findings -------------------------------------------------------------

/// Human-readable byte count. Binary units, because that is what `statvfs`
/// reports and what a person comparing with `df -h` will see.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn space_finding_id(severity: DoctorSeverity) -> &'static str {
    match severity {
        DoctorSeverity::Critical => "filesystem.critically_low_space",
        _ => "filesystem.low_space",
    }
}

/// Free-space findings, one per filesystem rather than one per path.
pub fn findings_from_free_space(
    assessment: &StorageAssessment,
    policy: &FreeSpacePolicy,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for group in &assessment.filesystems {
        let Some(stat) = group.stat else {
            continue;
        };
        let Some(severity) = policy.severity(
            stat,
            group.holds_transactional_data(),
            group.archivefs_writes_here(),
        ) else {
            continue;
        };
        let percent = stat.available_percent();
        let mut evidence = vec![
            format!(
                "Available: {} of {}",
                format_bytes(stat.available_bytes),
                format_bytes(stat.total_bytes)
            ),
            format!("Used by: {}", group.role_summary()),
            format!("Evidence source: {}", group.evidence_source),
        ];
        if let Some(percent) = percent {
            evidence.push(format!("Available: {percent:.1}%"));
        }
        if let Some(mount_point) = &group.mount_point {
            evidence.push(format!("Mount point: {}", mount_point.display));
        }
        if let Some(filesystem_type) = &group.filesystem_type {
            evidence.push(format!("Filesystem type: {filesystem_type}"));
        }
        if let Some(device) = group.device_id {
            evidence.push(format!("Device identity: {device}"));
        }
        evidence.push(format!(
            "Thresholds applied: error below {} or {:.0}%, critical below {} for transactional data, percentage bands only below {}",
            format_bytes(policy.error_floor_bytes),
            policy.error_percent,
            format_bytes(policy.critical_floor_bytes),
            format_bytes(policy.percentage_bands_apply_below_bytes)
        ));
        for path in &group.paths {
            evidence.push(format!("Shared by: {}", path.display));
        }
        findings.push(
            Finding::new(
                space_finding_id(severity),
                DoctorCategory::Storage,
                DoctorSubsystem::FilesystemCapacity,
                severity,
                match severity {
                    DoctorSeverity::Critical => "A filesystem is critically low on space",
                    _ => "A filesystem is low on space",
                },
                format!(
                    "{} available{} on the filesystem holding {}.",
                    format_bytes(stat.available_bytes),
                    percent
                        .map(|percent| format!(" ({percent:.1}%)"))
                        .unwrap_or_default(),
                    group.role_summary()
                ),
            )
            .with_affected(group.representative_path.clone())
            .with_evidence(evidence)
            .with_measurements(
                [
                    ("available_bytes", Measurement::Integer(stat.available_bytes)),
                    ("total_bytes", Measurement::Integer(stat.total_bytes)),
                    (
                        "holds_transactional_data",
                        Measurement::Flag(group.holds_transactional_data()),
                    ),
                    (
                        "filesystem_read_only",
                        Measurement::Flag(group.mount_mode == MountMode::ReadOnly),
                    ),
                ]
                .into_iter()
                .chain(
                    percent.map(|percent| ("available_percent", Measurement::percent(percent))),
                )
                .chain(group.filesystem_type.as_ref().map(|filesystem_type| {
                    ("filesystem_type", Measurement::text(filesystem_type))
                }))
                .chain(
                    group
                        .mount_point
                        .as_ref()
                        .map(|point| ("mount_point", Measurement::text(&point.display))),
                ),
            )
            .with_guidance(
                if group.holds_transactional_data() {
                    "EmuWiz writes install journals and backups here. Running out of room part-way through an operation is what makes one unrecoverable."
                } else {
                    "EmuWiz needs room here to write. Operations will start failing before the filesystem is completely full."
                },
                "Free some space on this filesystem, or move the affected EmuWiz directory to one with more room.",
            ),
        );
    }
    findings
}

/// Read-only-filesystem findings, one per affected filesystem.
///
/// A read-only *source folder* produces no finding at all: EmuWiz only
/// ever reads a library, so a read-only share or a mounted image is a
/// perfectly ordinary setup.
pub fn findings_from_read_only_filesystems(assessment: &StorageAssessment) -> Vec<Finding> {
    let mut findings = Vec::new();
    for group in &assessment.filesystems {
        if group.mount_mode != MountMode::ReadOnly {
            continue;
        }
        // The worst severity among the roles that actually need to write.
        let Some(severity) = group
            .roles
            .iter()
            .filter_map(|role| role.read_only_severity())
            .min_by_key(|severity| severity.rank())
        else {
            continue;
        };
        let writable_roles: Vec<&'static str> = group
            .roles
            .iter()
            .filter(|role| role.archivefs_writes_here())
            .map(|role| role.label())
            .collect();
        let read_only_roles: Vec<&'static str> = group
            .roles
            .iter()
            .filter(|role| !role.archivefs_writes_here())
            .map(|role| role.label())
            .collect();
        let mut evidence = vec![format!(
            "Mount state: {} (from /proc/self/mountinfo)",
            group.mount_mode.label()
        )];
        if let Some(mount_point) = &group.mount_point {
            evidence.push(format!("Mount point: {}", mount_point.display));
        }
        if let Some(filesystem_type) = &group.filesystem_type {
            evidence.push(format!("Filesystem type: {filesystem_type}"));
        }
        evidence.push(format!(
            "EmuWiz needs to write to: {}",
            writable_roles.join(", ")
        ));
        if !read_only_roles.is_empty() {
            evidence.push(format!(
                "Read-only is fine for: {}",
                read_only_roles.join(", ")
            ));
        }
        for path in &group.paths {
            evidence.push(format!("Affected: {}", path.display));
        }
        findings.push(
            Finding::new(
                "filesystem.read_only",
                DoctorCategory::Filesystems,
                DoctorSubsystem::FilesystemMountState,
                severity,
                "A filesystem EmuWiz writes to is mounted read-only",
                format!(
                    "The filesystem holding {} is mounted read-only, so EmuWiz cannot write there.",
                    writable_roles.join(", ")
                ),
            )
            .with_affected(group.representative_path.clone())
            .with_evidence(evidence)
            .with_measurements(
                [
                    ("filesystem_read_only", Measurement::Flag(true)),
                    (
                        "mount_mode",
                        Measurement::text(group.mount_mode.label()),
                    ),
                ]
                .into_iter()
                .chain(group.filesystem_type.as_ref().map(|filesystem_type| {
                    ("filesystem_type", Measurement::text(filesystem_type))
                }))
                .chain(
                    group
                        .mount_point
                        .as_ref()
                        .map(|point| ("mount_point", Measurement::text(&point.display))),
                ),
            )
            .with_guidance(
                "This is the mount's own state, not a permissions problem. No change to file permissions can make it writable.",
                "Remount that filesystem read-write, or point the affected EmuWiz setting at a writable location.",
            ),
        );
    }
    findings
}

/// Everything Doctor could not determine about storage. Never presented as a
/// pass.
pub fn not_checked_from_storage(assessment: &StorageAssessment) -> Vec<NotCheckedCheck> {
    let mut items = Vec::new();
    if !assessment.mount_table_available {
        items.push(NotCheckedCheck {
            name: "Filesystem mount state".to_string(),
            reason: "The mount table could not be read, so EmuWiz cannot tell whether any filesystem is mounted read-only.".to_string(),
            next_step: "This is unusual. Check that /proc is mounted.".to_string(),
        });
    }
    for resource in &assessment.unassessed {
        items.push(NotCheckedCheck {
            name: format!("Free space for the {}", resource.role.label()),
            reason: format!("{}: {}", resource.path.display, resource.reason),
            next_step: "Nothing is wrong yet - EmuWiz simply cannot report on this path."
                .to_string(),
        });
    }
    for group in &assessment.filesystems {
        if group.mount_mode == MountMode::Unknown {
            items.push(NotCheckedCheck {
                name: format!("Mount state for the {}", group.role_summary()),
                reason: format!(
                    "{} is not covered by any recognised mount entry, so read-only state is unknown.",
                    group.representative_path.display
                ),
                next_step: "Nothing is wrong yet - EmuWiz simply cannot confirm this one way or the other.".to_string(),
            });
        }
    }
    items
}

// --- Writability assessment ----------------------------------------------

/// What EmuWiz can honestly say about being able to write somewhere,
/// without writing to find out.
///
/// There is deliberately no `Writable` variant. A write can still fail
/// because of a POSIX ACL, an immutable attribute, SELinux, a Flatpak portal,
/// a full filesystem, or a race - none of which permission bits reveal. The
/// most positive verdict is therefore [`Self::AppearsWritable`], and the UI
/// wording matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WritabilityAssessment {
    /// The mount is read-write and the current user's permission bits allow
    /// writing. Consistent with writing succeeding; not a guarantee.
    AppearsWritable,
    /// The filesystem itself is read-only. No permission change helps.
    ReadOnlyFilesystem,
    /// The mount is read-write but the current user's bits do not allow
    /// writing.
    PermissionDenied,
    /// The path is not there.
    MissingDestination,
    /// Exists but is not a directory, or is a symlink EmuWiz will not
    /// follow.
    UnsafeDestination,
    /// Not established - unknown mount state, unreadable metadata, or a
    /// sandbox whose real behaviour cannot be predicted from metadata.
    NotProven,
}

impl WritabilityAssessment {
    /// Wording that never overclaims.
    pub fn label(self) -> &'static str {
        match self {
            Self::AppearsWritable => "Appears writable from permissions and read-write mount state",
            Self::ReadOnlyFilesystem => "Not writable: the filesystem is mounted read-only",
            Self::PermissionDenied => "Not writable: permissions deny it for this user",
            Self::MissingDestination => "Destination does not exist",
            Self::UnsafeDestination => "Destination is not a directory EmuWiz will write into",
            Self::NotProven => "Writability not proven without a write probe",
        }
    }

    pub fn is_problem(self) -> bool {
        !matches!(self, Self::AppearsWritable)
    }
}

impl fmt::Display for WritabilityAssessment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Permission-bit evidence for one directory. Recorded so a finding can show
/// what it saw, not just its conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PathPermissions {
    pub mode: Option<u32>,
    pub owner_uid: Option<u32>,
    pub group_gid: Option<u32>,
    /// Whether the *current* process's identity matches the owner.
    pub owned_by_current_user: Option<bool>,
    pub current_user_may_write: Option<bool>,
}

/// Reads permission metadata for a directory, without touching it.
///
/// `symlink_metadata` is used deliberately: a symlinked destination is
/// reported as unsafe rather than silently followed, matching the rule the
/// rest of EmuWiz applies to destinations.
#[cfg(unix)]
pub fn assess_permissions(path: &Path) -> Option<PathPermissions> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).ok()?;
    let mode = metadata.mode();
    // SAFETY: `geteuid`/`getegid` take no arguments and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let owned = metadata.uid() == uid;
    let same_group = metadata.gid() == gid;
    let may_write = if uid == 0 {
        // Root bypasses the bits, but a read-only mount still refuses, so
        // this is only one input to the verdict.
        true
    } else if owned {
        mode & 0o200 != 0
    } else if same_group {
        mode & 0o020 != 0
    } else {
        mode & 0o002 != 0
    };
    Some(PathPermissions {
        mode: Some(mode),
        owner_uid: Some(metadata.uid()),
        group_gid: Some(metadata.gid()),
        owned_by_current_user: Some(owned),
        current_user_may_write: Some(may_write),
    })
}

#[cfg(not(unix))]
pub fn assess_permissions(_path: &Path) -> Option<PathPermissions> {
    None
}

/// Combines mount mode and permission bits into one honest verdict.
///
/// Order matters: a read-only filesystem outranks permissions, because no
/// permission change can make it writable, and reporting "permission denied"
/// there would send someone chasing the wrong fix.
pub fn assess_writability(
    exists: bool,
    is_directory: bool,
    is_symlink: bool,
    mount_mode: MountMode,
    permissions: Option<PathPermissions>,
) -> WritabilityAssessment {
    if !exists {
        return WritabilityAssessment::MissingDestination;
    }
    if is_symlink || !is_directory {
        return WritabilityAssessment::UnsafeDestination;
    }
    match mount_mode {
        MountMode::ReadOnly => WritabilityAssessment::ReadOnlyFilesystem,
        MountMode::Unknown => WritabilityAssessment::NotProven,
        MountMode::ReadWrite => match permissions.and_then(|value| value.current_user_may_write) {
            Some(true) => WritabilityAssessment::AppearsWritable,
            Some(false) => WritabilityAssessment::PermissionDenied,
            None => WritabilityAssessment::NotProven,
        },
    }
}

// --- Building the resource list -------------------------------------------

/// Collects the storage locations EmuWiz depends on, from paths the caller
/// has already resolved.
///
/// Resolving a path is not the same as using it: nothing here touches the
/// filesystem, and a path that does not exist is still worth listing, because
/// [`assess_storage`] reports it as unassessed rather than silently dropping
/// it.
///
/// Source folders are included deliberately even though EmuWiz never writes
/// to them: a read-only source is entirely normal and must never be reported
/// as a problem, and the only way to say that confidently is to know the
/// filesystem is read-only.
pub fn storage_resources(
    config: Option<&Config>,
    database_path: Option<&Path>,
    index_path: Option<&Path>,
    transaction_root: Option<&Path>,
    emulator_profile_destinations: &[PathBuf],
) -> Vec<StorageResource> {
    let mut resources = Vec::new();
    if let Some(path) = database_path {
        // The data directory is the database's parent: that is where the
        // index, the install history and the backups all live too.
        if let Some(parent) = path.parent() {
            resources.push(StorageResource {
                role: ResourceRole::DataDirectory,
                path: parent.to_path_buf(),
            });
        }
        resources.push(StorageResource {
            role: ResourceRole::Database,
            path: path.to_path_buf(),
        });
    }
    if let Some(path) = index_path {
        resources.push(StorageResource {
            role: ResourceRole::ArchiveIndex,
            path: path.to_path_buf(),
        });
    }
    if let Some(path) = transaction_root {
        resources.push(StorageResource {
            role: ResourceRole::TransactionStorage,
            path: path.to_path_buf(),
        });
    }
    if let Some(config) = config {
        resources.push(StorageResource {
            role: ResourceRole::MountRoot,
            path: config.mount_root.clone(),
        });
        for folder in &config.source_folders {
            resources.push(StorageResource {
                role: ResourceRole::SourceRoot,
                path: folder.clone(),
            });
        }
    }
    for destination in emulator_profile_destinations {
        resources.push(StorageResource {
            role: ResourceRole::EmulatorProfile,
            path: destination.clone(),
        });
    }
    resources
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::tests::{TempTree, snapshot_tree};
    use std::collections::BTreeSet;

    fn stat(available: u64, total: u64) -> FilesystemStat {
        FilesystemStat {
            available_bytes: available,
            total_bytes: total,
        }
    }

    fn group(role: ResourceRole, stat: Option<FilesystemStat>, mode: MountMode) -> FilesystemGroup {
        FilesystemGroup {
            representative_path: EncodedPath::from_path(Path::new("/tmp/archivefs-test")),
            device_id: Some(42),
            mount_point: Some(EncodedPath::from_path(Path::new("/"))),
            filesystem_type: Some("ext4".to_string()),
            mount_mode: mode,
            stat,
            roles: vec![role],
            paths: vec![EncodedPath::from_path(Path::new("/tmp/archivefs-test"))],
            evidence_source: "statvfs and /proc/self/mountinfo",
        }
    }

    fn assessment(groups: Vec<FilesystemGroup>) -> StorageAssessment {
        StorageAssessment {
            filesystems: groups,
            unassessed: Vec::new(),
            mount_table_available: true,
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    // --- 1. Free disk space -----------------------------------------------

    /// Test 1
    #[test]
    fn filesystem_stat_reports_a_real_temporary_directory() {
        let temporary = TempTree::new("environment");
        let stat = filesystem_stat(temporary.path()).expect("a real directory has a filesystem");
        assert!(
            stat.total_bytes > 0,
            "a mounted filesystem always has a non-zero size"
        );
        assert!(stat.available_bytes <= stat.total_bytes);
    }

    /// Test 2
    #[test]
    fn filesystem_stat_of_a_missing_path_is_none_rather_than_a_guess() {
        let temporary = TempTree::new("environment");
        assert_eq!(filesystem_stat(&temporary.path().join("absent")), None);
    }

    /// Test 3
    #[test]
    fn available_percent_is_none_when_the_total_is_unknown() {
        assert_eq!(stat(0, 0).available_percent(), None);
        assert_eq!(stat(50, 100).available_percent(), Some(50.0));
    }

    /// Test 4
    #[test]
    fn plenty_of_space_produces_no_severity_at_all() {
        let policy = FreeSpacePolicy::default();
        assert_eq!(
            policy.severity(stat(500 * GIB, 1000 * GIB), true, true),
            None
        );
    }

    /// Test 5
    #[test]
    fn below_the_critical_floor_is_critical_for_transactional_data_only() {
        let policy = FreeSpacePolicy::default();
        assert_eq!(
            policy.severity(stat(100 * MIB, 1000 * GIB), true, true),
            Some(DoctorSeverity::Critical),
            "install journals and backups become unrecoverable if a write fails part-way"
        );
        assert_eq!(
            policy.severity(stat(100 * MIB, 1000 * GIB), false, true),
            Some(DoctorSeverity::Error)
        );
    }

    /// Test 6
    #[test]
    fn below_the_error_floor_is_an_error_however_large_the_filesystem() {
        let policy = FreeSpacePolicy::default();
        assert_eq!(
            policy.severity(stat(GIB, 100_000 * GIB), false, true),
            Some(DoctorSeverity::Error)
        );
    }

    /// Test 7
    #[test]
    fn a_small_percentage_of_a_very_large_filesystem_is_not_reported() {
        let policy = FreeSpacePolicy::default();
        // 200 GiB free of 100 TiB is 0.2%, but 200 GiB is plenty of room.
        assert_eq!(
            policy.severity(stat(200 * GIB, 100_000 * GIB), true, true),
            None,
            "percentage bands must not fire while absolute space is ample"
        );
    }

    /// Test 8
    #[test]
    fn percentage_bands_escalate_in_order_once_absolute_space_is_small() {
        let policy = FreeSpacePolicy::default();
        let total = 60 * GIB;
        assert_eq!(
            policy.severity(stat(total * 12 / 100, total), false, true),
            Some(DoctorSeverity::Info)
        );
        assert_eq!(
            policy.severity(stat(total * 8 / 100, total), false, true),
            Some(DoctorSeverity::Warning)
        );
        assert_eq!(
            policy.severity(stat(total * 4 / 100, total), false, true),
            Some(DoctorSeverity::Error)
        );
    }

    /// Test 9
    #[test]
    fn a_full_source_folder_is_never_reported_because_archivefs_never_writes_there() {
        let policy = FreeSpacePolicy::default();
        assert_eq!(policy.severity(stat(0, 1000 * GIB), false, false), None);
        assert!(!ResourceRole::SourceRoot.archivefs_writes_here());
    }

    /// Test 10
    #[test]
    fn resources_on_one_device_are_grouped_into_one_filesystem() {
        let temporary = TempTree::new("environment");
        let first = temporary.path().join("data");
        let second = temporary.path().join("history");
        fs::create_dir_all(&first).expect("fixture");
        fs::create_dir_all(&second).expect("fixture");
        let assessed = assess_storage(&[
            StorageResource::new(ResourceRole::DataDirectory, &first),
            StorageResource::new(ResourceRole::TransactionStorage, &second),
        ]);
        assert_eq!(
            assessed.filesystems.len(),
            1,
            "two directories on one device are one filesystem, not two findings"
        );
        let roles: BTreeSet<_> = assessed.filesystems[0].roles.iter().copied().collect();
        assert!(roles.contains(&ResourceRole::DataDirectory));
        assert!(roles.contains(&ResourceRole::TransactionStorage));
    }

    /// Test 11
    #[test]
    fn a_path_that_does_not_exist_is_unassessed_rather_than_assumed_healthy() {
        let temporary = TempTree::new("environment");
        let assessed = assess_storage(&[StorageResource::new(
            ResourceRole::Database,
            temporary.path().join("library.sqlite3"),
        )]);
        assert!(assessed.filesystems.is_empty());
        assert_eq!(assessed.unassessed.len(), 1);
        assert_eq!(assessed.unassessed[0].role, ResourceRole::Database);
        assert!(
            not_checked_from_storage(&assessed)
                .iter()
                .any(|item| item.reason.contains("does not exist")),
            "an unassessed resource must be stated, not silently dropped"
        );
    }

    /// Test 12
    #[test]
    fn a_low_space_finding_carries_machine_readable_values() {
        let assessed = assessment(vec![group(
            ResourceRole::Database,
            Some(stat(100 * MIB, 100 * GIB)),
            MountMode::ReadWrite,
        )]);
        let findings = findings_from_free_space(&assessed, &FreeSpacePolicy::default());
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.id, "filesystem.critically_low_space");
        assert_eq!(finding.severity, DoctorSeverity::Critical);
        assert_eq!(
            finding.measurements.get("available_bytes"),
            Some(&Measurement::Integer(100 * MIB))
        );
        assert_eq!(
            finding.measurements.get("total_bytes"),
            Some(&Measurement::Integer(100 * GIB))
        );
        assert!(finding.measurements.contains_key("available_percent"));
        assert_eq!(
            finding.measurements.get("filesystem_read_only"),
            Some(&Measurement::Flag(false))
        );
    }

    /// Test 13
    #[test]
    fn byte_counts_are_shown_in_units_a_person_reads() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2 * GIB), "2.0 GiB");
        assert!(format_bytes(100 * GIB).ends_with("GiB"));
    }

    /// Test 14
    #[test]
    fn a_filesystem_whose_capacity_could_not_be_read_produces_no_finding() {
        let assessed = assessment(vec![group(
            ResourceRole::Database,
            None,
            MountMode::ReadWrite,
        )]);
        assert!(
            findings_from_free_space(&assessed, &FreeSpacePolicy::default()).is_empty(),
            "unknown capacity must never be presented as a problem or as healthy"
        );
    }

    // --- 2. Read-only filesystem detection --------------------------------

    /// Test 15
    #[test]
    fn a_read_only_mount_line_is_recognised() {
        let line = b"36 35 0:32 / /mnt/games ro,relatime shared:1 - squashfs /dev/loop0 ro";
        let entry = parse_mount_line(line).expect("a well-formed line parses");
        assert_eq!(entry.mount_point, PathBuf::from("/mnt/games"));
        assert!(entry.read_only_mount);
        assert!(entry.read_only_superblock);
        assert_eq!(entry.mode(), MountMode::ReadOnly);
        assert_eq!(entry.filesystem_type.as_deref(), Some("squashfs"));
    }

    /// Test 16
    #[test]
    fn a_read_write_mount_line_is_recognised() {
        let line = b"25 0 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw";
        let entry = parse_mount_line(line).expect("a well-formed line parses");
        assert_eq!(entry.mode(), MountMode::ReadWrite);
        assert!(!entry.read_only_mount);
    }

    /// Test 17
    #[test]
    fn errors_remount_ro_is_not_mistaken_for_a_read_only_mount() {
        let line = b"25 0 8:2 / / rw,relatime,errors=remount-ro shared:1 - ext4 /dev/sda2 rw";
        let entry = parse_mount_line(line).expect("a well-formed line parses");
        assert_eq!(
            entry.mode(),
            MountMode::ReadWrite,
            "`ro` must match a whole option, never a substring of another option"
        );
    }

    /// Test 18
    #[test]
    fn a_read_only_superblock_under_a_read_write_mount_is_still_read_only() {
        let line = b"36 35 0:32 / /mnt/iso rw,relatime shared:1 - iso9660 /dev/loop0 ro";
        let entry = parse_mount_line(line).expect("a well-formed line parses");
        assert_eq!(entry.mode(), MountMode::ReadOnly);
    }

    /// Test 19
    #[test]
    fn an_octal_escaped_mount_point_is_decoded() {
        let line = b"36 35 0:32 / /mnt/my\\040games rw,relatime shared:1 - ext4 /dev/sdb1 rw";
        let entry = parse_mount_line(line).expect("a well-formed line parses");
        assert_eq!(entry.mount_point, PathBuf::from("/mnt/my games"));
    }

    /// Test 20
    #[test]
    fn a_truncated_mount_line_is_refused_rather_than_half_read() {
        assert!(parse_mount_line(b"25 0 8:2 / / rw").is_none());
        assert!(parse_mount_line(b"").is_none());
    }

    /// Test 21
    #[test]
    fn the_longest_matching_mount_point_wins() {
        let table = parse_mount_table(
            b"25 0 8:2 / / rw,relatime shared:1 - ext4 /dev/sda2 rw\n\
              36 25 8:3 / /mnt rw,relatime shared:2 - ext4 /dev/sda3 rw\n\
              37 36 0:32 / /mnt/games ro,relatime shared:3 - squashfs /dev/loop0 ro\n",
        );
        assert_eq!(table.len(), 3);
        let entry = mount_entry_for_path(&table, Path::new("/mnt/games/psx"))
            .expect("a nested path resolves to its nearest mount");
        assert_eq!(entry.mount_point, PathBuf::from("/mnt/games"));
        assert_eq!(entry.mode(), MountMode::ReadOnly);
    }

    /// Test 22
    #[test]
    fn a_read_only_filesystem_archivefs_writes_to_is_an_error() {
        let assessed = assessment(vec![group(
            ResourceRole::Database,
            Some(stat(500 * GIB, 1000 * GIB)),
            MountMode::ReadOnly,
        )]);
        let findings = findings_from_read_only_filesystems(&assessed);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].id, "filesystem.read_only");
        assert_eq!(findings[0].severity, DoctorSeverity::Error);
        assert_eq!(
            findings[0].measurements.get("filesystem_read_only"),
            Some(&Measurement::Flag(true))
        );
        assert!(
            findings[0]
                .why_it_matters
                .as_deref()
                .expect("guidance is always present")
                .contains("not a permissions problem"),
            "a person must not be sent off to change permissions that cannot help"
        );
    }

    /// Test 23
    #[test]
    fn a_read_only_source_folder_is_never_reported_as_a_problem() {
        let assessed = assessment(vec![group(
            ResourceRole::SourceRoot,
            Some(stat(0, 1000 * GIB)),
            MountMode::ReadOnly,
        )]);
        assert!(
            findings_from_read_only_filesystems(&assessed).is_empty(),
            "a read-only game library is entirely normal"
        );
        assert!(findings_from_free_space(&assessed, &FreeSpacePolicy::default()).is_empty());
    }

    /// Test 24
    #[test]
    fn a_read_only_emulator_profile_is_a_warning_not_an_error() {
        let assessed = assessment(vec![group(
            ResourceRole::EmulatorProfile,
            Some(stat(500 * GIB, 1000 * GIB)),
            MountMode::ReadOnly,
        )]);
        let findings = findings_from_read_only_filesystems(&assessed);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].severity,
            DoctorSeverity::Warning,
            "a read-only emulator profile blocks cheats, not EmuWiz itself"
        );
    }

    /// Test 25
    #[test]
    fn an_unknown_mount_mode_produces_no_read_only_finding() {
        let assessed = assessment(vec![group(
            ResourceRole::Database,
            Some(stat(500 * GIB, 1000 * GIB)),
            MountMode::Unknown,
        )]);
        assert!(findings_from_read_only_filesystems(&assessed).is_empty());
    }

    /// Test 26
    #[test]
    fn a_missing_mount_table_is_stated_rather_than_assumed_read_write() {
        let mut assessed = assessment(vec![group(
            ResourceRole::Database,
            Some(stat(500 * GIB, 1000 * GIB)),
            MountMode::Unknown,
        )]);
        assessed.mount_table_available = false;
        assert!(
            not_checked_from_storage(&assessed)
                .iter()
                .any(|item| item.name == "Filesystem mount state"
                    && item.reason.contains("mount table could not be read")),
            "without a mount table, read-only detection must be declared unavailable"
        );
    }

    // --- 3. Writability from metadata only --------------------------------

    /// Test 27
    #[test]
    fn a_writable_directory_appears_writable_and_is_never_claimed_as_proven() {
        let temporary = TempTree::new("environment");
        let permissions = assess_permissions(temporary.path());
        let assessed = assess_writability(true, true, false, MountMode::ReadWrite, permissions);
        assert_eq!(assessed, WritabilityAssessment::AppearsWritable);
        assert!(
            assessed.label().contains("Appears"),
            "metadata can only ever support `appears`, never `is`"
        );
    }

    /// Test 28
    #[test]
    fn a_missing_destination_is_reported_as_missing_not_as_denied() {
        assert_eq!(
            assess_writability(false, false, false, MountMode::ReadWrite, None),
            WritabilityAssessment::MissingDestination
        );
    }

    /// Test 29
    #[test]
    fn a_read_only_mount_outranks_permissions_that_look_fine() {
        let permissions = PathPermissions {
            mode: Some(0o755),
            owner_uid: Some(0),
            group_gid: Some(0),
            owned_by_current_user: Some(true),
            current_user_may_write: Some(true),
        };
        assert_eq!(
            assess_writability(true, true, false, MountMode::ReadOnly, Some(permissions)),
            WritabilityAssessment::ReadOnlyFilesystem,
            "permissions cannot make a read-only mount writable, so they must not be reported as if they could"
        );
    }

    /// Test 30
    #[test]
    fn permissions_that_deny_writing_are_reported_as_denied() {
        let permissions = PathPermissions {
            mode: Some(0o555),
            owner_uid: Some(0),
            group_gid: Some(0),
            owned_by_current_user: Some(false),
            current_user_may_write: Some(false),
        };
        assert_eq!(
            assess_writability(true, true, false, MountMode::ReadWrite, Some(permissions)),
            WritabilityAssessment::PermissionDenied
        );
    }

    /// Test 31
    #[test]
    fn a_symlink_or_a_file_where_a_directory_belongs_is_unsafe() {
        assert_eq!(
            assess_writability(true, true, true, MountMode::ReadWrite, None),
            WritabilityAssessment::UnsafeDestination
        );
        assert_eq!(
            assess_writability(true, false, false, MountMode::ReadWrite, None),
            WritabilityAssessment::UnsafeDestination
        );
    }

    /// Test 32
    #[test]
    fn an_unknown_mount_mode_leaves_writability_unproven() {
        assert_eq!(
            assess_writability(true, true, false, MountMode::Unknown, None),
            WritabilityAssessment::NotProven
        );
        assert!(
            WritabilityAssessment::NotProven
                .label()
                .contains("not proven")
        );
    }

    /// Test 33
    #[test]
    fn permissions_are_read_from_metadata_and_report_the_real_mode() {
        let temporary = TempTree::new("environment");
        let permissions =
            assess_permissions(temporary.path()).expect("a real directory has a mode");
        assert!(permissions.mode.is_some());
        assert_eq!(permissions.owned_by_current_user, Some(true));
        assert_eq!(permissions.current_user_may_write, Some(true));
    }

    /// Test 34
    #[test]
    fn permissions_of_a_missing_path_are_unknown_rather_than_denied() {
        let temporary = TempTree::new("environment");
        assert!(assess_permissions(&temporary.path().join("absent")).is_none());
    }

    // --- Read-only proof --------------------------------------------------

    /// Read-only proof 1: assessing storage changes nothing on disk.
    #[test]
    fn assessing_storage_leaves_the_tree_byte_for_byte_unchanged() {
        let temporary = TempTree::new("environment");
        let data = temporary.path().join("data");
        fs::create_dir_all(&data).expect("fixture");
        fs::write(data.join("library.sqlite3"), b"not a real database").expect("fixture");
        let before = snapshot_tree(temporary.path());

        let assessed = assess_storage(&[
            StorageResource::new(ResourceRole::DataDirectory, &data),
            StorageResource::new(ResourceRole::Database, data.join("library.sqlite3")),
            StorageResource::new(ResourceRole::MountRoot, temporary.path().join("mounts")),
        ]);
        let _ = findings_from_free_space(&assessed, &FreeSpacePolicy::default());
        let _ = findings_from_read_only_filesystems(&assessed);

        assert_eq!(
            snapshot_tree(temporary.path()),
            before,
            "a storage assessment must not create, remove or modify anything"
        );
    }

    /// Read-only proof 2: no probe file is ever created, even where one would
    /// answer the question outright.
    #[test]
    fn assessing_writability_creates_no_probe_file() {
        let temporary = TempTree::new("environment");
        let before = snapshot_tree(temporary.path());
        let permissions = assess_permissions(temporary.path());
        let _ = assess_writability(true, true, false, MountMode::ReadWrite, permissions);
        assert_eq!(
            snapshot_tree(temporary.path()),
            before,
            "writability is assessed from metadata; a probe file would prove more but change the tree"
        );
    }

    /// Read-only proof 3: this module contains no write, permission-changing
    /// or process-spawning call at all.
    #[test]
    fn this_module_contains_no_mutating_call() {
        // Only the production half of the file: the tests below deliberately
        // create fixture trees to prove the production code leaves them alone.
        let whole = include_str!("environment.rs");
        let source = whole
            .split_once("#[cfg(test)]")
            .expect("this file ends with its own test module")
            .0;
        for forbidden in [
            "fs::write",
            "fs::create_dir",
            "fs::remove_",
            "fs::set_permissions",
            "File::create",
            "OpenOptions",
            "libc::access",
            "libc::chmod",
            "libc::chown",
            "libc::mount",
            "Command",
            "ureq",
        ] {
            assert!(
                !source.contains(forbidden),
                "`{forbidden}` must never appear in a read-only diagnostic module"
            );
        }
    }
}
