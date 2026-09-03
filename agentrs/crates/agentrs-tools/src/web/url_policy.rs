use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use tokio::net::lookup_host;
use url::{Host, Url};

use agentrs_config::web::WebConfig;

/// Why a URL was refused.
///
/// Kept as a typed enum rather than a string so the fetch tool can decide what
/// to surface to the model and tests can assert on the exact rule that fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// The string did not parse as a URL at all.
    InvalidUrl,
    /// Longer than the configured maximum.
    UrlTooLong { length: usize, limit: usize },
    /// Embedded `user:password@` credentials.
    CredentialsInUrl,
    /// Scheme other than http/https.
    UnsupportedScheme { scheme: String },
    /// A bare name such as `intranet` that cannot be a public domain.
    NotPublicHostname { host: String },
    /// Loopback, link-local, or otherwise non-public destination.
    PrivateHost { host: String },
    /// Matched `deny_domains`.
    DeniedDomain { host: String },
    /// `allow_domains` is set and this host is not on it.
    NotAllowedDomain { host: String },
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => write!(f, "Invalid URL: could not be parsed"),
            Self::UrlTooLong { length, limit } => {
                write!(f, "Invalid URL: length {length} exceeds the {limit} character limit")
            }
            Self::CredentialsInUrl => write!(f, "Invalid URL: embedded credentials are not allowed"),
            Self::UnsupportedScheme { scheme } => {
                write!(
                    f,
                    "Invalid URL: unsupported scheme \"{scheme}\", expected http or https"
                )
            }
            Self::NotPublicHostname { host } => {
                write!(f, "Refused to fetch \"{host}\": not a publicly resolvable hostname")
            }
            Self::PrivateHost { host } => write!(
                f,
                "Refused to fetch \"{host}\": resolves to a private, loopback, or link-local address. \
                 Set web.allow_private_network = true to permit this."
            ),
            Self::DeniedDomain { host } => {
                write!(f, "Refused to fetch \"{host}\": matched web.deny_domains")
            }
            Self::NotAllowedDomain { host } => {
                write!(f, "Refused to fetch \"{host}\": not listed in web.allow_domains")
            }
        }
    }
}

/// Host and transfer policy applied to every outbound URL.
#[derive(Debug, Clone)]
pub struct UrlPolicy {
    max_url_length: usize,
    allow_private_network: bool,
    allow_domains: Vec<String>,
    deny_domains: Vec<String>,
    preapproved_domains: Vec<PreapprovedRule>,
}

impl UrlPolicy {
    pub fn new(config: &WebConfig) -> Self {
        Self {
            max_url_length: config.max_url_length,
            allow_private_network: config.allow_private_network,
            allow_domains: normalize_rules(&config.allow_domains),
            deny_domains: normalize_rules(&config.deny_domains),
            preapproved_domains: config
                .preapproved_domains
                .iter()
                .filter_map(|rule| PreapprovedRule::parse(rule))
                .collect(),
        }
    }

    /// Syntactic and literal-address validation.
    ///
    /// Returns the URL to actually request, with `http` upgraded to `https`.
    /// This deliberately does not resolve DNS — see [`Self::check_resolved`],
    /// which the client calls once per hop right before connecting.
    pub fn check(&self, raw: &str) -> Result<Url, RejectReason> {
        if raw.len() > self.max_url_length {
            return Err(RejectReason::UrlTooLong {
                length: raw.len(),
                limit: self.max_url_length,
            });
        }

        let mut url = Url::parse(raw).map_err(|_| RejectReason::InvalidUrl)?;

        let scheme = url.scheme().to_string();
        if scheme != "http" && scheme != "https" {
            return Err(RejectReason::UnsupportedScheme { scheme });
        }

        if !url.username().is_empty() || url.password().is_some() {
            return Err(RejectReason::CredentialsInUrl);
        }

        let host = url.host().ok_or(RejectReason::InvalidUrl)?;
        let private_destination = is_private_destination(&host);
        self.check_host(&host)?;

        // Promote cleartext to TLS, but only for public destinations. A
        // deliberately permitted loopback or LAN target — a local dev server,
        // an internal service — usually has no certificate at all, and
        // upgrading it turns a working URL into a handshake failure.
        if scheme == "http" && !private_destination {
            url.set_scheme("https").map_err(|()| RejectReason::InvalidUrl)?;
        }

        Ok(url)
    }

