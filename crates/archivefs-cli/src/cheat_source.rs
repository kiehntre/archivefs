use archivefs_core::patch_manager::{
    CheatSourceEntry, CheatSourceRegistry, build_default_registry,
    load_cheat_sources_config_default, save_cheat_sources_config_default,
};

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let json = take_flag(&mut args, "--json");
    let Some(command) = args.first().cloned() else {
        return Err("cheat-source requires a command".into());
    };
    args.remove(0);

    match command.as_str() {
        "list" => {
            reject_extra(&args, "list")?;
            render_source_list(json)?;
        }
        "info" => {
            if args.is_empty() {
                return Err("cheat-source info requires a source ID".into());
            }
            let id = args.remove(0);
            reject_extra(&args, "info")?;
            render_source_info(&id, json)?;
        }
        "enable" => {
            if args.is_empty() {
                return Err("cheat-source enable requires a source ID".into());
            }
            let id = args.remove(0);
            reject_extra(&args, "enable")?;
            set_enabled(&id, true, json)?;
        }
        "disable" => {
            if args.is_empty() {
                return Err("cheat-source disable requires a source ID".into());
            }
            let id = args.remove(0);
            reject_extra(&args, "disable")?;
            set_enabled(&id, false, json)?;
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
            set_priority(&id, priority, json)?;
        }
        _ => return Err(format!("unknown cheat-source command: {command}").into()),
    }
    Ok(())
}

fn render_source_list(json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let registry = load_registry_with_config()?;
    let entries = registry.sorted_enabled();
    if json {
        let output: Vec<&CheatSourceEntry> = entries;
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        if entries.is_empty() {
            println!("No cheat sources are enabled.");
            return Ok(());
        }
        let id_width = entries
            .iter()
            .map(|e| e.spec.id.len())
            .max()
            .unwrap_or(0)
            .max(2);
        for entry in &entries {
            let mark = if entry.enabled { " " } else { "!" };
            println!(
                "{mark} {:>3}  {:<id_width$}  {}",
                entry.priority, entry.spec.id, entry.spec.display_name,
            );
        }
    }
    Ok(())
}

fn render_source_info(id: &str, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let registry = load_registry_with_config()?;
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

fn set_enabled(id: &str, enabled: bool, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = load_registry_with_config()?;
    let entry = registry
        .get_mut(id)
        .ok_or_else(|| format!("unknown cheat source: {id}"))?;
    if entry.enabled == enabled {
        if json {
            println!(
                "{{\n  \"id\": \"{id}\",\n  \"enabled\": {enabled},\n  \"changed\": false\n}}"
            );
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
    save_cheat_sources_config_default(&config)?;
    if json {
        println!("{{\n  \"id\": \"{id}\",\n  \"enabled\": {enabled},\n  \"changed\": true\n}}");
    } else {
        println!(
            "Cheat source '{id}' {} {}",
            if enabled { "enabled" } else { "disabled" },
            config_path_hint()
        );
    }
    Ok(())
}

fn set_priority(id: &str, priority: u32, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let priority = priority.clamp(1, 999);
    let mut registry = load_registry_with_config()?;
    let entry = registry
        .get_mut(id)
        .ok_or_else(|| format!("unknown cheat source: {id}"))?;
    let changed = entry.priority != priority;
    entry.priority = priority;
    let config = registry.to_config();
    save_cheat_sources_config_default(&config)?;
    if json {
        println!(
            "{{\n  \"id\": \"{id}\",\n  \"priority\": {priority},\n  \"changed\": {changed}\n}}"
        );
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
            println!("{}", config_path_hint());
        }
    }
    Ok(())
}

fn load_registry_with_config() -> Result<CheatSourceRegistry, Box<dyn std::error::Error>> {
    let cfg = load_cheat_sources_config_default()?;
    let mut registry = build_default_registry();
    registry.apply_config(&cfg);
    Ok(registry)
}

fn config_path_hint() -> String {
    archivefs_core::patch_manager::default_cheat_sources_config_path()
        .map(|p| format!("Preferences saved to {}", p.display()))
        .unwrap_or_else(|_| "Preferences saved.".to_string())
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
