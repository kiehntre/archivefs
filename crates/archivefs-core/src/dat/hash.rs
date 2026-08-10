//! Hash normalisation for DAT checksums.
//!
//! Every checksum in a DAT file is normalised to lowercase hexadecimal of the
//! correct length for its algorithm. Malformed or truncated values are dropped.
//!
//! SHA-256 is included here because Redump publishes it; EmuWiz's own hashing
//! infrastructure in `identity_source` only needs CRC32/MD5/SHA-1 (what RomM
//! publishes), but DAT files carry a fourth algorithm, and a future stage will
//! need it.

/// Normalises a CRC32 value to 8-char lowercase hex, or `None` if invalid.
pub fn normalise_crc32(raw: &str) -> Option<String> {
    let v = raw.trim().to_ascii_lowercase();
    if v.len() == 8 && v.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(v)
    } else {
        None
    }
}

/// Normalises an MD5 value to 32-char lowercase hex, or `None` if invalid.
pub fn normalise_md5(raw: &str) -> Option<String> {
    let v = raw.trim().to_ascii_lowercase();
    if v.len() == 32 && v.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(v)
    } else {
        None
    }
}

/// Normalises a SHA-1 value to 40-char lowercase hex, or `None` if invalid.
pub fn normalise_sha1(raw: &str) -> Option<String> {
    let v = raw.trim().to_ascii_lowercase();
    if v.len() == 40 && v.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(v)
    } else {
        None
    }
}

/// Normalises a SHA-256 value to 64-char lowercase hex, or `None` if invalid.
pub fn normalise_sha256(raw: &str) -> Option<String> {
    let v = raw.trim().to_ascii_lowercase();
    if v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(v)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_normal_rejects_short() {
        assert_eq!(normalise_crc32("1234ABC"), None);
    }

    #[test]
    fn crc32_normal_rejects_non_hex() {
        assert_eq!(normalise_crc32("1234abcg"), None);
    }

    #[test]
    fn crc32_normal_accepts_valid() {
        assert_eq!(normalise_crc32("ABCD1234"), Some("abcd1234".into()));
    }

    #[test]
    fn md5_normal_rejects_short() {
        assert_eq!(normalise_md5("d41d8cd98f00b204e9800998ecf8427"), None);
    }

    #[test]
    fn md5_normal_accepts_valid() {
        assert_eq!(
            normalise_md5("d41d8cd98f00b204e9800998ecf8427e"),
            Some("d41d8cd98f00b204e9800998ecf8427e".into())
        );
    }

    #[test]
    fn sha1_normal_accepts_valid() {
        assert_eq!(
            normalise_sha1("da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".into())
        );
    }

    #[test]
    fn sha256_normal_rejects_short() {
        assert_eq!(
            normalise_sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b85"),
            None
        );
    }

    #[test]
    fn sha256_normal_accepts_valid() {
        assert_eq!(
            normalise_sha256("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into())
        );
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(normalise_crc32("  ABCD1234\n"), Some("abcd1234".into()));
    }
}
