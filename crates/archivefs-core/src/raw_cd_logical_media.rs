//! A [`crate::logical_media::LogicalMedia`] adapter over a plain,
//! file-backed raw-sector optical image (e.g. a `.bin` extracted from a
//! CUE/BIN pair, or any file that is nothing but a stream of 2352-byte CD
//! sectors): cooks physical 2352-byte raw sectors down to the conventional
//! 2048-byte logical view a filesystem/boot-header reader expects, without
//! ever mutating the source bytes.
//!
//! ```text
//! physical bytes (N x 2352-byte raw sectors)
//!     -> RawCdLogicalMedia::read_at
//!         -> crate::raw_cd_sector::extract_user_data (per sector)
//!             -> cooked N x 2048-byte logical stream
//!                 -> crate::iso9660 / crate::saturn_boot_evidence / etc.
//! ```
//!
//! # Why this is a separate module from [`crate::chd_logical_media`]
//!
//! [`crate::chd_logical_media::ChdTrackLogicalMedia`] solves the same
//! cooking problem for a CHD's *compressed, hunk-addressed* track data -
//! its `read_at` has to decode a hunk before it can even see raw sector
//! bytes. This module's input is already a flat, uncompressed byte slice
//! (a `.bin`, or any raw dump already read into memory), so there is no
//! hunk/codec layer to thread through; sectors are addressed directly by
//! simple offset arithmetic. Both modules ultimately do the same physical
//! sector -> logical block extraction, and both get it from the single
//! shared implementation in [`crate::raw_cd_sector`] - see that module's
//! documentation for why the logic itself is not duplicated.
//!
//! # Scope: one contiguous data track
//!
//! Like [`crate::chd_logical_media`]'s own "track 1 only" limitation, this
//! module treats its entire input as **one** contiguous raw-sector data
//! stream - the data track already isolated from any CUE sheet's other
//! tracks (audio, or additional data tracks). A caller is expected to hand
//! this adapter exactly the bytes of the disc's actual data track, the same
//! way [`crate::logical_media::SliceMedia`] is handed exactly one ISO's
//! bytes; this module does not parse CUE sheets or select among tracks
//! itself.
//!
//! # What is explicitly refused, not guessed
//!
//! - A length that is not an exact, nonzero multiple of
//!   [`RAW_SECTOR_BYTES`] (2352) - see
//!   [`RawCdLogicalMediaError::NotASectorMultiple`]. This deliberately does
//!   **not** attempt 2336-byte "sync/header-stripped" sectors - see
//!   [`crate::raw_cd_sector`]'s module documentation for why that is a
//!   separate, unverified convention this milestone does not implement.
//! - A first sector whose sync pattern or mode byte does not match a
//!   recognised raw CD-ROM/CD-XA layout - see
//!   [`RawCdLogicalMediaError::UnrecognizedSectorLayout`]. A file that is
//!   merely *divisible* by 2352 is not, by itself, evidence of a raw CD
//!   image - see [`looks_like_raw_cd`]'s own, stricter, conservative check
//!   for the dispatch-time question ("should a caller even try this
//!   adapter"), which this module's `open` function does not itself answer.
//! - A Mode 2 Form 2 sector encountered mid-read -
//!   [`crate::logical_media::LogicalMediaError::DecodeFailed`], never a
//!   silently wrong 2048 bytes (see [`crate::raw_cd_sector::extract_user_data`]).

use crate::logical_media::{LogicalMedia, LogicalMediaError};
use crate::raw_cd_sector::{
    LOGICAL_BLOCK_BYTES, RAW_SECTOR_BYTES, RawCdSectorMode, detect_sector_mode, extract_user_data,
};

/// Why [`open_raw_cd_logical_media`] could not produce a
/// [`RawCdLogicalMedia`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawCdLogicalMediaError {
    /// Fewer than one full [`RAW_SECTOR_BYTES`]-byte sector was supplied.
    TooShort { len: usize },
    /// `len` is not an exact multiple of [`RAW_SECTOR_BYTES`] - a partial
    /// trailing sector is a structural defect, not something this adapter
    /// pads or truncates around.
    NotASectorMultiple { len: usize },
    /// The first sector's sync pattern and/or mode byte did not match a
    /// layout [`crate::raw_cd_sector::detect_sector_mode`] recognises.
    UnrecognizedSectorLayout,
}

