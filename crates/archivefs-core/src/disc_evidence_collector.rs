//! Batch 11: the smallest reusable, read-only disc-evidence collector -
//! milestone section 22.
//!
//! Batch 10 deliberately did not port `examples/disc_probe.rs`'s
//! evidence-gathering into `library_plan_probe`, because that logic lived
//! only inside an *example* (which cannot be imported by anything else)
//! and was entangled with ~250 lines of printing. This module is the fix:
//! [`collect_disc_boot_evidence`] is the same evidence-gathering logic
//! `disc_probe.rs`'s own `print_boot_evidence` performs - same functions,
//! same order, same real facts - lifted into the crate library so both
//! `disc_probe` and `library_plan_probe` (and anything else) can call it,
//! with no duplication. `disc_probe.rs` itself is expected to switch to
//! calling this directly in a future pass; this batch adds the collector
//! and wires the *new* consumer (`library_plan_probe`) through it without
//! touching the already-reviewed example.
//!
//! # What this covers
//!
//! Everything `print_boot_evidence` covers: PS1/PS2 `SYSTEM.CNF`,
//! Dreamcast `IP.BIN`, Saturn/Sega CD/3DO/PC-FX raw boot signatures at
//! logical offset 0, PSP/PS3 `PARAM.SFO` layout, and Neo Geo CD `IPL.TXT` -
//! all read through the [`crate::logical_media::LogicalMedia`] abstraction,
//! never a raw byte-offset guess of its own.
//!
//! # What this does not cover (disclosed, not silently missing)
//!
//! The *container* dispatch above the filesystem layer - CHD selection/
//! specialist-backend routing, raw-CD-sector detection, XDVDFS, GameCube/
//! Wii (`nod`) - stays in `disc_probe.rs` for now; [`collect_chd_evidence`]
//! and [`collect_plain_iso_evidence`] cover the two most common real-corpus
//! shapes (a CHD, and a plain ISO9660 image under a safety size bound) so
//! this batch's real-corpus validation has real disc coverage, but Xbox/
//! Xbox 360/GameCube/Wii container dispatch is not yet reachable through
//! this collector - see this batch's final report for why (mostly: real
//! samples for those formats are multi-gigabyte, and a bounded reader for
//! them does not exist yet either in this collector or in `disc_probe`
//! itself, which already does a full-file read for those cases).
//!
//! # Large-disc safety (milestone section 24)
//!
//! [`collect_chd_evidence`] refuses (does not read) any file above
//! [`MAX_CHD_BYTES`]; [`collect_plain_iso_evidence`] takes an explicit
//! caller-supplied byte cap and refuses above it. Neither function ever
//! reads a file "just to see how big it might get" - the size check comes
//! first, from filesystem metadata alone.

use std::path::Path;

use crate::chd_identity::{
    ChdMetadataOutcome, looks_like_chd, needs_specialist_optical_backend, observe_chd_identity,
    select_candidate_data_track,
};
use crate::chd_logical_media::open_chd_track_logical_media;
use crate::content_detector::ContentDetector as _;
use crate::content_evidence::ContentEvidence;
use crate::dreamcast_boot_evidence::{
    IP_BIN_META_BYTES, observe_ip_bin_evidence, parse_ip_bin_meta,
};
use crate::executable_signatures::looks_like_elf;
use crate::game_identity::MAX_SYSTEM_CNF_BYTES;
use crate::iso9660::{DiscFilesystemObservation, find_path, looks_like_iso9660, observe_iso9660};
use crate::logical_media::{LogicalMedia, SliceMedia};
use crate::neogeocd_boot_evidence::{MAX_IPL_TXT_BYTES, observe_neogeocd_evidence, parse_ipl_txt};
use crate::param_sfo::parse_param_sfo;
use crate::pcfx_boot_evidence::{
    PCFX_BOOT_SECTOR_BYTES, observe_pcfx_evidence, parse_pcfx_boot_sector,
};
use crate::playstation_boot_evidence::{
    PSX_EXECUTABLE_HEADER_BYTES, PsxExeDetector, looks_like_psx_exe, observe_system_cnf_evidence,
    parse_system_cnf_boot,
};
use crate::ps2_boot_evidence::{observe_ps2_boot, observe_ps2_evidence, parse_ps2_system_cnf};
use crate::ps3_boot_evidence::PS3_LAYOUT_PATHS;
use crate::psp_boot_evidence::{PSP_LAYOUT_PATHS, PspLayoutObservation, observe_psp_evidence};
use crate::saturn_boot_evidence::{
    SATURN_SYSTEM_ID_BYTES, observe_saturn_evidence, parse_saturn_system_id,
};
use crate::segacd_boot_evidence::{looks_like_sega_cd_boot_sector, observe_segacd_evidence};
use crate::threedo_boot_evidence::{
    OPERA_HEADER_BYTES, observe_threedo_evidence, parse_opera_volume_header,
};

