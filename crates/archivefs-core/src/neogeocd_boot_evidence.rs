//! Pure, read-only Neo Geo CD boot evidence: `IPL.TXT`, the disc's own
//! Initial Program Load manifest.
//!
//! # Why `IPL.TXT`, not ISO9660
//!
//! Neo Geo CD discs are plain ISO9660 CD-ROMs - per the task's own
//! collision-safety rule, ISO9660 alone is never platform evidence (PSX,
//! Sega CD, CD-i, 3DO, and PC Engine CD all share the same generic
//! filesystem fact). `IPL.TXT` is the actual Neo Geo CD-specific internal
//! structure: a small, tightly-specified manifest the system ROM's loader
//! reads at boot to know which files to load into which memory bank. Its
//! presence *and* structurally valid content is a far more specific,
//! platform-relevant fact than the filesystem it happens to live on.
//!
//! # Format verified, not assumed
//!
//! Cross-checked against the NeoGeo Development Wiki's dedicated `IPL
//! file` page (`https://wiki.neogeodev.org/index.php?title=IPL_file`), a
//! long-maintained homebrew-community reference:
//!
//! ```text
//! IPL.TXT (root of the disc, name upper-cased by the loader):
//!   one line per entry: "FILENAME.EXT,BANK,OFFSET\r\n"
//!     FILENAME.EXT - 8.3 format, upper-cased
//!     BANK         - a single hex digit
//!     OFFSET       - 1 to 8 hex digits
//!   at most 32 entries
//!   the whole file must end with the byte 0x1A
//! ```
//!
//! File *type* (PRG/FIX/SPR/Z80/PCM/PAT/OBJ/...) is determined by the
//! filename's extension, per the same page's recognized-extension table;
//! this module only recognizes the five types the system ROM's loader
//! requires at minimum (PRG, FIX, SPR, Z80, PCM) - enough to report
//! whether the manifest looks bootable, without hand-modeling every
//! possible extension.
//!
//! # Collision safety
//!
//! No independently-corroborated source in this research pass identifies
//! any serial/catalog/product-code field anywhere in `IPL.TXT` - it is a
//! load manifest, not a release descriptor - so this module never emits a
//! `ProductCode` fact.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};

/// Bound on the number of bytes this parser will ever look at - real
/// `IPL.TXT` files are well under 1 KiB (32 entries at a few dozen bytes
/// each); this leaves generous headroom without admitting an unbounded
/// scan.
pub const MAX_IPL_TXT_BYTES: usize = 8192;
pub const MAX_IPL_ENTRIES: usize = 32;
const IPL_TERMINATOR: u8 = 0x1A;

const REQUIRED_EXTENSIONS: &[&str] = &["PRG", "FIX", "SPR", "Z80", "PCM"];

/// One parsed `IPL.TXT` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IplEntry {
    pub filename: String,
    pub bank: String,
    pub offset: String,
}

impl IplEntry {
    /// The filename's extension, upper-cased - `None` if it has none.
    pub fn extension(&self) -> Option<String> {
        self.filename
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_uppercase())
    }
}

/// What was observed about an `IPL.TXT` candidate - never a platform
/// decision.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IplTxtFact {
    pub entries: Vec<IplEntry>,
    /// Whether the byte [`IPL_TERMINATOR`] was found within
    /// [`MAX_IPL_TXT_BYTES`].
    pub terminator_present: bool,
}

impl IplTxtFact {
    /// Whether every one of PRG/FIX/SPR/Z80/PCM appears among the parsed
    /// entries' extensions - the minimum set the system ROM's loader
    /// requires to boot at all, per the module documentation. A stronger
    /// structural signal than mere presence of *some* valid entries, but
    /// still never platform proof by itself.
    pub fn has_required_extensions(&self) -> bool {
        REQUIRED_EXTENSIONS.iter().all(|required| {
            self.entries
                .iter()
                .any(|entry| entry.extension().as_deref() == Some(required))
        })
    }

    /// Whether this parsed as a structurally plausible manifest at all:
    /// at least one entry, no more than [`MAX_IPL_ENTRIES`], and the
    /// terminator byte present.
    pub fn is_structurally_valid(&self) -> bool {
        !self.entries.is_empty() && self.entries.len() <= MAX_IPL_ENTRIES && self.terminator_present
    }
}