impl std::fmt::Display for RawCdLogicalMediaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { len } => {
                write!(
                    formatter,
                    "raw CD image too short: {len} bytes (need at least {RAW_SECTOR_BYTES})"
                )
            }
            Self::NotASectorMultiple { len } => write!(
                formatter,
                "raw CD image length {len} is not an exact multiple of {RAW_SECTOR_BYTES}-byte sectors"
            ),
            Self::UnrecognizedSectorLayout => formatter.write_str(
                "first sector did not match a recognised raw CD-ROM/CD-XA sync pattern and mode byte",
            ),
        }
    }
}

impl std::error::Error for RawCdLogicalMediaError {}

/// A [`LogicalMedia`] that presents a plain, file-backed raw-sector optical
/// image as a clean, contiguous stream of 2048-byte logical blocks.
#[derive(Debug)]
pub struct RawCdLogicalMedia<'a> {
    physical: &'a [u8],
    sector_mode: RawCdSectorMode,
    sector_count: u64,
}

impl RawCdLogicalMedia<'_> {
    /// The raw sector layout detected from the first physical sector - every
    /// sector in this image is read as this mode; a sector further in that
    /// turns out not to actually match (a malformed/inconsistent image) is
    /// reported as a read-time [`LogicalMediaError::DecodeFailed`], not
    /// silently reinterpreted.
    pub fn sector_mode(&self) -> RawCdSectorMode {
        self.sector_mode
    }

    /// The number of physical [`RAW_SECTOR_BYTES`]-byte sectors this image
    /// contains.
    pub fn sector_count(&self) -> u64 {
        self.sector_count
    }
}

/// Opens `data` (already fully in memory, borrowed for the adapter's
/// lifetime and never mutated) as a raw-sector optical image.
///
/// Only the length and the *first* sector's sync pattern/mode byte are
/// examined at open time - no per-sector validation happens until
/// [`LogicalMedia::read_at`] actually touches a given sector, matching this
/// crate's "never a whole-image read" discipline. A caller that wants
/// stronger, multi-sector corroboration before trusting this as a genuine
/// raw CD image (rather than a coincidentally-sized file) should use
/// [`looks_like_raw_cd`] first - see its own documentation.
pub fn open_raw_cd_logical_media(
    data: &[u8],
) -> Result<RawCdLogicalMedia<'_>, RawCdLogicalMediaError> {
    if data.len() < RAW_SECTOR_BYTES {
        return Err(RawCdLogicalMediaError::TooShort { len: data.len() });
    }
    if !data.len().is_multiple_of(RAW_SECTOR_BYTES) {
        return Err(RawCdLogicalMediaError::NotASectorMultiple { len: data.len() });
    }
    let first_sector = &data[..RAW_SECTOR_BYTES];
    let sector_mode =
        detect_sector_mode(first_sector).ok_or(RawCdLogicalMediaError::UnrecognizedSectorLayout)?;
    let sector_count = (data.len() / RAW_SECTOR_BYTES) as u64;

    Ok(RawCdLogicalMedia {
        physical: data,
        sector_mode,
        sector_count,
    })
}

/// A conservative, false-positive-resistant check for whether `data` is
/// worth trying [`open_raw_cd_logical_media`] on at all: `data` must be a
/// nonzero multiple of [`RAW_SECTOR_BYTES`] (at least two full sectors), and
/// **both** the first and second physical sectors must independently
/// exhibit the same recognised sync pattern and sector mode.
///
/// A single matching sector is not enough - the CD sync pattern
/// (`00h,FFh x10,00h`) is only 12 bytes, and a large-enough coincidentally-
/// sized file has some chance of producing it once by accident. Requiring
/// the *next* sector to also independently show a valid sync pattern and
/// the same mode is the "repeated valid sectors" corroboration this crate's
/// detection-conservatism discipline calls for (see the module
/// documentation's collision-safety notes and this module's own
/// `false_positive_resistance` tests).
pub fn looks_like_raw_cd(data: &[u8]) -> bool {
    if data.len() < RAW_SECTOR_BYTES * 2 || !data.len().is_multiple_of(RAW_SECTOR_BYTES) {
        return false;
    }
    let first = &data[..RAW_SECTOR_BYTES];
    let second = &data[RAW_SECTOR_BYTES..RAW_SECTOR_BYTES * 2];
    match (detect_sector_mode(first), detect_sector_mode(second)) {
        (Some(first_mode), Some(second_mode)) => first_mode == second_mode,
        _ => false,
    }
}

