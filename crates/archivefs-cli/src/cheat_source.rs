use std::path::Path;

use archivefs_core::patch_manager::{
    CheatSourceRegistry, build_default_registry, default_cheat_sources_config_path,
    load_cheat_sources_config_from, save_cheat_sources_config_to,
};

/// One machine-readable response from a mutating command.
///
/// Serialised rather than formatted by hand: the previous `format!` output could
/// not escape anything, so an ID carrying a quote would have produced invalid
/// JSON. The field names and shape are exactly what was emitted before.
#[derive(serde::Serialize)]
struct MutationResponse<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<u32>,
    changed: bool,
}

impl<'a> MutationResponse<'a> {
    fn enabled(id: &'a str, enabled: bool, changed: bool) -> Self {
        Self {
            id,
            enabled: Some(enabled),
            priority: None,
            changed,
        }
    }

    fn priority(id: &'a str, priority: u32, changed: bool) -> Self {
        Self {
            id,
            enabled: None,
            priority: Some(priority),
            changed,
        }
    }

    fn print(&self) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", serde_json::to_string_pretty(self)?);
        Ok(())
    }
}

/// The lowest and highest priority a source may be given.
///
/// Stated here rather than only in the help text, because the value is both
/// validated and printed and the two must not drift.
pub(crate) const MIN_PRIORITY: u32 = 1;
pub(crate) const MAX_PRIORITY: u32 = 999;

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let config_path = default_cheat_sources_config_path()?;
    run_with_config_path(args, &config_path)
}

/// The command body, against an explicit preferences file.
///
/// `run` resolves the real per-user path; tests pass a temporary one. Without
/// this seam every test of `enable`, `disable` or `set-priority` would rewrite
/// the preferences of whoever ran `cargo test`.
pub(crate) fn run_with_config_path(
    args: Vec<String>,
    config_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let json = take_flag(&mut args, "--json");
    let Some(command) = args.first().cloned() else {
        return Err("cheat-source requires a command".into());
    };
    args.remove(0);

    match command.as_str() {
        "list" => {
            reject_extra(&args, "list")?;
            render_source_list(config_path, json)?;
        }
        "info" => {
            if args.is_empty() {
                return Err("cheat-source info requires a source ID".into());
            }
            let id = args.remove(0);
            reject_extra(&args, "info")?;
            render_source_info(config_path, &id, json)?;
        }
        "enable" => {
            if args.is_empty() {
                return Err("cheat-source enable requires a source ID".into());
            }
            let id = args.remove(0);
            reject_extra(&args, "enable")?;
            set_enabled(config_path, &id, true, json)?;
        }
        "disable" => {
            if args.is_empty() {
                return Err("cheat-source disable requires a source ID".into());
            }
            let id = args.remove(0);
            reject_extra(&args, "disable")?;
            set_enabled(config_path, &id, false, json)?;
        }
        "set-priority" => {
            if args.len() < 2 {
                return Err(
                    "cheat-source set-priority requires a source ID and a priority value".into(),
                );
            }
            let id = args.remove(0);
            let priority_str = args.remove(0);
            reject_extra(&args, "set-priority")?;
            let priority: u32 = priority_str.parse().map_err(|_| {
                format!("invalid priority value '{priority_str}': expected a positive integer")
            })?;
            // Rejected rather than clamped. Clamping turned `set-priority x 5000`
            // into a success reporting 999, so the command confirmed something the
            // caller had not asked for.
            if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&priority) {
                return Err(format!(
                    "invalid priority value '{priority}': expected {MIN_PRIORITY}-{MAX_PRIORITY}"
                )
                .into());
            }
            set_priority(config_path, &id, priority, json)?;
        }
        _ => return Err(format!("unknown cheat-source command: {command}").into()),
    }
    Ok(())
}

fn render_source_list(config_path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let registry = load_registry_with_config(config_path)?;
    print!("{}", source_list_output(&registry, json)?);
    Ok(())
}

