use serde::{Deserialize, Serialize};

/// Settings for the network-facing tools (`WebFetch`, `WebSearch`).
///
/// Everything that bounds outbound traffic lives here: transfer limits,
/// timeouts, the response cache, and the host policy that decides which URLs
/// may be reached at all. Defaults mirror the limits Claude Code applies,
/// except that private-network access is denied unless explicitly enabled.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WebConfig {
    /// Master switch. When false, neither web tool is registered.
    #[serde(default = "default_true")]
    pub enabled: bool,

    // --- Transfer limits ---
    /// Per-request timeout for a single HTTP fetch.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Hard ceiling on a response body, enforced while streaming.
    #[serde(default = "default_max_content_bytes")]
    pub max_content_bytes: u64,
    /// Maximum same-host redirect hops before giving up.
    #[serde(default = "default_max_redirects")]
    pub max_redirects: usize,
    /// Maximum accepted URL length.
    #[serde(default = "default_max_url_length")]
    pub max_url_length: usize,
    /// Characters of extracted Markdown kept before truncation.
    #[serde(default = "default_max_markdown_chars")]
    pub max_markdown_chars: usize,

    // --- Response cache ---
    /// How long a fetched URL stays in the response cache.
    #[serde(default = "default_cache_ttl_secs")]
    pub cache_ttl_secs: u64,
    /// Total byte budget for the response cache.
    #[serde(default = "default_cache_max_bytes")]
    pub cache_max_bytes: u64,

    // --- Host policy ---
    /// Permit loopback, link-local, and RFC1918 destinations.
    ///
    /// Off by default: with no upstream domain-reputation service to fall back
    /// on, allowing these would let a prompt-injected URL reach internal
    /// services or a cloud metadata endpoint.
    #[serde(default)]
    pub allow_private_network: bool,
    /// When non-empty, only these domains (and their subdomains) are reachable.
    #[serde(default)]
    pub allow_domains: Vec<String>,
    /// Domains (and their subdomains) that are never reachable. Takes
    /// precedence over `allow_domains`.
    #[serde(default)]
    pub deny_domains: Vec<String>,
    /// Domains whose Markdown responses are returned verbatim, skipping the
    /// summarizer. Mirrors Claude Code's preapproved-host list.
    #[serde(default)]
    pub preapproved_domains: Vec<String>,
    /// User-Agent header. Empty means a version-derived default.
    #[serde(default)]
    pub user_agent: String,

    #[serde(default)]
    pub search: WebSearchConfig,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            timeout_secs: default_timeout_secs(),
            max_content_bytes: default_max_content_bytes(),
            max_redirects: default_max_redirects(),
            max_url_length: default_max_url_length(),
            max_markdown_chars: default_max_markdown_chars(),
            cache_ttl_secs: default_cache_ttl_secs(),
            cache_max_bytes: default_cache_max_bytes(),
            allow_private_network: false,
            allow_domains: Vec::new(),
            deny_domains: Vec::new(),
            preapproved_domains: Vec::new(),
            user_agent: String::new(),
            search: WebSearchConfig::default(),
        }
    }
}

impl WebConfig {
    /// Resolved User-Agent, falling back to a version-derived string.
    ///
    /// The default carries a `+URL` the way crawlers conventionally do, so a
    /// site operator seeing this traffic can identify what it is and block it
    /// at their edge. A bare product token gives them nothing to act on.
    pub fn effective_user_agent(&self) -> String {
        if self.user_agent.trim().is_empty() {
            format!(
                "agentrs/{} (+https://github.com/iOfficeAI/agentrs)",
                env!("CARGO_PKG_VERSION")
            )
        } else {
            self.user_agent.clone()
        }
    }
}

/// Which search provider backs the `WebSearch` tool.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackendKind {
    /// No backend configured — the tool is not registered.
    #[default]
    None,
    Brave,
    Tavily,
    Searxng,
    /// Keyless, but scrapes a human-facing HTML page. See
    /// `web::search_duckduckgo` for the trade-offs.
    Duckduckgo,
}

/// Search-provider settings for the `WebSearch` tool.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct WebSearchConfig {
    #[serde(default)]
    pub backend: SearchBackendKind,
    /// Environment variable holding the backend's API key.
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    /// Base URL override; required for a self-hosted SearXNG instance.
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default = "default_search_timeout_secs")]
    pub timeout_secs: u64,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            backend: SearchBackendKind::None,
            api_key_env: default_api_key_env(),
            base_url: String::new(),
            max_results: default_max_results(),
            timeout_secs: default_search_timeout_secs(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_timeout_secs() -> u64 {
    60
}
fn default_max_content_bytes() -> u64 {
    10 * 1024 * 1024
}
fn default_max_redirects() -> usize {
    10
}
fn default_max_url_length() -> usize {
    2000
}
fn default_max_markdown_chars() -> usize {
    100_000
}
fn default_cache_ttl_secs() -> u64 {
    15 * 60
}
fn default_cache_max_bytes() -> u64 {
    50 * 1024 * 1024
}
fn default_api_key_env() -> String {
    "BRAVE_API_KEY".to_string()
}
fn default_max_results() -> usize {
    10
}
fn default_search_timeout_secs() -> u64 {
    30
}

#[cfg(test)]
#[path = "web_test.rs"]
mod web_test;
