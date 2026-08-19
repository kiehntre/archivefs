//! Pure, read-only PlayStation-style boot evidence extraction: `SYSTEM.CNF`
//! and a bounded PS-X EXE header check.
//!
//! Part of the `container -> media -> logical media reader ->
//! filesystem/root tree -> boot/layout observations` pipeline this whole
//! series builds toward - see [`crate::iso9660`]. This module is the next
//! layer: given an already-observed ISO9660 filesystem and a caller-
//! supplied [`crate::logical_media::LogicalMedia`], extract what
//! `SYSTEM.CNF` and the executable it names actually say, as neutral
//! [`crate::content_evidence::ContentEvidence`] - never a platform.
//!
//! # Reuse, not duplication
//!
//! [`crate::game_identity`] already has a reviewed, tested `SYSTEM.CNF`
//! `BOOT2=` parser (PS2) and a serial-normalization function. This module
//! does not re-implement serial normalization - it calls
//! [`crate::game_identity::serial_from_boot_path`] directly, the same
//! function `game_identity`'s own PS2 path uses, so a `SLUS_014.18;1`-
//! shaped filename normalizes identically everywhere in this crate. What
//! *is* new here: the `BOOT=` (not `BOOT2=`) key PS1 discs use, and
//! producing [`crate::content_evidence::ContentEvidence`] instead of
//! `game_identity`'s own `Verified`/`Candidate` report model - a
//! deliberately different, more conservative vocabulary for this
//! evidence-gathering pipeline (see the module documentation on
//! [`crate::content_evidence`] for why those two models are not merged).
//!
//! # Collision safety - read before treating any of this as identity
//!
//! - `SYSTEM.CNF` is not PlayStation-exclusive by construction; it is a
//!   plain text file at a conventional path, and its mere presence proves
//!   nothing about which PlayStation generation, let alone which platform.
//! - `BOOT=`/`BOOT2=` filenames are shared across PS1/PS2 (and the
//!   executable itself, `PS-X EXE` vs ELF, is what actually distinguishes
//!   them - this module only checks the PS1 `PS-X EXE` form).
//! - A serial candidate extracted from a boot filename is exactly that - a
//!   *candidate*. The same four-letter prefix family can recur across
//!   regions/reissues, and nothing here cross-checks it against a DAT or
//!   canonical release list.
//! - ISO9660 itself is a generic, cross-platform filesystem; observing one
//!   proves nothing about platform (see [`crate::iso9660`]'s own module
//!   documentation).
//!
//! Every parse here fails closed (`None`/no evidence) rather than guessing
//! at a malformed or oversized file - see [`parse_system_cnf_boot`].

use crate::content_detector::{ContentDetectionOutcome, ContentDetector, ContentDiagnostic};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::game_identity::{MAX_SYSTEM_CNF_BYTES, serial_from_boot_path};

/// The exact 8-byte ASCII magic a PS-X executable begins with, verified
/// against the community-maintained PlayStation technical reference
/// ("psx-spx", `https://psx-spx.consoledev.net/cdromfileformats/`): offset
/// `000h-007h` is the ASCII id `"PS-X EXE"`, followed by an 8-byte
/// zero-filled region before the rest of the (fixed 2048-byte) header.
pub const PSX_EXE_MAGIC: &[u8; 8] = b"PS-X EXE";

/// The fixed PS-X executable header size (one CD-ROM sector) - the bounded
/// read this module expects a caller to supply, though only the first 8
/// bytes are actually inspected for the magic.
pub const PSX_EXECUTABLE_HEADER_BYTES: usize = 2048;

/// What one `SYSTEM.CNF` boot-key line directly states, before any
/// evidence is derived from it.
///
/// `boot_key` is `"BOOT"` (PS1) or `"BOOT2"` (PS2, included for
/// completeness since both use the same file) - whichever key this parse
/// actually matched, never assumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemCnfBootFact {
    pub boot_key: &'static str,
    /// The exact, trimmed text after `=` - provenance, never rewritten.
    pub raw_value: String,
    /// `raw_value` with a recognised `cdrom:`/`cdrom0:` prefix, a
    /// leading path separator, and a trailing `;version` suffix removed -
    /// `None` if `raw_value` does not match that shape closely enough to
    /// normalize safely. See [`parse_system_cnf_boot`].
    pub executable_path: Option<String>,
    /// A candidate serial, via [`serial_from_boot_path`] on
    /// `executable_path` - `None` whenever that is `None`, or when the
    /// filename does not match the recognised serial shape.
    pub serial_candidate: Option<String>,
}

