//! The local-only endpoint policy for external identity sources.
//!
//! An identity source is a URL a person types in, and ArchiveFS then connects to
//! it with a bearer token attached. That combination is exactly the shape of a
//! server-side request forgery, so this module exists to make the dangerous
//! cases unreachable rather than unlikely.
//!
//! # What is allowed
//!
//! Only `http` or `https`, only to an address that resolves inside one of the
//! ranges a home or container network actually uses:
//!
//! - `127.0.0.0/8` and `::1` - loopback
//! - `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` - RFC 1918
//! - `fc00::/7` - IPv6 unique local, the equivalent for container networks
//!
//! Everything else is refused, including every public address.
//!
//! # Why resolution happens here
//!
//! A hostname is not an address. `romm.example.com` may resolve to a private
//! address today and a public one tomorrow, and a name that resolves to several
//! addresses may offer a private one first and a public one second. So the
//! policy resolves the host itself and requires **every** returned address to be
//! approved - not merely the one that happens to be tried first. A name that
//! resolves to any public address is refused outright, which is what closes the
//! DNS-rebinding hole.
//!
//! # Redirects
//!
//! Not followed. The client is configured with zero redirects, and a redirect
//! response is reported as a refusal naming the destination. Following one would
//! mean re-running this whole policy against an address the user never entered,
//! and Stage 1 has no need for it. [`validate_redirect_target`] exists so that a
//! later stage which does want a small bounded number of redirects has one
//! obvious place to revalidate, and so the tests can prove a public redirect is
//! refused today.
//!
//! # Cloud metadata
//!
//! `169.254.169.254` and its friends are link-local, so the range rules already
//! refuse them. They are also named explicitly, because "the metadata endpoint
//! is refused" is a property worth asserting directly rather than inferring.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use serde::Serialize;

/// Cloud and link-local metadata addresses, refused by name as well as by range.
///
/// Every one of these is already outside the approved ranges; listing them makes
/// the intent explicit and gives the tests something exact to assert.
pub const METADATA_ADDRESSES: &[&str] = &[
    "169.254.169.254", // AWS, Azure, GCP, DigitalOcean, Oracle
    "169.254.170.2",   // AWS ECS task metadata
    "100.100.100.200", // Alibaba Cloud
    "192.0.0.192",     // Oracle Cloud legacy
    "fd00:ec2::254",   // AWS IPv6 metadata
];

/// The largest number of addresses a single hostname may resolve to before the
/// policy stops looking. A name with more than this is not a home server.
pub const MAX_RESOLVED_ADDRESSES: usize = 16;

/// Why an endpoint was refused. Each variant is a distinct, explainable reason -
/// a person who typed the wrong thing deserves to know which wrong thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum EndpointRefusal {
    /// The text is not a URL at all.
    Unparseable { detail: String },
    /// A scheme other than `http` or `https`. Names the scheme so `file:` and
    /// `unix:` attempts are legible in a diagnostic.
    UnsupportedScheme { scheme: String },
    /// A URL carrying a username or password in its authority component, before
    /// the `@`. Never accepted, because such a URL ends up in caches, logs and
    /// diagnostics.
    ///
    /// Written as prose rather than as an example URL: the repository's secret
    /// scanner matches that shape wherever it appears, and a doc comment is not
    /// worth a standing exception to it.
    EmbeddedCredentials,
    /// No host at all.
    MissingHost,
    /// The host could not be resolved.
    UnresolvableHost { detail: String },
    /// The host resolved to nothing.
    NoAddresses,
    /// The host resolved to more addresses than the policy will consider.
    TooManyAddresses { count: usize },
    /// At least one resolved address is outside the approved ranges. The
    /// *address* is named, not just the host, because that is the fact that
    /// decided it.
    NotPrivateAddress { address: String },
    /// A known cloud or link-local metadata endpoint.
    MetadataEndpoint { address: String },
    /// A redirect, which Stage 1 does not follow.
    RedirectRefused { location: String },
    /// A port outside the usable range, or zero.
    InvalidPort { port: u16 },
    /// The URL carried a path that is not a prefix this client will use.
    UnsupportedUrlShape { detail: String },
}

