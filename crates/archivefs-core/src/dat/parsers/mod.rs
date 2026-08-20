//! DAT parser dispatch with backward-compatible format detection.
//!
//! A DAT file might be Logiqx XML or ClrMamePro text. This module sniffs the
//! first bytes of a file to decide which parser to use, then delegates.
//!
//! # Why this does not go through `safe_read`/`TrustedRoots`
//!
//! That policy exists to constrain paths EmuWiz derives from *data* - a RomM
//! record's `archivefs_path`, a cheat catalogue's destination - where a hostile
//! or careless source could otherwise steer a read outside the configured source
//! folders.
//!
//! A DAT path is not derived from anything: in Stage 1A it is typed on the
//! command line by the person running the command, and DAT files normally live
//! wherever they were downloaded rather than inside a source folder. Applying
//! trusted-root confinement here would refuse the ordinary case while protecting
//! against nothing the caller did not already choose.
//!
//! This is therefore a deliberate CLI exception, not an oversight. It stops being
//! one the moment a DAT path arrives from configuration, a manifest or any other
//! stored source - a later stage that feeds paths in that way must route them
//! through the same policy the rest of the codebase uses.

use std::path::Path;

use super::limits::DatLimits;
use super::model::DatFormat;
use super::parser::{ParseError, ParseOutcome};

pub mod clrmamepro;
pub mod logiqx;

use clrmamepro::parse_clrmamepro;
use logiqx::parse_logiqx;

/// Sniffs the given file path and parses it with the appropriate parser.
pub fn parse_dat_file(path: &Path, limits: DatLimits) -> Result<ParseOutcome, ParseError> {
    let metadata = std::fs::metadata(path).map_err(|error| ParseError::Io {
        path: path.to_path_buf(),
        error,
    })?;
    if !metadata.is_file() {
        return Err(ParseError::Io {
            path: path.to_path_buf(),
            error: std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
        });
    }
    let size = metadata.len();
    if size == 0 {
        // Empty file: try ClrMamePro (produces empty result), Logiqx would error.
        let mut outcome = parse_clrmamepro(path, limits)?;
        super::classification::classify_catalogue(&mut outcome.dat);
        return Ok(outcome);
    }
    if size > limits.max_file_size {
        return Err(ParseError::FileTooLarge {
            path: path.to_path_buf(),
            size,
            limit: limits.max_file_size,
        });
    }

    let detected = detect_format(path)?;
    let mut outcome = match detected {
        DatFormat::Logiqx => parse_logiqx(path, limits),
        DatFormat::ClrMamePro => parse_clrmamepro(path, limits),
    }?;
    super::classification::classify_catalogue(&mut outcome.dat);
    Ok(outcome)
}

