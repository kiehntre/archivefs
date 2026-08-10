use std::path::PathBuf;

use archivefs_core::platform_artwork::{
    apply_import_folder, default_platform_artwork_root, import_platform_artwork,
    inspect_platform_artwork, preview_import_folder, remove_custom_platform_artwork,
};

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = args;
    let json = take_flag(&mut args, "--json");
    let root = take_value(&mut args, "--root")?
        .map(PathBuf::from)
        .unwrap_or(default_platform_artwork_root()?);
    let Some(command) = args.first().cloned() else {
        return Err("platform-artwork requires a command".into());
    };
    args.remove(0);
    match command.as_str() {
        "status" | "rescan" => {
            reject_extra(&args, &command)?;
            render(&inspect_platform_artwork(&root)?, json)?;
        }
        "import" => {
            let platform = take_value(&mut args, "--platform")?
                .ok_or("platform-artwork import requires --platform <canonical-id>")?;
            let source = take_value(&mut args, "--file")?
                .map(PathBuf::from)
                .ok_or("platform-artwork import requires --file <path>")?;
            let replace = take_flag(&mut args, "--replace");
            reject_extra(&args, "import")?;
            render(
                &import_platform_artwork(&root, &platform, &source, replace)?,
                json,
            )?;
        }
        "import-folder" => {
            if args.is_empty() {
                return Err("platform-artwork import-folder requires one folder path".into());
            }
            let source = PathBuf::from(args.remove(0));
            let dry_run = take_flag(&mut args, "--dry-run");
            let replace = take_flag(&mut args, "--replace");
            reject_extra(&args, "import-folder")?;
            let preview = preview_import_folder(&root, &source)?;
            if dry_run {
                render(&preview, json)?;
            } else {
                render(&apply_import_folder(&root, &preview, replace)?, json)?;
            }
        }
        "remove" => {
            let platform = take_value(&mut args, "--platform")?
                .ok_or("platform-artwork remove requires --platform <canonical-id>")?;
            let confirmed = take_flag(&mut args, "--confirm");
            reject_extra(&args, "remove")?;
            #[derive(Debug, serde::Serialize)]
            struct Removed {
                platform_id: String,
                removed: bool,
                managed_root: PathBuf,
            }
            render(
                &Removed {
                    platform_id: platform.clone(),
                    removed: remove_custom_platform_artwork(&root, &platform, confirmed)?,
                    managed_root: root,
                },
                json,
            )?;
        }
        "open-folder" => {
            reject_extra(&args, "open-folder")?;
            std::fs::create_dir_all(&root)?;
            let (program, argument) = if cfg!(target_os = "windows") {
                ("explorer", root.as_os_str())
            } else if cfg!(target_os = "macos") {
                ("open", root.as_os_str())
            } else {
                ("xdg-open", root.as_os_str())
            };
            let status = std::process::Command::new(program).arg(argument).status()?;
            if !status.success() {
                return Err(format!("{program} could not open {}", root.display()).into());
            }
            if json {
                println!("{}", serde_json::json!({ "path": root, "opened": true }));
            } else {
                println!("Opened {}", root.display());
            }
        }
        _ => return Err(format!("unknown platform-artwork command {command:?}").into()),
    }
    Ok(())
}

fn render<T: serde::Serialize + std::fmt::Debug>(
    value: &T,
    json: bool,
) -> Result<(), serde_json::Error> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{value:#?}");
    }
    Ok(())
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(index) = args.iter().position(|argument| argument == flag) {
        args.remove(index);
        true
    } else {
        false
    }
}

fn take_value(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    let Some(index) = args.iter().position(|argument| argument == flag) else {
        return Ok(None);
    };
    if index + 1 >= args.len() {
        return Err(format!("{flag} requires a value"));
    }
    args.remove(index);
    Ok(Some(args.remove(index)))
}

fn reject_extra(args: &[String], command: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "platform-artwork {command} does not accept {args:?}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_command_is_read_only_and_typed() {
        let root = std::env::temp_dir().join(format!("emuwiz-cli-artwork-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        run(vec![
            "status".into(),
            "--root".into(),
            root.display().to_string(),
            "--json".into(),
        ])
        .unwrap();
        assert!(!root.exists());
    }
}
