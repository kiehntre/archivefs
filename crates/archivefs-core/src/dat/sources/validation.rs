//! Path policy, folder discovery, and read-only validation for DAT sources.
//!
//! # Why registered paths get a policy the CLI's do not
//!
//! `dat/parsers/mod.rs` explains why a DAT path typed on the command line does
//! not go through `safe_read`: the person running the command chose it, so
//! confining it to the configured source folders would refuse the ordinary case
//! while protecting against nothing. That module also says exactly when the
//! exception stops applying — "the moment a DAT path arrives from
//! configuration, a manifest or any other stored source".
//!
//! This is that moment. A registered path is read again on every validation and
//! every audit, long after whoever typed it has stopped watching, so it gets a
//! policy:
//!
//! - absolute, with no `.` or `..` component;
//! - not a filesystem root;
//! - no component of it may be a symlink, and neither may the target;
//! - it must be the kind of thing it was registered as - a file source that has
//!   become a directory is refused rather than quietly scanned.
//!
//! Note what this is *not*: it is not `TrustedRoots` confinement. DAT files
//! normally live wherever they were downloaded rather than inside a ROM source
//! folder, so confining them there would refuse almost every real DAT. The
//! symlink rules are what the safety model actually needs here, and they are
//! the same ones [`crate::safe_read`] applies with [`crate::safe_read::TrustedRoots::none`].
//!
//! # Folder sources are one level deep, deliberately
//!
//! A folder source scans its own directory and no deeper. Recursion is not
//! merely unbounded-by-default; it is the wrong shape for the thing being
//! modelled - a DAT folder is a flat drop of `.dat` files, and recursing would
//! reach into unrelated trees a user pointed near, not at. Nested collections
//! are supported by registering each folder, which keeps what is scanned equal
//! to what was chosen.
//!
//! # Nothing here modifies a DAT file
//!
//! Every filesystem call in this module is `read_dir`, `symlink_metadata`,
//! `metadata`, or a read-only open. There is no create, write, rename, remove,
//! or permission change anywhere in it.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::{DatHealthState, DatSourceEntry, DatSourceHealth, DatSourceKind, now_unix};
use crate::dat::limits::DatLimits;
use crate::dat::model::{DatEcosystem, DatFormat};
use crate::dat::parser::DiagnosticSeverity;
use crate::dat::parsers::parse_dat_file;

/// How many DAT files one folder source will take.
///
/// Not a guess at what is reasonable: it is what stops a folder that happens to
/// hold thousands of files from turning one "Validate" click into an unbounded
/// parse. Anything beyond it is reported as truncated rather than silently
/// ignored.
pub const MAX_FOLDER_DAT_FILES: usize = 512;

/// How many directory entries a folder scan will look at before stopping.
///
/// Separate from [`MAX_FOLDER_DAT_FILES`] because the cost of *considering* an
/// entry (one `symlink_metadata`, and a 512-byte read for anything with a
/// plausible extension) is paid even for files that are not DATs.
pub const MAX_FOLDER_ENTRIES_EXAMINED: usize = 20_000;

/// How much of a file is read to decide whether it is a DAT at all.
const SNIFF_BYTES: usize = 512;

/// Why a registered path was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatPathRefusal {
    NotAbsolute,
    NonNormalComponent,
    FilesystemRoot,
    SymlinkInPath(PathBuf),
    Unreadable(String),
    /// Registered as a file, but the path is a directory.
    ExpectedFileFoundDirectory,
    /// Registered as a folder, but the path is a file.
    ExpectedDirectoryFoundFile,
    /// Neither a regular file nor a directory: a FIFO, socket, device, …
    NotFileOrDirectory,
}

