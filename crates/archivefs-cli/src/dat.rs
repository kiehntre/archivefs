//! `archivefs-cli dat <command>`.
//!
//! Stage 1A: read-only inspection, validation, and hash-based audit of DAT
//! catalogue files (Logiqx XML and ClrMamePro text).

use std::fmt::Write;
use std::path::PathBuf;

use archivefs_core::dat::audit::{KnownFileEvidence, audit_files};
use archivefs_core::dat::index::DatIndex;
use archivefs_core::dat::limits::DatLimits;
use archivefs_core::dat::parser::{ParseOutcome, ParseWarning};
use archivefs_core::dat::parsers::parse_dat_file;
use serde::Serialize;

#[derive(Serialize)]
struct InspectOutput {
    file_path: String,
    format: &'static str,
    ecosystem: &'static str,
    name: Option<String>,
    description: Option<String>,
    version: Option<String>,
    author: Option<String>,
    entry_count: usize,
    rom_count: usize,
    warnings: Vec<ParseWarning>,
    warning_summary: Vec<String>,
}

#[derive(Serialize)]
struct ValidateOutput {
    file_path: String,
    valid: bool,
    format: &'static str,
    ecosystem: &'static str,
    name: Option<String>,
    entry_count: usize,
    rom_count: usize,
    errors: Vec<String>,
    warnings: Vec<ParseWarning>,
}

pub fn run(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let Some(command) = args.first().cloned() else {
        return Err(
            "dat requires a sub-command: inspect <path> | validate <path> | audit <path> [--json] \
             [--file <path> ...]\n\
             \x20 --file compares the given name against the DAT. It does not open,\n\
             \x20 read or hash the file."
                .into(),
        );
    };
    let rest: Vec<String> = args[1..].to_vec();

    match command.as_str() {
        "inspect" => run_inspect(rest),
        "validate" => run_validate(rest),
        "audit" => run_audit(rest),
        _ => Err(format!(
            "unknown dat sub-command '{command}' (expected inspect, validate, or audit)"
        )
        .into()),
    }
}