impl EndpointRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::Unparseable { detail } => format!("that is not a valid URL: {detail}"),
            Self::UnsupportedScheme { scheme } => format!(
                "`{scheme}` is not a supported scheme; an identity source must be http or https"
            ),
            Self::EmbeddedCredentials => {
                "a URL must not contain a username or password: supply the token separately, so \
                 it is never stored or logged as part of the address"
                    .to_string()
            }
            Self::MissingHost => "the URL has no host".to_string(),
            Self::UnresolvableHost { detail } => {
                format!("the host could not be resolved: {detail}")
            }
            Self::NoAddresses => "the host resolved to no addresses".to_string(),
            Self::TooManyAddresses { count } => format!(
                "the host resolved to {count} addresses, more than the \
                 {MAX_RESOLVED_ADDRESSES} this policy will consider"
            ),
            Self::NotPrivateAddress { address } => format!(
                "{address} is not on a local or private network; an identity source must be \
                 reachable only on loopback, a private LAN or a private container network"
            ),
            Self::MetadataEndpoint { address } => {
                format!("{address} is a cloud metadata endpoint and is never contacted")
            }
            Self::RedirectRefused { location } => format!(
                "the server redirected to {location}; redirects are not followed, because that \
                 would send the token to an address that was never approved"
            ),
            Self::InvalidPort { port } => format!("{port} is not a usable port"),
            Self::UnsupportedUrlShape { detail } => detail.clone(),
        }
    }

    /// A stable code, for counting and for tests.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unparseable { .. } => "unparseable",
            Self::UnsupportedScheme { .. } => "unsupported_scheme",
            Self::EmbeddedCredentials => "embedded_credentials",
            Self::MissingHost => "missing_host",
            Self::UnresolvableHost { .. } => "unresolvable_host",
            Self::NoAddresses => "no_addresses",
            Self::TooManyAddresses { .. } => "too_many_addresses",
            Self::NotPrivateAddress { .. } => "not_private_address",
            Self::MetadataEndpoint { .. } => "metadata_endpoint",
            Self::RedirectRefused { .. } => "redirect_refused",
            Self::InvalidPort { .. } => "invalid_port",
            Self::UnsupportedUrlShape { .. } => "unsupported_url_shape",
        }
    }
}

impl fmt::Display for EndpointRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail())
    }
}

/// An endpoint that has passed the policy.
///
/// Holds the addresses it resolved to at validation time, so a caller can see
/// what was actually approved and a diagnostic can report it. Constructing one
/// is the only way to get past the policy, so a function that takes this type
/// cannot be called with an unvalidated URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovedEndpoint {
    /// Scheme and authority only - never a path, query or fragment, and never
    /// credentials.
    origin: String,
    host: String,
    port: u16,
    scheme: &'static str,
    /// Every address the host resolved to, all of them approved.
    resolved: Vec<String>,
}

impl ApprovedEndpoint {
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn scheme(&self) -> &str {
        self.scheme
    }

    pub fn resolved_addresses(&self) -> &[String] {
        &self.resolved
    }

    /// Builds a request URL by appending an API path.
    ///
    /// The path is required to be absolute and free of `..`, so a caller cannot
    /// walk out of the API namespace, and the result always keeps the approved
    /// origin.
    pub fn url_for(&self, path: &str) -> Result<String, EndpointRefusal> {
        if !path.starts_with('/') {
            return Err(EndpointRefusal::UnsupportedUrlShape {
                detail: format!("`{path}` must be an absolute API path"),
            });
        }
        if path.split('/').any(|segment| segment == "..") {
            return Err(EndpointRefusal::UnsupportedUrlShape {
                detail: format!("`{path}` must not contain a `..` segment"),
            });
        }
        Ok(format!("{}{path}", self.origin))
    }
}

/// How a host is resolved. Injectable so the tests can drive the policy with
/// exact addresses - including a name that resolves to a public address, which
/// is the DNS-rebinding case and cannot be arranged with real DNS.
pub trait HostResolver {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, String>;
}

/// The real resolver, using the system's.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl HostResolver for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, String> {
        let addresses = (host, port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .map(|address: SocketAddr| address.ip())
            .collect();
        Ok(addresses)
    }
}

/// Whether one address is inside the approved local ranges.
///
/// Deliberately written out rather than relying only on `is_private`, so each
/// admitted range is visible and reviewable, and so the IPv6 unique-local range
/// used by container networks is included on purpose rather than by accident.
pub fn is_approved_local_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                return true; // 127.0.0.0/8
            }
            let octets = v4.octets();
            match octets {
                [10, ..] => true,                                         // 10.0.0.0/8
                [172, second, ..] if (16..=31).contains(&second) => true, // 172.16.0.0/12
                [192, 168, ..] => true,                                   // 192.168.0.0/16
                _ => false,
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return true; // ::1
            }
            // fc00::/7 - IPv6 unique local, what container runtimes hand out.
            if (v6.segments()[0] & 0xfe00) == 0xfc00 {
                return true;
            }
            // An IPv4-mapped address is judged as the IPv4 address it carries,
            // so `::ffff:8.8.8.8` cannot slip through as "some IPv6 address".
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_approved_local_address(IpAddr::V4(mapped));
            }
            false
        }
    }
}

