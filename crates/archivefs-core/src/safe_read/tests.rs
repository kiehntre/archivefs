//! Tests for the one bounded-read policy.
//!
//! Every case builds a real tree, because the whole point of this module is how
//! it behaves against real inodes, symlinks and special files.

use super::*;
use std::path::PathBuf;

/// A throwaway tree. Two roots are provided so "inside a trusted root" and
/// "outside every trusted root" are both expressible.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "archivefs-safe-read-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for directory in ["library", "downloads", "elsewhere"] {
            std::fs::create_dir_all(root.join(directory)).expect("fixture");
        }
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn file(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture");
        }
        std::fs::write(&path, contents).expect("fixture");
        path
    }

    fn link(&self, from: &str, to: &Path) -> PathBuf {
        let path = self.path(from);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(to, &path).expect("fixture");
        path
    }

    /// The library and the download tree are both configured sources; the
    /// `elsewhere` directory deliberately is not.
    fn trusted(&self) -> TrustedRoots {
        TrustedRoots::from_paths([self.path("library"), self.path("downloads")])
    }

    /// Every entry beneath the fixture, with what a mutation would disturb.
    fn snapshot(&self) -> std::collections::BTreeMap<String, String> {
        let mut entries = std::collections::BTreeMap::new();
        let mut stack = vec![self.root.clone()];
        while let Some(current) = stack.pop() {
            let Ok(read_dir) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in read_dir.filter_map(Result::ok) {
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    stack.push(path.clone());
                }
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode()
                };
                #[cfg(not(unix))]
                let mode = 0;
                entries.insert(
                    path.to_string_lossy().into_owned(),
                    format!(
                        "{:?}|{}|{mode:o}|{:?}|{:?}",
                        metadata.file_type(),
                        metadata.len(),
                        metadata.modified().ok(),
                        metadata.accessed().ok().is_some()
                    ),
                );
            }
        }
        entries
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const PS2_OFFSET: u64 = 0x8008;

/// An ISO 9660 primary volume descriptor naming `PLAYSTATION`.
fn playstation_image() -> Vec<u8> {
    let mut image = vec![0_u8; 0x8100];
    image[0x8001..0x8006].copy_from_slice(b"CD001");
    image[0x8008..0x8013].copy_from_slice(b"PLAYSTATION");
    image
}

// --- Permitted ------------------------------------------------------------

/// Test 1
#[test]
fn a_symlink_inside_a_trusted_root_to_a_file_inside_a_trusted_root_is_read() {
    let fixture = Fixture::new("permitted");
    let target = fixture.file("downloads/game.iso", &playstation_image());
    let link = fixture.link("library/game.iso", &target);

    let mut file = open_bounded_read(&link, &fixture.trusted()).expect("should be permitted");
    assert!(
        file.resolved_via_symlink(),
        "the caller must be able to say a link was followed"
    );
    assert_eq!(
        file.read_exact_at(PS2_OFFSET, 11, 64).as_deref(),
        Some(&b"PLAYSTATION"[..])
    );
}

/// Test 2
#[test]
fn a_plain_file_is_read_exactly_as_before_and_is_not_marked_as_a_symlink() {
    let fixture = Fixture::new("plain");
    let target = fixture.file("library/game.iso", &playstation_image());

    let mut file = open_bounded_read(&target, &TrustedRoots::none())
        .expect("a plain file needs no trusted roots at all");
    assert!(!file.resolved_via_symlink());
    assert_eq!(
        file.read_exact_at(PS2_OFFSET, 11, 64).as_deref(),
        Some(&b"PLAYSTATION"[..])
    );
}

/// Test 3
#[test]
fn a_nested_symlink_chain_inside_trusted_roots_resolves() {
    let fixture = Fixture::new("chain");
    let target = fixture.file("downloads/real.iso", &playstation_image());
    let first = fixture.link("downloads/hop.iso", &target);
    let entry = fixture.link("library/game.iso", &first);

    let mut file =
        open_bounded_read(&entry, &fixture.trusted()).expect("a chain inside the roots is fine");
    assert!(file.resolved_via_symlink());
    assert_eq!(
        file.read_exact_at(PS2_OFFSET, 11, 64).as_deref(),
        Some(&b"PLAYSTATION"[..])
    );
}

