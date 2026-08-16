//! `emuwiz-cli repair rollback` exit-code hardening.
//!
//! `FullyRolledBack` must exit 0; `PartiallyRolledBack` and `RollbackFailed`
//! must exit non-zero even though the normal (text or JSON) result is still
//! printed first. These spawn the real CLI binary (never the in-process
//! `repair::run`) so the actual process exit code - the contract a caller or
//! script depends on - is exercised, not just the `Result` `run()` returns
//! internally.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SHA1_TEST: &str = "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3";
const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
const SHA1_XYZ: &str = "66b27417d37e024c46526c2f6d358a754fc552f3";

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_archivefs-cli"))
        .args(args)
        .output()
        .expect("the CLI must run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp dir")
}

/// One DAT game/rom, one wrongly-named loose ROM: a single-entry batch.
fn single_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("single.dat");
    std::fs::write(
        &dat,
        format!(
            r#"<?xml version="1.0"?>
<datafile>
    <header><name>Single</name></header>
    <game name="Super Game (World)">
        <rom name="super.bin" size="4" sha1="{SHA1_TEST}"/>
    </game>
</datafile>"#
        ),
    )
    .unwrap();
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("wrongname.bin"), b"test").unwrap();
    (dat, roms)
}

/// Three DAT games, three wrongly-named loose ROMs: a three-entry batch, so
/// a rollback can reverse some entries before hitting a tampered one.
fn three_entry_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let dat = dir.join("three.dat");
    std::fs::write(
        &dat,
        format!(
            r#"<datafile><header><name>Three</name></header>
<game name="Alpha"><rom name="alpha.bin" size="4" sha1="{SHA1_TEST}"/></game>
<game name="Beta"><rom name="beta.bin" size="3" sha1="{SHA1_ABC}"/></game>
<game name="Gamma"><rom name="gamma.bin" size="3" sha1="{SHA1_XYZ}"/></game>
</datafile>"#
        ),
    )
    .unwrap();
    let roms = dir.join("roms");
    std::fs::create_dir(&roms).unwrap();
    std::fs::write(roms.join("a.bin"), b"test").unwrap();
    std::fs::write(roms.join("b.bin"), b"abc").unwrap();
    std::fs::write(roms.join("c.bin"), b"xyz").unwrap();
    (dat, roms)
}