/// Whether `address` is a known metadata endpoint.
pub fn is_metadata_address(address: IpAddr) -> bool {
    if METADATA_ADDRESSES
        .iter()
        .filter_map(|text| text.parse::<IpAddr>().ok())
        .any(|known| known == address)
    {
        return true;
    }
    // Every link-local address, not only the well-known ones.
    match address {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Validates an endpoint URL against the whole policy, resolving the host.
///
/// This is the only way to obtain an [`ApprovedEndpoint`].
pub fn validate_endpoint(
    url: &str,
    resolver: &impl HostResolver,
) -> Result<ApprovedEndpoint, EndpointRefusal> {
    let parsed = ParsedEndpoint::parse(url)?;
    let addresses = resolver
        .resolve(&parsed.host, parsed.port)
        .map_err(|detail| EndpointRefusal::UnresolvableHost { detail })?;

    if addresses.is_empty() {
        return Err(EndpointRefusal::NoAddresses);
    }
    if addresses.len() > MAX_RESOLVED_ADDRESSES {
        return Err(EndpointRefusal::TooManyAddresses {
            count: addresses.len(),
        });
    }
    // *Every* address must be approved. Checking only the first would leave a
    // name that offers a private address first and a public one second usable.
    for address in &addresses {
        if is_metadata_address(*address) {
            return Err(EndpointRefusal::MetadataEndpoint {
                address: address.to_string(),
            });
        }
        if !is_approved_local_address(*address) {
            return Err(EndpointRefusal::NotPrivateAddress {
                address: address.to_string(),
            });
        }
    }

    Ok(ApprovedEndpoint {
        origin: parsed.origin(),
        host: parsed.host,
        port: parsed.port,
        scheme: parsed.scheme,
        resolved: addresses.iter().map(IpAddr::to_string).collect(),
    })
}

/// Revalidates a redirect destination against the same policy.
///
/// Stage 1 does not follow redirects - the client is configured with none - so
/// this is what turns a redirect response into an explicit refusal that names
/// where the server tried to send the token. It is also the hook a later stage
/// would call per hop if a small bounded number of redirects were ever allowed.
pub fn validate_redirect_target(
    location: &str,
    approved: &ApprovedEndpoint,
    resolver: &impl HostResolver,
) -> EndpointRefusal {
    // A relative redirect stays on the approved origin. An absolute one has to
    // be assessed on its *origin*: the whole location carries a path, and
    // `validate_endpoint` deliberately refuses paths, so passing it the full URL
    // would always report the shape rather than the destination address - which
    // is the fact that actually matters here.
    let (absolute, origin) = if location.starts_with('/') {
        (
            format!("{}{location}", approved.origin()),
            approved.origin().to_string(),
        )
    } else {
        match origin_of(location) {
            Some(origin) => (location.to_string(), origin),
            None => {
                return EndpointRefusal::UnsupportedUrlShape {
                    detail: format!(
                        "the server redirected to `{location}`, which is not an address this \
                         policy can assess"
                    ),
                };
            }
        }
    };
    match validate_endpoint(&origin, resolver) {
        // The destination is approved, and is still refused: not following
        // redirects at all is the Stage 1 policy, and the refusal names where
        // the server tried to send the request.
        Ok(_) => EndpointRefusal::RedirectRefused { location: absolute },
        // An unapproved destination is refused for the stronger, more specific
        // reason - a public address, or a metadata endpoint.
        Err(refusal) => refusal,
    }
}

/// The `scheme://authority` prefix of an absolute URL, without its path.
///
/// Used only to assess a redirect destination; it deliberately does not accept a
/// relative reference, because those are handled against the approved origin.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(format!("{scheme}://{}", &rest[..authority_end]))
}

/// The syntactic half of the policy: scheme, credentials, host, port.
///
/// Hand-parsed rather than pulling in a URL crate: the accepted shape is
/// deliberately tiny - `scheme://host[:port][/]` - and refusing everything else
/// is the point. A permissive parser would accept forms this policy then has to
/// reason about.
struct ParsedEndpoint {
    scheme: &'static str,
    host: String,
    port: u16,
}

impl ParsedEndpoint {
    fn parse(url: &str) -> Result<Self, EndpointRefusal> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Err(EndpointRefusal::Unparseable {
                detail: "the address is empty".to_string(),
            });
        }
        // Control characters, whitespace and quotes are refused before anything
        // else: they are how a single field becomes two requests.
        if trimmed
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '"' || ch == '\'')
        {
            return Err(EndpointRefusal::Unparseable {
                detail: "the address contains whitespace, quoting or control characters"
                    .to_string(),
            });
        }

        let (scheme, rest) = match trimmed.split_once("://") {
            Some(("http", rest)) => ("http", rest),
            Some(("https", rest)) => ("https", rest),
            Some((scheme, _)) => {
                return Err(EndpointRefusal::UnsupportedScheme {
                    scheme: scheme.to_ascii_lowercase(),
                });
            }
            None => {
                // `file:/etc/passwd`, `unix:/var/run/x.sock`, a bare shell word.
                let scheme = trimmed
                    .split_once(':')
                    .map(|(scheme, _)| scheme.to_ascii_lowercase())
                    .unwrap_or_else(|| "(none)".to_string());
                return Err(EndpointRefusal::UnsupportedScheme { scheme });
            }
        };

        // Authority ends at the first `/`, `?` or `#`.
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        // Only a bare origin, or one with a trailing slash, is accepted. A path
        // would be silently dropped otherwise, and a person who typed one
        // deserves to be told rather than quietly ignored.
        if !tail.is_empty() && tail != "/" {
            return Err(EndpointRefusal::UnsupportedUrlShape {
                detail: format!(
                    "give the server address only, without a path: `{tail}` is not accepted"
                ),
            });
        }
        if authority.contains('@') {
            return Err(EndpointRefusal::EmbeddedCredentials);
        }
        if authority.is_empty() {
            return Err(EndpointRefusal::MissingHost);
        }

        // A bracketed IPv6 literal, or a host and optional port.
        let (host, port_text) = if let Some(closing) = authority.strip_prefix('[') {
            let Some((literal, remainder)) = closing.split_once(']') else {
                return Err(EndpointRefusal::Unparseable {
                    detail: "an IPv6 address must be written in [brackets]".to_string(),
                });
            };
            let port = remainder.strip_prefix(':');
            if !remainder.is_empty() && port.is_none() {
                return Err(EndpointRefusal::Unparseable {
                    detail: "unexpected text after the IPv6 address".to_string(),
                });
            }
            (literal.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((host, port)) => (host.to_string(), Some(port)),
                None => (authority.to_string(), None),
            }
        };

        if host.is_empty() {
            return Err(EndpointRefusal::MissingHost);
        }
        let port = match port_text {
            Some(text) => text
                .parse::<u16>()
                .map_err(|_| EndpointRefusal::Unparseable {
                    detail: format!("`{text}` is not a port number"),
                })?,
            None if scheme == "https" => 443,
            None => 80,
        };
        if port == 0 {
            return Err(EndpointRefusal::InvalidPort { port });
        }

        Ok(Self { scheme, host, port })
    }

    fn origin(&self) -> String {
        // An IPv6 literal keeps its brackets in a URL.
        if self.host.parse::<Ipv6Addr>().is_ok() {
            format!("{}://[{}]:{}", self.scheme, self.host, self.port)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

/// A fixed resolver, for tests and for previewing a configuration without
/// touching DNS.
#[derive(Debug, Clone, Default)]
pub struct StaticResolver {
    entries: Vec<(String, Vec<IpAddr>)>,
}

impl StaticResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, host: &str, addresses: &[IpAddr]) -> Self {
        self.entries
            .push((host.to_ascii_lowercase(), addresses.to_vec()));
        self
    }

    /// Convenience for the common single-address case.
    pub fn with_v4(self, host: &str, address: Ipv4Addr) -> Self {
        self.with(host, &[IpAddr::V4(address)])
    }
}

impl HostResolver for StaticResolver {
    fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>, String> {
        // A literal address needs no resolution, which keeps the tests honest
        // about what they are exercising.
        if let Ok(address) = host.parse::<IpAddr>() {
            return Ok(vec![address]);
        }
        self.entries
            .iter()
            .find(|(name, _)| name == &host.to_ascii_lowercase())
            .map(|(_, addresses)| addresses.clone())
            .ok_or_else(|| format!("no static entry for `{host}`"))
    }
}
