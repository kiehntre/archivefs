//! Safe, local-only resolution of a DAT's `DOCTYPE` external identifier -
//! provenance and diagnostics only, never DTD content-model validation and
//! never a second XML/DTD parser.
//!
//! # What this is not
//!
//! `quick-xml` (`default-features = false`, see `parsers::logiqx`) performs
//! no DTD processing at all: it never fetches an external DTD, never
//! resolves an external entity, and never validates document structure
//! against a DTD's grammar. Nothing here changes that - this module never
//! feeds a resolved DTD path back into the XML parser, never opens a
//! resolved DTD file's *contents*, and never claims a DAT was "DTD
//! validated". It answers exactly one narrower question, safely: **does
//! this DOCTYPE's external identifier name a DTD EmuWiz recognises, and if
//! so, is a trusted local copy of it available** - for provenance display
//! and as a seam a later milestone could build real validation on top of,
//! never for anything this milestone does itself.
//!
//! # Threat model / what is rejected outright
//!
//! A `SYSTEM`/`PUBLIC` external identifier is untrusted input from the DAT
//! file. This module never:
//! - makes a network request of any kind (no `http`/`https`/`ftp` scheme is
//!   ever treated as resolvable - see [`classify_system_literal`]),
//! - opens `file://` or any other URI scheme,
//! - accepts an absolute filesystem path,
//! - accepts `..` traversal or any path with more than one component,
//! - accepts a name that is not one of a short, explicit allowlist
//!   ([`TRUSTED_DTDS`]),
//! - reads or interprets anything from a `DOCTYPE`'s internal subset
//!   (`[ ... ]`) - entity declarations there are never looked at, matching
//!   the parser's existing "never expand a declared entity" guarantee.
//!
//! Every rejection is non-fatal: an unresolved or unsafe DOCTYPE reference
//! never fails DAT parsing (see [`classify_doctype`]'s doc comment).

use std::path::{Path, PathBuf};

/// One DTD EmuWiz recognises well enough to name and (maybe) resolve
/// locally. Deliberately a short, explicit allowlist - never derived from
/// the DAT itself.
#[derive(Debug, Clone, Copy)]
pub struct TrustedDtd {
    /// Stable, human-readable name for diagnostics (e.g. `"Logiqx"`).
    pub name: &'static str,
    /// The exact basename a `SYSTEM` identifier must equal, byte-for-byte,
    /// for the same-directory-as-DAT resolution tier to apply at all. Never
    /// a pattern - see [`classify_system_literal`].
    pub allowed_basename: &'static str,
    /// `PUBLIC` identifiers that name this same DTD. Real-world Logiqx DATs
    /// carry `PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://
    /// www.logiqx.com/Dats/datafile.dtd"` - the `SYSTEM` half is a URL
    /// (never resolvable locally), but the `PUBLIC` half still lets EmuWiz
    /// *name* the DTD for diagnostics/tier-A lookup even when tier B (beside
    /// the DAT) can never apply to that particular reference.
    pub public_ids: &'static [&'static str],
    /// The filename a bundled/installed local copy would have, if EmuWiz
    /// ever ships or the user installs one - resolved under
    /// [`default_trusted_dtd_dir`]. `None` here for every entry today: no
    /// DTD is bundled with this build yet. The tier itself is still real
    /// and checked (see [`resolve`]); it simply has nothing to find until a
    /// file is actually placed there.
    pub bundled_filename: Option<&'static str>,
}

/// The whole trusted-DTD registry. Logiqx only, per the milestone's scope -
/// a second entry (e.g. for a different DAT dialect with its own DOCTYPE)
/// is a one-line addition here, never a reason to touch the resolution
/// logic below.
pub const TRUSTED_DTDS: &[TrustedDtd] = &[TrustedDtd {
    name: "Logiqx",
    allowed_basename: "logiqx.dtd",
    public_ids: &["-//Logiqx//DTD ROM Management Datafile//EN"],
    bundled_filename: None,
}];

