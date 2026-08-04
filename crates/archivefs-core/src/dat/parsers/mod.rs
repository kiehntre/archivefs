//! DAT parser dispatch with backward-compatible format detection.
//!
//! A DAT file might be Logiqx XML or ClrMamePro text. This module sniffs the
//! first bytes of a file to decide which parser to use, then delegates.

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
        return parse_clrmamepro(path, limits);
    }
    if size > limits.max_file_size {
        return Err(ParseError::FileTooLarge {
            path: path.to_path_buf(),
            size,
            limit: limits.max_file_size,
        });
    }

    let detected = detect_format(path)?;
    match detected {
        DatFormat::Logiqx => parse_logiqx(path, limits),
        DatFormat::ClrMamePro => parse_clrmamepro(path, limits),
    }
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
    let head = &buf[..n];

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
}
