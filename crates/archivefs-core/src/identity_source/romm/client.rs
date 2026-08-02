//! A small read-only RomM client.
//!
//! Hand-written on purpose. Generating a client from RomM's OpenAPI document
//! would produce something around 170 endpoints wide, most of which write, and
//! every one of those would be a path this milestone promises does not exist. A
//! reviewed client with four read requests in it is the safer artefact: what it
//! can do is what you can see.
//!
//! # The four requests
//!
//! | Purpose | Request |
//! |---------|---------|
//! | connection test | `GET /api/heartbeat` (no token needed) |
//! | capability check | `GET /openapi.json` |
//! | platforms | `GET /api/platforms` |
//! | one page of ROMs | `GET /api/roms?limit=&offset=` |
//!
//! There is no `POST`, `PUT`, `PATCH` or `DELETE` anywhere in this module, and a
//! test asserts that by reading the source.
//!
//! # Bounds
//!
//! Every request has a connect and read timeout, a response-size ceiling
//! enforced while reading rather than after, and a cancellation check between
//! steps. Redirects are disabled at the agent, and a redirect status is turned
//! into an explained refusal by the endpoint policy rather than followed.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;

use super::capability::{RommApiCapability, RommCapabilityReport, RommHeartbeat};
use super::config::ValidatedRommSource;
use crate::identity_source::net_policy::{
    ApprovedEndpoint, EndpointRefusal, HostResolver, validate_redirect_target,
};

/// Connect timeout for one request.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Total per-request timeout, including reading the body.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The most any single response body may be. A RomM page of 200 ROMs is a few
/// hundred kilobytes; the OpenAPI document on the verified instance is 331 KiB.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// The most one page may request. RomM's own default is 50.
pub const MAX_PAGE_SIZE: u32 = 200;
/// The most pages one import will walk, so a runaway `total` cannot loop for
/// ever. At the maximum page size this is 200,000 records.
pub const MAX_PAGES: u32 = 1000;

/// Why a request failed. Deliberately never carries a token, a header or a
/// response body - only a status, a size, or a short reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RommRequestError {
    /// The endpoint policy refused something - including a redirect.
    Endpoint(EndpointRefusal),
    /// The token was rejected, or was not accepted for this scope.
    Unauthorised {
        status: u16,
    },
    /// The instance answered, unhappily.
    HttpStatus {
        status: u16,
    },
    /// Too many requests, or the instance asked us to back off.
    RateLimited {
        status: u16,
    },
    /// The body exceeded the ceiling. Reported with the limit, not the content.
    ResponseTooLarge {
        limit: usize,
    },
    /// The body was not the JSON this adapter expects.
    MalformedResponse {
        detail: String,
    },
    /// A transport problem. The message is a short classification, never a URL
    /// with a token in it.
    Transport {
        detail: String,
    },
    Timeout,
    Cancelled,
}