impl DatPathRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::NotAbsolute => "the path is not absolute".to_string(),
            Self::NonNormalComponent => "the path contains a '.' or '..' component".to_string(),
            Self::FilesystemRoot => {
                "the filesystem root cannot be registered as a DAT source".to_string()
            }
            Self::SymlinkInPath(component) => format!(
                "symlink refused: {} is a symbolic link, and a registered DAT path is read \
                 unattended, so it must not be able to change what it points at",
                component.display()
            ),
            Self::Unreadable(detail) => detail.clone(),
            Self::ExpectedFileFoundDirectory => {
                "this was registered as a single DAT file, but the path is a folder. \
                 Register it as a folder source instead."
                    .to_string()
            }
            Self::ExpectedDirectoryFoundFile => {
                "this was registered as a folder of DAT files, but the path is a single file. \
                 Register it as a file source instead."
                    .to_string()
            }
            Self::NotFileOrDirectory => {
                "the path is neither a regular file nor a folder".to_string()
            }
        }
    }

    /// A stable, machine-readable reason.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAbsolute => "not_absolute",
            Self::NonNormalComponent => "non_normal_component",
            Self::FilesystemRoot => "filesystem_root",
            Self::SymlinkInPath(_) => "symlink_in_path",
            Self::Unreadable(_) => "unreadable",
            Self::ExpectedFileFoundDirectory => "expected_file_found_directory",
            Self::ExpectedDirectoryFoundFile => "expected_directory_found_file",
            Self::NotFileOrDirectory => "not_file_or_directory",
        }
    }
}

impl std::fmt::Display for DatPathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail())
    }
}

/// Applies the registered-path policy documented on this module.
pub fn validate_dat_path(path: &Path, kind: DatSourceKind) -> Result<(), DatPathRefusal> {
    if !path.is_absolute() {
        return Err(DatPathRefusal::NotAbsolute);
    }
    let mut normal_components = 0usize;
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(_) => normal_components += 1,
            Component::CurDir | Component::ParentDir => {
                return Err(DatPathRefusal::NonNormalComponent);
            }
        }
    }
    if normal_components == 0 {
        return Err(DatPathRefusal::FilesystemRoot);
    }

    // Every component, not just the last: a symlinked parent directory can
    // redirect the read just as effectively as a symlinked file, and this path
    // is re-read on every audit rather than once under supervision.
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::Normal(part) => current.push(part),
            _ => return Err(DatPathRefusal::NonNormalComponent),
        }
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            DatPathRefusal::Unreadable(format!("{}: {error}", current.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(DatPathRefusal::SymlinkInPath(current));
        }
    }

    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| DatPathRefusal::Unreadable(format!("{}: {error}", path.display())))?;
    match kind {
        DatSourceKind::File => {
            if metadata.is_dir() {
                return Err(DatPathRefusal::ExpectedFileFoundDirectory);
            }
            if !metadata.is_file() {
                return Err(DatPathRefusal::NotFileOrDirectory);
            }
        }
        DatSourceKind::Folder => {
            if metadata.is_file() {
                return Err(DatPathRefusal::ExpectedDirectoryFoundFile);
            }
            if !metadata.is_dir() {
                return Err(DatPathRefusal::NotFileOrDirectory);
            }
        }
    }
    Ok(())
}

/// A file inside a folder source that was looked at but not taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedFolderEntry {
    pub file_name: String,
    pub reason: String,
}

/// The DAT files a folder source contributes, and what it passed over.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderScan {
    pub files: Vec<PathBuf>,
    pub skipped: Vec<SkippedFolderEntry>,
    /// The folder held more DAT files than [`MAX_FOLDER_DAT_FILES`], so this
    /// listing is the first of them and not all of them.
    pub truncated: bool,
    /// How many DAT files the folder actually holds, when that is genuinely
    /// known. `Some` only when the folder was fully examined (the
    /// [`MAX_FOLDER_ENTRIES_EXAMINED`] ceiling was not hit), so a report can
    /// say "512 of 2,024 DAT files read" instead of inventing a total. `None`
    /// when the scan stopped early and the true count is not known.
    pub total_dat_files: Option<usize>,
}