fn run_inspect(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = extract_flag(&mut args, "--json");
    let path = take_first_path(&mut args, "dat inspect requires a DAT file path")?;
    reject_extra(&args, "dat inspect")?;

    let limits = DatLimits::default();
    let ParseOutcome { dat, warnings } = match parse_dat_file(&path, limits) {
        Ok(outcome) => outcome,
        Err(error) => return Err(error.to_string().into()),
    };

    if json {
        let output = InspectOutput {
            file_path: dat.source.file_path.clone(),
            format: dat.source.format.label(),
            ecosystem: dat.source.ecosystem.label(),
            name: dat.source.name.clone(),
            description: dat.source.description.clone(),
            version: dat.source.version.clone(),
            author: dat.source.author.clone(),
            entry_count: dat.source.entry_count,
            rom_count: dat.source.rom_count,
            warning_summary: dat.source.parse_warnings.clone(),
            warnings,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    let mut out = String::new();
    writeln!(&mut out, "DAT File: {}", dat.source.file_path).unwrap();
    writeln!(&mut out, "Format:   {}", dat.source.format.label()).unwrap();
    writeln!(&mut out, "Ecosystem: {}", dat.source.ecosystem.label()).unwrap();
    if let Some(ref n) = dat.source.name {
        writeln!(&mut out, "Name:     {n}").unwrap();
    }
    if let Some(ref d) = dat.source.description {
        writeln!(&mut out, "Description: {d}").unwrap();
    }
    if let Some(ref v) = dat.source.version {
        writeln!(&mut out, "Version:  {v}").unwrap();
    }
    if let Some(ref a) = dat.source.author {
        writeln!(&mut out, "Author:   {a}").unwrap();
    }
    writeln!(&mut out, "Entries:  {}", dat.source.entry_count).unwrap();
    writeln!(&mut out, "ROMs:     {}", dat.source.rom_count).unwrap();
    writeln!(&mut out, "Warnings: {}", dat.source.parse_warnings.len()).unwrap();
    for w in &dat.source.parse_warnings {
        writeln!(&mut out, "  - {w}").unwrap();
    }
    writeln!(&mut out).unwrap();

    // Print game summary.
    writeln!(&mut out, "Games:").unwrap();
    for game in &dat.games {
        writeln!(&mut out, "  {}", game.name).unwrap();
        for rom in &game.roms {
            let mut desc = String::new();
            if let Some(s) = rom.size_bytes {
                desc.push_str(&format!("  {s}B"));
            }
            let checksums = rom.checksums();
            if !checksums.is_empty() {
                if !desc.is_empty() {
                    desc.push_str(", ");
                }
                desc.push_str(
                    &checksums
                        .iter()
                        .map(|c| format!("{}: {}", c.algorithm.label(), c.value))
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
            writeln!(&mut out, "    {}  [{desc}]", rom.name).unwrap();
        }
    }

    print!("{out}");
    Ok(())
}

fn run_validate(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = extract_flag(&mut args, "--json");
    let path = take_first_path(&mut args, "dat validate requires a DAT file path")?;
    reject_extra(&args, "dat validate")?;

    let limits = DatLimits::default();
    let (dat, warnings, errors) = match parse_dat_file(&path, limits) {
        Ok(outcome) => {
            let errors = if outcome.dat.games.is_empty() && outcome.dat.source.rom_count == 0 {
                if outcome.dat.source.format.label() == "Logiqx XML" {
                    vec!["file parsed but contains no game entries".to_string()]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            (outcome.dat, outcome.warnings, errors)
        }
        Err(error) => {
            let errors = vec![error.to_string()];
            // Construct a minimal DatSource for error reporting
            let source = archivefs_core::dat::model::DatSource {
                format: archivefs_core::dat::model::DatFormat::ClrMamePro,
                ecosystem: archivefs_core::dat::model::DatEcosystem::GenericClrMamePro,
                file_path: path.to_string_lossy().into_owned(),
                name: None,
                description: None,
                version: None,
                author: None,
                homepage: None,
                clrmamepro_header: None,
                entry_count: 0,
                rom_count: 0,
                parse_warnings: vec!["file failed to parse".to_string()],
            };
            let dat = archivefs_core::dat::model::ParsedDat {
                source,
                games: Vec::new(),
            };
            (dat, Vec::new(), errors)
        }
    };

    if json {
        let output = ValidateOutput {
            file_path: dat.source.file_path.clone(),
            valid: errors.is_empty(),
            format: dat.source.format.label(),
            ecosystem: dat.source.ecosystem.label(),
            name: dat.source.name.clone(),
            entry_count: dat.source.entry_count,
            rom_count: dat.source.rom_count,
            errors,
            warnings,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        if !output.valid {
            return Err("dat validate: file failed validation".into());
        }
        return Ok(());
    }

    let mut out = String::new();
    writeln!(&mut out, "DAT File:  {}", dat.source.file_path).unwrap();
    writeln!(&mut out, "Format:    {}", dat.source.format.label()).unwrap();
    writeln!(&mut out, "Ecosystem:  {}", dat.source.ecosystem.label()).unwrap();
    if let Some(ref n) = dat.source.name {
        writeln!(&mut out, "Name:      {n}").unwrap();
    }
    writeln!(&mut out, "Entries:   {}", dat.source.entry_count).unwrap();
    writeln!(&mut out, "ROMs:      {}", dat.source.rom_count).unwrap();

    if errors.is_empty() {
        writeln!(&mut out, "Valid:     yes").unwrap();
    } else {
        writeln!(&mut out, "Valid:     no").unwrap();
        writeln!(&mut out, "Errors:").unwrap();
        for e in &errors {
            writeln!(&mut out, "  - {e}").unwrap();
        }
    }

    if !warnings.is_empty() {
        writeln!(&mut out, "Warnings:").unwrap();
        for w in &warnings {
            writeln!(&mut out, "  - {w}").unwrap();
        }
    }

    // Hash coverage summary
    if !dat.games.is_empty() {
        writeln!(&mut out).unwrap();
        let total_roms: usize = dat.games.iter().map(|g| g.roms.len()).sum();
        let with_crc = dat
            .games
            .iter()
            .flat_map(|g| &g.roms)
            .filter(|r| r.crc32.is_some())
            .count();
        let with_md5 = dat
            .games
            .iter()
            .flat_map(|g| &g.roms)
            .filter(|r| r.md5.is_some())
            .count();
        let with_sha1 = dat
            .games
            .iter()
            .flat_map(|g| &g.roms)
            .filter(|r| r.sha1.is_some())
            .count();
        let with_sha256 = dat
            .games
            .iter()
            .flat_map(|g| &g.roms)
            .filter(|r| r.sha256.is_some())
            .count();
        writeln!(&mut out, "Hash coverage ({total_roms} ROMs):").unwrap();
        writeln!(&mut out, "  CRC32:   {with_crc}").unwrap();
        writeln!(&mut out, "  MD5:     {with_md5}").unwrap();
        writeln!(&mut out, "  SHA-1:   {with_sha1}").unwrap();
        writeln!(&mut out, "  SHA-256: {with_sha256}").unwrap();
    }

    print!("{out}");

    if !errors.is_empty() {
        return Err("dat validate: file failed validation".into());
    }
    Ok(())
}

fn run_audit(mut args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let json = extract_flag(&mut args, "--json");
    let path = take_first_path(&mut args, "dat audit requires a DAT file path")?;

    // Collect --file arguments.
    //
    // Stage 1A audits *known hashes*, and the CLI has no source of hashes for an
    // arbitrary path: nothing here opens, stats or hashes the file named. So a
    // `--file` can only ever be compared on its name, and the output has to say
    // so - reporting a bare "Filename only -> Some Game" for a corrupt dump, or
    // for a path that does not exist, reads as though the file had been checked.
    let mut local_files: Vec<PathBuf> = Vec::new();
    while let Some(pos) = args.iter().position(|a| a == "--file") {
        if pos + 1 >= args.len() {
            return Err("--file requires a path".into());
        }
        let file_path = args.remove(pos + 1);
        args.remove(pos);
        local_files.push(PathBuf::from(file_path));
    }
    reject_extra(&args, "dat audit")?;

    let limits = DatLimits::default();
    let ParseOutcome {
        dat,
        warnings: _parse_warnings,
    } = parse_dat_file(&path, limits)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

    let index = DatIndex::build(&dat);

    let known: Vec<KnownFileEvidence> = if local_files.is_empty() {
        // Audit everything in the DAT against itself (sanity check).
        Vec::new()
    } else {
        local_files
            .iter()
            .map(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                KnownFileEvidence::new(p.to_string_lossy().into_owned(), name)
            })
            .collect()
    };

    let report = audit_files(&known, &index);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    let mut out = String::new();
    writeln!(&mut out, "DAT Audit: {}", dat.source.file_path).unwrap();
    writeln!(
        &mut out,
        "DAT: {} entries, {} ROMs ({} format, {} ecosystem)",
        dat.source.entry_count,
        dat.source.rom_count,
        dat.source.format.label(),
        dat.source.ecosystem.label()
    )
    .unwrap();
    if let Some(ref n) = dat.source.name {
        writeln!(&mut out, "DAT name: {n}").unwrap();
    }
    writeln!(&mut out).unwrap();

    // Index stats
    writeln!(
        &mut out,
        "Index: CRC32={} ({} collisions), MD5={} ({} collisions), SHA-1={} ({} collisions), SHA-256={} ({} collisions)",
        index.crc32_count(),
        index.crc32_collisions(),
        index.md5_count(),
        index.md5_collisions(),
        index.sha1_count(),
        index.sha1_collisions(),
        index.sha256_count(),
        index.sha256_collisions(),
    )
    .unwrap();
    writeln!(&mut out).unwrap();

    // Audit results
    let s = &report.summary;
    if !local_files.is_empty() {
        writeln!(
            &mut out,
            "Note: --file compares names only. No file is opened, read or hashed,\n\
             \x20     so a match here says a name is in the DAT - not that this file is."
        )
        .unwrap();
        writeln!(&mut out).unwrap();
    }
    writeln!(&mut out, "Audited: {} files (by name)", s.total).unwrap();
    writeln!(&mut out, "  Exact:       {}", s.exact).unwrap();
    writeln!(&mut out, "  Exact (mult): {}", s.exact_multiple).unwrap();
    writeln!(&mut out, "  Probable:    {}", s.probable).unwrap();
    writeln!(&mut out, "  Probable (mult): {}", s.probable_multiple).unwrap();
    writeln!(&mut out, "  Filename:    {}", s.filename_only).unwrap();
    writeln!(&mut out, "  Ambiguous:   {}", s.ambiguous).unwrap();
    writeln!(&mut out, "  Not in DAT:  {}", s.not_in_dat).unwrap();
    writeln!(&mut out, "  No evidence: {}", s.no_evidence).unwrap();

    if !report.entries.is_empty() {
        writeln!(&mut out).unwrap();
        writeln!(&mut out, "Details:").unwrap();
        for entry in &report.entries {
            let label = entry.verdict.label();
            let extra = match &entry.verdict {
                archivefs_core::dat::audit::AuditVerdict::Exact {
                    game_name,
                    algorithm,
                    ..
                } => {
                    format!(" -> {game_name} [{algorithm}]")
                }
                archivefs_core::dat::audit::AuditVerdict::ExactMultipleCandidates {
                    count, ..
                }
                | archivefs_core::dat::audit::AuditVerdict::ProbableMultipleCandidates {
                    count,
                    ..
                } => {
                    format!(" -> {count} candidates")
                }
                archivefs_core::dat::audit::AuditVerdict::Probable { game_name, .. } => {
                    format!(" -> {game_name}")
                }
                archivefs_core::dat::audit::AuditVerdict::FilenameOnly { game_name, .. } => {
                    format!(" -> {game_name}")
                }
                _ => String::new(),
            };
            writeln!(&mut out, "  [{label}] {}{extra}", entry.local_filename).unwrap();
        }
    }

    print!("{out}");
    Ok(())
}

fn extract_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let had = args.iter().any(|a| a == flag);
    args.retain(|a| a != flag);
    had
}

fn take_first_path(
    args: &mut Vec<String>,
    usage: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    if args.is_empty() {
        return Err(usage.into());
    }
    Ok(std::path::PathBuf::from(args.remove(0)))
}

fn reject_extra(args: &[String], command: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !args.is_empty() {
        return Err(format!("{command} does not accept {:?}", args).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_file(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn inspect_logiqx_detects_format() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header>
        <name>Test No-Intro DAT</name>
        <author>No-Intro</author>
    </header>
    <game name="Test Game">
        <rom name="test.bin" size="1024" crc="DEADBEEF"/>
    </game>
</datafile>"#;
        let (_dir, path) = write_temp_file("test.dat", xml);
        let args = vec!["inspect".into(), path.to_string_lossy().into_owned()];
        run(args).unwrap();
    }

    #[test]
    fn validate_success_on_valid_dat() {
        let content = "clrmamepro (\n\tname Test\n)\ngame (\n\tname \"Test Game\"\n\trom ( name test.bin size 1024 crc DEADBEEF )\n)\n";
        let (_dir, path) = write_temp_file("test.dat", content);
        let args = vec!["validate".into(), path.to_string_lossy().into_owned()];
        run(args).unwrap();
    }

    #[test]
    fn validate_errors_on_invalid_xml() {
        // Malformed XML (unclosed tag) should fail validation.
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Test">
        <rom name="test.bin" size="100" crc="AAAAAAAA"
    </game>
</datafile>"#;
        let (_dir, path) = write_temp_file("bad.dat", xml);
        let args = vec!["validate".into(), path.to_string_lossy().into_owned()];
        assert!(run(args).is_err());
    }

    #[test]
    fn audit_empty_files_succeeds() {
        let content = "clrmamepro (\n\tname Test\n)\ngame (\n\tname \"Test Game\"\n\trom ( name test.bin size 1024 crc DEADBEEF )\n)\n";
        let (_dir, path) = write_temp_file("test.dat", content);
        let args = vec!["audit".into(), path.to_string_lossy().into_owned()];
        run(args).unwrap();
    }

    #[test]
    fn inspect_empty_args_errors() {
        assert!(run(vec![]).is_err());
    }

    #[test]
    fn inspect_unknown_subcommand_errors() {
        assert!(run(vec!["unknown".into()]).is_err());
    }

    #[test]
    fn inspect_json_output() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header>
        <name>Test DAT</name>
    </header>
    <game name="Game One">
        <rom name="game1.bin" size="1024" crc="DEADBEEF"/>
    </game>
</datafile>"#;
        let (_dir, path) = write_temp_file("test.dat", xml);
        let args = vec![
            "inspect".into(),
            "--json".into(),
            path.to_string_lossy().into_owned(),
        ];
        run(args).unwrap();
    }

    #[test]
    fn validate_json_output_no_intro() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<datafile>
    <header>
        <name>No-Intro DAT</name>
        <author>No-Intro Team</author>
    </header>
    <game name="Super Game (World)">
        <rom name="super.bin" size="2048" crc="CAFEBABE" md5="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"/>
    </game>
</datafile>"#;
        let (_dir, path) = write_temp_file("test.dat", xml);
        let args = vec![
            "validate".into(),
            "--json".into(),
            path.to_string_lossy().into_owned(),
        ];
        run(args).unwrap();
    }

    #[test]
    fn doctype_in_logiqx_is_accepted_by_inspect() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN">
<datafile>
    <game name="Test">
        <rom name="test.bin" size="100" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let (_dir, path) = write_temp_file("doctype.dat", xml);
        let args = vec!["inspect".into(), path.to_string_lossy().into_owned()];
        assert!(run(args).is_ok());
    }

    #[test]
    fn audit_with_files() {
        let content = "clrmamepro (\n\tname Test\n)\ngame (\n\tname \"Test Game\"\n\trom ( name test.bin size 1024 crc DEADBEEF )\n)\n";
        let (_dir, path) = write_temp_file("test.dat", content);
        let args = vec![
            "audit".into(),
            path.to_string_lossy().into_owned(),
            "--file".into(),
            "/tmp/nonexistent.bin".into(),
        ];
        run(args).unwrap();
    }

    #[test]
    fn inspect_with_extra_args_rejected() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Test">
        <rom name="test.bin" size="100" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let (_dir, path) = write_temp_file("test.dat", xml);
        let args = vec![
            "inspect".into(),
            path.to_string_lossy().into_owned(),
            "extra".into(),
        ];
        assert!(run(args).is_err());
    }

    #[test]
    fn audit_json_output() {
        let content = "clrmamepro (\n\tname Test\n)\ngame (\n\tname \"Test Game\"\n\trom ( name test.bin size 1024 crc DEADBEEF )\n)\n";
        let (_dir, path) = write_temp_file("test.dat", content);
        let args = vec![
            "audit".into(),
            "--json".into(),
            path.to_string_lossy().into_owned(),
        ];
        run(args).unwrap();
    }
}
