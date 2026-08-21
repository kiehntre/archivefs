//! The Hasheous transport and read-only client (Batch 20, sections 6, 25-28,
//! 49).
//!
//! Mirrors [`crate::identity_source::romm::client`]'s established shape: a
//! [`HasheousTransport`] trait so tests drive the whole client against a
//! deterministic fake with no socket, and [`UreqTransport`] as the one
//! production implementation, reusing `ureq` (already a workspace
//! dependency - no second HTTP client was added).

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use super::dto::{HashLookupResponse, HasheousHashSet, ProblemDetails};

/// The one endpoint this adapter ever calls. Verified against the live
/// `https://hasheous.org/swagger/v1/swagger.json` document during this
/// batch (section 5) - the path is `/api/v1/Lookup/ByHash`, **not**
/// `/api/v1.0/Lookup/ByHash` as this milestone's prior research assumed.
/// See the module doc on [`super`] for the full list of live-schema
/// differences found.
pub const LOOKUP_BY_HASH_PATH: &str = "/api/v1/Lookup/ByHash";

pub const HASHEOUS_DEFAULT_BASE_URL: &str = "https://hasheous.org";

/// Defensive client-side grouping size (section 16). The live schema does
/// **not** document a "50 lookups per request, 50 results back" batching
/// contract the way this milestone's prior research assumed - see
/// [`super`]'s module doc for why `lookup_many` issues one request per item
/// rather than array-batching them. This constant instead bounds how many
/// items `lookup_many` will process in one call to
/// [`HasheousClient::lookup_many`] before returning, matching the
/// milestone's own requested boundary values (0/1/49/50/51/100+) for
/// deterministic, testable chunking of *client-side work*, not of one HTTP
/// request.
pub const MAX_BATCH_SIZE: usize = 50;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Hasheous's real `HashLookup` response can legitimately take several
/// seconds to assemble (it aggregates many cross-referenced metadata
/// sources server-side) - 15s proved too tight against the live API during
/// this batch's own validation run; 30s matches the established timeout
/// this crate already uses for the RomM client.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A `HashLookup` response is small (one game's signatures); this is a
/// generous ceiling that still refuses a pathological body.
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// Adapter-level configuration (section 8). Network evidence is opt-in:
/// `enabled` defaults to `false`, so every existing offline workflow is
/// completely unaffected unless a caller deliberately turns this on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HasheousConfig {
    pub enabled: bool,
    pub base_url: String,
    pub timeout: Duration,
}

impl Default for HasheousConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: HASHEOUS_DEFAULT_BASE_URL.to_string(),
            timeout: REQUEST_TIMEOUT,
        }
    }
}

/// Why a request failed. Never conflates "no match" (section 26 - a
/// successful, neutral result) with a real provider/transport problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HasheousRequestError {
    /// The adapter is disabled by configuration; no request was made.
    Disabled,
    Timeout,
    /// A transport problem. Never a URL or header.
    Network {
        detail: String,
    },
    HttpStatus {
        status: u16,
    },
    RateLimited {
        status: u16,
        retry_after_secs: Option<u64>,
    },
    InvalidResponse {
        detail: String,
    },
    ResponseTooLarge {
        limit: usize,
    },
    /// The hash set supplied to a lookup was empty (section 25).
    UnsupportedHash,
    /// A caller passed more items to `lookup_many` in one call than
    /// [`MAX_BATCH_SIZE`] without using the chunking this client already
    /// provides internally - only reachable via direct misuse of the lower
    /// level, since `lookup_many` itself always chunks.
    BatchTooLarge {
        requested: usize,
        max: usize,
    },
    Cancelled,
}