/// Lists the DAT files directly inside `folder`, in a deterministic order.
///
/// # Why extension is not enough
///
/// `.dat` is used by plenty of unrelated things and `.xml` by almost
/// everything. Taking every `.xml` in a folder would quietly pull a user's
/// unrelated documents into their catalogue and report entry counts for them.
/// So a candidate is chosen by extension and then *confirmed* by reading its
/// first few hundred bytes: it must actually open with a Logiqx `<datafile`
/// root or a ClrMamePro `clrmamepro (` header. Anything else is skipped with a
/// reason, so the user can see the file was considered rather than missed.
///
/// This is stricter than [`crate::dat::parsers::detect_format`], which assumes
/// ClrMamePro for anything it does not recognise. That assumption is right for
/// a path someone typed - they meant that file - and wrong for a folder sweep,
/// where the same assumption would accept every text file present.
pub fn discover_dat_files(folder: &Path) -> Result<FolderScan, DatPathRefusal> {
    validate_dat_path(folder, DatSourceKind::Folder)?;

    let read_dir = std::fs::read_dir(folder)
        .map_err(|error| DatPathRefusal::Unreadable(format!("{}: {error}", folder.display())))?;

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut skipped: Vec<SkippedFolderEntry> = Vec::new();
    let mut truncated = false;
    let mut examined = 0usize;
    // Whether the folder was looked at to the end. When this is false the DAT
    // count below is a partial one and must not be reported as a total.
    let mut scan_was_complete = true;

    for entry in read_dir {
        let Ok(entry) = entry else { continue };
        examined += 1;
        if examined > MAX_FOLDER_ENTRIES_EXAMINED {
            truncated = true;
            scan_was_complete = false;
            break;
        }
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().into_owned();

        let Ok(metadata) = entry.metadata() else {
            skipped.push(SkippedFolderEntry {
                file_name,
                reason: "could not be read".to_string(),
            });
            continue;
        };
        // `DirEntry::metadata` does not follow symlinks, so this catches a link
        // whatever it points at. A folder source scans what is in the folder,
        // and a link is an instruction to look somewhere else.
        if metadata.file_type().is_symlink() {
            skipped.push(SkippedFolderEntry {
                file_name,
                reason: "symbolic link; register its target directly if you want it".to_string(),
            });
            continue;
        }
        if metadata.is_dir() {
            // Not an error and not interesting: subfolders are simply not part
            // of a one-level scan, and saying so for each of them would bury
            // the files that were skipped for a reason worth reading.
            continue;
        }
        if !metadata.is_file() {
            skipped.push(SkippedFolderEntry {
                file_name,
                reason: "not a regular file".to_string(),
            });
            continue;
        }
        if !has_dat_extension(&path) {
            continue;
        }
        match sniff_dat_format(&path) {
            Some(_) => candidates.push(path),
            None => skipped.push(SkippedFolderEntry {
                file_name,
                reason: "does not start with a Logiqx <datafile> root or a ClrMamePro header"
                    .to_string(),
            }),
        }
    }

    // Sorted by name so two runs over the same folder produce the same order,
    // and so a report can be compared with one taken yesterday. `read_dir`
    // ordering is filesystem-defined and must never leak into a result.
    candidates.sort();
    skipped.sort_by(|left, right| left.file_name.cmp(&right.file_name));

    // The total is only honest when the whole folder was examined; a scan cut
    // short by the entries ceiling has only seen part of it.
    let total_dat_files = scan_was_complete.then_some(candidates.len());

    if candidates.len() > MAX_FOLDER_DAT_FILES {
        candidates.truncate(MAX_FOLDER_DAT_FILES);
        truncated = true;
    }

    Ok(FolderScan {
        files: candidates,
        skipped,
        truncated,
        total_dat_files,
    })
}

/// Whether a filename is worth opening at all.
fn has_dat_extension(path: &Path) -> bool {
    path.extension()
        .map(|extension| {
            let extension = extension.to_string_lossy().to_ascii_lowercase();
            extension == "dat" || extension == "xml"
        })
        .unwrap_or(false)
}