/// Where a bundled/installed trusted DTD would live:
/// `<EmuWiz data dir>/dtds`. Never a source-controlled path, never anything
/// derived from the DAT being parsed. Only ever read from (existence/
/// canonicalization checks) - this module never creates this directory.
pub fn default_trusted_dtd_dir() -> crate::Result<PathBuf> {
    Ok(crate::app_dirs::data_dir()?.join("dtds"))
}

/// The external identifier form a `DOCTYPE` declared, if any - parsed only
/// far enough to extract the literal `SYSTEM`/`PUBLIC` strings XML's own
/// `<!DOCTYPE root ...>` grammar puts immediately after the root name. This
/// is not a DTD parser: it never looks past the external identifier, and in
/// particular never looks inside an internal subset (`[ ... ]`), so an
/// entity declaration there - however it's spelled - is never seen by this
/// code at all, exactly like it is never seen by `quick-xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExternalId {
    System(String),
    Public {
        public_id: String,
        system: Option<String>,
    },
}

/// Parses the raw bytes `quick-xml`'s `Event::DocType` hands back (the text
/// between `<!DOCTYPE` and the terminating `>`, not re-parsed by any XML or
/// DTD grammar - just plain string/quote scanning over a bounded, already
/// size-limited buffer) into an [`ExternalId`], if the declaration has one.
///
/// Deliberately tolerant of anything malformed: invalid UTF-8, an
/// unterminated quote, a missing keyword, or a truncated declaration all
/// simply produce `None` (still `str::find`/slice on already-validated `&str`
/// byte offsets throughout, so this can never panic on adversarial input) -
/// see `malformed_doctype_cannot_panic` for the regression test.
fn parse_external_id(raw: &[u8]) -> Option<ExternalId> {
    let text = std::str::from_utf8(raw).ok()?;
    // Skip the root name (the first whitespace-delimited token) - the
    // external identifier keyword follows it.
    let after_root = text.trim_start();
    let root_end = after_root.find(|ch: char| ch.is_whitespace())?;
    let rest = after_root[root_end..].trim_start();

    if let Some(after_keyword) = rest.strip_prefix("SYSTEM") {
        let system = take_quoted_literal(after_keyword)?;
        return Some(ExternalId::System(system));
    }
    if let Some(after_keyword) = rest.strip_prefix("PUBLIC") {
        let (public_id, remainder) = take_quoted_literal_with_remainder(after_keyword)?;
        let system = take_quoted_literal(remainder);
        return Some(ExternalId::Public { public_id, system });
    }
    None
}

/// Extracts one `"..."`/`'...'`-quoted literal from the start of `text`
/// (after skipping leading whitespace), ignoring anything after its closing
/// quote. `None` if `text` does not start with a quote or the quote is
/// never closed.
fn take_quoted_literal(text: &str) -> Option<String> {
    take_quoted_literal_with_remainder(text).map(|(literal, _)| literal)
}

/// Like [`take_quoted_literal`], but also returns the text following the
/// closing quote, so a second literal (`PUBLIC`'s optional system literal)
/// can be read from it in turn.
fn take_quoted_literal_with_remainder(text: &str) -> Option<(String, &str)> {
    let trimmed = text.trim_start();
    let quote = trimmed.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let after_quote = &trimmed[quote.len_utf8()..];
    let end = after_quote.find(quote)?;
    let literal = after_quote[..end].to_string();
    let remainder = &after_quote[end + quote.len_utf8()..];
    Some((literal, remainder))
}

/// What a `SYSTEM` literal (or a `PUBLIC` declaration's optional trailing
/// `SYSTEM` literal) turned out to be, safety-wise.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SystemLiteralShape {
    /// A single safe path component - no separator, no `..`, non-empty, and
    /// not itself shaped like a URI scheme (`something:`). The only shape
    /// [`resolve`] ever considers for the same-directory-as-DAT tier.
    SimpleBasename(String),
    /// Recognised as unsafe/non-local by construction - a URL, an absolute
    /// path, traversal, a multi-component relative path, or empty. Carries
    /// a human-readable reason for diagnostics.
    Rejected(&'static str),
}

