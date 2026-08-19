//! Pure, read-only PSP/PS3/Vita `EBOOT.PBP` ("PBP", `\0PBP`) container
//! header evidence: a bounded fixed-header parser plus offset-table
//! validation, reusing [`crate::param_sfo`] for the embedded `PARAM.SFO`
//! section rather than a second SFO parser.
//!
//! # Format verified, not assumed
//!
//! Cross-checked against two independent, mutually-agreeing sources: a
//! search-summarized primary reference documenting the on-disk struct
//! (`char signature[4]; int version; int offset[8];`, with the offset
//! table's index 0 = `PARAM.SFO` and confirmed at byte `0x08`), and
//! PPSSPP's own loader source (`Core/Loaders.cpp`,
//! `https://raw.githubusercontent.com/hrydgard/ppsspp/master/Core/Loaders.cpp`),
//! a real, production PSP emulator, which reads the `DATA.PSAR` section
//! offset from byte `0x24` - exactly `0x08 + 7*4`, the 8th (index 7) slot
//! of the same offset table, independently confirming both the table's
//! base offset and its element order.
//!
//! ```text
//! PBP fixed header (all fields little-endian):
//! [0x00..0x04]   signature       4 bytes   "\0PBP"
//! [0x04..0x08]   version         4 bytes
//! [0x08..0x0C]   offset[0]       4 bytes   PARAM.SFO
//! [0x0C..0x10]   offset[1]       4 bytes   ICON0.PNG
//! [0x10..0x14]   offset[2]       4 bytes   ICON1.PMF
//! [0x14..0x18]   offset[3]       4 bytes   PIC0.PNG
//! [0x18..0x1C]   offset[4]       4 bytes   PIC1.PNG
//! [0x1C..0x20]   offset[5]       4 bytes   SND0.AT3
//! [0x20..0x24]   offset[6]       4 bytes   DATA.PSP  (a PSP executable)
//! [0x24..0x28]   offset[7]       4 bytes   DATA.PSAR (the large payload -
//!                                          a real UMD ISO for a PSN
//!                                          full-game PBP, or a compressed
//!                                          data archive for homebrew)
//! ```
//!
//! Every section's byte range is `[offset[i], offset[i+1])`, except the
//! last (`DATA.PSAR`), which runs to end-of-file - the conventional
//! interpretation this module's own offset-table validation assumes (see
//! [`validate_pbp_offsets`]).
//!
//! # Scope: header + offset validation + PARAM.SFO only
//!
//! This module never decompresses or extracts `DATA.PSAR` (which, for a
//! full PSN game, effectively **is** the disc image - decompressing it is
//! out of scope by the same "no extraction" discipline every other
//! container observer in this crate follows), never touches `ICON0.PNG`/
//! `PIC0.PNG`/`SND0.AT3` beyond knowing their declared byte ranges exist,
//! and reads `PARAM.SFO` (via [`crate::param_sfo::parse_param_sfo`]) only
//! from the bounded slice the header's own offsets declare - never a
//! second, competing SFO implementation.
//!
//! # No real specimen in this project's corpus
//!
//! Unlike [`crate::ps3_disc_evidence`]'s `.pkg` support, no real
//! `EBOOT.PBP` file was found anywhere in this project's read-only ROM
//! corpus search (see the crate-level milestone report) - PSP titles
//! present there are UMD `.iso`/`.cso` dumps, whose `PSP_GAME/SYSDIR/`
//! layout is a different, already-covered structure
//! ([`crate::psp_boot_evidence`]). This module is therefore validated only
//! against synthetic, hand-built fixtures - explicitly permitted for a
//! format with no real sample available, per this milestone's own policy.
//! No fixture here is derived from copyrighted material.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::param_sfo::{SfoObservation, parse_param_sfo, product_code_evidence};

pub const PBP_MAGIC: &[u8; 4] = b"\x00PBP";
pub const PBP_SECTION_COUNT: usize = 8;
/// Fixed header length: 4-byte signature + 4-byte version + 8 x 4-byte
/// offsets = `0x28`.
pub const PBP_HEADER_BYTES: usize = 4 + 4 + PBP_SECTION_COUNT * 4;

const VERSION_OFFSET: usize = 0x04;
const OFFSET_TABLE_START: usize = 0x08;