/// Reads a few hundred bytes and reports the DAT format, or `None`.
///
/// Bounded by construction: one open, one short read, no allocation
/// proportional to file size. A file that cannot be opened is not a DAT as far
/// as this is concerned.
pub fn sniff_dat_format(path: &Path) -> Option<DatFormat> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = [0u8; SNIFF_BYTES];
    let read = file.read(&mut buffer).ok()?;
    // `from_utf8_lossy` rather than a strict decode: a DAT that declares a
    // non-UTF-8 encoding still has an ASCII root element, and refusing to sniff
    // it here would report a real catalogue as "not a DAT". Whether its
    // *contents* decode is the parser's question, answered with a real error.
    let head = String::from_utf8_lossy(&buffer[..read]);
    let head = head.trim_start().to_ascii_lowercase();

    if head.starts_with("<?xml") || head.starts_with("<!doctype") || head.starts_with("<datafile") {
        // An XML declaration alone is not enough: plenty of XML is not a DAT.
        // The root element has to be `datafile`, which is what every Logiqx
        // catalogue uses.
        return head.contains("<datafile").then_some(DatFormat::Logiqx);
    }
    if head.starts_with("clrmamepro") {
        return Some(DatFormat::ClrMamePro);
    }
    None
}

/// One diagnostic attached to a parsed DAT file, reduced to what a report and
/// the GUI show: the severity, a stable code, the message, and the location the
/// parser recorded (when it records one at all).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatDiagnostic {
    pub severity: crate::dat::parser::DiagnosticSeverity,
    pub code: &'static str,
    pub message: String,
    /// The line in the DAT the parser attributed the diagnostic to, when the
    /// parser records one. `None` when the parser does not track lines.
    pub line: Option<usize>,
    /// The column within the line, when the parser records one.
    pub column: Option<usize>,
}

/// What one DAT file in a source turned out to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum DatFileOutcome {
    Parsed {
        format: DatFormat,
        ecosystem: DatEcosystem,
        name: Option<String>,
        version: Option<String>,
        entry_count: usize,
        rom_count: usize,
        /// Every diagnostic the parser produced, each with its severity. A
        /// parser note is expected behaviour and must not lower the verdict.
        diagnostics: Vec<DatDiagnostic>,
    },
    /// The parser refused it, with the reason it gave.
    Failed { error: String },
}

impl DatFileOutcome {
    pub fn is_parsed(&self) -> bool {
        matches!(self, Self::Parsed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatFileReport {
    pub path: String,
    pub file_name: String,
    pub outcome: DatFileOutcome,
}

/// Two DAT files in one folder source claiming to be the same catalogue.
///
/// Reported, never resolved: which of two same-named catalogues is the one the
/// user wants is not a question this build can answer, and picking one silently
/// would make the other's absence invisible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateDatIdentity {
    /// The header identity both files claim, as `name` or `name (version)`.
    pub identity: String,
    pub file_names: Vec<String>,
}

/// Everything a validation run observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatValidationReport {
    pub source_id: String,
    pub path: String,
    pub kind: &'static str,
    pub state: DatHealthState,
    pub files: Vec<DatFileReport>,
    pub duplicate_identities: Vec<DuplicateDatIdentity>,
    pub skipped: Vec<SkippedFolderEntry>,
    pub truncated: bool,
    /// How many DAT files the folder actually holds, when genuinely known
    /// (`None` when the scan stopped before seeing all of them). Only
    /// meaningful for a folder source whose listing was truncated.
    pub total_dat_files: Option<usize>,
    /// One line, suitable for a status row.
    pub summary: String,
    pub entry_count: usize,
    pub rom_count: usize,
    /// Distinct format labels observed, sorted.
    pub formats: Vec<String>,
    /// A path-policy refusal, when the source never got as far as parsing.
    pub path_refusal: Option<String>,
}

impl DatValidationReport {
    /// The health record this run produced, ready to store on the entry.
    pub fn to_health(&self, path: &Path, kind: DatSourceKind) -> DatSourceHealth {
        let (size, modified) = match kind {
            DatSourceKind::File => observe_file(path),
            DatSourceKind::Folder => (None, None),
        };
        DatSourceHealth {
            state: Some(self.state),
            last_validated_unix_seconds: Some(now_unix()),
            detail: Some(self.summary.clone()),
            entry_count: Some(self.entry_count as u64),
            rom_count: Some(self.rom_count as u64),
            file_count: match kind {
                DatSourceKind::Folder => Some(self.files.len() as u64),
                DatSourceKind::File => None,
            },
            formats: (!self.formats.is_empty()).then(|| self.formats.clone()),
            observed_size_bytes: size,
            observed_modified_unix_seconds: modified,
        }
    }
}

fn observe_file(path: &Path) -> (Option<u64>, Option<i64>) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return (None, None);
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs() as i64);
    (Some(metadata.len()), modified)
}

