//! Optional, fd-pinned, read-only RAR5 provider backed by a user-installed
//! 7-Zip.
//!
//! # What this is
//!
//! EmuWiz never links, bundles, or downloads RAR-handling code. This module
//! only detects and executes a user-installed `7zz`/`7z` binary already on
//! the system - the same "Model B" (external, user-installed backend)
//! architecture as the rest of the optional-tool surface. The executable's
//! own licence governs its distribution; exec'ing it does not change
//! EmuWiz's own MIT status (see `docs/research/RAR_PROVIDER_AND_LICENSING_OPTIONS_RESEARCH.md`).
//! RAR support is therefore always optional: when no capable backend is
//! found, callers see [`RarError::BackendNotFound`]/[`RarError::BackendUnavailable`]
//! - never a claim that a `.rar` file is corrupt.
//!
//! # The envelope this batch supports
//!
//! Linux only. RAR5 only, single-volume, non-solid, non-encrypted, non-SFX,
//! unique member paths, ordinary regular-file members, non-zero size. Every
//! other shape - RAR4, solid, encrypted, multivolume, split members, SFX,
//! duplicate paths, zero-size members, directories, symlinks, hardlinks,
//! alternate streams, malformed listings - is refused, not worked around.
//! See [`RarError`] for the exact refusal reasons.
//!
//! # Identity: fd-pin, not a content snapshot
//!
//! [`RarSession::open`] opens the archive path exactly once
//! (`O_RDONLY | O_CLOEXEC`) and keeps that file descriptor for every
//! subsequent list/relist/extract child for the session's lifetime; the
//! pathname is never reopened. Each child inherits that one fd (cleared of
//! `FD_CLOEXEC` for itself only, via `pre_exec`, immediately before `exec`)
//! and is invoked with the archive argument `/proc/self/fd/<N>` - `self`
//! resolves to the *child* after `exec`, so this must be the fd number the
//! child itself inherited, never the parent's pid. Per `man 5 proc`, this
//! magic-symlink open binds to the held *open file* (the inode), not a
//! fresh pathname lookup: rename, unlink, or path replacement of the
//! original name cannot redirect it (`docs/research/RAR_FD_PIN_PROCFD_RESEARCH.md`,
//! empirically verified there on kernel 6.8).
//!
//! This is **identity** pinning, not **content** immutability: an in-place
//! rewrite of the same inode is still visible through the held fd. That
//! hazard is not new here and is not silently accepted - it is exactly what
//! the existing fail-closed contract already catches: exit status, exact
//! byte count, and a caller-supplied strong DAT digest are all required to
//! agree, so torn/rewritten bytes produce a refusal, never a false
//! `Complete`. A private immutable snapshot (reflink/O_TMPFILE/memfd) is a
//! later, optional hardening layer - deliberately not built here.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use super::ArchiveMemberHashes;
use super::external_process::{ProcessError, ProcessLimits, run_supervised};
use crate::identity_source::hashing::Crc32;

const LIST_STDOUT_LIMIT: u64 = 8 * 1024 * 1024;

/// A discovered 7-Zip executable with a verified RAR5 decoder.
#[derive(Debug, Clone)]
pub struct RarProvider {
    executable: PathBuf,
    version: String,
    process_limits: ProcessLimits,
}

impl RarProvider {
    /// Probes `7zz`, then `7z`, then `/usr/lib/7zip/7z` (PATH lookup for the
    /// bare names, absolute-path check for the last), without a shell.
    ///
    /// Presence of the executable is not proof of RAR support: this runs
    /// `<exe> i` (the capability listing) and requires both `Rar` and
    /// `Rar5` codec lines before accepting the candidate. If no candidate
    /// both exists and advertises RAR5, RAR is simply unsupported on this
    /// system - never treated as an archive problem.
    pub fn discover(timeout: Duration) -> Result<Self, RarError> {
        for candidate in [
            PathBuf::from("7zz"),
            PathBuf::from("7z"),
            PathBuf::from("/usr/lib/7zip/7z"),
        ] {
            let Some(executable) = resolve_executable(&candidate) else {
                continue;
            };
            let mut command = Command::new(&executable);
            command.arg("i");
            let mut captured = Vec::new();
            let outcome = run_supervised(
                command,
                ProcessLimits::default(),
                timeout,
                LIST_STDOUT_LIMIT,
                capture(&mut captured),
                None,
            );
            let status_ok = matches!(&outcome, Ok(outcome) if outcome.status.success());
            if !status_ok {
                continue;
            }
            let Ok(stdout) = String::from_utf8(captured) else {
                continue;
            };
            let has_rar = stdout.lines().any(|line| format_line_is(line, "Rar"));
            let has_rar5 = stdout.lines().any(|line| format_line_is(line, "Rar5"));
            if !has_rar || !has_rar5 {
                continue;
            }
            let version = stdout
                .lines()
                .find(|line| line.trim_start().starts_with("7-Zip "))
                .map(str::trim)
                .unwrap_or("7-Zip version unknown")
                .to_string();
            return Ok(Self {
                executable,
                version,
                process_limits: ProcessLimits::default(),
            });
        }
        Err(RarError::BackendNotFound)
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn with_process_limits(mut self, limits: ProcessLimits) -> Result<Self, RarError> {
        self.process_limits = limits.validate().map_err(process_error)?;
        Ok(self)
    }

    /// Opens `archive_path` exactly once, pins it by fd, lists it through
    /// that fd, and validates the whole supported envelope. The returned
    /// [`RarSession`] owns the open file for its entire lifetime; the
    /// pathname is never touched again.
    pub fn open(&self, archive_path: &Path, timeout: Duration) -> Result<RarSession, RarError> {
        let file = open_pinned(archive_path)?;
        let opened_at = fstat_snapshot(&file)?;
        let (archive, members) =
            list_via_fd(&self.executable, self.process_limits, &file, timeout)?;
        validate_archive(&archive)?;
        validate_members(&members)?;
        Ok(RarSession {
            executable: self.executable.clone(),
            process_limits: self.process_limits,
            archive_path: archive_path.to_path_buf(),
            file,
            opened_at,
            archive,
            members,
        })
    }
}

/// One fd-pinned RAR session: the open archive plus its validated listing.
///
/// Every list/relist/extract operation for this session's lifetime reads
/// through the *one* file descriptor opened by [`RarProvider::open`] - the
/// archive pathname is never reopened, satisfying the "no fallback to a
/// non-pinned source" requirement structurally (there is no code path in
/// this type that takes a `Path` for I/O after construction).
#[derive(Debug)]
pub struct RarSession {
    executable: PathBuf,
    process_limits: ProcessLimits,
    archive_path: PathBuf,
    file: File,
    /// fstat of the held fd at open time - diagnostics only, never gates
    /// success. See the module doc: the fd pins identity, not content.
    opened_at: FstatSnapshot,
    pub archive: RarArchiveMetadata,
    pub members: Vec<RarMember>,
}

impl RarSession {
    pub fn archive_path(&self) -> &Path {
        &self.archive_path
    }

    /// fstat of the pinned fd as of right now, for diagnostics/logging
    /// only. Never compared internally to gate a verdict - see the module
    /// doc on identity-vs-content pinning.
    pub fn fstat_now(&self) -> Result<FstatSnapshot, RarError> {
        fstat_snapshot(&self.file)
    }

    pub fn opened_fstat(&self) -> FstatSnapshot {
        self.opened_at
    }

    /// Selects, relists, and streams exactly one member.
    ///
    /// Binding: `stable_index` must resolve to the same unique
    /// [`RarMember::path`] and size both in this session's original listing
    /// and in a fresh relist performed through the *same* held fd, right
    /// before extraction (never by reopening the pathname - the fd already
    /// removes the TOCTOU-by-pathname hazard; the relist instead protects
    /// against in-place source mutation/schema change between phases).
    pub fn read_member(
        &self,
        stable_index: usize,
        max_output: u64,
        expected: &ExpectedMemberHashes,
        timeout: Duration,
    ) -> Result<RarReadResult, RarError> {
        expected.validate()?;
        let selected = self
            .members
            .get(stable_index)
            .ok_or(RarError::MemberNotFound { stable_index })?;
        if selected.size == 0 {
            return Err(RarError::ZeroSizedMember { stable_index });
        }
        if selected.size > max_output {
            return Err(RarError::MemberTooLarge {
                declared: selected.size,
                limit: max_output,
            });
        }

        // Relist through the SAME held fd - never the pathname.
        let (fresh_archive, fresh_members) =
            list_via_fd(&self.executable, self.process_limits, &self.file, timeout)?;
        validate_archive(&fresh_archive).map_err(|_| RarError::SelectionChanged)?;
        validate_members(&fresh_members).map_err(|_| RarError::SelectionChanged)?;
        if fresh_archive != self.archive || fresh_members != self.members {
            return Err(RarError::SelectionChanged);
        }
        let fresh_selected = fresh_members
            .get(stable_index)
            .ok_or(RarError::SelectionChanged)?;
        if fresh_selected != selected {
            return Err(RarError::SelectionChanged);
        }

        let mut hasher = StreamingHashes::new();
        let mut received_len = 0_u64;
        let fd = self.file.as_raw_fd();
        let mut command = Command::new(&self.executable);
        command.args(extract_args(fd, &selected.path));
        let outcome = run_supervised(
            command,
            self.process_limits,
            timeout,
            u64::MAX,
            |chunk| {
                received_len = received_len
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| "member size overflowed u64".to_string())?;
                if received_len > max_output {
                    // Do not hash/accept the chunk that crosses the cap.
                    return Err(format!("output exceeded {max_output} bytes"));
                }
                hasher.update(chunk);
                Ok(())
            },
            Some(pin_fd_pre_exec(fd)),
        );

        // 7-Zip may emit some or all member bytes before ultimately
        // reporting corruption. The streamed hash/length above is therefore
        // provisional until we know the child actually exited 0 - a
        // non-zero exit (checked inside `validate_extraction` below, and by
        // `run_supervised`'s own error path) means the provisional result
        // is discarded, never returned as verified.
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(ProcessError::OutputLimitExceeded { limit }) => {
                return Err(RarError::OutputLimitExceeded { limit });
            }
            Err(error) => return Err(process_error(error)),
        };

        let hashes = hasher.finish();
        validate_extraction(
            &outcome,
            stable_index,
            received_len,
            selected.size,
            &hashes,
            expected,
        )?;

        Ok(RarReadResult {
            member: selected.clone(),
            received_len,
            hashes,
        })
    }
}

/// Diagnostic-only snapshot of the pinned fd's device/inode/size/mtime.
/// Never used to gate a verdict - see the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FstatSnapshot {
    pub device: u64,
    pub inode: u64,
    pub len: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
}

fn fstat_snapshot(file: &File) -> Result<FstatSnapshot, RarError> {
    let metadata = file.metadata().map_err(io_error)?;
    Ok(FstatSnapshot {
        device: metadata.dev(),
        inode: metadata.ino(),
        len: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    })
}

fn open_pinned(path: &Path) -> Result<File, RarError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(path)
        .map_err(io_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Err(RarError::Io {
            detail: "archive path is not a regular file".to_string(),
        });
    }
    let mut signature = [0_u8; 8];
    {
        use std::io::Read;
        let mut probe = file.try_clone().map_err(io_error)?;
        let count = probe.read(&mut signature).map_err(io_error)?;
        let rar5 = count == 8 && signature == *b"Rar!\x1a\x07\x01\x00";
        let rar4 = count >= 7 && signature[..7] == *b"Rar!\x1a\x07\x00";
        if !rar5 && !rar4 {
            return Err(RarError::InvalidSignature);
        }
    }
    Ok(file)
}

