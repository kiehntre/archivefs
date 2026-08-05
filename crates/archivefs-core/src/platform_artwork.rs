//! Managed, local-only platform artwork overrides.
//!
//! Inputs are opened read-only and never altered. A validated static PNG,
//! JPEG or WebP is decoded under fixed limits, fitted without stretching onto
//! a transparent 1024px canvas, and atomically published as a metadata-free
//! PNG named from the exact canonical platform identifier.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use image::imageops::{FilterType, overlay};
use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::database::default_database_path;
use crate::platform::{PLATFORMS, platform_by_id};

pub const NORMALIZED_ARTWORK_SIZE: u32 = 1024;
pub const NORMALIZED_CONTENT_SIZE: u32 = 896;
pub const MAX_INPUT_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_INPUT_DIMENSION: u32 = 8192;
pub const MAX_INPUT_PIXELS: u64 = 40_000_000;
const CATEGORY_IDS: &[&str] = &[
    "console",
    "handheld",
    "computer",
    "arcade",
    "optical-disc",
    "cartridge",
];
const BUNDLED_PLATFORM_IDS: &[&str] = &[
    "3DO",
    "Acorn Archimedes",
    "Acorn Electron",
    "Amiga",
    "AmigaCD32",
    "Amstrad CPC",
    "Apple II",
    "Arcade",
    "Atari Jaguar",
    "Atari Lynx",
    "Atari2600",
    "Atari5200",
    "Atari7800",
    "AtariST",
    "BBC Micro",
    "ColecoVision",
    "Commodore 64",
    "Dreamcast",
    "Game Boy",
    "Game Boy Advance",
    "Game Boy Color",
    "GameCube",
    "GameGear",
    "MegaDrive",
    "N64",
    "NES",
    "Neo Geo Pocket",
    "NeoGeo",
    "Nintendo 3DS",
    "PS2",
    "PS3",
    "PSP",
    "PSX",
    "Philips CD-i",
    "PlayStation Vita",
    "SNES",
    "Saturn",
    "ScummVM",
    "Sega 32X",
    "Sharp X68000",
    "Switch",
    "TurboGrafx-16",
    "VIC-20",
    "Virtual Boy",
    "Wii",
    "WiiU",
    "WonderSwan Color",
    "Xbox",
    "Xbox360",
    "ZX Spectrum",
];
static ARTWORK_WRITE_LOCK: Mutex<()> = Mutex::new(());
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkInputFormat {
    Png,
    Jpeg,
    WebP,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkResolutionSource {
    Custom,
    Bundled,
    CustomCategoryFallback,
    BundledCategoryFallback,
    UnknownFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkImportResult {
    pub platform_id: String,
    pub canonical_filename: String,
    pub destination: PathBuf,
    pub input_format: ArtworkInputFormat,
    pub input_width: u32,
    pub input_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub replaced_existing: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkArtworkDisposition {
    Recognised,
    UnknownFilename,
    Invalid,
    DuplicateTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkArtworkEntry {
    pub source: PathBuf,
    pub platform_id: Option<String>,
    pub disposition: BulkArtworkDisposition,
    pub existing_custom: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkArtworkPreview {
    pub source_directory: PathBuf,
    pub dry_run: bool,
    pub entries: Vec<BulkArtworkEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkArtworkApplyResult {
    pub imported: Vec<ArtworkImportResult>,
    pub skipped: Vec<BulkArtworkEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidArtworkFile {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformArtworkStatus {
    pub root: PathBuf,
    pub total_canonical_platforms: usize,
    pub custom_images: usize,
    pub bundled_images: usize,
    pub fallback_only_platforms: usize,
    pub invalid_custom_files: Vec<InvalidArtworkFile>,
    pub unknown_files: Vec<PathBuf>,
    pub total_custom_disk_bytes: u64,
}

#[derive(Debug)]
pub struct PlatformArtworkError {
    pub kind: PlatformArtworkErrorKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformArtworkErrorKind {
    UnknownPlatform,
    UnsafePath,
    UnsupportedFormat,
    AnimatedImage,
    InvalidImage,
    LimitExceeded,
    ExistingCustom,
    ConfirmationRequired,
    Io,
}

impl PlatformArtworkError {
    fn new(kind: PlatformArtworkErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for PlatformArtworkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.detail)
    }
}

impl std::error::Error for PlatformArtworkError {}

pub fn default_platform_artwork_root() -> Result<PathBuf, PlatformArtworkError> {
    let database = default_database_path().map_err(|error| {
        PlatformArtworkError::new(PlatformArtworkErrorKind::UnsafePath, error.to_string())
    })?;
    Ok(database
        .parent()
        .expect("default database path has a parent")
        .join("platform-artwork"))
}

#[must_use]
pub fn canonical_artwork_stem(platform_id: &str) -> Option<String> {
    platform_by_id(platform_id).map(|platform| {
        platform
            .id
            .bytes()
            .filter(u8::is_ascii_alphanumeric)
            .map(|byte| byte.to_ascii_lowercase() as char)
            .collect()
    })
}

/// The canonical platforms this build ships artwork for.
///
/// Exposed so the GUI's own bundled table can be checked against it: the two are
/// maintained by hand and, if they drift, the status report claims a platform
/// has artwork that nothing can draw.
#[must_use]
pub fn bundled_platform_ids() -> &'static [&'static str] {
    BUNDLED_PLATFORM_IDS
}

#[must_use]
pub fn canonical_platform_for_stem(stem: &str) -> Option<&'static str> {
    PLATFORMS
        .iter()
        .find(|platform| canonical_artwork_stem(platform.id).as_deref() == Some(stem))
        .map(|platform| platform.id)
}

pub fn custom_artwork_path(
    root: &Path,
    platform_id: &str,
) -> Result<PathBuf, PlatformArtworkError> {
    let stem = canonical_artwork_stem(platform_id).ok_or_else(|| {
        PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnknownPlatform,
            format!("{platform_id:?} is not an exact canonical platform ID"),
        )
    })?;
    Ok(root.join(format!("{stem}.png")))
}

pub fn import_platform_artwork(
    root: &Path,
    platform_id: &str,
    source: &Path,
    replace: bool,
) -> Result<ArtworkImportResult, PlatformArtworkError> {
    let destination = custom_artwork_path(root, platform_id)?;
    let decoded = decode_source(source)?;
    let mut warnings = Vec::new();
    let (input_width, input_height) = decoded.image.dimensions();
    let max_fit = input_width.max(input_height).min(NORMALIZED_CONTENT_SIZE);
    let scale = max_fit as f64 / input_width.max(input_height) as f64;
    let width = ((input_width as f64 * scale).round() as u32).max(1);
    let height = ((input_height as f64 * scale).round() as u32).max(1);
    if input_width.max(input_height) < NORMALIZED_CONTENT_SIZE {
        warnings.push(format!(
            "Small source retained at {}x{}; ArchiveFS did not upscale it",
            input_width, input_height
        ));
    }
    let fitted = if width == input_width && height == input_height {
        decoded.image.into_rgba8()
    } else {
        decoded
            .image
            .resize_exact(width, height, FilterType::Lanczos3)
            .into_rgba8()
    };
    let mut canvas = RgbaImage::new(NORMALIZED_ARTWORK_SIZE, NORMALIZED_ARTWORK_SIZE);
    let x = i64::from((NORMALIZED_ARTWORK_SIZE - fitted.width()) / 2);
    let y = i64::from((NORMALIZED_ARTWORK_SIZE - fitted.height()) / 2);
    overlay(&mut canvas, &fitted, x, y);
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(canvas)
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|error| {
            PlatformArtworkError::new(PlatformArtworkErrorKind::Io, error.to_string())
        })?;

    let _guard = ARTWORK_WRITE_LOCK.lock().map_err(|_| {
        PlatformArtworkError::new(
            PlatformArtworkErrorKind::Io,
            "artwork writer lock is poisoned",
        )
    })?;
    validate_managed_root(root, true)?;
    reject_case_collision(
        root,
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap(),
    )?;
    let replaced_existing = match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(PlatformArtworkError::new(
                PlatformArtworkErrorKind::UnsafePath,
                format!(
                    "managed destination {} is not a direct regular file",
                    destination.display()
                ),
            ));
        }
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(io_error(&destination, error)),
    };
    if replaced_existing && !replace {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::ExistingCustom,
            format!(
                "custom artwork already exists at {}; replacement was not confirmed",
                destination.display()
            ),
        ));
    }
    atomic_publish(&destination, png.get_ref())?;
    Ok(ArtworkImportResult {
        platform_id: platform_id.to_owned(),
        canonical_filename: destination
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        destination,
        input_format: decoded.format,
        input_width,
        input_height,
        output_width: NORMALIZED_ARTWORK_SIZE,
        output_height: NORMALIZED_ARTWORK_SIZE,
        replaced_existing,
        warnings,
    })
}