/// Validates one registered source, reading it and nothing else.
///
/// The path policy runs first, so a source whose path has become a directory,
/// a symlink, or nothing at all is reported as unreadable without any parsing
/// being attempted. Parsing itself is bounded by `limits`; every ceiling in
/// [`DatLimits`] applies, including the file-size one, so a hostile or
/// accidental 4 GB file is refused rather than read.
pub fn validate_dat_source(entry: &DatSourceEntry, limits: DatLimits) -> DatValidationReport {
    let mut report = DatValidationReport {
        source_id: entry.id.clone(),
        path: entry.path.to_string_lossy().into_owned(),
        kind: entry.kind.label(),
        state: DatHealthState::Unreadable,
        files: Vec::new(),
        duplicate_identities: Vec::new(),
        skipped: Vec::new(),
        truncated: false,
        total_dat_files: None,
        summary: String::new(),
        entry_count: 0,
        rom_count: 0,
        formats: Vec::new(),
        path_refusal: None,
    };

    if let Err(refusal) = validate_dat_path(&entry.path, entry.kind) {
        report.summary = refusal.detail();
        report.path_refusal = Some(refusal.code().to_string());
        return report;
    }

    let files: Vec<PathBuf> = match entry.kind {
        DatSourceKind::File => vec![entry.path.clone()],
        DatSourceKind::Folder => match discover_dat_files(&entry.path) {
            Ok(scan) => {
                report.skipped = scan.skipped;
                report.truncated = scan.truncated;
                report.total_dat_files = scan.total_dat_files;
                scan.files
            }
            Err(refusal) => {
                report.summary = refusal.detail();
                report.path_refusal = Some(refusal.code().to_string());
                return report;
            }
        },
    };

    if files.is_empty() {
        report.state = DatHealthState::Invalid;
        report.summary = match entry.kind {
            DatSourceKind::Folder => {
                "No DAT files found in this folder. Only .dat and .xml files that start with a \
                 Logiqx <datafile> root or a ClrMamePro header are used."
                    .to_string()
            }
            DatSourceKind::File => "The file is empty.".to_string(),
        };
        return report;
    }

    let mut formats: Vec<String> = Vec::new();
    let mut identities: Vec<(String, String)> = Vec::new();
    let mut failures = 0usize;
    let mut warned = false;
    let mut errored = false;

    for path in &files {
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());

        let outcome = match parse_dat_file(path, limits) {
            Ok(parsed) => {
                let source = &parsed.dat.source;
                report.entry_count = report.entry_count.saturating_add(source.entry_count);
                report.rom_count = report.rom_count.saturating_add(source.rom_count);
                let label = source.format.label().to_string();
                if !formats.contains(&label) {
                    formats.push(label);
                }
                if let Some(identity) =
                    header_identity(source.name.as_deref(), source.version.as_deref())
                {
                    identities.push((identity, file_name.clone()));
                }
                // The structured warnings the parser returned carry a severity;
                // the parser's own string copy (`source.parse_warnings`) holds
                // the same messages, so it is not folded in again.
                let mut diagnostics: Vec<DatDiagnostic> = parsed
                    .warnings
                    .iter()
                    .map(|warning| DatDiagnostic {
                        severity: warning.severity(),
                        code: warning.code(),
                        message: warning.to_string(),
                        line: warning.line,
                        column: warning.column,
                    })
                    .collect();
                // The same message can legitimately be recorded several times
                // (for example one per affected ROM); report it once.
                let mut seen = std::collections::BTreeSet::new();
                diagnostics.retain(|diagnostic| seen.insert(diagnostic.message.clone()));
                for diagnostic in &diagnostics {
                    match diagnostic.severity {
                        DiagnosticSeverity::Note => {}
                        DiagnosticSeverity::Warning => warned = true,
                        DiagnosticSeverity::Error => errored = true,
                    }
                }
                DatFileOutcome::Parsed {
                    format: source.format,
                    ecosystem: source.ecosystem,
                    name: source.name.clone(),
                    version: source.version.clone(),
                    entry_count: source.entry_count,
                    rom_count: source.rom_count,
                    diagnostics,
                }
            }
            Err(error) => {
                failures += 1;
                DatFileOutcome::Failed {
                    error: error.to_string(),
                }
            }
        };

        report.files.push(DatFileReport {
            path: path.to_string_lossy().into_owned(),
            file_name,
            outcome,
        });
    }

    formats.sort();
    report.formats = formats;
    report.duplicate_identities = collect_duplicate_identities(identities);

    // The verdict is the highest severity present. A file that failed to parse
    // is an error; so is a diagnostic classified as an error. Warnings (but no
    // errors) mean the source is valid but worth a look. Parser notes are
    // expected behaviour and never lower a Valid verdict.
    report.state = if failures == files.len() {
        DatHealthState::Invalid
    } else if failures > 0 || errored {
        // A folder where some files parsed and some did not is not "valid":
        // the user asked for the folder, and part of what they asked for is
        // unusable.
        DatHealthState::Invalid
    } else if warned || !report.duplicate_identities.is_empty() || report.truncated {
        DatHealthState::ValidWithWarnings
    } else {
        DatHealthState::Valid
    };

    report.summary = summarise(&report, files.len(), failures);
    report
}