/// Parses a `SYSTEM.CNF`'s bytes for its `BOOT=`/`BOOT2=` line.
///
/// Bounded and fails closed: `bytes` longer than
/// [`MAX_SYSTEM_CNF_BYTES`] (64 KiB, the same bound
/// [`crate::game_identity`] already uses for the same file) returns `None`
/// outright, never a partial parse. Lines are split on `\n`, with any
/// trailing `\r` trimmed per line - both bare `\n` and `\r\n` line endings
/// are accepted. Matching is case-insensitive on the key
/// (`boot`/`Boot`/`BOOT` all match). The **first** `BOOT=` or `BOOT2=` line
/// found wins; a well-formed `SYSTEM.CNF` has only one.
///
/// A key with no recognisable `cdrom:`/`cdrom0:` prefix, or an empty/
/// traversal-shaped path, still returns a fact - `raw_value` is always
/// preserved - but `executable_path`/`serial_candidate` are `None` rather
/// than a guess. Returns `None` only when no `BOOT=`/`BOOT2=` line exists
/// at all, or the input exceeds the size bound.
pub fn parse_system_cnf_boot(bytes: &[u8]) -> Option<SystemCnfBootFact> {
    if bytes.len() as u64 > MAX_SYSTEM_CNF_BYTES {
        return None;
    }

    for line in bytes.split(|byte| *byte == b'\n') {
        let line = trim_ascii(trim_trailing_cr(line));
        let Some(equals) = line.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = trim_ascii(&line[..equals]);
        let boot_key = if key.eq_ignore_ascii_case(b"BOOT") {
            "BOOT"
        } else if key.eq_ignore_ascii_case(b"BOOT2") {
            "BOOT2"
        } else {
            continue;
        };

        let value = trim_ascii(&line[equals + 1..]);
        let raw_value = String::from_utf8_lossy(value).into_owned();
        let executable_path = normalize_boot_target(value);
        let serial_candidate = executable_path
            .as_ref()
            .and_then(|path| serial_from_boot_path(path.as_bytes()));

        return Some(SystemCnfBootFact {
            boot_key,
            raw_value,
            executable_path,
            serial_candidate,
        });
    }
    None
}

fn trim_trailing_cr(line: &[u8]) -> &[u8] {
    match line.last() {
        Some(b'\r') => &line[..line.len() - 1],
        _ => line,
    }
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

/// Strips a `cdrom:`/`cdrom0:` prefix (case-insensitive), leading path
/// separators, and a trailing `;version` suffix from a raw `BOOT=` value,
/// the same normalization [`crate::game_identity::parse_system_cnf_boot2`]
/// already applies for `BOOT2=`. Returns `None` (never a guess) when the
/// prefix is missing, the remaining path is empty or exceeds a generous
/// bound, or any path component is empty or a traversal segment (`.`/`..`).
fn normalize_boot_target(value: &[u8]) -> Option<String> {
    const MAX_PATH_BYTES: usize = 512;

    let lower: Vec<u8> = value.iter().map(u8::to_ascii_lowercase).collect();
    let prefix_len = if lower.starts_with(b"cdrom0:") {
        7
    } else if lower.starts_with(b"cdrom:") {
        6
    } else {
        return None;
    };
    let mut path = &value[prefix_len..];
    while path
        .first()
        .is_some_and(|byte| *byte == b'/' || *byte == b'\\')
    {
        path = &path[1..];
    }
    if let Some(version) = path.iter().position(|byte| *byte == b';') {
        path = &path[..version];
    }
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return None;
    }
    if path
        .split(|byte| *byte == b'/' || *byte == b'\\')
        .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        return None;
    }
    Some(String::from_utf8_lossy(path).into_owned())
}

/// Whether `header` begins with the [`PSX_EXE_MAGIC`]. `header` may be any
/// length; only the first 8 bytes are ever inspected.
pub fn looks_like_psx_exe(header: &[u8]) -> bool {
    header.len() >= PSX_EXE_MAGIC.len()
        && &header[..PSX_EXE_MAGIC.len()] == PSX_EXE_MAGIC.as_slice()
}

