//! The smallest reusable, bounded primitives shared by this crate's
//! cartridge-header observers (NES, SNES, GB/GBC, GBA, Mega Drive, SMS/Game
//! Gear, Atari 7800, Atari Lynx, Neo Geo Pocket, WonderSwan) - deliberately
//! not a generic "header framework": every format module still owns its own
//! verified field offsets and its own `parse_*`/`observe_*` functions, the
//! same discipline [`crate::saturn_boot_evidence`]/[`crate::threedo_boot_evidence`]
//! already established. This module exists only to stop the same three
//! primitives - ASCII field trimming, an 8-bit wrapping checksum sum, and a
//! 16-bit wrapping checksum sum - from being hand-copied into every one of
//! them.
//!
//! Nothing here performs I/O, decides a platform, or emits
//! [`crate::content_evidence::ContentEvidence`] - that stays in each format's
//! own module, exactly where the confidence judgement belongs.

/// Reads `bytes[offset..offset+length]` as ASCII/Latin-1-ish text, trimming
/// trailing NUL bytes and whitespace. The same trim behaviour
/// [`crate::saturn_boot_evidence`]'s local `field` helper and
/// [`crate::threedo_boot_evidence`]'s local `ascii_trimmed` helper each
/// independently implemented - factored here so a new module reuses it
/// instead of writing a fourth copy.
///
/// Fails closed (returns `None`) rather than panicking when the requested
/// range does not fit in `bytes`.
pub fn ascii_field(bytes: &[u8], offset: usize, length: usize) -> Option<String> {
    let end = offset.checked_add(length)?;
    let slice = bytes.get(offset..end)?;
    Some(
        String::from_utf8_lossy(slice)
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string(),
    )
}

/// A NUL-terminated ASCII/Latin-1-ish field: reads from `offset` up to (not
/// including) the first `\0` byte, or up to `max_length` bytes if no `\0` is
/// found first. Distinct from [`ascii_field`], which reads an exact fixed
/// width and only trims a *trailing* run of NULs/whitespace - this is for
/// fields (like [`crate::psp_pbp_evidence`]'s key strings) whose real content
/// can be shorter than the field's maximum allocated width, with unrelated
/// bytes potentially following the terminator.
pub fn ascii_field_nul_terminated(
    bytes: &[u8],
    offset: usize,
    max_length: usize,
) -> Option<String> {
    let end = offset.checked_add(max_length)?;
    let slice = bytes.get(offset..end)?;
    let terminator = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Some(String::from_utf8_lossy(&slice[..terminator]).into_owned())
}

/// An 8-bit wrapping sum of `bytes` - the primitive the Game Boy header
/// checksum ([`crate::gb_header_evidence`]) and, independently, real
/// WonderSwan-style checksums build on. Deliberately just `u8::wrapping_add`
/// in a loop: no format-specific starting value, subtraction order, or
/// exclusion range is baked in here, since those genuinely differ between
/// formats (compare the Game Boy header checksum's `checksum - byte - 1`
/// loop against a plain additive sum) - only the "sum every byte, wrapping"
/// core is shared.
pub fn wrapping_sum_u8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte))
}

/// A 16-bit wrapping sum of `bytes`, treated as a stream of bytes (not
/// pre-paired words) - the primitive a global/whole-ROM checksum
/// (WonderSwan, and the "global checksum" GB/GBC field mentions but does not
/// require) builds on.
pub fn wrapping_sum_u16(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0u16, |sum, &byte| sum.wrapping_add(byte as u16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_field_trims_trailing_nul_and_whitespace() {
        assert_eq!(
            ascii_field(b"HELLO\0\0\0 ", 0, 9),
            Some("HELLO".to_string())
        );
    }

    #[test]
    fn ascii_field_reads_at_an_offset() {
        assert_eq!(ascii_field(b"XXHELLO\0\0", 2, 7), Some("HELLO".to_string()));
    }

    #[test]
    fn ascii_field_out_of_bounds_fails_closed() {
        assert_eq!(ascii_field(b"short", 3, 10), None);
    }

    #[test]
    fn ascii_field_offset_overflow_fails_closed_not_panic() {
        assert_eq!(ascii_field(b"short", usize::MAX, 1), None);
    }

    #[test]
    fn ascii_field_empty_length_is_empty_string() {
        assert_eq!(ascii_field(b"anything", 0, 0), Some(String::new()));
    }

    #[test]
    fn ascii_field_nul_terminated_stops_at_first_nul() {
        assert_eq!(
            ascii_field_nul_terminated(b"HI\0IGNORED", 0, 10),
            Some("HI".to_string())
        );
    }

    #[test]
    fn ascii_field_nul_terminated_reads_full_width_when_no_nul() {
        assert_eq!(
            ascii_field_nul_terminated(b"HELLOWORLD", 0, 10),
            Some("HELLOWORLD".to_string())
        );
    }

    #[test]
    fn ascii_field_nul_terminated_out_of_bounds_fails_closed() {
        assert_eq!(ascii_field_nul_terminated(b"short", 0, 100), None);
    }

    #[test]
    fn wrapping_sum_u8_wraps_around() {
        assert_eq!(wrapping_sum_u8(&[0xFF, 0x01]), 0x00);
        assert_eq!(wrapping_sum_u8(&[0x10, 0x20, 0x30]), 0x60);
    }

    #[test]
    fn wrapping_sum_u8_empty_is_zero() {
        assert_eq!(wrapping_sum_u8(&[]), 0);
    }

    #[test]
    fn wrapping_sum_u16_wraps_around() {
        let bytes = vec![0xFFu8; 0x10001];
        // 0x10001 bytes each contributing 0xFF: sum = 0x10001 * 0xFF, taken
        // mod 0x10000. Computed independently below rather than reusing the
        // function's own arithmetic.
        let expected = ((0x10001u64 * 0xFF) % 0x1_0000) as u16;
        assert_eq!(wrapping_sum_u16(&bytes), expected);
    }

    #[test]
    fn wrapping_sum_u16_empty_is_zero() {
        assert_eq!(wrapping_sum_u16(&[]), 0);
    }
}