fn header_identity(name: Option<&str>, version: Option<&str>) -> Option<String> {
    let name = name?.trim();
    if name.is_empty() {
        return None;
    }
    Some(match version.map(str::trim).filter(|v| !v.is_empty()) {
        Some(version) => format!("{name} ({version})"),
        None => name.to_string(),
    })
}

/// Groups identities claimed by more than one file, in a stable order.
fn collect_duplicate_identities(identities: Vec<(String, String)>) -> Vec<DuplicateDatIdentity> {
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (identity, file_name) in identities {
        grouped.entry(identity).or_default().push(file_name);
    }
    grouped
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(identity, mut file_names)| {
            file_names.sort();
            DuplicateDatIdentity {
                identity,
                file_names,
            }
        })
        .collect()
}

fn summarise(report: &DatValidationReport, total_files: usize, failures: usize) -> String {
    let mut parts = Vec::new();
    if total_files == 1 {
        if failures == 1 {
            return match report.files.first().map(|file| &file.outcome) {
                Some(DatFileOutcome::Failed { error }) => error.clone(),
                _ => "The DAT file could not be parsed.".to_string(),
            };
        }
        parts.push(format!(
            "{} entries, {} ROMs",
            report.entry_count, report.rom_count
        ));
    } else {
        parts.push(format!(
            "{} DAT files, {} entries, {} ROMs",
            total_files, report.entry_count, report.rom_count
        ));
        if failures > 0 {
            parts.push(format!("{failures} could not be parsed"));
        }
    }
    if !report.formats.is_empty() {
        parts.push(report.formats.join(", "));
    }
    if !report.duplicate_identities.is_empty() {
        parts.push(format!(
            "{} duplicate catalogue identity(s)",
            report.duplicate_identities.len()
        ));
    }
    if report.truncated {
        parts.push(format!(
            "more than {MAX_FOLDER_DAT_FILES} DAT files; only the first were read"
        ));
    }
    parts.join(" · ")
}
