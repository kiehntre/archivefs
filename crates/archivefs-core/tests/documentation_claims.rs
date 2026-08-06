//! Guards specific, previously-false claims in the top-level docs against
//! silently drifting back to wrong again.
//!
//! `README.md` and `CHANGELOG.md` both stated "RomM integration is not
//! included" in their current (not historical) sections, while the RomM
//! identity source (`crates/archivefs-core/src/identity_source/romm`,
//! `crates/archivefs-cli/src/romm_identity.rs`, and the GUI's
//! `romm_config`/`romm_source`/`romm_browse`/`romm_game` modules) is a real,
//! tested, wired feature. These tests read the actual files rather than a
//! copy, so they fail the moment either claim reappears.

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/archivefs-core has two ancestors up to the repo root")
        .to_path_buf()
}

fn read_repo_file(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

#[test]
fn readme_does_not_claim_romm_is_unsupported() {
    let readme = read_repo_file("README.md");
    assert!(
        !readme.contains("RomM integration is not included"),
        "README.md must not claim RomM is unsupported: the identity source \
         is implemented, tested, and reachable from both the CLI \
         (`identity source romm ...`) and the GUI (Sources -> RomM)"
    );
    assert!(
        readme.contains("RomM"),
        "README.md should describe the RomM identity source somewhere, not omit it entirely"
    );
}

#[test]
fn changelog_does_not_claim_romm_is_unsupported() {
    let changelog = read_repo_file("CHANGELOG.md");
    assert!(
        !changelog.contains("RomM integration is not included"),
        "CHANGELOG.md's current (unreleased v0.7.0) Known limitations must not claim \
         RomM is unsupported"
    );
}

/// `ROADMAP.md` said RetroArch cheat rollback had "GUI support remains out
/// of scope" in one list entry while, a few dozen lines later in the same
/// "Completed foundations" list, describing "A working RetroArch GUI
/// apply/history/rollback flow" as already shipped - an internal
/// contradiction within a single document, not just doc-vs-code drift.
#[test]
fn roadmap_does_not_contradict_itself_about_retroarch_gui_rollback() {
    let roadmap = read_repo_file("ROADMAP.md");
    assert!(
        !roadmap.contains("GUI support remains out of scope"),
        "ROADMAP.md must not claim RetroArch rollback GUI support is out of scope: \
         the same document already lists a working GUI apply/history/rollback flow \
         as completed, and the GUI's History & Logs page (main.rs's \
         show_shared_rollback_card/start_shared_rollback) wires it end to end"
    );
}
