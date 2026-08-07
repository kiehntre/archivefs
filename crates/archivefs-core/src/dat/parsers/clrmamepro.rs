//! ClrMamePro text format DAT file parser.
//!
//! Parses the line-oriented ClrMamePro format used by TOSEC and some other DAT
//! catalogues. The format uses `clrmamepro (...)` for header metadata and
//! `game (...)` / `rom (...)` blocks for entries.
//!
//! Two game-block styles are supported:
//!
//! * Single-line: `game ( name "Game Name" ... )`
//! * Multi-line:  `game (\n\tname "Game Name"\n\t...\n)`
//!
//! The same two styles apply to ROM blocks.

use std::fs;
use std::path::Path;

use super::super::hash::{normalise_crc32, normalise_md5, normalise_sha1, normalise_sha256};
use super::super::limits::DatLimits;
use super::super::model::{
    DatEcosystem, DatFormat, DatGameEntry, DatRomEntry, DatSource, ParsedDat,
};
use super::super::parser::{DiagnosticSeverity, ParseError, ParseOutcome, ParseWarning};

pub fn parse_clrmamepro(path: &Path, limits: DatLimits) -> Result<ParseOutcome, ParseError> {
    let metadata = std::fs::metadata(path).map_err(|error| ParseError::Io {
        path: path.to_path_buf(),
        error,
    })?;
    let size = metadata.len();
    if size > limits.max_file_size {
        return Err(ParseError::FileTooLarge {
            path: path.to_path_buf(),
            size,
            limit: limits.max_file_size,
        });
    }

    let content = fs::read_to_string(path).map_err(|error| ParseError::Io {
        path: path.to_path_buf(),
        error,
    })?;

    let lines: Vec<&str> = content.lines().collect();

    let mut warnings: Vec<ParseWarning> = Vec::new();
    let push_warning = |warnings: &mut Vec<ParseWarning>, offset: usize, msg: &str| {
        if warnings.len() < limits.max_warnings {
            let line_num = lines
                .iter()
                .enumerate()
                .rfind(|(_, l)| {
                    let line_start = l.as_ptr() as usize - content.as_ptr() as usize;
                    line_start <= offset
                })
                .map(|(i, _)| i + 1);
            warnings.push(ParseWarning {
                byte_offset: Some(offset),
                line: line_num,
                column: None,
                context: String::new(),
                message: msg.to_string(),
                severity: DiagnosticSeverity::Warning,
                code: "description_truncated",
            });
        }
    };

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut version: Option<String> = None;
    let mut author: Option<String> = None;
    let mut clrmamepro_header: Vec<String> = Vec::new();

    let mut games: Vec<DatGameEntry> = Vec::new();
    let mut in_clrmamepro = false;
    let mut in_game = false;
    let mut in_rom = false;
    let mut current_game_name: Option<String> = None;
    let mut current_game_desc: Option<String> = None;
    let mut current_game_clone_of: Option<String> = None;
    let mut current_rom_name: Option<String> = None;
    let mut current_rom_size: Option<u64> = None;
    let mut current_rom_crc: Option<String> = None;
    let mut current_rom_md5: Option<String> = None;
    let mut current_rom_sha1: Option<String> = None;
    let mut current_rom_sha256: Option<String> = None;
    let mut current_rom_status: Option<String> = None;
    let mut current_rom_merge: Option<String> = None;
    let mut current_rom_date: Option<String> = None;
    let mut current_roms: Vec<DatRomEntry> = Vec::new();

    for line in &lines {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        let offset = line.as_ptr() as usize - content.as_ptr() as usize;

        if trimmed_line == "clrmamepro (" {
            in_clrmamepro = true;
            continue;
        }
        if in_clrmamepro && trimmed_line == ")" {
            in_clrmamepro = false;
            continue;
        }
        if in_clrmamepro {
            clrmamepro_header.push(trimmed_line.to_string());
            parse_header_field(
                trimmed_line,
                &mut name,
                &mut description,
                &mut version,
                &mut author,
                &limits,
                offset,
                &mut warnings,
                &push_warning,
            );
            continue;
        }

        if trimmed_line.starts_with("game (") {
            let inner = extract_inner(trimmed_line, "game (");
            let is_closed = trimmed_line.ends_with(')')
                && trimmed_line.len() > "game ()".len()
                && inner.is_some();

            emit_rom_flush(
                &mut current_rom_name,
                &mut current_rom_size,
                &mut current_rom_crc,
                &mut current_rom_md5,
                &mut current_rom_sha1,
                &mut current_rom_sha256,
                &mut current_rom_status,
                &mut current_rom_merge,
                &mut current_rom_date,
                &mut current_roms,
                &mut in_rom,
            );
            emit_game(
                &mut current_game_name,
                &mut current_game_desc,
                &mut current_game_clone_of,
                &mut current_roms,
                &mut games,
                &limits,
            )?;

            current_game_name = None;
            current_game_desc = None;
            current_game_clone_of = None;
            current_roms = Vec::new();

            if let Some(inner) = inner {
                apply_kvs(inner, &mut |k, v| match k {
                    "name" => {
                        if v.len() <= limits.max_identifier_length {
                            current_game_name = Some(v.to_string());
                        }
                    }
                    "description" => {
                        current_game_desc = Some(v.to_string());
                    }
                    // `cloneof` / `romof` both declare a parent relationship;
                    // `romof` names the parent's ROM set, so either is a parent
                    // reference for the clone policy. `cloneof` wins when both
                    // are present.
                    "cloneof" | "romof" => {
                        capture_parent(&mut current_game_clone_of, k, v);
                    }
                    _ => {}
                });
            }

            if is_closed {
                emit_game(
                    &mut current_game_name,
                    &mut current_game_desc,
                    &mut current_game_clone_of,
                    &mut current_roms,
                    &mut games,
                    &limits,
                )?;
                current_game_name = None;
                current_game_desc = None;
                current_game_clone_of = None;
                current_roms = Vec::new();
                in_game = false;
            } else {
                in_game = true;
            }
            in_rom = false;
        } else if trimmed_line == ")" {
            if in_rom {
                emit_rom_flush(
                    &mut current_rom_name,
                    &mut current_rom_size,
                    &mut current_rom_crc,
                    &mut current_rom_md5,
                    &mut current_rom_sha1,
                    &mut current_rom_sha256,
                    &mut current_rom_status,
                    &mut current_rom_merge,
                    &mut current_rom_date,
                    &mut current_roms,
                    &mut in_rom,
                );
            }
            if in_game {
                emit_game(
                    &mut current_game_name,
                    &mut current_game_desc,
                    &mut current_game_clone_of,
                    &mut current_roms,
                    &mut games,
                    &limits,
                )?;
                current_game_name = None;
                current_game_desc = None;
                current_game_clone_of = None;
                current_roms = Vec::new();
                in_game = false;
            }
        } else if trimmed_line.starts_with("rom (") {
            let inner = extract_inner(trimmed_line, "rom (");
            let is_closed = trimmed_line.ends_with(')') && inner.is_some();

            emit_rom_flush(
                &mut current_rom_name,
                &mut current_rom_size,
                &mut current_rom_crc,
                &mut current_rom_md5,
                &mut current_rom_sha1,
                &mut current_rom_sha256,
                &mut current_rom_status,
                &mut current_rom_merge,
                &mut current_rom_date,
                &mut current_roms,
                &mut in_rom,
            );

            if let Some(ref game_name) = current_game_name
                && current_roms.len() >= limits.max_roms_per_entry
            {
                return Err(ParseError::RomsPerEntryExceeded {
                    game_name: game_name.clone(),
                    count: current_roms.len(),
                    limit: limits.max_roms_per_entry,
                });
            }

            current_rom_name = None;
            current_rom_size = None;
            current_rom_crc = None;
            current_rom_md5 = None;
            current_rom_sha1 = None;
            current_rom_sha256 = None;
            current_rom_status = None;
            current_rom_merge = None;
            current_rom_date = None;

            if let Some(inner) = inner {
                apply_kvs(inner, &mut |k, v| {
                    apply_rom_kv(
                        k,
                        v,
                        &mut current_rom_name,
                        &mut current_rom_size,
                        &mut current_rom_crc,
                        &mut current_rom_md5,
                        &mut current_rom_sha1,
                        &mut current_rom_sha256,
                        &mut current_rom_status,
                        &mut current_rom_merge,
                        &mut current_rom_date,
                    );
                });
            }

            if is_closed {
                // Need to signal we have a ROM to emit before the flush call.
                in_rom = true;
                emit_rom_flush(
                    &mut current_rom_name,
                    &mut current_rom_size,
                    &mut current_rom_crc,
                    &mut current_rom_md5,
                    &mut current_rom_sha1,
                    &mut current_rom_sha256,
                    &mut current_rom_status,
                    &mut current_rom_merge,
                    &mut current_rom_date,
                    &mut current_roms,
                    &mut in_rom,
                );
            } else {
                in_rom = true;
            }
        } else if in_rom {
            apply_kvs(trimmed_line, &mut |k, v| {
                apply_rom_kv(
                    k,
                    v,
                    &mut current_rom_name,
                    &mut current_rom_size,
                    &mut current_rom_crc,
                    &mut current_rom_md5,
                    &mut current_rom_sha1,
                    &mut current_rom_sha256,
                    &mut current_rom_status,
                    &mut current_rom_merge,
                    &mut current_rom_date,
                );
            });
        } else if in_game {
            if let Some(inner) = trimmed_line.strip_suffix(')') {
                // This `)` might be inline with game attributes
                apply_kvs(inner, &mut |k, v| {
                    if k == "name" {
                        current_game_name = Some(v.to_string());
                    } else if k == "description" {
                        current_game_desc = Some(v.to_string());
                    } else if k == "cloneof" || k == "romof" {
                        capture_parent(&mut current_game_clone_of, k, v);
                    }
                });
                in_game = false;
            } else {
                apply_kvs(trimmed_line, &mut |k, v| {
                    if k == "name" {
                        current_game_name = Some(v.to_string());
                    } else if k == "description" {
                        current_game_desc = Some(v.to_string());
                    } else if k == "cloneof" || k == "romof" {
                        capture_parent(&mut current_game_clone_of, k, v);
                    }
                });
            }
        }
    }

    emit_rom_flush(
        &mut current_rom_name,
        &mut current_rom_size,
        &mut current_rom_crc,
        &mut current_rom_md5,
        &mut current_rom_sha1,
        &mut current_rom_sha256,
        &mut current_rom_status,
        &mut current_rom_merge,
        &mut current_rom_date,
        &mut current_roms,
        &mut in_rom,
    );
    emit_game(
        &mut current_game_name,
        &mut current_game_desc,
        &mut current_game_clone_of,
        &mut current_roms,
        &mut games,
        &limits,
    )?;

    let ecosystem = detect_clrmamepro_ecosystem(&name, &description, &clrmamepro_header);

    let source = DatSource {
        format: DatFormat::ClrMamePro,
        ecosystem,
        file_path: path.to_string_lossy().into_owned(),
        name: name.clone(),
        description,
        version,
        author,
        homepage: None,
        clrmamepro_header: if clrmamepro_header.is_empty() {
            None
        } else {
            Some(clrmamepro_header.join("\n"))
        },
        entry_count: games.len(),
        rom_count: games.iter().map(|g| g.roms.len()).sum(),
        parse_warnings: warnings.iter().map(|w| w.to_string()).collect(),
    };

    Ok(ParseOutcome {
        dat: ParsedDat { source, games },
        warnings,
    })
}