impl HasheousRequestError {
    pub fn detail(&self) -> String {
        match self {
            Self::Disabled => "the Hasheous adapter is disabled".to_string(),
            Self::Timeout => "Hasheous did not answer in time".to_string(),
            Self::Network { detail } => format!("could not reach Hasheous: {detail}"),
            Self::HttpStatus { status } => format!("Hasheous answered with status {status}"),
            Self::RateLimited { status, .. } => {
                format!("Hasheous asked us to slow down ({status})")
            }
            Self::InvalidResponse { detail } => {
                format!("Hasheous's answer was not in the expected form: {detail}")
            }
            Self::ResponseTooLarge { limit } => {
                format!("the response was larger than the {limit}-byte ceiling and was not read")
            }
            Self::UnsupportedHash => {
                "no hash was supplied for this lookup (crc/md5/sha1/sha256 were all empty)"
                    .to_string()
            }
            Self::BatchTooLarge { requested, max } => {
                format!("{requested} items were requested in one call; the maximum is {max}")
            }
            Self::Cancelled => "the request was cancelled".to_string(),
        }
    }
}

/// What a successful lookup found - not yet the `no match` question, which
/// is `Ok(HasheousLookupOutcome::NoMatch)`, distinct from `Err(_)` (section
/// 26).
#[derive(Debug, Clone, PartialEq)]
pub enum HasheousLookupOutcome {
    NoMatch,
    Found(Box<HashLookupResponse>),
}

/// How one request is actually performed - a trait purely for testability,
/// matching [`crate::identity_source::romm::client::RommTransport`].
pub trait HasheousTransport {
    fn post_json(
        &self,
        url: &str,
        body: &[u8],
    ) -> Result<HasheousHttpResponse, HasheousRequestError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HasheousHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub retry_after_secs: Option<u64>,
}

/// The production transport: `ureq`, redirects disabled, both timeouts set,
/// response size bounded while reading (matching the RomM client's
/// established shape).
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new(timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(timeout))
            .max_redirects(0)
            .build();
        Self {
            agent: config.into(),
        }
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new(REQUEST_TIMEOUT)
    }
}

impl HasheousTransport for UreqTransport {
    fn post_json(
        &self,
        url: &str,
        body: &[u8],
    ) -> Result<HasheousHttpResponse, HasheousRequestError> {
        let request = self
            .agent
            .post(url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json");
        let response = match request.send(body) {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(status)) => {
                return Ok(HasheousHttpResponse {
                    status,
                    body: Vec::new(),
                    retry_after_secs: None,
                });
            }
            Err(ureq::Error::Timeout(_)) => return Err(HasheousRequestError::Timeout),
            Err(error) => {
                return Err(HasheousRequestError::Network {
                    detail: classify_transport_error(&error),
                });
            }
        };
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let mut body_out = Vec::new();
        let mut reader = response
            .into_body()
            .into_reader()
            .take(MAX_RESPONSE_BYTES as u64 + 1);
        reader
            .read_to_end(&mut body_out)
            .map_err(|error| HasheousRequestError::Network {
                detail: format!("while reading the response: {}", error.kind()),
            })?;
        if body_out.len() > MAX_RESPONSE_BYTES {
            return Err(HasheousRequestError::ResponseTooLarge {
                limit: MAX_RESPONSE_BYTES,
            });
        }
        Ok(HasheousHttpResponse {
            status,
            body: body_out,
            retry_after_secs,
        })
    }
}

fn classify_transport_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::ConnectionFailed => "the connection failed".to_string(),
        ureq::Error::HostNotFound => "the host could not be found".to_string(),
        ureq::Error::Io(io) => format!("an I/O error occurred ({})", io.kind()),
        ureq::Error::Tls(_) => "the TLS handshake failed".to_string(),
        other => format!("an unexpected transport error occurred ({other})"),
    }
}

/// The read-only client.
pub struct HasheousClient<'a, T: HasheousTransport> {
    config: &'a HasheousConfig,
    transport: &'a T,
}

impl<'a, T: HasheousTransport> HasheousClient<'a, T> {
    pub fn new(config: &'a HasheousConfig, transport: &'a T) -> Self {
        Self { config, transport }
    }