/// Reads the first few KB of a file and decides its DAT format.
///
/// Logiqx XML files start with `<?xml` or `<datafile` or `<!DOCTYPE datafile`.
/// ClrMamePro files start with `clrmamepro (` after optional whitespace.
pub fn detect_format(path: &Path) -> Result<DatFormat, ParseError> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|error| ParseError::Io {
        path: path.to_path_buf(),
        error,
    })?;
    let mut buf = vec![0u8; 4096];
    let n = file.read(&mut buf).map_err(|error| ParseError::Io {
        path: path.to_path_buf(),
        error,
    })?;
    let mut head = &buf[..n];
    // A UTF-8 BOM (some real TOSEC DATs carry one) precedes the XML
    // declaration and is not Unicode whitespace, so `.trim()` alone never
    // removes it - left unstripped, sniffing below silently fails to
    // recognize `<?xml` and misclassifies a real Logiqx file as
    // ClrMamePro (which then parses zero games, no error surfaced).
    if let Some(without_bom) = head.strip_prefix(b"\xEF\xBB\xBF") {
        head = without_bom;
    }

    let trimmed = String::from_utf8_lossy(head).trim().to_ascii_lowercase();

    if trimmed.is_empty() {
        return Ok(DatFormat::ClrMamePro);
    }

    // XML detection: look for XML declaration, datafile root, or DOCTYPE
    if trimmed.starts_with("<?xml")
        || trimmed.starts_with("<datafile")
        || trimmed.starts_with("<!doctype")
    {
        return Ok(DatFormat::Logiqx);
    }

    // ClrMamePro detection
    if trimmed.starts_with("clrmamepro") {
        return Ok(DatFormat::ClrMamePro);
    }

    // Fallback: check if first non-whitespace char is '<'
    if trimmed.starts_with('<') {
        return Ok(DatFormat::Logiqx);
    }

    // Assume ClrMamePro for anything else
    Ok(DatFormat::ClrMamePro)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::classification::DatContentClass;

    #[test]
    fn detect_logiqx_by_xml_declaration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.dat");
        std::fs::write(&path, r#"<?xml version="1.0" encoding="UTF-8"?>"#).unwrap();
        assert_eq!(detect_format(&path).unwrap(), DatFormat::Logiqx);
    }

    #[test]
    fn detect_logiqx_by_doctype() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.dat");
        std::fs::write(
            &path,
            r#"<!DOCTYPE datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN">"#,
        )
        .unwrap();
        assert_eq!(detect_format(&path).unwrap(), DatFormat::Logiqx);
    }

    #[test]
    fn detect_clrmamepro_by_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.dat");
        std::fs::write(&path, "clrmamepro (\n\tname TOSEC\n)\n").unwrap();
        assert_eq!(detect_format(&path).unwrap(), DatFormat::ClrMamePro);
    }

    #[test]
    fn detect_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.dat");
        std::fs::write(&path, "").unwrap();
        assert_eq!(detect_format(&path).unwrap(), DatFormat::ClrMamePro);
    }

    #[test]
    fn sanitized_no_intro_shape_uses_structured_category_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-intro.dat");
        std::fs::write(
            &path,
            r#"<datafile><header><name>No-Intro Example</name></header><game name="Example" category="Games"><rom name="example.bin" size="4" sha1="a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"/></game></datafile>"#,
        )
        .unwrap();
        let outcome = parse_dat_file(&path, DatLimits::default()).unwrap();
        assert_eq!(
            outcome.dat.games[0].content_classification.class,
            DatContentClass::Game
        );
        assert_eq!(
            outcome.dat.games[0].original_metadata.fields["category"],
            "Games"
        );
    }

    #[test]
    fn sanitized_redump_shape_without_category_remains_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("redump.dat");
        std::fs::write(
            &path,
            r#"<datafile><header><name>Redump Example</name></header><game name="Retail Disc"><rom name="disc.bin" size="4" sha1="a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"/></game></datafile>"#,
        )
        .unwrap();
        let outcome = parse_dat_file(&path, DatLimits::default()).unwrap();
        assert_eq!(
            outcome.dat.games[0].content_classification.class,
            DatContentClass::Unknown
        );
    }

    #[test]
    fn a_utf8_bom_before_the_xml_declaration_still_detects_as_logiqx() {
        // Batch 8: some real TOSEC DATs carry a UTF-8 BOM before `<?xml`;
        // this must still be recognized as Logiqx, not silently
        // misclassified as ClrMamePro (which would then parse zero games
        // without ever surfacing an error).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bom.dat");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(
            br#"<?xml version="1.0" encoding="UTF-8"?><datafile><header><name>BOM Example</name></header><game name="Some Game"><rom name="game.bin" size="4" crc="00000000"/></game></datafile>"#,
        );
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(detect_format(&path).unwrap(), DatFormat::Logiqx);
        let outcome = parse_dat_file(&path, DatLimits::default()).unwrap();
        assert_eq!(outcome.dat.games.len(), 1);
        assert_eq!(outcome.dat.games[0].name, "Some Game");
    }

    #[test]
    fn no_bom_logiqx_detection_is_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-bom.dat");
        std::fs::write(
            &path,
            r#"<?xml version="1.0"?><datafile><header><name>No BOM</name></header></datafile>"#,
        )
        .unwrap();
        assert_eq!(detect_format(&path).unwrap(), DatFormat::Logiqx);
    }

    #[test]
    fn sanitized_tosec_shape_classifies_from_the_set_category() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tosec.dat");
        std::fs::write(
            &path,
            "clrmamepro (\n name \"TOSEC - Commodore Amiga - Games - ADF\"\n)\ngame (\n name \"Quest (Disk 1 of 2)\"\n rom ( name \"quest.adf\" size 4 crc 00000000 )\n)\n",
        )
        .unwrap();
        let outcome = parse_dat_file(&path, DatLimits::default()).unwrap();
        assert_eq!(
            outcome.dat.games[0].content_classification.class,
            DatContentClass::RequiredMultidiscPart
        );
    }
}
