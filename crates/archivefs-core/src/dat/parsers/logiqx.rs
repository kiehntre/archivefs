//! Streaming Logiqx XML DAT file parser.
//!
//! Parses the standard XML format used by No-Intro and Redump DAT files.
//! Uses streaming (pull-based) XML parsing, so the *document* is never held in
//! memory at once - though the parsed model is, and it grows with the number of
//! entries.
//!
//! # Entities
//!
//! `quick-xml` with `default-features = false` performs no DTD processing: a
//! DOCTYPE arrives as inert text, no external DTD is fetched, and no declared
//! entity is ever expanded. A DOCTYPE is therefore accepted (every real
//! No-Intro and Redump DAT carries one) and recorded as a warning.
//!
//! Only the five predefined XML entities and numeric character references are
//! resolved. A reference to anything else - including an entity a DOCTYPE
//! purports to declare - cannot be resolved, and is reported as a warning
//! rather than silently dropping the text that contained it.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::escape::{resolve_predefined_entity, unescape};
use quick_xml::events::Event;

use super::super::hash::{normalise_crc32, normalise_md5, normalise_sha1, normalise_sha256};
use super::super::limits::DatLimits;
use super::super::model::{
    DatEcosystem, DatFormat, DatGameEntry, DatRomEntry, DatSource, ParsedDat,
};
use super::super::parser::{ParseError, ParseOutcome, ParseWarning};