fn parse_entry(line: &str) -> Option<IplEntry> {
    let mut fields = line.splitn(3, ',');
    let filename = fields.next()?.trim();
    let bank = fields.next()?.trim();
    let offset = fields.next()?.trim();
    if fields.next().is_some() {
        return None; // more than three comma-separated fields - malformed
    }

    let (name, ext) = filename.split_once('.').unwrap_or((filename, ""));
    if name.is_empty()
        || name.len() > 8
        || ext.len() > 3
        || !filename
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return None;
    }
    if bank.is_empty() || bank.len() > 1 || !bank.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if offset.is_empty() || offset.len() > 8 || !offset.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some(IplEntry {
        filename: filename.to_ascii_uppercase(),
        bank: bank.to_ascii_uppercase(),
        offset: offset.to_ascii_uppercase(),
    })
}

/// Parses an `IPL.TXT` candidate from `bytes` (at most
/// [`MAX_IPL_TXT_BYTES`] are ever examined). Never panics: an
/// unparseable/malformed line is simply skipped (not counted as an entry),
/// so a caller can distinguish "nothing at all parsed" from "some lines
/// parsed, some did not" via `entries.is_empty()`/
/// [`IplTxtFact::is_structurally_valid`]. Always returns a fact (never
/// `None`) - "an IPL.TXT-shaped file with zero valid entries" is itself a
/// legitimate, reportable observation, not a parse failure.
pub fn parse_ipl_txt(bytes: &[u8]) -> IplTxtFact {
    let bound = bytes.len().min(MAX_IPL_TXT_BYTES);
    let bytes = &bytes[..bound];
    let terminator_present = bytes.contains(&IPL_TERMINATOR);

    let text_end = bytes
        .iter()
        .position(|&b| b == IPL_TERMINATOR)
        .unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..text_end]);

    let entries: Vec<IplEntry> = text
        .split("\r\n")
        .flat_map(|line| line.split('\n'))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(MAX_IPL_ENTRIES + 1) // +1 so an over-long manifest is still observed as over-long, not silently truncated to "valid"
        .filter_map(parse_entry)
        .collect();

    IplTxtFact {
        entries,
        terminator_present,
    }
}