pub fn remove_custom_platform_artwork(
    root: &Path,
    platform_id: &str,
    confirmed: bool,
) -> Result<bool, PlatformArtworkError> {
    if !confirmed {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::ConfirmationRequired,
            "removing custom artwork requires explicit confirmation",
        ));
    }
    validate_managed_root(root, false)?;
    let destination = custom_artwork_path(root, platform_id)?;
    let _guard = ARTWORK_WRITE_LOCK.lock().map_err(|_| {
        PlatformArtworkError::new(
            PlatformArtworkErrorKind::Io,
            "artwork writer lock is poisoned",
        )
    })?;
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(PlatformArtworkError::new(
                PlatformArtworkErrorKind::UnsafePath,
                format!(
                    "refusing to remove non-regular managed path {}",
                    destination.display()
                ),
            ))
        }
        Ok(_) => {
            fs::remove_file(&destination).map_err(|error| io_error(&destination, error))?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(&destination, error)),
    }
}

pub fn preview_import_folder(
    root: &Path,
    source_directory: &Path,
) -> Result<BulkArtworkPreview, PlatformArtworkError> {
    validate_managed_root(root, false)?;
    let metadata = fs::symlink_metadata(source_directory)
        .map_err(|error| io_error(source_directory, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnsafePath,
            "bulk import source must be a direct directory, not a symlink",
        ));
    }
    let mut paths = fs::read_dir(source_directory)
        .map_err(|error| io_error(source_directory, error))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    let mut targets = BTreeMap::<String, Vec<usize>>::new();
    let mut entries = Vec::new();
    for path in paths {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            entries.push(unknown_bulk(path));
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let extension = Path::new(&lower)
            .extension()
            .and_then(|value| value.to_str());
        let stem = Path::new(&lower)
            .file_stem()
            .and_then(|value| value.to_str());
        let Some(platform_id) = stem.and_then(canonical_platform_for_stem) else {
            entries.push(unknown_bulk(path));
            continue;
        };
        if !matches!(extension, Some("png" | "jpg" | "jpeg" | "webp")) || name != lower {
            entries.push(unknown_bulk(path));
            continue;
        }
        let existing_custom = custom_artwork_path(root, platform_id)?.exists();
        let index = entries.len();
        match inspect_source(&path) {
            Ok(_) => entries.push(BulkArtworkEntry {
                source: path,
                platform_id: Some(platform_id.to_owned()),
                disposition: BulkArtworkDisposition::Recognised,
                existing_custom,
                detail: "exact canonical filename and valid static image".to_owned(),
            }),
            Err(error) => entries.push(BulkArtworkEntry {
                source: path,
                platform_id: Some(platform_id.to_owned()),
                disposition: BulkArtworkDisposition::Invalid,
                existing_custom,
                detail: error.to_string(),
            }),
        }
        targets
            .entry(platform_id.to_owned())
            .or_default()
            .push(index);
    }
    for indexes in targets.values().filter(|indexes| indexes.len() > 1) {
        for &index in indexes {
            entries[index].disposition = BulkArtworkDisposition::DuplicateTarget;
            entries[index].detail =
                "more than one input targets this canonical platform".to_owned();
        }
    }
    Ok(BulkArtworkPreview {
        source_directory: source_directory.to_owned(),
        dry_run: true,
        entries,
    })
}