/// Classifies a raw `SYSTEM` literal string. Never touches the filesystem -
/// purely string inspection, so it can be applied uniformly whether the
/// literal came from a `SYSTEM` or a `PUBLIC` declaration's trailing system
/// literal.
fn classify_system_literal(system: &str) -> SystemLiteralShape {
    if system.is_empty() {
        return SystemLiteralShape::Rejected("the SYSTEM identifier is empty");
    }
    // Catches `http:`, `https:`, `ftp:`, `file:`, and any other
    // `scheme:...` URI form, plus a Windows drive letter (`C:\...`) - both
    // are rejected for the same reason: this is not a plain local
    // filename. Checked before the separator check so a message naming the
    // URI scheme specifically is preferred when both are true (a URL also
    // contains `/`).
    if let Some(colon) = system.find(':') {
        let scheme = &system[..colon];
        if !scheme.is_empty() && scheme.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return SystemLiteralShape::Rejected(
                "the SYSTEM identifier names a URL/URI scheme, not a local filename",
            );
        }
    }
    if system.contains('/') || system.contains('\\') {
        return SystemLiteralShape::Rejected(
            "the SYSTEM identifier contains a path separator - only a bare filename beside the \
             DAT is ever trusted",
        );
    }
    if system == "." || system == ".." {
        return SystemLiteralShape::Rejected("the SYSTEM identifier is a directory reference");
    }
    SystemLiteralShape::SimpleBasename(system.to_string())
}

/// One resolved location for a trusted DTD's local copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DtdSource {
    /// Resolved under [`default_trusted_dtd_dir`] (tier A).
    Bundled(PathBuf),
    /// Resolved directly beside the DAT file (tier B) - already
    /// canonicalized and verified to still resolve inside the DAT's own
    /// directory (see [`resolve_beside_dat`]).
    BesideDat(PathBuf),
}

impl DtdSource {
    pub fn path(&self) -> &Path {
        match self {
            Self::Bundled(path) | Self::BesideDat(path) => path,
        }
    }
}