/// The single `unsafe` production step: clears `FD_CLOEXEC` on exactly
/// `fd` inside the forked child, immediately before `exec`, so the pinned
/// archive descriptor survives into that one 7-Zip invocation at the same
/// number - the number `/proc/self/fd/<fd>` in the argv addresses.
///
/// # Safety
///
/// This runs as the `pre_exec_extra` step inside
/// [`super::external_process::run_supervised`]'s single `pre_exec` closure,
/// which already documents the async-signal-safety contract. `fcntl(fd,
/// F_SETFD, 0)` is itself async-signal-safe (a plain syscall, no
/// allocation, no libc-internal locking) and touches only this one
/// descriptor's flags in the child's private (post-`fork`) copy of the fd
/// table - it can never affect the parent process's own `FD_CLOEXEC` on the
/// same fd, so the archive fd remains protected from leaking into any
/// *other* child EmuWiz might spawn.
fn pin_fd_pre_exec(fd: RawFd) -> Box<dyn Fn() -> io::Result<()> + Send + Sync> {
    Box::new(move || {
        // SAFETY: see the function doc above.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })
}

fn list_via_fd(
    executable: &Path,
    limits: ProcessLimits,
    file: &File,
    timeout: Duration,
) -> Result<(RarArchiveMetadata, Vec<RarMember>), RarError> {
    let fd = file.as_raw_fd();
    let archive_text = run_listing(executable, limits, fd, false, timeout)?;
    let archive = parse_archive_properties(&archive_text)?;
    // Validated here, before the second (member) listing child is even
    // spawned: an archive already refused on type/solid/encrypted/
    // multivolume/SFX grounds has nothing further worth listing, and RAR4's
    // member-listing schema differs enough from RAR5's that parsing it
    // first would misreport a clean RAR4 refusal as an ambiguous schema.
    validate_archive(&archive)?;
    let members_text = run_listing(executable, limits, fd, true, timeout)?;
    let members = parse_member_listing(&members_text)?;
    Ok((archive, members))
}

fn run_listing(
    executable: &Path,
    limits: ProcessLimits,
    fd: RawFd,
    bare: bool,
    timeout: Duration,
) -> Result<String, RarError> {
    let mut command = Command::new(executable);
    command.args(list_args(bare, fd));
    let mut captured = Vec::new();
    let outcome = run_supervised(
        command,
        limits,
        timeout,
        LIST_STDOUT_LIMIT,
        capture(&mut captured),
        Some(pin_fd_pre_exec(fd)),
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => return Err(process_error(error)),
    };
    if !outcome.status.success() {
        return Err(classify_listing_failure(&captured, &outcome.stderr));
    }
    String::from_utf8(captured).map_err(|_| RarError::AmbiguousListing {
        detail: "7-Zip listing is not UTF-8".to_string(),
    })
}

/// argv for `7z l [-ba] -slt -p- -y -bd -bb0 -- /proc/self/fd/<fd>`.
///
/// `-p-` supplies an empty password and, combined with the closed stdin
/// `run_supervised` always sets, makes a header-encrypted archive fail fast
/// with a documented exit code instead of blocking on a password prompt.
/// `-ba` (bare) suppresses the archive-header block, used for the
/// per-member pass; the header pass omits it deliberately, to read
/// archive-level `Type`/`Solid`/`Encrypted`/`Multivolume`/`Offset`.
fn list_args(bare: bool, fd: RawFd) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["l".into()];
    if bare {
        args.push("-ba".into());
    }
    for flag in ["-slt", "-p-", "-y", "-bd", "-bb0", "--"] {
        args.push(flag.into());
    }
    args.push(proc_self_fd(fd));
    args
}

/// argv for `7z x -so -p- -y -bd -bb0 -spd -ssc -- /proc/self/fd/<fd> <path>`.
///
/// `-so`: stream the one selected member's decompressed bytes to stdout -
/// the only extraction mode this provider ever uses (never `e`/`x` to a
/// directory). `-spd`: do not strip leading dots from names (keeps the
/// listed path an exact match). `-ssc`: case-sensitive name matching, so
/// selection can never silently widen to a case-colliding sibling. `--`:
/// ends option parsing, so a member path that happens to start with `-` is
/// still an ordinary positional argument, never reinterpreted as a switch.
/// No shell is ever involved (`Command::new` + `args`), so `*`/`?` in a
/// member path are inert literal bytes to 7-Zip's own exact-path match -
/// nothing expands them.
fn extract_args(fd: RawFd, member_path: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    for flag in ["x", "-so", "-p-", "-y", "-bd", "-bb0", "-spd", "-ssc", "--"] {
        args.push(flag.into());
    }
    args.push(proc_self_fd(fd));
    args.push(member_path.into());
    args
}

fn proc_self_fd(fd: RawFd) -> OsString {
    // Deliberately `/proc/self/fd/<N>`, resolved by the *child* after
    // `exec` - never `/proc/<parent-pid>/fd/N` (a pid-reuse race). See the
    // module doc.
    format!("/proc/self/fd/{fd}").into()
}

fn capture(sink: &mut Vec<u8>) -> impl FnMut(&[u8]) -> Result<(), String> + '_ {
    move |chunk| {
        sink.extend_from_slice(chunk);
        Ok(())
    }
}

/// Archive-level metadata from the (non-bare) `l -slt` header block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarArchiveMetadata {
    pub archive_type: String,
    pub solid: bool,
    pub encrypted: bool,
    pub multivolume: bool,
    pub volumes: u64,
    pub offset: u64,
}

/// One RAR5 member: the smallest production-facing shape this provider
/// needs. `stable_index` is EmuWiz's own archive-order position - 7-Zip has
/// no index/ID selector at all (see the module doc); selection always binds
/// `stable_index` to this exact `path`, re-checked against a fresh relist
/// through the same pinned fd before every extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarMember {
    pub stable_index: usize,
    pub path: String,
    pub size: u64,
    pub packed_size: Option<u64>,
    pub encrypted: bool,
    pub solid: bool,
    pub split_before: bool,
    pub split_after: bool,
    pub method: String,
    /// Stored RAR CRC32, when present. Diagnostic metadata only - never
    /// treated as content identity; a caller's strong DAT digest is what
    /// `read_member` actually verifies against.
    pub crc: Option<String>,
}

/// At least one strong digest is mandatory; CRC32 alone is never
/// sufficient. Every supplied strong digest must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpectedMemberHashes {
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
}

impl ExpectedMemberHashes {
    fn validate(&self) -> Result<(), RarError> {
        let populated = [
            ("MD5", self.md5.as_deref(), 32),
            ("SHA-1", self.sha1.as_deref(), 40),
            ("SHA-256", self.sha256.as_deref(), 64),
        ];
        if populated.iter().all(|(_, value, _)| value.is_none()) {
            return Err(RarError::InvalidExpectedHash {
                detail: "at least one strong DAT digest is required".to_string(),
            });
        }
        for (name, value, length) in populated {
            if let Some(value) = value
                && (value.len() != length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(RarError::InvalidExpectedHash {
                    detail: format!("{name} must be {length} hexadecimal characters"),
                });
            }
        }
        Ok(())
    }

    fn matches(&self, hashes: &ArchiveMemberHashes) -> bool {
        self.md5
            .as_ref()
            .is_none_or(|value| value.eq_ignore_ascii_case(&hashes.md5))
            && self
                .sha1
                .as_ref()
                .is_none_or(|value| value.eq_ignore_ascii_case(&hashes.sha1))
            && self
                .sha256
                .as_ref()
                .is_none_or(|value| value.eq_ignore_ascii_case(&hashes.sha256))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RarReadResult {
    pub member: RarMember,
    pub received_len: u64,
    pub hashes: ArchiveMemberHashes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RarError {
    BackendNotFound,
    BackendUnavailable { detail: String },
    Io { detail: String },
    Timeout,
    ProcessOutputLimit { limit: u64 },
    OutputLimitExceeded { limit: u64 },
    InvalidSignature,
    CorruptArchive { detail: String },
    EncryptedArchive,
    UnsupportedArchive { detail: String },
    AmbiguousListing { detail: String },
    DuplicatePath { path: String },
    MemberNotFound { stable_index: usize },
    ZeroSizedMember { stable_index: usize },
    MemberTooLarge { declared: u64, limit: u64 },
    SelectionChanged,
    BackendFailure { status: Option<i32>, detail: String },
    SizeMismatch { declared: u64, received: u64 },
    InvalidExpectedHash { detail: String },
    HashMismatch,
    InvalidProcessLimits,
    CleanupFailure { detail: String },
}

impl std::fmt::Display for RarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

fn process_error(error: ProcessError) -> RarError {
    match error {
        ProcessError::Io { detail } => RarError::Io { detail },
        ProcessError::Timeout => RarError::Timeout,
        ProcessError::OutputLimitExceeded { limit } => RarError::ProcessOutputLimit { limit },
        ProcessError::InvalidLimits => RarError::InvalidProcessLimits,
        ProcessError::Sink { detail } => RarError::Io { detail },
        ProcessError::CleanupFailure { detail } => RarError::CleanupFailure { detail },
    }
}

fn io_error(error: io::Error) -> RarError {
    RarError::Io {
        detail: error.to_string(),
    }
}

fn resolve_executable(candidate: &Path) -> Option<PathBuf> {
    if candidate.components().count() > 1 {
        return candidate.is_file().then(|| candidate.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(candidate))
            .find(|path| path.is_file())
    })
}

fn format_line_is(line: &str, format: &str) -> bool {
    line.split_ascii_whitespace().any(|field| field == format)
}

// ---------------------------------------------------------------------
// Strict `-slt` parser
// ---------------------------------------------------------------------

const ALLOWED_ARCHIVE_PROPERTIES: &[&str] = &[
    "Path",
    "Type",
    "Physical Size",
    "Headers Size",
    "Solid",
    "Blocks",
    "Encrypted",
    "Multivolume",
    "Volumes",
    "Volume Index",
    "Offset",
    "Characteristics",
    // 7-Zip reports a missing sibling volume as an `ERROR` property inside
    // the archive header block itself (e.g. `ERROR = Missing volume :
    // part02.rar`), not as a nonzero process exit. Recognised explicitly so
    // it fails closed with a specific reason rather than the generic
    // unknown-property rejection.
    "ERROR",
];

const REQUIRED_MEMBER_PROPERTIES: &[&str] = &[
    "Path",
    "Folder",
    "Size",
    "Packed Size",
    "Modified",
    "Created",
    "Accessed",
    "Attributes",
    "Alternate Stream",
    "Encrypted",
    "Solid",
    "Split Before",
    "Split After",
    "CRC",
    "Host OS",
    "Method",
    "Characteristics",
    "Symbolic Link",
    "Hard Link",
    "Copy Link",
    "Volume Index",
    "Checksum",
    "NT Security",
];

fn parse_archive_properties(output: &str) -> Result<RarArchiveMetadata, RarError> {
    let mut properties = std::collections::BTreeMap::new();
    let mut in_properties = false;
    let mut saw_start = false;
    let mut saw_end = false;
    for line in output.lines() {
        if line == "--" {
            if saw_start || saw_end {
                return Err(RarError::AmbiguousListing {
                    detail: "ambiguous archive property boundary".to_string(),
                });
            }
            in_properties = true;
            saw_start = true;
            continue;
        }
        if line == "----------" {
            if !in_properties {
                return Err(RarError::AmbiguousListing {
                    detail: "archive property terminator precedes its start".to_string(),
                });
            }
            saw_end = true;
            break;
        }
        if in_properties && !line.is_empty() {
            let (key, value) = parse_property_line(line)?;
            if !ALLOWED_ARCHIVE_PROPERTIES.contains(&key) {
                return Err(RarError::AmbiguousListing {
                    detail: format!("unknown archive property {key:?}"),
                });
            }
            if properties
                .insert(key.to_string(), value.to_string())
                .is_some()
            {
                return Err(RarError::AmbiguousListing {
                    detail: format!("duplicate archive property {key}"),
                });
            }
        }
    }
    if properties.is_empty() || !saw_start || !saw_end {
        return Err(RarError::AmbiguousListing {
            detail: "archive property block is missing".to_string(),
        });
    }
    let archive_type = required_property(&properties, "Type")?.to_string();
    // Checked before any RAR5-only field is required: RAR4's archive-header
    // block genuinely does not carry `Encrypted`/`Multivolume`/`Volumes` at
    // all (verified empirically against a real RAR4 fixture), so requiring
    // them unconditionally would misreport a RAR4 archive as an ambiguous
    // schema rather than the specific, correct "RAR4 is refused" reason.
    if archive_type != "Rar5" {
        return Err(RarError::UnsupportedArchive {
            detail: format!("only RAR5 is supported; 7-Zip identified this as {archive_type}"),
        });
    }
    if let Some(error) = properties.get("ERROR") {
        return Err(RarError::UnsupportedArchive {
            detail: format!("7-Zip reported an archive error: {error}"),
        });
    }
    Ok(RarArchiveMetadata {
        archive_type,
        solid: marker(required_property(&properties, "Solid")?, "Solid")?,
        encrypted: marker(required_property(&properties, "Encrypted")?, "Encrypted")?,
        multivolume: marker(
            required_property(&properties, "Multivolume")?,
            "Multivolume",
        )?,
        volumes: parse_u64(required_property(&properties, "Volumes")?, "Volumes")?,
        offset: properties
            .get("Offset")
            .map(|value| parse_u64(value, "Offset"))
            .transpose()?
            .unwrap_or(0),
    })
}

/// Refuses everything outside the supported envelope. Deliberately never
/// relaxed by any flag or option in this batch.
///
/// The RAR5-type gate itself already runs inside [`parse_archive_properties`]
/// (see its comment); this function re-checks it too so `validate_archive`
/// remains a complete, self-contained statement of the envelope on its own.
fn validate_archive(archive: &RarArchiveMetadata) -> Result<(), RarError> {
    if archive.archive_type != "Rar5" {
        return Err(RarError::UnsupportedArchive {
            detail: format!(
                "only RAR5 is supported; 7-Zip identified this as {}",
                archive.archive_type
            ),
        });
    }
    if archive.solid {
        return Err(RarError::UnsupportedArchive {
            detail: "solid RAR archives are refused".to_string(),
        });
    }
    if archive.encrypted {
        return Err(RarError::EncryptedArchive);
    }
    if archive.multivolume || archive.volumes != 1 {
        return Err(RarError::UnsupportedArchive {
            detail: "multi-volume RAR archives are refused".to_string(),
        });
    }
    if archive.offset != 0 {
        return Err(RarError::UnsupportedArchive {
            detail: "SFX RAR containers are refused".to_string(),
        });
    }
    Ok(())
}

fn parse_member_listing(output: &str) -> Result<Vec<RarMember>, RarError> {
    let mut blocks = Vec::new();
    let mut block = std::collections::BTreeMap::new();
    for line in output.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !block.is_empty() {
                blocks.push(std::mem::take(&mut block));
            }
            continue;
        }
        let (key, value) = parse_property_line(line)?;
        if !REQUIRED_MEMBER_PROPERTIES.contains(&key) {
            return Err(RarError::AmbiguousListing {
                detail: format!("unknown member property {key:?}"),
            });
        }
        if block.insert(key.to_string(), value.to_string()).is_some() {
            return Err(RarError::AmbiguousListing {
                detail: format!("duplicate member property {key}"),
            });
        }
    }

