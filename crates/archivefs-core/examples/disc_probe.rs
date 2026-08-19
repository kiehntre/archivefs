//! Read-only container -> media -> logical-filesystem probe.
//!
//! Reads exactly one, explicitly supplied file and walks as far down the
//! pipeline as this crate currently can:
//!
//! ```text
//! container -> media -> logical media reader -> filesystem/root tree
//!     -> boot/layout observations
//! ```
//!
//! For a plain `.iso`/`.bin` image, the whole pipeline runs: the file's own
//! bytes are the logical media, so [`archivefs_core::iso9660`] can observe
//! it directly via [`archivefs_core::logical_media::SliceMedia`].
//!
//! For a `.chd`, the full pipeline now runs too:
//! [`archivefs_core::chd_identity`] observes the CHD's own identity and
//! media facts and selects a candidate data track. For an ordinary
//! single-data-track disc, [`archivefs_core::chd_logical_media`] (pure
//! Rust) decodes that track's sectors on demand and exposes them as
//! [`archivefs_core::logical_media::LogicalMedia`]. For a Dreamcast GD-ROM
//! whose real game data lives beyond the low-density track the simple
//! selection reaches (detected via
//! `chd_identity::needs_specialist_optical_backend`), this probe instead
//! reaches for the optional
//! [`archivefs_core::chd_optical_specialist`] backend - if the
//! `chd-optical-specialist` feature was compiled in; otherwise it reports
//! that plainly rather than falling back to a wrong/incomplete answer. Any
//! step neither backend supports (a parent-required CHD, an unsupported
//! track type) is reported explicitly rather than guessed at.
//!
//! Nothing is ever written. The only filesystem call in this file is the
//! single read of the path given on the command line. No platform is ever
//! printed - every line here is a container, media, or filesystem fact.
//!
//! # Usage
//!
//! ```text
//! cargo run -p archivefs-core --example disc_probe -- /path/to/game.iso
//! cargo run -p archivefs-core --example disc_probe -- /path/to/game.chd
//! ```

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use archivefs_core::chd_identity::{
    ChdMetadataOutcome, looks_like_chd, needs_specialist_optical_backend, observe_chd_identity,
    select_candidate_data_track,
};
use archivefs_core::chd_logical_media::{ChdLogicalMediaError, open_chd_track_logical_media};
#[cfg(feature = "chd-optical-specialist")]
use archivefs_core::chd_optical_specialist::open_chd_optical_specialist;
use archivefs_core::dat::archive::hash::hash_member_stream;
use archivefs_core::dreamcast_boot_evidence::{IP_BIN_META_BYTES, parse_ip_bin_meta};
use archivefs_core::executable_signatures::looks_like_elf;
use archivefs_core::game_identity::MAX_SYSTEM_CNF_BYTES;
use archivefs_core::gamecube_wii_boot_evidence::{observe_gc_wii_disc, observe_gc_wii_evidence};
use archivefs_core::identity_source::hashing::FileFingerprint;
use archivefs_core::iso9660::{
    DiscFilesystemObservation, INTERESTING_ROOT_PATHS, find_path, looks_like_iso9660,
    observe_iso9660,
};
use archivefs_core::logical_media::{LogicalMedia, SliceMedia};
use archivefs_core::neogeocd_boot_evidence::{
    MAX_IPL_TXT_BYTES, observe_neogeocd_evidence, parse_ipl_txt,
};
use archivefs_core::param_sfo;
use archivefs_core::param_sfo::parse_param_sfo;
use archivefs_core::pcfx_boot_evidence::{
    PCFX_BOOT_SECTOR_BYTES, observe_pcfx_evidence, parse_pcfx_boot_sector,
};
use archivefs_core::playstation_boot_evidence::{
    PSX_EXECUTABLE_HEADER_BYTES, looks_like_psx_exe, parse_system_cnf_boot,
};
use archivefs_core::ps3_boot_evidence::PS3_LAYOUT_PATHS;
use archivefs_core::ps3_disc_evidence::{
    PKG_HEADER_BYTES, locate_ps3_game_dir, looks_like_pkg, observe_ps3_directory,
    observe_ps3_directory_evidence, parse_pkg_header, pkg_header_evidence,
};
use archivefs_core::psp_boot_evidence::PSP_LAYOUT_PATHS;
use archivefs_core::psp_pbp_evidence::{
    PBP_HEADER_BYTES, PBP_SECTION_ICON0_PNG, PBP_SECTION_PARAM_SFO, looks_like_pbp,
    observe_pbp_evidence, parse_pbp_header, validate_pbp_offsets,
};
use archivefs_core::raw_cd_logical_media::{looks_like_raw_cd, open_raw_cd_logical_media};
use archivefs_core::raw_cd_sector::{LOGICAL_BLOCK_BYTES, RAW_SECTOR_BYTES};
use archivefs_core::saturn_boot_evidence::{
    SATURN_SYSTEM_ID_BYTES, observe_saturn_evidence, parse_saturn_system_id,
};
use archivefs_core::segacd_boot_evidence::{
    looks_like_sega_cd_boot_sector, observe_segacd_evidence,
};
use archivefs_core::threedo_boot_evidence::{
    OPERA_HEADER_BYTES, observe_threedo_evidence, parse_opera_volume_header,
};
use archivefs_core::xbox_boot_evidence::{observe_xbox_disc, observe_xbox_evidence};
use archivefs_core::xbox360_boot_evidence::{observe_xbox360_disc, observe_xbox360_evidence};
use archivefs_core::xbox360_stfs_evidence::{
    STFS_HEADER_PEEK_BYTES, looks_like_stfs, observe_stfs_evidence, parse_stfs_header,
};
use archivefs_core::xdvdfs_signature::{XDVDFS_VOLUME_DESCRIPTOR_OFFSET, looks_like_xdvdfs};
use archivefs_core::xdvdfs_traversal::list_root;
use std::io::Read as _;