/// Index into [`PbpHeaderFact::section_offsets`] for each named section, in
/// on-disk order.
pub const PBP_SECTION_PARAM_SFO: usize = 0;
pub const PBP_SECTION_ICON0_PNG: usize = 1;
pub const PBP_SECTION_ICON1_PMF: usize = 2;
pub const PBP_SECTION_PIC0_PNG: usize = 3;
pub const PBP_SECTION_PIC1_PNG: usize = 4;
pub const PBP_SECTION_SND0_AT3: usize = 5;
pub const PBP_SECTION_DATA_PSP: usize = 6;
pub const PBP_SECTION_DATA_PSAR: usize = 7;

pub fn looks_like_pbp(header: &[u8]) -> bool {
    header.len() >= PBP_MAGIC.len() && header[..PBP_MAGIC.len()] == *PBP_MAGIC.as_slice()
}

/// What a parsed PBP fixed header directly states - no bound-checking
/// against the actual file length happens here; see [`validate_pbp_offsets`]
/// for that, kept as a separate step so a caller can distinguish "the fixed
/// header itself didn't parse" from "the header parsed but its offsets are
/// nonsensical for this file's actual length."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PbpHeaderFact {
    pub version: u32,
    /// The 8 raw section-start offsets, in on-disk order - index with the
    /// `PBP_SECTION_*` constants.
    pub section_offsets: [u32; PBP_SECTION_COUNT],
}

/// Parses the fixed [`PBP_HEADER_BYTES`]-byte PBP header from `header`.
/// `None` when the magic does not match or fewer than [`PBP_HEADER_BYTES`]
/// bytes were supplied - fails closed, never panics.
pub fn parse_pbp_header(header: &[u8]) -> Option<PbpHeaderFact> {
    if !looks_like_pbp(header) || header.len() < PBP_HEADER_BYTES {
        return None;
    }
    let version = u32::from_le_bytes(
        header[VERSION_OFFSET..VERSION_OFFSET + 4]
            .try_into()
            .unwrap(),
    );
    let mut section_offsets = [0u32; PBP_SECTION_COUNT];
    for (index, slot) in section_offsets.iter_mut().enumerate() {
        let start = OFFSET_TABLE_START + index * 4;
        *slot = u32::from_le_bytes(header[start..start + 4].try_into().unwrap());
    }
    Some(PbpHeaderFact {
        version,
        section_offsets,
    })
}

/// Why [`validate_pbp_offsets`] rejected a [`PbpHeaderFact`] against a real
/// file length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PbpOffsetError {
    /// `section_offsets[0]` (`PARAM.SFO`) starts before the fixed header
    /// itself ends - every real section must start at or after
    /// [`PBP_HEADER_BYTES`].
    FirstOffsetBeforeHeader { offset: u32 },
    /// Two adjacent offsets are not in non-decreasing order - sections are
    /// laid out sequentially, so `section_offsets[i] > section_offsets[i+1]`
    /// is structurally impossible in a well-formed PBP.
    NonMonotonicOffsets {
        index: usize,
        offset: u32,
        next_offset: u32,
    },
    /// The last section (`DATA.PSAR`) starts beyond the file's actual
    /// length.
    OffsetBeyondEof { offset: u32, total_len: u64 },
}

impl std::fmt::Display for PbpOffsetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FirstOffsetBeforeHeader { offset } => write!(
                formatter,
                "PARAM.SFO offset {offset} starts before the end of the fixed {PBP_HEADER_BYTES}-byte header"
            ),
            Self::NonMonotonicOffsets {
                index,
                offset,
                next_offset,
            } => write!(
                formatter,
                "section offset table is not monotonic: section {index} starts at {offset}, but section {} starts earlier, at {next_offset}",
                index + 1
            ),
            Self::OffsetBeyondEof { offset, total_len } => write!(
                formatter,
                "DATA.PSAR offset {offset} is beyond the file's actual length ({total_len} bytes)"
            ),
        }
    }
}

impl std::error::Error for PbpOffsetError {}

/// Validates `fact`'s offset table against `total_len` (the real file's
/// actual byte length): the first offset must not start inside the fixed
/// header, every offset must be non-decreasing, and the last offset must
/// not exceed `total_len`. Returns `Ok(())` for a structurally sound table -
/// this is a shape check only, never proof the sections' actual contents
/// are valid.
pub fn validate_pbp_offsets(fact: &PbpHeaderFact, total_len: u64) -> Result<(), PbpOffsetError> {
    let first = fact.section_offsets[PBP_SECTION_PARAM_SFO];
    if (first as usize) < PBP_HEADER_BYTES {
        return Err(PbpOffsetError::FirstOffsetBeforeHeader { offset: first });
    }
    for index in 0..PBP_SECTION_COUNT - 1 {
        let offset = fact.section_offsets[index];
        let next_offset = fact.section_offsets[index + 1];
        if offset > next_offset {
            return Err(PbpOffsetError::NonMonotonicOffsets {
                index,
                offset,
                next_offset,
            });
        }
    }
    let last = fact.section_offsets[PBP_SECTION_DATA_PSAR];
    if u64::from(last) > total_len {
        return Err(PbpOffsetError::OffsetBeyondEof {
            offset: last,
            total_len,
        });
    }
    Ok(())
}