/// The truthful outcome of looking at one DAT's `DOCTYPE` (or its absence).
/// Every variant is safe to surface directly to a person - see
/// `describe_doctype_outcome` for the exact wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctypeOutcome {
    /// The document had no `DOCTYPE` at all.
    NoDoctype,
    /// The `DOCTYPE` named a DTD in [`TRUSTED_DTDS`], and a trusted local
    /// copy was actually found (tier A or B) - resolution only, **not** a
    /// claim that the DAT was validated against it.
    TrustedDtdResolved {
        name: &'static str,
        source: DtdSource,
    },
    /// The `DOCTYPE` named a DTD in [`TRUSTED_DTDS`], but no trusted local
    /// copy could be found (bundled/installed absent, and either no
    /// same-directory candidate applies or none exists there).
    TrustedDtdUnavailable { name: &'static str },
    /// The `DOCTYPE` either named nothing EmuWiz recognises, or its
    /// external identifier was not a shape this module ever resolves
    /// locally (a URL, an absolute path, traversal, ...). Nothing was
    /// fetched, opened outside the allowed directory, or otherwise acted
    /// on; `reason` is safe, static, human-readable text - never an echo of
    /// attacker-controlled content.
    UnsafeOrUnknownDoctypeIgnored { reason: &'static str },
}

/// Classifies one `DOCTYPE` declaration's raw bytes (as `quick-xml`'s
/// `Event::DocType` hands them back) against [`TRUSTED_DTDS`], resolving a
/// same-directory candidate against `dat_path`'s own directory.
///
/// Read-only and infallible: this only ever calls `fs::canonicalize`/
/// `fs::symlink_metadata`-style existence checks (via [`resolve`]), never
/// opens a file's contents, and every unresolvable/unsafe case maps to
/// [`DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored`] or
/// [`DoctypeOutcome::TrustedDtdUnavailable`] rather than an error - callers
/// must never treat any outcome here as a reason to fail DAT parsing.
pub fn classify_doctype(raw_doctype_text: &[u8], dat_path: &Path) -> DoctypeOutcome {
    let Some(external_id) = parse_external_id(raw_doctype_text) else {
        return DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored {
            reason: "the DOCTYPE declared no external SYSTEM/PUBLIC identifier to resolve",
        };
    };

    match external_id {
        ExternalId::System(system) => resolve_by_system_literal(&system, dat_path),
        ExternalId::Public { public_id, system } => {
            if let Some(trusted) = TRUSTED_DTDS
                .iter()
                .find(|candidate| candidate.public_ids.contains(&public_id.as_str()))
            {
                // The PUBLIC id alone already names the DTD; try the system
                // literal (if any) for a same-directory candidate too, but
                // "unavailable" - never "unknown" - is the correct outcome
                // either way, since the DTD *is* recognised.
                if let Some(system) = &system
                    && let SystemLiteralShape::SimpleBasename(basename) =
                        classify_system_literal(system)
                    && basename == trusted.allowed_basename
                    && let Some(source) = resolve_beside_dat(dat_path, &basename)
                {
                    return DoctypeOutcome::TrustedDtdResolved {
                        name: trusted.name,
                        source,
                    };
                }
                return resolve(trusted);
            }
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored {
                reason: "the PUBLIC identifier does not name a DTD EmuWiz trusts",
            }
        }
    }
}

fn resolve_by_system_literal(system: &str, dat_path: &Path) -> DoctypeOutcome {
    match classify_system_literal(system) {
        SystemLiteralShape::Rejected(reason) => {
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { reason }
        }
        SystemLiteralShape::SimpleBasename(basename) => {
            let Some(trusted) = TRUSTED_DTDS
                .iter()
                .find(|candidate| candidate.allowed_basename == basename)
            else {
                return DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored {
                    reason: "the SYSTEM identifier does not name a DTD EmuWiz trusts",
                };
            };
            if let Some(source) = resolve_beside_dat(dat_path, &basename) {
                return DoctypeOutcome::TrustedDtdResolved {
                    name: trusted.name,
                    source,
                };
            }
            resolve(trusted)
        }
    }
}

/// Tries tier A (bundled/installed) for a DTD already known to be trusted
/// by name - tier B (beside the DAT) is always tried first by the two call
/// sites above, since it needs the original `SYSTEM` literal's exact
/// basename, which is not part of `TrustedDtd` when reached only via a
/// `PUBLIC` id.
fn resolve(trusted: &TrustedDtd) -> DoctypeOutcome {
    if let Some(filename) = trusted.bundled_filename
        && let Ok(dir) = default_trusted_dtd_dir()
        && let Some(path) = existing_file_directly_inside(&dir, filename)
    {
        return DoctypeOutcome::TrustedDtdResolved {
            name: trusted.name,
            source: DtdSource::Bundled(path),
        };
    }
    DoctypeOutcome::TrustedDtdUnavailable { name: trusted.name }
}

/// Tier B: a candidate `basename` file directly beside `dat_path`, resolved
/// and verified exactly as [`existing_file_directly_inside`] does for tier
/// A - see that function's doc comment for the safety property this
/// enforces (a symlink cannot smuggle in a file from outside the DAT's own
/// directory).
fn resolve_beside_dat(dat_path: &Path, basename: &str) -> Option<DtdSource> {
    let dat_dir = dat_path.parent()?;
    existing_file_directly_inside(dat_dir, basename).map(DtdSource::BesideDat)
}

/// Resolves `dir.join(filename)`, but only returns it if it actually exists
/// as a regular file **and** - after fully resolving every symlink in its
/// path - its resolved parent directory is still exactly `dir`'s own
/// resolved form. This is what stops `logiqx.dtd` beside a DAT (or in the
/// bundled directory) from being a symlink that quietly points somewhere
/// else entirely: resolution only ever trusts a file that is genuinely
/// inside the one allowed directory once every symlink has been followed,
/// never merely a path that was *typed* as if it were.
///
/// Never reads the file's contents - existence/location only.
fn existing_file_directly_inside(dir: &Path, filename: &str) -> Option<PathBuf> {
    let canonical_dir = std::fs::canonicalize(dir).ok()?;
    let candidate = dir.join(filename);
    let canonical_candidate = std::fs::canonicalize(&candidate).ok()?;
    if !canonical_candidate.is_file() {
        return None;
    }
    if canonical_candidate.parent() != Some(canonical_dir.as_path()) {
        return None;
    }
    Some(canonical_candidate)
}

/// The exact, plain-English sentence a person sees for one [`DoctypeOutcome`].
/// Deliberately never says anything resembling "DTD validation passed" -
/// see this module's own top-of-file doc comment for why that claim would
/// be false with the current parser.
pub fn describe_doctype_outcome(outcome: &DoctypeOutcome) -> String {
    match outcome {
        DoctypeOutcome::NoDoctype => String::new(),
        DoctypeOutcome::TrustedDtdResolved { name, .. } => format!(
            "{name} DTD referenced. A trusted local copy was found. This build does not \
             perform DTD schema validation."
        ),
        DoctypeOutcome::TrustedDtdUnavailable { name } => format!(
            "{name} DTD referenced, but no trusted local copy was found. The DAT was parsed \
             normally without DTD validation."
        ),
        DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { reason } => format!(
            "External or unknown DTD reference ignored for security ({reason}). No network or \
             arbitrary filesystem access was allowed."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "archivefs-trusted-dtd-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn simple_system_basename_is_parsed() {
        let id = parse_external_id(br#"datafile SYSTEM "logiqx.dtd""#).unwrap();
        assert_eq!(id, ExternalId::System("logiqx.dtd".to_string()));
    }

    #[test]
    fn public_declaration_is_parsed_with_its_system_literal() {
        let id = parse_external_id(
            br#"datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd""#,
        )
        .unwrap();
        assert_eq!(
            id,
            ExternalId::Public {
                public_id: "-//Logiqx//DTD ROM Management Datafile//EN".to_string(),
                system: Some("http://www.logiqx.com/Dats/datafile.dtd".to_string()),
            }
        );
    }

    #[test]
    fn malformed_doctype_cannot_panic() {
        for raw in [
            &b""[..],
            b"datafile",
            b"datafile SYSTEM",
            b"datafile SYSTEM \"unterminated",
            b"datafile PUBLIC \"only-one-literal",
            b"\xff\xfe not valid utf8",
            b"   ",
            b"datafile [ <!ENTITY xxe SYSTEM \"file:///etc/passwd\"> ]",
        ] {
            let _ = parse_external_id(raw);
            let _ = classify_doctype(raw, Path::new("/tmp/does-not-matter.dat"));
        }
    }

    #[test]
    fn internal_subset_entity_declarations_are_never_inspected() {
        // Even a DOCTYPE whose external id looks trusted, but which also
        // carries an XXE-shaped internal subset, must resolve purely from
        // the external id - the internal subset is never parsed at all, so
        // it cannot influence the outcome.
        let raw = br#"datafile SYSTEM "logiqx.dtd" [ <!ENTITY xxe SYSTEM "file:///etc/passwd"> <!ENTITY net SYSTEM "http://evil.example/x"> ]"#;
        let outcome = classify_doctype(raw, Path::new("/nonexistent/does-not-matter.dat"));
        // No local logiqx.dtd exists beside a nonexistent path, so this
        // must be "unavailable", never an error, and never anything that
        // implies the entity text was read.
        assert_eq!(
            outcome,
            DoctypeOutcome::TrustedDtdUnavailable { name: "Logiqx" }
        );
    }

    #[test]
    fn no_doctype_case_is_represented_by_the_caller_never_this_module() {
        // `DoctypeOutcome::NoDoctype` is produced by the parser simply never
        // calling `classify_doctype` at all - there is no "empty" raw
        // DOCTYPE text to feed this module that would itself mean "absent".
        // This test documents that division of responsibility.
        assert_eq!(describe_doctype_outcome(&DoctypeOutcome::NoDoctype), "");
    }

    #[test]
    fn https_url_system_identifier_is_rejected() {
        let dir = test_dir("https-rejected");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(
            br#"datafile SYSTEM "https://example.com/logiqx.dtd""#,
            &dat_path,
        );
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_url_system_identifier_is_rejected() {
        let dir = test_dir("http-rejected");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(
            br#"datafile SYSTEM "http://www.logiqx.com/Dats/datafile.dtd""#,
            &dat_path,
        );
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_scheme_system_identifier_is_rejected() {
        let dir = test_dir("file-scheme-rejected");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(br#"datafile SYSTEM "file:///etc/passwd""#, &dat_path);
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn absolute_path_system_identifier_is_rejected() {
        let dir = test_dir("absolute-path-rejected");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(br#"datafile SYSTEM "/etc/passwd""#, &dat_path);
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_drive_letter_path_system_identifier_is_rejected() {
        // `C:\path\to\logiqx.dtd` - the leading `C:` is caught by the same
        // URI-scheme check that rejects `http:`/`file:` (a single-letter
        // "scheme" is still alphanumeric-only, so it matches), and even if
        // it did not, the `\` path separator would still reject it. Either
        // way this must never resolve, and never be treated as though it
        // named the trusted `logiqx.dtd` basename.
        let dir = test_dir("windows-drive-letter-rejected");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(br#"datafile SYSTEM "C:\path\to\logiqx.dtd""#, &dat_path);
        assert!(
            matches!(
                outcome,
                DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
            ),
            "{outcome:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unc_path_system_identifier_is_rejected() {
        // `\\server\share\logiqx.dtd` - a UNC path never resolves locally
        // and is refused by the `\` path-separator check, exactly like the
        // absolute-POSIX-path case above.
        let dir = test_dir("unc-path-rejected");
        let dat_path = dir.join("game.dat");
        let outcome =
            classify_doctype(br#"datafile SYSTEM "\\server\share\logiqx.dtd""#, &dat_path);
        assert!(
            matches!(
                outcome,
                DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
            ),
            "{outcome:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn traversal_system_identifier_is_rejected() {
        let dir = test_dir("traversal-rejected");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(br#"datafile SYSTEM "../../etc/passwd""#, &dat_path);
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_relative_path_system_identifier_is_rejected() {
        let dir = test_dir("nested-relative-rejected");
        let dat_path = dir.join("game.dat");
        // A multi-component relative path - not traversal, but still not a
        // "simple basename", so it must never be trusted either.
        let outcome = classify_doctype(br#"datafile SYSTEM "subdir/logiqx.dtd""#, &dat_path);
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_dtd_basename_is_rejected() {
        let dir = test_dir("unknown-basename-rejected");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(br#"datafile SYSTEM "not-a-real-dtd.dtd""#, &dat_path);
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trusted_beside_dat_copy_is_resolved() {
        let dir = test_dir("beside-dat-resolved");
        let dat_path = dir.join("game.dat");
        fs::write(&dat_path, b"<datafile/>").unwrap();
        fs::write(dir.join("logiqx.dtd"), b"<!-- not read -->").unwrap();

        let outcome = classify_doctype(br#"datafile SYSTEM "logiqx.dtd""#, &dat_path);
        match &outcome {
            DoctypeOutcome::TrustedDtdResolved { name, source } => {
                assert_eq!(*name, "Logiqx");
                assert_eq!(*source, DtdSource::BesideDat(dir.join("logiqx.dtd")));
            }
            other => panic!("expected TrustedDtdResolved, got {other:?}"),
        }
        let message = describe_doctype_outcome(&outcome);
        assert!(message.contains("trusted local copy was found"));
        assert!(!message.to_lowercase().contains("validation passed"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trusted_dtd_absent_locally_is_reported_unavailable_not_fatal() {
        let dir = test_dir("beside-dat-unavailable");
        let dat_path = dir.join("game.dat");
        fs::write(&dat_path, b"<datafile/>").unwrap();
        // No logiqx.dtd written beside it.

        let outcome = classify_doctype(br#"datafile SYSTEM "logiqx.dtd""#, &dat_path);
        assert_eq!(
            outcome,
            DoctypeOutcome::TrustedDtdUnavailable { name: "Logiqx" }
        );
        let message = describe_doctype_outcome(&outcome);
        assert!(message.contains("no trusted local copy was found"));
        assert!(message.contains("parsed normally without DTD validation"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_declaration_names_the_dtd_even_with_a_remote_system_literal() {
        let dir = test_dir("public-remote-system");
        let dat_path = dir.join("game.dat");
        fs::write(&dat_path, b"<datafile/>").unwrap();

        let outcome = classify_doctype(
            br#"datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "http://www.logiqx.com/Dats/datafile.dtd""#,
            &dat_path,
        );
        // Recognised by name via the PUBLIC id, but the SYSTEM half is a
        // URL and nothing is bundled, so this must be "unavailable" - never
        // silently fetched, never "unknown" either, since the DTD *is*
        // named.
        assert_eq!(
            outcome,
            DoctypeOutcome::TrustedDtdUnavailable { name: "Logiqx" }
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_declaration_still_resolves_a_trusted_beside_dat_copy() {
        let dir = test_dir("public-beside-dat");
        let dat_path = dir.join("game.dat");
        fs::write(&dat_path, b"<datafile/>").unwrap();
        fs::write(dir.join("logiqx.dtd"), b"<!-- not read -->").unwrap();

        let outcome = classify_doctype(
            br#"datafile PUBLIC "-//Logiqx//DTD ROM Management Datafile//EN" "logiqx.dtd""#,
            &dat_path,
        );
        match outcome {
            DoctypeOutcome::TrustedDtdResolved { name, source } => {
                assert_eq!(name, "Logiqx");
                assert_eq!(source, DtdSource::BesideDat(dir.join("logiqx.dtd")));
            }
            other => panic!("expected TrustedDtdResolved, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_beside_the_dat_cannot_escape_the_allowed_directory() {
        let dir = test_dir("symlink-escape-rejected");
        let outside_dir = test_dir("symlink-escape-outside");
        let dat_path = dir.join("game.dat");
        fs::write(&dat_path, b"<datafile/>").unwrap();
        let outside_target = outside_dir.join("real.dtd");
        fs::write(&outside_target, b"<!-- outside -->").unwrap();
        std::os::unix::fs::symlink(&outside_target, dir.join("logiqx.dtd")).unwrap();

        let outcome = classify_doctype(br#"datafile SYSTEM "logiqx.dtd""#, &dat_path);
        // The symlink's *target* is outside `dir`, so this must not be
        // trusted, even though a file named exactly `logiqx.dtd` exists
        // directly beside the DAT.
        assert_eq!(
            outcome,
            DoctypeOutcome::TrustedDtdUnavailable { name: "Logiqx" }
        );
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside_dir);
    }

    #[test]
    fn no_external_identifier_is_ignored_safely() {
        let dir = test_dir("no-external-id");
        let dat_path = dir.join("game.dat");
        let outcome = classify_doctype(b"datafile", &dat_path);
        assert!(matches!(
            outcome,
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored { .. }
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn describe_never_claims_validation_passed() {
        for outcome in [
            DoctypeOutcome::NoDoctype,
            DoctypeOutcome::TrustedDtdResolved {
                name: "Logiqx",
                source: DtdSource::BesideDat(PathBuf::from("/tmp/logiqx.dtd")),
            },
            DoctypeOutcome::TrustedDtdUnavailable { name: "Logiqx" },
            DoctypeOutcome::UnsafeOrUnknownDoctypeIgnored {
                reason: "test reason",
            },
        ] {
            let message = describe_doctype_outcome(&outcome).to_lowercase();
            assert!(!message.contains("validation passed"));
            assert!(!message.contains("dtd validation succeeded"));
        }
    }
}