pub fn apply_import_folder(
    root: &Path,
    preview: &BulkArtworkPreview,
    replace_existing: bool,
) -> Result<BulkArtworkApplyResult, PlatformArtworkError> {
    let fresh = preview_import_folder(root, &preview.source_directory)?;
    let mut imported = Vec::new();
    let mut skipped = Vec::new();
    for entry in fresh.entries {
        if entry.disposition != BulkArtworkDisposition::Recognised
            || (entry.existing_custom && !replace_existing)
        {
            skipped.push(entry);
            continue;
        }
        imported.push(import_platform_artwork(
            root,
            entry
                .platform_id
                .as_deref()
                .expect("recognised entry has platform"),
            &entry.source,
            replace_existing,
        )?);
    }
    Ok(BulkArtworkApplyResult { imported, skipped })
}

pub fn inspect_platform_artwork(
    root: &Path,
) -> Result<PlatformArtworkStatus, PlatformArtworkError> {
    let mut custom = BTreeSet::new();
    let mut invalid = Vec::new();
    let mut unknown = Vec::new();
    let mut bytes = 0_u64;
    if root.exists() {
        validate_managed_root(root, false)?;
        for entry in fs::read_dir(root).map_err(|error| io_error(root, error))? {
            let path = entry.map_err(|error| io_error(root, error))?.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                unknown.push(path);
                continue;
            }
            bytes = bytes.saturating_add(metadata.len());
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                unknown.push(path);
                continue;
            };
            let Some(stem) = name.strip_suffix(".png") else {
                unknown.push(path);
                continue;
            };
            if canonical_platform_for_stem(stem).is_none() && !CATEGORY_IDS.contains(&stem) {
                unknown.push(path);
                continue;
            }
            match inspect_managed_png(&path) {
                Ok(_) => {
                    custom.insert(stem.to_owned());
                }
                Err(error) => invalid.push(InvalidArtworkFile {
                    path,
                    reason: error.to_string(),
                }),
            }
        }
    }
    invalid.sort_by(|left, right| left.path.cmp(&right.path));
    unknown.sort();
    let custom_platforms = custom
        .iter()
        .filter(|stem| canonical_platform_for_stem(stem).is_some())
        .count();
    let bundled_not_overridden = BUNDLED_PLATFORM_IDS
        .iter()
        .filter(|id| canonical_artwork_stem(id).is_some_and(|stem| !custom.contains(&stem)))
        .count();
    Ok(PlatformArtworkStatus {
        root: root.to_owned(),
        total_canonical_platforms: PLATFORMS.len(),
        custom_images: custom_platforms,
        bundled_images: bundled_not_overridden,
        fallback_only_platforms: PLATFORMS
            .len()
            .saturating_sub(custom_platforms + bundled_not_overridden),
        invalid_custom_files: invalid,
        unknown_files: unknown,
        total_custom_disk_bytes: bytes,
    })
}