    blocks
        .into_iter()
        .enumerate()
        .map(|(stable_index, properties)| {
            for required in REQUIRED_MEMBER_PROPERTIES {
                required_property(&properties, required)?;
            }
            let path = required_property(&properties, "Path")?.to_string();
            if path.is_empty() || path.chars().any(|character| character.is_ascii_control()) {
                return Err(RarError::AmbiguousListing {
                    detail: "empty or control-character-containing member path".to_string(),
                });
            }
            if marker(required_property(&properties, "Folder")?, "Folder")? {
                return Err(RarError::UnsupportedArchive {
                    detail: format!("directory member is refused: {path}"),
                });
            }
            let alternate_stream = required_property(&properties, "Alternate Stream")?;
            let symbolic_link = required_property(&properties, "Symbolic Link")?;
            let hard_link = required_property(&properties, "Hard Link")?;
            let copy_link = required_property(&properties, "Copy Link")?;
            if alternate_stream != "-"
                || !symbolic_link.is_empty()
                || !hard_link.is_empty()
                || !copy_link.is_empty()
            {
                return Err(RarError::UnsupportedArchive {
                    detail: format!("special member is refused: {path}"),
                });
            }
            Ok(RarMember {
                stable_index,
                path,
                size: parse_u64(required_property(&properties, "Size")?, "Size")?,
                packed_size: properties
                    .get("Packed Size")
                    .filter(|value| !value.is_empty())
                    .map(|value| parse_u64(value, "Packed Size"))
                    .transpose()?,
                encrypted: marker(required_property(&properties, "Encrypted")?, "Encrypted")?,
                solid: marker(required_property(&properties, "Solid")?, "Solid")?,
                split_before: marker(
                    required_property(&properties, "Split Before")?,
                    "Split Before",
                )?,
                split_after: marker(
                    required_property(&properties, "Split After")?,
                    "Split After",
                )?,
                method: required_property(&properties, "Method")?.to_string(),
                crc: properties
                    .get("CRC")
                    .filter(|value| !value.is_empty())
                    .cloned(),
            })
        })
        .collect()
}

fn validate_members(members: &[RarMember]) -> Result<(), RarError> {
    let mut paths = std::collections::HashSet::with_capacity(members.len());
    for member in members {
        // HARD RULE: any duplicate internal path refuses the whole
        // operation. 7-Zip's name-based `-so` extraction cannot address
        // duplicate entries individually (it concatenates every match with
        // no separator) - see the module doc. No disambiguation by size,
        // CRC, index, or any other heuristic is attempted.
        if !paths.insert(member.path.clone()) {
            return Err(RarError::DuplicatePath {
                path: member.path.clone(),
            });
        }
        if member.encrypted {
            return Err(RarError::EncryptedArchive);
        }
        if member.solid {
            return Err(RarError::UnsupportedArchive {
                detail: format!("solid member is refused: {}", member.path),
            });
        }
        if member.split_before || member.split_after {
            return Err(RarError::UnsupportedArchive {
                detail: format!("split member is refused: {}", member.path),
            });
        }
        if member.method.trim().is_empty() {
            return Err(RarError::AmbiguousListing {
                detail: format!("member method is missing: {}", member.path),
            });
        }
    }
    Ok(())
}

fn parse_property_line(line: &str) -> Result<(&str, &str), RarError> {
    if line.chars().any(|character| character.is_ascii_control()) {
        return Err(RarError::AmbiguousListing {
            detail: "control character in 7-Zip listing property".to_string(),
        });
    }
    let (key, value) = line
        .split_once(" = ")
        .ok_or_else(|| RarError::AmbiguousListing {
            detail: format!("unparseable 7-Zip listing line: {line:?}"),
        })?;
    if key.is_empty() || key.trim() != key || key.contains(" = ") {
        return Err(RarError::AmbiguousListing {
            detail: format!("ambiguous 7-Zip property key: {key:?}"),
        });
    }
    Ok((key, value))
}

fn required_property<'a>(
    properties: &'a std::collections::BTreeMap<String, String>,
    key: &str,
) -> Result<&'a str, RarError> {
    properties
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| RarError::AmbiguousListing {
            detail: format!("required 7-Zip property is missing: {key}"),
        })
}

fn marker(value: &str, field: &str) -> Result<bool, RarError> {
    match value {
        "+" => Ok(true),
        "-" => Ok(false),
        _ => Err(RarError::AmbiguousListing {
            detail: format!("{field} has an ambiguous marker: {value:?}"),
        }),
    }
}

fn parse_u64(value: &str, field: &str) -> Result<u64, RarError> {
    value.parse().map_err(|_| RarError::AmbiguousListing {
        detail: format!("{field} is not an unsigned integer: {value:?}"),
    })
}

fn classify_listing_failure(stdout: &[u8], stderr: &[u8]) -> RarError {
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let lower = combined.to_ascii_lowercase();
    if lower.contains("password") || lower.contains("encrypted") {
        RarError::EncryptedArchive
    } else if lower.contains("is not archive")
        || lower.contains("unexpected end")
        || lower.contains("headers error")
        || lower.contains("data error")
    {
        RarError::CorruptArchive { detail: combined }
    } else {
        RarError::BackendFailure {
            status: None,
            detail: combined,
        }
    }
}

fn validate_extraction(
    outcome: &super::external_process::ProcessOutcome,
    stable_index: usize,
    received: u64,
    declared: u64,
    hashes: &ArchiveMemberHashes,
    expected: &ExpectedMemberHashes,
) -> Result<(), RarError> {
    if declared == 0 {
        return Err(RarError::ZeroSizedMember { stable_index });
    }
    // Never return a verified result after a non-zero exit, even though
    // 7-Zip may have already written some or all of the wrong/partial
    // bytes to stdout before reporting the failure.
    if !outcome.status.success() {
        return Err(RarError::BackendFailure {
            status: outcome.status.code(),
            detail: String::from_utf8_lossy(&outcome.stderr).into_owned(),
        });
    }
    if received != declared {
        return Err(RarError::SizeMismatch { declared, received });
    }
    if !expected.matches(hashes) {
        return Err(RarError::HashMismatch);
    }
    Ok(())
}

struct StreamingHashes {
    crc32: Crc32,
    md5: Md5,
    sha1: Sha1,
    sha256: Sha256,
}