/// Returns the bounded byte range `[offset[index], offset[index + 1])` for
/// `data`, or `None` if `index` is not a valid non-final section index, the
/// range is inverted, or it falls outside `data`. Never used for the final
/// section (`DATA.PSAR`), whose end is end-of-file, not another offset -
/// see [`read_data_psar_prefix`] for that case.
fn section_slice<'a>(data: &'a [u8], fact: &PbpHeaderFact, index: usize) -> Option<&'a [u8]> {
    if index >= PBP_SECTION_COUNT - 1 {
        return None;
    }
    let start = fact.section_offsets[index] as usize;
    let end = fact.section_offsets[index + 1] as usize;
    if start > end {
        return None;
    }
    data.get(start..end)
}

/// Parses the embedded `PARAM.SFO` section (bounded to
/// `[offset[0], offset[1])`) via [`crate::param_sfo::parse_param_sfo`] -
/// the shared SFO parser, not a second implementation.
pub fn read_pbp_param_sfo(data: &[u8], fact: &PbpHeaderFact) -> Option<SfoObservation> {
    let slice = section_slice(data, fact, PBP_SECTION_PARAM_SFO)?;
    parse_param_sfo(slice)
}

/// A bounded prefix of the `DATA.PSAR` section (which, for a real PSN
/// full-game PBP, can itself be gigabytes) - up to `max_bytes` starting at
/// its declared offset. Never reads the whole section; a caller wanting
/// more must bound its own request explicitly.
pub fn read_data_psar_prefix<'a>(
    data: &'a [u8],
    fact: &PbpHeaderFact,
    max_bytes: usize,
) -> Option<&'a [u8]> {
    let start = fact.section_offsets[PBP_SECTION_DATA_PSAR] as usize;
    let end = start.checked_add(max_bytes)?.min(data.len());
    if start > end {
        return None;
    }
    data.get(start..end)
}