struct DecodedSource {
    format: ArtworkInputFormat,
    image: DynamicImage,
}

fn inspect_source(path: &Path) -> Result<(ArtworkInputFormat, u32, u32), PlatformArtworkError> {
    let bytes = read_source(path)?;
    let format = detect_format(&bytes)?;
    reject_animation(&bytes, format)?;
    let image_format = image_format(format);
    let reader = ImageReader::with_format(Cursor::new(&bytes), image_format);
    let (width, height) = reader.into_dimensions().map_err(|error| {
        PlatformArtworkError::new(PlatformArtworkErrorKind::InvalidImage, error.to_string())
    })?;
    validate_dimensions(width, height)?;
    Ok((format, width, height))
}

fn inspect_managed_png(path: &Path) -> Result<(), PlatformArtworkError> {
    let (format, width, height) = inspect_source(path)?;
    if format != ArtworkInputFormat::Png {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnsupportedFormat,
            "managed artwork must be a normalized PNG",
        ));
    }
    if width > NORMALIZED_ARTWORK_SIZE || height > NORMALIZED_ARTWORK_SIZE {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::LimitExceeded,
            format!("managed PNG {width}x{height} exceeds the 1024x1024 renderer limit"),
        ));
    }
    Ok(())
}

fn decode_source(path: &Path) -> Result<DecodedSource, PlatformArtworkError> {
    let bytes = read_source(path)?;
    let format = detect_format(&bytes)?;
    reject_animation(&bytes, format)?;
    let reader = ImageReader::with_format(Cursor::new(&bytes), image_format(format));
    let (width, height) = reader.into_dimensions().map_err(|error| {
        PlatformArtworkError::new(PlatformArtworkErrorKind::InvalidImage, error.to_string())
    })?;
    validate_dimensions(width, height)?;
    let image =
        image::load_from_memory_with_format(&bytes, image_format(format)).map_err(|error| {
            PlatformArtworkError::new(PlatformArtworkErrorKind::InvalidImage, error.to_string())
        })?;
    Ok(DecodedSource { format, image })
}