pub fn parse_logiqx(path: &Path, limits: DatLimits) -> Result<ParseOutcome, ParseError> {
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

    let file = File::open(path).map_err(|error| ParseError::Io {
        path: path.to_path_buf(),
        error,
    })?;
    let reader = BufReader::with_capacity(64 * 1024, file);
    let mut xml_reader = Reader::from_reader(reader);

    let mut warnings: Vec<ParseWarning> = Vec::new();

    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut version: Option<String> = None;
    let mut author: Option<String> = None;
    let mut homepage: Option<String> = None;
    let mut clrmamepro_header: Option<String> = None;

    let mut games: Vec<DatGameEntry> = Vec::new();
    let mut current_game_name: Option<String> = None;
    let mut current_game_desc: Option<String> = None;
    let mut current_game_clone_of: Option<String> = None;
    let mut current_roms: Vec<DatRomEntry> = Vec::new();
    let mut current_rom_name: Option<String> = None;
    let mut current_rom_size: Option<u64> = None;
    let mut current_rom_crc: Option<String> = None;
    let mut current_rom_md5: Option<String> = None;
    let mut current_rom_sha1: Option<String> = None;
    let mut current_rom_sha256: Option<String> = None;
    let mut current_rom_status: Option<String> = None;
    let mut current_rom_merge: Option<String> = None;
    let mut current_rom_date: Option<String> = None;

    let mut text_buf = String::new();
    let mut depth: usize = 0;
    let mut in_game_element: bool = false;
    let mut buf = Vec::new();

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Decl(_decl)) => {
                // XML declaration is harmless; skip it.
            }
            Ok(Event::DocType(_)) => {
                // The Logiqx XML schema publishes a standard DOCTYPE. quick-xml
                // with default-features=false does not fetch external DTDs and
                // does not expand entities — the DOCTYPE is raw text only.
                // Accepting it as inert text is both safe and required: every
                // real-world No-Intro and Redump DAT file carries this DOCTYPE,
                // and rejecting it would mean supporting no DAT files at all.
                // This is expected parser behaviour, so it is a parser note, not
                // a warning: the DAT is fine and nothing needs to be done.
                record_note(
                    &mut warnings,
                    limits.max_warnings,
                    "doctype_ignored",
                    "DOCTYPE declaration accepted as inert text: external DTDs are \
                     intentionally never fetched and no entity is expanded, for security"
                        .to_string(),
                );
            }
            Ok(Event::Start(ref start_bytes)) => {
                depth += 1;
                if depth > limits.max_xml_depth {
                    return Err(ParseError::XmlDepthExceeded {
                        depth,
                        limit: limits.max_xml_depth,
                    });
                }

                let name_bytes = start_bytes.name();
                let tag = std::str::from_utf8(name_bytes.as_ref())
                    .map_err(|e| ParseError::MalformedXml {
                        detail: e.to_string(),
                        byte_offset: Some(xml_reader.buffer_position() as usize),
                    })?
                    .to_ascii_lowercase();

                match tag.as_str() {
                    "datafile" => {}
                    "game" | "machine" => {
                        in_game_element = true;
                        drop_current_game(
                            &mut current_game_name,
                            &mut current_game_desc,
                            &mut current_game_clone_of,
                            &mut current_roms,
                            &mut games,
                        );
                        if games.len() >= limits.max_entries {
                            return Err(ParseError::EntryLimitExceeded {
                                count: games.len(),
                                limit: limits.max_entries,
                            });
                        }
                        current_game_name = attr_str_checked(
                            start_bytes,
                            b"name",
                            limits.max_identifier_length,
                            &mut warnings,
                            limits.max_warnings,
                        )?;
                        // A `cloneof` attribute names the parent entry; when a
                        // catalogue uses the MAME-style `cloneofid` (a ROM name)
                        // instead, that is captured as the parent reference.
                        // Deliberately not length-checked beyond the identifier
                        // ceiling: a parent name is a label, and overlong values
                        // are carried as-is so nothing is dropped.
                        current_game_clone_of = attr_str_opt(
                            start_bytes,
                            b"cloneof",
                            &mut warnings,
                            limits.max_warnings,
                        )
                        .or_else(|| {
                            attr_str_opt(start_bytes, b"cloneofid", &mut warnings, limits.max_warnings)
                        });
                        current_game_desc = None;
                        current_roms = Vec::new();
                    }
                    "rom" => {
                        if current_roms.len() >= limits.max_roms_per_entry {
                            return Err(ParseError::RomsPerEntryExceeded {
                                game_name: current_game_name
                                    .clone()
                                    .unwrap_or_else(|| "<unnamed game>".to_string()),
                                count: current_roms.len(),
                                limit: limits.max_roms_per_entry,
                            });
                        }
                        current_rom_name = attr_str_checked(
                            start_bytes,
                            b"name",
                            limits.max_identifier_length,
                            &mut warnings,
                            limits.max_warnings,
                        )?;
                        current_rom_size =
                            attr_u64(start_bytes, b"size", &mut warnings, limits.max_warnings)?;
                        current_rom_crc = checksum_attr(
                            start_bytes,
                            b"crc",
                            normalise_crc32,
                            "a rom element",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_md5 = checksum_attr(
                            start_bytes,
                            b"md5",
                            normalise_md5,
                            "a rom element",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_sha1 = checksum_attr(
                            start_bytes,
                            b"sha1",
                            normalise_sha1,
                            "a rom element",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_sha256 = checksum_attr(
                            start_bytes,
                            b"sha256",
                            normalise_sha256,
                            "a rom element",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_status = attr_str_opt(
                            start_bytes,
                            b"status",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_merge =
                            attr_str_opt(start_bytes, b"merge", &mut warnings, limits.max_warnings);
                        current_rom_date =
                            attr_str_opt(start_bytes, b"date", &mut warnings, limits.max_warnings);
                    }
                    _ => {}
                }
                text_buf.clear();
            }
            Ok(Event::End(ref end_bytes)) => {
                if depth == 0 {
                    break;
                }
                depth -= 1;

                let name_bytes = end_bytes.name();
                let tag = std::str::from_utf8(name_bytes.as_ref())
                    .map_err(|e| ParseError::MalformedXml {
                        detail: e.to_string(),
                        byte_offset: Some(xml_reader.buffer_position() as usize),
                    })?
                    .to_ascii_lowercase();

                match tag.as_str() {
                    "name" if !in_game_element => {
                        name = Some(trimmed(&text_buf));
                    }
                    "description" if !in_game_element => {
                        let text = trimmed(&text_buf);
                        if text.len() > limits.max_description_length {
                            record_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "description_truncated",
                                format!(
                                    "description truncated from {} to {} bytes",
                                    text.len(),
                                    limits.max_description_length
                                ),
                            );
                            description =
                                Some(text.chars().take(limits.max_description_length).collect());
                        } else {
                            description = Some(text);
                        }
                    }
                    "version" if !in_game_element => {
                        version = Some(trimmed(&text_buf));
                    }
                    "author" if !in_game_element => {
                        author = Some(trimmed(&text_buf));
                    }
                    "homepage" if !in_game_element => {
                        homepage = Some(trimmed(&text_buf));
                    }
                    "clrmamepro" if !in_game_element => {
                        clrmamepro_header = Some(trimmed(&text_buf));
                    }
                    "description" => {
                        let text = trimmed(&text_buf);
                        if text.len() > limits.max_description_length {
                            record_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "game_description_truncated",
                                format!(
                                    "game description truncated at {} bytes",
                                    limits.max_description_length
                                ),
                            );
                            current_game_desc =
                                Some(text.chars().take(limits.max_description_length).collect());
                        } else if !text.is_empty() {
                            current_game_desc = Some(text);
                        }
                    }
                    "rom" => {
                        if let Some(rom_name) = current_rom_name.take() {
                            current_roms.push(DatRomEntry {
                                name: rom_name,
                                size_bytes: current_rom_size.take(),
                                crc32: current_rom_crc.take(),
                                md5: current_rom_md5.take(),
                                sha1: current_rom_sha1.take(),
                                sha256: current_rom_sha256.take(),
                                status: current_rom_status.take(),
                                merge: current_rom_merge.take(),
                                date: current_rom_date.take(),
                            });
                        }
                    }
                    "game" | "machine" => {
                        in_game_element = false;
                    }
                    _ => {}
                }
                text_buf.clear();
            }
            Ok(Event::Empty(ref empty_bytes)) => {
                let name_bytes = empty_bytes.name();
                let tag = std::str::from_utf8(name_bytes.as_ref())
                    .map_err(|e| ParseError::MalformedXml {
                        detail: e.to_string(),
                        byte_offset: Some(xml_reader.buffer_position() as usize),
                    })?
                    .to_ascii_lowercase();

                if tag == "rom" {
                    // Real Logiqx DATs write every ROM as a self-closing element,
                    // so this - not the Start/End pair below - is the path that
                    // actually needs the ceiling. Checked against the game being
                    // built whether or not it carries a name, because an unnamed
                    // game is exactly the case where an unbounded list would be
                    // built without anyone noticing.
                    if current_roms.len() >= limits.max_roms_per_entry {
                        return Err(ParseError::RomsPerEntryExceeded {
                            game_name: current_game_name
                                .clone()
                                .unwrap_or_else(|| "<unnamed game>".to_string()),
                            count: current_roms.len(),
                            limit: limits.max_roms_per_entry,
                        });
                    }
                    let rom_name = attr_str_checked(
                        empty_bytes,
                        b"name",
                        limits.max_identifier_length,
                        &mut warnings,
                        limits.max_warnings,
                    )?;
                    let rom_name = match rom_name {
                        Some(n) => n,
                        None => {
                            record_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "rom_missing_name",
                                "ROM element missing required name attribute".to_string(),
                            );
                            text_buf.clear();
                            buf.clear();
                            continue;
                        }
                    };
                    let size = attr_u64(empty_bytes, b"size", &mut warnings, limits.max_warnings)?;
                    let crc = checksum_attr(
                        empty_bytes,
                        b"crc",
                        normalise_crc32,
                        "a rom element",
                        &mut warnings,
                        limits.max_warnings,
                    );
                    let md5 = checksum_attr(
                        empty_bytes,
                        b"md5",
                        normalise_md5,
                        "a rom element",
                        &mut warnings,
                        limits.max_warnings,
                    );
                    let sha1 = checksum_attr(
                        empty_bytes,
                        b"sha1",
                        normalise_sha1,
                        "a rom element",
                        &mut warnings,
                        limits.max_warnings,
                    );
                    let sha256 = checksum_attr(
                        empty_bytes,
                        b"sha256",
                        normalise_sha256,
                        "a rom element",
                        &mut warnings,
                        limits.max_warnings,
                    );
                    let status =
                        attr_str_opt(empty_bytes, b"status", &mut warnings, limits.max_warnings);
                    let merge =
                        attr_str_opt(empty_bytes, b"merge", &mut warnings, limits.max_warnings);
                    let date =
                        attr_str_opt(empty_bytes, b"date", &mut warnings, limits.max_warnings);

                    current_roms.push(DatRomEntry {
                        name: rom_name,
                        size_bytes: size,
                        crc32: crc,
                        md5,
                        sha1,
                        sha256,
                        status,
                        merge,
                        date,
                    });
                }
                text_buf.clear();
            }
            Ok(Event::Text(ref text_bytes)) => {
                // quick-xml 0.41 split what `BytesText::unescape` used to do into
                // decoding the bytes and then resolving entity references. The
                // pair is used rather than `normalized_value`, which additionally
                // collapses tabs and newlines to spaces - that would rewrite a ROM
                // name rather than read it.
                match text_bytes.decode() {
                    Ok(decoded) => match unescape(&decoded) {
                        Ok(text) => text_buf.push_str(&text),
                        Err(error) => {
                            // Only a DTD could define whatever this references, and
                            // no DTD is processed. Keeping the raw text preserves
                            // the field; the warning is what stops the loss being
                            // silent.
                            record_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "entity_unresolved_text",
                                format!(
                                    "unresolvable entity reference in text kept as \
                                     written: {error}"
                                ),
                            );
                            text_buf.push_str(&decoded);
                        }
                    },
                    Err(error) => {
                        record_warning(
                            &mut warnings,
                            limits.max_warnings,
                            "text_invalid_utf8",
                            format!("text that is not valid UTF-8 was dropped: {error}"),
                        );
                    }
                }
            }
            Ok(Event::CData(ref cdata_bytes)) => {
                if let Ok(s) = std::str::from_utf8(cdata_bytes.as_ref()) {
                    text_buf.push_str(s);
                }
            }
            Ok(Event::GeneralRef(ref reference)) => {
                // New in quick-xml 0.41: entity and character references arrive as
                // their own event instead of being resolved inside `Text`. The
                // rules are the ones this parser already applied - the five
                // predefined entities and numeric character references resolve,
                // and anything a DTD would have to define does not, because no DTD
                // is ever processed.
                match reference.decode() {
                    Ok(name) => {
                        if let Ok(Some(character)) = reference.resolve_char_ref() {
                            text_buf.push(character);
                        } else if let Some(resolved) = resolve_predefined_entity(&name) {
                            text_buf.push_str(resolved);
                        } else {
                            record_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "entity_unrecognized",
                                format!(
                                    "unresolvable entity reference in text kept as \
                                     written: unrecognized entity `{name}`"
                                ),
                            );
                            text_buf.push('&');
                            text_buf.push_str(&name);
                            text_buf.push(';');
                        }
                    }
                    Err(error) => {
                        record_warning(
                            &mut warnings,
                            limits.max_warnings,
                            "reference_invalid_utf8",
                            format!("a reference that is not valid UTF-8 was dropped: {error}"),
                        );
                    }
                }
            }
            Ok(Event::Comment(_)) | Ok(Event::PI(_)) => {}
            Ok(Event::Eof) => {
                // quick-xml rejects a cut *inside* a tag, but a file cut cleanly
                // between elements simply ends with elements still open - which is
                // what a half-written or half-downloaded DAT looks like. The
                // entries recovered so far are real, so they are kept, but the
                // caller has to be told the catalogue is incomplete.
                if depth > 0 {
                    record_warning(
                        &mut warnings,
                        limits.max_warnings,
                        "document_truncated",
                        format!(
                            "document ended with {depth} element(s) still open: the DAT is \
                             truncated and these entries may be incomplete"
                        ),
                    );
                }
                break;
            }
            Err(error) => {
                return Err(ParseError::MalformedXml {
                    detail: error.to_string(),
                    byte_offset: Some(xml_reader.buffer_position() as usize),
                });
            }
        }
        buf.clear();
    }

    drop_current_game(
        &mut current_game_name,
        &mut current_game_desc,
        &mut current_game_clone_of,
        &mut current_roms,
        &mut games,
    );

    let ecosystem = detect_logiqx_ecosystem(&name, &author, &description);

    let source = DatSource {
        format: DatFormat::Logiqx,
        ecosystem,
        file_path: path.to_string_lossy().into_owned(),
        name,
        description,
        version,
        author,
        homepage,
        clrmamepro_header,
        entry_count: games.len(),
        rom_count: games.iter().map(|g| g.roms.len()).sum(),
        parse_warnings: warnings.iter().map(|w| w.to_string()).collect(),
    };

    Ok(ParseOutcome {
        dat: ParsedDat { source, games },
        warnings,
    })
}

