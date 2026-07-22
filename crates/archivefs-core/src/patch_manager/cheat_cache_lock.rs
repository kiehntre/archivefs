//! One cross-process lock protocol for the RetroArch cheat-source cache.
//!
//! The lock is held on an open descriptor for the cache-root directory, so
//! read-only operations create no lock file. The operating system releases
//! the advisory lock when the descriptor closes or the process exits. Callers
//! acquire once at their public operation boundary and pass
//! [`LockedCheatCache`] to locked helpers.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;

use super::cheat_sources::{
    CheatSourceError, cache_error, safe_regular_or_directory, validate_cache_path_for_read,
};

#[cfg(not(test))]
pub(super) const CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
pub(super) const CACHE_LOCK_TIMEOUT: Duration = Duration::from_millis(250);
const CACHE_LOCK_RETRY: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub(super) struct LockedCheatCache {
    root: PathBuf,
    guard: Option<CheatCacheLockGuard>,
}

impl LockedCheatCache {
    pub(super) fn acquire_existing(root: &Path) -> Result<Self, CheatSourceError> {
        Self::acquire_existing_with_timeout(root, CACHE_LOCK_TIMEOUT)
    }

    pub(super) fn acquire_existing_with_timeout(
        root: &Path,
        timeout: Duration,
    ) -> Result<Self, CheatSourceError> {
        validate_cache_root_identity(root)?;
        if !root.exists() {
            return Ok(Self {
                root: root.to_path_buf(),
                guard: None,
            });
        }
        safe_regular_or_directory(root, true)?;
        let guard = CheatCacheLockGuard::acquire(root, timeout)?;
        Ok(Self {
            root: root.to_path_buf(),
            guard: Some(guard),
        })
    }

    pub(super) fn acquire_required(root: &Path) -> Result<Self, CheatSourceError> {
        Self::acquire_required_with_timeout(root, CACHE_LOCK_TIMEOUT)
    }

