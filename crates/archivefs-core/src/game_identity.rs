//! Strictly read-only, bounded game identity inspection.
//!
//! Identity is evidence: only values obtained from reviewed on-disc structures
//! are `Verified`. Archive and member names can only produce `Candidate` values.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::ZipArchive;

pub const MAX_BYTES_READ: u64 = 64 * 1024 * 1024;
pub const MAX_ARCHIVE_MEMBERS: usize = 4_096;
pub const MAX_METADATA_PATHS: usize = 32;
pub const MAX_ISO_DIRECTORY_ENTRIES: usize = 4_096;
pub const MAX_ISO_DESCRIPTORS: usize = 32;
pub const MAX_PATH_BYTES: usize = 512;
pub const MAX_SYSTEM_CNF_BYTES: u64 = 64 * 1024;
pub const MAX_EXECUTABLE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_DIRECTORY_BYTES: u64 = 1024 * 1024;
pub const MAX_NESTED_CONTAINER_DEPTH: usize = 1;
pub const MAX_RETAINED_WARNINGS: usize = 64;
pub const MAX_LOOSE_ROM_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_LOOSE_ROM_FILES: usize = 1;
pub const MAX_LOOSE_ROM_WARNINGS: usize = 16;
pub const MAX_LOOSE_ROM_METADATA_TOKENS: usize = 16;

const ISO_SECTOR_SIZE: u64 = 2_048;
const DOLPHIN_HEADER_BYTES: usize = 0x20;
const WII_MAGIC_OFFSET: usize = 0x18;
const GAMECUBE_MAGIC_OFFSET: usize = 0x1c;
const WII_MAGIC: [u8; 4] = [0x5d, 0x1c, 0x9e, 0xa3];
const GAMECUBE_MAGIC: [u8; 4] = [0xc2, 0x33, 0x9f, 0x3d];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityStatus {
    Verified,
    Candidate,
    Missing,
    Unsupported,
    Deferred,
    Invalid,
    Ambiguous,
    ResourceLimitReached,
}