// --- Refused --------------------------------------------------------------

/// Test 4
#[test]
fn a_symlink_is_refused_when_no_trusted_root_is_configured() {
    let fixture = Fixture::new("fail-closed");
    let target = fixture.file("downloads/game.iso", &playstation_image());
    let link = fixture.link("library/game.iso", &target);

    assert_eq!(
        open_bounded_read(&link, &TrustedRoots::none()).unwrap_err(),
        SafeReadRefusal::NoTrustedRoots,
        "absent roots must fail closed, so existing callers keep refusing symlinks"
    );
}

/// Test 5
#[test]
fn a_symlink_whose_target_escapes_every_trusted_root_is_refused() {
    let fixture = Fixture::new("escape");
    let target = fixture.file("elsewhere/secret.iso", &playstation_image());
    let link = fixture.link("library/game.iso", &target);

    assert_eq!(
        open_bounded_read(&link, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::TargetOutsideTrustedRoots
    );
}

/// Test 6
#[test]
fn a_relative_escape_out_of_a_trusted_root_is_refused() {
    let fixture = Fixture::new("relative-escape");
    fixture.file("elsewhere/secret.iso", &playstation_image());
    // A relative link climbing out of the library and into an untrusted tree.
    let link = fixture.link("library/game.iso", Path::new("../elsewhere/secret.iso"));

    assert_eq!(
        open_bounded_read(&link, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::TargetOutsideTrustedRoots,
        "`..` in a link target must not smuggle a read outside the roots"
    );
}

/// Test 7
#[test]
fn a_nested_chain_that_escapes_is_refused_even_though_its_first_hop_is_trusted() {
    let fixture = Fixture::new("chain-escape");
    let outside = fixture.file("elsewhere/secret.iso", &playstation_image());
    let hop = fixture.link("downloads/hop.iso", &outside);
    let entry = fixture.link("library/game.iso", &hop);

    assert_eq!(
        open_bounded_read(&entry, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::TargetOutsideTrustedRoots,
        "containment is decided by where the chain ends, not where it starts"
    );
}

/// Test 8
#[test]
fn a_symlink_loop_is_refused() {
    let fixture = Fixture::new("loop");
    let first = fixture.path("library/a.iso");
    let second = fixture.path("library/b.iso");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&second, &first).expect("fixture");
        std::os::unix::fs::symlink(&first, &second).expect("fixture");
    }
    let error = open_bounded_read(&first, &fixture.trusted()).unwrap_err();
    assert_eq!(
        error.code(),
        "unresolvable_target",
        "a loop must be refused, not followed: {error:?}"
    );
}

/// Test 9
#[test]
fn a_broken_symlink_is_refused() {
    let fixture = Fixture::new("broken");
    let link = fixture.link("library/game.iso", &fixture.path("downloads/absent.iso"));

    assert_eq!(
        open_bounded_read(&link, &fixture.trusted())
            .unwrap_err()
            .code(),
        "unresolvable_target"
    );
}

/// Test 10
#[test]
fn a_symlink_to_a_directory_is_refused() {
    let fixture = Fixture::new("directory");
    let link = fixture.link("library/game.iso", &fixture.path("downloads"));

    assert_eq!(
        open_bounded_read(&link, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::NotRegularFile
    );
}

/// Test 11
#[test]
fn a_directory_is_refused_without_any_symlink_involved() {
    let fixture = Fixture::new("plain-directory");
    assert_eq!(
        open_bounded_read(&fixture.path("library"), &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::NotRegularFile
    );
}

/// Test 12
#[cfg(unix)]
#[test]
fn a_symlink_to_a_fifo_is_refused() {
    let fixture = Fixture::new("fifo");
    let fifo = fixture.path("downloads/pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("cstring");
    // SAFETY: `name` is a valid NUL-terminated path inside a fresh temporary
    // directory; `mkfifo` only creates the FIFO the test then refuses to read.
    let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
    if made != 0 {
        // Some filesystems cannot host a FIFO; skip rather than fail falsely.
        return;
    }
    let link = fixture.link("library/game.iso", &fifo);
    assert_eq!(
        open_bounded_read(&link, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::NotRegularFile,
        "a FIFO would block a read forever and is never a game file"
    );
    // And directly, with no symlink in the way.
    assert_eq!(
        open_bounded_read(&fifo, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::NotRegularFile
    );
}

/// Test 13
#[cfg(unix)]
#[test]
fn a_symlink_to_a_character_device_is_refused() {
    let fixture = Fixture::new("device");
    // /dev/null is a character device that certainly exists.
    let link = fixture.link("library/game.iso", Path::new("/dev/null"));
    let error = open_bounded_read(&link, &fixture.trusted()).unwrap_err();
    assert!(
        matches!(
            error,
            SafeReadRefusal::NotRegularFile | SafeReadRefusal::TargetOutsideTrustedRoots
        ),
        "a device must be refused, either for its type or for leaving the roots: {error:?}"
    );
}

/// Test 14
#[test]
fn a_symlink_outside_every_trusted_root_is_refused_even_when_its_target_is_trusted() {
    let fixture = Fixture::new("link-outside");
    let target = fixture.file("downloads/game.iso", &playstation_image());
    let link = fixture.link("elsewhere/game.iso", &target);

    assert_eq!(
        open_bounded_read(&link, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::SymlinkOutsideTrustedRoots,
        "the link itself has to be part of the library"
    );
}

/// Test 15
#[test]
fn a_symlinked_parent_directory_is_still_refused_for_a_plain_file() {
    let fixture = Fixture::new("symlinked-parent");
    fixture.file("downloads/inner/game.iso", &playstation_image());
    fixture.link("library/inner", &fixture.path("downloads/inner"));
    let through_link = fixture.path("library/inner/game.iso");

    // The final component is a real file, but it was reached through a link.
    // This is the case the old platform detector silently allowed.
    let error = open_bounded_read(&through_link, &fixture.trusted()).unwrap_err();
    assert_eq!(
        error.code(),
        "symlink_in_path",
        "a symlinked parent must be refused, not silently followed: {error:?}"
    );
}

/// Test 16
#[test]
fn a_relative_path_and_a_dot_dot_component_are_refused() {
    let fixture = Fixture::new("traversal");
    assert_eq!(
        open_bounded_read(Path::new("library/game.iso"), &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::NotAbsolute
    );
    let traversing = fixture.path("library/../elsewhere/secret.iso");
    assert_eq!(
        open_bounded_read(&traversing, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::NonNormalComponent
    );
}

/// Test 17
#[test]
fn a_missing_file_is_refused() {
    let fixture = Fixture::new("missing");
    assert_eq!(
        open_bounded_read(&fixture.path("library/absent.iso"), &fixture.trusted())
            .unwrap_err()
            .code(),
        "unreadable"
    );
}

// --- Bounds and purity ----------------------------------------------------

/// Test 18
#[test]
fn reads_stay_within_the_callers_bound_and_the_file() {
    let fixture = Fixture::new("bounds");
    let target = fixture.file("library/game.iso", &playstation_image());
    let mut file = open_bounded_read(&target, &fixture.trusted()).expect("permitted");

    assert!(
        file.read_exact_at(0, 65, 64).is_none(),
        "a read longer than the caller's bound must be refused outright"
    );
    assert!(
        file.read_exact_at(0, 0, 64).is_none(),
        "a zero-length read is meaningless"
    );
    assert!(
        file.read_exact_at(0x8100, 8, 64).is_none(),
        "a read past the end of the file must be refused, not truncated"
    );
    assert!(
        file.read_exact_at(0x80fc, 8, 64).is_none(),
        "a read straddling the end must be refused too"
    );
    assert_eq!(file.read_exact_at(0, 5, 64).map(|b| b.len()), Some(5));
}

/// Test 19
#[test]
fn opening_and_reading_never_changes_the_tree() {
    let fixture = Fixture::new("read-only");
    let target = fixture.file("downloads/game.iso", &playstation_image());
    let link = fixture.link("library/game.iso", &target);
    fixture.file("library/plain.iso", &playstation_image());
    fixture.link("library/broken.iso", &fixture.path("downloads/absent.iso"));
    let before = fixture.snapshot();

    for path in [
        link,
        fixture.path("library/plain.iso"),
        fixture.path("library/broken.iso"),
        fixture.path("library"),
        fixture.path("library/absent.iso"),
    ] {
        if let Ok(mut file) = open_bounded_read(&path, &fixture.trusted()) {
            let _ = file.read_exact_at(PS2_OFFSET, 11, 64);
        }
    }

    assert_eq!(
        fixture.snapshot(),
        before,
        "no file, mode or timestamp may change: this module only ever reads"
    );
}

/// Test 20
#[test]
fn trusted_roots_are_canonicalised_and_reject_unusable_entries() {
    let fixture = Fixture::new("roots");
    let roots = TrustedRoots::from_paths([
        fixture.path("library"),
        fixture.path("does-not-exist"),
        PathBuf::from("relative/path"),
        fixture.path("library/../library"),
    ]);
    assert_eq!(
        roots.roots().len(),
        1,
        "a missing root, a relative root and a duplicate must all be dropped: {:?}",
        roots.roots()
    );
    assert!(!roots.is_empty());
    assert!(TrustedRoots::none().is_empty());

    // A file target must not be accepted as a root.
    let file = fixture.file("library/game.iso", b"x");
    assert!(TrustedRoots::from_paths([file]).is_empty());
}

/// Test 21
#[test]
fn a_sibling_directory_with_a_shared_name_prefix_is_not_inside_the_root() {
    let fixture = Fixture::new("prefix");
    std::fs::create_dir_all(fixture.path("library-backup")).expect("fixture");
    let target = fixture.file("library-backup/game.iso", &playstation_image());
    let link = fixture.link("library/game.iso", &target);

    assert_eq!(
        open_bounded_read(&link, &fixture.trusted()).unwrap_err(),
        SafeReadRefusal::TargetOutsideTrustedRoots,
        "`library-backup` must not count as being inside `library`"
    );
}

/// Test 22
#[test]
fn every_refusal_explains_itself_and_has_a_stable_code() {
    let cases = [
        SafeReadRefusal::NotAbsolute,
        SafeReadRefusal::NonNormalComponent,
        SafeReadRefusal::SymlinkInPath(PathBuf::from("/roms/link")),
        SafeReadRefusal::Unreadable("boom".to_string()),
        SafeReadRefusal::NotRegularFile,
        SafeReadRefusal::NoTrustedRoots,
        SafeReadRefusal::SymlinkOutsideTrustedRoots,
        SafeReadRefusal::TargetOutsideTrustedRoots,
        SafeReadRefusal::UnresolvableTarget("boom".to_string()),
        SafeReadRefusal::ChangedWhileOpening,
        SafeReadRefusal::OpenFailed("boom".to_string()),
    ];
    let mut codes = Vec::new();
    for case in &cases {
        assert!(!case.detail().is_empty(), "{case:?} has no explanation");
        assert!(!case.code().is_empty());
        codes.push(case.code());
    }
    let mut unique = codes.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(codes.len(), unique.len(), "two refusals share a code");
}

/// Test 23
#[test]
fn this_module_contains_no_write_process_or_network_call() {
    let whole = include_str!("mod.rs");
    let code = whole.split("#[cfg(test)]").next().expect("production half");
    for forbidden in [
        "fs::write",
        "fs::create_dir",
        "fs::remove_",
        "fs::rename",
        "fs::set_permissions",
        "File::create",
        "ureq",
        "reqwest",
        "Command",
        "std::process",
        ".write(true)",
        ".create(true)",
        ".append(true)",
        ".truncate(true)",
    ] {
        assert!(
            !code.contains(forbidden),
            "`{forbidden}` must never appear in a read-only open policy"
        );
    }
}