    /// Reject a host based on its literal form, before any DNS lookup.
    fn check_host(&self, host: &Host<&str>) -> Result<(), RejectReason> {
        match host {
            // A bare IP literal never goes through the domain rules — there is
            // no name to match — so it is judged purely on reachability.
            //
            // Decimal (`2130706433`) and hex (`0x7f000001`) spellings of an
            // address are normalized to Ipv4 by the WHATWG parser, so they land
            // here too rather than slipping past as opaque domain names.
            Host::Ipv4(address) => self.check_ip(IpAddr::V4(*address), &address.to_string()),
            Host::Ipv6(address) => self.check_ip(IpAddr::V6(*address), &address.to_string()),
            Host::Domain(name) => {
                let name = name.to_ascii_lowercase();

                if matches_any(&name, &self.deny_domains) {
                    return Err(RejectReason::DeniedDomain { host: name });
                }
                if !self.allow_domains.is_empty() && !matches_any(&name, &self.allow_domains) {
                    return Err(RejectReason::NotAllowedDomain { host: name });
                }
                if self.allow_private_network {
                    return Ok(());
                }
                if is_private_domain(&name) {
                    return Err(RejectReason::PrivateHost { host: name });
                }
                // A name with no dot cannot be a public domain; it is a local
                // alias, a search-domain-completed name, or a container host.
                if !name.contains('.') {
                    return Err(RejectReason::NotPublicHostname { host: name });
                }
                Ok(())
            }
        }
    }

    fn check_ip(&self, address: IpAddr, display: &str) -> Result<(), RejectReason> {
        if self.allow_private_network || is_public_ip(address) {
            return Ok(());
        }
        Err(RejectReason::PrivateHost {
            host: display.to_string(),
        })
    }

    /// Re-check a host after DNS resolution.
    ///
    /// Without this, a public name whose A record points at 127.0.0.1 — or one
    /// that changes answers between the check and the connect — walks straight
    /// past [`Self::check`]. Resolution failures are not treated as rejections;
    /// the subsequent connect reports them with a better message.
    pub async fn check_resolved(&self, url: &Url) -> Result<(), RejectReason> {
        if self.allow_private_network {
            return Ok(());
        }
        let Some(Host::Domain(name)) = url.host() else {
            // Literal addresses were already judged in `check`.
            return Ok(());
        };
        let port = url.port_or_known_default().unwrap_or(443);

        let Ok(addresses) = lookup_host((name, port)).await else {
            return Ok(());
        };
        judge_resolved_addresses(name, addresses.map(|socket| socket.ip()))
    }

    /// Whether a redirect may be followed silently.
    ///
    /// Mirrors Claude Code: same scheme, same port, and the same host modulo a
    /// leading `www.`. Anything else is handed back to the model so a redirect
    /// through a trusted domain cannot quietly retarget the request.
    pub fn is_permitted_redirect(&self, original: &Url, target: &Url) -> bool {
        if original.scheme() != target.scheme() {
            return false;
        }
        if original.port_or_known_default() != target.port_or_known_default() {
            return false;
        }
        if !target.username().is_empty() || target.password().is_some() {
            return false;
        }
        match (original.host_str(), target.host_str()) {
            (Some(from), Some(to)) => strip_www(&from.to_ascii_lowercase()) == strip_www(&to.to_ascii_lowercase()),
            _ => false,
        }
    }

    /// Whether responses from this host may be returned verbatim, skipping the
    /// summarizer.
    ///
    /// Matching is stricter than the deny/allow rules on purpose. Those gate
    /// *whether* a fetch happens; this one decides whether raw remote text goes
    /// into the transcript unreviewed, so a rule must name the exact host.
    /// Subdomains only match when the rule opts in with a `*.` prefix —
    /// otherwise one attacker-controlled subdomain of a trusted site would
    /// inherit that trust.
    ///
    /// A rule may also carry a path prefix (`example.com/docs`), matched on
    /// segment boundaries so `/docs` does not match `/docs-evil`.
    pub fn is_preapproved(&self, url: &Url) -> bool {
        let Some(host) = url.host_str().map(|host| host.to_ascii_lowercase()) else {
            return false;
        };
        let path = url.path();
        self.preapproved_domains.iter().any(|rule| rule.matches(&host, path))
    }
}