impl fmt::Display for IdentityStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Verified => "Verified",
            Self::Candidate => "Candidate",
            Self::Missing => "Missing",
            Self::Unsupported => "Unsupported",
            Self::Deferred => "Deferred",
            Self::Invalid => "Invalid",
            Self::Ambiguous => "Ambiguous",
            Self::ResourceLimitReached => "Resource limit reached",
        };
        f.write_str(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityKind {
    Platform,
    Ps2Serial,
    Pcsx2ExecutableCrc,
    DolphinGameId,
    DolphinRevision,
    DolphinDiscNumber,
    DolphinRegion,
    LooseRomSha256,
    LooseRomFormat,
    LooseRomTitle,
}

impl fmt::Display for IdentityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Platform => "Platform",
            Self::Ps2Serial => "PS2 serial",
            Self::Pcsx2ExecutableCrc => "PCSX2 executable CRC",
            Self::DolphinGameId => "Dolphin Game ID",
            Self::DolphinRevision => "Dolphin revision",
            Self::DolphinDiscNumber => "Dolphin disc number",
            Self::DolphinRegion => "Dolphin region code",
            Self::LooseRomSha256 => "Local ROM SHA-256",
            Self::LooseRomFormat => "Loose ROM format",
            Self::LooseRomTitle => "Normalized ROM title",
        };
        f.write_str(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityConfidence {
    ExactBytes,
    StructuredMetadata,
    CatalogueContext,
    FilenameOnly,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityPlatform {
    PlayStation2,
    GameCube,
    Wii,
    MegaDrive,
    Snes,
    Other,
}

impl IdentityPlatform {
    pub fn from_catalogue(value: Option<&str>) -> Self {
        let value = value.unwrap_or_default().trim().to_ascii_lowercase();
        match value.as_str() {
            "playstation 2" | "playstation2" | "ps2" | "sony playstation 2" => Self::PlayStation2,
            "gamecube" | "nintendo gamecube" | "gc" | "gcn" => Self::GameCube,
            "wii" | "nintendo wii" => Self::Wii,
            "megadrive" | "mega drive" | "genesis" | "sega mega drive" | "sega genesis" => {
                Self::MegaDrive
            }
            "snes"
            | "super nintendo"
            | "super nintendo entertainment system"
            | "nintendo super nintendo entertainment system"
            | "super famicom" => Self::Snes,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PlayStation2 => "PlayStation 2",
            Self::GameCube => "GameCube",
            Self::Wii => "Wii",
            Self::MegaDrive => "Mega Drive / Genesis",
            Self::Snes => "SNES",
            Self::Other => "Unsupported platform",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityImageFormat {
    Iso,
    ZipContainingIso,
    LooseCartridgeRom,
    Deferred,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityProvenance {
    pub archive_path: PathBuf,
    pub member_path: Option<Vec<u8>>,
    pub member_index: Option<usize>,
    pub method: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityEvidence {
    pub kind: IdentityKind,
    pub status: IdentityStatus,
    pub value: Option<String>,
    pub confidence: IdentityConfidence,
    pub provenance: IdentityProvenance,
    pub diagnostic: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameIdentityReport {
    pub archive_path: PathBuf,
    pub platform: IdentityPlatform,
    pub format: IdentityImageFormat,
    pub evidence: Vec<IdentityEvidence>,
    pub warnings: Vec<String>,
    pub bytes_read: u64,
    pub archive_members_inspected: usize,
    pub metadata_paths_inspected: usize,
    pub nested_container_depth: usize,
    pub complete: bool,
}

impl GameIdentityReport {
    pub fn verified_value(&self, kind: IdentityKind) -> Option<&str> {
        self.evidence.iter().find_map(|evidence| {
            (evidence.kind == kind && evidence.status == IdentityStatus::Verified)
                .then_some(evidence.value.as_deref())
                .flatten()
        })
    }

    pub fn verified_dolphin_game_id(&self) -> Option<&str> {
        self.verified_value(IdentityKind::DolphinGameId)
    }

    pub fn verified_dolphin_revision(&self) -> Option<u16> {
        self.verified_value(IdentityKind::DolphinRevision)?
            .parse()
            .ok()
    }

    pub fn verified_pcsx2_crc(&self) -> Option<&str> {
        self.verified_value(IdentityKind::Pcsx2ExecutableCrc)
    }

    pub fn verified_loose_rom_sha256(&self) -> Option<&str> {
        self.verified_value(IdentityKind::LooseRomSha256)
    }

    pub fn is_verified_loose_rom(&self) -> bool {
        self.format == IdentityImageFormat::LooseCartridgeRom
            && self.verified_loose_rom_sha256().is_some()
    }
}

pub fn inspect_game_identity(path: &Path, platform_hint: Option<&str>) -> GameIdentityReport {
    inspect_game_identity_with_platform_trust(path, platform_hint, false)
}

/// Inspect identity using platform evidence already validated by the library
/// scanner or an explicit manual assignment. The boolean is deliberately not
/// inferred from a filename: callers must opt in at the catalogue boundary.
pub fn inspect_catalogued_game_identity(
    path: &Path,
    platform_hint: Option<&str>,
) -> GameIdentityReport {
    inspect_game_identity_with_platform_trust(path, platform_hint, true)
}

fn inspect_game_identity_with_platform_trust(
    path: &Path,
    platform_hint: Option<&str>,
    trusted_platform: bool,
) -> GameIdentityReport {
    let platform = IdentityPlatform::from_catalogue(platform_hint);
    let mut report = GameIdentityReport {
        archive_path: path.to_path_buf(),
        platform,
        format: IdentityImageFormat::Unsupported,
        evidence: Vec::new(),
        warnings: Vec::new(),
        bytes_read: 0,
        archive_members_inspected: 0,
        metadata_paths_inspected: 0,
        nested_container_depth: 0,
        complete: false,
    };
    report.evidence.push(evidence(
        &report,
        IdentityKind::Platform,
        if trusted_platform {
            IdentityStatus::Verified
        } else {
            IdentityStatus::Candidate
        },
        platform_hint.map(str::to_owned),
        IdentityConfidence::CatalogueContext,
        if trusted_platform {
            "exact platform supplied by scanner or manual assignment; not derived from ROM bytes"
        } else {
            "catalogue platform context; not derived from disc bytes"
        },
        if trusted_platform {
            "trusted ArchiveFS library platform context"
        } else {
            "ArchiveFS catalogue context"
        },
    ));
    add_filename_candidate(&mut report);

    if matches!(
        platform,
        IdentityPlatform::MegaDrive | IdentityPlatform::Snes
    ) {
        inspect_loose_rom(&mut report, trusted_platform);
        return report;
    }

    if platform == IdentityPlatform::Other {
        report.evidence.push(evidence(
            &report,
            IdentityKind::DolphinGameId,
            IdentityStatus::Unsupported,
            None,
            IdentityConfidence::Unavailable,
            "shared identity inspection currently supports PS2, GameCube, and Wii",
            "platform eligibility",
        ));
        return report;
    }

    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "iso" => inspect_direct_iso(&mut report),
        "zip" => inspect_zip_iso(&mut report),
        "chd" | "cso" | "rvz" | "wbfs" | "7z" | "rar" => {
            report.format = IdentityImageFormat::Deferred;
            add_unavailable(
                &mut report,
                IdentityStatus::Deferred,
                "format has no existing safe bounded reader in ArchiveFS",
            );
        }
        _ => add_unavailable(
            &mut report,
            IdentityStatus::Unsupported,
            "only direct ISO and a single ISO inside ZIP are supported",
        ),
    }
    report
}

pub fn supported_loose_rom_format(path: &Path, platform: IdentityPlatform) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match (platform, extension.as_str()) {
        (IdentityPlatform::MegaDrive, "md") => Some("md"),
        (IdentityPlatform::MegaDrive, "gen") => Some("gen"),
        (IdentityPlatform::MegaDrive, "smd") => Some("smd"),
        (IdentityPlatform::MegaDrive, "bin") => Some("bin"),
        (IdentityPlatform::Snes, "sfc") => Some("sfc"),
        (IdentityPlatform::Snes, "smc") => Some("smc"),
        _ => None,
    }
}

fn inspect_loose_rom(report: &mut GameIdentityReport, trusted_platform: bool) {
    report.format = IdentityImageFormat::LooseCartridgeRom;
    let Some(format) = supported_loose_rom_format(&report.archive_path, report.platform) else {
        add_loose_rom_unavailable(
            report,
            IdentityStatus::Unsupported,
            "file extension is not supported for the exact cartridge platform",
        );
        return;
    };
    if !trusted_platform {
        add_loose_rom_unavailable(
            report,
            IdentityStatus::Ambiguous,
            "loose ROM identity requires exact scanner or manual platform evidence",
        );
        return;
    }
    let mut file = match open_read_only_regular(&report.archive_path) {
        Ok(file) => file,
        Err(message) => {
            add_loose_rom_unavailable(report, IdentityStatus::Invalid, &message);
            return;
        }
    };
    let before = match StableFileMetadata::from_file(&file) {
        Ok(metadata) => metadata,
        Err(error) => {
            add_loose_rom_unavailable(report, IdentityStatus::Invalid, &error.to_string());
            return;
        }
    };
    if before.len > MAX_LOOSE_ROM_BYTES {
        add_loose_rom_unavailable(
            report,
            IdentityStatus::ResourceLimitReached,
            &format!(
                "loose ROM is {} bytes; maximum supported size is {} bytes",
                before.len, MAX_LOOSE_ROM_BYTES
            ),
        );
        return;
    }
    let digest = match hash_bounded_file(&mut file, MAX_LOOSE_ROM_BYTES) {
        Ok((digest, bytes_read)) => {
            report.bytes_read = bytes_read;
            digest
        }
        Err(error) => {
            add_loose_rom_unavailable(report, source_error_status(&error), &error.to_string());
            return;
        }
    };
    let after = match StableFileMetadata::from_file(&file) {
        Ok(metadata) => metadata,
        Err(error) => {
            add_loose_rom_unavailable(report, IdentityStatus::Invalid, &error.to_string());
            return;
        }
    };
    if !loose_rom_read_was_stable(&before, &after, report.bytes_read) {
        add_loose_rom_unavailable(
            report,
            IdentityStatus::Invalid,
            "loose ROM changed while its identity was being read",
        );
        return;
    }

    report.evidence.push(evidence(
        report,
        IdentityKind::LooseRomSha256,
        IdentityStatus::Verified,
        Some(digest),
        IdentityConfidence::ExactBytes,
        "SHA-256 covers the exact on-disk bytes; it is not a known-good dump claim",
        "bounded full-file SHA-256",
    ));
    report.evidence.push(evidence(
        report,
        IdentityKind::LooseRomFormat,
        IdentityStatus::Verified,
        Some(format.to_string()),
        IdentityConfidence::StructuredMetadata,
        if format == "smd" {
            "format is recorded from the exact extension; bytes were not header-stripped or deinterleaved"
        } else {
            "format is recorded from the exact extension and trusted platform context"
        },
        "exact file extension",
    ));
    if let Some(title) = normalized_loose_rom_title(&report.archive_path) {
        report.evidence.push(evidence(
            report,
            IdentityKind::LooseRomTitle,
            IdentityStatus::Verified,
            Some(title),
            IdentityConfidence::CatalogueContext,
            "deterministic display title derived from the exact filename; not content identity",
            "filename stem normalization",
        ));
    } else {
        retain_warning(
            report,
            "ROM title contains unsupported path encoding; exact path bytes remain preserved",
        );
    }
    report.complete = true;
}

fn loose_rom_read_was_stable(
    before: &StableFileMetadata,
    after: &StableFileMetadata,
    bytes_read: u64,
) -> bool {
    before == after && bytes_read == before.len
}

fn normalized_loose_rom_title(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let mut normalized = String::with_capacity(stem.len());
    let mut separator = true;
    for character in stem.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
            separator = false;
        } else if !separator {
            normalized.push(' ');
            separator = true;
        }
    }
    normalized.truncate(normalized.trim_end().len());
    (!normalized.is_empty()).then_some(normalized)
}

fn add_loose_rom_unavailable(
    report: &mut GameIdentityReport,
    status: IdentityStatus,
    diagnostic: &str,
) {
    for kind in [IdentityKind::LooseRomSha256, IdentityKind::LooseRomFormat] {
        report.evidence.push(evidence(
            report,
            kind,
            status,
            None,
            IdentityConfidence::Unavailable,
            diagnostic,
            "loose cartridge ROM safety eligibility",
        ));
    }
}

fn hash_bounded_file(file: &mut File, maximum: u64) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("loose ROM byte count overflow"))?;
        if total > maximum {
            return Err(io::Error::other("loose ROM hash byte limit reached"));
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok((
        digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        total,
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct StableFileMetadata {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl StableFileMetadata {
    fn from_file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }
}

fn retain_warning(report: &mut GameIdentityReport, warning: &str) {
    if report.warnings.len() < MAX_LOOSE_ROM_WARNINGS.min(MAX_RETAINED_WARNINGS) {
        report.warnings.push(warning.to_string());
    }
}

fn inspect_direct_iso(report: &mut GameIdentityReport) {
    report.format = IdentityImageFormat::Iso;
    let file = match open_read_only_regular(&report.archive_path) {
        Ok(file) => file,
        Err(message) => {
            add_unavailable(report, IdentityStatus::Invalid, &message);
            return;
        }
    };
    let len = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            add_unavailable(report, IdentityStatus::Invalid, &error.to_string());
            return;
        }
    };
    let mut source = FileSource {
        file,
        len,
        bytes_read: 0,
    };
    inspect_iso_source(report, &mut source, None, None);
    report.bytes_read = source.bytes_read;
}

fn inspect_zip_iso(report: &mut GameIdentityReport) {
    report.format = IdentityImageFormat::ZipContainingIso;
    report.nested_container_depth = 1;
    let file = match open_read_only_regular(&report.archive_path) {
        Ok(file) => file,
        Err(message) => {
            add_unavailable(report, IdentityStatus::Invalid, &message);
            return;
        }
    };
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(error) => {
            add_unavailable(
                report,
                IdentityStatus::Invalid,
                &format!("invalid ZIP: {error}"),
            );
            return;
        }
    };
    if archive.len() > MAX_ARCHIVE_MEMBERS {
        report.archive_members_inspected = MAX_ARCHIVE_MEMBERS;
        add_unavailable(
            report,
            IdentityStatus::ResourceLimitReached,
            "ZIP member limit reached before identity inspection",
        );
        return;
    }
    let mut iso_members = Vec::new();
    for index in 0..archive.len() {
        report.archive_members_inspected += 1;
        let raw = match archive.by_index_raw(index) {
            Ok(raw) => raw,
            Err(error) => {
                add_unavailable(report, IdentityStatus::Invalid, &error.to_string());
                return;
            }
        };
        if raw.encrypted() {
            add_unavailable(
                report,
                IdentityStatus::Unsupported,
                "encrypted ZIP entries are refused",
            );
            return;
        }
        if !raw.is_dir() && ascii_extension_is_iso(raw.name_raw()) {
            iso_members.push((index, raw.name_raw().to_vec(), raw.size()));
        }
    }
    if iso_members.is_empty() {
        add_unavailable(
            report,
            IdentityStatus::Missing,
            "ZIP contains no ISO member",
        );
        return;
    }
    if iso_members.len() != 1 {
        add_unavailable(
            report,
            IdentityStatus::Ambiguous,
            "ZIP contains multiple ISO members; none was selected implicitly",
        );
        return;
    }
    let (index, member_path, member_size) = iso_members.remove(0);
    if member_path.len() > MAX_PATH_BYTES {
        add_unavailable(
            report,
            IdentityStatus::ResourceLimitReached,
            "ISO member path exceeds the path-length limit",
        );
        return;
    }
    let mut entry = match archive.by_index(index) {
        Ok(entry) => entry,
        Err(error) => {
            add_unavailable(report, IdentityStatus::Invalid, &error.to_string());
            return;
        }
    };
    let read_cap = match report.platform {
        IdentityPlatform::GameCube | IdentityPlatform::Wii => {
            member_size.min(DOLPHIN_HEADER_BYTES as u64)
        }
        IdentityPlatform::PlayStation2
        | IdentityPlatform::MegaDrive
        | IdentityPlatform::Snes
        | IdentityPlatform::Other => member_size.min(MAX_BYTES_READ),
    };
    let mut data = Vec::with_capacity(read_cap.min(usize::MAX as u64) as usize);
    if let Err(error) = entry.by_ref().take(read_cap).read_to_end(&mut data) {
        add_unavailable(
            report,
            IdentityStatus::Invalid,
            &format!("could not read ISO member: {error}"),
        );
        return;
    }
    report.bytes_read = data.len() as u64;
    let mut source = SliceSource {
        data: &data,
        declared_len: member_size,
        truncated: member_size > data.len() as u64,
    };
    inspect_iso_source(report, &mut source, Some(member_path), Some(index));
}

fn inspect_iso_source(
    report: &mut GameIdentityReport,
    source: &mut dyn ByteSource,
    member_path: Option<Vec<u8>>,
    member_index: Option<usize>,
) {
    match report.platform {
        IdentityPlatform::GameCube | IdentityPlatform::Wii => {
            inspect_dolphin_header(report, source, member_path, member_index)
        }
        IdentityPlatform::PlayStation2 => {
            inspect_ps2_iso(report, source, member_path, member_index)
        }
        IdentityPlatform::MegaDrive | IdentityPlatform::Snes | IdentityPlatform::Other => {}
    }
}

fn inspect_dolphin_header(
    report: &mut GameIdentityReport,
    source: &mut dyn ByteSource,
    member_path: Option<Vec<u8>>,
    member_index: Option<usize>,
) {
    let mut header = [0_u8; DOLPHIN_HEADER_BYTES];
    if let Err(error) = source.read_exact_at(0, &mut header) {
        let status = if error.kind() == io::ErrorKind::UnexpectedEof {
            IdentityStatus::Invalid
        } else {
            IdentityStatus::ResourceLimitReached
        };
        push_with_source(
            report,
            IdentityKind::DolphinGameId,
            status,
            None,
            IdentityConfidence::Unavailable,
            member_path,
            member_index,
            "bounded disc-header read",
            "disc header is truncated or unavailable",
        );
        return;
    }
    report.bytes_read = report.bytes_read.max(source.bytes_read());
    let has_gc_magic = header[GAMECUBE_MAGIC_OFFSET..GAMECUBE_MAGIC_OFFSET + 4] == GAMECUBE_MAGIC;
    let has_wii_magic = header[WII_MAGIC_OFFSET..WII_MAGIC_OFFSET + 4] == WII_MAGIC;
    let expected_magic = match report.platform {
        IdentityPlatform::GameCube => has_gc_magic,
        IdentityPlatform::Wii => has_wii_magic,
        _ => false,
    };
    if !expected_magic {
        push_with_source(
            report,
            IdentityKind::DolphinGameId,
            IdentityStatus::Invalid,
            None,
            IdentityConfidence::Unavailable,
            member_path,
            member_index,
            "reviewed disc-header magic",
            "disc magic does not match the selected platform",
        );
        return;
    }
    let id = &header[..6];
    if !id
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        push_with_source(
            report,
            IdentityKind::DolphinGameId,
            IdentityStatus::Invalid,
            None,
            IdentityConfidence::Unavailable,
            member_path,
            member_index,
            "disc-header bytes 0x00..0x06",
            "Game ID must contain six uppercase ASCII letters or digits",
        );
        return;
    }
    let id = String::from_utf8_lossy(id).into_owned();
    push_with_source(
        report,
        IdentityKind::DolphinGameId,
        IdentityStatus::Verified,
        Some(id),
        IdentityConfidence::ExactBytes,
        member_path.clone(),
        member_index,
        "disc-header bytes 0x00..0x06 with platform magic validation",
        "verified directly from the reviewed disc header",
    );
    push_with_source(
        report,
        IdentityKind::DolphinDiscNumber,
        IdentityStatus::Verified,
        Some(header[6].to_string()),
        IdentityConfidence::ExactBytes,
        member_path.clone(),
        member_index,
        "disc-header byte 0x06",
        "verified directly from the reviewed disc header",
    );
    let revision_status = if report.platform == IdentityPlatform::GameCube {
        IdentityStatus::Verified
    } else {
        IdentityStatus::Candidate
    };
    push_with_source(
        report,
        IdentityKind::DolphinRevision,
        revision_status,
        Some(header[7].to_string()),
        if revision_status == IdentityStatus::Verified {
            IdentityConfidence::ExactBytes
        } else {
            IdentityConfidence::StructuredMetadata
        },
        member_path.clone(),
        member_index,
        "outer disc-header byte 0x07",
        if revision_status == IdentityStatus::Verified {
            "verified GameCube revision"
        } else {
            "Wii outer-header revision is not promoted because Dolphin may use the game-partition header"
        },
    );
    push_with_source(
        report,
        IdentityKind::DolphinRegion,
        IdentityStatus::Verified,
        Some(char::from(header[3]).to_string()),
        IdentityConfidence::ExactBytes,
        member_path,
        member_index,
        "fourth Game ID byte",
        "raw region code byte; no locale name inferred",
    );
    report.complete = true;
}

fn inspect_ps2_iso(
    report: &mut GameIdentityReport,
    source: &mut dyn ByteSource,
    member_path: Option<Vec<u8>>,
    member_index: Option<usize>,
) {
    let root = match iso_root(source) {
        Ok(root) => root,
        Err((status, diagnostic)) => {
            push_with_source(
                report,
                IdentityKind::Ps2Serial,
                status,
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "ISO 9660 primary volume descriptor",
                &diagnostic,
            );
            return;
        }
    };
    report.metadata_paths_inspected += 1;
    let cnf = match find_iso_path(source, root, &[b"SYSTEM.CNF"]) {
        Ok(Some(record)) => record,
        Ok(None) => {
            push_with_source(
                report,
                IdentityKind::Ps2Serial,
                IdentityStatus::Missing,
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "ISO 9660 root directory lookup",
                "SYSTEM.CNF is missing",
            );
            return;
        }
        Err((status, diagnostic)) => {
            push_with_source(
                report,
                IdentityKind::Ps2Serial,
                status,
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "ISO 9660 root directory lookup",
                &diagnostic,
            );
            return;
        }
    };
    if cnf.size > MAX_SYSTEM_CNF_BYTES {
        push_with_source(
            report,
            IdentityKind::Ps2Serial,
            IdentityStatus::ResourceLimitReached,
            None,
            IdentityConfidence::Unavailable,
            member_path,
            member_index,
            "SYSTEM.CNF bounded read",
            "SYSTEM.CNF exceeds 64 KiB",
        );
        return;
    }
    let cnf_bytes = match read_iso_record(source, cnf) {
        Ok(bytes) => bytes,
        Err(error) => {
            push_with_source(
                report,
                IdentityKind::Ps2Serial,
                source_error_status(&error),
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "SYSTEM.CNF bounded read",
                &error.to_string(),
            );
            return;
        }
    };
    let boot = match parse_system_cnf_boot2(&cnf_bytes) {
        Ok(boot) => boot,
        Err(message) => {
            push_with_source(
                report,
                IdentityKind::Ps2Serial,
                IdentityStatus::Invalid,
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "SYSTEM.CNF BOOT2 assignment",
                &message,
            );
            return;
        }
    };
    let serial = match serial_from_boot_path(&boot) {
        Some(serial) => serial,
        None => {
            push_with_source(
                report,
                IdentityKind::Ps2Serial,
                IdentityStatus::Invalid,
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "SYSTEM.CNF BOOT2 executable name",
                "boot executable does not contain a valid PS2 product code",
            );
            return;
        }
    };
    push_with_source(
        report,
        IdentityKind::Ps2Serial,
        IdentityStatus::Verified,
        Some(serial),
        IdentityConfidence::StructuredMetadata,
        member_path.clone(),
        member_index,
        "SYSTEM.CNF BOOT2 on ISO 9660",
        "serial derived from the exact boot executable path, not an archive filename",
    );
    report.metadata_paths_inspected += 1;
    let components: Vec<&[u8]> = boot.split(|byte| *byte == b'\\' || *byte == b'/').collect();
    let executable = match find_iso_path(source, root, &components) {
        Ok(Some(record)) => record,
        Ok(None) => {
            push_with_source(
                report,
                IdentityKind::Pcsx2ExecutableCrc,
                IdentityStatus::Missing,
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "SYSTEM.CNF BOOT2 ISO lookup",
                "boot executable is missing",
            );
            return;
        }
        Err((status, diagnostic)) => {
            push_with_source(
                report,
                IdentityKind::Pcsx2ExecutableCrc,
                status,
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "SYSTEM.CNF BOOT2 ISO lookup",
                &diagnostic,
            );
            return;
        }
    };
    if executable.size > MAX_EXECUTABLE_BYTES {
        push_with_source(
            report,
            IdentityKind::Pcsx2ExecutableCrc,
            IdentityStatus::ResourceLimitReached,
            None,
            IdentityConfidence::Unavailable,
            member_path,
            member_index,
            "bounded boot executable read",
            "boot executable exceeds 32 MiB",
        );
        return;
    }
    let executable = match read_iso_record(source, executable) {
        Ok(bytes) => bytes,
        Err(error) => {
            push_with_source(
                report,
                IdentityKind::Pcsx2ExecutableCrc,
                source_error_status(&error),
                None,
                IdentityConfidence::Unavailable,
                member_path,
                member_index,
                "bounded boot executable read",
                &error.to_string(),
            );
            return;
        }
    };
    if executable.len() < 4 || executable[..4] != [0x7f, b'E', b'L', b'F'] {
        push_with_source(
            report,
            IdentityKind::Pcsx2ExecutableCrc,
            IdentityStatus::Invalid,
            None,
            IdentityConfidence::Unavailable,
            member_path,
            member_index,
            "ELF signature validation",
            "boot executable is not an ELF file",
        );
        return;
    }
    let crc = pcsx2_executable_crc(&executable);
    push_with_source(
        report,
        IdentityKind::Pcsx2ExecutableCrc,
        IdentityStatus::Verified,
        Some(format!("{crc:08X}")),
        IdentityConfidence::ExactBytes,
        member_path,
        member_index,
        "PCSX2 ELF word-XOR algorithm over exact executable bytes",
        "full bounded boot executable was read and hashed with the reviewed PCSX2 algorithm",
    );
    report.bytes_read = report.bytes_read.max(source.bytes_read());
    report.complete = true;
}

pub fn parse_system_cnf_boot2(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() as u64 > MAX_SYSTEM_CNF_BYTES {
        return Err("SYSTEM.CNF exceeds 64 KiB".to_string());
    }
    let mut result = None;
    for line in bytes.split(|byte| *byte == b'\n' || *byte == b'\r') {
        let line = trim_ascii(line);
        let Some(equals) = line.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        if !trim_ascii(&line[..equals]).eq_ignore_ascii_case(b"BOOT2") {
            continue;
        }
        if result.is_some() {
            return Err("SYSTEM.CNF contains multiple BOOT2 assignments".to_string());
        }
        let mut value = trim_ascii(&line[equals + 1..]);
        let lower: Vec<u8> = value.iter().map(u8::to_ascii_lowercase).collect();
        let prefix_len = if lower.starts_with(b"cdrom0:") {
            7
        } else if lower.starts_with(b"cdrom:") {
            6
        } else {
            return Err("BOOT2 must use cdrom: or cdrom0:".to_string());
        };
        value = &value[prefix_len..];
        while value
            .first()
            .is_some_and(|byte| *byte == b'/' || *byte == b'\\')
        {
            value = &value[1..];
        }
        if let Some(version) = value.iter().position(|byte| *byte == b';') {
            value = &value[..version];
        }
        if value.is_empty() || value.len() > MAX_PATH_BYTES {
            return Err("BOOT2 path is empty or exceeds 512 bytes".to_string());
        }
        if value
            .split(|byte| *byte == b'/' || *byte == b'\\')
            .any(|component| component.is_empty() || component == b"." || component == b"..")
        {
            return Err("BOOT2 path contains an empty or traversal component".to_string());
        }
        result = Some(value.to_vec());
    }
    result.ok_or_else(|| "SYSTEM.CNF has no BOOT2 assignment".to_string())
}

pub fn serial_from_boot_path(path: &[u8]) -> Option<String> {
    let name = path.rsplit(|byte| *byte == b'/' || *byte == b'\\').next()?;
    let name = std::str::from_utf8(name).ok()?.to_ascii_uppercase();
    let bytes = name.as_bytes();
    if bytes.len() < 11
        || !bytes[..4].iter().all(u8::is_ascii_alphanumeric)
        || !matches!(bytes[4], b'_' | b'-')
        || !bytes[5..8].iter().all(u8::is_ascii_digit)
        || bytes[8] != b'.'
        || !bytes[9..11].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some(format!("{}-{}{}", &name[..4], &name[5..8], &name[9..11]))
}

/// PCSX2's executable "CRC": XOR each complete little-endian 32-bit ELF word.
/// Trailing one-to-three bytes are intentionally ignored to match PCSX2.
pub fn pcsx2_executable_crc(bytes: &[u8]) -> u32 {
    bytes.chunks_exact(4).fold(0_u32, |crc, word| {
        crc ^ u32::from_le_bytes([word[0], word[1], word[2], word[3]])
    })
}

#[derive(Clone, Copy)]
struct IsoRecord {
    extent: u32,
    size: u64,
    directory: bool,
}

fn iso_root(source: &mut dyn ByteSource) -> Result<IsoRecord, (IdentityStatus, String)> {
    let mut sector = [0_u8; ISO_SECTOR_SIZE as usize];
    for descriptor in 0..MAX_ISO_DESCRIPTORS {
        let offset = (16 + descriptor as u64) * ISO_SECTOR_SIZE;
        source.read_exact_at(offset, &mut sector).map_err(|error| {
            (
                source_error_status(&error),
                format!("volume descriptor unavailable: {error}"),
            )
        })?;
        if &sector[1..6] != b"CD001" || sector[6] != 1 {
            return Err((
                IdentityStatus::Invalid,
                "invalid ISO 9660 volume descriptor".to_string(),
            ));
        }
        match sector[0] {
            1 => {
                return parse_iso_record(&sector[156..]).ok_or((
                    IdentityStatus::Invalid,
                    "invalid ISO root directory record".to_string(),
                ));
            }
            255 => break,
            _ => {}
        }
    }
    Err((
        IdentityStatus::Missing,
        "ISO 9660 primary volume descriptor not found".to_string(),
    ))
}

fn find_iso_path(
    source: &mut dyn ByteSource,
    mut directory: IsoRecord,
    components: &[&[u8]],
) -> Result<Option<IsoRecord>, (IdentityStatus, String)> {
    if components.is_empty() || components.len() > MAX_METADATA_PATHS {
        return Err((
            IdentityStatus::ResourceLimitReached,
            "metadata path-component limit reached".to_string(),
        ));
    }
    for (component_index, wanted) in components.iter().enumerate() {
        if wanted.is_empty() || wanted.len() > MAX_PATH_BYTES {
            return Err((
                IdentityStatus::Invalid,
                "invalid ISO path component".to_string(),
            ));
        }
        if directory.size > MAX_DIRECTORY_BYTES {
            return Err((
                IdentityStatus::ResourceLimitReached,
                "ISO directory exceeds 1 MiB".to_string(),
            ));
        }
        let data = read_iso_record(source, directory).map_err(|error| {
            (
                source_error_status(&error),
                format!("ISO directory read failed: {error}"),
            )
        })?;
        let mut offset = 0_usize;
        let mut entries = 0_usize;
        let mut found = None;
        while offset < data.len() {
            let length = data[offset] as usize;
            if length == 0 {
                offset = ((offset / ISO_SECTOR_SIZE as usize) + 1) * ISO_SECTOR_SIZE as usize;
                continue;
            }
            if offset + length > data.len() || length < 34 {
                return Err((
                    IdentityStatus::Invalid,
                    "malformed ISO directory record".to_string(),
                ));
            }
            entries += 1;
            if entries > MAX_ISO_DIRECTORY_ENTRIES {
                return Err((
                    IdentityStatus::ResourceLimitReached,
                    "ISO directory-entry limit reached".to_string(),
                ));
            }
            let record_bytes = &data[offset..offset + length];
            let name_len = record_bytes[32] as usize;
            if 33 + name_len > record_bytes.len() {
                return Err((
                    IdentityStatus::Invalid,
                    "malformed ISO filename".to_string(),
                ));
            }
            let name = strip_iso_version(&record_bytes[33..33 + name_len]);
            if name.eq_ignore_ascii_case(wanted) {
                found = Some(parse_iso_record(record_bytes).ok_or((
                    IdentityStatus::Invalid,
                    "unsupported or inconsistent ISO directory record".to_string(),
                ))?);
                break;
            }
            offset += length;
        }
        let Some(record) = found else { return Ok(None) };
        let last = component_index + 1 == components.len();
        if !last && !record.directory {
            return Ok(None);
        }
        directory = record;
    }
    Ok(Some(directory))
}

fn parse_iso_record(bytes: &[u8]) -> Option<IsoRecord> {
    let length = *bytes.first()? as usize;
    if length < 34 || bytes.len() < length {
        return None;
    }
    let extent = u32::from_le_bytes(bytes[2..6].try_into().ok()?);
    let size = u32::from_le_bytes(bytes[10..14].try_into().ok()?) as u64;
    if extent != u32::from_be_bytes(bytes[6..10].try_into().ok()?)
        || size != u64::from(u32::from_be_bytes(bytes[14..18].try_into().ok()?))
        || bytes[26] != 0
        || bytes[27] != 0
        || bytes[25] & 0x80 != 0
    {
        return None;
    }
    Some(IsoRecord {
        extent,
        size,
        directory: bytes[25] & 0x02 != 0,
    })
}

fn read_iso_record(source: &mut dyn ByteSource, record: IsoRecord) -> io::Result<Vec<u8>> {
    let offset = u64::from(record.extent)
        .checked_mul(ISO_SECTOR_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ISO extent overflow"))?;
    let end = offset
        .checked_add(record.size)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ISO record overflow"))?;
    if end > source.len() || record.size > usize::MAX as u64 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "ISO record is outside the readable image",
        ));
    }
    let mut bytes = vec![0; record.size as usize];
    source.read_exact_at(offset, &mut bytes)?;
    Ok(bytes)
}