/// Normalises one checksum attribute, reporting a malformed value.
///
/// A checksum that is not well-formed hex of the right length cannot be indexed,
/// so it is dropped - but dropping it silently is what makes a DAT with a typo
/// look like a DAT that simply publishes fewer algorithms. `dat validate` reports
/// hash coverage, and without this the missing coverage has no explanation.
fn checksum_attr(
    elem: &quick_xml::events::BytesStart<'_>,
    attr_name: &[u8],
    normalise: fn(&str) -> Option<String>,
    context: &str,
    warnings: &mut Vec<ParseWarning>,
    max_warnings: usize,
) -> Option<String> {
    let raw = attr_str_opt(elem, attr_name, warnings, max_warnings)?;
    match normalise(&raw) {
        Some(value) => Some(value),
        None => {
            record_warning(
                warnings,
                max_warnings,
                "checksum_dropped",
                format!(
                    "{} attribute on {context} is not a well-formed checksum and was dropped: {:?}",
                    String::from_utf8_lossy(attr_name),
                    raw.chars().take(32).collect::<String>()
                ),
            );
            None
        }
    }
}

fn record_warning(
    warnings: &mut Vec<ParseWarning>,
    limit: usize,
    code: &'static str,
    message: String,
) {
    if warnings.len() < limit {
        warnings.push(ParseWarning::with_code(code, message));
    }
}