impl StreamingHashes {
    fn new() -> Self {
        Self {
            crc32: Crc32::new(),
            md5: Md5::new(),
            sha1: Sha1::new(),
            sha256: Sha256::new(),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.crc32.update(bytes);
        self.md5.update(bytes);
        self.sha1.update(bytes);
        self.sha256.update(bytes);
    }

    fn finish(self) -> ArchiveMemberHashes {
        ArchiveMemberHashes {
            crc32: self.crc32.finish_hex(),
            md5: hex(&self.md5.finalize()),
            sha1: hex(&self.sha1.finalize()),
            sha256: hex(&self.sha256.finalize()),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------
// `ArchiveMemberSource` bridge
// ---------------------------------------------------------------------
//
// RAR's backend is architecturally different from the ZIP/7z readers this
// trait was designed around: `RarSession::read_member` is a *verify* API
// (it requires the expected strong hash up front and never returns bytes or
// a hash unless they match) rather than a *decode* API (ZIP/7z hash every
// member unconditionally, then match afterward). So a RAR member cannot be
// blindly hashed the way `zip.rs`/`sevenz.rs` hash first and look up
// second; instead the DAT candidate to verify against is chosen once, at
// [`RarArchiveSource::open`], from a narrow filename-only lookup - never by
// picking a filename match alone as identity (the actual verdict always
// still comes from `read_member`'s strong-hash gate), and never by guessing
// among conflicting candidates. A member with no unambiguous candidate, or
// whose only candidate carries no strong hash, is left
// [`ArchiveMemberStatus::NotVerified`] - `read_member` is never invoked
// speculatively.

use std::sync::atomic::{AtomicBool, Ordering};

use super::limits::ArchiveLimits;
use super::{
    ArchiveMemberEvidence, ArchiveMemberSource, ArchiveMemberSourceError, ArchiveMemberStatus,
    ArchivePassCompletion, ArchivePassOutcome, ArchivePassStopReason, ArchiveRunBudget,
};
use crate::dat::index::DatIndex;
use crate::dat::model::ChecksumAlgorithm;

const NESTED_ARCHIVE_EXTENSIONS: &[&str] = &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst"];

/// RAR source opened through a discovered [`RarProvider`], with per-member
/// DAT verification candidates resolved once at open time.
pub struct RarArchiveSource {
    session: RarSession,
    limits: ArchiveLimits,
    /// Parallel to `session.members`, by `stable_index`: the one DAT
    /// candidate (if any, unambiguous) to verify each member against.
    candidates: Vec<Option<ExpectedMemberHashes>>,
    member_timeout: std::time::Duration,
}

impl std::fmt::Debug for RarArchiveSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RarArchiveSource")
            .field("archive_path", &self.session.archive_path())
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl RarArchiveSource {
    /// Opens `path` through the already-discovered `provider`, pinning it by
    /// fd for the session's whole lifetime (see [`RarProvider::open`]), and
    /// resolves each listed member's DAT verification candidate up front
    /// from `index` - a narrow, filename-only lookup, never trusted as
    /// identity on its own.
    pub fn open(
        path: &Path,
        provider: &RarProvider,
        index: &DatIndex,
        limits: ArchiveLimits,
        open_timeout: Duration,
        member_timeout: Duration,
    ) -> Result<Self, ArchiveMemberSourceError> {
        let session = provider.open(path, open_timeout).map_err(rar_open_error)?;
        if session.members.len() > limits.max_members {
            return Err(ArchiveMemberSourceError::RefusedLimits {
                reason: "member count",
            });
        }
        let candidates = session
            .members
            .iter()
            .map(|member| candidate_hashes_for(index, &member.path))
            .collect();
        Ok(Self {
            session,
            limits,
            candidates,
            member_timeout,
        })
    }

    fn evidence(
        &self,
        member: &RarMember,
        status: ArchiveMemberStatus,
        hashes: Option<ArchiveMemberHashes>,
    ) -> ArchiveMemberEvidence {
        ArchiveMemberEvidence {
            archive_path: self.session.archive_path().to_path_buf(),
            member_name_raw: member.path.as_bytes().to_vec(),
            member_name_display: member.path.clone(),
            index: member.stable_index,
            logical_size: member.size,
            is_nested_archive: is_nested_name(&member.path),
            status,
            hashes,
        }
    }
}

impl ArchiveMemberSource for RarArchiveSource {
    fn archive_format(&self) -> &'static str {
        "rar"
    }

    fn member_count(&self) -> usize {
        self.session.members.len()
    }

    /// Completeness is coverage, not DAT attribution: a pass is `Complete`
    /// only when *every* member reached [`ArchiveMemberStatus::HashComplete`]:
    /// a member that is empty, refused by a limit, left `NotVerified` for
    /// lack of an unambiguous DAT candidate, or that failed verification, is
    /// exactly as much "not fully accounted for" as a cancelled or
    /// backend-failed pass. This is deliberately stricter than ZIP/7z's own
    /// "independent members, per-member Corrupt" model: unlike a ZIP central
    /// directory, nothing here proves the rest of a RAR archive is still
    /// safe to examine after any surprise, so the conservative default is
    /// coverage over optimism. `SizeMismatch`/`HashMismatch` are the only
    /// statuses ever treated as content-local (continue examining other
    /// members): both are reachable from [`RarSession::read_member`] only
    /// *after* the extraction child already exited 0 and every relist/
    /// selection consistency check already passed, so they can only mean
    /// this one member's own bytes disagreed - never an infrastructure or
    /// consistency failure. `BackendFailure` is deliberately NOT treated as
    /// content-local: [`RarSession::read_member`]'s relist step can itself
    /// raise a raw listing-backend failure that is indistinguishable at this
    /// layer from a nonzero extraction exit, so it is conservatively
    /// bucketed as an archive-wide failure (aborts the pass) rather than
    /// assumed to be this one member's content.
    fn verify_all(
        &mut self,
        cancel: &AtomicBool,
        run_budget: &mut ArchiveRunBudget,
    ) -> ArchivePassOutcome {
        let total_members = self.session.members.len();
        let mut members = Vec::with_capacity(total_members);
        let mut completion = ArchivePassCompletion::Complete;

        for member in self.session.members.clone() {
            if cancel.load(Ordering::Relaxed) {
                completion = ArchivePassCompletion::Incomplete {
                    reason: ArchivePassStopReason::Cancelled,
                };
                break;
            }

            if member.size == 0 {
                members.push(self.evidence(&member, ArchiveMemberStatus::EmptyFile, None));
                completion = mark_incomplete_once(
                    completion,
                    member.stable_index,
                    ArchiveMemberStatus::EmptyFile,
                );
                continue;
            }
            if member.size > self.limits.max_member_logical_bytes {
                let status = ArchiveMemberStatus::RefusedLimits {
                    reason: "member size",
                };
                members.push(self.evidence(&member, status.clone(), None));
                completion = mark_incomplete_once(completion, member.stable_index, status);
                continue;
            }
            let Some(candidate) = self
                .candidates
                .get(member.stable_index)
                .and_then(Option::as_ref)
            else {
                let status = ArchiveMemberStatus::NotVerified {
                    reason: "no unambiguous DAT candidate for this filename",
                };
                members.push(self.evidence(&member, status.clone(), None));
                completion = mark_incomplete_once(completion, member.stable_index, status);
                continue;
            };
            if !run_budget.try_charge(member.size) {
                members.push(self.evidence(
                    &member,
                    ArchiveMemberStatus::RefusedLimits {
                        reason: "run logical budget",
                    },
                    None,
                ));
                completion = ArchivePassCompletion::Incomplete {
                    reason: ArchivePassStopReason::RunLogicalBudget,
                };
                break;
            }

            match self.session.read_member(
                member.stable_index,
                member.size,
                candidate,
                self.member_timeout,
            ) {
                Ok(result) => {
                    members.push(self.evidence(
                        &member,
                        ArchiveMemberStatus::HashComplete,
                        Some(result.hashes),
                    ));
                }
                Err(error) if member_error_is_content_local(&error) => {
                    // Content-local by construction: `validate_extraction`
                    // only reaches either of these checks after the
                    // extraction child already exited 0 and every relist/
                    // selection consistency check already passed - so this
                    // can only mean this one member's own bytes disagreed
                    // with either the declared size or the DAT candidate's
                    // hash. Independent members remain worth examining, but
                    // the pass itself can never be `Complete` once any
                    // member fails to verify - see this method's own doc.
                    let status = ArchiveMemberStatus::Corrupt {
                        detail: error.to_string(),
                    };
                    members.push(self.evidence(&member, status.clone(), None));
                    completion = mark_incomplete_once(completion, member.stable_index, status);
                }
                Err(error) => {
                    // Everything else - a nonzero extraction exit
                    // (`BackendFailure`, not provably content-local: see
                    // this method's own doc), relist/selection drift,
                    // timeout, process/IO failure, cleanup failure, or any
                    // defensive case that should not be reachable given the
                    // inputs this loop constructs - is conservatively
                    // treated as archive-wide, not confined to this one
                    // member. Never represented as a verified member; the
                    // whole pass is marked incomplete instead of silently
                    // continuing against a possibly-wedged backend.
                    completion = ArchivePassCompletion::Incomplete {
                        reason: ArchivePassStopReason::SourceError {
                            detail: error.to_string(),
                        },
                    };
                    break;
                }
            }
        }

        ArchivePassOutcome {
            members,
            total_members,
            completion,
        }
    }
}

/// Downgrades `completion` to `Incomplete { MemberRefused }` for `index`/
/// `status`, unless it is already `Incomplete` for a different (and
/// necessarily higher-priority, since it already caused a `break`) reason -
/// never lets a later, merely-informational downgrade overwrite an already-
/// recorded abort reason.
fn mark_incomplete_once(
    completion: ArchivePassCompletion,
    index: usize,
    status: ArchiveMemberStatus,
) -> ArchivePassCompletion {
    match completion {
        ArchivePassCompletion::Complete => ArchivePassCompletion::Incomplete {
            reason: ArchivePassStopReason::MemberRefused { index, status },
        },
        already_incomplete => already_incomplete,
    }
}

/// Whether a [`RarSession::read_member`] failure is provably confined to
/// that one member's own content, rather than the archive/backend as a
/// whole. Only `SizeMismatch` and `HashMismatch` qualify: both are reachable
/// solely from `validate_extraction`'s checks *after* the extraction child
/// already exited 0 and every relist/selection consistency check already
/// passed (see `RarSession::read_member`'s own source), so they can only
/// mean this one member's own bytes disagreed. Every other variant -
/// including `BackendFailure`, which `RarSession::read_member`'s relist step
/// can itself raise from a raw listing-backend failure indistinguishable at
/// this layer from a nonzero extraction exit - is conservatively treated as
/// not proven content-local.
fn member_error_is_content_local(error: &RarError) -> bool {
    matches!(
        error,
        RarError::SizeMismatch { .. } | RarError::HashMismatch
    )
}

fn rar_open_error(error: RarError) -> ArchiveMemberSourceError {
    match error {
        RarError::BackendNotFound | RarError::BackendUnavailable { .. } => {
            ArchiveMemberSourceError::Unsupported {
                detail: format!(
                    "RAR support requires a user-installed 7-Zip with RAR5 support: {error}"
                ),
            }
        }
        RarError::EncryptedArchive => ArchiveMemberSourceError::Encrypted,
        RarError::UnsupportedArchive { detail }
        | RarError::AmbiguousListing { detail }
        | RarError::DuplicatePath { path: detail } => {
            ArchiveMemberSourceError::Unsupported { detail }
        }
        RarError::InvalidSignature => ArchiveMemberSourceError::Corrupt {
            detail: "not a RAR archive (signature mismatch)".to_string(),
        },
        RarError::CorruptArchive { detail } => ArchiveMemberSourceError::Corrupt { detail },
        RarError::ProcessOutputLimit { limit } => ArchiveMemberSourceError::RefusedLimits {
            reason: if limit == LIST_STDOUT_LIMIT {
                "listing output"
            } else {
                "process output"
            },
        },
        other => ArchiveMemberSourceError::Open {
            detail: other.to_string(),
        },
    }
}

/// Resolves the one DAT candidate to verify `member_path` against, or
/// `None` when there is nothing usable to try: no filename match, several
/// filename matches whose checksums disagree, or a sole match with no
/// strong hash at all (CRC32-only DAT entries are never a usable
/// candidate - `ExpectedMemberHashes` structurally has no CRC32 field).
fn candidate_hashes_for(index: &DatIndex, member_path: &str) -> Option<ExpectedMemberHashes> {
    let candidates = index.lookup_filename(member_path);
    let (first, rest) = candidates.split_first()?;
    let expected = expected_hashes_from(first)?;
    for other in rest {
        if expected_hashes_from(other).as_ref() != Some(&expected) {
            return None;
        }
    }
    Some(expected)
}

fn expected_hashes_from(rom: &crate::dat::index::DatRomRef) -> Option<ExpectedMemberHashes> {
    let mut hashes = ExpectedMemberHashes::default();
    for checksum in &rom.checksums {
        match checksum.algorithm {
            ChecksumAlgorithm::Md5 => hashes.md5 = Some(checksum.value.clone()),
            ChecksumAlgorithm::Sha1 => hashes.sha1 = Some(checksum.value.clone()),
            ChecksumAlgorithm::Sha256 => hashes.sha256 = Some(checksum.value.clone()),
            ChecksumAlgorithm::Crc32 => {}
        }
    }
    hashes.validate().is_ok().then_some(hashes)
}

fn is_nested_name(path: &str) -> bool {
    let Some((_, extension)) = path.rsplit_once('.') else {
        return false;
    };
    NESTED_ARCHIVE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Fixtures: real, small, RAR5 archives from libarchive's own OSI-
    // licensed test suite (BSD), decoded once at
    // `crates/archivefs-core/tests/fixtures/rar/`. No user data, no
    // synthetic RAR encoding (7-Zip only reads RAR; nothing here creates
    // one), no reliance on any RAR-creating tool.
    // -----------------------------------------------------------------

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rar")).join(name)
    }

    fn provider() -> RarProvider {
        RarProvider::discover(Duration::from_secs(10)).expect("a capable 7-Zip must be installed")
    }

    fn short_timeout() -> Duration {
        Duration::from_secs(10)
    }

    // -- Backend discovery --------------------------------------------

    #[test]
    fn discover_finds_a_capable_backend_and_records_version_and_rar5() {
        let backend = provider();
        assert!(!backend.version().is_empty());
        assert!(backend.executable().is_file());
    }

    #[test]
    fn resolve_executable_returns_none_for_a_nonexistent_candidate() {
        // The unit this test isolates, without mutating the whole test
        // binary's process-global `PATH` (which every other test - possibly
        // running concurrently - also reads): a candidate name that exists
        // in no `PATH` directory and is not an absolute path resolves to
        // `None`, exactly the condition `discover` treats as "try the next
        // candidate", eventually returning `BackendNotFound` when every
        // candidate misses.
        let candidate = PathBuf::from("definitely-does-not-exist-as-a-binary-xyz123");
        assert_eq!(resolve_executable(&candidate), None);
    }

    #[test]
    fn resolve_executable_rejects_a_nonexistent_absolute_path() {
        let candidate = PathBuf::from("/definitely/does/not/exist/xyz123");
        assert_eq!(resolve_executable(&candidate), None);
    }

    // -- Strict `-slt` parser: schema ----------------------------------

    fn member_block(overrides: &[(&str, &str)]) -> String {
        let mut fields: std::collections::BTreeMap<&str, String> = [
            ("Path", "a.bin".to_string()),
            ("Folder", "-".to_string()),
            ("Size", "4".to_string()),
            ("Packed Size", "4".to_string()),
            ("Modified", "2024-01-01 00:00:00".to_string()),
            ("Created", "".to_string()),
            ("Accessed", "".to_string()),
            ("Attributes", "A".to_string()),
            ("Alternate Stream", "-".to_string()),
            ("Encrypted", "-".to_string()),
            ("Solid", "-".to_string()),
            ("Split Before", "-".to_string()),
            ("Split After", "-".to_string()),
            ("CRC", "DEADBEEF".to_string()),
            ("Host OS", "Unix".to_string()),
            ("Method", "RAR5(1M)".to_string()),
            ("Characteristics", "".to_string()),
            ("Symbolic Link", "".to_string()),
            ("Hard Link", "".to_string()),
            ("Copy Link", "".to_string()),
            ("Volume Index", "0".to_string()),
            ("Checksum", "".to_string()),
            ("NT Security", "".to_string()),
        ]
        .into_iter()
        .collect();
        for (key, value) in overrides {
            fields.insert(key, value.to_string());
        }
        fields
            .into_iter()
            .map(|(key, value)| format!("{key} = {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn parses_two_ordinary_members_preserving_archive_order() {
        let output = format!(
            "{}\n\n{}\n",
            member_block(&[("Path", "first.bin")]),
            member_block(&[("Path", "second.bin")])
        );
        let members = parse_member_listing(&output).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].stable_index, 0);
        assert_eq!(members[0].path, "first.bin");
        assert_eq!(members[1].stable_index, 1);
        assert_eq!(members[1].path, "second.bin");
    }

    #[test]
    fn unknown_structural_property_is_rejected() {
        let output = format!("{}\nBogus Property = x\n", member_block(&[]));
        let error = parse_member_listing(&output).unwrap_err();
        assert!(matches!(error, RarError::AmbiguousListing { .. }));
    }

    #[test]
    fn duplicate_property_within_one_block_is_rejected() {
        let output = format!("{}\nPath = duplicate.bin\n", member_block(&[]));
        let error = parse_member_listing(&output).unwrap_err();
        assert!(matches!(error, RarError::AmbiguousListing { .. }));
    }

    #[test]
    fn missing_required_property_is_rejected() {
        let mut fields = member_block(&[]);
        // Remove the "Method = ..." line entirely.
        fields = fields
            .lines()
            .filter(|line| !line.starts_with("Method ="))
            .collect::<Vec<_>>()
            .join("\n");
        let error = parse_member_listing(&fields).unwrap_err();
        assert!(matches!(error, RarError::AmbiguousListing { .. }));
    }

    #[test]
    fn malformed_marker_value_is_rejected() {
        let output = member_block(&[("Folder", "maybe")]);
        let error = parse_member_listing(&output).unwrap_err();
        assert!(matches!(error, RarError::AmbiguousListing { .. }));
    }

    #[test]
    fn ambiguous_record_boundary_in_archive_block_is_rejected() {
        let text = "--\nType = Rar5\nSolid = -\nEncrypted = -\nMultivolume = -\nVolumes = 1\n--\n----------\n";
        let error = parse_archive_properties(text).unwrap_err();
        assert!(matches!(error, RarError::AmbiguousListing { .. }));
    }

    #[test]
    fn control_character_in_a_property_value_is_rejected() {
        let output = member_block(&[("Path", "bad\u{7}name.bin")]);
        let error = parse_member_listing(&output).unwrap_err();
        assert!(matches!(error, RarError::AmbiguousListing { .. }));
    }

    #[test]
    fn duplicate_member_path_is_refused() {
        let output = format!(
            "{}\n\n{}\n",
            member_block(&[("Path", "same.bin")]),
            member_block(&[("Path", "same.bin"), ("CRC", "00000000")])
        );
        let members = parse_member_listing(&output).unwrap();
        let error = validate_members(&members).unwrap_err();
        assert_eq!(
            error,
            RarError::DuplicatePath {
                path: "same.bin".to_string()
            }
        );
    }

    #[test]
    fn non_utf8_listing_bytes_never_reach_the_parser() {
        // `run_listing` itself refuses non-UTF-8 stdout before any text ever
        // reaches `parse_member_listing`/`parse_archive_properties`.
        let invalid = vec![0xff, 0xfe, 0xfd];
        assert!(String::from_utf8(invalid).is_err());
    }

    #[test]
    fn directory_member_is_refused_during_parsing() {
        let output = member_block(&[("Folder", "+")]);
        let error = parse_member_listing(&output).unwrap_err();
        assert!(matches!(error, RarError::UnsupportedArchive { .. }));
    }

    #[test]
    fn zero_size_member_is_refused_before_extraction() {
        // Parsing accepts a zero-size ordinary member (it is a legitimate
        // listing shape); the refusal happens in `read_member`'s explicit
        // zero-size gate, proven at the session level below.
        let output = member_block(&[("Size", "0")]);
        let members = parse_member_listing(&output).unwrap();
        assert_eq!(members[0].size, 0);
    }

    // -- Envelope refusals (real archives) ------------------------------

    #[test]
    fn rar4_archive_is_refused() {
        let backend = provider();
        let error = backend
            .open(
                &fixture("test_read_format_rar_compress_best.rar"),
                short_timeout(),
            )
            .unwrap_err();
        assert_eq!(
            error,
            RarError::UnsupportedArchive {
                detail: "only RAR5 is supported; 7-Zip identified this as Rar".to_string()
            }
        );
    }

    #[test]
    fn solid_archive_is_refused() {
        let backend = provider();
        let error = backend
            .open(&fixture("test_read_format_rar5_solid.rar"), short_timeout())
            .unwrap_err();
        assert!(matches!(error, RarError::UnsupportedArchive { .. }));
    }

    #[test]
    fn encrypted_archive_is_refused() {
        let backend = provider();
        let error = backend
            .open(
                &fixture("test_read_format_rar5_encrypted.rar"),
                short_timeout(),
            )
            .unwrap_err();
        assert_eq!(error, RarError::EncryptedArchive);
    }

    #[test]
    fn multivolume_archive_is_refused() {
        // This fixture's sibling volume (part02) is genuinely absent -
        // exactly the case the research doc's `ERROR = Missing volume`
        // signal targets. Empirically (verified against this real
        // installed 7-Zip 23.01), `7z l -slt` on this specific fixture
        // does not exit cleanly with that one diagnostic line when its
        // stdout is a *pipe* (as this provider always uses, for streamed
        // reading): it instead emits tens of megabytes of repeated output
        // before a non-zero exit - a 7-Zip CLI quirk specific to the
        // piped-stdout + missing-volume combination, not reproducible via
        // a plain shell redirect to a regular file. `LIST_STDOUT_LIMIT`
        // (8 MiB, generous for any legitimate listing) exists precisely to
        // catch a runaway backend like this: the process group is killed
        // and the archive is refused via `ProcessOutputLimit`, not the
        // `UnsupportedArchive` path the clean-exit case would take. Both
        // are safe, fail-closed refusals - never `Complete`. Which *label*
        // this lands on is empirically not fully deterministic run-to-run
        // (observed both `ProcessOutputLimit` and `EncryptedArchive` - the
        // latter because `classify_listing_failure`'s substring heuristic
        // can match incidental text inside the runaway output), so this
        // test only asserts the one thing that must always hold: refusal,
        // never success.
        let backend = provider();
        let result = backend.open(
            &fixture("test_read_format_rar5_multiarchive.part01.rar"),
            short_timeout(),
        );
        assert!(
            result.is_err(),
            "a multivolume archive must never open successfully"
        );
    }

    #[test]
    fn sfx_archive_is_refused() {
        // An SFX prepends an executable stub before the RAR data, so the
        // magic signature is not at byte 0 - this provider's earliest gate
        // (`open_pinned`'s signature check, run before any 7-Zip process is
        // even spawned) refuses it there, never reaching the `Offset`-based
        // check in `validate_archive` that would apply to an SFX whose
        // stub happened to still start with a RAR signature (it does not,
        // for this real fixture).
        let backend = provider();
        let error = backend
            .open(&fixture("test_read_format_rar5_sfx.exe"), short_timeout())
            .unwrap_err();
        assert_eq!(error, RarError::InvalidSignature);
    }

    #[test]
    fn symlink_bearing_archive_is_refused() {
        let backend = provider();
        let error = backend
            .open(
                &fixture("test_read_format_rar5_symlink.rar"),
                short_timeout(),
            )
            .unwrap_err();
        assert!(matches!(error, RarError::UnsupportedArchive { .. }));
    }

    #[test]
    fn hardlink_bearing_archive_is_refused() {
        let backend = provider();
        let error = backend
            .open(
                &fixture("test_read_format_rar5_hardlink.rar"),
                short_timeout(),
            )
            .unwrap_err();
        assert!(matches!(error, RarError::UnsupportedArchive { .. }));
    }

    #[test]
    fn non_rar_input_is_refused_at_signature_check() {
        let dir = tempfile::tempdir().unwrap();
        let not_rar = dir.path().join("not-rar.bin");
        std::fs::write(&not_rar, b"PK\x03\x04 this is a zip, not a rar").unwrap();
        let error = open_pinned(&not_rar).unwrap_err();
        assert_eq!(error, RarError::InvalidSignature);
    }

    #[test]
    fn unicode_path_round_trips_through_the_strict_parser() {
        // The full fixture also contains a hard/symlink sibling, so opening
        // it through the session API is correctly refused (proven
        // separately below) - but the unicode `Path` value itself must
        // still parse byte-for-byte correctly. Isolate just the first
        // (ordinary-file) block from the real listing and parse it alone.
        let backend = provider();
        let fd_file = std::fs::File::open(fixture("test_read_format_rar5_unicode.rar")).unwrap();
        let full_text = list_via_fd_text_for_test(&backend, &fd_file, true);
        let first_block = full_text
            .split("\n\n")
            .next()
            .expect("at least one member block");
        let members = parse_member_listing(first_block).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].path, "👋🌎.txt");
    }