trait ByteSource {
    fn len(&self) -> u64;
    fn bytes_read(&self) -> u64;
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()>;
}

struct FileSource {
    file: File,
    len: u64,
    bytes_read: u64,
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }
    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        if self.bytes_read.saturating_add(buffer.len() as u64) > MAX_BYTES_READ {
            return Err(io::Error::other("64 MiB identity read limit reached"));
        }
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read offset overflow"))?;
        if end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read exceeds image",
            ));
        }
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buffer)?;
        self.bytes_read += buffer.len() as u64;
        Ok(())
    }
}

struct SliceSource<'a> {
    data: &'a [u8],
    declared_len: u64,
    truncated: bool,
}

impl ByteSource for SliceSource<'_> {
    fn len(&self) -> u64 {
        self.declared_len
    }
    fn bytes_read(&self) -> u64 {
        self.data.len() as u64
    }
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "offset exceeds buffered ISO prefix",
            )
        })?;
        let end = start
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "read overflow"))?;
        if end > self.data.len() {
            let message = if self.truncated {
                "64 MiB ZIP member read limit reached"
            } else {
                "read exceeds image"
            };
            return Err(if self.truncated {
                io::Error::other(message)
            } else {
                io::Error::new(io::ErrorKind::UnexpectedEof, message)
            });
        }
        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }
}