/// Neutral evidence for a parsed, offset-validated PBP: `Container` =
/// `"PBP"` (`Strong`), plus (only if the embedded `PARAM.SFO` parses and
/// carries a `DISC_ID`) a `ProductCode` fact via
/// [`crate::param_sfo::product_code_evidence`] - the same shared helper
/// [`crate::psp_boot_evidence`] uses, not a second lookup convention.
pub fn observe_pbp_evidence(sfo: Option<&SfoObservation>) -> Vec<ContentEvidence> {
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::Container,
        "PBP",
        ContentEvidenceConfidence::Strong,
        "PBP header magic matched, the fixed header parsed, and the section-offset table validated",
    )];
    if let Some(sfo) = sfo
        && let Some(product_code) = product_code_evidence(sfo, "DISC_ID")
    {
        evidence.push(product_code);
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_pbp(sfo_bytes: &[u8], psar_bytes: &[u8]) -> Vec<u8> {
        let param_sfo_start = PBP_HEADER_BYTES as u32;
        let param_sfo_end = param_sfo_start + sfo_bytes.len() as u32;
        // ICON0..DATA.PSP all share the end of PARAM.SFO (empty sections) -
        // a legitimate, if minimal, layout: consecutive equal offsets mean
        // a zero-length section, not a malformed one.
        let mut offsets = [param_sfo_end; PBP_SECTION_COUNT];
        offsets[PBP_SECTION_PARAM_SFO] = param_sfo_start;
        offsets[PBP_SECTION_DATA_PSAR] = param_sfo_end;

        let mut file = vec![0u8; PBP_HEADER_BYTES];
        file[0..4].copy_from_slice(PBP_MAGIC.as_slice());
        file[VERSION_OFFSET..VERSION_OFFSET + 4].copy_from_slice(&1u32.to_le_bytes());
        for (index, offset) in offsets.iter().enumerate() {
            let start = OFFSET_TABLE_START + index * 4;
            file[start..start + 4].copy_from_slice(&offset.to_le_bytes());
        }
        file.extend_from_slice(sfo_bytes);
        file.extend_from_slice(psar_bytes);
        file
    }

    fn synthetic_sfo(disc_id: &str) -> Vec<u8> {
        let key_bytes = b"DISC_ID\0".to_vec();
        let value_bytes = format!("{disc_id}\0").into_bytes();
        let key_table_start = 20 + 16u32;
        let data_table_start = key_table_start + key_bytes.len() as u32;
        let mut file = Vec::new();
        file.extend_from_slice(&[0x00, b'P', b'S', b'F']);
        file.extend_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&key_table_start.to_le_bytes());
        file.extend_from_slice(&data_table_start.to_le_bytes());
        file.extend_from_slice(&1u32.to_le_bytes());
        file.extend_from_slice(&0u16.to_le_bytes());
        file.extend_from_slice(&0x0204u16.to_le_bytes());
        file.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes());
        file.extend_from_slice(&(value_bytes.len() as u32).to_le_bytes());
        file.extend_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(&key_bytes);
        file.extend_from_slice(&value_bytes);
        file
    }

    // ------------------------------------------------------------------
    // Magic / header parsing
    // ------------------------------------------------------------------

    #[test]
    fn magic_is_detected() {
        assert!(looks_like_pbp(PBP_MAGIC.as_slice()));
        assert!(!looks_like_pbp(b"ZZZZ"));
    }

    #[test]
    fn header_parses_version_and_offsets() {
        let sfo = synthetic_sfo("ULUS10000");
        let file = synthetic_pbp(&sfo, b"psar-payload");
        let fact = parse_pbp_header(&file).unwrap();
        assert_eq!(fact.version, 1);
        assert_eq!(
            fact.section_offsets[PBP_SECTION_PARAM_SFO],
            PBP_HEADER_BYTES as u32
        );
    }

    #[test]
    fn truncated_header_fails_closed() {
        let file = synthetic_pbp(&synthetic_sfo("X"), b"");
        assert_eq!(parse_pbp_header(&file[..PBP_HEADER_BYTES - 1]), None);
    }

    #[test]
    fn wrong_magic_fails_closed() {
        assert_eq!(parse_pbp_header(&[0u8; PBP_HEADER_BYTES]), None);
    }

    #[test]
    fn empty_input_fails_closed_not_panic() {
        assert_eq!(parse_pbp_header(&[]), None);
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let file = synthetic_pbp(&synthetic_sfo("X"), b"payload");
        assert_eq!(parse_pbp_header(&file), parse_pbp_header(&file));
    }

    #[test]
    fn parsing_never_mutates_input() {
        let file = synthetic_pbp(&synthetic_sfo("X"), b"payload");
        let before = file.clone();
        let _ = parse_pbp_header(&file);
        assert_eq!(file, before);
    }

    // ------------------------------------------------------------------
    // Offset validation (section 25: non-monotonic, before-header, EOF)
    // ------------------------------------------------------------------

    #[test]
    fn valid_offsets_pass_validation() {
        let sfo = synthetic_sfo("ULUS10000");
        let file = synthetic_pbp(&sfo, b"payload");
        let fact = parse_pbp_header(&file).unwrap();
        assert!(validate_pbp_offsets(&fact, file.len() as u64).is_ok());
    }

    #[test]
    fn first_offset_before_header_is_rejected() {
        let mut fact = PbpHeaderFact {
            version: 1,
            section_offsets: [PBP_HEADER_BYTES as u32; PBP_SECTION_COUNT],
        };
        fact.section_offsets[PBP_SECTION_PARAM_SFO] = 4; // inside the fixed header
        assert_eq!(
            validate_pbp_offsets(&fact, 1_000_000),
            Err(PbpOffsetError::FirstOffsetBeforeHeader { offset: 4 })
        );
    }

    #[test]
    fn non_monotonic_offsets_are_rejected() {
        let mut fact = PbpHeaderFact {
            version: 1,
            section_offsets: [PBP_HEADER_BYTES as u32; PBP_SECTION_COUNT],
        };
        fact.section_offsets[PBP_SECTION_ICON0_PNG] = 9999;
        fact.section_offsets[PBP_SECTION_ICON1_PMF] = 100; // goes backwards
        assert_eq!(
            validate_pbp_offsets(&fact, 1_000_000),
            Err(PbpOffsetError::NonMonotonicOffsets {
                index: PBP_SECTION_ICON0_PNG,
                offset: 9999,
                next_offset: 100
            })
        );
    }

    #[test]
    fn offset_beyond_eof_is_rejected() {
        let mut fact = PbpHeaderFact {
            version: 1,
            section_offsets: [PBP_HEADER_BYTES as u32; PBP_SECTION_COUNT],
        };
        fact.section_offsets[PBP_SECTION_DATA_PSAR] = 5_000_000;
        assert_eq!(
            validate_pbp_offsets(&fact, 1000),
            Err(PbpOffsetError::OffsetBeyondEof {
                offset: 5_000_000,
                total_len: 1000
            })
        );
    }

    #[test]
    fn equal_adjacent_offsets_are_not_non_monotonic() {
        // A zero-length section (e.g. no ICON1.PMF) is legitimate.
        let fact = PbpHeaderFact {
            version: 1,
            section_offsets: [PBP_HEADER_BYTES as u32; PBP_SECTION_COUNT],
        };
        assert!(validate_pbp_offsets(&fact, 1_000_000).is_ok());
    }

    #[test]
    fn zero_offsets_are_before_header_not_panic() {
        let fact = PbpHeaderFact {
            version: 1,
            section_offsets: [0; PBP_SECTION_COUNT],
        };
        assert_eq!(
            validate_pbp_offsets(&fact, 1_000_000),
            Err(PbpOffsetError::FirstOffsetBeforeHeader { offset: 0 })
        );
    }

    // ------------------------------------------------------------------
    // PARAM.SFO section reuse
    // ------------------------------------------------------------------

    #[test]
    fn embedded_param_sfo_is_parsed_via_shared_parser() {
        let sfo = synthetic_sfo("ULUS10000");
        let file = synthetic_pbp(&sfo, b"payload");
        let fact = parse_pbp_header(&file).unwrap();
        let observation = read_pbp_param_sfo(&file, &fact).unwrap();
        assert_eq!(observation.get_text("DISC_ID"), Some("ULUS10000"));
    }

    #[test]
    fn malformed_embedded_sfo_yields_none_not_panic() {
        let file = synthetic_pbp(b"not a valid sfo blob", b"payload");
        let fact = parse_pbp_header(&file).unwrap();
        assert_eq!(read_pbp_param_sfo(&file, &fact), None);
    }

    #[test]
    fn param_sfo_section_out_of_bounds_yields_none_not_panic() {
        let mut fact = PbpHeaderFact {
            version: 1,
            section_offsets: [PBP_HEADER_BYTES as u32; PBP_SECTION_COUNT],
        };
        fact.section_offsets[PBP_SECTION_ICON0_PNG] = u32::MAX;
        let file = vec![0u8; 100];
        assert_eq!(read_pbp_param_sfo(&file, &fact), None);
    }

    #[test]
    fn data_psar_prefix_is_bounded_by_max_bytes() {
        let sfo = synthetic_sfo("X");
        let psar_payload = vec![0xABu8; 10_000];
        let file = synthetic_pbp(&sfo, &psar_payload);
        let fact = parse_pbp_header(&file).unwrap();
        let prefix = read_data_psar_prefix(&file, &fact, 16).unwrap();
        assert_eq!(prefix.len(), 16);
        assert!(prefix.iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn data_psar_prefix_never_reads_past_eof() {
        let sfo = synthetic_sfo("X");
        let psar_payload = vec![0xCDu8; 5];
        let file = synthetic_pbp(&sfo, &psar_payload);
        let fact = parse_pbp_header(&file).unwrap();
        let prefix = read_data_psar_prefix(&file, &fact, 10_000).unwrap();
        assert_eq!(prefix.len(), 5);
    }

    #[test]
    fn data_psar_prefix_out_of_bounds_start_yields_none() {
        let mut fact = PbpHeaderFact {
            version: 1,
            section_offsets: [0; PBP_SECTION_COUNT],
        };
        fact.section_offsets[PBP_SECTION_DATA_PSAR] = u32::MAX;
        let file = vec![0u8; 100];
        assert_eq!(read_data_psar_prefix(&file, &fact, 10), None);
    }

    // ------------------------------------------------------------------
    // Evidence
    // ------------------------------------------------------------------

    #[test]
    fn evidence_includes_container_and_product_code() {
        let sfo_bytes = synthetic_sfo("ULUS10000");
        let observation = parse_param_sfo(&sfo_bytes).unwrap();
        let evidence = observe_pbp_evidence(Some(&observation));
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::Container && item.value == "PBP")
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode
                    && item.value == "ULUS10000")
        );
    }

    #[test]
    fn evidence_without_sfo_is_container_only() {
        let evidence = observe_pbp_evidence(None);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, ContentEvidenceKind::Container);
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let sfo_bytes = synthetic_sfo("ULUS10000");
        let observation = parse_param_sfo(&sfo_bytes).unwrap();
        for item in observe_pbp_evidence(Some(&observation)) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::Container | ContentEvidenceKind::ProductCode
            ));
        }
    }
}