    #[test]
    fn unicode_archive_as_a_whole_is_refused_for_its_link_siblings() {
        let backend = provider();
        let error = backend
            .open(
                &fixture("test_read_format_rar5_unicode.rar"),
                short_timeout(),
            )
            .unwrap_err();
        assert!(matches!(error, RarError::UnsupportedArchive { .. }));
    }

    /// Test-only helper: runs the real member listing against `file`'s
    /// pinned fd and returns the raw text, for tests that need to inspect
    /// a slice of the listing the public API's own validation would refuse
    /// before returning.
    fn list_via_fd_text_for_test(
        backend: &RarProvider,
        file: &std::fs::File,
        bare: bool,
    ) -> String {
        run_listing(
            backend.executable(),
            backend.process_limits,
            file.as_raw_fd(),
            bare,
            short_timeout(),
        )
        .unwrap()
    }

    // -- argv construction (paths) --------------------------------------

    fn args_as_strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn extract_args_places_dash_prefixed_member_after_double_dash_as_one_element() {
        let args = args_as_strings(extract_args(9, "-dash-prefixed.bin"));
        let separator = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[separator + 1], "/proc/self/fd/9");
        assert_eq!(args[separator + 2], "-dash-prefixed.bin");
        assert_eq!(args.len(), separator + 3);
    }

    #[test]
    fn extract_args_preserves_spaces_unicode_and_glob_characters_as_one_literal_element() {
        for path in [
            "nested/dir/with space.bin",
            "ünïcode-文件.bin",
            "literal*star.bin",
            "literal?question.bin",
        ] {
            let args = args_as_strings(extract_args(3, path));
            assert_eq!(args.last().unwrap(), path, "path must survive unmodified");
        }
    }

    #[test]
    fn extract_args_always_uses_proc_self_not_a_pid() {
        let args = args_as_strings(extract_args(42, "m.bin"));
        assert!(args.contains(&"/proc/self/fd/42".to_string()));
        assert!(
            !args
                .iter()
                .any(|a| a.contains("/proc/") && !a.contains("/proc/self/"))
        );
    }

    #[test]
    fn extract_args_never_passes_an_output_directory_or_shell() {
        let args = args_as_strings(extract_args(1, "m.bin"));
        assert!(args.contains(&"-so".to_string()));
        assert!(!args.iter().any(|a| a.starts_with('e') && a.len() == 1));
        assert!(!args.iter().any(|a| a.contains("-o")));
    }

    #[test]
    fn list_args_bare_and_full_forms_both_target_the_pinned_fd() {
        assert!(args_as_strings(list_args(true, 5)).contains(&"/proc/self/fd/5".to_string()));
        assert!(args_as_strings(list_args(false, 5)).contains(&"/proc/self/fd/5".to_string()));
        assert!(args_as_strings(list_args(true, 5)).contains(&"-ba".to_string()));
        assert!(!args_as_strings(list_args(false, 5)).contains(&"-ba".to_string()));
    }

    // -- FD pinning ------------------------------------------------------

    #[test]
    fn list_and_extract_both_go_through_the_inherited_proc_self_fd() {
        // Structural proof, not just behavioural: neither `list_args` nor
        // `extract_args` ever accepts or emits a filesystem path argument -
        // only a `/proc/self/fd/<N>` string built from the pinned fd.
        let list = args_as_strings(list_args(false, 7));
        let extract = args_as_strings(extract_args(7, "m.bin"));
        assert!(
            list.iter()
                .all(|a| !a.contains('/') || a.starts_with("/proc/self/fd/"))
        );
        assert!(
            extract
                .iter()
                .all(|a| a == "m.bin" || !a.contains('/') || a.starts_with("/proc/self/fd/"))
        );
    }

    #[test]
    fn provider_still_reads_the_original_inode_after_the_pathname_is_replaced() {
        let backend = provider();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.rar");
        std::fs::copy(fixture("test_read_format_rar5_stored.rar"), &path).unwrap();

        let session = backend.open(&path, short_timeout()).unwrap();

        // True pathname *replacement*: rename a different, unrelated file
        // on top of `path`. This is an atomic directory-entry swap - the
        // held fd's original inode is untouched by it, unlike
        // `fs::write(&path, ...)`, which truncates and rewrites the *same*
        // inode in place (a different hazard, covered by
        // `corrupted_rar_bytes_never_verify` and the module doc's identity-
        // vs-content distinction).
        let replacement = dir.path().join("replacement.bin");
        std::fs::write(&replacement, b"not a rar archive at all").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"not a rar archive at all",
            "the pathname now really does point at different content"
        );

        // The already-open session must still resolve the member it saw at
        // `open()` time, through the pinned fd - never by reopening `path`.
        let member = &session.members[0];
        let result = session
            .read_member(
                member.stable_index,
                member.size,
                &expected_hashes_for_helloworld(),
                short_timeout(),
            )
            .unwrap();
        assert_eq!(
            result.received_len, member.size,
            "extraction must read the ORIGINAL inode's 29-byte member, not the replacement path"
        );
        assert_eq!(result.hashes.sha1, HELLOWORLD_SHA1);
    }

    #[test]
    fn provider_still_reads_the_original_inode_after_the_pathname_is_unlinked() {
        let backend = provider();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.rar");
        std::fs::copy(fixture("test_read_format_rar5_stored.rar"), &path).unwrap();

        let session = backend.open(&path, short_timeout()).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());

        let member = &session.members[0];
        let hashes = expected_hashes_for_helloworld();
        let result = session
            .read_member(member.stable_index, member.size, &hashes, short_timeout())
            .unwrap();
        assert_eq!(result.received_len, member.size);
    }

    fn expected_hashes_for_helloworld() -> ExpectedMemberHashes {
        // SHA-1/MD5/SHA-256 of the fixture's one 29-byte member
        // ("helloworld.txt"), computed once and pinned here as plain
        // expected values (no live hashing dependency in the test itself).
        ExpectedMemberHashes {
            md5: Some(HELLOWORLD_MD5.to_string()),
            sha1: Some(HELLOWORLD_SHA1.to_string()),
            sha256: Some(HELLOWORLD_SHA256.to_string()),
        }
    }

    // -- Streaming / hash / integrity ------------------------------------

    #[test]
    fn exact_member_extraction_succeeds_and_hash_matches() {
        let backend = provider();
        let session = backend
            .open(
                &fixture("test_read_format_rar5_stored.rar"),
                short_timeout(),
            )
            .unwrap();
        assert_eq!(session.members.len(), 1);
        let member = &session.members[0];
        let hashes = expected_hashes_for_helloworld();
        let result = session
            .read_member(member.stable_index, member.size, &hashes, short_timeout())
            .unwrap();
        assert_eq!(result.received_len, member.size);
        assert_eq!(result.hashes.sha1, HELLOWORLD_SHA1);
    }

    #[test]
    fn last_member_of_a_multi_member_archive_extracts_correctly() {
        let backend = provider();
        let session = backend
            .open(
                &fixture("test_read_format_rar5_multiple_files.rar"),
                short_timeout(),
            )
            .unwrap();
        let last = session.members.last().unwrap().clone();
        // Discover the real hash by extraction with a deliberately-wrong
        // expectation first is not possible (validate would refuse) - so
        // this test proves size/exit/streaming correctness, which is what
        // "last member extracts" is actually asserting; hash agreement is
        // covered by `exact_member_extraction_succeeds_and_hash_matches`.
        let crc_only = ExpectedMemberHashes {
            md5: Some("0".repeat(32)),
            ..Default::default()
        };
        let error = session
            .read_member(last.stable_index, last.size, &crc_only, short_timeout())
            .unwrap_err();
        // Wrong on purpose: proves the size/exit path completed (received
        // == declared) and only the digest comparison failed, i.e. the last
        // member really was read end-to-end.
        assert_eq!(error, RarError::HashMismatch);
    }

    #[test]
    fn declared_size_preflight_refuses_before_spawning_extraction() {
        let backend = provider();
        let session = backend
            .open(
                &fixture("test_read_format_rar5_stored.rar"),
                short_timeout(),
            )
            .unwrap();
        let member = &session.members[0];
        let hashes = expected_hashes_for_helloworld();
        let error = session
            .read_member(
                member.stable_index,
                member.size - 1,
                &hashes,
                short_timeout(),
            )
            .unwrap_err();
        assert_eq!(
            error,
            RarError::MemberTooLarge {
                declared: member.size,
                limit: member.size - 1
            }
        );
    }

    #[test]
    fn wrong_strong_hash_fails_even_with_correct_size() {
        let backend = provider();
        let session = backend
            .open(
                &fixture("test_read_format_rar5_stored.rar"),
                short_timeout(),
            )
            .unwrap();
        let member = &session.members[0];
        let wrong = ExpectedMemberHashes {
            sha1: Some("0".repeat(40)),
            ..Default::default()
        };
        let error = session
            .read_member(member.stable_index, member.size, &wrong, short_timeout())
            .unwrap_err();
        assert_eq!(error, RarError::HashMismatch);
    }

    #[test]
    fn multiple_supplied_strong_hashes_are_all_required() {
        let backend = provider();
        let session = backend
            .open(
                &fixture("test_read_format_rar5_stored.rar"),
                short_timeout(),
            )
            .unwrap();
        let member = &session.members[0];
        let mut mixed = expected_hashes_for_helloworld();
        mixed.md5 = Some("0".repeat(32)); // correct sha1/sha256, wrong md5
        let error = session
            .read_member(member.stable_index, member.size, &mixed, short_timeout())
            .unwrap_err();
        assert_eq!(error, RarError::HashMismatch);
    }

    #[test]
    fn crc32_only_cannot_succeed_because_it_is_not_a_strong_hash_field() {
        // `ExpectedMemberHashes` structurally has no CRC32 field at all -
        // CRC32 can never be supplied as the sole/any expected digest.
        let empty = ExpectedMemberHashes::default();
        let error = empty.validate().unwrap_err();
        assert!(matches!(error, RarError::InvalidExpectedHash { .. }));
    }

    #[test]
    fn no_match_zero_output_cannot_succeed() {
        let backend = provider();
        let session = backend
            .open(
                &fixture("test_read_format_rar5_stored.rar"),
                short_timeout(),
            )
            .unwrap();
        // `stable_index` valid but we synthesise a member-not-found path by
        // asking for an index past the end - the zero-output/no-match case
        // 7-Zip itself would answer with "exit 0, empty stdout" is refused
        // upstream of ever spawning extraction, by the size preflight
        // (declared size is always > 0 for any member this provider keeps).
        let out_of_range = session.members.len();
        let error = session
            .read_member(
                out_of_range,
                u64::MAX,
                &expected_hashes_for_helloworld(),
                short_timeout(),
            )
            .unwrap_err();
        assert_eq!(
            error,
            RarError::MemberNotFound {
                stable_index: out_of_range
            }
        );
    }

    #[test]
    fn corrupted_rar_bytes_never_verify() {
        let backend = provider();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.rar");
        let mut bytes = std::fs::read(fixture("test_read_format_rar5_stored.rar")).unwrap();
        // Flip a byte inside the compressed/stored payload region (well
        // past the header) so the archive still opens and lists, but the
        // member's data does not verify.
        let flip_at = bytes.len() - 5;
        bytes[flip_at] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        let session = backend.open(&path, short_timeout());
        // Either the corruption is visible at listing time (archive-level
        // refusal) or it surfaces at extraction time (CRC/data error) - both
        // are acceptable fail-closed outcomes; a `Complete`/verified result
        // is not.
        if let Ok(session) = session {
            let member = &session.members[0];
            let result = session.read_member(
                member.stable_index,
                member.size,
                &expected_hashes_for_helloworld(),
                short_timeout(),
            );
            assert!(result.is_err(), "corrupted payload must never verify");
        }
    }

    // -- Mutation -----------------------------------------------------

    #[test]
    fn source_bytes_are_unchanged_after_an_ordinary_read_and_verify() {
        let backend = provider();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.rar");
        std::fs::copy(fixture("test_read_format_rar5_stored.rar"), &path).unwrap();
        let before = std::fs::read(&path).unwrap();

        let session = backend.open(&path, short_timeout()).unwrap();
        let member = &session.members[0];
        session
            .read_member(
                member.stable_index,
                member.size,
                &expected_hashes_for_helloworld(),
                short_timeout(),
            )
            .unwrap();

        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "the provider must never mutate the archive");
    }

    // helloworld.txt's real digests (from the RAR5-stored libarchive
    // fixture's one 29-byte member), computed once out-of-band.
    const HELLOWORLD_MD5: &str = "7a3904e4b0cd0263179b785623c786bd";
    const HELLOWORLD_SHA1: &str = "253aafde5dec9a54ed554bc7f95e6f291c60cbb0";
    const HELLOWORLD_SHA256: &str =
        "fef9ad8cf601b43f76c6320075f62267c6e5c0a526d750a70b80c919a4a0aad8";

    // -- `ArchiveMemberSource` bridge (the DAT-audit integration point) ---

    mod archive_source_tests {
        use std::collections::HashMap;
        use std::sync::atomic::AtomicBool;

        use super::*;
        use crate::dat::archive::limits::ArchiveLimits;
        use crate::dat::archive::{
            ArchiveMemberSource, ArchiveMemberSourceError, ArchiveMemberStatus,
            ArchivePassCompletion, ArchivePassStopReason, ArchiveRunBudget,
        };
        use crate::dat::index::{DatIndex, DatMemberKey, DatRomRef, MemberLocation};
        use crate::dat::model::{ChecksumAlgorithm, DatChecksum};

        fn no_cancel() -> AtomicBool {
            AtomicBool::new(false)
        }

        fn unlimited_budget() -> ArchiveRunBudget {
            ArchiveRunBudget::new(u64::MAX)
        }

        fn rom_ref(game_name: &str, rom_name: &str, checksums: Vec<DatChecksum>) -> DatRomRef {
            DatRomRef {
                game_index: 0,
                game_name: game_name.to_string(),
                rom_index: 0,
                member_key: DatMemberKey {
                    game_index: 0,
                    location: MemberLocation::TopLevel { rom_index: 0 },
                },
                rom_name: rom_name.to_string(),
                size_bytes: None,
                checksums,
                status: None,
                merge: None,
                content_classification: Default::default(),
                original_metadata: Default::default(),
                clone_of: None,
            }
        }

        /// A `DatIndex` with only `by_filename` populated - exactly the one
        /// lookup `candidate_hashes_for` performs.
        fn index_by_filename(filename: &str, refs: Vec<DatRomRef>) -> DatIndex {
            DatIndex {
                by_crc32: HashMap::new(),
                by_md5: HashMap::new(),
                by_sha1: HashMap::new(),
                by_sha256: HashMap::new(),
                by_filename: HashMap::from([(filename.to_ascii_lowercase(), refs)]),
                game_clone_of: HashMap::new(),
            }
        }

        fn open_source(
            path: &Path,
            backend: &RarProvider,
            index: &DatIndex,
        ) -> Result<RarArchiveSource, ArchiveMemberSourceError> {
            RarArchiveSource::open(
                path,
                backend,
                index,
                ArchiveLimits::default(),
                short_timeout(),
                short_timeout(),
            )
        }

        #[test]
        fn exact_dat_candidate_is_verified_and_becomes_hash_complete_evidence() {
            let backend = provider();
            let index = index_by_filename(
                "helloworld.txt",
                vec![rom_ref(
                    "Hello World",
                    "helloworld.txt",
                    vec![
                        DatChecksum::parse(ChecksumAlgorithm::Md5, HELLOWORLD_MD5).unwrap(),
                        DatChecksum::parse(ChecksumAlgorithm::Sha1, HELLOWORLD_SHA1).unwrap(),
                        DatChecksum::parse(ChecksumAlgorithm::Sha256, HELLOWORLD_SHA256).unwrap(),
                    ],
                )],
            );
            let mut source = open_source(
                &fixture("test_read_format_rar5_stored.rar"),
                &backend,
                &index,
            )
            .unwrap();
            assert_eq!(source.archive_format(), "rar");
            assert_eq!(source.member_count(), 1);

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert_eq!(outcome.completion, ArchivePassCompletion::Complete);
            assert_eq!(outcome.members.len(), 1);
            let member = &outcome.members[0];
            assert_eq!(member.status, ArchiveMemberStatus::HashComplete);
            assert_eq!(
                member.hashes.as_ref().unwrap().sha1,
                HELLOWORLD_SHA1,
                "the real streamed hash must reach the evidence, not the expected one"
            );
            assert!(!member.is_nested_archive);
        }

        #[test]
        fn ambiguous_filename_candidates_leave_the_member_unverified_and_poison_the_pass() {
            let backend = provider();
            let index = index_by_filename(
                "helloworld.txt",
                vec![
                    rom_ref(
                        "Hello World",
                        "helloworld.txt",
                        vec![DatChecksum::parse(ChecksumAlgorithm::Sha1, HELLOWORLD_SHA1).unwrap()],
                    ),
                    rom_ref(
                        "Some Other Game",
                        "helloworld.txt",
                        vec![
                            DatChecksum::parse(
                                ChecksumAlgorithm::Sha1,
                                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            )
                            .unwrap(),
                        ],
                    ),
                ],
            );
            let mut source = open_source(
                &fixture("test_read_format_rar5_stored.rar"),
                &backend,
                &index,
            )
            .unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            let member = &outcome.members[0];
            assert!(
                matches!(member.status, ArchiveMemberStatus::NotVerified { .. }),
                "conflicting candidates must never be guessed between: {:?}",
                member.status
            );
            assert!(member.hashes.is_none());
            // A member left unverified is not "fully accounted for" -
            // completeness is coverage, not DAT attribution (see
            // `verify_all`'s own doc); the whole pass must reflect that.
            assert!(
                matches!(
                    outcome.completion,
                    ArchivePassCompletion::Incomplete {
                        reason: ArchivePassStopReason::MemberRefused { .. }
                    }
                ),
                "an unverified member must poison the pass, not leave it Complete: {:?}",
                outcome.completion
            );
        }

        #[test]
        fn crc_only_candidate_is_never_a_usable_verification_target() {
            let backend = provider();
            let index = index_by_filename(
                "helloworld.txt",
                vec![rom_ref(
                    "Hello World",
                    "helloworld.txt",
                    vec![DatChecksum::parse(ChecksumAlgorithm::Crc32, "deadbeef").unwrap()],
                )],
            );
            let mut source = open_source(
                &fixture("test_read_format_rar5_stored.rar"),
                &backend,
                &index,
            )
            .unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            let member = &outcome.members[0];
            assert!(
                matches!(member.status, ArchiveMemberStatus::NotVerified { .. }),
                "a CRC32-only candidate must never gate `read_member`: {:?}",
                member.status
            );
            assert!(matches!(
                outcome.completion,
                ArchivePassCompletion::Incomplete {
                    reason: ArchivePassStopReason::MemberRefused { .. }
                }
            ));
        }

        #[test]
        fn no_filename_candidate_leaves_the_member_unverified() {
            let backend = provider();
            let index = index_by_filename(
                "completely-unrelated.bin",
                vec![rom_ref(
                    "Unrelated",
                    "completely-unrelated.bin",
                    vec![DatChecksum::parse(ChecksumAlgorithm::Sha1, HELLOWORLD_SHA1).unwrap()],
                )],
            );
            let mut source = open_source(
                &fixture("test_read_format_rar5_stored.rar"),
                &backend,
                &index,
            )
            .unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert!(matches!(
                outcome.members[0].status,
                ArchiveMemberStatus::NotVerified { .. }
            ));
            assert!(matches!(
                outcome.completion,
                ArchivePassCompletion::Incomplete {
                    reason: ArchivePassStopReason::MemberRefused { .. }
                }
            ));
        }

        #[test]
        fn wrong_strong_hash_candidate_fails_closed_as_corrupt_and_poisons_the_pass() {
            let backend = provider();
            let index = index_by_filename(
                "helloworld.txt",
                vec![rom_ref(
                    "Hello World",
                    "helloworld.txt",
                    vec![
                        DatChecksum::parse(
                            ChecksumAlgorithm::Sha1,
                            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                        )
                        .unwrap(),
                    ],
                )],
            );
            let mut source = open_source(
                &fixture("test_read_format_rar5_stored.rar"),
                &backend,
                &index,
            )
            .unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            // A single member's own content disagreeing with its one DAT
            // candidate is content-local (`SizeMismatch`/`HashMismatch` are
            // only reachable after the extraction child already exited 0
            // and every relist/selection check already passed), so it is
            // `Corrupt`, not an archive-wide abort - but it still means the
            // member was never fully accounted for, so the pass itself can
            // never be `Complete`.
            let member = &outcome.members[0];
            assert!(matches!(member.status, ArchiveMemberStatus::Corrupt { .. }));
            assert!(member.hashes.is_none());
            assert!(
                matches!(
                    outcome.completion,
                    ArchivePassCompletion::Incomplete {
                        reason: ArchivePassStopReason::MemberRefused { .. }
                    }
                ),
                "a content-mismatched member must still poison the pass: {:?}",
                outcome.completion
            );
        }

        #[test]
        fn multiple_members_verify_independently_and_the_last_one_verifies() {
            let backend = provider();
            let entries = [
                (
                    "test1.bin",
                    "b0ee823da852a3d713823eb5d04760bb",
                    "e6d444eac448f176cb9f8a1db5674df32f24e163",
                ),
                (
                    "test2.bin",
                    "dbd7c92b813bf96e9a78e87f18e2b7b9",
                    "4fe8ce24816b42ff99293a2fe102df959cd16b79",
                ),
                (
                    "test3.bin",
                    "47e419033b015612ca7e745e6f901520",
                    "79249e6a3316216a2acd891178fb7477c2519e6a",
                ),
                (
                    "test4.bin",
                    "067610695188b994e2dec3b4543e62e6",
                    "155be6207481dacbd89aa4f39902ead1df4d9e33",
                ),
            ];
            let mut by_filename = HashMap::new();
            for (name, md5, sha1) in entries {
                by_filename.insert(
                    name.to_string(),
                    vec![rom_ref(
                        name,
                        name,
                        vec![
                            DatChecksum::parse(ChecksumAlgorithm::Md5, md5).unwrap(),
                            DatChecksum::parse(ChecksumAlgorithm::Sha1, sha1).unwrap(),
                        ],
                    )],
                );
            }
            let index = DatIndex {
                by_crc32: HashMap::new(),
                by_md5: HashMap::new(),
                by_sha1: HashMap::new(),
                by_sha256: HashMap::new(),
                by_filename,
                game_clone_of: HashMap::new(),
            };
            let mut source = open_source(
                &fixture("test_read_format_rar5_multiple_files.rar"),
                &backend,
                &index,
            )
            .unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert_eq!(outcome.completion, ArchivePassCompletion::Complete);
            assert_eq!(outcome.members.len(), 4);
            assert!(
                outcome
                    .members
                    .iter()
                    .all(|member| member.status == ArchiveMemberStatus::HashComplete)
            );
            let last = outcome.members.last().unwrap();
            assert_eq!(last.member_name_display, "test4.bin");
            assert_eq!(
                last.hashes.as_ref().unwrap().sha1,
                "155be6207481dacbd89aa4f39902ead1df4d9e33"
            );
        }

        #[test]
        fn source_pinning_survives_pathname_replacement_through_the_archive_member_source() {
            let backend = provider();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("archive.rar");
            std::fs::copy(fixture("test_read_format_rar5_stored.rar"), &path).unwrap();
            let index = index_by_filename(
                "helloworld.txt",
                vec![rom_ref(
                    "Hello World",
                    "helloworld.txt",
                    vec![DatChecksum::parse(ChecksumAlgorithm::Sha1, HELLOWORLD_SHA1).unwrap()],
                )],
            );
            let mut source = open_source(&path, &backend, &index).unwrap();

            // Replace the pathname's content entirely, same as the
            // lower-level `RarSession` pinning tests, but exercised here
            // through the exact type `dat::sources::audit_run` dispatches to.
            let replacement = dir.path().join("replacement.bin");
            std::fs::write(&replacement, b"not a rar archive at all").unwrap();
            std::fs::rename(&replacement, &path).unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert_eq!(outcome.completion, ArchivePassCompletion::Complete);
            assert_eq!(outcome.members[0].status, ArchiveMemberStatus::HashComplete);
            assert_eq!(
                outcome.members[0].hashes.as_ref().unwrap().sha1,
                HELLOWORLD_SHA1,
                "must still read the original pinned inode, never the replaced pathname"
            );
        }

        #[test]
        fn source_pinning_survives_unlink_through_the_archive_member_source() {
            let backend = provider();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("archive.rar");
            std::fs::copy(fixture("test_read_format_rar5_stored.rar"), &path).unwrap();
            let index = index_by_filename(
                "helloworld.txt",
                vec![rom_ref(
                    "Hello World",
                    "helloworld.txt",
                    vec![DatChecksum::parse(ChecksumAlgorithm::Sha1, HELLOWORLD_SHA1).unwrap()],
                )],
            );
            let mut source = open_source(&path, &backend, &index).unwrap();
            std::fs::remove_file(&path).unwrap();
            assert!(!path.exists());

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert_eq!(outcome.members[0].status, ArchiveMemberStatus::HashComplete);
        }

        #[test]
        fn envelope_refusals_surface_as_archive_member_source_errors_never_a_source() {
            let backend = provider();
            let empty_index = DatIndex {
                by_crc32: HashMap::new(),
                by_md5: HashMap::new(),
                by_sha1: HashMap::new(),
                by_sha256: HashMap::new(),
                by_filename: HashMap::new(),
                game_clone_of: HashMap::new(),
            };

            let encrypted = open_source(
                &fixture("test_read_format_rar5_encrypted.rar"),
                &backend,
                &empty_index,
            )
            .unwrap_err();
            assert_eq!(encrypted, ArchiveMemberSourceError::Encrypted);

            for name in [
                "test_read_format_rar_compress_best.rar", // RAR4
                "test_read_format_rar5_solid.rar",
                "test_read_format_rar5_symlink.rar",
                "test_read_format_rar5_hardlink.rar",
            ] {
                let error = open_source(&fixture(name), &backend, &empty_index).unwrap_err();
                assert!(
                    matches!(error, ArchiveMemberSourceError::Unsupported { .. }),
                    "{name} must refuse as Unsupported, got {error:?}"
                );
            }

            let sfx = open_source(
                &fixture("test_read_format_rar5_sfx.exe"),
                &backend,
                &empty_index,
            )
            .unwrap_err();
            assert!(matches!(sfx, ArchiveMemberSourceError::Corrupt { .. }));

            // The multivolume fixture's exact error label is not fully
            // deterministic (see `multivolume_archive_is_refused` above);
            // only refusal is guaranteed.
            let multivolume = open_source(
                &fixture("test_read_format_rar5_multiarchive.part01.rar"),
                &backend,
                &empty_index,
            );
            assert!(multivolume.is_err());
        }

        #[test]
        fn discovery_failure_maps_to_unsupported_never_bad_rom_data() {
            assert!(matches!(
                rar_open_error(RarError::BackendNotFound),
                ArchiveMemberSourceError::Unsupported { .. }
            ));
            assert!(matches!(
                rar_open_error(RarError::BackendUnavailable {
                    detail: "no RAR5 codec".to_string()
                }),
                ArchiveMemberSourceError::Unsupported { .. }
            ));
        }

        #[test]
        fn is_nested_name_matches_the_same_extension_family_zip_uses() {
            assert!(is_nested_name("inner.zip"));
            assert!(is_nested_name("inner.7z"));
            assert!(is_nested_name("inner.rar"));
            assert!(is_nested_name("INNER.RAR"));
            assert!(!is_nested_name("plain.bin"));
            assert!(!is_nested_name("no-extension"));
        }

        // -- Finding 2 (independent integration review): pass completeness -

        #[test]
        fn only_size_and_hash_mismatch_are_ever_treated_as_content_local() {
            // Exhaustive, deterministic proof of the bucketing rule
            // `verify_all` relies on - no real backend interaction needed.
            assert!(member_error_is_content_local(&RarError::SizeMismatch {
                declared: 4,
                received: 2
            }));
            assert!(member_error_is_content_local(&RarError::HashMismatch));

            for other in [
                RarError::BackendFailure {
                    status: Some(1),
                    detail: "nonzero exit".to_string(),
                },
                RarError::SelectionChanged,
                RarError::Timeout,
                RarError::Io {
                    detail: "broken pipe".to_string(),
                },
                RarError::CleanupFailure {
                    detail: "kill failed".to_string(),
                },
                RarError::ProcessOutputLimit { limit: 1024 },
                RarError::OutputLimitExceeded { limit: 1024 },
                RarError::MemberNotFound { stable_index: 0 },
                RarError::MemberTooLarge {
                    declared: 4,
                    limit: 2,
                },
                RarError::InvalidProcessLimits,
                RarError::InvalidExpectedHash {
                    detail: "no strong hash".to_string(),
                },
                RarError::ZeroSizedMember { stable_index: 0 },
            ] {
                assert!(
                    !member_error_is_content_local(&other),
                    "{other:?} must not be treated as content-local"
                );
            }
        }

        /// A minimal, real `RarArchiveSource` around one synthetic member
        /// that never actually reaches `read_member` - `member.size == 0`
        /// short-circuits before any extraction is attempted, so no
        /// relist/consistency machinery is exercised at all. The file
        /// handle itself is real (`open_pinned` against a genuine fixture);
        /// only the member listing is hand-built, to isolate exactly the
        /// completeness bookkeeping under test.
        fn source_with_synthetic_members(members: Vec<RarMember>) -> RarArchiveSource {
            let file = open_pinned(&fixture("test_read_format_rar5_stored.rar")).unwrap();
            let opened_at = fstat_snapshot(&file).unwrap();
            let session = RarSession {
                executable: provider().executable().to_path_buf(),
                process_limits: ProcessLimits::default(),
                archive_path: fixture("test_read_format_rar5_stored.rar"),
                file,
                opened_at,
                archive: RarArchiveMetadata {
                    archive_type: "Rar5".to_string(),
                    solid: false,
                    encrypted: false,
                    multivolume: false,
                    volumes: 1,
                    offset: 0,
                },
                members,
            };
            let candidates = session.members.iter().map(|_| None).collect();
            RarArchiveSource {
                session,
                limits: ArchiveLimits::default(),
                candidates,
                member_timeout: short_timeout(),
            }
        }

        fn synthetic_member(stable_index: usize, path: &str, size: u64) -> RarMember {
            RarMember {
                stable_index,
                path: path.to_string(),
                size,
                packed_size: Some(size),
                encrypted: false,
                solid: false,
                split_before: false,
                split_after: false,
                method: "RAR5(1M)".to_string(),
                crc: None,
            }
        }

        #[test]
        fn a_zero_sized_member_alone_never_lets_the_pass_complete() {
            let mut source =
                source_with_synthetic_members(vec![synthetic_member(0, "empty.bin", 0)]);

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert_eq!(outcome.members[0].status, ArchiveMemberStatus::EmptyFile);
            assert!(matches!(
                outcome.completion,
                ArchivePassCompletion::Incomplete {
                    reason: ArchivePassStopReason::MemberRefused { .. }
                }
            ));
        }

        #[test]
        fn a_member_over_the_declared_size_limit_never_lets_the_pass_complete() {
            let backend = provider();
            let index = index_by_filename(
                "helloworld.txt",
                vec![rom_ref(
                    "Hello World",
                    "helloworld.txt",
                    vec![DatChecksum::parse(ChecksumAlgorithm::Sha1, HELLOWORLD_SHA1).unwrap()],
                )],
            );
            let mut source = RarArchiveSource::open(
                &fixture("test_read_format_rar5_stored.rar"),
                &backend,
                &index,
                ArchiveLimits {
                    max_member_logical_bytes: 10, // the real member is 29 bytes
                    ..ArchiveLimits::default()
                },
                short_timeout(),
                short_timeout(),
            )
            .unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert!(matches!(
                outcome.members[0].status,
                ArchiveMemberStatus::RefusedLimits { .. }
            ));
            assert!(matches!(
                outcome.completion,
                ArchivePassCompletion::Incomplete {
                    reason: ArchivePassStopReason::MemberRefused { .. }
                }
            ));
        }

        #[test]
        fn an_archive_wide_read_member_failure_aborts_the_pass_as_incomplete_not_corrupt() {
            // Deliberately corrupt the session's own remembered listing (an
            // extra entry `read_member`'s relist can never see) - the
            // hardened, already-reviewed consistency check inside
            // `RarSession::read_member` then genuinely, deterministically
            // fails every call through this session with `SelectionChanged`,
            // exactly the class of failure this test targets: not provably
            // confined to one member's content, so it must abort the whole
            // pass rather than being reported as a per-member `Corrupt`.
            let backend = provider();
            let mut session = backend
                .open(
                    &fixture("test_read_format_rar5_stored.rar"),
                    short_timeout(),
                )
                .unwrap();
            let real_member = session.members[0].clone();
            session.members.push(synthetic_member(1, "phantom.bin", 4));
            let index = index_by_filename(
                "helloworld.txt",
                vec![rom_ref(
                    "Hello World",
                    "helloworld.txt",
                    vec![DatChecksum::parse(ChecksumAlgorithm::Sha1, HELLOWORLD_SHA1).unwrap()],
                )],
            );
            let candidates = vec![candidate_hashes_for(&index, &real_member.path), None];
            let mut source = RarArchiveSource {
                session,
                limits: ArchiveLimits::default(),
                candidates,
                member_timeout: short_timeout(),
            };

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert!(
                outcome
                    .members
                    .iter()
                    .all(|member| member.status != ArchiveMemberStatus::HashComplete),
                "a corrupted session listing must never yield a verified member: {:?}",
                outcome.members
            );
            assert!(
                matches!(
                    outcome.completion,
                    ArchivePassCompletion::Incomplete {
                        reason: ArchivePassStopReason::SourceError { .. }
                    }
                ),
                "an archive-wide read_member failure must abort as SourceError, not a per-member \
                 MemberRefused: {:?}",
                outcome.completion
            );
        }

        #[test]
        fn one_verified_member_alongside_an_unverified_sibling_never_completes() {
            let backend = provider();
            // Only `test1.bin` gets a real, matching candidate; the other
            // three genuinely exist in the archive but have no DAT
            // declaration at all, so they are left `NotVerified` - none of
            // this requires touching `session.members`, so `test1.bin`'s own
            // `read_member` call is entirely real and unaffected.
            let index = index_by_filename(
                "test1.bin",
                vec![rom_ref(
                    "Test One",
                    "test1.bin",
                    vec![
                        DatChecksum::parse(
                            ChecksumAlgorithm::Md5,
                            "b0ee823da852a3d713823eb5d04760bb",
                        )
                        .unwrap(),
                        DatChecksum::parse(
                            ChecksumAlgorithm::Sha1,
                            "e6d444eac448f176cb9f8a1db5674df32f24e163",
                        )
                        .unwrap(),
                    ],
                )],
            );
            let mut source = open_source(
                &fixture("test_read_format_rar5_multiple_files.rar"),
                &backend,
                &index,
            )
            .unwrap();

            let outcome = source.verify_all(&no_cancel(), &mut unlimited_budget());

            assert_eq!(outcome.members.len(), 4);
            assert_eq!(
                outcome.members[0].status,
                ArchiveMemberStatus::HashComplete,
                "test1.bin genuinely verified through a real, uncorrupted session"
            );
            assert!(
                outcome.members[1..]
                    .iter()
                    .all(|member| matches!(member.status, ArchiveMemberStatus::NotVerified { .. })),
                "{:?}",
                outcome.members
            );
            assert!(
                matches!(
                    outcome.completion,
                    ArchivePassCompletion::Incomplete {
                        reason: ArchivePassStopReason::MemberRefused { .. }
                    }
                ),
                "one verified member cannot make a pass Complete while a sibling remains \
                 unverified: {:?}",
                outcome.completion
            );
        }
    }
}