impl RommRequestError {
    pub fn detail(&self) -> String {
        match self {
            Self::Endpoint(refusal) => refusal.detail(),
            Self::Unauthorised { status } => format!(
                "RomM rejected the token ({status}); check that it is a client token with the \
                 read scopes this source needs and that it has not expired"
            ),
            Self::HttpStatus { status } => format!("RomM answered with status {status}"),
            Self::RateLimited { status } => {
                format!("RomM asked us to slow down ({status}); try the refresh again shortly")
            }
            Self::ResponseTooLarge { limit } => {
                format!("the response was larger than the {limit}-byte ceiling and was not read")
            }
            Self::MalformedResponse { detail } => {
                format!("RomM's answer was not in the expected form: {detail}")
            }
            Self::Transport { detail } => format!("could not reach RomM: {detail}"),
            Self::Timeout => "RomM did not answer in time".to_string(),
            Self::Cancelled => "the request was cancelled".to_string(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Endpoint(refusal) => refusal.code(),
            Self::Unauthorised { .. } => "unauthorised",
            Self::HttpStatus { .. } => "http_status",
            Self::RateLimited { .. } => "rate_limited",
            Self::ResponseTooLarge { .. } => "response_too_large",
            Self::MalformedResponse { .. } => "malformed_response",
            Self::Transport { .. } => "transport",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether a failed refresh should keep the previous cache. Every error does:
    /// nothing here is a reason to discard identity that was working.
    pub fn preserves_cache(&self) -> bool {
        true
    }
}

/// How a request is actually performed.
///
/// A trait so the tests can drive the whole adapter against a deterministic fake
/// instance, with no socket and no dependence on a real RomM. The production
/// implementation is [`UreqTransport`].
pub trait RommTransport {
    /// Performs one authenticated GET and returns `(status, body)`.
    ///
    /// `authorization` is already the finished header value; an implementation
    /// must not log it.
    fn get(
        &self,
        url: &str,
        authorization: Option<&str>,
        max_bytes: usize,
    ) -> Result<RommHttpResponse, RommRequestError>;
}

/// What a transport returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RommHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// A `Location` header, when the instance sent one. Present so a redirect
    /// can be reported precisely instead of silently followed.
    pub location: Option<String>,
}

/// The production transport: `ureq`, with redirects disabled and both timeouts
/// set.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(REQUEST_TIMEOUT))
            // Zero redirects. Following one would send the token to an address
            // the endpoint policy never approved.
            .max_redirects(0)
            .build();
        Self {
            agent: config.into(),
        }
    }
}

impl RommTransport for UreqTransport {
    fn get(
        &self,
        url: &str,
        authorization: Option<&str>,
        max_bytes: usize,
    ) -> Result<RommHttpResponse, RommRequestError> {
        let mut request = self.agent.get(url);
        if let Some(authorization) = authorization {
            request = request.header("Authorization", authorization);
        }
        request = request.header("Accept", "application/json");
        let response = match request.call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(status)) => {
                return Ok(RommHttpResponse {
                    status,
                    body: Vec::new(),
                    location: None,
                });
            }
            Err(ureq::Error::Timeout(_)) => return Err(RommRequestError::Timeout),
            Err(error) => {
                // Classified, never echoed with the URL: the URL is safe here,
                // but keeping the habit means a future change cannot leak one.
                return Err(RommRequestError::Transport {
                    detail: classify_transport_error(&error),
                });
            }
        };
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);

        // Read with a hard ceiling, enforced while reading. `take` means an
        // oversized body is never fully buffered.
        let mut body = Vec::new();
        let mut reader = response
            .into_body()
            .into_reader()
            .take(max_bytes as u64 + 1);
        reader
            .read_to_end(&mut body)
            .map_err(|error| RommRequestError::Transport {
                detail: format!("while reading the response: {}", error.kind()),
            })?;
        if body.len() > max_bytes {
            return Err(RommRequestError::ResponseTooLarge { limit: max_bytes });
        }
        Ok(RommHttpResponse {
            status,
            body,
            location,
        })
    }
}

/// A short classification of a transport failure, with no URL or header in it.
fn classify_transport_error(error: &ureq::Error) -> String {
    match error {
        ureq::Error::ConnectionFailed => "the connection failed".to_string(),
        ureq::Error::HostNotFound => "the host could not be found".to_string(),
        ureq::Error::Io(io) => format!("an I/O error occurred ({})", io.kind()),
        ureq::Error::Tls(_) => "the TLS handshake failed".to_string(),
        other => format!("an unexpected transport error occurred ({})", other),
    }
}

/// One page of ROMs, as RomM's `CustomLimitOffsetPage` envelope carries it.
///
/// `reported_limit` and `reported_offset` are what the *server* said, not what
/// was asked for. Keeping them distinct from the request is the whole point: an
/// importer can only detect a server echoing the wrong page if it can see what
/// the server claimed. They are `Option` because an older instance might omit
/// them, and "the server did not say" is tolerable where "the server said
/// something wrong" is not.
#[derive(Debug, Clone, PartialEq)]
pub struct RommRomPage {
    pub items: Vec<serde_json::Value>,
    pub total: u64,
    /// The limit this client asked for, after clamping.
    pub requested_limit: u32,
    /// The offset this client asked for.
    pub requested_offset: u32,
    pub reported_limit: Option<u32>,
    pub reported_offset: Option<u32>,
    /// Whether this page was fetched with per-file detail. `false` means every
    /// record on it has no file list, and the caller must say so.
    pub with_files: bool,
}

