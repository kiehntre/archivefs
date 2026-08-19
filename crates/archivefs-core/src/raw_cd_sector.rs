//! Shared, pure, read-only CD/CD-XA raw-sector layout: sync pattern, mode
//! byte, and the Mode 1 / Mode 2 Form 1 -> 2048-byte user-data extraction
//! every raw-sector-backed [`crate::logical_media::LogicalMedia`] adapter in
//! this crate needs.
//!
//! # Why this module exists
//!
//! [`crate::chd_logical_media`] already hand-verified this exact layout (see
//! its own module documentation) for CHD-backed CD images. [`crate::raw_cd_logical_media`]
//! needs the identical byte-for-byte layout for plain file-backed raw-sector
//! images (`.bin`, or any file that is a bare stream of 2352-byte sectors) -
//! the same physical convention, just a different container. Rather than
//! re-verify and duplicate the offsets a second time, this module is the one
//! place they live; [`crate::chd_logical_media`] now re-exports them from
//! here instead of defining its own copy.
//!
//! # Format verified, not assumed
//!
//! Sector layout: verified against MAME's `cdrom.h`/`chd.h` (`MAX_SECTOR_DATA
//! = 2352`), cross-checked against `simias/cdimage`'s `sector.rs` (an
//! independently authored, working CD sector reader) and ECMA-130's
//! documented Mode 1 field sizes (`12 + 4 + 2048 + 4 + 8 + 172 + 104 =
//! 2352`).
//!
//! ```text
//! Raw 2352-byte CD-ROM sector:
//! [0..12]   sync        12 bytes  00h, FFh x10, 00h (ECMA-130 sec. 19,
//!                                                     ISO 9660/Yellow Book)
//! [12..15]  address     3 bytes   minute/second/frame (BCD)
//! [15]      mode        1 byte    0x00 = empty/blank, 0x01 = Mode 1,
//!                                  0x02 = Mode 2
//! -- Mode 1 --
//! [16..2064]   user data    2048 bytes
//! [2064..2072] EDC/spare
//! [2072..2076] EDC
//! [2076..2248] ECC (P+Q parity)
//! [2248..2352] (unused in Mode 1 - reserved)
//! -- Mode 2 (CD-XA) --
//! [16..24]     subheader    8 bytes  (submode duplicated at [18] and [22])
//! [24..2072]   user data    2048 bytes  (Form 1 only - see below)
//! [2072..2076] EDC
//! [2076..2348] ECC
//! ```
//!
//! The CD-XA submode byte's bit `0x20` distinguishes Form 1 (2048-byte
//! user-data payload meant for a filesystem, ECC/EDC-protected) from Form 2
//! (2324-byte payload, e.g. streaming audio/video, EDC-only) - verified
//! against the same `simias/cdimage` source
//! (`self.data[18] & (1 << 5) != 0` => Form 2). This module extracts Form 1
//! only; a Form 2 sector is refused, never misread as 2048 bytes of
//! filesystem data (see [`extract_user_data`]).
//!
//! # What this module deliberately does not cover
//!
//! - **2336-byte "cooked minus sync/header" sectors.** Real tools sometimes
//!   store CD-XA sectors with the 16-byte sync+address+mode prefix already
//!   stripped, leaving a 2336-byte record (8-byte subheader + 2328 bytes of
//!   Form 1 user data + EDC, or the Form 2 equivalent). This is a genuinely
//!   different on-disk convention, not a variant of the 2352-byte layout
//!   above, and no source consulted while building this module corroborates
//!   the exact boundary between "this crate has verified 2336-byte handling"
//!   and "guessed" closely enough to add it here. [`crate::raw_cd_logical_media`]
//!   explicitly rejects a 2336-byte-sector image rather than guess - see
//!   that module's documentation.
//! - **Mode 2 Form 2** payload extraction (2324 bytes, not 2048) - tracked
//!   as a real gap, not silently mapped onto Form 1's offsets.
//! - Subchannel/subcode data (P-W, typically an extra 96 bytes per sector in
//!   some raw dump conventions) - out of scope; every adapter built on this
//!   module reads only the leading [`RAW_SECTOR_BYTES`] of each unit.