/// The largest `.chd` this collector will read - real single-disc CD-ROM
/// CHDs (the common real-corpus case: PS1/Saturn/Sega CD/3DO) compress to a
/// few hundred MB; a DVD-based CHD can be larger. Chosen as a conservative
/// bound well below what would risk this crate's own documented "a heavy
/// read operation destabilized a shared machine" incident class, not a
/// format limitation.
pub const MAX_CHD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Why a disc collection attempt produced no evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscCollectionRefusal {
    NotReadable(String),
    TooLarge { bytes: u64, maximum: u64 },
    NotRecognizedContainer,
    ChdHeaderDidNotParse(String),
    NoLogicalReaderAvailable,
    NotIso9660,
    Iso9660DidNotParse(String),
}

/// Collects real, structural [`ContentEvidence`] from a `.chd`'s decoded
/// logical media - the pure-Rust single/simple-track path only (no
/// specialist optical backend; a CHD [`needs_specialist_optical_backend`]
/// is refused rather than silently read incompletely).
pub fn collect_chd_evidence(path: &Path) -> Result<Vec<ContentEvidence>, DiscCollectionRefusal> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| DiscCollectionRefusal::NotReadable(error.to_string()))?;
    if metadata.len() > MAX_CHD_BYTES {
        return Err(DiscCollectionRefusal::TooLarge {
            bytes: metadata.len(),
            maximum: MAX_CHD_BYTES,
        });
    }
    let bytes = std::fs::read(path)
        .map_err(|error| DiscCollectionRefusal::NotReadable(error.to_string()))?;
    if !looks_like_chd(&bytes) {
        return Err(DiscCollectionRefusal::NotRecognizedContainer);
    }
    let observation = observe_chd_identity(&bytes)
        .map_err(|error| DiscCollectionRefusal::ChdHeaderDidNotParse(error.to_string()))?;
    let ChdMetadataOutcome::Observed(chd_metadata) = &observation.metadata else {
        return Err(DiscCollectionRefusal::NoLogicalReaderAvailable);
    };
    if needs_specialist_optical_backend(chd_metadata) {
        return Err(DiscCollectionRefusal::NoLogicalReaderAvailable);
    }
    let _ = select_candidate_data_track(chd_metadata);
    let media = open_chd_track_logical_media(&bytes)
        .map_err(|_| DiscCollectionRefusal::NoLogicalReaderAvailable)?;
    if !looks_like_iso9660(&media) {
        return Err(DiscCollectionRefusal::NotIso9660);
    }
    let filesystem = observe_iso9660(&media)
        .map_err(|error| DiscCollectionRefusal::Iso9660DidNotParse(error.to_string()))?;
    Ok(collect_disc_boot_evidence(&media, &filesystem))
}

/// Collects evidence from a plain, uncompressed ISO9660 image - refuses
/// anything above `max_bytes` before ever reading it (milestone section
/// 24). `max_bytes` is the caller's choice: this collector applies no
/// default of its own, so a caller cannot forget to set one and get a
/// silent unbounded read.
pub fn collect_plain_iso_evidence(
    path: &Path,
    max_bytes: u64,
) -> Result<Vec<ContentEvidence>, DiscCollectionRefusal> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| DiscCollectionRefusal::NotReadable(error.to_string()))?;
    if metadata.len() > max_bytes {
        return Err(DiscCollectionRefusal::TooLarge {
            bytes: metadata.len(),
            maximum: max_bytes,
        });
    }
    let bytes = std::fs::read(path)
        .map_err(|error| DiscCollectionRefusal::NotReadable(error.to_string()))?;
    let media = SliceMedia(&bytes);
    if !looks_like_iso9660(&media) {
        return Err(DiscCollectionRefusal::NotIso9660);
    }
    let filesystem = observe_iso9660(&media)
        .map_err(|error| DiscCollectionRefusal::Iso9660DidNotParse(error.to_string()))?;
    Ok(collect_disc_boot_evidence(&media, &filesystem))
}