/// Records a parent reference, with `cloneof` taking precedence over `romof`.
///
/// Both keys declare a parent relationship (a `romof` names the parent's ROM
/// set), so either is a parent reference for the clone policy; `cloneof` wins
/// when both are present on one game.
fn capture_parent(clone_of: &mut Option<String>, key: &str, value: &str) {
    if key == "cloneof" || clone_of.is_none() {
        *clone_of = Some(value.to_string());
    }
}

/// Extract the content inside `prefix(...)`, stripping the closing `)` if present.
fn extract_inner<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line[prefix.len()..].trim();
    if rest.is_empty() {
        return None;
    }
    if let Some(inner) = rest.strip_suffix(')') {
        let inner = inner.trim();
        if inner.is_empty() { None } else { Some(inner) }
    } else {
        Some(rest)
    }
}

/// Iterate over key-value pairs in a ClrMamePro attribute string.
/// Keys are alphabetic identifiers; values are either quoted strings or unquoted tokens.
fn apply_kvs(line: &str, cb: &mut dyn FnMut(&str, &str)) {
    let mut pos = 0;
    let bytes = line.as_bytes();

    while pos < bytes.len() {
        // Skip whitespace
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        // Read key.
        //
        // Alphanumeric, not alphabetic: every strong-hash key in this format ends
        // in a digit (`md5`, `sha1`, `sha256`). Stopping at the first digit split
        // `md5 <hash>` into the key `md` with the value `5`, left the hash itself
        // starting with a hex digit, and the next iteration discarded it as a
        // non-alphabetic token - so every MD5, SHA-1 and SHA-256 in a ClrMamePro
        // DAT was silently dropped while `crc` came through.
        let key_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_alphanumeric() {
            pos += 1;
        }
        if key_start == pos {
            // Non-alphabetic at start — skip this token
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            continue;
        }
        let key = &line[key_start..pos];

        // Skip whitespace after key
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        // Read value (quoted or unquoted)
        let value = if bytes[pos] == b'"' {
            pos += 1; // skip opening quote
            let val_start = pos;
            while pos < bytes.len() && bytes[pos] != b'"' {
                pos += 1;
            }
            let value = &line[val_start..pos];
            if pos < bytes.len() {
                pos += 1; // skip closing quote
            }
            value
        } else {
            let val_start = pos;
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b')' {
                pos += 1;
            }
            &line[val_start..pos]
        };

        cb(key, value);
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_rom_kv(
    key: &str,
    value: &str,
    name: &mut Option<String>,
    size: &mut Option<u64>,
    crc: &mut Option<String>,
    md5: &mut Option<String>,
    sha1: &mut Option<String>,
    sha256: &mut Option<String>,
    status: &mut Option<String>,
    merge: &mut Option<String>,
    date: &mut Option<String>,
) {
    match key {
        "name" => {
            if !value.is_empty() {
                *name = Some(value.to_string());
            }
        }
        "size" => {
            if let Ok(n) = value.parse::<u64>() {
                *size = Some(n);
            }
        }
        "crc" => {
            if let Some(n) = normalise_crc32(value) {
                *crc = Some(n);
            }
        }
        "md5" => {
            if let Some(n) = normalise_md5(value) {
                *md5 = Some(n);
            }
        }
        "sha1" => {
            if let Some(n) = normalise_sha1(value) {
                *sha1 = Some(n);
            }
        }
        "sha256" => {
            if let Some(n) = normalise_sha256(value) {
                *sha256 = Some(n);
            }
        }
        "status" => {
            *status = Some(value.to_string());
        }
        "merge" => {
            *merge = Some(value.to_string());
        }
        "date" => {
            *date = Some(value.to_string());
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_rom_flush(
    name: &mut Option<String>,
    size: &mut Option<u64>,
    crc: &mut Option<String>,
    md5: &mut Option<String>,
    sha1: &mut Option<String>,
    sha256: &mut Option<String>,
    status: &mut Option<String>,
    merge: &mut Option<String>,
    date: &mut Option<String>,
    roms: &mut Vec<DatRomEntry>,
    in_rom: &mut bool,
) {
    if !*in_rom {
        return;
    }
    if let Some(rom_name) = name.take() {
        roms.push(DatRomEntry {
            name: rom_name,
            size_bytes: size.take(),
            crc32: crc.take(),
            md5: md5.take(),
            sha1: sha1.take(),
            sha256: sha256.take(),
            status: status.take(),
            merge: merge.take(),
            date: date.take(),
        });
    }
    *in_rom = false;
}

fn emit_game(
    name: &mut Option<String>,
    desc: &mut Option<String>,
    clone_of: &mut Option<String>,
    roms: &mut Vec<DatRomEntry>,
    games: &mut Vec<DatGameEntry>,
    limits: &DatLimits,
) -> Result<(), ParseError> {
    if let Some(game_name) = name.take() {
        if games.len() >= limits.max_entries {
            return Err(ParseError::EntryLimitExceeded {
                count: games.len(),
                limit: limits.max_entries,
            });
        }
        games.push(DatGameEntry {
            name: game_name,
            description: desc.take(),
            roms: std::mem::take(roms),
            clone_of: clone_of.take(),
            sample_of: None,
            board: None,
            rebuild_to: None,
            year: None,
            manufacturer: None,
            source_file: None,
            comment: None,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn parse_header_field(
    line: &str,
    name: &mut Option<String>,
    description: &mut Option<String>,
    version: &mut Option<String>,
    author: &mut Option<String>,
    limits: &DatLimits,
    offset: usize,
    warnings: &mut Vec<ParseWarning>,
    push_warning: &dyn Fn(&mut Vec<ParseWarning>, usize, &str),
) {
    let lower = line.to_ascii_lowercase();
    if lower.starts_with("name ") {
        *name = Some(unquote(&line[5..]));
    } else if lower.starts_with("description ") {
        let text = unquote(&line[12..]);
        if text.len() > limits.max_description_length {
            push_warning(
                warnings,
                offset,
                &format!(
                    "description truncated from {} to {} bytes",
                    text.len(),
                    limits.max_description_length
                ),
            );
            *description = Some(text.chars().take(limits.max_description_length).collect());
        } else {
            *description = Some(text);
        }
    } else if lower.starts_with("version ") {
        *version = Some(unquote(&line[8..]));
    } else if lower.starts_with("author ") {
        *author = Some(unquote(&line[7..]));
    }
}

/// Trims a header value and removes one matched pair of surrounding quotes.
///
/// The game and ROM parsers already strip quotes via `apply_kvs`; the header
/// parser did not, so a header read back `"\"Commodore C64 - Games\""` while the
/// games in the same file read back cleanly. Ecosystem detection and every
/// display of the DAT's name inherited the stray quotes.
fn unquote(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed);
    unquoted.to_string()
}

fn detect_clrmamepro_ecosystem(
    name: &Option<String>,
    description: &Option<String>,
    header: &[String],
) -> DatEcosystem {
    let name_lower = name.as_deref().unwrap_or("").to_ascii_lowercase();
    let desc_lower = description.as_deref().unwrap_or("").to_ascii_lowercase();

    if name_lower.contains("tosec") || desc_lower.contains("tosec") {
        return DatEcosystem::Tosec;
    }

    let header_text = header.join("\n").to_ascii_lowercase();
    if header_text.contains("tosec") {
        return DatEcosystem::Tosec;
    }

    DatEcosystem::GenericClrMamePro
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_dat_produces_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.dat");
        std::fs::write(&path, "").unwrap();
        let limits = DatLimits::default();
        let result = parse_clrmamepro(&path, limits).unwrap();
        assert_eq!(result.dat.games.len(), 0);
    }

    #[test]
    fn parse_single_game_with_one_rom_multiline() {
        let content = concat!(
            "clrmamepro (\n",
            "\tname Test\n",
            ")\n",
            "game (\n",
            "\tname \"Test Game\"\n",
            "\tdescription \"A test\"\n",
            "\trom ( name test.bin size 1024 crc DEADBEEF )\n",
            ")\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.dat");
        std::fs::write(&path, content).unwrap();
        let limits = DatLimits::default();
        let result = parse_clrmamepro(&path, limits).unwrap();
        assert_eq!(result.dat.games.len(), 1);
        assert_eq!(result.dat.games[0].name, "Test Game");
        assert_eq!(result.dat.games[0].roms.len(), 1);
        assert_eq!(result.dat.games[0].roms[0].name, "test.bin");
        assert_eq!(result.dat.games[0].roms[0].size_bytes, Some(1024));
        assert_eq!(result.dat.games[0].roms[0].crc32, Some("deadbeef".into()));
    }

    #[test]
    fn tosec_ecosystem_detected_in_header() {
        let content = "clrmamepro (\n\tname TOSEC (2024-01-01)\n)\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tosec.dat");
        std::fs::write(&path, content).unwrap();
        let limits = DatLimits::default();
        let result = parse_clrmamepro(&path, limits).unwrap();
        assert_eq!(result.dat.source.ecosystem, DatEcosystem::Tosec);
    }

    #[test]
    fn multiple_roms_per_game() {
        let content = concat!(
            "clrmamepro (\n",
            "\tname Test\n",
            ")\n",
            "game (\n",
            "\tname \"Multi-ROM Game\"\n",
            "\trom ( name rom1.bin size 100 crc AAAAAAAA )\n",
            "\trom ( name rom2.bin size 200 crc BBBBBBBB )\n",
            "\trom ( name rom3.bin size 300 crc CCCCCCCC )\n",
            ")\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.dat");
        std::fs::write(&path, content).unwrap();
        let limits = DatLimits::default();
        let result = parse_clrmamepro(&path, limits).unwrap();
        assert_eq!(result.dat.games.len(), 1);
        assert_eq!(result.dat.games[0].roms.len(), 3);
    }

    #[test]
    fn cloneof_and_romof_declare_the_parent_entry() {
        let content = concat!(
            "clrmamepro (\n",
            "\tname Test\n",
            ")\n",
            "game (\n",
            "\tname \"Parent Game\"\n",
            "\trom ( name parent.bin size 100 crc AAAAAAAA )\n",
            ")\n",
            "game (\n",
            "\tname \"Clone Game\"\n",
            "\tcloneof \"Parent Game\"\n",
            "\trom ( name clone.bin size 100 crc BBBBBBBB )\n",
            ")\n",
            "game (\n",
            "\tname \"ROM Clone Game\"\n",
            "\tromof \"Parent Game\"\n",
            "\trom ( name romclone.bin size 100 crc CCCCCCCC )\n",
            ")\n"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clone.dat");
        std::fs::write(&path, content).unwrap();
        let limits = DatLimits::default();
        let result = parse_clrmamepro(&path, limits).unwrap();
        assert_eq!(result.dat.games.len(), 3);
        assert_eq!(result.dat.games[0].clone_of, None);
        assert_eq!(result.dat.games[1].clone_of.as_deref(), Some("Parent Game"));
        assert_eq!(result.dat.games[2].clone_of.as_deref(), Some("Parent Game"));
    }

    #[test]
    fn apply_kvs_single_line_rom() {
        let mut name = None;
        let mut size = None;
        let mut crc = None;
        let mut md5 = None;
        let mut sha1 = None;
        let mut sha256 = None;
        let mut status = None;
        let mut merge = None;
        let mut date = None;
        apply_kvs("name test.bin size 1024 crc DEADBEEF", &mut |k, v| {
            apply_rom_kv(
                k,
                v,
                &mut name,
                &mut size,
                &mut crc,
                &mut md5,
                &mut sha1,
                &mut sha256,
                &mut status,
                &mut merge,
                &mut date,
            );
        });
        assert_eq!(name, Some("test.bin".into()));
        assert_eq!(size, Some(1024));
        assert_eq!(crc, Some("deadbeef".into()));
    }

    #[test]
    fn apply_kvs_quoted_values() {
        let mut name = None;
        let mut size = None;
        let mut crc = None;
        let mut md5 = None;
        let mut sha1 = None;
        let mut sha256 = None;
        let mut status = None;
        let mut merge = None;
        let mut date = None;
        apply_kvs(
            "name \"Super Mario (World)\" size 4096 crc ABCD1234",
            &mut |k, v| {
                apply_rom_kv(
                    k,
                    v,
                    &mut name,
                    &mut size,
                    &mut crc,
                    &mut md5,
                    &mut sha1,
                    &mut sha256,
                    &mut status,
                    &mut merge,
                    &mut date,
                );
            },
        );
        assert_eq!(name, Some("Super Mario (World)".into()));
        assert_eq!(size, Some(4096));
        assert_eq!(crc, Some("abcd1234".into()));
    }
}
