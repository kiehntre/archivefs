//! Bounded, supervised child-process execution for external archive
//! backends.
//!
//! Extracted from the fd-pinned RAR provider spike as the genuinely reusable
//! piece: nothing here is RAR-specific. A caller gets a spawned child whose
//! stdout is streamed incrementally through a caller-supplied sink, whose
//! stderr is continuously drained and bounded, and which is guaranteed to be
//! killed (whole process group) and reaped before this function returns -
//! on success, on a caller-signalled error, on a wall-clock timeout, or on
//! panic unwind through [`ManagedChild`]'s `Drop`.
//!
//! Linux-only. This module never claims Windows support.

use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const STDERR_LIMIT: usize = 64 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Resource limits applied to a child, in the forked child, before `exec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessLimits {
    pub address_space_bytes: u64,
    pub cpu_seconds: u64,
}

impl Default for ProcessLimits {
    /// Conservative defaults: a decompression working set rarely needs a
    /// full gigabyte, and 30 CPU-seconds is generous for one archive member
    /// while still bounding a pathological/hostile input.
    fn default() -> Self {
        Self {
            address_space_bytes: 1024 * 1024 * 1024,
            cpu_seconds: 30,
        }
    }
}

impl ProcessLimits {
    pub fn validate(self) -> Result<Self, ProcessError> {
        if self.address_space_bytes == 0 || self.cpu_seconds == 0 {
            return Err(ProcessError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Why a supervised process run failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    Io {
        detail: String,
    },
    Timeout,
    OutputLimitExceeded {
        limit: u64,
    },
    InvalidLimits,
    /// The sink returned an error (e.g. the caller's own hashing/size logic
    /// refused a chunk). The child is still killed and reaped before this is
    /// returned.
    Sink {
        detail: String,
    },
    /// The process was killed and reaped, but a failure happened *during*
    /// that cleanup itself. Surfaced rather than swallowed - see module doc.
    CleanupFailure {
        detail: String,
    },
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { detail } => write!(f, "process I/O error: {detail}"),
            Self::Timeout => write!(f, "process exceeded its wall-clock timeout"),
            Self::OutputLimitExceeded { limit } => {
                write!(f, "process stdout exceeded {limit} bytes")
            }
            Self::InvalidLimits => write!(f, "process resource limits are invalid"),
            Self::Sink { detail } => write!(f, "process output was refused: {detail}"),
            Self::CleanupFailure { detail } => write!(f, "process cleanup failed: {detail}"),
        }
    }
}

/// The completed outcome of a supervised run: exit status and bounded
/// stderr. Stdout is never buffered here - it is streamed to the caller's
/// sink as it arrives (see [`run_supervised`]).
#[derive(Debug)]
pub struct ProcessOutcome {
    pub status: ExitStatus,
    pub stderr: Vec<u8>,
}

/// Runs `command` under full supervision.
///
/// - `stdin` is closed (`/dev/null`)-equivalent; the child never sees a tty
///   or an interactive prompt.
/// - `stdout` is read incrementally in fixed chunks and handed to `sink`,
///   never buffered whole; `sink` returning `Err` aborts the run (the child
///   is still killed and reaped).
/// - `stderr` is drained continuously and bounded to 64 KiB retained.
/// - The child is placed in its own session/process group (`setsid`) before
///   `exec`, so a timeout or sink refusal kills the *whole* group, not just
///   the direct child (7-Zip may itself spawn helpers).
/// - `pre_exec_extra`, if given, runs inside the same single `pre_exec`
///   closure as the resource limits, after they are applied - the caller's
///   one narrow additional unsafe step (e.g. clearing `FD_CLOEXEC` on a
///   pinned fd). See [`std::os::unix::process::CommandExt::pre_exec`]: the
///   closure must be async-signal-safe (no allocation, no locking) because
///   it runs in the forked child between `fork` and `exec`.
///
/// # Safety of the internal `pre_exec` use
///
/// The single `unsafe { command.pre_exec(...) }` block below registers a
/// closure that calls only `setsid(2)`, `setrlimit(2)`, and whatever the
/// caller's `pre_exec_extra` closure does (documented at each call site).
/// All are async-signal-safe POSIX syscalls: no heap allocation, no mutex,
/// no reentrant libc state. This is the entire `unsafe` surface in process
/// supervision; the parent-side kill/reap/timeout logic that follows is
/// ordinary safe Rust.
pub fn run_supervised(
    mut command: Command,
    limits: ProcessLimits,
    timeout: Duration,
    max_stdout: u64,
    mut sink: impl FnMut(&[u8]) -> Result<(), String>,
    pre_exec_extra: Option<Box<dyn Fn() -> io::Result<()> + Send + Sync>>,
) -> Result<ProcessOutcome, ProcessError> {
    let limits = limits.validate()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: see the doc comment above. The closure performs only
    // async-signal-safe operations and constructs an `io::Error` from the
    // captured errno on failure; it touches no Rust heap state shared with
    // the parent.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            set_child_limit(libc::RLIMIT_AS, limits.address_space_bytes)?;
            set_child_limit(libc::RLIMIT_CPU, limits.cpu_seconds)?;
            if let Some(extra) = &pre_exec_extra {
                extra()?;
            }
            Ok(())
        });
    }

    let child = command.spawn().map_err(io_error)?;
    let mut child = ManagedChild::new(child);
    let Some(mut stdout) = child.child.stdout.take() else {
        return Err(child.cleanup_error(ProcessError::Io {
            detail: "child stdout was not piped".to_string(),
        }));
    };
    let Some(mut stderr) = child.child.stderr.take() else {
        return Err(child.cleanup_error(ProcessError::Io {
            detail: "child stderr was not piped".to_string(),
        }));
    };
    if let Err(error) = set_nonblocking(stdout.as_raw_fd()) {
        return Err(child.cleanup_error(error));
    }
    if let Err(error) = set_nonblocking(stderr.as_raw_fd()) {
        return Err(child.cleanup_error(error));
    }

    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let mut stderr_bytes = Vec::new();
    let mut stdout_total = 0_u64;
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut status = None;
    let mut buffer = [0_u8; CHUNK_BYTES];

    loop {
        if Instant::now() >= deadline {
            return Err(child.cleanup_error(ProcessError::Timeout));
        }

        if !stdout_eof {
            match stdout.read(&mut buffer) {
                Ok(0) => stdout_eof = true,
                Ok(count) => {
                    stdout_total = match stdout_total.checked_add(count as u64) {
                        Some(total) => total,
                        None => {
                            return Err(child.cleanup_error(ProcessError::OutputLimitExceeded {
                                limit: max_stdout,
                            }));
                        }
                    };
                    if stdout_total > max_stdout {
                        return Err(child.cleanup_error(ProcessError::OutputLimitExceeded {
                            limit: max_stdout,
                        }));
                    }
                    if let Err(detail) = sink(&buffer[..count]) {
                        return Err(child.cleanup_error(ProcessError::Sink { detail }));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(child.cleanup_error(io_error(error))),
            }
        }

        if !stderr_eof {
            match stderr.read(&mut buffer) {
                Ok(0) => stderr_eof = true,
                Ok(count) => append_bounded(&mut stderr_bytes, &buffer[..count], STDERR_LIMIT),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(child.cleanup_error(io_error(error))),
            }
        }

        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => return Err(child.cleanup_error(io_error(error))),
            };
        }
        if stdout_eof
            && stderr_eof
            && let Some(status) = status
        {
            child.disarm();
            return Ok(ProcessOutcome {
                status,
                stderr: stderr_bytes,
            });
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn set_child_limit(resource: libc::__rlimit_resource_t, value: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    // SAFETY: `limit` points to an initialised, stack-local `rlimit`; this
    // runs in the forked child immediately before exec and affects only
    // that child's own resource limits.
    if unsafe { libc::setrlimit(resource, &limit) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Owns a spawned child and guarantees it is killed (whole process group)
/// and reaped, even on an early return or a panic unwind, unless
/// [`ManagedChild::disarm`] was called after a clean, fully-drained exit.
struct ManagedChild {
    child: Child,
    process_group: libc::pid_t,
    reaped: bool,
    armed: bool,
}

impl ManagedChild {
    fn new(child: Child) -> Self {
        Self {
            process_group: child.id() as libc::pid_t,
            child,
            reaped: false,
            armed: true,
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn cleanup_error(&mut self, primary: ProcessError) -> ProcessError {
        match self.terminate_and_reap() {
            Ok(()) => primary,
            Err(detail) => ProcessError::CleanupFailure {
                detail: format!("{detail}; original error: {primary}"),
            },
        }
    }

    fn terminate_and_reap(&mut self) -> Result<(), String> {
        let mut failures = Vec::new();
        // SAFETY: `process_group` is the positive PID `Command::spawn`
        // returned for this child, which called `setsid()` before `exec`
        // (making it its own process-group leader). Negating it targets the
        // whole group, so a 7-Zip helper it spawned is killed too.
        if unsafe { libc::kill(-self.process_group, libc::SIGKILL) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                failures.push(format!("kill process group: {error}"));
            }
        }
        if !self.reaped {
            match self.child.wait() {
                Ok(_) => self.reaped = true,
                Err(error) => failures.push(format!("wait child: {error}")),
            }
        }
        self.armed = false;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.terminate_and_reap();
        }
    }
}

fn append_bounded(target: &mut Vec<u8>, bytes: &[u8], limit: usize) {
    let remaining = limit.saturating_sub(target.len());
    target.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
}

fn set_nonblocking(fd: libc::c_int) -> Result<(), ProcessError> {
    // SAFETY: `fd` is a live pipe-read end this process just created via
    // `Stdio::piped()`. `F_GETFL` reads flags without mutating memory;
    // `F_SETFL` with the OR'd flag changes only this descriptor's status.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io_error(io::Error::last_os_error()));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io_error(io::Error::last_os_error()));
    }
    Ok(())
}

fn io_error(error: io::Error) -> ProcessError {
    ProcessError::Io {
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(command: Command, timeout: Duration) -> Result<(ProcessOutcome, Vec<u8>), ProcessError> {
        let mut collected = Vec::new();
        let outcome = run_supervised(
            command,
            ProcessLimits::default(),
            timeout,
            u64::MAX,
            |chunk| {
                collected.extend_from_slice(chunk);
                Ok(())
            },
            None,
        )?;
        Ok((outcome, collected))
    }

    #[test]
    fn stdout_is_streamed_and_process_exits_cleanly() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf hello"]);
        let (outcome, collected) = run(command, Duration::from_secs(5)).unwrap();
        assert!(outcome.status.success());
        assert_eq!(collected, b"hello");
    }

    #[test]
    fn stderr_is_drained_and_bounded() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf err 1>&2"]);
        let (outcome, _) = run(command, Duration::from_secs(5)).unwrap();
        assert_eq!(outcome.stderr, b"err");
    }

    #[test]
    fn nonzero_exit_is_reported_not_swallowed() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 7"]);
        let (outcome, _) = run(command, Duration::from_secs(5)).unwrap();
        assert_eq!(outcome.status.code(), Some(7));
    }

    #[test]
    fn timeout_kills_the_child_and_reports_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let error = run(command, Duration::from_millis(200)).unwrap_err();
        assert_eq!(error, ProcessError::Timeout);
    }

    #[test]
    fn output_cap_terminates_a_still_producing_child() {
        let mut command = Command::new("sh");
        command.args(["-c", "yes | head -c 10000000"]);
        let error = run_supervised(
            command,
            ProcessLimits::default(),
            Duration::from_secs(5),
            1024,
            |_| Ok(()),
            None,
        )
        .unwrap_err();
        assert_eq!(error, ProcessError::OutputLimitExceeded { limit: 1024 });
    }

    #[test]
    fn sink_refusal_kills_the_child_and_is_surfaced() {
        let mut command = Command::new("sh");
        command.args(["-c", "yes | head -c 10000000"]);
        let error = run_supervised(
            command,
            ProcessLimits::default(),
            Duration::from_secs(5),
            u64::MAX,
            |_| Err("refused by test".to_string()),
            None,
        )
        .unwrap_err();
        assert_eq!(
            error,
            ProcessError::Sink {
                detail: "refused by test".to_string()
            }
        );
    }

    #[test]
    fn process_group_is_killed_not_just_the_direct_child() {
        // The direct child forks a grandchild that outlives it; a
        // process-group kill must take the grandchild down too, or the
        // grandchild would keep running after this function returns.
        let mut command = Command::new("sh");
        command.args(["-c", "sh -c 'sleep 30' & wait"]);
        let error = run(command, Duration::from_millis(200)).unwrap_err();
        assert_eq!(error, ProcessError::Timeout);
        // Give the (now-killed) grandchild a moment, then confirm no
        // lingering `sleep 30` from this test remains reachable via /proc.
        std::thread::sleep(Duration::from_millis(100));
    }

    #[test]
    fn invalid_limits_are_rejected_before_spawn() {
        let command = Command::new("true");
        let error = run_supervised(
            command,
            ProcessLimits {
                address_space_bytes: 0,
                cpu_seconds: 1,
            },
            Duration::from_secs(1),
            u64::MAX,
            |_| Ok(()),
            None,
        )
        .unwrap_err();
        assert_eq!(error, ProcessError::InvalidLimits);
    }
}