/// Neutral evidence: `Strong` `BootStructure` = `"IPL.TXT"` only when
/// [`IplTxtFact::is_structurally_valid`] - a file that merely happens to be
/// named `IPL.TXT` but does not parse as one yields no evidence at all,
/// consistent with every other multi-field structural check in this crate.
pub fn observe_neogeocd_evidence(fact: &IplTxtFact) -> Vec<ContentEvidence> {
    if !fact.is_structurally_valid() {
        return Vec::new();
    }
    let detail = if fact.has_required_extensions() {
        "IPL.TXT parsed with a valid entry list, terminator present, and all five loader-required file types (PRG/FIX/SPR/Z80/PCM) present"
    } else {
        "IPL.TXT parsed with a valid entry list and terminator present, though not every loader-required file type was found"
    };
    vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        "IPL.TXT",
        ContentEvidenceConfidence::Strong,
        detail,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ipl_txt() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MAIN.PRG,0,00000000\r\n");
        bytes.extend_from_slice(b"MAIN.FIX,0,00010000\r\n");
        bytes.extend_from_slice(b"MAIN.SPR,1,00000000\r\n");
        bytes.extend_from_slice(b"MAIN.Z80,0,00000000\r\n");
        bytes.extend_from_slice(b"MAIN.PCM,2,00000000\r\n");
        bytes.push(0x1A);
        bytes
    }

    #[test]
    fn valid_manifest_parses_all_entries() {
        let fact = parse_ipl_txt(&sample_ipl_txt());
        assert_eq!(fact.entries.len(), 5);
        assert!(fact.terminator_present);
        assert!(fact.is_structurally_valid());
    }

    #[test]
    fn required_extensions_are_detected() {
        let fact = parse_ipl_txt(&sample_ipl_txt());
        assert!(fact.has_required_extensions());
    }

    #[test]
    fn missing_required_extension_is_detected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MAIN.PRG,0,00000000\r\n");
        bytes.push(0x1A);
        let fact = parse_ipl_txt(&bytes);
        assert!(!fact.has_required_extensions());
        assert!(fact.is_structurally_valid());
    }

    #[test]
    fn entry_fields_are_uppercased() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"main.prg,a,1f\r\n");
        bytes.push(0x1A);
        let fact = parse_ipl_txt(&bytes);
        let entry = &fact.entries[0];
        assert_eq!(entry.filename, "MAIN.PRG");
        assert_eq!(entry.bank, "A");
        assert_eq!(entry.offset, "1F");
    }

    #[test]
    fn missing_terminator_is_not_structurally_valid() {
        let bytes = b"MAIN.PRG,0,00000000\r\n".to_vec();
        let fact = parse_ipl_txt(&bytes);
        assert!(!fact.terminator_present);
        assert!(!fact.is_structurally_valid());
    }

    #[test]
    fn empty_bytes_yields_no_entries_not_panic() {
        let fact = parse_ipl_txt(&[]);
        assert!(fact.entries.is_empty());
        assert!(!fact.is_structurally_valid());
    }

    #[test]
    fn too_many_entries_is_not_structurally_valid() {
        let mut bytes = Vec::new();
        for i in 0..40 {
            bytes.extend_from_slice(format!("FILE{i:04}.PRG,0,{i:08x}\r\n").as_bytes());
        }
        bytes.push(0x1A);
        let fact = parse_ipl_txt(&bytes);
        assert!(fact.entries.len() > MAX_IPL_ENTRIES);
        assert!(!fact.is_structurally_valid());
    }

    #[test]
    fn malformed_line_is_skipped_not_panicking() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MAIN.PRG,0,00000000\r\n");
        bytes.extend_from_slice(b"this line has no commas at all\r\n");
        bytes.extend_from_slice(b"too,many,commas,here,at,all\r\n");
        bytes.push(0x1A);
        let fact = parse_ipl_txt(&bytes);
        assert_eq!(fact.entries.len(), 1);
    }

    #[test]
    fn bank_field_longer_than_one_hex_digit_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MAIN.PRG,12,00000000\r\n");
        bytes.push(0x1A);
        let fact = parse_ipl_txt(&bytes);
        assert!(fact.entries.is_empty());
    }

    #[test]
    fn non_hex_offset_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"MAIN.PRG,0,ZZZZZZZZ\r\n");
        bytes.push(0x1A);
        let fact = parse_ipl_txt(&bytes);
        assert!(fact.entries.is_empty());
    }

    #[test]
    fn oversized_input_is_bounded_not_scanned_fully() {
        let mut bytes = vec![b'A'; MAX_IPL_TXT_BYTES * 4];
        bytes[MAX_IPL_TXT_BYTES * 3] = 0x1A; // terminator far past the bound
        let fact = parse_ipl_txt(&bytes);
        // The terminator beyond MAX_IPL_TXT_BYTES must not be seen.
        assert!(!fact.terminator_present);
    }

    #[test]
    fn extension_is_extracted_uppercased() {
        let entry = IplEntry {
            filename: "MAIN.PRG".to_string(),
            bank: "0".to_string(),
            offset: "0".to_string(),
        };
        assert_eq!(entry.extension().as_deref(), Some("PRG"));
    }

    #[test]
    fn valid_manifest_yields_strong_boot_structure_evidence() {
        let fact = parse_ipl_txt(&sample_ipl_txt());
        let evidence = observe_neogeocd_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, ContentEvidenceKind::BootStructure);
        assert_eq!(evidence[0].value, "IPL.TXT");
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Strong);
    }

    #[test]
    fn invalid_manifest_yields_no_evidence() {
        let fact = parse_ipl_txt(b"not an ipl.txt at all, no terminator");
        assert!(observe_neogeocd_evidence(&fact).is_empty());
    }

    #[test]
    fn evidence_never_includes_product_code() {
        let fact = parse_ipl_txt(&sample_ipl_txt());
        for item in observe_neogeocd_evidence(&fact) {
            assert_ne!(item.kind, ContentEvidenceKind::ProductCode);
        }
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let fact = parse_ipl_txt(&sample_ipl_txt());
        for item in observe_neogeocd_evidence(&fact) {
            assert!(matches!(item.kind, ContentEvidenceKind::BootStructure));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let fact = parse_ipl_txt(&sample_ipl_txt());
        assert_eq!(
            observe_neogeocd_evidence(&fact),
            observe_neogeocd_evidence(&fact)
        );
    }
}