/// Records a parser note: expected parser behaviour that needs no action.
fn record_note(
    warnings: &mut Vec<ParseWarning>,
    limit: usize,
    code: &'static str,
    message: String,
) {
    if warnings.len() < limit {
        warnings.push(ParseWarning::note(code, message));
    }
}

fn drop_current_game(
    name: &mut Option<String>,
    desc: &mut Option<String>,
    clone_of: &mut Option<String>,
    roms: &mut Vec<DatRomEntry>,
    games: &mut Vec<DatGameEntry>,
) {
    if let Some(game_name) = name.take() {
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
}

fn trimmed(text: &str) -> String {
    text.trim().to_string()
}

fn attr_str_checked(
    elem: &quick_xml::events::BytesStart<'_>,
    attr_name: &[u8],
    max_length: usize,
    warnings: &mut Vec<ParseWarning>,
    max_warnings: usize,
) -> Result<Option<String>, ParseError> {
    let value = attr_str_opt(elem, attr_name, warnings, max_warnings);
    if let Some(ref v) = value
        && v.len() > max_length
    {
        return Err(ParseError::IdentifierTooLong {
            field: String::from_utf8_lossy(attr_name).into_owned(),
            length: v.len(),
            limit: max_length,
            content_snippet: v.chars().take(60).collect(),
        });
    }
    Ok(value)
}

/// Reads one attribute, resolving XML entity references in its value.
///
/// `Attribute::value` is the *raw* escaped bytes. Real DAT files are full of
/// `&amp;` in game and ROM names ("Tom &amp; Jerry"), so using the raw value
/// stores a name that matches nothing and displays wrongly. `unescape_value`
/// resolves the predefined entities and numeric character references.
///
/// A value that cannot be unescaped - it references an entity only a DTD could
/// define - keeps its raw text rather than being dropped, so the name is still
/// present and still comparable, and the failure is recorded in `warnings`. Text
/// nodes are handled the same way, so the same content reports the same thing
/// whether it arrived as an attribute or as element text.
fn attr_str_opt(
    elem: &quick_xml::events::BytesStart<'_>,
    attr_name: &[u8],
    warnings: &mut Vec<ParseWarning>,
    max_warnings: usize,
) -> Option<String> {
    let attr = elem.try_get_attribute(attr_name).ok().flatten()?;
    // As for text nodes: decode, then resolve entities. `normalized_value` would
    // also apply XML attribute-value whitespace normalisation, turning a tab or a
    // newline inside a name into a space.
    //
    // The decode is checked rather than lossy. Replacing invalid bytes with U+FFFD
    // and carrying on would corrupt an identifier without a word - and because the
    // replacement happens before unescaping, the unescape then succeeds and there
    // is nothing left to notice. The old `unescape_value` failed on such input and
    // warned; that is preserved here, and it matches how text nodes are handled.
    let raw = match std::str::from_utf8(&attr.value) {
        Ok(text) => std::borrow::Cow::Borrowed(text),
        Err(error) => {
            record_warning(
                warnings,
                max_warnings,
                "attribute_invalid_utf8",
                format!(
                    "attribute {} is not valid UTF-8 and was read with replacement \
                     characters: {error}",
                    String::from_utf8_lossy(attr_name)
                ),
            );
            String::from_utf8_lossy(&attr.value)
        }
    };
    let value = match unescape(&raw) {
        Ok(decoded) => decoded.into_owned(),
        Err(error) => {
            record_warning(
                warnings,
                max_warnings,
                "entity_unresolved_attribute",
                format!(
                    "unresolvable entity reference in attribute {} kept as written: {error}",
                    String::from_utf8_lossy(attr_name)
                ),
            );
            raw.into_owned()
        }
    };
    if value.is_empty() {
        return None;
    }
    Some(value)
}

fn attr_u64(
    elem: &quick_xml::events::BytesStart<'_>,
    attr_name: &[u8],
    warnings: &mut Vec<ParseWarning>,
    max_warnings: usize,
) -> Result<Option<u64>, ParseError> {
    let Some(raw) = attr_str_opt(elem, attr_name, warnings, max_warnings) else {
        return Ok(None);
    };
    raw.parse::<u64>()
        .map(Some)
        .map_err(|_| ParseError::MalformedXml {
            detail: format!(
                "attribute {}={raw:?} is not a valid u64",
                String::from_utf8_lossy(attr_name)
            ),
            byte_offset: None,
        })
}

fn detect_logiqx_ecosystem(
    name: &Option<String>,
    author: &Option<String>,
    _description: &Option<String>,
) -> DatEcosystem {
    let name_lower = name.as_deref().unwrap_or("").to_ascii_lowercase();
    let author_lower = author.as_deref().unwrap_or("").to_ascii_lowercase();

    if name_lower.contains("no-intro") || author_lower.contains("no-intro") {
        return DatEcosystem::NoIntro;
    }
    if name_lower.contains("redump") || author_lower.contains("redump") {
        return DatEcosystem::Redump;
    }

    DatEcosystem::GenericLogiqx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(path_name: &str, content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(path_name);
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    fn parse_xml(content: &str) -> Result<ParseOutcome, ParseError> {
        let (_dir, path) = write_temp("test.dat", content);
        parse_logiqx(&path, DatLimits::default())
    }

    // ------------------------------------------------------------------
    // DOCTYPE: real-world No-Intro and Redump DATs carry it, so accept it.
    // ------------------------------------------------------------------

    #[test]
    fn doctype_is_accepted_and_dat_parses_correctly() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd">
<datafile>
    <header>
        <name>Test DAT</name>
    </header>
    <game name="Game One">
        <rom name="g1.bin" size="100" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 1);
        assert_eq!(outcome.dat.games[0].name, "Game One");
        assert!(
            outcome
                .dat
                .source
                .parse_warnings
                .iter()
                .any(|w| w.contains("DOCTYPE")),
            "DOCTYPE acceptance warning expected"
        );
    }

    // ------------------------------------------------------------------
    // Header metadata
    // ------------------------------------------------------------------

    #[test]
    fn header_metadata_is_extracted() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>No-Intro Nintendo 64</name>
        <description>Nintendo 64 (2025-01-01)</description>
        <version>2025-01-01</version>
        <author>No-Intro Team</author>
        <homepage>https://no-intro.org</homepage>
    </header>
    <game name="Sample Game">
        <rom name="sample.z64" size="8388608" crc="DEADBEEF"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        let s = &outcome.dat.source;
        assert_eq!(s.name.as_deref(), Some("No-Intro Nintendo 64"));
        assert_eq!(s.description.as_deref(), Some("Nintendo 64 (2025-01-01)"));
        assert_eq!(s.version.as_deref(), Some("2025-01-01"));
        assert_eq!(s.author.as_deref(), Some("No-Intro Team"));
        assert_eq!(s.homepage.as_deref(), Some("https://no-intro.org"));
    }

    // ------------------------------------------------------------------
    // Multiple games
    // ------------------------------------------------------------------

    #[test]
    fn multiple_games_are_parsed() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Game Alpha (USA)">
        <rom name="alpha.bin" size="100" crc="AAAAAAAA"/>
    </game>
    <game name="Game Beta (Japan)">
        <rom name="beta.bin" size="200" crc="BBBBBBBB"/>
    </game>
    <game name="Game Gamma (Europe)">
        <rom name="gamma.bin" size="300" crc="CCCCCCCC"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 3);
        assert_eq!(outcome.dat.games[0].name, "Game Alpha (USA)");
        assert_eq!(outcome.dat.games[1].name, "Game Beta (Japan)");
        assert_eq!(outcome.dat.games[2].name, "Game Gamma (Europe)");
    }

    // ------------------------------------------------------------------
    // Multiple ROMs per game
    // ------------------------------------------------------------------

    #[test]
    fn multiple_roms_per_game_are_parsed() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Multi-ROM Game">
        <rom name="program.rom" size="4096" crc="AAAAAAAA"/>
        <rom name="char.rom" size="2048" crc="BBBBBBBB"/>
        <rom name="sound.rom" size="1024" crc="CCCCCCCC"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 1);
        assert_eq!(outcome.dat.games[0].roms.len(), 3);
        assert_eq!(outcome.dat.games[0].roms[0].name, "program.rom");
        assert_eq!(outcome.dat.games[0].roms[1].name, "char.rom");
        assert_eq!(outcome.dat.games[0].roms[2].name, "sound.rom");
    }

    // ------------------------------------------------------------------
    // All four hash algorithms: CRC32, MD5, SHA-1, SHA-256
    // ------------------------------------------------------------------

    #[test]
    fn crc32_is_normalised() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="CRC Test">
        <rom name="crc.bin" size="1" crc="ABCD1234"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games[0].roms[0].crc32, Some("abcd1234".into()));
    }

    #[test]
    fn md5_is_normalised() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="MD5 Test">
        <rom name="md5.bin" size="1" md5="D41D8CD98F00B204E9800998ECF8427E"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(
            outcome.dat.games[0].roms[0].md5,
            Some("d41d8cd98f00b204e9800998ecf8427e".into())
        );
    }

    #[test]
    fn sha1_is_normalised() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="SHA1 Test">
        <rom name="sha1.bin" size="1" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(
            outcome.dat.games[0].roms[0].sha1,
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".into())
        );
    }

    #[test]
    fn sha256_is_normalised() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="SHA256 Test">
        <rom name="sha256.bin" size="1" sha256="E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(
            outcome.dat.games[0].roms[0].sha256,
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into())
        );
    }

    // ------------------------------------------------------------------
    // Parent/clone relationships
    // ------------------------------------------------------------------

    #[test]
    fn parent_clone_attributes_are_preserved() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Parent Game">
        <rom name="parent.bin" size="100" crc="AAAAAAAA"/>
    </game>
    <game name="Clone Game" cloneofid="parent.bin">
        <rom name="clone.bin" size="100" crc="BBBBBBBB"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 2);
        assert_eq!(outcome.dat.games[0].name, "Parent Game");
        assert_eq!(outcome.dat.games[1].name, "Clone Game");
        // The parent declaration is captured so the clone policy can act on
        // it; `cloneofid` (MAME-style, a ROM name) is used when `cloneof` is
        // absent.
        assert_eq!(outcome.dat.games[0].clone_of, None);
        assert_eq!(outcome.dat.games[1].clone_of.as_deref(), Some("parent.bin"));
    }

    #[test]
    fn a_cloneof_attribute_names_the_parent_entry() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Parent">
        <rom name="parent.bin" size="100" crc="AAAAAAAA"/>
    </game>
    <game name="Clone" cloneof="Parent">
        <rom name="clone.bin" size="100" crc="BBBBBBBB"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games[1].clone_of.as_deref(), Some("Parent"));
    }

    // ------------------------------------------------------------------
    // Unknown elements are silently ignored
    // ------------------------------------------------------------------

    #[test]
    fn unknown_elements_are_ignored() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Test</name>
        <unknown_header_field>ignored</unknown_header_field>
    </header>
    <game name="Game With Extras">
        <unknown_game_field>also ignored</unknown_game_field>
        <rom name="test.bin" size="100" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 1);
        assert_eq!(outcome.dat.games[0].roms.len(), 1);
        assert_eq!(outcome.dat.games[0].roms[0].crc32, Some("aaaaaaaa".into()));
    }

    // ------------------------------------------------------------------
    // Ecosystem detection: No-Intro
    // ------------------------------------------------------------------

    #[test]
    fn no_intro_ecosystem_detected_by_name() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>No-Intro Nintendo 64 (2025-01-01)</name>
    </header>
    <game name="Test">
        <rom name="test.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.source.ecosystem, DatEcosystem::NoIntro);
    }

    #[test]
    fn no_intro_ecosystem_detected_by_author() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Nintendo 64 Datfile</name>
        <author>No-Intro Team</author>
    </header>
    <game name="Test">
        <rom name="test.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.source.ecosystem, DatEcosystem::NoIntro);
    }

    // ------------------------------------------------------------------
    // Ecosystem detection: Redump
    // ------------------------------------------------------------------

    #[test]
    fn redump_ecosystem_detected_by_name() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Redump - Sony PlayStation 2</name>
    </header>
    <game name="Test Game (USA)">
        <rom name="test.iso" size="4700000000" crc="AAAAAAAA" md5="BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.source.ecosystem, DatEcosystem::Redump);
    }

    #[test]
    fn redump_disk_records() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Redump - Sega Saturn</name>
    </header>
    <game name="NiGHTS into Dreams... (USA)">
        <description>NiGHTS into Dreams...</description>
        <rom name="NiGHTS into Dreams... (USA) (Track 1).bin" size="47237760" crc="63BB9CA4" md5="afc3265164aaf59c1f26700586d79fd3" sha1="989f62a6457bd8c1f32b7bc60ceb6cdf307be855"/>
        <rom name="NiGHTS into Dreams... (USA) (Track 2).bin" size="41669520" crc="47B1CAAE" md5="956076a8b2d6b50d8a3a43bee65b67c5" sha1="e2d8d1567b9f53545d65a151bbdc7c54f0c8e2de"/>
        <rom name="NiGHTS into Dreams... (USA) (Track 3).bin" size="37867200" crc="D42B9132" md5="74ece34b77f75151dd1a1b6cba74ce16" sha1="ba151c6cb8f1e4c1e2e91b4f64dc2aed92a5a3e5"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.source.ecosystem, DatEcosystem::Redump);
        assert_eq!(outcome.dat.games.len(), 1);
        let game = &outcome.dat.games[0];
        assert_eq!(game.name, "NiGHTS into Dreams... (USA)");
        assert_eq!(game.description.as_deref(), Some("NiGHTS into Dreams..."));
        assert_eq!(game.roms.len(), 3);
        assert_eq!(game.roms[0].size_bytes, Some(47237760));
        assert_eq!(
            game.roms[0].sha1.as_deref(),
            Some("989f62a6457bd8c1f32b7bc60ceb6cdf307be855")
        );
        assert_eq!(game.roms[2].size_bytes, Some(37867200));
    }

    // ------------------------------------------------------------------
    // Generic Logiqx (no known ecosystem)
    // ------------------------------------------------------------------

    #[test]
    fn generic_logiqx_when_no_ecosystem_match() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>Custom DAT Collection</name>
        <author>Unknown Author</author>
    </header>
    <game name="Test">
        <rom name="test.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.source.ecosystem, DatEcosystem::GenericLogiqx);
    }

    // ------------------------------------------------------------------
    // Regression: depth-independent state tracking (the in_game_element fix)
    // ------------------------------------------------------------------

    #[test]
    fn header_metadata_works_with_or_without_header_element() {
        let no_header = r#"<?xml version="1.0"?>
<datafile>
    <name>Without Header</name>
    <game name="G">
        <rom name="g.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let o1 = parse_xml(no_header).unwrap();
        assert_eq!(o1.dat.source.name.as_deref(), Some("Without Header"));

        let with_header = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>With Header</name>
    </header>
    <game name="G">
        <rom name="g.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let o2 = parse_xml(with_header).unwrap();
        assert_eq!(o2.dat.source.name.as_deref(), Some("With Header"));
    }

    #[test]
    fn game_description_does_not_overwrite_header_description() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <header>
        <name>DAT Name</name>
        <description>This is the DAT description</description>
    </header>
    <game name="Game With Desc">
        <description>This is the game description</description>
        <rom name="g.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(
            outcome.dat.source.description.as_deref(),
            Some("This is the DAT description"),
            "DAT description must not be overwritten by game description"
        );
        assert_eq!(
            outcome.dat.games[0].description.as_deref(),
            Some("This is the game description"),
            "Game description must be captured separately"
        );
    }
}