fn read_source(path: &Path) -> Result<Vec<u8>, PlatformArtworkError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnsafePath,
            "image input must be a direct regular file; symlinks are refused",
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAX_INPUT_BYTES {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::LimitExceeded,
            format!(
                "image input size {} is outside 1..={MAX_INPUT_BYTES} bytes",
                metadata.len()
            ),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| io_error(path, error))?;
    let opened = file.metadata().map_err(|error| io_error(path, error))?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnsafePath,
            "image changed while it was opened",
        ));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        PlatformArtworkError::new(
            PlatformArtworkErrorKind::LimitExceeded,
            "image size does not fit memory limits",
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnsafePath,
            "image changed while it was read",
        ));
    }
    Ok(bytes)
}

fn detect_format(bytes: &[u8]) -> Result<ArtworkInputFormat, PlatformArtworkError> {
    match image::guess_format(bytes) {
        Ok(ImageFormat::Png) => Ok(ArtworkInputFormat::Png),
        Ok(ImageFormat::Jpeg) => Ok(ArtworkInputFormat::Jpeg),
        Ok(ImageFormat::WebP) => Ok(ArtworkInputFormat::WebP),
        Ok(other) => Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnsupportedFormat,
            format!("unsupported image format {other:?}"),
        )),
        Err(error) => Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::InvalidImage,
            format!("image magic could not be recognised: {error}"),
        )),
    }
}

fn image_format(format: ArtworkInputFormat) -> ImageFormat {
    match format {
        ArtworkInputFormat::Png => ImageFormat::Png,
        ArtworkInputFormat::Jpeg => ImageFormat::Jpeg,
        ArtworkInputFormat::WebP => ImageFormat::WebP,
    }
}

fn reject_animation(bytes: &[u8], format: ArtworkInputFormat) -> Result<(), PlatformArtworkError> {
    let animated = match format {
        ArtworkInputFormat::Png => png_contains_animation_chunk(bytes),
        ArtworkInputFormat::WebP => webp_contains_animation_chunk(bytes),
        ArtworkInputFormat::Jpeg => false,
    };
    if animated {
        Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::AnimatedImage,
            "animated platform artwork is not supported",
        ))
    } else {
        Ok(())
    }
}

fn png_contains_animation_chunk(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    let mut offset = 8_usize;
    while let Some(header_end) = offset.checked_add(8).filter(|end| *end <= bytes.len()) {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let kind = &bytes[offset + 4..header_end];
        if kind == b"acTL" {
            return true;
        }
        let Some(next) = header_end
            .checked_add(length)
            .and_then(|end| end.checked_add(4))
            .filter(|end| *end <= bytes.len())
        else {
            break;
        };
        offset = next;
        if kind == b"IEND" {
            break;
        }
    }
    false
}