fn plan_generation(plan_path: &Path) -> u64 {
    let text = std::fs::read_to_string(plan_path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["generation"].as_u64().expect("a generation number")
}

/// Scans and applies the whole plan through the real CLI, returning the
/// journal directory and the resulting transaction's id. Asserts exactly one
/// journal was written (a whole-plan apply is a single `RenameTransaction`).
fn scan_and_apply(dir: &Path, dat: &Path, roms: &Path) -> (PathBuf, String) {
    let plan_path = dir.join("plan.json");
    let journal_dir = dir.join("journal");

    let scan = run_cli(&[
        "repair",
        "scan",
        "--root",
        roms.to_str().unwrap(),
        "--dat",
        dat.to_str().unwrap(),
        "--plan-out",
        plan_path.to_str().unwrap(),
    ]);
    assert!(scan.status.success(), "{}", stdout(&scan));

    let generation = plan_generation(&plan_path).to_string();
    let apply = run_cli(&[
        "repair",
        "apply",
        "--plan",
        plan_path.to_str().unwrap(),
        "--root",
        roms.to_str().unwrap(),
        "--dat",
        dat.to_str().unwrap(),
        "--generation",
        &generation,
        "--journal-dir",
        journal_dir.to_str().unwrap(),
    ]);
    assert!(apply.status.success(), "{}", stdout(&apply));

    let journal_files: Vec<PathBuf> = std::fs::read_dir(&journal_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(journal_files.len(), 1, "{journal_files:?}");
    let transaction_id = journal_files[0]
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    (journal_dir, transaction_id)
}

// 1. FullyRolledBack => success (exit code 0).
#[test]
fn fully_rolled_back_exits_successfully() {
    let dir = temp();
    let (dat, roms) = single_fixture(dir.path());
    let (journal_dir, transaction_id) = scan_and_apply(dir.path(), &dat, &roms);

    let rollback = run_cli(&[
        "repair",
        "rollback",
        "--transaction",
        &transaction_id,
        "--journal-dir",
        journal_dir.to_str().unwrap(),
    ]);
    let text = stdout(&rollback);
    assert!(
        rollback.status.success(),
        "a full rollback must exit 0:\n{text}"
    );
    assert!(text.contains("FullyRolledBack"), "{text}");
    assert!(roms.join("wrongname.bin").exists());
}

// 2. PartiallyRolledBack => non-zero/error exit, with the normal text result
// still printed.
#[test]
fn partially_rolled_back_exits_non_zero() {
    let dir = temp();
    let (dat, roms) = three_entry_fixture(dir.path());
    let (journal_dir, transaction_id) = scan_and_apply(dir.path(), &dat, &roms);

    // Tamper the FIRST journaled entry's destination. Rollback runs in
    // reverse order, so the first entry is the *last* one reversed - this
    // guarantees the other two entries roll back successfully before the
    // tampered one is reached and stops the pass.
    let journal_path = journal_dir.join(format!("{transaction_id}.json"));
    let journal_value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&journal_path).unwrap()).unwrap();
    let first_destination = journal_value["entries"][0]["destination_path"]
        .as_str()
        .expect("the first entry's destination path")
        .to_string();
    std::fs::write(&first_destination, b"tampered-content-of-a-different-size").unwrap();

    let rollback = run_cli(&[
        "repair",
        "rollback",
        "--transaction",
        &transaction_id,
        "--journal-dir",
        journal_dir.to_str().unwrap(),
    ]);
    let text = stdout(&rollback);
    assert!(
        !rollback.status.success(),
        "a partial rollback must exit non-zero:\n{text}"
    );
    assert!(text.contains("PartiallyRolledBack"), "{text}");
    assert!(text.contains("Entries restored: 2"), "{text}");
    assert!(text.contains("Entries not restored: 1"), "{text}");
}

// 3. RollbackFailed => non-zero/error exit, with the normal text result
// still printed.
#[test]
fn rollback_failed_exits_non_zero() {
    let dir = temp();
    let (dat, roms) = single_fixture(dir.path());
    let (journal_dir, transaction_id) = scan_and_apply(dir.path(), &dat, &roms);

    std::fs::write(
        roms.join("super.bin"),
        b"tampered-content-of-a-different-size",
    )
    .unwrap();

    let rollback = run_cli(&[
        "repair",
        "rollback",
        "--transaction",
        &transaction_id,
        "--journal-dir",
        journal_dir.to_str().unwrap(),
    ]);
    let text = stdout(&rollback);
    assert!(
        !rollback.status.success(),
        "a failed rollback must exit non-zero:\n{text}"
    );
    assert!(text.contains("RollbackFailed"), "{text}");
    assert!(!roms.join("wrongname.bin").exists(), "nothing was restored");
}

// 4. JSON output is still emitted before the non-zero exit on failure.
#[test]
fn json_output_is_emitted_before_the_non_zero_return_on_failure() {
    let dir = temp();
    let (dat, roms) = single_fixture(dir.path());
    let (journal_dir, transaction_id) = scan_and_apply(dir.path(), &dat, &roms);

    std::fs::write(
        roms.join("super.bin"),
        b"tampered-content-of-a-different-size",
    )
    .unwrap();

    let rollback = run_cli(&[
        "repair",
        "rollback",
        "--transaction",
        &transaction_id,
        "--journal-dir",
        journal_dir.to_str().unwrap(),
        "--json",
    ]);
    let text = stdout(&rollback);
    assert!(
        !rollback.status.success(),
        "the process must still exit non-zero even though JSON was printed:\n{text}"
    );
    let value: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("valid JSON must still be on stdout ({error}):\n{text}"));
    assert_eq!(value["result"], "RollbackFailed");
    assert_eq!(value["transaction_id"], transaction_id);
}
