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

use super::super::classification::{DatContentClassification, DatOriginalMetadata};
use super::super::hash::{normalise_crc32, normalise_md5, normalise_sha1, normalise_sha256};
use super::super::limits::DatLimits;
use super::super::model::{
    DatBiosSetEntry, DatDataAreaEntry, DatDeviceRefEntry, DatDiskAreaEntry, DatDiskEntry,
    DatEcosystem, DatFormat, DatGameEntry, DatPartEntry, DatRomEntry, DatSampleEntry, DatSource,
    ParsedDat,
};
use super::super::parser::{ParseError, ParseOutcome, ParseWarning};
use super::super::trusted_dtd::{self, classify_doctype, describe_doctype_outcome};

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
    let mut current_game_id: Option<String> = None;
    let mut current_game_clone_of: Option<String> = None;
    let mut current_game_rom_of: Option<String> = None;
    let mut current_game_sample_of: Option<String> = None;
    let mut current_game_is_bios: Option<String> = None;
    let mut current_game_is_device: Option<String> = None;
    let mut current_game_runnable: Option<String> = None;
    let mut current_game_supported: Option<String> = None;
    let mut current_game_metadata = DatOriginalMetadata::default();
    // Provenance only, never interpreted - see `DatGameEntry::unsupported_structure`.
    let mut current_game_unsupported_structure: bool = false;
    let mut current_roms: Vec<DatRomEntry> = Vec::new();
    let mut current_disks: Vec<DatDiskEntry> = Vec::new();
    let mut current_device_refs: Vec<DatDeviceRefEntry> = Vec::new();
    let mut current_samples: Vec<DatSampleEntry> = Vec::new();
    let mut current_bios_sets: Vec<DatBiosSetEntry> = Vec::new();
    let mut current_parts: Vec<DatPartEntry> = Vec::new();
    let mut current_part: Option<DatPartEntry> = None;
    let mut current_part_depth: Option<usize> = None;
    let mut current_data_area: Option<DatDataAreaEntry> = None;
    let mut current_data_area_depth: Option<usize> = None;
    let mut current_disk_area: Option<DatDiskAreaEntry> = None;
    let mut current_disk_area_depth: Option<usize> = None;
    let mut current_disk: Option<DatDiskEntry> = None;
    let mut current_disk_depth: Option<usize> = None;
    let mut current_rom_count: usize = 0;
    let mut current_rom_name: Option<String> = None;
    let mut current_rom_size: Option<u64> = None;
    let mut current_rom_crc: Option<String> = None;
    let mut current_rom_md5: Option<String> = None;
    let mut current_rom_sha1: Option<String> = None;
    let mut current_rom_sha256: Option<String> = None;
    let mut current_rom_status: Option<String> = None;
    let mut current_rom_merge: Option<String> = None;
    let mut current_rom_date: Option<String> = None;
    let mut current_rom_offset: Option<String> = None;
    // Raw passthrough only - see `DatRomEntry::loadflag`.
    let mut current_rom_loadflag: Option<String> = None;
    let mut current_rom_value: Option<String> = None;
    let mut current_rom_optional: Option<String> = None;
    let mut current_rom_bios: Option<String> = None;
    let mut current_rom_region: Option<String> = None;

    let mut text_buf = String::new();
    let mut depth: usize = 0;
    let mut in_game_element: bool = false;
    let mut buf = Vec::new();

    loop {
        match xml_reader.read_event_into(&mut buf) {
            Ok(Event::Decl(_decl)) => {
                // XML declaration is harmless; skip it.
            }
            Ok(Event::DocType(ref doctype)) => {
                // The Logiqx XML schema publishes a standard DOCTYPE. quick-xml
                // with default-features=false does not fetch external DTDs and
                // does not expand entities — the DOCTYPE arrives as raw text
                // only, and still does here: `classify_doctype` never feeds
                // this back into the XML parser and never opens a resolved
                // DTD's *contents* - it only decides, from the declaration's
                // own external identifier, whether a trusted local copy is
                // available for provenance/diagnostics. Accepting the
                // declaration itself is both safe and required: every
                // real-world No-Intro and Redump DAT file carries one, and
                // rejecting it would mean supporting no DAT files at all.
                let outcome = classify_doctype(doctype.as_ref(), path);
                let code = match &outcome {
                    trusted_dtd::DoctypeOutcome::NoDoctype => "doctype_none",
                    trusted_dtd::DoctypeOutcome::TrustedDtdResolved { .. } => {
                        "trusted_dtd_resolved"
                    }
                    trusted_dtd::DoctypeOutcome::TrustedDtdUnavailable { .. } => {
                        "trusted_dtd_unavailable"
                    }
                    trusted_dtd::DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. } => {
                        "unsafe_or_unknown_doctype_ignored"
                    }
                };
                let message = describe_doctype_outcome(&outcome);
                // Expected, benign outcomes (the DTD is trusted by name,
                // whether or not a local copy happens to be available) stay
                // a parser note - nothing needs to be done. An unrecognised
                // or unsafe reference is a step further from the expected
                // shape every real-world DAT has, so it is a warning: still
                // entirely non-fatal, but worth a person's attention.
                if matches!(
                    outcome,
                    trusted_dtd::DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
                ) {
                    record_warning(&mut warnings, limits.max_warnings, code, message);
                } else {
                    record_note(&mut warnings, limits.max_warnings, code, message);
                }
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
                    "game" | "machine" | "software" => {
                        in_game_element = true;
                        finish_current_structure(
                            &mut current_disk,
                            &mut current_disk_area,
                            &mut current_data_area,
                            &mut current_part,
                            &mut current_disks,
                            &mut current_parts,
                        );
                        drop_current_game(
                            &mut current_game_name,
                            &mut current_game_id,
                            &mut current_game_desc,
                            &mut current_game_clone_of,
                            &mut current_game_rom_of,
                            &mut current_game_sample_of,
                            &mut current_game_is_bios,
                            &mut current_game_is_device,
                            &mut current_game_runnable,
                            &mut current_game_supported,
                            &mut current_game_metadata,
                            &mut current_game_unsupported_structure,
                            &mut current_roms,
                            &mut current_disks,
                            &mut current_device_refs,
                            &mut current_samples,
                            &mut current_bios_sets,
                            &mut current_parts,
                            &mut current_part,
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
                        // The DAT's own `id` for this entry, when it publishes
                        // one (No-Intro DATs always do). Preserved verbatim,
                        // never length-checked or required - this is a
                        // secondary identity a `cloneofid` reference resolves
                        // against, not the entry's primary key.
                        current_game_id =
                            attr_str_opt(start_bytes, b"id", &mut warnings, limits.max_warnings);
                        // A `cloneof` attribute names the parent entry; when a
                        // catalogue uses `cloneofid` instead (No-Intro: another
                        // entry's `id`, not a name), that raw value is captured
                        // as the parent reference just the same. Resolving it
                        // against the right index (name vs `id`) happens later,
                        // in `DependencyGraph::resolve_set`. Deliberately not
                        // length-checked beyond the identifier ceiling: a
                        // parent reference is a label, and overlong values are
                        // carried as-is so nothing is dropped.
                        current_game_clone_of = attr_str_opt(
                            start_bytes,
                            b"cloneof",
                            &mut warnings,
                            limits.max_warnings,
                        )
                        .or_else(|| {
                            attr_str_opt(
                                start_bytes,
                                b"cloneofid",
                                &mut warnings,
                                limits.max_warnings,
                            )
                        });
                        current_game_rom_of =
                            attr_str_opt(start_bytes, b"romof", &mut warnings, limits.max_warnings);
                        current_game_sample_of = attr_str_opt(
                            start_bytes,
                            b"sampleof",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_game_is_bios = attr_str_opt(
                            start_bytes,
                            b"isbios",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_game_is_device = attr_str_opt(
                            start_bytes,
                            b"isdevice",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_game_runnable = attr_str_opt(
                            start_bytes,
                            b"runnable",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_game_supported = if tag == "software" {
                            attr_str_opt(
                                start_bytes,
                                b"supported",
                                &mut warnings,
                                limits.max_warnings,
                            )
                        } else {
                            None
                        };
                        current_game_desc = None;
                        current_game_metadata = DatOriginalMetadata::default();
                        for (key, attribute) in [
                            ("category", b"category".as_slice()),
                            ("type", b"type".as_slice()),
                            ("content_type", b"content_type".as_slice()),
                            ("media", b"media".as_slice()),
                            ("release_type", b"release_type".as_slice()),
                            ("archive_devstatus", b"archive_devstatus".as_slice()),
                        ] {
                            if let Some(value) = attr_str_opt(
                                start_bytes,
                                attribute,
                                &mut warnings,
                                limits.max_warnings,
                            ) {
                                current_game_metadata.fields.insert(key.to_string(), value);
                            }
                        }
                        current_roms = Vec::new();
                        current_disks = Vec::new();
                        current_device_refs = Vec::new();
                        current_samples = Vec::new();
                        current_bios_sets = Vec::new();
                        current_parts = Vec::new();
                        current_part = None;
                        current_part_depth = None;
                        current_data_area = None;
                        current_data_area_depth = None;
                        current_disk_area = None;
                        current_disk_area_depth = None;
                        current_disk = None;
                        current_disk_depth = None;
                        current_rom_count = 0;
                    }
                    "rom" if in_game_element => {
                        if current_rom_count >= limits.max_roms_per_entry {
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
                        if current_rom_name.is_none() {
                            record_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "rom_missing_name",
                                "ROM element missing name; retained for structural classification"
                                    .to_string(),
                            );
                            current_rom_name = Some(String::new());
                        }
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
                        current_rom_offset = attr_str_opt(
                            start_bytes,
                            b"offset",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_loadflag = attr_str_opt(
                            start_bytes,
                            b"loadflag",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_value =
                            attr_str_opt(start_bytes, b"value", &mut warnings, limits.max_warnings);
                        current_rom_optional = attr_str_opt(
                            start_bytes,
                            b"optional",
                            &mut warnings,
                            limits.max_warnings,
                        );
                        current_rom_bios =
                            attr_str_opt(start_bytes, b"bios", &mut warnings, limits.max_warnings);
                        current_rom_region = attr_str_opt(
                            start_bytes,
                            b"region",
                            &mut warnings,
                            limits.max_warnings,
                        );
                    }
                    "disk" if in_game_element => {
                        if current_disk.is_some() {
                            current_game_unsupported_structure = true;
                            record_nested_state_warning(&mut warnings, limits.max_warnings, "disk");
                        } else {
                            current_disk = Some(parse_disk_entry(
                                start_bytes,
                                &mut warnings,
                                limits.max_warnings,
                            ));
                            current_disk_depth = Some(depth);
                        }
                    }
                    "device_ref" if in_game_element => {
                        current_device_refs.push(DatDeviceRefEntry {
                            name: attr_str_opt(
                                start_bytes,
                                b"name",
                                &mut warnings,
                                limits.max_warnings,
                            ),
                        });
                    }
                    "sample" if in_game_element => {
                        current_samples.push(DatSampleEntry {
                            name: attr_str_opt(
                                start_bytes,
                                b"name",
                                &mut warnings,
                                limits.max_warnings,
                            ),
                        });
                    }
                    "biosset" if in_game_element => {
                        current_bios_sets.push(parse_bios_set_entry(
                            start_bytes,
                            &mut warnings,
                            limits.max_warnings,
                        ));
                    }
                    "part" if in_game_element => {
                        if current_part.is_some() {
                            current_game_unsupported_structure = true;
                            record_nested_state_warning(&mut warnings, limits.max_warnings, "part");
                        } else {
                            current_part = Some(parse_part_entry(
                                start_bytes,
                                &mut warnings,
                                limits.max_warnings,
                            ));
                            current_part_depth = Some(depth);
                        }
                    }
                    "dataarea" if in_game_element => {
                        if current_data_area.is_some() {
                            current_game_unsupported_structure = true;
                            record_nested_state_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "dataarea",
                            );
                        } else if current_part.is_some() {
                            current_data_area = Some(DatDataAreaEntry {
                                name: attr_str_opt(
                                    start_bytes,
                                    b"name",
                                    &mut warnings,
                                    limits.max_warnings,
                                ),
                                roms: Vec::new(),
                            });
                            current_data_area_depth = Some(depth);
                        } else {
                            current_game_unsupported_structure = true;
                        }
                    }
                    "diskarea" if in_game_element => {
                        if current_disk_area.is_some() {
                            current_game_unsupported_structure = true;
                            record_nested_state_warning(
                                &mut warnings,
                                limits.max_warnings,
                                "diskarea",
                            );
                        } else if current_part.is_some() {
                            current_disk_area = Some(DatDiskAreaEntry {
                                name: attr_str_opt(
                                    start_bytes,
                                    b"name",
                                    &mut warnings,
                                    limits.max_warnings,
                                ),
                                disks: Vec::new(),
                            });
                            current_disk_area_depth = Some(depth);
                        } else {
                            current_game_unsupported_structure = true;
                        }
                    }
                    "device" if in_game_element => {
                        current_game_unsupported_structure = true;
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
                    "category" | "type" | "content_type" | "media" | "release_type"
                    | "archive_devstatus"
                        if in_game_element =>
                    {
                        let value = trimmed(&text_buf);
                        if !value.is_empty() {
                            current_game_metadata.fields.insert(tag, value);
                        }
                    }
                    "rom" => {
                        if let Some(rom_name) = current_rom_name.take() {
                            let rom = DatRomEntry {
                                name: rom_name,
                                size_bytes: current_rom_size.take(),
                                crc32: current_rom_crc.take(),
                                md5: current_rom_md5.take(),
                                sha1: current_rom_sha1.take(),
                                sha256: current_rom_sha256.take(),
                                status: current_rom_status.take(),
                                merge: current_rom_merge.take(),
                                date: current_rom_date.take(),
                                offset: current_rom_offset.take(),
                                loadflag: current_rom_loadflag.take(),
                                value: current_rom_value.take(),
                                optional: current_rom_optional.take(),
                                bios: current_rom_bios.take(),
                                region: current_rom_region.take(),
                            };
                            append_rom(rom, &mut current_data_area, &mut current_roms);
                            current_rom_count += 1;
                        }
                    }
                    "disk" => {
                        if current_disk_depth == Some(depth + 1) {
                            if let Some(disk) = current_disk.take() {
                                append_disk(disk, &mut current_disk_area, &mut current_disks);
                            }
                            current_disk_depth = None;
                        }
                    }
                    "dataarea" => {
                        if current_data_area_depth == Some(depth + 1) {
                            if let (Some(area), Some(part)) =
                                (current_data_area.take(), current_part.as_mut())
                            {
                                part.data_areas.push(area);
                            }
                            current_data_area_depth = None;
                        }
                    }
                    "diskarea" => {
                        if current_disk_area_depth == Some(depth + 1) {
                            if let (Some(area), Some(part)) =
                                (current_disk_area.take(), current_part.as_mut())
                            {
                                part.disk_areas.push(area);
                            }
                            current_disk_area_depth = None;
                        }
                    }
                    "part" => {
                        if current_part_depth == Some(depth + 1) {
                            if let Some(part) = current_part.take() {
                                current_parts.push(part);
                            }
                            current_part_depth = None;
                        }
                    }
                    "game" | "machine" | "software" => {
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

                if tag == "rom" && in_game_element {
                    // Real Logiqx DATs write every ROM as a self-closing element,
                    // so this - not the Start/End pair below - is the path that
                    // actually needs the ceiling. Checked against the game being
                    // built whether or not it carries a name, because an unnamed
                    // game is exactly the case where an unbounded list would be
                    // built without anyone noticing.
                    if current_rom_count >= limits.max_roms_per_entry {
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
                                "ROM element missing name; retained for structural classification"
                                    .to_string(),
                            );
                            String::new()
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
                    let offset =
                        attr_str_opt(empty_bytes, b"offset", &mut warnings, limits.max_warnings);
                    let loadflag =
                        attr_str_opt(empty_bytes, b"loadflag", &mut warnings, limits.max_warnings);
                    let value =
                        attr_str_opt(empty_bytes, b"value", &mut warnings, limits.max_warnings);
                    let optional =
                        attr_str_opt(empty_bytes, b"optional", &mut warnings, limits.max_warnings);
                    let bios =
                        attr_str_opt(empty_bytes, b"bios", &mut warnings, limits.max_warnings);
                    let region =
                        attr_str_opt(empty_bytes, b"region", &mut warnings, limits.max_warnings);

                    let rom = DatRomEntry {
                        name: rom_name,
                        size_bytes: size,
                        crc32: crc,
                        md5,
                        sha1,
                        sha256,
                        status,
                        merge,
                        date,
                        offset,
                        loadflag,
                        value,
                        optional,
                        bios,
                        region,
                    };
                    append_rom(rom, &mut current_data_area, &mut current_roms);
                    current_rom_count += 1;
                } else if in_game_element {
                    match tag.as_str() {
                        "disk" => {
                            if current_disk.is_some() {
                                current_game_unsupported_structure = true;
                                record_nested_state_warning(
                                    &mut warnings,
                                    limits.max_warnings,
                                    "disk",
                                );
                            } else {
                                let disk = parse_disk_entry(
                                    empty_bytes,
                                    &mut warnings,
                                    limits.max_warnings,
                                );
                                append_disk(disk, &mut current_disk_area, &mut current_disks);
                            }
                        }
                        "device_ref" => {
                            current_device_refs.push(DatDeviceRefEntry {
                                name: attr_str_opt(
                                    empty_bytes,
                                    b"name",
                                    &mut warnings,
                                    limits.max_warnings,
                                ),
                            });
                        }
                        "sample" => {
                            current_samples.push(DatSampleEntry {
                                name: attr_str_opt(
                                    empty_bytes,
                                    b"name",
                                    &mut warnings,
                                    limits.max_warnings,
                                ),
                            });
                        }
                        "biosset" => {
                            current_bios_sets.push(parse_bios_set_entry(
                                empty_bytes,
                                &mut warnings,
                                limits.max_warnings,
                            ));
                        }
                        "part" => {
                            if current_part.is_some() {
                                current_game_unsupported_structure = true;
                                record_nested_state_warning(
                                    &mut warnings,
                                    limits.max_warnings,
                                    "part",
                                );
                            } else {
                                current_parts.push(parse_part_entry(
                                    empty_bytes,
                                    &mut warnings,
                                    limits.max_warnings,
                                ));
                            }
                        }
                        "dataarea" => {
                            if current_data_area.is_some() {
                                current_game_unsupported_structure = true;
                                record_nested_state_warning(
                                    &mut warnings,
                                    limits.max_warnings,
                                    "dataarea",
                                );
                            } else if let Some(part) = current_part.as_mut() {
                                part.data_areas.push(DatDataAreaEntry {
                                    name: attr_str_opt(
                                        empty_bytes,
                                        b"name",
                                        &mut warnings,
                                        limits.max_warnings,
                                    ),
                                    roms: Vec::new(),
                                });
                            } else {
                                current_game_unsupported_structure = true;
                            }
                        }
                        "diskarea" => {
                            if current_disk_area.is_some() {
                                current_game_unsupported_structure = true;
                                record_nested_state_warning(
                                    &mut warnings,
                                    limits.max_warnings,
                                    "diskarea",
                                );
                            } else if let Some(part) = current_part.as_mut() {
                                part.disk_areas.push(DatDiskAreaEntry {
                                    name: attr_str_opt(
                                        empty_bytes,
                                        b"name",
                                        &mut warnings,
                                        limits.max_warnings,
                                    ),
                                    disks: Vec::new(),
                                });
                            } else {
                                current_game_unsupported_structure = true;
                            }
                        }
                        "device" => current_game_unsupported_structure = true,
                        _ => {}
                    }
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

    finish_current_structure(
        &mut current_disk,
        &mut current_disk_area,
        &mut current_data_area,
        &mut current_part,
        &mut current_disks,
        &mut current_parts,
    );
    drop_current_game(
        &mut current_game_name,
        &mut current_game_id,
        &mut current_game_desc,
        &mut current_game_clone_of,
        &mut current_game_rom_of,
        &mut current_game_sample_of,
        &mut current_game_is_bios,
        &mut current_game_is_device,
        &mut current_game_runnable,
        &mut current_game_supported,
        &mut current_game_metadata,
        &mut current_game_unsupported_structure,
        &mut current_roms,
        &mut current_disks,
        &mut current_device_refs,
        &mut current_samples,
        &mut current_bios_sets,
        &mut current_parts,
        &mut current_part,
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

fn parse_disk_entry(
    elem: &quick_xml::events::BytesStart<'_>,
    warnings: &mut Vec<ParseWarning>,
    max_warnings: usize,
) -> DatDiskEntry {
    let writable = attr_str_opt(elem, b"writable", warnings, max_warnings)
        .or_else(|| attr_str_opt(elem, b"writeable", warnings, max_warnings));
    DatDiskEntry {
        name: attr_str_opt(elem, b"name", warnings, max_warnings),
        sha1: checksum_attr(
            elem,
            b"sha1",
            normalise_sha1,
            "a disk element",
            warnings,
            max_warnings,
        ),
        merge: attr_str_opt(elem, b"merge", warnings, max_warnings),
        region: attr_str_opt(elem, b"region", warnings, max_warnings),
        index: attr_str_opt(elem, b"index", warnings, max_warnings),
        writable,
        status: attr_str_opt(elem, b"status", warnings, max_warnings),
        optional: attr_str_opt(elem, b"optional", warnings, max_warnings),
    }
}

fn parse_bios_set_entry(
    elem: &quick_xml::events::BytesStart<'_>,
    warnings: &mut Vec<ParseWarning>,
    max_warnings: usize,
) -> DatBiosSetEntry {
    DatBiosSetEntry {
        name: attr_str_opt(elem, b"name", warnings, max_warnings),
        description: attr_str_opt(elem, b"description", warnings, max_warnings),
        default: attr_str_opt(elem, b"default", warnings, max_warnings),
    }
}

fn parse_part_entry(
    elem: &quick_xml::events::BytesStart<'_>,
    warnings: &mut Vec<ParseWarning>,
    max_warnings: usize,
) -> DatPartEntry {
    DatPartEntry {
        name: attr_str_opt(elem, b"name", warnings, max_warnings),
        interface: attr_str_opt(elem, b"interface", warnings, max_warnings),
        data_areas: Vec::new(),
        disk_areas: Vec::new(),
    }
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

fn record_nested_state_warning(warnings: &mut Vec<ParseWarning>, limit: usize, element: &str) {
    record_warning(
        warnings,
        limit,
        "nested_state_overwrite_blocked",
        format!(
            "nested <{element}> start would overwrite an unclosed <{element}>; \
             the active structure was retained"
        ),
    );
}

fn append_rom(
    rom: DatRomEntry,
    current_data_area: &mut Option<DatDataAreaEntry>,
    game_roms: &mut Vec<DatRomEntry>,
) {
    if let Some(area) = current_data_area.as_mut() {
        area.roms.push(rom);
    } else {
        game_roms.push(rom);
    }
}

fn append_disk(
    disk: DatDiskEntry,
    current_disk_area: &mut Option<DatDiskAreaEntry>,
    game_disks: &mut Vec<DatDiskEntry>,
) {
    if let Some(area) = current_disk_area.as_mut() {
        area.disks.push(disk);
    } else {
        game_disks.push(disk);
    }
}

fn finish_current_structure(
    current_disk: &mut Option<DatDiskEntry>,
    current_disk_area: &mut Option<DatDiskAreaEntry>,
    current_data_area: &mut Option<DatDataAreaEntry>,
    current_part: &mut Option<DatPartEntry>,
    game_disks: &mut Vec<DatDiskEntry>,
    parts: &mut Vec<DatPartEntry>,
) {
    if let Some(disk) = current_disk.take() {
        append_disk(disk, current_disk_area, game_disks);
    }
    if let (Some(area), Some(part)) = (current_data_area.take(), current_part.as_mut()) {
        part.data_areas.push(area);
    }
    if let (Some(area), Some(part)) = (current_disk_area.take(), current_part.as_mut()) {
        part.disk_areas.push(area);
    }
    if let Some(part) = current_part.take() {
        parts.push(part);
    }
}

#[allow(clippy::too_many_arguments)]
fn drop_current_game(
    name: &mut Option<String>,
    id: &mut Option<String>,
    desc: &mut Option<String>,
    clone_of: &mut Option<String>,
    rom_of: &mut Option<String>,
    sample_of: &mut Option<String>,
    is_bios: &mut Option<String>,
    is_device: &mut Option<String>,
    runnable: &mut Option<String>,
    supported: &mut Option<String>,
    metadata: &mut DatOriginalMetadata,
    unsupported_structure: &mut bool,
    roms: &mut Vec<DatRomEntry>,
    disks: &mut Vec<DatDiskEntry>,
    device_refs: &mut Vec<DatDeviceRefEntry>,
    samples: &mut Vec<DatSampleEntry>,
    bios_sets: &mut Vec<DatBiosSetEntry>,
    parts: &mut Vec<DatPartEntry>,
    current_part: &mut Option<DatPartEntry>,
    games: &mut Vec<DatGameEntry>,
) {
    // Taken unconditionally, not just on the `Some(name)` path below: this is
    // what resets the flag for the *next* game regardless of whether the
    // just-finished one ever got a name.
    let had_unsupported_structure = std::mem::take(unsupported_structure);
    if let Some(part) = current_part.take() {
        parts.push(part);
    }
    if let Some(game_name) = name.take() {
        games.push(DatGameEntry {
            name: game_name,
            id: id.take(),
            description: desc.take(),
            roms: std::mem::take(roms),
            clone_of: clone_of.take(),
            rom_of: rom_of.take(),
            sample_of: sample_of.take(),
            is_bios: is_bios.take(),
            is_device: is_device.take(),
            runnable: runnable.take(),
            supported: supported.take(),
            disks: std::mem::take(disks),
            device_refs: std::mem::take(device_refs),
            samples: std::mem::take(samples),
            bios_sets: std::mem::take(bios_sets),
            parts: std::mem::take(parts),
            board: None,
            rebuild_to: None,
            year: None,
            manufacturer: None,
            source_file: None,
            comment: None,
            original_metadata: std::mem::take(metadata),
            content_classification: DatContentClassification::unknown(),
            unsupported_structure: had_unsupported_structure,
        });
    } else {
        rom_of.take();
        sample_of.take();
        is_bios.take();
        runnable.take();
        supported.take();
        roms.clear();
        disks.clear();
        device_refs.clear();
        samples.clear();
        bios_sets.clear();
        parts.clear();
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
    let parsed = if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
    } else {
        raw.parse::<u64>()
    };
    parsed.map(Some).map_err(|_| ParseError::MalformedXml {
        detail: format!(
            "attribute {}={raw:?} is not a valid decimal or 0x-prefixed u64",
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
                .any(|w| w.contains("Logiqx") && w.contains("DTD")),
            "a Logiqx DTD provenance diagnostic is expected"
        );
        assert!(
            !outcome
                .dat
                .source
                .parse_warnings
                .iter()
                .any(|w| w.to_lowercase().contains("validation passed")),
            "no diagnostic may claim DTD validation occurred"
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
        // it; `cloneofid` (No-Intro-style, another entry's `id`) is used when
        // `cloneof` is absent. Resolving that value against the right index
        // (name vs `id`) is `DependencyGraph::resolve_set`'s job, not this
        // parser's - see logiqx.rs's `id`-preservation tests below and
        // `dependency/graph.rs`.
        assert_eq!(outcome.dat.games[0].clone_of, None);
        assert_eq!(outcome.dat.games[1].clone_of.as_deref(), Some("parent.bin"));
    }

    #[test]
    fn a_game_id_attribute_is_preserved_exactly() {
        // No-Intro DATs assign every entry a stable id, independent of its
        // name, and reference it from a clone via `cloneofid`.
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Phantasy Star (World) (En) (Sega Ages)" id="0658">
        <rom name="ps.bin" size="100" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games[0].id.as_deref(), Some("0658"));
    }

    #[test]
    fn a_game_with_no_id_attribute_leaves_id_absent() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="No Id Here">
        <rom name="x.bin" size="1" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games[0].id, None);
    }

    #[test]
    fn a_cloneofid_reference_is_preserved_as_the_clone_reference_string() {
        // The parser's job is only to capture the literal reference; turning
        // it into a resolved entry (by id, not by name) is
        // `DependencyGraph::resolve_set`'s job.
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Phantasy Star (World) (En) (Sega Ages)" id="0658" cloneofid="0272">
        <rom name="ps.bin" size="100" crc="AAAAAAAA"/>
    </game>
    <game name="Phantasy Star (USA, Europe)" id="0272">
        <rom name="ps-parent.bin" size="100" crc="BBBBBBBB"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games[0].id.as_deref(), Some("0658"));
        assert_eq!(outcome.dat.games[0].clone_of.as_deref(), Some("0272"));
        assert_eq!(outcome.dat.games[1].id.as_deref(), Some("0272"));
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

    #[test]
    fn rom_size_accepts_decimal_and_prefixed_hexadecimal() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Mixed size formats">
        <rom name="decimal.bin" size="524288" crc="AAAAAAAA"/>
        <rom name="hex.bin" size="0x80000" crc="BBBBBBBB"/>
    </game>
</datafile>"#;

        let outcome = parse_xml(xml).unwrap();
        let roms = &outcome.dat.games[0].roms;

        assert_eq!(roms[0].size_bytes, Some(524_288));
        assert_eq!(roms[1].size_bytes, Some(524_288));
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
    // Set-completeness provenance: <disk> presence, loadflag passthrough.
    // Both are retained without interpretation so a consumer downstream
    // (dat::set) can refuse to reason about structure it does not support.
    // See DatGameEntry::unsupported_structure / DatRomEntry::loadflag.
    // ------------------------------------------------------------------

    #[test]
    fn self_closing_disk_element_is_fully_preserved() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Disc Game">
        <rom name="game.cue" size="4" crc="AAAAAAAA"/>
        <disk name="game (Track 1)" sha1="da39a3ee5e6b4b0d3255bfef95601890afd80709"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 1);
        assert!(!outcome.dat.games[0].unsupported_structure);
        // The <rom> itself must still parse normally alongside it.
        assert_eq!(outcome.dat.games[0].roms.len(), 1);
        assert_eq!(outcome.dat.games[0].roms[0].name, "game.cue");
    }

    #[test]
    fn start_end_disk_element_is_fully_preserved() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Disc Game">
        <disk name="game (Track 1)" sha1="da39a3ee5e6b4b0d3255bfef95601890afd80709"></disk>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 1);
        assert!(!outcome.dat.games[0].unsupported_structure);
    }

    #[test]
    fn game_without_disk_children_leaves_unsupported_structure_false() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Ordinary Game">
        <rom name="game.bin" size="4" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert!(!outcome.dat.games[0].unsupported_structure);
    }

    #[test]
    fn unsupported_structure_flag_does_not_leak_into_the_next_game() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="Unsupported Game">
        <device name="unrepresented"/>
    </game>
    <game name="Ordinary Game">
        <rom name="game.bin" size="4" crc="AAAAAAAA"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 2);
        assert!(outcome.dat.games[0].unsupported_structure);
        assert!(
            !outcome.dat.games[1].unsupported_structure,
            "unsupported_structure must reset per game, not stick from a previous one"
        );
    }

    #[test]
    fn only_unrepresented_structural_elements_set_unsupported_structure() {
        for (tag, expected) in [
            ("sample", false),
            ("part", false),
            ("dataarea", true),
            ("diskarea", true),
            ("device_ref", false),
            ("device", true),
            ("biosset", false),
        ] {
            let xml = format!(
                r#"<?xml version="1.0"?>
<datafile>
    <game name="G">
        <rom name="g.bin" size="4" crc="AAAAAAAA"/>
        <{tag} name="whatever"/>
    </game>
</datafile>"#
            );
            let outcome = parse_xml(&xml).unwrap();
            assert_eq!(
                outcome.dat.games[0].unsupported_structure, expected,
                "unexpected support capability for <{tag}/>"
            );
        }
    }

    #[test]
    fn a_genuinely_unrecognised_element_does_not_set_unsupported_structure() {
        // Contrast with the test above: an element name this parser has
        // never heard of (not one of the specific structural tags it
        // detects) must not be treated as unsupported structure - only the
        // named list is, everything else is ordinary "unknown, ignored" XML.
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="G">
        <rom name="g.bin" size="4" crc="AAAAAAAA"/>
        <totally_made_up_tag name="whatever"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert!(!outcome.dat.games[0].unsupported_structure);
    }

    #[test]
    fn self_closing_rom_loadflag_is_captured_verbatim() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="mame-set">
        <rom name="fill.bin" size="4" crc="AAAAAAAA" loadflag="fill"/>
        <rom name="cpu.bin" size="4" crc="BBBBBBBB"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        let roms = &outcome.dat.games[0].roms;
        assert_eq!(roms[0].loadflag.as_deref(), Some("fill"));
        assert_eq!(roms[1].loadflag, None);
    }

    #[test]
    fn unnamed_non_file_rom_is_retained_for_classification() {
        let xml = r#"<?xml version="1.0"?>
<softwarelist name="test">
    <software name="metadata-only">
        <part name="cart">
            <dataarea name="prg">
                <rom size="4" loadflag="fill" value="ff"/>
            </dataarea>
        </part>
    </software>
</softwarelist>"#;
        let outcome = parse_xml(xml).unwrap();
        let rom = &outcome.dat.games[0].parts[0].data_areas[0].roms[0];

        assert!(rom.name.is_empty());
        assert_eq!(rom.loadflag.as_deref(), Some("fill"));
        assert_eq!(rom.value.as_deref(), Some("ff"));
    }

    #[test]
    fn start_end_rom_loadflag_is_captured_verbatim() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="mame-set">
        <rom name="reload.bin" size="4" crc="AAAAAAAA" loadflag="reload"></rom>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(
            outcome.dat.games[0].roms[0].loadflag.as_deref(),
            Some("reload")
        );
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

    #[test]
    fn mame_machine_and_member_fidelity_is_preserved() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <machine name="clone" cloneof="parent" romof="bios" sampleof="samples" isbios="yes" runnable="no">
        <rom name="program.bin" size="4" crc="AAAAAAAA" offset="1000" loadflag="reload" value="ff" optional="yes" bios="us" region="maincpu"></rom>
        <disk name="disk0" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709" merge="parent.chd" region="cdrom" index="0" writable="no" status="baddump" optional="yes"></disk>
        <disk name="disk1" writeable="yes"/>
        <device_ref name="namco51"/>
        <sample name="shot"></sample>
        <biosset name="us" description="US BIOS" default="yes"/>
    </machine>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        let game = &outcome.dat.games[0];
        assert_eq!(game.clone_of.as_deref(), Some("parent"));
        assert_eq!(game.rom_of.as_deref(), Some("bios"));
        assert_eq!(game.sample_of.as_deref(), Some("samples"));
        assert_eq!(game.is_bios.as_deref(), Some("yes"));
        assert_eq!(game.runnable.as_deref(), Some("no"));

        let rom = &game.roms[0];
        assert_eq!(rom.offset.as_deref(), Some("1000"));
        assert_eq!(rom.loadflag.as_deref(), Some("reload"));
        assert_eq!(rom.value.as_deref(), Some("ff"));
        assert_eq!(rom.optional.as_deref(), Some("yes"));
        assert_eq!(rom.bios.as_deref(), Some("us"));
        assert_eq!(rom.region.as_deref(), Some("maincpu"));

        let disk = &game.disks[0];
        assert_eq!(disk.name.as_deref(), Some("disk0"));
        assert_eq!(
            disk.sha1.as_deref(),
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        assert_eq!(disk.merge.as_deref(), Some("parent.chd"));
        assert_eq!(disk.region.as_deref(), Some("cdrom"));
        assert_eq!(disk.index.as_deref(), Some("0"));
        assert_eq!(disk.writable.as_deref(), Some("no"));
        assert_eq!(disk.status.as_deref(), Some("baddump"));
        assert_eq!(disk.optional.as_deref(), Some("yes"));
        assert_eq!(game.disks[1].writable.as_deref(), Some("yes"));
        assert_eq!(game.device_refs[0].name.as_deref(), Some("namco51"));
        assert_eq!(game.samples[0].name.as_deref(), Some("shot"));
        assert_eq!(game.bios_sets[0].name.as_deref(), Some("us"));
        assert_eq!(game.bios_sets[0].description.as_deref(), Some("US BIOS"));
        assert_eq!(game.bios_sets[0].default.as_deref(), Some("yes"));
        assert!(!game.unsupported_structure);
    }

    #[test]
    fn software_part_area_members_retain_parent_linkage_and_game_scope() {
        let xml = r#"<?xml version="1.0"?>
<softwarelist name="test">
    <software name="structured">
        <part name="cart" interface="nes_cart">
            <dataarea name="prg">
                <rom name="program.bin" size="4" crc="AAAAAAAA"/>
                <rom name="program-2.bin" size="8" crc="BBBBBBBB"></rom>
            </dataarea>
            <dataarea name="gfx">
                <rom name="graphics.bin" size="16" crc="CCCCCCCC"/>
            </dataarea>
            <diskarea name="cdrom">
                <disk name="disc" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
                <disk name="disc-2" sha1="1111111111111111111111111111111111111111"></disk>
            </diskarea>
            <diskarea name="harddisk">
                <disk name="install" sha1="2222222222222222222222222222222222222222"/>
            </diskarea>
        </part>
    </software>
    <software name="ordinary">
        <rom name="ordinary.bin" size="4" crc="BBBBBBBB"/>
    </software>
</softwarelist>"#;
        let outcome = parse_xml(xml).unwrap();
        assert_eq!(outcome.dat.games.len(), 2);
        let structured = &outcome.dat.games[0];
        assert_eq!(structured.parts.len(), 1);
        assert_eq!(structured.parts[0].name.as_deref(), Some("cart"));
        assert_eq!(structured.parts[0].interface.as_deref(), Some("nes_cart"));
        assert_eq!(structured.parts[0].data_areas.len(), 2);
        assert_eq!(
            structured.parts[0].data_areas[0].name.as_deref(),
            Some("prg")
        );
        assert_eq!(structured.parts[0].data_areas[0].roms.len(), 2);
        assert_eq!(
            structured.parts[0].data_areas[0].roms[0].name,
            "program.bin"
        );
        assert_eq!(
            structured.parts[0].data_areas[0].roms[1].name,
            "program-2.bin"
        );
        assert_eq!(
            structured.parts[0].data_areas[1].name.as_deref(),
            Some("gfx")
        );
        assert_eq!(
            structured.parts[0].data_areas[1].roms[0].name,
            "graphics.bin"
        );
        assert_eq!(structured.parts[0].disk_areas.len(), 2);
        assert_eq!(
            structured.parts[0].disk_areas[0].name.as_deref(),
            Some("cdrom")
        );
        assert_eq!(structured.parts[0].disk_areas[0].disks.len(), 2);
        assert_eq!(
            structured.parts[0].disk_areas[0].disks[0].name.as_deref(),
            Some("disc")
        );
        assert_eq!(
            structured.parts[0].disk_areas[1].name.as_deref(),
            Some("harddisk")
        );
        assert_eq!(
            structured.parts[0].disk_areas[1].disks[0].name.as_deref(),
            Some("install")
        );
        assert!(structured.roms.is_empty());
        assert!(structured.disks.is_empty());
        assert!(!structured.unsupported_structure);

        let ordinary = &outcome.dat.games[1];
        assert_eq!(ordinary.roms[0].name, "ordinary.bin");
        assert!(ordinary.parts.is_empty());
        assert!(ordinary.disks.is_empty());
        assert!(ordinary.device_refs.is_empty());
        assert!(ordinary.samples.is_empty());
        assert!(ordinary.bios_sets.is_empty());
        assert_eq!(ordinary.rom_of, None);
        assert_eq!(ordinary.sample_of, None);
        assert_eq!(ordinary.is_bios, None);
        assert_eq!(ordinary.runnable, None);
        assert!(!ordinary.unsupported_structure);
    }

    #[test]
    fn software_supported_attribute_is_preserved_without_leakage() {
        let xml = r#"<?xml version="1.0"?>
<softwarelist name="test">
    <software name="yes" supported="yes"></software>
    <software name="partial" supported="partial"></software>
    <software name="no" supported="no"></software>
    <software name="absent"></software>
</softwarelist>"#;
        let outcome = parse_xml(xml).unwrap();

        assert_eq!(outcome.dat.games[0].supported.as_deref(), Some("yes"));
        assert_eq!(outcome.dat.games[1].supported.as_deref(), Some("partial"));
        assert_eq!(outcome.dat.games[2].supported.as_deref(), Some("no"));
        assert_eq!(outcome.dat.games[3].supported, None);
    }

    #[test]
    fn machine_level_roms_and_disks_stay_at_game_level() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <machine name="machine">
        <rom name="program.bin" size="4" crc="AAAAAAAA"/>
        <disk name="drive" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
    </machine>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        let machine = &outcome.dat.games[0];
        assert_eq!(machine.roms[0].name, "program.bin");
        assert_eq!(machine.disks[0].name.as_deref(), Some("drive"));
        assert!(machine.parts.is_empty());
    }

    #[test]
    fn malformed_nested_starts_preserve_active_structure_and_fail_closed() {
        let xml = r#"<?xml version="1.0"?>
<softwarelist name="test">
    <software name="malformed">
        <part name="outer">
            <part name="ignored-inner"></part>
            <dataarea name="outer-data">
                <rom name="before.bin" size="1" crc="AAAAAAAA"/>
                <dataarea name="ignored-inner-data">
                    <rom name="during.bin" size="2" crc="BBBBBBBB"/>
                </dataarea>
                <rom name="after.bin" size="3" crc="CCCCCCCC"/>
            </dataarea>
            <diskarea name="outer-disks">
                <disk name="outer-disk" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709">
                    <disk name="ignored-inner-disk" sha1="1111111111111111111111111111111111111111"></disk>
                </disk>
                <disk name="after-disk" sha1="2222222222222222222222222222222222222222"/>
                <diskarea name="ignored-inner-diskarea"></diskarea>
            </diskarea>
        </part>
    </software>
</softwarelist>"#;
        let outcome = parse_xml(xml).unwrap();
        let game = &outcome.dat.games[0];

        assert!(game.unsupported_structure);
        assert_eq!(game.parts.len(), 1);
        assert_eq!(game.parts[0].name.as_deref(), Some("outer"));
        assert_eq!(game.parts[0].data_areas.len(), 1);
        assert_eq!(
            game.parts[0].data_areas[0].name.as_deref(),
            Some("outer-data")
        );
        assert_eq!(
            game.parts[0].data_areas[0]
                .roms
                .iter()
                .map(|rom| rom.name.as_str())
                .collect::<Vec<_>>(),
            vec!["before.bin", "during.bin", "after.bin"]
        );
        assert_eq!(game.parts[0].disk_areas.len(), 1);
        assert_eq!(
            game.parts[0].disk_areas[0].name.as_deref(),
            Some("outer-disks")
        );
        assert_eq!(
            game.parts[0].disk_areas[0]
                .disks
                .iter()
                .filter_map(|disk| disk.name.as_deref())
                .collect::<Vec<_>>(),
            vec!["outer-disk", "after-disk"]
        );
        assert_eq!(
            outcome
                .warnings
                .iter()
                .filter(|warning| warning.code() == "nested_state_overwrite_blocked")
                .count(),
            4,
            "part, dataarea, diskarea, and paired disk overwrites must each be blocked"
        );
    }

    #[test]
    fn self_closing_structural_entries_are_preserved() {
        let xml = r#"<?xml version="1.0"?>
<datafile>
    <game name="self-closing">
        <disk name="disc" sha1="DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"/>
        <device_ref name="device"/>
        <sample name="sample"/>
        <biosset name="bios" description="BIOS" default="no"/>
        <part name="cart" interface="slot"/>
    </game>
</datafile>"#;
        let outcome = parse_xml(xml).unwrap();
        let game = &outcome.dat.games[0];
        assert_eq!(game.disks.len(), 1);
        assert_eq!(game.device_refs.len(), 1);
        assert_eq!(game.samples.len(), 1);
        assert_eq!(game.bios_sets.len(), 1);
        assert_eq!(game.parts.len(), 1);
    }
}
