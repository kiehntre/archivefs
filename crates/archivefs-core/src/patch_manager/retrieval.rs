use std::io::Read;
use std::time::Duration;

use super::{BUILT_IN_SOURCE_URL, MAX_METADATA_BYTES, MetadataFetch, PatchManagerError, Result};

/// Small synchronous boundary around Phase 1 metadata retrieval. Tests supply
/// in-memory implementations; production uses [`HttpsMetadataFetcher`].
pub trait MetadataFetcher {
    fn fetch(&self, url: &str) -> Result<MetadataFetch>;
}

/// In-process HTTPS client for the one compiled-in Phase 1 source. Redirects
/// and proxies are disabled, response bytes are bounded while streaming, and
/// no response is persisted.
#[derive(Debug, Clone)]
pub struct HttpsMetadataFetcher {
    agent: ureq::Agent,
}

impl HttpsMetadataFetcher {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(15)))
            .timeout_resolve(Some(Duration::from_secs(3)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_recv_response(Some(Duration::from_secs(5)))
            .timeout_recv_body(Some(Duration::from_secs(10)))
            .build();
        Self {
            agent: config.new_agent(),
        }
    }
}

impl Default for HttpsMetadataFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataFetcher for HttpsMetadataFetcher {
    fn fetch(&self, url: &str) -> Result<MetadataFetch> {
        if url != BUILT_IN_SOURCE_URL {
            return Err(PatchManagerError::UnsupportedMetadata(
                "Phase 1 retrieval accepts only the compiled-in PCSX2 metadata endpoint"
                    .to_string(),
            ));
        }

        let mut response = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("Accept-Encoding", "identity")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header(
                "User-Agent",
                concat!("archivefs/", env!("CARGO_PKG_VERSION")),
            )
            .call()
            .map_err(|error| {
                PatchManagerError::Network(format!("metadata HTTPS request failed: {error}"))
            })?;

        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(PatchManagerError::RedirectRejected { status });
        }
        if !(200..300).contains(&status) {
            return Err(PatchManagerError::Network(format!(
                "metadata server returned HTTP {status}"
            )));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let content_encoding = response
            .headers()
            .get("content-encoding")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        validate_content_encoding(content_encoding)?;
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take((MAX_METADATA_BYTES + 1) as u64)
            .read_to_end(&mut body)
            .map_err(|error| {
                PatchManagerError::Network(format!("failed to read metadata response: {error}"))
            })?;
        if body.len() > MAX_METADATA_BYTES {
            return Err(PatchManagerError::ResponseTooLarge {
                limit: MAX_METADATA_BYTES,
            });
        }

        Ok(MetadataFetch {
            status,
            content_type,
            body,
            verification: super::VerificationLevel::TransportOnly,
        })
    }
}

fn validate_content_encoding(content_encoding: &str) -> Result<()> {
    if content_encoding.is_empty() || content_encoding.eq_ignore_ascii_case("identity") {
        Ok(())
    } else {
        Err(PatchManagerError::UnsupportedMetadata(
            "Phase 1 accepts only identity-encoded metadata responses".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_fetcher_rejects_every_non_compiled_endpoint_before_networking() {
        let fetcher = HttpsMetadataFetcher::new();
        for url in [
            "http://api.github.com/",
            "https://127.0.0.1/metadata.json",
            "https://example.invalid/metadata.json",
        ] {
            let error = fetcher.fetch(url).unwrap_err();
            assert!(matches!(error, PatchManagerError::UnsupportedMetadata(_)));
        }
    }

    #[test]
    fn compressed_metadata_responses_are_rejected() {
        assert!(validate_content_encoding("").is_ok());
        assert!(validate_content_encoding("identity").is_ok());
        assert!(validate_content_encoding("Identity").is_ok());
        assert!(matches!(
            validate_content_encoding("gzip"),
            Err(PatchManagerError::UnsupportedMetadata(_))
        ));
    }
}