fn open_read_only_regular(path: &Path) -> Result<File, String> {
    if !path.is_absolute() {
        return Err("identity path must be absolute".to_string());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::Normal(component) => current.push(component),
            _ => return Err("identity path contains a non-normal component".to_string()),
        }
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|error| format!("{}: {error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("symlink refused: {}", current.display()));
        }
    }
    let before = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !before.is_file() {
        return Err("identity source is not a regular file".to_string());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = file.metadata().map_err(|error| error.to_string())?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err("identity source changed while it was opened".to_string());
        }
    }
    Ok(file)
}

fn evidence(
    report: &GameIdentityReport,
    kind: IdentityKind,
    status: IdentityStatus,
    value: Option<String>,
    confidence: IdentityConfidence,
    diagnostic: &str,
    method: &str,
) -> IdentityEvidence {
    IdentityEvidence {
        kind,
        status,
        value,
        confidence,
        provenance: IdentityProvenance {
            archive_path: report.archive_path.clone(),
            member_path: None,
            member_index: None,
            method: method.to_string(),
        },
        diagnostic: diagnostic.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn push_with_source(
    report: &mut GameIdentityReport,
    kind: IdentityKind,
    status: IdentityStatus,
    value: Option<String>,
    confidence: IdentityConfidence,
    member_path: Option<Vec<u8>>,
    member_index: Option<usize>,
    method: &str,
    diagnostic: &str,
) {
    report.evidence.push(IdentityEvidence {
        kind,
        status,
        value,
        confidence,
        provenance: IdentityProvenance {
            archive_path: report.archive_path.clone(),
            member_path,
            member_index,
            method: method.to_string(),
        },
        diagnostic: diagnostic.to_string(),
    });
}

fn add_unavailable(report: &mut GameIdentityReport, status: IdentityStatus, diagnostic: &str) {
    let kinds: &[IdentityKind] = match report.platform {
        IdentityPlatform::PlayStation2 => {
            &[IdentityKind::Ps2Serial, IdentityKind::Pcsx2ExecutableCrc]
        }
        IdentityPlatform::GameCube | IdentityPlatform::Wii => {
            &[IdentityKind::DolphinGameId, IdentityKind::DolphinRevision]
        }
        IdentityPlatform::MegaDrive | IdentityPlatform::Snes | IdentityPlatform::Other => &[],
    };
    for kind in kinds {
        report.evidence.push(evidence(
            report,
            *kind,
            status,
            None,
            IdentityConfidence::Unavailable,
            diagnostic,
            "format and safety eligibility",
        ));
    }
}

fn add_filename_candidate(report: &mut GameIdentityReport) {
    let Some(stem) = report.archive_path.file_stem() else {
        return;
    };
    let stem = stem.to_string_lossy().to_ascii_uppercase();
    match report.platform {
        IdentityPlatform::GameCube | IdentityPlatform::Wii => {
            for token in stem.split(|character: char| !character.is_ascii_alphanumeric()) {
                if token.len() == 6
                    && token
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                {
                    report.evidence.push(evidence(
                        report,
                        IdentityKind::DolphinGameId,
                        IdentityStatus::Candidate,
                        Some(token.to_string()),
                        IdentityConfidence::FilenameOnly,
                        "archive filename is candidate evidence only",
                        "archive filename token",
                    ));
                    break;
                }
            }
        }
        IdentityPlatform::PlayStation2 => {
            let bytes = stem.as_bytes();
            for start in 0..bytes.len() {
                if let Some(serial) = bytes
                    .get(start..start.saturating_add(11))
                    .and_then(serial_from_boot_path)
                {
                    report.evidence.push(evidence(
                        report,
                        IdentityKind::Ps2Serial,
                        IdentityStatus::Candidate,
                        Some(serial),
                        IdentityConfidence::FilenameOnly,
                        "archive filename is candidate evidence only",
                        "archive filename token",
                    ));
                    break;
                }
            }
        }
        IdentityPlatform::MegaDrive | IdentityPlatform::Snes | IdentityPlatform::Other => {}
    }
}

fn source_error_status(error: &io::Error) -> IdentityStatus {
    if error.kind() == io::ErrorKind::Other {
        IdentityStatus::ResourceLimitReached
    } else {
        IdentityStatus::Invalid
    }
}

fn ascii_extension_is_iso(path: &[u8]) -> bool {
    let Some(name) = path.rsplit(|byte| *byte == b'/' || *byte == b'\\').next() else {
        return false;
    };
    let Some(dot) = name.iter().rposition(|byte| *byte == b'.') else {
        return false;
    };
    name[dot + 1..].eq_ignore_ascii_case(b"iso")
}

fn strip_iso_version(name: &[u8]) -> &[u8] {
    name.iter()
        .position(|byte| *byte == b';')
        .map_or(name, |position| &name[..position])
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use zip::CompressionMethod;
    use zip::write::{SimpleFileOptions, ZipWriter};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct FixtureDir(PathBuf);

    impl FixtureDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "archivefs-game-identity-{label}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for FixtureDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn dolphin_fixture(platform: IdentityPlatform, id: &[u8; 6], revision: u8) -> Vec<u8> {
        let mut bytes = vec![0_u8; DOLPHIN_HEADER_BYTES];
        bytes[..6].copy_from_slice(id);
        bytes[6] = 1;
        bytes[7] = revision;
        match platform {
            IdentityPlatform::GameCube => {
                bytes[GAMECUBE_MAGIC_OFFSET..][..4].copy_from_slice(&GAMECUBE_MAGIC)
            }
            IdentityPlatform::Wii => bytes[WII_MAGIC_OFFSET..][..4].copy_from_slice(&WII_MAGIC),
            _ => unreachable!(),
        }
        bytes
    }

    fn directory_record(name: &[u8], extent: u32, size: u32, directory: bool) -> Vec<u8> {
        let length = 33 + name.len() + usize::from(name.len().is_multiple_of(2));
        let mut record = vec![0_u8; length];
        record[0] = length as u8;
        record[2..6].copy_from_slice(&extent.to_le_bytes());
        record[6..10].copy_from_slice(&extent.to_be_bytes());
        record[10..14].copy_from_slice(&size.to_le_bytes());
        record[14..18].copy_from_slice(&size.to_be_bytes());
        record[25] = if directory { 2 } else { 0 };
        record[28..30].copy_from_slice(&1_u16.to_le_bytes());
        record[30..32].copy_from_slice(&1_u16.to_be_bytes());
        record[32] = name.len() as u8;
        record[33..33 + name.len()].copy_from_slice(name);
        record
    }

    fn ps2_iso(cnf: &[u8], include_elf: bool, declared_cnf_size: Option<u32>) -> Vec<u8> {
        const SECTORS: usize = 24;
        let mut iso = vec![0_u8; SECTORS * ISO_SECTOR_SIZE as usize];
        let pvd = 16 * ISO_SECTOR_SIZE as usize;
        iso[pvd] = 1;
        iso[pvd + 1..pvd + 6].copy_from_slice(b"CD001");
        iso[pvd + 6] = 1;
        let root = directory_record(&[0], 20, ISO_SECTOR_SIZE as u32, true);
        iso[pvd + 156..pvd + 156 + root.len()].copy_from_slice(&root);
        let terminator = 17 * ISO_SECTOR_SIZE as usize;
        iso[terminator] = 255;
        iso[terminator + 1..terminator + 6].copy_from_slice(b"CD001");
        iso[terminator + 6] = 1;

        let root_offset = 20 * ISO_SECTOR_SIZE as usize;
        let cnf_record = directory_record(
            b"SYSTEM.CNF;1",
            21,
            declared_cnf_size.unwrap_or(cnf.len() as u32),
            false,
        );
        iso[root_offset..root_offset + cnf_record.len()].copy_from_slice(&cnf_record);
        let mut cursor = root_offset + cnf_record.len();
        if include_elf {
            let elf_record = directory_record(b"SLUS_123.45;1", 22, 12, false);
            iso[cursor..cursor + elf_record.len()].copy_from_slice(&elf_record);
            cursor += elf_record.len();
            let elf_offset = 22 * ISO_SECTOR_SIZE as usize;
            iso[elf_offset..elf_offset + 12]
                .copy_from_slice(&[0x7f, b'E', b'L', b'F', 1, 2, 3, 4, 5, 6, 7, 8]);
        }
        iso[cursor] = 0;
        let cnf_offset = 21 * ISO_SECTOR_SIZE as usize;
        iso[cnf_offset..cnf_offset + cnf.len()].copy_from_slice(cnf);
        iso
    }

    fn write_fixture(directory: &FixtureDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = directory.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn parses_system_cnf_and_serial_without_filename_elevation() {
        let boot =
            parse_system_cnf_boot2(b"VER = 1.00\r\nBOOT2 = cdrom0:\\SLUS_123.45;1\r\n").unwrap();
        assert_eq!(boot, b"SLUS_123.45");
        assert_eq!(serial_from_boot_path(&boot).as_deref(), Some("SLUS-12345"));
    }

    #[test]
    fn traversal_boot_path_is_rejected() {
        assert!(parse_system_cnf_boot2(b"BOOT2=cdrom0:\\..\\SLUS_123.45;1").is_err());
    }

    #[test]
    fn pcsx2_crc_is_little_endian_word_xor_and_ignores_tail() {
        assert_eq!(
            pcsx2_executable_crc(&[1, 2, 3, 4, 5, 6, 7, 8, 9]),
            0x0c04_0404
        );
    }

    #[test]
    fn unsupported_filename_never_becomes_verified() {
        let report = inspect_game_identity(Path::new("/games/GAME01.chd"), Some("GameCube"));
        assert_eq!(report.verified_dolphin_game_id(), None);
        assert!(report.evidence.iter().any(|item| {
            item.kind == IdentityKind::DolphinGameId
                && item.status == IdentityStatus::Candidate
                && item.value.as_deref() == Some("GAME01")
        }));
        assert!(
            report
                .evidence
                .iter()
                .all(|item| item.kind != IdentityKind::DolphinGameId
                    || item.status != IdentityStatus::Verified)
        );
    }

    #[test]
    fn production_identity_reader_has_no_write_execution_or_network_path() {
        let production = include_str!("game_identity.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for forbidden in [
            "File::create",
            "fs::write",
            ".write(",
            "create_dir",
            "remove_dir",
            "Command::",
            "std::process",
            "TcpStream",
            "ureq::",
            "http://",
            "https://",
        ] {
            assert!(
                !production.contains(forbidden),
                "production identity reader contains forbidden path: {forbidden}"
            );
        }
    }

    #[test]
    fn verifies_gamecube_id_disc_number_and_revision() {
        let directory = FixtureDir::new("gamecube");
        let path = write_fixture(
            &directory,
            "not-an-id.iso",
            &dolphin_fixture(IdentityPlatform::GameCube, b"GM8E01", 3),
        );
        let report = inspect_game_identity(&path, Some("Nintendo GameCube"));
        assert_eq!(report.verified_dolphin_game_id(), Some("GM8E01"));
        assert_eq!(report.verified_dolphin_revision(), Some(3));
        assert_eq!(report.bytes_read, DOLPHIN_HEADER_BYTES as u64);
        assert!(report.complete);
    }

    #[test]
    fn verifies_wii_id_but_keeps_outer_revision_candidate() {
        let directory = FixtureDir::new("wii");
        let path = write_fixture(
            &directory,
            "wii.iso",
            &dolphin_fixture(IdentityPlatform::Wii, b"RMGE01", 7),
        );
        let report = inspect_game_identity(&path, Some("Wii"));
        assert_eq!(report.verified_dolphin_game_id(), Some("RMGE01"));
        assert_eq!(report.verified_dolphin_revision(), None);
        assert!(report.evidence.iter().any(|item| {
            item.kind == IdentityKind::DolphinRevision
                && item.status == IdentityStatus::Candidate
                && item.value.as_deref() == Some("7")
        }));
    }

    #[test]
    fn malformed_and_truncated_dolphin_headers_are_invalid() {
        let directory = FixtureDir::new("bad-header");
        let truncated = write_fixture(&directory, "short.iso", b"GM8E01");
        let malformed = write_fixture(&directory, "wrong.iso", &[0_u8; DOLPHIN_HEADER_BYTES]);
        for path in [truncated, malformed] {
            let report = inspect_game_identity(&path, Some("GameCube"));
            assert_eq!(report.verified_dolphin_game_id(), None);
            assert!(
                report
                    .evidence
                    .iter()
                    .any(|item| item.status == IdentityStatus::Invalid)
            );
        }
    }

    #[test]
    fn zip_with_one_iso_reads_only_the_dolphin_header() {
        let directory = FixtureDir::new("zip-iso");
        let path = directory.0.join("container.zip");
        let file = fs::File::create(&path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer
            .start_file(
                "disc.iso",
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
        let mut image = dolphin_fixture(IdentityPlatform::GameCube, b"GALE01", 2);
        image.resize(1024 * 1024, 0);
        writer.write_all(&image).unwrap();
        writer.finish().unwrap();

        let report = inspect_game_identity(&path, Some("GameCube"));
        assert_eq!(report.verified_dolphin_game_id(), Some("GALE01"));
        assert_eq!(report.bytes_read, DOLPHIN_HEADER_BYTES as u64);
        assert_eq!(report.archive_members_inspected, 1);
        assert_eq!(report.nested_container_depth, 1);
    }

    #[test]
    fn ps2_iso_verifies_serial_and_exact_executable_crc() {
        let directory = FixtureDir::new("ps2");
        let bytes = ps2_iso(b"BOOT2 = cdrom0:\\SLUS_123.45;1\r\n", true, None);
        let path = write_fixture(&directory, "unrelated.iso", &bytes);
        let report = inspect_game_identity(&path, Some("PlayStation 2"));
        assert_eq!(
            report.verified_value(IdentityKind::Ps2Serial),
            Some("SLUS-12345")
        );
        let expected = format!(
            "{:08X}",
            pcsx2_executable_crc(&bytes[22 * 2048..22 * 2048 + 12])
        );
        assert_eq!(report.verified_pcsx2_crc(), Some(expected.as_str()));
        assert!(report.complete);
    }

    #[test]
    fn missing_boot_executable_is_reported_without_crc() {
        let directory = FixtureDir::new("missing-elf");
        let path = write_fixture(
            &directory,
            "missing.iso",
            &ps2_iso(b"BOOT2=cdrom0:\\SLUS_123.45;1\n", false, None),
        );
        let report = inspect_game_identity(&path, Some("PS2"));
        assert_eq!(
            report.verified_value(IdentityKind::Ps2Serial),
            Some("SLUS-12345")
        );
        assert_eq!(report.verified_pcsx2_crc(), None);
        assert!(report.evidence.iter().any(|item| {
            item.kind == IdentityKind::Pcsx2ExecutableCrc && item.status == IdentityStatus::Missing
        }));
    }

    #[test]
    fn oversized_system_cnf_stops_at_the_declared_bound() {
        let directory = FixtureDir::new("large-cnf");
        let path = write_fixture(
            &directory,
            "large.iso",
            &ps2_iso(
                b"BOOT2=cdrom0:\\SLUS_123.45;1\n",
                true,
                Some(MAX_SYSTEM_CNF_BYTES as u32 + 1),
            ),
        );
        let report = inspect_game_identity(&path, Some("PS2"));
        assert!(report.evidence.iter().any(|item| {
            item.kind == IdentityKind::Ps2Serial
                && item.status == IdentityStatus::ResourceLimitReached
        }));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_is_refused_and_non_utf8_archive_path_is_preserved() {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let directory = FixtureDir::new("path-safety");
        let mut name = Vec::from(&b"game-"[..]);
        name.push(0xff);
        name.extend_from_slice(b".iso");
        let path = directory.0.join(std::ffi::OsString::from_vec(name));
        fs::write(
            &path,
            dolphin_fixture(IdentityPlatform::GameCube, b"GM8E01", 0),
        )
        .unwrap();
        let report = inspect_game_identity(&path, Some("GameCube"));
        assert_eq!(report.archive_path, path);
        assert_eq!(report.verified_dolphin_game_id(), Some("GM8E01"));

        let link = directory.0.join("link.iso");
        symlink(&path, &link).unwrap();
        let refused = inspect_game_identity(&link, Some("GameCube"));
        assert_eq!(refused.verified_dolphin_game_id(), None);
        assert!(
            refused
                .evidence
                .iter()
                .any(|item| item.diagnostic.contains("symlink refused"))
        );
    }

    #[test]
    fn mega_drive_loose_formats_receive_verified_local_byte_identity() {
        let directory = FixtureDir::new("loose-mega-drive");
        for extension in ["md", "gen", "smd"] {
            let bytes = format!("synthetic-{extension}-bytes").into_bytes();
            let path = write_fixture(
                &directory,
                &format!("Alien 3 (USA, Europe).{extension}"),
                &bytes,
            );
            let report = inspect_catalogued_game_identity(&path, Some("MegaDrive"));
            assert_eq!(report.platform, IdentityPlatform::MegaDrive);
            assert_eq!(report.format, IdentityImageFormat::LooseCartridgeRom);
            assert_eq!(report.bytes_read, bytes.len() as u64);
            let expected = sha256_hex(&bytes);
            assert_eq!(report.verified_loose_rom_sha256(), Some(expected.as_str()));
            assert!(report.complete);
            assert!(report.evidence.iter().any(|item| {
                item.kind == IdentityKind::LooseRomSha256
                    && item.diagnostic.contains("not a known-good dump claim")
            }));
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn contextual_bin_requires_trusted_exact_platform_evidence() {
        let directory = FixtureDir::new("loose-bin-context");
        let path = write_fixture(&directory, "Game.bin", b"bytes");
        let candidate = inspect_game_identity(&path, Some("MegaDrive"));
        assert_eq!(candidate.verified_loose_rom_sha256(), None);
        assert!(candidate.evidence.iter().any(|item| {
            item.kind == IdentityKind::LooseRomSha256 && item.status == IdentityStatus::Ambiguous
        }));

        let verified = inspect_catalogued_game_identity(&path, Some("MegaDrive"));
        let expected = sha256_hex(b"bytes");
        assert_eq!(
            verified.verified_loose_rom_sha256(),
            Some(expected.as_str())
        );
        let unrelated = inspect_catalogued_game_identity(&path, Some("SNES"));
        assert_eq!(unrelated.verified_loose_rom_sha256(), None);
    }

    #[test]
    fn snes_loose_formats_receive_verified_local_byte_identity() {
        let directory = FixtureDir::new("loose-snes");
        for extension in ["sfc", "smc"] {
            let bytes = format!("synthetic-{extension}-bytes").into_bytes();
            let path = write_fixture(&directory, &format!("Chrono Quest.{extension}"), &bytes);
            let report = inspect_catalogued_game_identity(&path, Some("SNES"));
            assert_eq!(report.platform, IdentityPlatform::Snes);
            let expected = sha256_hex(&bytes);
            assert_eq!(report.verified_loose_rom_sha256(), Some(expected.as_str()));
        }
    }

    #[test]
    fn oversized_loose_rom_fails_closed_without_hashing() {
        let directory = FixtureDir::new("loose-oversized");
        let path = directory.0.join("Too Large.md");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_LOOSE_ROM_BYTES + 1).unwrap();
        let report = inspect_catalogued_game_identity(&path, Some("MegaDrive"));
        assert_eq!(report.bytes_read, 0);
        assert_eq!(report.verified_loose_rom_sha256(), None);
        assert!(report.evidence.iter().any(|item| {
            item.kind == IdentityKind::LooseRomSha256
                && item.status == IdentityStatus::ResourceLimitReached
        }));
    }

    #[test]
    fn loose_rom_stability_check_rejects_file_mutation() {
        let directory = FixtureDir::new("loose-mutated");
        let path = write_fixture(&directory, "Changing.md", b"initial bytes");
        let file = OpenOptions::new().read(true).open(&path).unwrap();
        let before = StableFileMetadata::from_file(&file).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        let after = StableFileMetadata::from_file(&file).unwrap();
        assert!(!loose_rom_read_was_stable(&before, &after, before.len));
    }

    #[cfg(unix)]
    #[test]
    fn loose_rom_refuses_symlinked_parent_and_preserves_non_utf8_path() {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        let directory = FixtureDir::new("loose-paths");
        let real_parent = directory.0.join("real");
        fs::create_dir(&real_parent).unwrap();
        let link_parent = directory.0.join("linked");
        symlink(&real_parent, &link_parent).unwrap();
        let real = real_parent.join("Game.md");
        fs::write(&real, b"rom").unwrap();
        let refused =
            inspect_catalogued_game_identity(&link_parent.join("Game.md"), Some("MegaDrive"));
        assert_eq!(refused.verified_loose_rom_sha256(), None);

        let file_link = directory.0.join("linked-file.md");
        symlink(&real, &file_link).unwrap();
        let refused = inspect_catalogued_game_identity(&file_link, Some("MegaDrive"));
        assert_eq!(refused.verified_loose_rom_sha256(), None);

        let mut name = b"game-".to_vec();
        name.push(0xff);
        name.extend_from_slice(b".md");
        let path = directory.0.join(std::ffi::OsString::from_vec(name));
        fs::write(&path, b"non utf8 rom").unwrap();
        let report = inspect_catalogued_game_identity(&path, Some("MegaDrive"));
        assert_eq!(report.archive_path, path);
        assert!(report.verified_loose_rom_sha256().is_some());
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("path encoding"))
        );
        assert!(real.exists());
    }
}