/// Turns a parsed [`SystemCnfBootFact`] into neutral evidence.
///
/// Always includes a [`ContentEvidenceKind::BootStructure`] fact naming the
/// boot key found (`Corroborated`: a real key/value assignment was parsed,
/// but a text config key is not proof of anything beyond its own
/// presence). Adds a [`ContentEvidenceKind::ProductCode`] fact
/// (`Corroborated`) only when a serial candidate was actually extracted -
/// see the module documentation's collision-safety notes for why this is
/// a candidate, never a platform claim.
pub fn observe_system_cnf_evidence(fact: &SystemCnfBootFact) -> Vec<ContentEvidence> {
    let mut evidence = vec![ContentEvidence::new(
        ContentEvidenceKind::BootStructure,
        fact.boot_key,
        ContentEvidenceConfidence::Corroborated,
        format!("SYSTEM.CNF declares {}={}", fact.boot_key, fact.raw_value),
    )];
    if let Some(serial) = &fact.serial_candidate {
        evidence.push(ContentEvidence::new(
            ContentEvidenceKind::ProductCode,
            serial.clone(),
            ContentEvidenceConfidence::Corroborated,
            "candidate serial parsed from the SYSTEM.CNF boot executable filename - not \
             verified against a canonical release list, and not proof of any one platform",
        ));
    }
    evidence
}

/// A [`ContentDetector`] operating on `SYSTEM.CNF`'s own bytes (not a whole
/// disc image - a caller locates and reads the file via
/// [`crate::iso9660::find_path`] first).
pub struct SystemCnfDetector;

impl ContentDetector for SystemCnfDetector {
    fn id(&self) -> &'static str {
        "system_cnf_boot"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if data.len() as u64 > MAX_SYSTEM_CNF_BYTES {
            return ContentDetectionOutcome::Malformed {
                evidence: Vec::new(),
                diagnostic: ContentDiagnostic {
                    detector_id: "system_cnf_boot",
                    category: "system_cnf_too_large",
                    message: format!(
                        "SYSTEM.CNF is {} bytes, exceeding the {MAX_SYSTEM_CNF_BYTES}-byte bound",
                        data.len()
                    ),
                },
            };
        }
        match parse_system_cnf_boot(data) {
            Some(fact) => ContentDetectionOutcome::Recognized {
                evidence: observe_system_cnf_evidence(&fact),
            },
            None => ContentDetectionOutcome::NotRecognized,
        }
    }
}

/// A [`ContentDetector`] operating on a bounded executable header (not a
/// whole disc image).
pub struct PsxExeDetector;