    pub(super) fn acquire_required_with_timeout(
        root: &Path,
        timeout: Duration,
    ) -> Result<Self, CheatSourceError> {
        validate_cache_root_identity(root)?;
        safe_regular_or_directory(root, true)?;
        let guard = CheatCacheLockGuard::acquire(root, timeout)?;
        Ok(Self {
            root: root.to_path_buf(),
            guard: Some(guard),
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn present_at_acquisition(&self) -> bool {
        self.guard.is_some()
    }
}

#[derive(Debug)]
struct CheatCacheLockGuard {
    file: File,
}

impl CheatCacheLockGuard {
    fn acquire(root: &Path, timeout: Duration) -> Result<Self, CheatSourceError> {
        #[cfg(not(unix))]
        return Err(cache_error(
            "cache_lock_unsupported",
            "RetroArch cheat cache locking is not supported on this platform",
        ));

        validate_cache_path_for_read(root)?;
        safe_regular_or_directory(root, true)?;
        let file =
            File::open(root).map_err(|error| cache_error("cache_lock_open_failed", error))?;
        let deadline = Instant::now().checked_add(timeout);
        loop {
            match try_lock_exclusive(&file) {
                Ok(true) => return Ok(Self { file }),
                Ok(false) => {
                    if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
                        return Err(cache_error(
                            "cache_lock_timeout",
                            format!(
                                "timed out after {} ms waiting for the RetroArch cheat cache lock",
                                timeout.as_millis()
                            ),
                        ));
                    }
                    thread::sleep(CACHE_LOCK_RETRY.min(timeout));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for CheatCacheLockGuard {
    fn drop(&mut self) {
        unlock(&self.file);
    }
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> Result<bool, CheatSourceError> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    let raw = error.raw_os_error();
    if raw == Some(libc::EWOULDBLOCK) || raw == Some(libc::EAGAIN) || raw == Some(libc::EINTR) {
        Ok(false)
    } else {
        Err(cache_error("cache_lock_acquire_failed", error))
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> Result<bool, CheatSourceError> {
    Err(cache_error(
        "cache_lock_unsupported",
        "RetroArch cheat cache locking is not supported on this platform",
    ))
}

#[cfg(unix)]
fn unlock(file: &File) {
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

#[cfg(not(unix))]
fn unlock(_file: &File) {}

pub(super) fn validate_cache_root_identity(root: &Path) -> Result<(), CheatSourceError> {
    validate_cache_path_for_read(root)?;
    if !root.is_absolute() || root.parent().is_none() {
        return Err(cache_error(
            "unsafe_cache_root",
            "cheat cache roots must be absolute and cannot be filesystem roots",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::Command;
    use std::{fs, path::PathBuf};

    use super::*;

    struct Temp(PathBuf);

    impl Temp {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-cache-lock-{label}-{}-{}",
                std::process::id(),
                super::super::cheat_sources::now_seconds()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_read_only_cache_is_not_created_and_roots_are_strict() {
        let temp = Temp::new("missing");
        let missing = temp.0.join("absent");
        let locked = LockedCheatCache::acquire_existing(&missing).unwrap();
        assert_eq!(locked.root(), missing);
        assert!(!missing.exists());
        assert!(LockedCheatCache::acquire_existing(Path::new("relative")).is_err());
        assert!(LockedCheatCache::acquire_existing(Path::new("/")).is_err());
    }

    #[test]
    fn recursive_acquisition_times_out_and_release_allows_retry() {
        let temp = Temp::new("recursive");
        let first = LockedCheatCache::acquire_required(&temp.0).unwrap();
        let error =
            LockedCheatCache::acquire_required_with_timeout(&temp.0, Duration::from_millis(60))
                .unwrap_err();
        assert_eq!(error.code, "cache_lock_timeout");
        drop(first);
        LockedCheatCache::acquire_required_with_timeout(&temp.0, Duration::from_secs(1)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lock_path_is_refused() {
        use std::os::unix::fs::symlink;
        let temp = Temp::new("symlink");
        let outside = temp.0.join("outside");
        let linked = temp.0.join("linked-cache");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &linked).unwrap();
        let error = LockedCheatCache::acquire_required(&linked).unwrap_err();
        assert_eq!(error.code, "unsafe_cache_symlink");
    }

    #[test]
    fn prefix_collision_roots_have_distinct_locks() {
        let temp = Temp::new("prefix");
        let first_root = temp.0.join("cache");
        let second_root = temp.0.join("cache-old");
        fs::create_dir(&first_root).unwrap();
        fs::create_dir(&second_root).unwrap();
        let _first = LockedCheatCache::acquire_required(&first_root).unwrap();
        let _second = LockedCheatCache::acquire_required(&second_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_roots_have_distinct_locks() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let temp = Temp::new("non-utf8");
        let first_root = temp.0.join(OsString::from_vec(vec![b'c', 0xfe]));
        let second_root = temp.0.join(OsString::from_vec(vec![b'c', 0xff]));
        fs::create_dir(&first_root).unwrap();
        fs::create_dir(&second_root).unwrap();
        let _first = LockedCheatCache::acquire_required(&first_root).unwrap();
        let _second = LockedCheatCache::acquire_required(&second_root).unwrap();
    }

    #[test]
    fn real_child_process_contention_times_out_then_releases() {
        let temp = Temp::new("child");
        let mut child = spawn_lock_child(&temp.0, 500);
        wait_for_child_lock(&mut child, &temp.0);
        let error =
            LockedCheatCache::acquire_required_with_timeout(&temp.0, Duration::from_millis(80))
                .unwrap_err();
        assert_eq!(error.code, "cache_lock_timeout");
        assert!(child.wait().unwrap().success());
        LockedCheatCache::acquire_required_with_timeout(&temp.0, Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn process_termination_releases_lock_without_poisoning() {
        let temp = Temp::new("termination");
        let mut child = spawn_lock_child(&temp.0, 10_000);
        wait_for_child_lock(&mut child, &temp.0);
        child.kill().unwrap();
        child.wait().unwrap();
        LockedCheatCache::acquire_required_with_timeout(&temp.0, Duration::from_secs(1)).unwrap();
    }

    fn spawn_lock_child(root: &Path, hold_millis: u64) -> std::process::Child {
        Command::new(std::env::current_exe().unwrap())
            .arg("lock_child_helper")
            .arg("--nocapture")
            .env("ARCHIVEFS_LOCK_CHILD", "1")
            .env("ARCHIVEFS_LOCK_ROOT", root)
            .env("ARCHIVEFS_LOCK_HOLD_MILLIS", hold_millis.to_string())
            .spawn()
            .unwrap()
    }

    fn wait_for_child_lock(child: &mut std::process::Child, root: &Path) {
        let ready = root.join("child-ready");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            assert!(
                child.try_wait().unwrap().is_none(),
                "lock child exited early"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "lock child did not signal readiness");
    }

    #[test]
    fn lock_child_helper() {
        if std::env::var_os("ARCHIVEFS_LOCK_CHILD").is_none() {
            return;
        }
        let root = PathBuf::from(std::env::var_os("ARCHIVEFS_LOCK_ROOT").unwrap());
        let hold = std::env::var("ARCHIVEFS_LOCK_HOLD_MILLIS")
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let _lock =
            LockedCheatCache::acquire_required_with_timeout(&root, Duration::from_secs(1)).unwrap();
        fs::write(root.join("child-ready"), b"ready").unwrap();
        println!("LOCKED");
        std::io::stdout().flush().unwrap();
        thread::sleep(Duration::from_millis(hold));
    }
}