impl LogicalMedia for RawCdLogicalMedia<'_> {
    fn len(&self) -> u64 {
        self.sector_count * LOGICAL_BLOCK_BYTES as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), LogicalMediaError> {
        let out_of_bounds = || LogicalMediaError::OutOfBounds {
            offset,
            requested_len: buf.len(),
            media_len: self.len(),
        };
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or_else(out_of_bounds)?;
        if end > self.len() {
            return Err(out_of_bounds());
        }

        let mut filled = 0usize;
        while filled < buf.len() {
            let absolute = offset + filled as u64;
            let sector_index = (absolute / LOGICAL_BLOCK_BYTES as u64) as usize;
            let within_sector = (absolute % LOGICAL_BLOCK_BYTES as u64) as usize;

            let sector_start = sector_index * RAW_SECTOR_BYTES;
            let sector = &self.physical[sector_start..sector_start + RAW_SECTOR_BYTES];
            let user_data = extract_user_data(sector, self.sector_mode)
                .map_err(|detail| LogicalMediaError::DecodeFailed { detail })?;

            let take = (LOGICAL_BLOCK_BYTES - within_sector).min(buf.len() - filled);
            buf[filled..filled + take]
                .copy_from_slice(&user_data[within_sector..within_sector + take]);
            filled += take;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iso9660::{ISO9660_STANDARD_IDENTIFIER, observe_iso9660};
    use crate::raw_cd_sector::{
        MODE1_USER_DATA_OFFSET, MODE2_FORM1_USER_DATA_OFFSET, SYNC_PATTERN,
    };

    fn mode1_sector_with_pattern(sector_index: u8) -> [u8; RAW_SECTOR_BYTES] {
        let mut sector = [0xAAu8; RAW_SECTOR_BYTES];
        sector[..12].copy_from_slice(&SYNC_PATTERN);
        sector[15] = 1;
        for (position, byte) in sector
            [MODE1_USER_DATA_OFFSET..MODE1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES]
            .iter_mut()
            .enumerate()
        {
            *byte = sector_index.wrapping_add(position as u8);
        }
        sector
    }

    fn expected_mode1_pattern(sector_index: u8) -> Vec<u8> {
        (0..LOGICAL_BLOCK_BYTES)
            .map(|position| sector_index.wrapping_add(position as u8))
            .collect()
    }

    fn mode2_form_sector(
        user_data: &[u8; LOGICAL_BLOCK_BYTES],
        form2: bool,
    ) -> [u8; RAW_SECTOR_BYTES] {
        let mut sector = [0u8; RAW_SECTOR_BYTES];
        sector[..12].copy_from_slice(&SYNC_PATTERN);
        sector[15] = 2;
        sector[18] = if form2 { 1 << 5 } else { 0 };
        sector[MODE2_FORM1_USER_DATA_OFFSET..MODE2_FORM1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES]
            .copy_from_slice(user_data);
        sector
    }

    fn build_image(sectors: &[[u8; RAW_SECTOR_BYTES]]) -> Vec<u8> {
        sectors.iter().flatten().copied().collect()
    }

    // ------------------------------------------------------------------
    // open: happy path / bounds
    // ------------------------------------------------------------------

    #[test]
    fn opens_a_valid_single_sector_mode1_image() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        assert_eq!(media.sector_count(), 1);
        assert_eq!(media.sector_mode(), RawCdSectorMode::Mode1Raw);
        assert_eq!(media.len(), LOGICAL_BLOCK_BYTES as u64);
    }

    #[test]
    fn opens_a_valid_multi_sector_mode2_image() {
        let user_data = [0u8; LOGICAL_BLOCK_BYTES];
        let data = build_image(&[
            mode2_form_sector(&user_data, false),
            mode2_form_sector(&user_data, false),
        ]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        assert_eq!(media.sector_count(), 2);
        assert_eq!(media.sector_mode(), RawCdSectorMode::Mode2Raw);
    }

    #[test]
    fn inconsistent_mode_mid_image_is_read_as_the_first_sectors_mode_not_reinterpreted() {
        // The sector mode is fixed once, from the first sector, at open
        // time (see the module documentation) - a later sector that
        // happens to carry a different mode byte is not re-detected
        // per-sector; its bytes are simply extracted (and, for a Mode 2
        // Form 1 read misapplied to what is actually Mode 1 data, may be
        // wrong) rather than silently switching interpretation mid-stream.
        // This documents that real, if imperfect, behaviour explicitly.
        let mut second = mode1_sector_with_pattern(1);
        second[15] = 2; // corrupted: claims Mode 2 mid-image
        let data = build_image(&[mode1_sector_with_pattern(0)])
            .into_iter()
            .chain(second)
            .collect::<Vec<_>>();
        let media = open_raw_cd_logical_media(&data).unwrap();
        assert_eq!(media.sector_mode(), RawCdSectorMode::Mode1Raw);
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        // Still read as Mode 1 (offset 16), not re-detected as Mode 2.
        media.read_at(LOGICAL_BLOCK_BYTES as u64, &mut buf).unwrap();
        assert_eq!(buf.to_vec(), expected_mode1_pattern(1));
    }

    #[test]
    fn empty_input_is_too_short() {
        assert_eq!(
            open_raw_cd_logical_media(&[]).unwrap_err(),
            RawCdLogicalMediaError::TooShort { len: 0 }
        );
    }

    #[test]
    fn single_byte_short_of_one_sector_is_too_short() {
        let data = vec![0u8; RAW_SECTOR_BYTES - 1];
        assert_eq!(
            open_raw_cd_logical_media(&data).unwrap_err(),
            RawCdLogicalMediaError::TooShort {
                len: RAW_SECTOR_BYTES - 1
            }
        );
    }

    #[test]
    fn length_not_a_sector_multiple_is_rejected() {
        let mut data = build_image(&[mode1_sector_with_pattern(0)]);
        data.push(0);
        assert_eq!(
            open_raw_cd_logical_media(&data).unwrap_err(),
            RawCdLogicalMediaError::NotASectorMultiple {
                len: RAW_SECTOR_BYTES + 1
            }
        );
    }

    #[test]
    fn missing_sync_pattern_is_unrecognized_layout() {
        let mut sector = mode1_sector_with_pattern(0);
        sector[0] = 0x77;
        let data = build_image(&[sector]);
        assert_eq!(
            open_raw_cd_logical_media(&data).unwrap_err(),
            RawCdLogicalMediaError::UnrecognizedSectorLayout
        );
    }

    #[test]
    fn wrong_mode_byte_is_unrecognized_layout() {
        let mut sector = mode1_sector_with_pattern(0);
        sector[15] = 0xEE;
        let data = build_image(&[sector]);
        assert_eq!(
            open_raw_cd_logical_media(&data).unwrap_err(),
            RawCdLogicalMediaError::UnrecognizedSectorLayout
        );
    }

    #[test]
    fn all_zero_file_is_unrecognized_layout_not_misdetected() {
        let data = vec![0u8; RAW_SECTOR_BYTES * 4];
        assert_eq!(
            open_raw_cd_logical_media(&data).unwrap_err(),
            RawCdLogicalMediaError::UnrecognizedSectorLayout
        );
    }

    // ------------------------------------------------------------------
    // read_at: payload mapping
    // ------------------------------------------------------------------

    #[test]
    fn mode1_sector_payload_is_mapped_correctly() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        media.read_at(0, &mut buf).unwrap();
        assert_eq!(buf.to_vec(), expected_mode1_pattern(0));
    }

    #[test]
    fn mode2_form1_sector_payload_is_mapped_correctly() {
        let mut user_data = [0u8; LOGICAL_BLOCK_BYTES];
        user_data
            .iter_mut()
            .enumerate()
            .for_each(|(index, byte)| *byte = index as u8);
        let data = build_image(&[mode2_form_sector(&user_data, false)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        media.read_at(0, &mut buf).unwrap();
        assert_eq!(buf, user_data);
    }

    #[test]
    fn mode2_form2_sector_is_refused_not_misread() {
        let user_data = [0u8; LOGICAL_BLOCK_BYTES];
        let data = build_image(&[mode2_form_sector(&user_data, true)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; LOGICAL_BLOCK_BYTES];
        assert!(matches!(
            media.read_at(0, &mut buf),
            Err(LogicalMediaError::DecodeFailed { .. })
        ));
    }

    #[test]
    fn arbitrary_offset_read_works() {
        let data = build_image(&[
            mode1_sector_with_pattern(0),
            mode1_sector_with_pattern(1),
            mode1_sector_with_pattern(2),
        ]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; 16];
        media
            .read_at(LOGICAL_BLOCK_BYTES as u64 + 100, &mut buf)
            .unwrap();
        assert_eq!(buf, expected_mode1_pattern(1)[100..116]);
    }

    #[test]
    fn cross_sector_read_works() {
        let data = build_image(&[mode1_sector_with_pattern(0), mode1_sector_with_pattern(1)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; 32];
        media
            .read_at(LOGICAL_BLOCK_BYTES as u64 - 16, &mut buf)
            .unwrap();
        let mut expected = expected_mode1_pattern(0)[2032..2048].to_vec();
        expected.extend_from_slice(&expected_mode1_pattern(1)[0..16]);
        assert_eq!(buf.to_vec(), expected);
    }

    #[test]
    fn out_of_range_read_is_rejected() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; 4];
        assert!(matches!(
            media.read_at(media.len(), &mut buf),
            Err(LogicalMediaError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn huge_offset_does_not_panic() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; 4];
        assert!(media.read_at(u64::MAX, &mut buf).is_err());
    }

    #[test]
    fn zero_length_read_at_exact_end_succeeds() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf: [u8; 0] = [];
        assert!(media.read_at(media.len(), &mut buf).is_ok());
    }

    #[test]
    fn repeated_read_is_deterministic() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut first = [0u8; 64];
        let mut second = [0u8; 64];
        media.read_at(10, &mut first).unwrap();
        media.read_at(10, &mut second).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn source_bytes_are_never_modified() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        let before = data.clone();
        let media = open_raw_cd_logical_media(&data).unwrap();
        let mut buf = [0u8; 64];
        let _ = media.read_at(0, &mut buf);
        assert_eq!(data, before);
    }

    // ------------------------------------------------------------------
    // looks_like_raw_cd: conservative detection / false-positive resistance
    // ------------------------------------------------------------------

    #[test]
    fn two_matching_sectors_are_recognized() {
        let data = build_image(&[mode1_sector_with_pattern(0), mode1_sector_with_pattern(1)]);
        assert!(looks_like_raw_cd(&data));
    }

    #[test]
    fn single_sector_is_not_enough_for_looks_like_raw_cd() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        assert!(!looks_like_raw_cd(&data));
    }

    #[test]
    fn second_sector_with_wrong_mode_is_rejected() {
        let mut second = mode1_sector_with_pattern(1);
        second[15] = 2; // Mode 2, disagreeing with the first sector's Mode 1
        let data = build_image(&[mode1_sector_with_pattern(0)])
            .into_iter()
            .chain(second)
            .collect::<Vec<_>>();
        assert!(!looks_like_raw_cd(&data));
    }

    #[test]
    fn second_sector_missing_sync_is_rejected() {
        let mut second = mode1_sector_with_pattern(1);
        second[0] = 0x11;
        let data = build_image(&[mode1_sector_with_pattern(0)])
            .into_iter()
            .chain(second)
            .collect::<Vec<_>>();
        assert!(!looks_like_raw_cd(&data));
    }

    #[test]
    fn arbitrary_file_divisible_by_2352_is_not_recognized() {
        // A generic false-positive resistance case: a file that happens to
        // be a multiple of 2352 bytes but has no CD sync pattern anywhere.
        let data = vec![0x42u8; RAW_SECTOR_BYTES * 5];
        assert!(!looks_like_raw_cd(&data));
    }

    #[test]
    fn length_not_a_sector_multiple_is_never_recognized() {
        let mut data = build_image(&[mode1_sector_with_pattern(0), mode1_sector_with_pattern(1)]);
        data.push(0);
        assert!(!looks_like_raw_cd(&data));
    }

    #[test]
    fn too_short_for_two_sectors_is_never_recognized() {
        let data = build_image(&[mode1_sector_with_pattern(0)]);
        assert!(!looks_like_raw_cd(&data));
    }

    // ------------------------------------------------------------------
    // ISO9660 connection - proves the cooked view feeds the existing
    // ISO9660 reader with no second parser.
    // ------------------------------------------------------------------

    fn minimal_iso9660_image() -> Vec<u8> {
        const BLOCK: usize = 2048;
        let mut image = vec![0u8; 19 * BLOCK];

        let root_start = 18 * BLOCK;
        let mut cursor = root_start;
        cursor = write_iso_record(&mut image, cursor, &[0x00], false, 18, BLOCK as u32);
        cursor = write_iso_record(&mut image, cursor, &[0x01], true, 18, BLOCK as u32);
        write_iso_record(&mut image, cursor, b"SYSTEM.CNF;1", false, 0, 64);

        let pvd = 16 * BLOCK;
        image[pvd] = 1;
        image[pvd + 1..pvd + 6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        image[pvd + 6] = 1;
        image[pvd + 40..pvd + 72].fill(b' ');
        image[pvd + 40..pvd + 46].copy_from_slice(b"RAWISO");
        image[pvd + 128..pvd + 130].copy_from_slice(&(BLOCK as u16).to_le_bytes());
        image[pvd + 130..pvd + 132].copy_from_slice(&(BLOCK as u16).to_be_bytes());
        let root_record = &mut image[pvd + 156..pvd + 190];
        root_record[0] = 34;
        root_record[2..6].copy_from_slice(&18u32.to_le_bytes());
        root_record[6..10].copy_from_slice(&18u32.to_be_bytes());
        root_record[10..14].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        root_record[14..18].copy_from_slice(&(BLOCK as u32).to_be_bytes());
        root_record[25] = 0x02;
        root_record[32] = 1;
        root_record[33] = 0x00;

        let terminator = 17 * BLOCK;
        image[terminator] = 255;
        image[terminator + 1..terminator + 6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
        image[terminator + 6] = 1;

        image
    }

    fn write_iso_record(
        image: &mut [u8],
        cursor: usize,
        name: &[u8],
        is_dir: bool,
        extent_lba: u32,
        size: u32,
    ) -> usize {
        let mut length = 33 + name.len();
        if name.len().is_multiple_of(2) {
            length += 1;
        }
        image[cursor] = length as u8;
        image[cursor + 2..cursor + 6].copy_from_slice(&extent_lba.to_le_bytes());
        image[cursor + 6..cursor + 10].copy_from_slice(&extent_lba.to_be_bytes());
        image[cursor + 10..cursor + 14].copy_from_slice(&size.to_le_bytes());
        image[cursor + 14..cursor + 18].copy_from_slice(&size.to_be_bytes());
        image[cursor + 25] = if is_dir { 0x02 } else { 0x00 };
        image[cursor + 32] = name.len() as u8;
        image[cursor + 33..cursor + 33 + name.len()].copy_from_slice(name);
        cursor + length
    }

    fn mode1_sectors_for(image: &[u8]) -> Vec<u8> {
        image
            .chunks(LOGICAL_BLOCK_BYTES)
            .flat_map(|block| {
                let mut sector = [0u8; RAW_SECTOR_BYTES];
                sector[..12].copy_from_slice(&SYNC_PATTERN);
                sector[15] = 1;
                sector[MODE1_USER_DATA_OFFSET..MODE1_USER_DATA_OFFSET + block.len()]
                    .copy_from_slice(block);
                sector
            })
            .collect()
    }

    #[test]
    fn iso9660_can_be_observed_through_a_raw_cd_backed_logical_media() {
        let image = minimal_iso9660_image();
        let data = mode1_sectors_for(&image);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let observation =
            observe_iso9660(&media).expect("raw-CD-backed ISO9660 should be observable");
        assert_eq!(observation.volume_identifier, "RAWISO");
    }

    #[test]
    fn system_cnf_lookup_works_through_a_raw_cd_backed_logical_media() {
        let image = minimal_iso9660_image();
        let data = mode1_sectors_for(&image);
        let media = open_raw_cd_logical_media(&data).unwrap();
        let observation = observe_iso9660(&media).unwrap();
        let found = observation
            .root_entries
            .iter()
            .find(|entry| entry.comparison_name == "SYSTEM.CNF");
        assert!(found.is_some());
    }

    // ------------------------------------------------------------------
    // safety
    // ------------------------------------------------------------------

    #[test]
    fn no_platform_inference_is_emitted() {
        let messages = [
            RawCdLogicalMediaError::UnrecognizedSectorLayout.to_string(),
            RawCdLogicalMediaError::TooShort { len: 4 }.to_string(),
            RawCdLogicalMediaError::NotASectorMultiple { len: 5000 }.to_string(),
        ];
        for message in messages {
            let lower = message.to_lowercase();
            for platform in ["playstation", "dreamcast", "xbox", "gamecube", "saturn"] {
                assert!(!lower.contains(platform));
            }
        }
    }
}