/// One `preapproved_domains` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreapprovedRule {
    host: String,
    /// Set when the rule was written as `*.example.com`.
    include_subdomains: bool,
    /// Set when the rule carried a path, e.g. `github.com/anthropics`.
    path_prefix: Option<String>,
}

impl PreapprovedRule {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().to_ascii_lowercase();
        let (authority, path) = match raw.split_once('/') {
            Some((authority, path)) => (authority, Some(format!("/{}", path.trim_matches('/')))),
            None => (raw.as_str(), None),
        };

        let (authority, include_subdomains) = match authority.strip_prefix("*.") {
            Some(rest) => (rest, true),
            None => (authority.trim_start_matches('.'), false),
        };
        if authority.is_empty() {
            return None;
        }

        Some(Self {
            host: authority.to_string(),
            include_subdomains,
            path_prefix: path.filter(|path| path != "/"),
        })
    }

    fn matches(&self, host: &str, path: &str) -> bool {
        let host_matches = host == self.host || (self.include_subdomains && host.ends_with(&format!(".{}", self.host)));
        if !host_matches {
            return false;
        }
        match &self.path_prefix {
            None => true,
            Some(prefix) => path == prefix || path.starts_with(&format!("{prefix}/")),
        }
    }
}

/// Reject a resolved name if any of its addresses is non-public.
///
/// Split out from the lookup so the verdict is testable without depending on
/// what a resolver happens to return.
fn judge_resolved_addresses(name: &str, addresses: impl IntoIterator<Item = IpAddr>) -> Result<(), RejectReason> {
    for address in addresses {
        if !is_public_ip(address) {
            return Err(RejectReason::PrivateHost {
                host: format!("{name} ({address})"),
            });
        }
    }
    Ok(())
}

fn normalize_rules(rules: &[String]) -> Vec<String> {
    rules
        .iter()
        .map(|rule| rule.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|rule| !rule.is_empty())
        .collect()
}

/// Exact host match, or a subdomain of the rule.
///
/// Anchored on a dot boundary: a `evil.com` rule must not swallow
/// `notevil.com`.
fn matches_any(host: &str, rules: &[String]) -> bool {
    rules
        .iter()
        .any(|rule| host == rule || host.ends_with(&format!(".{rule}")))
}

fn strip_www(host: &str) -> String {
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// Whether a host is non-public on its face, independent of policy.
///
/// Used only to decide whether the http→https promotion applies; the
/// accept/reject decision lives in [`UrlPolicy::check_host`].
fn is_private_destination(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(address) => !is_public_ipv4(*address),
        Host::Ipv6(address) => !is_public_ipv6(*address),
        Host::Domain(name) => {
            let name = name.to_ascii_lowercase();
            is_private_domain(&name) || !name.contains('.')
        }
    }
}

/// Names that never denote a public host, regardless of DNS.
fn is_private_domain(name: &str) -> bool {
    name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.ends_with(".internal")
        || name.ends_with(".localdomain")
}

/// Whether an address is routable on the public internet.
///
/// Written as an allowlist of "not obviously internal" rather than a blocklist
/// of known-bad ranges, so an unlisted special-purpose range fails closed.
fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, ..] = address.octets();
    !(address.is_loopback()          // 127.0.0.0/8
        || address.is_private()      // 10/8, 172.16/12, 192.168/16
        || address.is_link_local()   // 169.254.0.0/16, incl. cloud metadata
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_unspecified()  // 0.0.0.0
        || a == 0                    // 0.0.0.0/8
        || a == 100 && (64..128).contains(&b) // 100.64/10 carrier-grade NAT
        || a == 192 && b == 0        // 192.0.0.0/24 IETF protocol assignments
        || a >= 224) // multicast and reserved
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if address.is_loopback() || address.is_unspecified() || address.is_multicast() {
        return false;
    }
    let first = address.segments()[0];
    // fc00::/7 unique-local and fe80::/10 link-local.
    if first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80 {
        return false;
    }
    // IPv4-mapped and IPv4-compatible forms would otherwise bypass the v4 rules.
    if let Some(v4) = address.to_ipv4() {
        return is_public_ipv4(v4);
    }
    true
}

#[cfg(test)]
#[path = "url_policy_test.rs"]
mod url_policy_test;