/// The size of one raw CD sector, verified against MAME's `cdrom.h`
/// (`MAX_SECTOR_DATA = 2352`).
pub const RAW_SECTOR_BYTES: usize = 2352;

/// The size of the logical block this module's extraction functions expose -
/// the conventional ISO9660/CD-ROM user-data block size.
pub const LOGICAL_BLOCK_BYTES: usize = 2048;

/// The 12-byte CD-ROM sync pattern every raw sector begins with (ECMA-130
/// sec. 19): `00h, FFh x10, 00h`.
pub const SYNC_PATTERN: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

/// Byte offset, within a raw sector, of the mode byte (sync(12) +
/// address(3)).
pub const MODE_BYTE_OFFSET: usize = 15;

/// `MODE1` user-data offset within a raw sector: sync(12) + header(4).
pub const MODE1_USER_DATA_OFFSET: usize = 16;

/// `MODE2 Form 1` user-data offset within a raw sector: sync(12) + header(4)
/// + subheader(8).
pub const MODE2_FORM1_USER_DATA_OFFSET: usize = 24;

/// The byte offset, within a raw sector, of the XA subheader's `submode`
/// byte (first of its two duplicated copies).
pub const MODE2_SUBMODE_OFFSET: usize = 18;

/// The bit of the XA submode byte that is set for a Form 2 sector and clear
/// for a Form 1 sector.
pub const MODE2_SUBMODE_FORM2_BIT: u8 = 1 << 5;

/// Which raw CD track type this module knows how to extract filesystem-
/// relevant user data from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawCdSectorMode {
    Mode1Raw,
    Mode2Raw,
}

impl RawCdSectorMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mode1Raw => "Mode1",
            Self::Mode2Raw => "Mode2 Form1",
        }
    }
}

/// Checks whether `sector` begins with the CD-ROM [`SYNC_PATTERN`].
pub fn sync_pattern_valid(sector: &[u8]) -> bool {
    sector.len() >= SYNC_PATTERN.len() && sector[..SYNC_PATTERN.len()] == SYNC_PATTERN
}

/// Detects a raw sector's mode from its sync pattern and mode byte.
/// `None` when `sector` is too short, the sync pattern does not match, or
/// the mode byte is neither `0x01` nor `0x02` - conservative by
/// construction, matching this crate's "never classify by size alone"
/// discipline (see [`crate::raw_cd_logical_media`]'s detection notes).
pub fn detect_sector_mode(sector: &[u8]) -> Option<RawCdSectorMode> {
    if sector.len() < RAW_SECTOR_BYTES || !sync_pattern_valid(sector) {
        return None;
    }
    match sector[MODE_BYTE_OFFSET] {
        1 => Some(RawCdSectorMode::Mode1Raw),
        2 => Some(RawCdSectorMode::Mode2Raw),
        _ => None,
    }
}