fn webp_contains_animation_chunk(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return false;
    }
    let mut offset = 12_usize;
    while let Some(header_end) = offset.checked_add(8).filter(|end| *end <= bytes.len()) {
        let kind = &bytes[offset..offset + 4];
        if kind == b"ANIM" || kind == b"ANMF" {
            return true;
        }
        let length = u32::from_le_bytes(bytes[offset + 4..header_end].try_into().unwrap()) as usize;
        let padded = length.saturating_add(length & 1);
        let Some(next) = header_end
            .checked_add(padded)
            .filter(|end| *end <= bytes.len())
        else {
            break;
        };
        offset = next;
    }
    false
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), PlatformArtworkError> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| {
            PlatformArtworkError::new(
                PlatformArtworkErrorKind::LimitExceeded,
                "image dimensions overflow",
            )
        })?;
    if width == 0 || height == 0 {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::InvalidImage,
            "zero-sized image",
        ));
    }
    if width > MAX_INPUT_DIMENSION || height > MAX_INPUT_DIMENSION || pixels > MAX_INPUT_PIXELS {
        return Err(PlatformArtworkError::new(
            PlatformArtworkErrorKind::LimitExceeded,
            format!("image dimensions {width}x{height} exceed safe limits"),
        ));
    }
    Ok(())
}