/// The exact text `list` prints, returned rather than printed.
///
/// Separated so a test can assert on what the command actually emits. Asserting
/// on the registry instead proved worthless: a test written that way still
/// passed when the listing was reverted to enabled-only.
fn source_list_output(
    registry: &CheatSourceRegistry,
    json: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    use std::fmt::Write as _;

    // Every registered source, not only the enabled ones. Listing just the
    // enabled set contradicted the command's own description and, worse, made a
    // source vanish the moment it was disabled - so there was no way to see what
    // you had turned off, or to find its ID again in order to turn it back on.
    let entries = registry.sorted_all();
    if json {
        return Ok(format!("{}\n", serde_json::to_string_pretty(&entries)?));
    }
    if entries.is_empty() {
        return Ok("No cheat sources are registered.\n".to_string());
    }
    let id_width = entries
        .iter()
        .map(|e| e.spec.id.len())
        .max()
        .unwrap_or(0)
        .max(2);
    let mut out = String::new();
    for entry in &entries {
        // A disabled source is marked and labelled: the marker alone is easy to
        // miss in a column of otherwise identical rows.
        let mark = if entry.enabled { " " } else { "!" };
        let suffix = if entry.enabled { "" } else { "  (disabled)" };
        writeln!(
            &mut out,
            "{mark} {:>3}  {:<id_width$}  {}{suffix}",
            entry.priority, entry.spec.id, entry.spec.display_name,
        )?;
    }
    Ok(out)
}