/// The reused core: identical evidence-gathering to `disc_probe.rs`'s own
/// `print_boot_evidence`, with all printing removed. See the module doc
/// comment for exactly what this covers.
pub fn collect_disc_boot_evidence<M: LogicalMedia>(
    media: &M,
    observation: &DiscFilesystemObservation,
) -> Vec<ContentEvidence> {
    let mut evidence: Vec<ContentEvidence> = Vec::new();

    if let Ok(Some(entry)) = find_path(media, observation, "SYSTEM.CNF")
        && !entry.is_directory
        && entry.size as u64 <= MAX_SYSTEM_CNF_BYTES
    {
        let offset = entry.extent_lba as u64 * observation.logical_block_size as u64;
        let mut buf = vec![0u8; entry.size as usize];
        if media.read_at(offset, &mut buf).is_ok()
            && let Some(fact) = parse_system_cnf_boot(&buf)
        {
            let mut exec_header: Option<Vec<u8>> = None;
            if let Some(exec_path) = &fact.executable_path
                && let Ok(Some(exec_entry)) = find_path(media, observation, exec_path)
                && !exec_entry.is_directory
            {
                let header_len = (exec_entry.size as usize).min(PSX_EXECUTABLE_HEADER_BYTES);
                let exec_offset =
                    exec_entry.extent_lba as u64 * observation.logical_block_size as u64;
                let mut header = vec![0u8; header_len];
                if media.read_at(exec_offset, &mut header).is_ok() {
                    exec_header = Some(header);
                }
            }
            if fact.boot_key == "BOOT2" {
                if let Some(ps2_fact) = parse_ps2_system_cnf(&buf) {
                    let ps2_observation = observe_ps2_boot(ps2_fact, exec_header.as_deref());
                    evidence.extend(observe_ps2_evidence(&ps2_observation));
                }
            } else {
                evidence.extend(observe_system_cnf_evidence(&fact));
                if let Some(header) = &exec_header {
                    let _ = looks_like_elf(header) || looks_like_psx_exe(header);
                    evidence.extend(PsxExeDetector.detect(header).evidence().to_vec());
                }
            }
        }
    }

    let mut ip_bin = vec![0u8; IP_BIN_META_BYTES];
    let mut boot_signature_found = false;
    if media.read_at(0, &mut ip_bin).is_ok()
        && let Some(fact) = parse_ip_bin_meta(&ip_bin)
    {
        if fact.hardware_id_recognized {
            boot_signature_found = true;
        }
        evidence.extend(observe_ip_bin_evidence(&fact));
    }

    if !boot_signature_found {
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
                evidence.extend(observe_saturn_evidence(&fact));
            } else if looks_like_sega_cd_boot_sector(&prefix) {
                evidence.extend(observe_segacd_evidence(&prefix));
            } else if prefix.len() >= OPERA_HEADER_BYTES
                && let Some(fact) = parse_opera_volume_header(&prefix[..OPERA_HEADER_BYTES])
                && fact.header_is_valid()
            {
                evidence.extend(observe_threedo_evidence(&fact));
            } else if prefix.len() >= PCFX_BOOT_SECTOR_BYTES {
                let fact = parse_pcfx_boot_sector(&prefix[..PCFX_BOOT_SECTOR_BYTES]);
                evidence.extend(observe_pcfx_evidence(&fact));
            }
        }
    }

    let mut psp_layout = PspLayoutObservation::default();
    for sfo_path in ["PSP_GAME/PARAM.SFO", "PS3_GAME/PARAM.SFO"] {
        if let Ok(Some(entry)) = find_path(media, observation, sfo_path)
            && !entry.is_directory
        {
            let offset = entry.extent_lba as u64 * observation.logical_block_size as u64;
            let mut buf = vec![0u8; entry.size as usize];
            if media.read_at(offset, &mut buf).is_ok()
                && let Some(sfo) = parse_param_sfo(&buf)
                && sfo_path.starts_with("PSP_GAME")
            {
                psp_layout.param_sfo = Some(sfo);
            }
        }
    }
    psp_layout.psp_game_dir_present =
        matches!(find_path(media, observation, "PSP_GAME"), Ok(Some(_)));
    psp_layout.sysdir_present = matches!(
        find_path(media, observation, "PSP_GAME/SYSDIR"),
        Ok(Some(_))
    );
    psp_layout.eboot_bin_present = matches!(
        find_path(media, observation, "PSP_GAME/SYSDIR/EBOOT.BIN"),
        Ok(Some(_))
    );
    psp_layout.umd_data_bin_present =
        matches!(find_path(media, observation, "UMD_DATA.BIN"), Ok(Some(_)));
    let _ = PS3_LAYOUT_PATHS;
    for path in PSP_LAYOUT_PATHS {
        let _ = find_path(media, observation, path);
    }
    if psp_layout.psp_game_dir_present {
        evidence.extend(observe_psp_evidence(&psp_layout));
    }

    if let Ok(Some(entry)) = find_path(media, observation, "IPL.TXT")
        && !entry.is_directory
    {
        let bound = (entry.size as usize).min(MAX_IPL_TXT_BYTES);
        let offset = entry.extent_lba as u64 * observation.logical_block_size as u64;
        let mut buf = vec![0u8; bound];
        if media.read_at(offset, &mut buf).is_ok() {
            let fact = parse_ipl_txt(&buf);
            evidence.extend(observe_neogeocd_evidence(&fact));
        }
    }

    evidence
}

#[cfg(test)]
mod tests;