fn atomic_publish(destination: &Path, bytes: &[u8]) -> Result<(), PlatformArtworkError> {
    let parent = destination.parent().ok_or_else(|| {
        PlatformArtworkError::new(
            PlatformArtworkErrorKind::UnsafePath,
            "managed destination has no parent",
        )
    })?;
    let temp = parent.join(format!(
        ".archivefs-artwork-{}-{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| io_error(&temp, error))?;
        file.write_all(bytes)
            .map_err(|error| io_error(&temp, error))?;
        file.sync_all().map_err(|error| io_error(&temp, error))?;
        fs::rename(&temp, destination).map_err(|error| io_error(destination, error))?;
        // Publication is already visible after the atomic rename. Directory
        // syncing improves crash durability on Linux, but platforms that do
        // not permit opening a directory must not produce a false failure
        // after the old custom image has already been replaced.
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn reject_case_collision(root: &Path, expected: &str) -> Result<(), PlatformArtworkError> {
    for entry in fs::read_dir(root).map_err(|error| io_error(root, error))? {
        let name = entry.map_err(|error| io_error(root, error))?.file_name();
        let name = name.to_string_lossy();
        if name.eq_ignore_ascii_case(expected) && name != expected {
            return Err(PlatformArtworkError::new(
                PlatformArtworkErrorKind::UnsafePath,
                format!("case-colliding managed filename {name:?} already exists"),
            ));
        }
    }
    Ok(())
}

fn validate_managed_root(root: &Path, create: bool) -> Result<(), PlatformArtworkError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(PlatformArtworkError::new(
                PlatformArtworkErrorKind::UnsafePath,
                format!(
                    "managed artwork root {} must be a direct directory",
                    root.display()
                ),
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir_all(root).map_err(|error| io_error(root, error))?;
            let metadata = fs::symlink_metadata(root).map_err(|error| io_error(root, error))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(PlatformArtworkError::new(
                    PlatformArtworkErrorKind::UnsafePath,
                    "managed artwork root was not created as a direct directory",
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(root, error)),
    }
}

fn unknown_bulk(path: PathBuf) -> BulkArtworkEntry {
    BulkArtworkEntry {
        source: path,
        platform_id: None,
        disposition: BulkArtworkDisposition::UnknownFilename,
        existing_custom: false,
        detail: "filename is not an exact lowercase canonical artwork filename".to_owned(),
    }
}

fn io_error(path: &Path, error: std::io::Error) -> PlatformArtworkError {
    PlatformArtworkError::new(
        PlatformArtworkErrorKind::Io,
        format!("{}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "archivefs-artwork-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn image_file(path: &Path, format: ImageFormat, width: u32, height: u32) {
        DynamicImage::ImageRgba8(ImageBuffer::from_pixel(
            width,
            height,
            Rgba([20, 40, 60, 255]),
        ))
        .save_with_format(path, format)
        .unwrap();
    }

    #[test]
    fn canonical_names_are_unique_and_exact() {
        let stems = PLATFORMS
            .iter()
            .map(|p| canonical_artwork_stem(p.id).unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(stems.len(), PLATFORMS.len());
        assert!(
            custom_artwork_path(Path::new("/tmp/a"), "PS2")
                .unwrap()
                .ends_with("ps2.png")
        );
        assert!(custom_artwork_path(Path::new("/tmp/a"), "playstation 2").is_err());
    }

    #[test]
    fn png_and_jpeg_import_to_square_without_touching_original() {
        for (extension, format) in [("png", ImageFormat::Png), ("jpg", ImageFormat::Jpeg)] {
            let temp = temp_dir(extension);
            let root = temp.join("managed");
            let source = temp.join(format!("source.{extension}"));
            image_file(&source, format, 400, 200);
            let before = fs::read(&source).unwrap();
            let result = import_platform_artwork(&root, "PS2", &source, false).unwrap();
            assert_eq!((result.output_width, result.output_height), (1024, 1024));
            assert_eq!(fs::read(&source).unwrap(), before);
            let output = image::open(result.destination).unwrap();
            assert_eq!(output.dimensions(), (1024, 1024));
            fs::remove_dir_all(temp).unwrap();
        }
    }

    #[test]
    fn failed_replacement_preserves_existing_custom() {
        let temp = temp_dir("replace");
        let root = temp.join("managed");
        let good = temp.join("good.png");
        let bad = temp.join("bad.png");
        image_file(&good, ImageFormat::Png, 100, 200);
        import_platform_artwork(&root, "PS2", &good, false).unwrap();
        let destination = root.join("ps2.png");
        let before = fs::read(&destination).unwrap();
        fs::write(&bad, b"not an image").unwrap();
        assert!(import_platform_artwork(&root, "PS2", &bad, true).is_err());
        assert_eq!(fs::read(destination).unwrap(), before);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn magic_not_extension_controls_explicit_import_and_existing_requires_replace() {
        let temp = temp_dir("magic");
        let root = temp.join("managed");
        let misleading = temp.join("dreamcatst.jpg");
        image_file(&misleading, ImageFormat::Png, 64, 32);
        let imported = import_platform_artwork(&root, "Dreamcast", &misleading, false).unwrap();
        assert_eq!(imported.input_format, ArtworkInputFormat::Png);
        assert_eq!(
            import_platform_artwork(&root, "Dreamcast", &misleading, false)
                .unwrap_err()
                .kind,
            PlatformArtworkErrorKind::ExistingCustom
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn bulk_preview_is_exact_dry_run_and_reports_unknown_and_duplicate() {
        let temp = temp_dir("bulk");
        let input = temp.join("input");
        let root = temp.join("managed");
        fs::create_dir_all(&input).unwrap();
        image_file(&input.join("ps2.png"), ImageFormat::Png, 10, 20);
        image_file(&input.join("ps2.jpg"), ImageFormat::Jpeg, 10, 20);
        image_file(&input.join("dreamcatst.png"), ImageFormat::Png, 10, 20);
        let preview = preview_import_folder(&root, &input).unwrap();
        assert!(preview.dry_run);
        assert!(!root.exists());
        assert_eq!(
            preview
                .entries
                .iter()
                .filter(|e| e.disposition == BulkArtworkDisposition::DuplicateTarget)
                .count(),
            2
        );
        assert_eq!(
            preview
                .entries
                .iter()
                .filter(|e| e.disposition == BulkArtworkDisposition::UnknownFilename)
                .count(),
            1
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn bulk_apply_and_rescan_are_bounded_and_keep_unknown_files() {
        let temp = temp_dir("bulk-apply");
        let input = temp.join("input");
        let root = temp.join("managed");
        fs::create_dir_all(&input).unwrap();
        image_file(&input.join("ps2.png"), ImageFormat::Png, 320, 200);
        fs::write(input.join("mystery.png"), b"not assigned").unwrap();
        let preview = preview_import_folder(&root, &input).unwrap();
        let applied = apply_import_folder(&root, &preview, false).unwrap();
        assert_eq!(applied.imported.len(), 1);
        assert_eq!(applied.skipped.len(), 1);
        fs::write(root.join("unknown-name.txt"), b"keep me").unwrap();
        let status = inspect_platform_artwork(&root).unwrap();
        assert_eq!(status.custom_images, 1);
        assert_eq!(status.unknown_files.len(), 1);
        assert!(root.join("unknown-name.txt").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn normalisation_preserves_aspect_ratio_and_centres_with_padding() {
        let temp = temp_dir("aspect");
        let root = temp.join("managed");
        let source = temp.join("wide.png");
        DynamicImage::ImageRgba8(ImageBuffer::from_pixel(1600, 400, Rgba([1, 2, 3, 255])))
            .save_with_format(&source, ImageFormat::Png)
            .unwrap();
        let result = import_platform_artwork(&root, "PS2", &source, false).unwrap();
        let output = image::open(result.destination).unwrap().into_rgba8();
        let visible = output
            .enumerate_pixels()
            .filter(|(_, _, pixel)| pixel[3] != 0)
            .map(|(x, y, _)| (x, y))
            .collect::<Vec<_>>();
        let min_x = visible.iter().map(|(x, _)| *x).min().unwrap();
        let max_x = visible.iter().map(|(x, _)| *x).max().unwrap();
        let min_y = visible.iter().map(|(_, y)| *y).min().unwrap();
        let max_y = visible.iter().map(|(_, y)| *y).max().unwrap();
        assert_eq!(max_x - min_x + 1, 896);
        assert_eq!(max_y - min_y + 1, 224);
        assert_eq!(min_x, 64);
        assert_eq!(min_y, 400);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn remove_requires_confirmation_and_reveals_default() {
        let temp = temp_dir("remove");
        let source = temp.join("input.png");
        let root = temp.join("managed");
        image_file(&source, ImageFormat::Png, 32, 32);
        import_platform_artwork(&root, "Dreamcast", &source, false).unwrap();
        assert!(remove_custom_platform_artwork(&root, "Dreamcast", false).is_err());
        assert!(remove_custom_platform_artwork(&root, "Dreamcast", true).unwrap());
        assert!(!root.join("dreamcast.png").exists());
        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inputs_and_case_collisions_are_refused() {
        use std::os::unix::fs::symlink;
        let temp = temp_dir("links");
        let source = temp.join("input.png");
        let link = temp.join("link.png");
        let root = temp.join("managed");
        image_file(&source, ImageFormat::Png, 32, 32);
        symlink(&source, &link).unwrap();
        assert_eq!(
            import_platform_artwork(&root, "PS2", &link, false)
                .unwrap_err()
                .kind,
            PlatformArtworkErrorKind::UnsafePath
        );
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("PS2.PNG"), b"x").unwrap();
        assert_eq!(
            import_platform_artwork(&root, "PS2", &source, false)
                .unwrap_err()
                .kind,
            PlatformArtworkErrorKind::UnsafePath
        );
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn animation_markers_and_malformed_images_are_refused() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&0_u32.to_be_bytes());
        png.extend_from_slice(b"acTL");
        png.extend_from_slice(&0_u32.to_be_bytes());
        assert_eq!(
            reject_animation(&png, ArtworkInputFormat::Png)
                .unwrap_err()
                .kind,
            PlatformArtworkErrorKind::AnimatedImage
        );
        let mut webp = b"RIFF\x0c\x00\x00\x00WEBP".to_vec();
        webp.extend_from_slice(b"ANMF\x00\x00\x00\x00");
        assert_eq!(
            reject_animation(&webp, ArtworkInputFormat::WebP)
                .unwrap_err()
                .kind,
            PlatformArtworkErrorKind::AnimatedImage
        );
        assert!(reject_animation(b"payload acTL but not a chunk", ArtworkInputFormat::Png).is_ok());
        assert!(detect_format(b"not an image").is_err());
        assert!(validate_dimensions(0, 1).is_err());
        assert!(validate_dimensions(8192, 8192).is_err());
    }
}