/// Extracts the 2048-byte logical block from one raw `sector` (must be
/// exactly [`RAW_SECTOR_BYTES`] long), according to `mode`.
///
/// For `Mode2Raw`, a Form 2 sector (submode bit `0x20` set) is refused
/// rather than misread - Form 2's own user-data field is 2324 bytes, not
/// 2048, and treating it as a plain 2048-byte block would silently corrupt
/// every byte read from that sector onward.
pub fn extract_user_data(sector: &[u8], mode: RawCdSectorMode) -> Result<&[u8], String> {
    debug_assert_eq!(sector.len(), RAW_SECTOR_BYTES);
    match mode {
        RawCdSectorMode::Mode1Raw => {
            Ok(&sector[MODE1_USER_DATA_OFFSET..MODE1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES])
        }
        RawCdSectorMode::Mode2Raw => {
            let submode = sector[MODE2_SUBMODE_OFFSET];
            if submode & MODE2_SUBMODE_FORM2_BIT != 0 {
                Err(
                    "encountered a CD-XA Mode 2 Form 2 sector; only Form 1 sectors are \
                     supported for filesystem reads"
                        .to_string(),
                )
            } else {
                Ok(&sector[MODE2_FORM1_USER_DATA_OFFSET
                    ..MODE2_FORM1_USER_DATA_OFFSET + LOGICAL_BLOCK_BYTES])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode1_sector() -> [u8; RAW_SECTOR_BYTES] {
        let mut sector = [0u8; RAW_SECTOR_BYTES];
        sector[..12].copy_from_slice(&SYNC_PATTERN);
        sector[MODE_BYTE_OFFSET] = 1;
        sector
    }

    fn mode2_sector(form2: bool) -> [u8; RAW_SECTOR_BYTES] {
        let mut sector = [0u8; RAW_SECTOR_BYTES];
        sector[..12].copy_from_slice(&SYNC_PATTERN);
        sector[MODE_BYTE_OFFSET] = 2;
        sector[MODE2_SUBMODE_OFFSET] = if form2 { MODE2_SUBMODE_FORM2_BIT } else { 0 };
        sector
    }

    #[test]
    fn mode1_sync_and_mode_byte_are_detected() {
        assert_eq!(
            detect_sector_mode(&mode1_sector()),
            Some(RawCdSectorMode::Mode1Raw)
        );
    }

    #[test]
    fn mode2_sync_and_mode_byte_are_detected() {
        assert_eq!(
            detect_sector_mode(&mode2_sector(false)),
            Some(RawCdSectorMode::Mode2Raw)
        );
    }

    #[test]
    fn form2_submode_does_not_change_detected_mode() {
        // Form 1 vs Form 2 is a read-time distinction (extract_user_data),
        // not a detection-time one - both are still "Mode 2 Raw" sectors.
        assert_eq!(
            detect_sector_mode(&mode2_sector(true)),
            Some(RawCdSectorMode::Mode2Raw)
        );
    }

    #[test]
    fn missing_sync_pattern_is_not_detected() {
        let mut sector = mode1_sector();
        sector[0] = 0x11;
        assert_eq!(detect_sector_mode(&sector), None);
    }

    #[test]
    fn wrong_mode_byte_is_not_detected() {
        let mut sector = mode1_sector();
        sector[MODE_BYTE_OFFSET] = 0x09;
        assert_eq!(detect_sector_mode(&sector), None);
    }

    #[test]
    fn truncated_sector_fails_closed() {
        let sector = &mode1_sector()[..100];
        assert_eq!(detect_sector_mode(sector), None);
    }

    #[test]
    fn empty_sector_fails_closed() {
        assert_eq!(detect_sector_mode(&[]), None);
    }

    #[test]
    fn mode1_user_data_offsets_are_correct() {
        let mut sector = mode1_sector();
        sector[MODE1_USER_DATA_OFFSET] = 0xAB;
        let data = extract_user_data(&sector, RawCdSectorMode::Mode1Raw).unwrap();
        assert_eq!(data.len(), LOGICAL_BLOCK_BYTES);
        assert_eq!(data[0], 0xAB);
    }

    #[test]
    fn mode2_form1_user_data_offsets_are_correct() {
        let mut sector = mode2_sector(false);
        sector[MODE2_FORM1_USER_DATA_OFFSET] = 0xCD;
        let data = extract_user_data(&sector, RawCdSectorMode::Mode2Raw).unwrap();
        assert_eq!(data.len(), LOGICAL_BLOCK_BYTES);
        assert_eq!(data[0], 0xCD);
    }

    #[test]
    fn mode2_form2_is_refused_not_misread() {
        let sector = mode2_sector(true);
        assert!(extract_user_data(&sector, RawCdSectorMode::Mode2Raw).is_err());
    }

    #[test]
    fn sync_pattern_valid_rejects_short_input() {
        assert!(!sync_pattern_valid(&[0x00, 0xFF]));
    }

    #[test]
    fn labels_are_distinct() {
        assert_ne!(
            RawCdSectorMode::Mode1Raw.label(),
            RawCdSectorMode::Mode2Raw.label()
        );
    }
}