impl RommRomPage {
    /// The page size to advance by: what the server reported, or what was asked
    /// for when it reported nothing.
    pub fn effective_limit(&self) -> u32 {
        self.reported_limit.unwrap_or(self.requested_limit)
    }
}

/// The read-only client.
pub struct RommClient<'a, T: RommTransport> {
    source: &'a ValidatedRommSource,
    transport: &'a T,
}

impl<'a, T: RommTransport> RommClient<'a, T> {
    pub fn new(source: &'a ValidatedRommSource, transport: &'a T) -> Self {
        Self { source, transport }
    }

    /// `GET /api/heartbeat`, which needs no token.
    ///
    /// This is what a connection test uses, so an address can be checked before
    /// a token has been entered - and so a test never has to send one.
    pub fn heartbeat(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> Result<RommHeartbeat, RommRequestError> {
        let document = self.get_json("/api/heartbeat", false, cancel)?;
        RommHeartbeat::parse(&document).ok_or(RommRequestError::MalformedResponse {
            detail: "the heartbeat did not report SYSTEM.VERSION".to_string(),
        })
    }

    /// `GET /openapi.json`, to verify the endpoints and fields this adapter needs
    /// actually exist on this instance.
    pub fn api_capability(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> Result<RommApiCapability, RommRequestError> {
        let document = self.get_json("/openapi.json", false, cancel)?;
        Ok(RommApiCapability::from_openapi(&document))
    }

    /// Everything a connection test should report - and nothing else.
    ///
    /// Deliberately built from the two unauthenticated documents plus, if a token
    /// works, one single-record probe. It reports a version, a platform count and
    /// capability flags; it never reports a header, a token or a body.
    pub fn capability_report(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> Result<RommCapabilityReport, RommRequestError> {
        let heartbeat = self.heartbeat(cancel).ok();
        let api = self.api_capability(cancel)?;
        let mut notes = Vec::new();
        if let Some(heartbeat) = &heartbeat
            && !heartbeat.is_supported()
        {
            notes.push(format!(
                "RomM {} is older than this adapter has been checked against; import will be \
                 attempted only if the endpoints it needs are present",
                heartbeat.version
            ));
        }
        if let Some(reason) = api.blocking_reason() {
            notes.push(reason);
        }
        if !api.supports_client_tokens {
            notes.push(
                "this instance does not publish a client-token facility, so a read-only token \
                 cannot be created on it"
                    .to_string(),
            );
        }
        if api.available_hash_fields.is_empty() {
            notes.push(
                "this instance's ROM records carry no hash fields, so imported identity cannot \
                 reach confirmed without a local hash"
                    .to_string(),
            );
        }
        Ok(RommCapabilityReport {
            server_id: self.source.server_id().to_string(),
            heartbeat,
            api,
            notes,
        })
    }

    /// `GET /api/platforms`.
    pub fn platforms(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<serde_json::Value>, RommRequestError> {
        let document = self.get_json("/api/platforms", true, cancel)?;
        document
            .as_array()
            .cloned()
            .ok_or(RommRequestError::MalformedResponse {
                detail: "the platform list was not an array".to_string(),
            })
    }

    /// One bounded page of ROMs.
    ///
    /// `limit` is clamped to [`MAX_PAGE_SIZE`], so a caller cannot ask the
    /// instance for an unbounded page however it was configured.
    pub fn roms_page(
        &self,
        limit: u32,
        offset: u32,
        cancel: Option<&AtomicBool>,
    ) -> Result<RommRomPage, RommRequestError> {
        self.roms_page_detail(limit, offset, true, cancel)
    }

    /// One page of ROMs, with the option of leaving the per-file list out.
    ///
    /// `with_files` is what makes multi-file and multi-disc relationships
    /// available, and it is cheap for almost every record. It is not cheap for
    /// all of them: one real PS4 game held 28,831 file entries, which alone made
    /// its single-record response 17.5 MB against a 436 KB response without them.
    /// Asking without the file list is therefore the last way to read a record
    /// that would otherwise be unreadable - at the cost of that record's file
    /// detail, which the caller must report rather than quietly drop.
    pub fn roms_page_detail(
        &self,
        limit: u32,
        offset: u32,
        with_files: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<RommRomPage, RommRequestError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let path = if with_files {
            format!("/api/roms?limit={limit}&offset={offset}&with_files=true")
        } else {
            format!("/api/roms?limit={limit}&offset={offset}")
        };
        let document = self.get_json(&path, true, cancel)?;
        let items = document
            .get("items")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .ok_or(RommRequestError::MalformedResponse {
                detail: "the ROM page had no `items` array".to_string(),
            })?;
        let total = document
            .get("total")
            .and_then(serde_json::Value::as_u64)
            .ok_or(RommRequestError::MalformedResponse {
                detail: "the ROM page had no `total`".to_string(),
            })?;
        // Read back what the server said about the page it sent, so a caller can
        // check it against what was requested.
        let reported_limit = document
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        let reported_offset = document
            .get("offset")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        Ok(RommRomPage {
            items,
            total,
            requested_limit: limit,
            requested_offset: offset,
            reported_limit,
            reported_offset,
            with_files,
        })
    }

    /// Performs one GET and parses JSON, applying every bound.
    fn get_json(
        &self,
        path: &str,
        authenticate: bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<serde_json::Value, RommRequestError> {
        if cancelled(cancel) {
            return Err(RommRequestError::Cancelled);
        }
        let url = self
            .source
            .endpoint()
            .url_for(path)
            .map_err(RommRequestError::Endpoint)?;

        // The token is materialised only inside this closure, and only when the
        // request actually needs it.
        let response = if authenticate {
            self.source.token().with_header_value(|header| {
                self.transport.get(&url, Some(header), MAX_RESPONSE_BYTES)
            })
        } else {
            self.transport.get(&url, None, MAX_RESPONSE_BYTES)
        }?;

        if cancelled(cancel) {
            return Err(RommRequestError::Cancelled);
        }
        // A redirect is never followed: it is reported through the endpoint
        // policy, which names where the instance tried to send the request.
        if (300..400).contains(&response.status) {
            let location = response
                .location
                .unwrap_or_else(|| "(no location)".to_string());
            return Err(RommRequestError::Endpoint(validate_redirect_target(
                &location,
                self.source.endpoint(),
                &RefusingResolver,
            )));
        }
        match response.status {
            200 => {}
            401 | 403 => {
                return Err(RommRequestError::Unauthorised {
                    status: response.status,
                });
            }
            429 | 503 => {
                return Err(RommRequestError::RateLimited {
                    status: response.status,
                });
            }
            status => return Err(RommRequestError::HttpStatus { status }),
        }
        serde_json::from_slice(&response.body).map_err(|error| {
            // The parser's message names a position, never content.
            RommRequestError::MalformedResponse {
                detail: format!("invalid JSON at line {}", error.line()),
            }
        })
    }
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

/// A resolver that refuses everything.
///
/// Used when reporting a redirect: the destination is not going to be contacted,
/// so resolving it would be a pointless DNS lookup driven by a remote server's
/// header. Refusing means the report says "not approved" without performing a
/// lookup the instance asked for.
struct RefusingResolver;

impl HostResolver for RefusingResolver {
    fn resolve(&self, host: &str, _port: u16) -> Result<Vec<std::net::IpAddr>, String> {
        // A literal address still resolves, so a redirect to a public literal is
        // reported as public rather than as unresolvable.
        if let Ok(address) = host.parse::<std::net::IpAddr>() {
            return Ok(vec![address]);
        }
        Err("redirect destinations are not resolved".to_string())
    }
}

/// The URL a request would use, for a preview or a diagnostic. Never includes a
/// token, because the token travels in a header.
pub fn describe_request(endpoint: &ApprovedEndpoint, path: &str) -> String {
    endpoint
        .url_for(path)
        .unwrap_or_else(|refusal| format!("(invalid request: {})", refusal.detail()))
}