fn main() -> ExitCode {
    let mut hash_requested = false;
    let mut path_arg: Option<PathBuf> = None;
    for arg in env::args_os().skip(1) {
        if arg == "--hash" {
            hash_requested = true;
        } else if path_arg.is_none() {
            path_arg = Some(PathBuf::from(arg));
        } else {
            eprintln!(
                "usage: disc_probe [--hash] <path-to-iso-or-chd-or-pkg-or-ps3-folder>  (exactly one path)"
            );
            return ExitCode::FAILURE;
        }
    }
    let Some(path) = path_arg else {
        eprintln!("usage: disc_probe [--hash] <path-to-iso-or-chd-or-pkg-or-ps3-folder>");
        return ExitCode::FAILURE;
    };

    println!("Path: {}", path.display());

    if path.is_dir() {
        return probe_directory(&path);
    }

    // A cheap, bounded peek at just the fixed PKG header - a real .pkg can
    // be many gigabytes, so this must never trigger a whole-file read (see
    // probe_pkg below and the pkg_header module docs on why only this
    // fixed header is ever read).
    if let Ok(mut file) = std::fs::File::open(&path) {
        let mut peek = [0u8; PKG_HEADER_BYTES];
        if let Ok(read) = file.read(&mut peek)
            && looks_like_pkg(&peek[..read])
        {
            return probe_pkg(&peek[..read]);
        }
    }

    // Same bounded-peek discipline for Xbox 360 STFS packages - a real
    // package can be multiple gigabytes; only the fixed metadata header
    // prefix is ever read here.
    if let Ok(mut file) = std::fs::File::open(&path) {
        let mut peek = vec![0u8; STFS_HEADER_PEEK_BYTES];
        if let Ok(read) = file.read(&mut peek)
            && looks_like_stfs(&peek[..read.min(4)])
        {
            return probe_stfs(&peek[..read]);
        }
    }

    // Same bounded-peek discipline for EBOOT.PBP - a full PSN-game PBP's
    // DATA.PSAR section alone can be gigabytes; only the fixed header (plus
    // a small bounded PARAM.SFO read, itself bounded by
    // crate::param_sfo::MAX_SFO_BYTES) is ever read here.
    if let Ok(mut file) = std::fs::File::open(&path) {
        let mut peek = [0u8; PBP_HEADER_BYTES];
        if let Ok(read) = file.read(&mut peek)
            && looks_like_pbp(&peek[..read])
            && let Ok(metadata) = file.metadata()
        {
            return probe_pbp(&path, &peek[..read], metadata.len());
        }
    }

    let before = FileFingerprint::observe(&path);

    // The one and only whole-file filesystem access this program performs
    // (the .pkg case above deliberately avoids it).
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("could not read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    println!("File size: {} bytes", bytes.len());

    let after = FileFingerprint::observe(&path);
    let confirmed_unmodified = before.is_some() && before == after;
    println!(
        "Original modified: {}",
        if confirmed_unmodified {
            "NO"
        } else {
            "UNKNOWN (could not confirm the file was unchanged while it was being read)"
        }
    );

    // SHA-256 of a multi-gigabyte image (an uncompressed Xbox/Xbox 360 ISO
    // in particular) is slow and, for most probe runs, not what the caller
    // actually needs - so it is opt-in via --hash rather than mandatory.
    if hash_requested {
        match hash_bytes(&bytes) {
            Ok(hex) => println!("Physical SHA-256: {hex}"),
            Err(detail) => {
                eprintln!("failed to hash the physical bytes: {detail}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("Physical SHA-256: (skipped - pass --hash to compute; large images can be slow)");
    }

    if looks_like_chd(&bytes) {
        return probe_chd(&path, &bytes);
    }
    if looks_like_iso9660(&SliceMedia(&bytes)) {
        return probe_iso9660(&bytes);
    }
    // nod supports several GameCube/Wii container formats (plain ISO/GCM,
    // WIA/RVZ, WBFS, CISO, NFS, GCZ) with their own distinct on-disk magic
    // bytes - rather than duplicating nod's own format-sniffing here, just
    // try it and let a non-GC/Wii file fail closed on its own.
    if let Ok(observation) = observe_gc_wii_disc(&path) {
        return print_gc_wii_probe(observation);
    }
    if bytes.len() as u64 >= XDVDFS_VOLUME_DESCRIPTOR_OFFSET + 32 {
        let sector = &bytes[XDVDFS_VOLUME_DESCRIPTOR_OFFSET as usize..];
        if looks_like_xdvdfs(sector) {
            return probe_xdvdfs(&bytes);
        }
    }
    // Raw-sector optical images (.bin-style: a bare stream of 2352-byte CD
    // sectors) - checked before the raw-byte-offset-0 boot-signature checks
    // below, since those checks only make sense against *cooked* logical
    // bytes; a genuine raw image simply will not match them at offset 0
    // (sync/header bytes live there instead), so this ordering costs
    // nothing on a plain ISO/BIN either way.
    if looks_like_raw_cd(&bytes) {
        return probe_raw_cd(&bytes);
    }
    if bytes.len() >= SATURN_SYSTEM_ID_BYTES
        && parse_saturn_system_id(&bytes[..SATURN_SYSTEM_ID_BYTES])
            .is_some_and(|f| f.hardware_id_recognized)
    {
        return probe_saturn(&bytes);
    }
    if looks_like_sega_cd_boot_sector(&bytes) {
        return probe_segacd(&bytes);
    }
    if bytes.len() >= OPERA_HEADER_BYTES
        && parse_opera_volume_header(&bytes[..OPERA_HEADER_BYTES])
            .is_some_and(|fact| fact.header_is_valid())
    {
        return probe_threedo(&bytes);
    }
    if bytes.len() >= PCFX_BOOT_SECTOR_BYTES
        && parse_pcfx_boot_sector(&bytes[..PCFX_BOOT_SECTOR_BYTES]).any_magic_present()
    {
        return probe_pcfx(&bytes[..PCFX_BOOT_SECTOR_BYTES]);
    }

    println!("Container: Unknown (no recognised container/disc signature)");
    ExitCode::SUCCESS
}

/// Directory input: currently only a PS3 folder install (`PS3_GAME/` tree)
/// is a recognised directory shape - see the module-level policy in the
/// task this probe was extended under: "do not turn disc_probe into a
/// generic recursive scanner," so an unrelated directory is reported as
/// Unknown rather than scanned for anything else.
fn probe_directory(path: &Path) -> ExitCode {
    println!("Input type: directory");
    if locate_ps3_game_dir(path).is_none() {
        println!("Container: Unknown (directory does not resolve to a PS3_GAME layout)");
        return ExitCode::SUCCESS;
    }
    println!("Container: PS3 folder install");
    let observation = observe_ps3_directory(path);
    println!(
        "PS3_GAME present: {}",
        observation.layout.ps3_game_dir_present
    );
    println!("USRDIR present: {}", observation.layout.usrdir_present);
    println!(
        "EBOOT.BIN present: {}",
        observation.layout.eboot_bin_present
    );
    println!(
        "SELF magic present: {:?}",
        observation.layout.eboot_self_magic_present
    );
    println!(
        "PARAM.SFO present: {}",
        observation.layout.param_sfo.is_some()
    );
    println!(
        "TITLE_ID: {}",
        observation.layout.title_id().unwrap_or("N/A")
    );
    println!("TITLE: {}", observation.layout.title().unwrap_or("N/A"));
    println!(
        "CATEGORY: {}",
        observation.layout.category().unwrap_or("N/A")
    );
    println!(
        "APP_VER: {}",
        observation.layout.app_version().unwrap_or("N/A")
    );
    println!("PS3_DISC.SFB present: {}", observation.disc_sfb_present);
    println!(
        "Evidence: {:?}",
        observe_ps3_directory_evidence(&observation)
    );
    ExitCode::SUCCESS
}

/// A `.pkg`'s fixed [`PKG_HEADER_BYTES`]-byte header, read as a bounded
/// prefix by `main` - never the whole (potentially many-gigabyte) package.
fn probe_pkg(header: &[u8]) -> ExitCode {
    println!("Container: PS3/PSN .pkg package");
    match parse_pkg_header(header) {
        Some(fact) => {
            println!("Revision: {:#06x}", fact.revision);
            println!("Type: {:#06x}", fact.package_type);
            println!("Item count: {}", fact.item_count);
            println!("Total size: {} bytes", fact.total_size);
            println!("Data offset: {}", fact.data_offset);
            println!("Data size: {} bytes", fact.data_size);
            println!("Content ID: {}", fact.content_id);
            println!("Evidence: {:?}", pkg_header_evidence(&fact));
        }
        None => println!("PKG header did not parse (truncated?)"),
    }
    ExitCode::SUCCESS
}

/// `header` is only the fixed [`PBP_HEADER_BYTES`]-byte prefix `main`
/// already read; `total_len` is the real file's length (from
/// `fs::Metadata`, not a read) so [`validate_pbp_offsets`] can bound-check
/// the offset table without ever reading the (potentially gigabyte-sized)
/// `DATA.PSAR` section. The embedded `PARAM.SFO` is read separately, bounded
/// by [`crate::param_sfo::MAX_SFO_BYTES`], from `path` directly (only that
/// small section, never the whole file).
fn probe_pbp(path: &Path, header: &[u8], total_len: u64) -> ExitCode {
    println!("Container: PBP package (EBOOT.PBP)");
    let Some(fact) = parse_pbp_header(header) else {
        println!("PBP header did not parse (truncated?)");
        return ExitCode::SUCCESS;
    };
    println!("Version: {}", fact.version);
    match validate_pbp_offsets(&fact, total_len) {
        Ok(()) => println!("Offset table: valid"),
        Err(error) => {
            println!("Offset table: invalid ({error})");
            println!("Evidence: {:?}", observe_pbp_evidence(None));
            return ExitCode::SUCCESS;
        }
    }

    // Read only the PARAM.SFO section's declared byte range, directly from
    // the file via a seek + bounded read - never the whole (potentially
    // gigabyte-sized) PBP.
    let sfo_start = u64::from(fact.section_offsets[PBP_SECTION_PARAM_SFO]);
    let sfo_end = u64::from(fact.section_offsets[PBP_SECTION_ICON0_PNG]);
    let sfo = if sfo_end >= sfo_start && sfo_end - sfo_start <= param_sfo::MAX_SFO_BYTES as u64 {
        std::fs::File::open(path).ok().and_then(|mut file| {
            use std::io::Seek;
            let sfo_len = (sfo_end - sfo_start) as usize;
            let mut buf = vec![0u8; sfo_len];
            if file.seek(std::io::SeekFrom::Start(sfo_start)).is_ok()
                && file.read_exact(&mut buf).is_ok()
            {
                parse_param_sfo(&buf)
            } else {
                None
            }
        })
    } else {
        None
    };
    if let Some(sfo) = &sfo {
        println!("PARAM.SFO: {} entries", sfo.entries.len());
        if let Some(disc_id) = sfo.get_text("DISC_ID") {
            println!("DISC_ID: {disc_id}");
        }
    } else {
        println!("PARAM.SFO: unavailable (missing, malformed, or out of bounds)");
    }
    println!("Evidence: {:?}", observe_pbp_evidence(sfo.as_ref()));
    ExitCode::SUCCESS
}

/// A `.pkg`-style bounded prefix already read by `main` for an STFS
/// package's fixed metadata header - never the whole (potentially multi-
/// gigabyte) package.
fn probe_stfs(header: &[u8]) -> ExitCode {
    println!("Container: Xbox 360 STFS digital package");
    match parse_stfs_header(header) {
        Some(fact) => {
            println!("Signing variant: {}", fact.variant.magic_str());
            println!("Content type: {:#010x}", fact.content_type);
            println!("Title ID: {:08X}", fact.title_id);
            println!("Media ID: {:#010x}", fact.media_id);
            println!("Content size (declared): {} bytes", fact.content_size);
            println!("Display name: {}", fact.display_name);
            println!("Title name: {}", fact.title_name);
            println!(
                "Disc: {} of {}",
                fact.disc_number.max(1),
                fact.disc_in_set.max(1)
            );
            println!("Evidence: {:?}", observe_stfs_evidence(&fact));
        }
        None => println!("STFS header did not parse (truncated, or bad magic)"),
    }
    ExitCode::SUCCESS
}

/// A raw-sector optical image (.bin-style) that `looks_like_raw_cd`
/// already corroborated with two independently-matching sync sectors -
/// cooks it through [`RawCdLogicalMedia`] and reuses every existing
/// generic-over-[`LogicalMedia`] observer (ISO9660, Saturn/Sega CD/3DO/
/// PC-FX boot signatures) exactly as `probe_chd` already does for CHD-
/// backed media - no second ISO9660/boot-signature implementation.
fn probe_raw_cd(bytes: &[u8]) -> ExitCode {
    println!("Container: Raw optical sector image (bare 2352-byte-sector stream)");
    let media = match open_raw_cd_logical_media(bytes) {
        Ok(media) => media,
        Err(error) => {
            println!("Raw sector adapter: rejected ({error})");
            return ExitCode::SUCCESS;
        }
    };
    println!("Physical sector size: {RAW_SECTOR_BYTES} bytes");
    println!("Logical sector size: {LOGICAL_BLOCK_BYTES} bytes");
    println!("Raw mode: {}", media.sector_mode().label());
    println!("Sector count: {}", media.sector_count());
    println!("Source modified: NO (read-only adapter over already-loaded bytes)");

    if looks_like_iso9660(&media) {
        println!("Logical filesystem: ISO9660");
        match observe_iso9660(&media) {
            Ok(observation) => {
                print_iso9660_observation(&media, &observation);
                print_boot_evidence(&media, &observation);
            }
            Err(error) => {
                println!("Filesystem: Unsupported (ISO9660 structure did not parse: {error})")
            }
        }
        return ExitCode::SUCCESS;
    }
    println!("Logical filesystem: Unknown (no ISO9660 PVD at cooked sector 16)");

    print_raw_cd_boot_evidence(&media);
    ExitCode::SUCCESS
}

/// Re-runs this crate's existing raw-boot-signature checks (Saturn/Sega
/// CD/3DO/PC-FX) against the *cooked* first sector(s) of a raw-sector
/// image, the same checks `main` already runs against a plain file's raw
/// bytes at offset 0 - this is the one part of a raw-sector image that
/// those existing detectors cannot see without cooking first, since their
/// signatures sit at a cooked-byte offset, not a physical-byte offset.
fn print_raw_cd_boot_evidence<M: LogicalMedia>(media: &M) {
    let prefix_len = SATURN_SYSTEM_ID_BYTES
        .max(OPERA_HEADER_BYTES)
        .max(PCFX_BOOT_SECTOR_BYTES)
        .max(16);
    let mut prefix = vec![0u8; prefix_len.min(media.len() as usize)];
    if media.read_at(0, &mut prefix).is_err() {
        println!("Boot signature: N/A (could not read cooked sector 0)");
        return;
    }

    if prefix.len() >= SATURN_SYSTEM_ID_BYTES
        && let Some(fact) = parse_saturn_system_id(&prefix[..SATURN_SYSTEM_ID_BYTES])
        && fact.hardware_id_recognized
    {
        println!(
            "Boot signature: {} (Sega Saturn System ID)",
            fact.hardware_id
        );
        println!("Product code: {}", fact.product_number);
        println!("Version: {}", fact.version);
        println!("Region: {}", fact.area_symbols);
        println!("Game title: {}", fact.game_title);
        println!("Evidence: {:?}", observe_saturn_evidence(&fact));
        return;
    }
    if looks_like_sega_cd_boot_sector(&prefix) {
        println!("Boot signature: SEGADISCSYSTEM (Sega CD/Mega-CD)");
        println!("Evidence: {:?}", observe_segacd_evidence(&prefix));
        return;
    }
    if prefix.len() >= OPERA_HEADER_BYTES
        && let Some(fact) = parse_opera_volume_header(&prefix[..OPERA_HEADER_BYTES])
        && fact.header_is_valid()
    {
        println!("Boot signature: OperaFS (3DO)");
        println!("Volume label: {}", fact.volume_label);
        println!("Evidence: {:?}", observe_threedo_evidence(&fact));
        return;
    }
    if prefix.len() >= PCFX_BOOT_SECTOR_BYTES {
        let fact = parse_pcfx_boot_sector(&prefix[..PCFX_BOOT_SECTOR_BYTES]);
        if fact.any_magic_present() {
            println!("Boot signature: PC-FX boot sector");
            println!("Evidence: {:?}", observe_pcfx_evidence(&fact));
            return;
        }
    }
    println!("Boot signature: N/A (no recognised boot structure at cooked sector 0)");
}

fn probe_threedo(bytes: &[u8]) -> ExitCode {
    println!("Container: 3DO (Opera filesystem)");
    if let Some(fact) = parse_opera_volume_header(bytes) {
        println!("Volume label: {}", fact.volume_label);
        println!("Volume comment: {}", fact.volume_comment);
        println!("Block size: {}", fact.block_size);
        println!("Block count: {}", fact.block_count);
        println!("Evidence: {:?}", observe_threedo_evidence(&fact));
    }
    ExitCode::SUCCESS
}

fn probe_pcfx(sector: &[u8]) -> ExitCode {
    println!("Container: PC-FX boot sector");
    let fact = parse_pcfx_boot_sector(sector);
    println!("Primary magic present: {}", fact.primary_magic_present);
    println!("Secondary magic present: {}", fact.secondary_magic_present);
    println!("Evidence: {:?}", observe_pcfx_evidence(&fact));
    ExitCode::SUCCESS
}

fn print_gc_wii_probe(
    observation: archivefs_core::gamecube_wii_boot_evidence::GcWiiDiscObservation,
) -> ExitCode {
    println!("Container: GameCube/Wii disc image");
    println!(
        "Media: {}",
        if observation.is_wii {
            "Wii optical disc"
        } else {
            "GameCube optical disc"
        }
    );
    println!("Filesystem: nod-managed GameCube/Wii disc structure");
    println!("Product code: {}", observation.game_id);
    println!(
        "Version: disc {} version {}",
        observation.disc_num, observation.disc_version
    );
    println!("Game title: {}", observation.game_title);
    println!("Partitions: {}", observation.partitions.len());
    for partition in &observation.partitions {
        println!("  [{}] {}", partition.index, partition.kind);
    }
    match &observation.data_partition_meta {
        Some(meta) => {
            println!("main.dol present: {}", meta.main_dol_present);
            println!(
                "FST present: {} (root entries: {:?})",
                meta.fst_present, meta.fst_root_entry_count
            );
            println!("Apploader date: {:?}", meta.apploader_date);
        }
        None => println!("Data partition metadata: unavailable"),
    }
    println!("Evidence: {:?}", observe_gc_wii_evidence(&observation));
    ExitCode::SUCCESS
}

fn probe_xdvdfs(bytes: &[u8]) -> ExitCode {
    println!("Container: XDVDFS volume (Xbox/Xbox 360 family)");
    println!(
        "Filesystem: XDVDFS (magic verified, bounded traversal - see xdvdfs_traversal module docs)"
    );

    match list_root(bytes) {
        Ok(root) => {
            println!(
                "XDVDFS root entries: {} (truncated: {})",
                root.entries.len(),
                root.truncated
            );
            for entry in root.entries.iter().take(50) {
                println!(
                    "  {} ({}, size={})",
                    entry.name,
                    if entry.is_directory { "dir" } else { "file" },
                    entry.size
                );
            }
        }
        Err(error) => println!("XDVDFS root listing: unavailable ({error})"),
    }

    let xbox = observe_xbox_disc(bytes);
    println!("default.xbe present: {}", xbox.default_xbe_present);
    println!("XBE header parsed: {}", xbox.xbe_header.is_some());
    if let Some(header) = &xbox.xbe_header {
        println!("  title_id: {:?}", header.title_id);
        println!("  title_name: {:?}", header.title_name);
    }
    println!("Xbox evidence: {:?}", observe_xbox_evidence(&xbox));

    let xbox360 = observe_xbox360_disc(bytes);
    println!("default.xex present: {}", xbox360.default_xex_present);
    println!("XEX2 header parsed: {}", xbox360.xex2_header.is_some());
    if let Some(header) = &xbox360.xex2_header {
        println!("  media_id: {}", header.media_id);
        println!("  title_id: {}", header.title_id);
    }
    println!(
        "Xbox 360 evidence: {:?}",
        observe_xbox360_evidence(&xbox360)
    );

    ExitCode::SUCCESS
}

fn probe_saturn(bytes: &[u8]) -> ExitCode {
    println!("Container: Sega Saturn boot header");
    if let Some(fact) = parse_saturn_system_id(&bytes[..SATURN_SYSTEM_ID_BYTES]) {
        println!("Boot signature: {}", fact.hardware_id);
        println!("Product code: {}", fact.product_number);
        println!("Version: {}", fact.version);
        println!("Region: {}", fact.area_symbols);
        println!("Game title: {}", fact.game_title);
        println!("Evidence: {:?}", observe_saturn_evidence(&fact));
    }
    ExitCode::SUCCESS
}

fn probe_segacd(bytes: &[u8]) -> ExitCode {
    println!("Container: Sega CD / Mega-CD boot sector");
    println!("Boot signature: SEGADISCSYSTEM");
    println!("Evidence: {:?}", observe_segacd_evidence(bytes));
    ExitCode::SUCCESS
}

fn probe_chd(path: &Path, bytes: &[u8]) -> ExitCode {
    println!("Container: CHD");

    let observation = match observe_chd_identity(bytes) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("CHD header did not parse: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut needs_specialist = false;
    match &observation.metadata {
        ChdMetadataOutcome::Empty => println!("Media: Unknown (no metadata chain)"),
        ChdMetadataOutcome::Malformed(error) => {
            println!("Media: Unknown (metadata chain malformed: {error})");
        }
        ChdMetadataOutcome::Observed(metadata) => {
            let classes = metadata.media_classes();
            println!(
                "Media: {}",
                if classes.is_empty() {
                    "Unknown".to_string()
                } else {
                    format!("{classes:?}")
                }
            );

            match select_candidate_data_track(metadata) {
                Some(candidate) => println!(
                    "Data track: track {} (type={}, media={:?}) - conservative metadata-only selection, audio tracks excluded",
                    candidate.track, candidate.track_type, candidate.media_class
                ),
                None => println!(
                    "Data track: none identified (all-audio, or no CD/GD-ROM track metadata)"
                ),
            }

            needs_specialist = needs_specialist_optical_backend(metadata);
            if needs_specialist {
                println!(
                    "High-density data: YES - this GD-ROM's real data lives beyond the \
                     low-density track the simple reader selects"
                );
            }
        }
    }

    if needs_specialist {
        return probe_chd_specialist(path);
    }

    let media = match open_chd_track_logical_media(bytes) {
        Ok(media) => {
            println!("Logical reader: OK (pure-Rust)");
            media
        }
        Err(error) => {
            println!("Logical reader: {}", logical_reader_outcome(&error));
            println!("Filesystem: Unknown (no logical reader available)");
            print_interesting_paths_unavailable();
            return ExitCode::SUCCESS;
        }
    };

    if !looks_like_iso9660(&media) {
        println!(
            "Filesystem: Unknown (logical data does not begin with an ISO9660 CD001 identifier)"
        );
        print_interesting_paths_unavailable();
        return ExitCode::SUCCESS;
    }

    match observe_iso9660(&media) {
        Ok(observation) => {
            print_iso9660_observation(&media, &observation);
            print_boot_evidence(&media, &observation);
        }
        Err(error) => {
            println!("Filesystem: Unsupported (ISO9660 structure did not parse: {error})");
            print_interesting_paths_unavailable();
        }
    }

    ExitCode::SUCCESS
}

#[cfg(feature = "chd-optical-specialist")]
fn probe_chd_specialist(path: &Path) -> ExitCode {
    let media = match open_chd_optical_specialist(path) {
        Ok(media) => {
            println!("Logical reader: OK (specialist optical backend)");
            media
        }
        Err(error) => {
            println!("Logical reader: Malformed (specialist backend: {error})");
            println!("Filesystem: Unknown (no logical reader available)");
            print_interesting_paths_unavailable();
            return ExitCode::SUCCESS;
        }
    };

    if !looks_like_iso9660(&media) {
        println!(
            "Filesystem: Unknown (logical data does not begin with an ISO9660 CD001 identifier)"
        );
        print_interesting_paths_unavailable();
        return ExitCode::SUCCESS;
    }

    match observe_iso9660(&media) {
        Ok(observation) => {
            print_iso9660_observation(&media, &observation);
            print_boot_evidence(&media, &observation);
        }
        Err(error) => {
            println!("Filesystem: Unsupported (ISO9660 structure did not parse: {error})");
            print_interesting_paths_unavailable();
        }
    }

    ExitCode::SUCCESS
}

#[cfg(not(feature = "chd-optical-specialist"))]
fn probe_chd_specialist(_path: &Path) -> ExitCode {
    println!(
        "Logical reader: Unsupported (this CHD needs the specialist optical backend, which is \
         not compiled into this build - rebuild with --features chd-optical-specialist)"
    );
    println!("Filesystem: Unknown (specialist backend not available)");
    print_interesting_paths_unavailable();
    ExitCode::SUCCESS
}

fn logical_reader_outcome(error: &ChdLogicalMediaError) -> String {
    match error {
        ChdLogicalMediaError::NeedsParent { parent_sha1 } => {
            format!(
                "NeedsParent (combined SHA-1 {})",
                parent_sha1
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )
        }
        ChdLogicalMediaError::NoDataTrack
        | ChdLogicalMediaError::UnsupportedTrackType { .. }
        | ChdLogicalMediaError::UnsupportedTrackPosition { .. }
        | ChdLogicalMediaError::UnsupportedPregap { .. } => format!("Unsupported ({error})"),
        ChdLogicalMediaError::Header(_) | ChdLogicalMediaError::Codec { .. } => {
            format!("Malformed ({error})")
        }
    }
}

fn print_interesting_paths_unavailable() {
    println!("Volume ID: N/A");
    println!("Root entries: N/A");
    for path in INTERESTING_ROOT_PATHS {
        println!("  {path}: N/A");
    }
}

fn print_iso9660_observation<M: LogicalMedia>(media: &M, observation: &DiscFilesystemObservation) {
    println!("Filesystem: ISO9660");
    println!("Volume ID: {}", observation.volume_identifier);
    println!("Root entries: {}", observation.root_entries.len());
    for entry in &observation.root_entries {
        println!(
            "  {} ({}, size={})",
            entry.original_name,
            if entry.is_directory { "dir" } else { "file" },
            entry.size
        );
    }
    for path in INTERESTING_ROOT_PATHS {
        let exists = matches!(find_path(media, observation, path), Ok(Some(_)));
        println!("  {path}: {}", if exists { "YES" } else { "NO" });
    }
}

/// Prints neutral internal boot/release facts - never a platform decision.
/// `SYSTEM.CNF` is looked up through the filesystem; `IP.BIN` is read
/// directly from `media` at offset 0 (it is not a filesystem entry - see
/// [`archivefs_core::dreamcast_boot_evidence`]'s module documentation).
/// A disc with neither present prints `N/A` for every field; a disc with
/// only one populates only that backend's fields.
fn print_boot_evidence<M: LogicalMedia>(media: &M, observation: &DiscFilesystemObservation) {
    let mut boot_file = "N/A".to_string();
    let mut boot_target = "N/A".to_string();
    let mut serial_candidate = "N/A".to_string();
    let mut executable_magic = "N/A".to_string();
    let mut boot_signature = "N/A".to_string();
    let mut version = "N/A".to_string();
    let mut region = "N/A".to_string();

    if let Ok(Some(entry)) = find_path(media, observation, "SYSTEM.CNF")
        && !entry.is_directory
        && entry.size as u64 <= MAX_SYSTEM_CNF_BYTES
    {
        let offset = entry.extent_lba as u64 * observation.logical_block_size as u64;
        let mut buf = vec![0u8; entry.size as usize];
        if media.read_at(offset, &mut buf).is_ok() {
            boot_file = "SYSTEM.CNF".to_string();
            if let Some(fact) = parse_system_cnf_boot(&buf) {
                boot_target = format!("{}={}", fact.boot_key, fact.raw_value);
                if let Some(serial) = &fact.serial_candidate {
                    serial_candidate = serial.clone();
                }
                if let Some(exec_path) = &fact.executable_path
                    && let Ok(Some(exec_entry)) = find_path(media, observation, exec_path)
                    && !exec_entry.is_directory
                {
                    let header_len = (exec_entry.size as usize).min(PSX_EXECUTABLE_HEADER_BYTES);
                    let exec_offset =
                        exec_entry.extent_lba as u64 * observation.logical_block_size as u64;
                    let mut header = vec![0u8; header_len];
                    executable_magic = if media.read_at(exec_offset, &mut header).is_ok() {
                        if looks_like_psx_exe(&header) {
                            "PS-X EXE"
                        } else if looks_like_elf(&header) {
                            "ELF"
                        } else {
                            "NO"
                        }
                        .to_string()
                    } else {
                        "N/A (could not read executable header)".to_string()
                    };
                } else {
                    executable_magic = "NO (executable not found)".to_string();
                }
            }
        }
    }

    let mut ip_bin = vec![0u8; IP_BIN_META_BYTES];
    if media.read_at(0, &mut ip_bin).is_ok()
        && let Some(fact) = parse_ip_bin_meta(&ip_bin)
    {
        if fact.hardware_id_recognized {
            boot_signature = fact.hardware_id.clone();
            if serial_candidate == "N/A" && !fact.product_number.is_empty() {
                serial_candidate = fact.product_number.clone();
            }
        }
        if !fact.product_version.is_empty() {
            version = fact.product_version.clone();
        }
        if !fact.area_symbols.is_empty() {
            region = fact.area_symbols.clone();
        }
        if boot_file == "N/A" && !fact.boot_filename.is_empty() {
            boot_file = format!("{} (declared in IP.BIN)", fact.boot_filename);
        }
    }

    // Saturn/Sega CD/3DO/PC-FX all place their own boot signature at
    // logical offset 0, the same convention IP.BIN above already reads -
    // checked only if Dreamcast's own IP.BIN check above did not already
    // claim `boot_signature`, matching this function's existing "one
    // backend populates its own fields" discipline. This is what lets a
    // Saturn/3DO disc surface its real boot evidence even when its
    // filesystem also happens to be plain ISO9660 (as most Saturn/3DO
    // discs' data area is) and so was already routed through the generic
    // ISO9660 path above rather than a dedicated raw-signature probe.
    if boot_signature == "N/A" {
        let mut prefix = vec![
            0u8;
            SATURN_SYSTEM_ID_BYTES
                .max(OPERA_HEADER_BYTES)
                .max(PCFX_BOOT_SECTOR_BYTES)
                .min(media.len() as usize)
        ];
        if media.read_at(0, &mut prefix).is_ok() {
            if prefix.len() >= SATURN_SYSTEM_ID_BYTES
                && let Some(fact) = parse_saturn_system_id(&prefix[..SATURN_SYSTEM_ID_BYTES])
                && fact.hardware_id_recognized
            {
                boot_signature = fact.hardware_id.clone();
                if serial_candidate == "N/A" && !fact.product_number.is_empty() {
                    serial_candidate = fact.product_number.clone();
                }
                if !fact.version.is_empty() {
                    version = fact.version.clone();
                }
                if !fact.area_symbols.is_empty() {
                    region = fact.area_symbols.clone();
                }
                if boot_file == "N/A" && !fact.game_title.is_empty() {
                    boot_file = format!("{} (declared in Saturn System ID)", fact.game_title);
                }
            } else if looks_like_sega_cd_boot_sector(&prefix) {
                boot_signature = "SEGADISCSYSTEM".to_string();
            } else if prefix.len() >= OPERA_HEADER_BYTES
                && let Some(fact) = parse_opera_volume_header(&prefix[..OPERA_HEADER_BYTES])
                && fact.header_is_valid()
            {
                boot_signature = "OperaFS".to_string();
                if boot_file == "N/A" && !fact.volume_label.is_empty() {
                    boot_file = format!("{} (declared in Opera volume header)", fact.volume_label);
                }
            } else if prefix.len() >= PCFX_BOOT_SECTOR_BYTES {
                let fact = parse_pcfx_boot_sector(&prefix[..PCFX_BOOT_SECTOR_BYTES]);
                if fact.primary_magic_present {
                    boot_signature = "PC-FX:Hu_CD-ROM".to_string();
                } else if fact.secondary_magic_present {
                    boot_signature = "PPPPHHHHOOOOTTTTOOOO____CCCCDDDD".to_string();
                }
            }
        }
    }

    // PSP_GAME / PS3_GAME layout + PARAM.SFO (independent of the SYSTEM.CNF
    // check above - a disc uses at most one of these conventions, but this
    // probe checks each honestly rather than assuming).
    let mut sony_layout_paths = Vec::new();
    for path in PSP_LAYOUT_PATHS.iter().chain(PS3_LAYOUT_PATHS.iter()) {
        let exists = matches!(find_path(media, observation, path), Ok(Some(_)));
        sony_layout_paths.push((*path, exists));
    }
    for sfo_path in ["PSP_GAME/PARAM.SFO", "PS3_GAME/PARAM.SFO"] {
        if let Ok(Some(entry)) = find_path(media, observation, sfo_path)
            && !entry.is_directory
        {
            let offset = entry.extent_lba as u64 * observation.logical_block_size as u64;
            let mut buf = vec![0u8; entry.size as usize];
            if media.read_at(offset, &mut buf).is_ok()
                && let Some(sfo) = parse_param_sfo(&buf)
            {
                let id = sfo.get_text("DISC_ID").or_else(|| sfo.get_text("TITLE_ID"));
                if let Some(id) = id
                    && serial_candidate == "N/A"
                {
                    serial_candidate = id.to_string();
                }
                println!("PARAM.SFO ({sfo_path}): {} entries", sfo.entries.len());
            }
        }
    }

    // Neo Geo CD: IPL.TXT lives inside a plain ISO9660 filesystem, so - like
    // the Sony PARAM.SFO paths above - it is looked up here rather than at
    // a raw byte offset. ISO9660 alone is never Neo Geo CD evidence; the
    // manifest's own structural validity is the real signal.
    if let Ok(Some(entry)) = find_path(media, observation, "IPL.TXT")
        && !entry.is_directory
    {
        let bound = (entry.size as usize).min(MAX_IPL_TXT_BYTES);
        let offset = entry.extent_lba as u64 * observation.logical_block_size as u64;
        let mut buf = vec![0u8; bound];
        if media.read_at(offset, &mut buf).is_ok() {
            let fact = parse_ipl_txt(&buf);
            println!(
                "IPL.TXT: {} entries, required extensions present: {}",
                fact.entries.len(),
                fact.has_required_extensions()
            );
            println!(
                "Neo Geo CD evidence: {:?}",
                observe_neogeocd_evidence(&fact)
            );
        }
    }

    println!("Boot file: {boot_file}");
    println!("Boot target: {boot_target}");
    println!("Serial/product candidate: {serial_candidate}");
    println!("Executable magic: {executable_magic}");
    println!("Boot signature: {boot_signature}");
    println!("Version: {version}");
    println!("Region: {region}");
    for (path, exists) in &sony_layout_paths {
        if *exists {
            println!("  {path}: YES");
        }
    }
}

fn probe_iso9660(bytes: &[u8]) -> ExitCode {
    println!("Container: raw logical image (plain ISO9660 byte stream)");
    println!("Media: N/A (no CHD container to report media facts for)");
    println!("Data track(s): N/A (single logical byte stream, not a multi-track container)");

    let media = SliceMedia(bytes);
    let observation = match observe_iso9660(&media) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("ISO9660 structure did not parse: {error}");
            return ExitCode::FAILURE;
        }
    };

    print_iso9660_observation(&media, &observation);
    print_boot_evidence(&media, &observation);
    ExitCode::SUCCESS
}

/// SHA-256 of an in-memory buffer, via the crate's existing
/// [`hash_member_stream`] (the same helper the other probes already use)
/// rather than a new hashing implementation.
fn hash_bytes(data: &[u8]) -> Result<String, String> {
    hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
        .map(|hashed| hashed.hashes.sha256)
        .map_err(|error| format!("{error:?}"))
}