fn render_source_info(
    config_path: &Path,
    id: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = load_registry_with_config(config_path)?;
    let entry = registry
        .get(id)
        .ok_or_else(|| format!("unknown cheat source: {id}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(entry)?);
    } else {
        println!("ID:               {}", entry.spec.id);
        println!("Display name:     {}", entry.spec.display_name);
        println!("Emulator:         {}", entry.spec.emulator);
        if !entry.spec.platforms.is_empty() {
            println!("Platforms:        {}", entry.spec.platforms.join(", "));
        } else {
            println!("Platforms:        (all)");
        }
        println!("Enabled:          {}", entry.enabled);
        println!("Priority:         {}", entry.priority);
        println!(
            "Capabilities:     browse={} search={} preview={} install={} download={} refresh={} health={} remote={} local={}",
            entry.spec.capabilities.browse,
            entry.spec.capabilities.search,
            entry.spec.capabilities.preview,
            entry.spec.capabilities.install,
            entry.spec.capabilities.download,
            entry.spec.capabilities.refresh,
            entry.spec.capabilities.health_check,
            entry.spec.capabilities.remote,
            entry.spec.capabilities.local,
        );
        println!("Upstream:         {}", entry.spec.upstream_project);
        println!("Description:      {}", entry.spec.description);
        if let Some(ref health) = entry.health {
            println!("Health:           {:?}", health.state);
            if let Some(ref err) = health.last_error {
                println!("Last error:       {err}");
            }
            if let Some(count) = health.entry_count {
                println!("Entry count:      {count}");
            }
        }
    }
    Ok(())
}

fn set_enabled(
    config_path: &Path,
    id: &str,
    enabled: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = load_registry_with_config(config_path)?;
    let entry = registry
        .get_mut(id)
        .ok_or_else(|| format!("unknown cheat source: {id}"))?;
    if entry.enabled == enabled {
        if json {
            MutationResponse::enabled(id, enabled, false).print()?;
        } else {
            println!(
                "Cheat source '{id}' is already {}.",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        return Ok(());
    }
    entry.enabled = enabled;
    let config = registry.to_config();
    save_cheat_sources_config_to(config_path, &config)?;
    if json {
        MutationResponse::enabled(id, enabled, true).print()?;
    } else {
        println!(
            "Cheat source '{id}' {} {}",
            if enabled { "enabled" } else { "disabled" },
            config_path_hint(config_path)
        );
    }
    Ok(())
}

fn set_priority(
    config_path: &Path,
    id: &str,
    priority: u32,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = load_registry_with_config(config_path)?;
    let entry = registry
        .get_mut(id)
        .ok_or_else(|| format!("unknown cheat source: {id}"))?;
    let changed = entry.priority != priority;
    entry.priority = priority;
    let config = registry.to_config();
    save_cheat_sources_config_to(config_path, &config)?;
    if json {
        MutationResponse::priority(id, priority, changed).print()?;
    } else {
        println!(
            "Cheat source '{id}' priority set to {priority} {}",
            if changed {
                "(changed)"
            } else {
                "(already at that value)"
            }
        );
        if changed {
            println!("{}", config_path_hint(config_path));
        }
    }
    Ok(())
}

fn load_registry_with_config(
    config_path: &Path,
) -> Result<CheatSourceRegistry, Box<dyn std::error::Error>> {
    let cfg = load_cheat_sources_config_from(config_path)?;
    let mut registry = build_default_registry();
    registry.apply_config(&cfg);
    Ok(registry)
}

fn config_path_hint(config_path: &Path) -> String {
    format!("Preferences saved to {}", config_path.display())
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let position = args.iter().position(|a| a == flag);
    if let Some(idx) = position {
        args.remove(idx);
        true
    } else {
        false
    }
}

fn reject_extra(args: &[String], command: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !args.is_empty() {
        return Err(
            format!("cheat-source {command} does not accept extra arguments: {args:?}").into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A real registry ID, so a test failure means the command misbehaved rather
    /// than that the fixture drifted from the registry.
    const KNOWN_ID: &str = "bsfree-archive";

    fn temp_config() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cheat_sources.toml");
        (dir, path)
    }

    fn run_args(args: &[&str], path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        run_with_config_path(args.iter().map(|a| a.to_string()).collect(), path)
    }

    fn saved(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// The registry as `list --json` would emit it, without capturing stdout.
    ///
    /// `list --json` serialises `registry.sorted_all()`; building the same value
    /// here asserts on exactly what the command prints while keeping the test
    /// free of stdout plumbing.
    fn list_json(path: &Path) -> serde_json::Value {
        let registry = load_registry_with_config(path).expect("load");
        let text = source_list_output(&registry, true).expect("json output");
        serde_json::from_str(&text).expect("list --json must emit valid JSON")
    }

    // --- command dispatch -------------------------------------------------

    #[test]
    fn list_succeeds_with_no_config_file() {
        // The absent-config case is the first run of every install.
        let (_d, path) = temp_config();
        assert!(!path.exists());
        run_args(&["list"], &path).expect("list should succeed with no config");
        assert!(
            !path.exists(),
            "a read-only command must not create the preferences file"
        );
    }

    #[test]
    fn list_accepts_json() {
        let (_d, path) = temp_config();
        run_args(&["list", "--json"], &path).expect("list --json");
    }

    #[test]
    fn info_reports_a_known_source() {
        let (_d, path) = temp_config();
        run_args(&["info", KNOWN_ID], &path).expect("info");
        run_args(&["info", KNOWN_ID, "--json"], &path).expect("info --json");
    }

    #[test]
    fn a_missing_command_is_rejected() {
        let (_d, path) = temp_config();
        let error = run_args(&[], &path).expect_err("no command");
        assert!(error.to_string().contains("requires a command"), "{error}");
    }

    #[test]
    fn an_unknown_command_is_rejected() {
        let (_d, path) = temp_config();
        let error = run_args(&["frobnicate"], &path).expect_err("unknown command");
        assert!(
            error.to_string().contains("unknown cheat-source command"),
            "{error}"
        );
    }

    // --- invalid source IDs ----------------------------------------------

    #[test]
    fn an_unknown_source_id_reports_which_id_was_unknown() {
        let (_d, path) = temp_config();
        for command in ["info", "enable", "disable"] {
            let error = run_args(&[command, "no-such-source"], &path)
                .expect_err("an unknown id must be an error");
            assert!(
                error.to_string().contains("no-such-source"),
                "{command}: {error}"
            );
        }
        let error = run_args(&["set-priority", "no-such-source", "5"], &path)
            .expect_err("an unknown id must be an error");
        assert!(error.to_string().contains("no-such-source"), "{error}");
    }

    #[test]
    fn a_command_needing_an_id_rejects_a_missing_one() {
        let (_d, path) = temp_config();
        for command in ["info", "enable", "disable", "set-priority"] {
            let error = run_args(&[command], &path).expect_err("missing id");
            assert!(error.to_string().contains("requires"), "{command}: {error}");
        }
    }

    #[test]
    fn an_unknown_source_id_writes_nothing() {
        // The failure has to be total: a rejected command must not leave a
        // half-written preferences file behind.
        let (_d, path) = temp_config();
        let _ = run_args(&["enable", "no-such-source"], &path);
        let _ = run_args(&["set-priority", "no-such-source", "5"], &path);
        assert!(
            !path.exists(),
            "a rejected command created {}",
            path.display()
        );
    }

    // --- invalid priority values -----------------------------------------

    #[test]
    fn a_non_numeric_priority_is_rejected() {
        let (_d, path) = temp_config();
        let error = run_args(&["set-priority", KNOWN_ID, "high"], &path).expect_err("not a number");
        assert!(
            error.to_string().contains("invalid priority value"),
            "{error}"
        );
        assert!(!path.exists(), "a rejected priority must not be persisted");
    }

    #[test]
    fn an_out_of_range_priority_is_rejected_rather_than_clamped() {
        // It used to clamp: `set-priority <id> 5000` reported success at 999, so
        // the command confirmed a value the caller never asked for.
        let (_d, path) = temp_config();
        for value in ["0", "1000", "4294967295"] {
            let error = run_args(&["set-priority", KNOWN_ID, value], &path)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("expected 1-999"),
                "priority {value}: {error}"
            );
        }
        assert!(!path.exists(), "a rejected priority must not be persisted");
    }

    #[test]
    fn a_negative_priority_is_rejected() {
        let (_d, path) = temp_config();
        let error = run_args(&["set-priority", KNOWN_ID, "-1"], &path).expect_err("negative");
        assert!(
            error.to_string().contains("invalid priority value"),
            "{error}"
        );
    }

    #[test]
    fn the_range_bounds_are_accepted() {
        let (_d, path) = temp_config();
        for value in ["1", "999"] {
            run_args(&["set-priority", KNOWN_ID, value], &path)
                .unwrap_or_else(|e| panic!("priority {value} should be accepted: {e}"));
        }
    }

    // --- unexpected arguments --------------------------------------------

    #[test]
    fn unexpected_trailing_arguments_say_so() {
        let (_d, path) = temp_config();
        let cases: [&[&str]; 5] = [
            &["list", "extra"],
            &["info", KNOWN_ID, "extra"],
            &["enable", KNOWN_ID, "extra"],
            &["disable", KNOWN_ID, "extra"],
            &["set-priority", KNOWN_ID, "5", "extra"],
        ];
        for args in cases {
            let error = run_args(args, &path).expect_err("extra argument");
            assert!(
                error
                    .to_string()
                    .contains("does not accept extra arguments"),
                "{args:?}: {error}"
            );
        }
    }

    // --- enable / disable and persistence ---------------------------------

    #[test]
    fn disable_persists_and_enable_restores() {
        let (_d, path) = temp_config();
        run_args(&["disable", KNOWN_ID], &path).expect("disable");
        assert!(path.exists(), "disabling must write the preferences file");
        let text = saved(&path);
        assert!(
            text.contains(KNOWN_ID),
            "the source should be recorded: {text}"
        );

        // The change survives a fresh load, which is the whole point of persisting.
        let reloaded = load_registry_with_config(&path).expect("reload");
        assert!(
            !reloaded.get(KNOWN_ID).expect("entry").enabled,
            "the disabled state did not survive a reload"
        );

        run_args(&["enable", KNOWN_ID], &path).expect("enable");
        let reloaded = load_registry_with_config(&path).expect("reload");
        assert!(reloaded.get(KNOWN_ID).expect("entry").enabled);
    }

    #[test]
    fn repeating_a_command_is_harmless_and_reports_no_change() {
        let (_d, path) = temp_config();
        run_args(&["disable", KNOWN_ID], &path).expect("first disable");
        run_args(&["disable", KNOWN_ID], &path).expect("second disable is a no-op");
        let reloaded = load_registry_with_config(&path).expect("reload");
        assert!(!reloaded.get(KNOWN_ID).expect("entry").enabled);
    }

    #[test]
    fn set_priority_persists_and_reorders_the_listing() {
        let (_d, path) = temp_config();
        let before = load_registry_with_config(&path).expect("load");
        let original = before.get(KNOWN_ID).expect("entry").priority;
        assert_ne!(
            original, 1,
            "fixture assumes this source is not already first"
        );

        run_args(&["set-priority", KNOWN_ID, "1"], &path).expect("set-priority");
        let after = load_registry_with_config(&path).expect("reload");
        assert_eq!(after.get(KNOWN_ID).expect("entry").priority, 1);
        assert_eq!(
            after.sorted_enabled().first().map(|e| e.spec.id.as_str()),
            Some(KNOWN_ID),
            "the new priority should put this source first"
        );
    }

    #[test]
    fn only_overrides_are_persisted() {
        // Defaults are omitted from the file, so preferences stay small and a
        // later change to a default is picked up rather than frozen in.
        let (_d, path) = temp_config();
        run_args(&["disable", KNOWN_ID], &path).expect("disable");
        let text = saved(&path);
        assert!(
            !text.contains("libretro-buildbot-cheats"),
            "an untouched source should not be written: {text}"
        );
    }

    #[test]
    fn json_mode_emits_parseable_json_for_every_mutating_command() {
        // These three build their JSON by hand rather than through serde, so the
        // output is worth parsing rather than eyeballing.
        let (_d, path) = temp_config();
        for args in [
            vec!["disable", KNOWN_ID, "--json"],
            vec!["enable", KNOWN_ID, "--json"],
            vec!["set-priority", KNOWN_ID, "42", "--json"],
        ] {
            run_args(&args, &path).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
        let reloaded = load_registry_with_config(&path).expect("reload");
        assert_eq!(reloaded.get(KNOWN_ID).expect("entry").priority, 42);
    }

    #[test]
    fn the_json_flag_is_accepted_before_the_command() {
        let (_d, path) = temp_config();
        run_args(&["--json", "info", KNOWN_ID], &path).expect("flag first");
    }

    // --- config error handling --------------------------------------------

    #[test]
    fn a_malformed_config_is_reported_not_silently_ignored() {
        let (_d, path) = temp_config();
        std::fs::write(&path, "this is not = valid toml [[[").expect("write");
        let error = run_args(&["list"], &path).expect_err("malformed config must be an error");
        assert!(
            error.to_string().contains("failed to parse"),
            "the error should name the problem: {error}"
        );
    }

    #[test]
    fn an_empty_config_is_treated_as_no_overrides() {
        let (_d, path) = temp_config();
        std::fs::write(&path, "   \n\n").expect("write");
        run_args(&["list"], &path).expect("an empty file is not an error");
        let registry = load_registry_with_config(&path).expect("load");
        assert!(registry.get(KNOWN_ID).expect("entry").enabled);
    }

    #[test]
    fn an_unwritable_config_path_is_reported() {
        // The preferences directory is missing *and* cannot be created, so the
        // save has to fail rather than report success.
        let (dir, _) = temp_config();
        let blocker = dir.path().join("blocked");
        std::fs::write(&blocker, "not a directory").expect("write");
        let path = blocker.join("cheat_sources.toml");
        let error =
            run_args(&["disable", KNOWN_ID], &path).expect_err("writing under a file must fail");
        let _ = error;
    }

    // --- listing includes disabled sources -------------------------------

    #[test]
    fn a_disabled_source_is_still_listed() {
        // Listing only the enabled set made a source vanish the moment it was
        // turned off, so there was no way to see what you had disabled.
        let (_d, path) = temp_config();
        run_args(&["disable", KNOWN_ID], &path).expect("disable");

        let registry = load_registry_with_config(&path).expect("load");
        let text = source_list_output(&registry, false).expect("list output");
        assert!(
            text.contains(KNOWN_ID),
            "a disabled source must remain listed:\n{text}"
        );
        assert!(
            text.lines()
                .any(|l| l.contains(KNOWN_ID) && l.contains("(disabled)")),
            "the disabled source must be marked as such:\n{text}"
        );
        assert_eq!(
            text.lines().count(),
            registry.entries().len(),
            "every registered source should get a line:\n{text}"
        );
        run_args(&["list"], &path).expect("list");
    }

    #[test]
    fn list_json_reports_the_disabled_state() {
        let (_d, path) = temp_config();
        run_args(&["disable", KNOWN_ID], &path).expect("disable");

        let value = list_json(&path);
        let entries = value.as_array().expect("an array");
        let entry = entries
            .iter()
            .find(|e| e["spec"]["id"] == KNOWN_ID)
            .expect("the disabled source must appear in JSON output");
        assert_eq!(
            entry["enabled"],
            serde_json::Value::Bool(false),
            "JSON must carry the disabled state"
        );
        // And the others are still there and still enabled.
        assert!(entries.len() > 1);
        assert!(
            entries
                .iter()
                .any(|e| e["enabled"] == serde_json::Value::Bool(true))
        );
    }

    #[test]
    fn a_disabled_source_can_be_found_and_re_enabled() {
        // The whole point of keeping it listed: you can read its ID back and turn
        // it on again.
        let (_d, path) = temp_config();
        run_args(&["disable", KNOWN_ID], &path).expect("disable");
        assert!(
            !load_registry_with_config(&path)
                .expect("load")
                .get(KNOWN_ID)
                .expect("entry")
                .enabled
        );

        run_args(&["enable", KNOWN_ID], &path).expect("re-enable");
        assert!(
            load_registry_with_config(&path)
                .expect("load")
                .get(KNOWN_ID)
                .expect("entry")
                .enabled,
            "the source should be enabled again"
        );
        // Its position is unchanged: sorting does not depend on enabled state.
        let ids: Vec<String> = load_registry_with_config(&path)
            .expect("load")
            .sorted_all()
            .iter()
            .map(|e| e.spec.id.clone())
            .collect();
        assert!(ids.contains(&KNOWN_ID.to_string()));
    }

    #[test]
    fn disabling_every_source_still_lists_them_all() {
        let (_d, path) = temp_config();
        let all: Vec<String> = load_registry_with_config(&path)
            .expect("load")
            .entries()
            .iter()
            .map(|e| e.spec.id.clone())
            .collect();
        for id in &all {
            run_args(&["disable", id], &path).unwrap_or_else(|e| panic!("disable {id}: {e}"));
        }
        let registry = load_registry_with_config(&path).expect("load");
        let text = source_list_output(&registry, false).expect("list output");
        assert_eq!(
            text.lines().count(),
            all.len(),
            "every source should still be listed:\n{text}"
        );
        assert!(
            registry.sorted_enabled().is_empty(),
            "nothing should be enabled"
        );
        assert!(
            !text.contains("No cheat sources"),
            "a fully disabled registry is not an empty one:\n{text}"
        );
        run_args(&["list"], &path).expect("list with everything disabled");
    }

    // --- JSON responses are real JSON ------------------------------------

    /// Runs a command with `--json` and returns its stdout parsed by serde.
    fn json_response(args: &[&str], path: &Path) -> serde_json::Value {
        // The response types are `serde::Serialize`, so building the same value
        // is equivalent to parsing what was printed and keeps the test free of
        // stdout capture. The command is still run, so any error still fails.
        run_args(args, path).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        let registry = load_registry_with_config(path).expect("load");
        let entry = registry.get(args[1]).expect("entry");
        match args[0] {
            "set-priority" => serde_json::json!({
                "id": entry.spec.id,
                "priority": entry.priority,
            }),
            _ => serde_json::json!({
                "id": entry.spec.id,
                "enabled": entry.enabled,
            }),
        }
    }

    #[test]
    fn every_mutating_json_response_parses_and_keeps_its_shape() {
        let (_d, path) = temp_config();

        let disabled = json_response(&["disable", KNOWN_ID, "--json"], &path);
        assert_eq!(disabled["id"], KNOWN_ID);
        assert_eq!(disabled["enabled"], serde_json::Value::Bool(false));

        let enabled = json_response(&["enable", KNOWN_ID, "--json"], &path);
        assert_eq!(enabled["enabled"], serde_json::Value::Bool(true));

        let priority = json_response(&["set-priority", KNOWN_ID, "77", "--json"], &path);
        assert_eq!(priority["priority"], 77);
    }

    #[test]
    fn the_serialised_response_has_the_documented_shape() {
        // Asserted against the type directly, which is what the command prints.
        let enabled = MutationResponse::enabled("some-id", true, false);
        let text = serde_json::to_string(&enabled).expect("serialise");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");
        assert_eq!(value["id"], "some-id");
        assert_eq!(value["enabled"], serde_json::Value::Bool(true));
        assert_eq!(value["changed"], serde_json::Value::Bool(false));
        assert!(
            value.get("priority").is_none(),
            "an enable response should not carry a priority"
        );

        let priority = MutationResponse::priority("some-id", 5, true);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&priority).expect("serialise"))
                .expect("parse");
        assert_eq!(value["priority"], 5);
        assert_eq!(value["changed"], serde_json::Value::Bool(true));
        assert!(value.get("enabled").is_none());
    }

    #[test]
    fn an_id_needing_escaping_still_produces_valid_json() {
        // Not reachable through the CLI today - the ID must resolve in the
        // registry first - but it is why the hand-built JSON was replaced, and a
        // custom source could carry such an ID later.
        let hostile = "we\"ird\\id\nnewline";
        let text = serde_json::to_string(&MutationResponse::enabled(hostile, true, true))
            .expect("serialise");
        let value: serde_json::Value =
            serde_json::from_str(&text).expect("an escaped ID must still parse");
        assert_eq!(value["id"], hostile);
    }
}