impl ContentDetector for PsxExeDetector {
    fn id(&self) -> &'static str {
        "psx_exe_magic"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_psx_exe(data) {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized {
            evidence: vec![ContentEvidence::new(
                ContentEvidenceKind::ContentSignature,
                "PS-X EXE",
                ContentEvidenceConfidence::Strong,
                "PS-X EXE magic present at the start of the boot executable",
            )],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_evidence::ContentEvidenceConfidence;

    // ------------------------------------------------------------------
    // SYSTEM.CNF BOOT= parsing
    // ------------------------------------------------------------------

    #[test]
    fn boot_line_is_parsed() {
        let fact = parse_system_cnf_boot(b"BOOT=cdrom:\\SLUS_014.18;1\r\nTCB=4\r\n").unwrap();
        assert_eq!(fact.boot_key, "BOOT");
        assert_eq!(fact.executable_path.as_deref(), Some("SLUS_014.18"));
    }

    #[test]
    fn whitespace_and_case_variation_are_tolerated() {
        let fact = parse_system_cnf_boot(b"  boot   =   cdrom:\\SLUS_014.18;1  \n").unwrap();
        assert_eq!(fact.boot_key, "BOOT");
        assert_eq!(fact.executable_path.as_deref(), Some("SLUS_014.18"));
    }

    #[test]
    fn crlf_and_lf_both_work() {
        let crlf = parse_system_cnf_boot(b"BOOT=cdrom:\\SLUS_014.18;1\r\n").unwrap();
        let lf = parse_system_cnf_boot(b"BOOT=cdrom:\\SLUS_014.18;1\n").unwrap();
        assert_eq!(crlf.executable_path, lf.executable_path);
    }

    #[test]
    fn version_suffix_is_stripped() {
        let with_suffix = parse_system_cnf_boot(b"BOOT=cdrom:\\SLES_123.45;1\n").unwrap();
        let without_suffix = parse_system_cnf_boot(b"BOOT=cdrom:\\SLES_123.45\n").unwrap();
        assert_eq!(with_suffix.executable_path, without_suffix.executable_path);
    }

    #[test]
    fn malformed_line_without_prefix_fails_safely() {
        let fact = parse_system_cnf_boot(b"BOOT=SLUS_014.18;1\n").unwrap();
        assert_eq!(fact.boot_key, "BOOT");
        assert_eq!(fact.raw_value, "SLUS_014.18;1");
        assert_eq!(fact.executable_path, None);
        assert_eq!(fact.serial_candidate, None);
    }

    #[test]
    fn oversized_system_cnf_is_bounded() {
        let huge = vec![b'A'; MAX_SYSTEM_CNF_BYTES as usize + 1];
        assert_eq!(parse_system_cnf_boot(&huge), None);
    }

    #[test]
    fn no_boot_key_returns_none() {
        assert_eq!(parse_system_cnf_boot(b"TCB=4\nEVENT=16\n"), None);
    }

    // ------------------------------------------------------------------
    // Serial extraction
    // ------------------------------------------------------------------

    #[test]
    fn known_serial_family_is_extracted() {
        let fact = parse_system_cnf_boot(b"BOOT=cdrom:\\SLUS_014.18;1\n").unwrap();
        assert_eq!(fact.serial_candidate.as_deref(), Some("SLUS-01418"));
    }

    #[test]
    fn unknown_serial_shape_is_preserved_but_not_promoted() {
        // A recognisable cdrom: path whose filename does not match the
        // XXXX_NNN.NN serial shape at all - executable_path is still
        // populated (the BOOT= line itself was fine), but no serial.
        let fact = parse_system_cnf_boot(b"BOOT=cdrom:\\MAIN.EXE;1\n").unwrap();
        assert_eq!(fact.executable_path.as_deref(), Some("MAIN.EXE"));
        assert_eq!(fact.serial_candidate, None);
    }

    // ------------------------------------------------------------------
    // PS-X EXE magic
    // ------------------------------------------------------------------

    #[test]
    fn psx_exe_magic_is_detected() {
        let mut header = vec![0u8; PSX_EXECUTABLE_HEADER_BYTES];
        header[0..8].copy_from_slice(PSX_EXE_MAGIC);
        assert!(looks_like_psx_exe(&header));
        assert!(PsxExeDetector.detect(&header).is_recognized());
    }

    #[test]
    fn missing_executable_header_is_not_recognized() {
        assert!(!looks_like_psx_exe(b"not an executable"));
        assert_eq!(
            PsxExeDetector.detect(b"ELF\0garbage"),
            ContentDetectionOutcome::NotRecognized
        );
    }

    // ------------------------------------------------------------------
    // Evidence / platform-safety boundary
    // ------------------------------------------------------------------

    #[test]
    fn generic_boot_key_evidence_never_assigns_a_platform() {
        let fact = parse_system_cnf_boot(b"BOOT=cdrom:\\SLUS_014.18;1\n").unwrap();
        let evidence = observe_system_cnf_evidence(&fact);
        for item in &evidence {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure | ContentEvidenceKind::ProductCode
            ));
        }
    }

    #[test]
    fn system_cnf_alone_does_not_assign_playstation() {
        // Structural: SystemCnfBootFact and the evidence derived from it
        // carry no platform field, and this module imports nothing from
        // crate::platform or crate::dat::identity.
        let fact = parse_system_cnf_boot(b"BOOT=cdrom:\\SLUS_014.18;1\n").unwrap();
        let evidence = observe_system_cnf_evidence(&fact);
        assert!(
            evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::BootStructure)
        );
    }

    #[test]
    fn boot_evidence_confidence_is_corroborated_not_strong() {
        let fact = parse_system_cnf_boot(b"BOOT=cdrom:\\SLUS_014.18;1\n").unwrap();
        let evidence = observe_system_cnf_evidence(&fact);
        for item in &evidence {
            assert_eq!(item.confidence, ContentEvidenceConfidence::Corroborated);
        }
    }

    #[test]
    fn no_serial_candidate_means_no_product_code_evidence() {
        let fact = parse_system_cnf_boot(b"BOOT=cdrom:\\MAIN.EXE;1\n").unwrap();
        let evidence = observe_system_cnf_evidence(&fact);
        assert!(
            !evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ProductCode)
        );
    }

    #[test]
    fn parsing_never_mutates_input() {
        let data = b"BOOT=cdrom:\\SLUS_014.18;1\n".to_vec();
        let before = data.clone();
        let _ = parse_system_cnf_boot(&data);
        assert_eq!(data, before);
    }

    #[test]
    fn repeated_parse_is_deterministic() {
        let data = b"BOOT=cdrom:\\SLUS_014.18;1\n";
        assert_eq!(parse_system_cnf_boot(data), parse_system_cnf_boot(data));
    }
}