    /// Builds the exact request URL, always with `returnAllSources=true`
    /// (section 4) so a match against several upstream families is never
    /// collapsed to the first one, and `returnFields=All` so platform/
    /// publisher/signature metadata all come back in one call.
    fn request_url(&self) -> String {
        format!(
            "{}{}?returnAllSources=true&returnFields=All",
            self.config.base_url.trim_end_matches('/'),
            LOOKUP_BY_HASH_PATH
        )
    }

    /// One hash-set lookup. `hash_set` may carry more than one algorithm
    /// (crc/md5/sha1/sha256) for the *same* file/representation in one
    /// object (section 15) - never hashes drawn from different
    /// representations.
    pub fn lookup(
        &self,
        hash_set: &HasheousHashSet,
        cancel: Option<&AtomicBool>,
    ) -> Result<HasheousLookupOutcome, HasheousRequestError> {
        if !self.config.enabled {
            return Err(HasheousRequestError::Disabled);
        }
        if hash_set.is_empty() {
            return Err(HasheousRequestError::UnsupportedHash);
        }
        if cancelled(cancel) {
            return Err(HasheousRequestError::Cancelled);
        }
        let body = serde_json::to_vec(hash_set).map_err(|error| {
            HasheousRequestError::InvalidResponse {
                detail: format!("could not build request body: {error}"),
            }
        })?;
        let url = self.request_url();
        let response = self.transport.post_json(&url, &body)?;
        if cancelled(cancel) {
            return Err(HasheousRequestError::Cancelled);
        }
        match response.status {
            200 => {
                let parsed: HashLookupResponse =
                    serde_json::from_slice(&response.body).map_err(|error| {
                        HasheousRequestError::InvalidResponse {
                            detail: format!("invalid JSON at line {}", error.line()),
                        }
                    })?;
                Ok(HasheousLookupOutcome::Found(Box::new(parsed)))
            }
            404 => Ok(HasheousLookupOutcome::NoMatch),
            400 => {
                let problem: ProblemDetails =
                    serde_json::from_slice(&response.body).unwrap_or_default();
                Err(HasheousRequestError::InvalidResponse {
                    detail: problem
                        .detail
                        .or(problem.title)
                        .unwrap_or_else(|| "the hash was rejected as invalid".to_string()),
                })
            }
            429 => Err(HasheousRequestError::RateLimited {
                status: 429,
                retry_after_secs: response.retry_after_secs,
            }),
            status => Err(HasheousRequestError::HttpStatus { status }),
        }
    }

    /// Looks up several hash-sets, one request per item (section 16's
    /// deterministic chunking - see [`MAX_BATCH_SIZE`]'s doc for why this is
    /// per-item rather than one array HTTP request). Order of the returned
    /// vector always matches the order of `hash_sets`. A single item's
    /// error does not abort the rest; it is carried in that item's own
    /// `Result` slot.
    pub fn lookup_many(
        &self,
        hash_sets: &[HasheousHashSet],
        cancel: Option<&AtomicBool>,
    ) -> Vec<Result<HasheousLookupOutcome, HasheousRequestError>> {
        let mut out = Vec::with_capacity(hash_sets.len());
        for hash_set in hash_sets.chunks(MAX_BATCH_SIZE).flatten() {
            if cancelled(cancel) {
                out.push(Err(HasheousRequestError::Cancelled));
                continue;
            }
            out.push(self.lookup(hash_set, cancel));
        }
        out
    }
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// The current time, for stamping [`crate::platform_evidence_fusion::evidence_lineage::Provenance::retrieved_at_unix`]
/// on a live network observation (section 30). A real caller uses this;
/// [`super::convert::observations_from_hash_lookup`] itself stays pure and
/// takes the timestamp as a plain parameter instead of reading the clock,
/// so it remains deterministically testable.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
