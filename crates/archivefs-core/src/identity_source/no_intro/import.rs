//! Local No-Intro DAT import: artifact provenance + reused hash index.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dat::index::DatIndex;
use crate::dat::limits::DatLimits;
use crate::dat::model::{DatEcosystem, ParsedDat};
use crate::dat::parser::ParseError;
use crate::dat::parsers::parse_dat_file;

/// Whether a DAT's own metadata says what representation its hashes
/// describe. No-Intro DATs commonly publish separate "Headered"/
/// "Headerless" catalogues for the same system; this is never guessed from
/// the filename alone (section 6/22/23) - only from the DAT's own
/// `<header><name>`/`<header><description>` text, and only when that text
/// is unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoIntroVariant {
    Headered,
    Headerless,
    Aftermarket,
    Bios,
    Unknown,
}

impl NoIntroVariant {
    /// Conservative variant detection from a DAT's own name/description
    /// text. Never consults the filename.
    fn detect(name: &Option<String>, description: &Option<String>) -> Self {
        let haystack = format!(
            "{} {}",
            name.as_deref().unwrap_or(""),
            description.as_deref().unwrap_or("")
        )
        .to_ascii_lowercase();
        if haystack.contains("headerless") {
            Self::Headerless
        } else if haystack.contains("headered") {
            Self::Headered
        } else if haystack.contains("aftermarket") {
            Self::Aftermarket
        } else if haystack.contains("bios") {
            Self::Bios
        } else {
            Self::Unknown
        }
    }
}

/// Errors importing a local DAT as a No-Intro source.
#[derive(Debug)]
pub enum NoIntroImportError {
    /// The file could not be read at all (also covers "does not exist").
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The DAT parsed but its own internal metadata does not identify it as
    /// No-Intro (section 4: never guessed from filename).
    NotNoIntro {
        path: PathBuf,
        detected_ecosystem: DatEcosystem,
    },
    /// The existing DAT parser rejected the file (malformed XML/CMPro,
    /// oversized, etc.) - propagated verbatim, never papered over.
    Parse(ParseError),
}

impl fmt::Display for NoIntroImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "cannot read {}: {error}", path.display()),
            Self::NotNoIntro {
                path,
                detected_ecosystem,
            } => write!(
                f,
                "{} does not identify itself as No-Intro (detected: {:?})",
                path.display(),
                detected_ecosystem
            ),
            Self::Parse(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for NoIntroImportError {}

/// One imported local No-Intro DAT: the parsed catalogue, its reused hash
/// index, and the artifact-level provenance of the DAT file itself
/// (section 8/9) - distinct from the provenance of any ROM it describes.
#[derive(Debug, Clone)]
pub struct ImportedNoIntroSource {
    pub system_name: String,
    pub variant: NoIntroVariant,
    /// Preference order per section 7: internal DAT metadata only in this
    /// batch (no caller-supplied override plumbing yet, no filename
    /// fallback - both would risk fabricating a fact the DAT itself does
    /// not assert).
    pub upstream_version: Option<String>,
    pub artifact_sha256: String,
    pub artifact_name: String,
    pub artifact_path: PathBuf,
    pub entry_count: usize,
    pub rom_count: usize,
    pub dat: ParsedDat,
    pub index: DatIndex,
}

impl ImportedNoIntroSource {
    /// A deterministic manifest line (section 52) - no network, no vibes.
    pub fn manifest_line(&self) -> String {
        format!(
            "{}\n  variant: {:?}\n  version: {}\n  artifact: {}\n  entries: {}",
            self.system_name,
            self.variant,
            self.upstream_version.as_deref().unwrap_or("unknown"),
            self.artifact_sha256,
            self.entry_count,
        )
    }
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Imports one local DAT file as a No-Intro source.
///
/// Reuses [`parse_dat_file`] (Logiqx/ClrMamePro) and [`DatIndex::build`]
/// unchanged - see this module's own doc comment. Refuses (rather than
/// guesses) when the DAT's own internal metadata does not identify it as
/// No-Intro.
pub fn import_no_intro_dat(path: &Path) -> Result<ImportedNoIntroSource, NoIntroImportError> {
    let artifact_sha256 = sha256_file(path).map_err(|error| NoIntroImportError::Io {
        path: path.to_path_buf(),
        error,
    })?;

    let outcome = parse_dat_file(path, DatLimits::default()).map_err(NoIntroImportError::Parse)?;
    let dat = outcome.dat;

    if dat.source.ecosystem != DatEcosystem::NoIntro {
        return Err(NoIntroImportError::NotNoIntro {
            path: path.to_path_buf(),
            detected_ecosystem: dat.source.ecosystem,
        });
    }

    let variant = NoIntroVariant::detect(&dat.source.name, &dat.source.description);
    let system_name = dat
        .source
        .name
        .clone()
        .unwrap_or_else(|| path.display().to_string());
    let artifact_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let index = DatIndex::build(&dat);

    Ok(ImportedNoIntroSource {
        system_name,
        variant,
        upstream_version: dat.source.version.clone(),
        artifact_sha256,
        artifact_name,
        artifact_path: path.to_path_buf(),
        entry_count: dat.source.entry_count,
        rom_count: dat.source.rom_count,
        dat,
        index,
    })
}
