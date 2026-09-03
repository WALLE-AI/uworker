use std::env;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;

use agentrs_config::web::{SearchBackendKind, WebConfig, WebSearchConfig};

use crate::web::search_brave::BraveBackend;
use crate::web::search_duckduckgo::DuckDuckGoBackend;
use crate::web::search_searxng::SearxngBackend;
use crate::web::search_tavily::TavilyBackend;

/// One result row from a search provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

/// Domain restrictions requested by the model.
///
/// The two lists are mutually exclusive at the tool boundary, so at most one is
/// ever populated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainFilter {
    pub allowed: Vec<String>,
    pub blocked: Vec<String>,
}

impl DomainFilter {
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty() && self.blocked.is_empty()
    }

    /// Apply the filter locally, for backends that cannot push it down.
    pub fn retain(&self, hits: Vec<SearchHit>) -> Vec<SearchHit> {
        if self.is_empty() {
            return hits;
        }
        hits.into_iter()
            .filter(|hit| {
                let Some(host) = host_of(&hit.url) else {
                    // An unparseable result URL is dropped under an allow-list
                    // (it cannot be shown to be permitted) but kept under a
                    // block-list (it cannot be shown to be forbidden).
                    return self.allowed.is_empty();
                };
                if !self.allowed.is_empty() {
                    return self.allowed.iter().any(|rule| host_matches(&host, rule));
                }
                !self.blocked.iter().any(|rule| host_matches(&host, rule))
            })
            .collect()
    }
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
}

fn host_matches(host: &str, rule: &str) -> bool {
    let rule = rule.trim().trim_start_matches('.').to_ascii_lowercase();
    !rule.is_empty() && (host == rule || host.ends_with(&format!(".{rule}")))
}

/// A pluggable web search provider.
///
/// Keeping this behind a trait is what lets `WebSearch` stay provider-neutral:
/// the tool never learns which LLM vendor is in use, and no provider gains a
/// hardcoded server-side-tool path.
#[async_trait]
pub trait SearchBackend: Send + Sync {
    fn name(&self) -> &str;

    async fn search(&self, query: &str, filter: &DomainFilter, limit: usize) -> Result<Vec<SearchHit>, String>;

    /// Whether the provider applies `filter` itself. When false the tool
    /// filters the returned rows locally.
    fn supports_domain_filter(&self) -> bool {
        false
    }
}

/// Why no backend could be built.
#[derive(Debug, PartialEq, Eq)]
pub enum BackendError {
    /// No backend was requested; `WebSearch` should not be registered.
    Disabled,
    /// A backend was requested but is not usable as configured.
    Misconfigured(String),
}

/// Build the configured search backend.
///
/// Returns [`BackendError::Disabled`] when search is switched off, which the
/// caller treats as "do not register the tool" rather than as a failure.
pub fn build_backend(config: &WebConfig) -> Result<Arc<dyn SearchBackend>, BackendError> {
    let search = &config.search;
    let client = build_client(search).map_err(BackendError::Misconfigured)?;

    match search.backend {
        SearchBackendKind::None => Err(BackendError::Disabled),
        SearchBackendKind::Brave => {
            let key = require_api_key(search)?;
            Ok(Arc::new(BraveBackend::new(client, key, search.base_url.clone())))
        }
        SearchBackendKind::Tavily => {
            let key = require_api_key(search)?;
            Ok(Arc::new(TavilyBackend::new(client, key, search.base_url.clone())))
        }
        SearchBackendKind::Duckduckgo => Ok(Arc::new(DuckDuckGoBackend::new(client, search.base_url.clone()))),
        SearchBackendKind::Searxng => {
            let base_url = search.base_url.trim();
            if base_url.is_empty() {
                return Err(BackendError::Misconfigured(
                    "web.search.base_url is required for the searxng backend".to_string(),
                ));
            }
            Ok(Arc::new(SearxngBackend::new(client, base_url.to_string())))
        }
    }
}

fn build_client(search: &WebSearchConfig) -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(search.timeout_secs))
        .build()
        .map_err(|error| format!("failed to build the search HTTP client: {error}"))
}

fn require_api_key(search: &WebSearchConfig) -> Result<String, BackendError> {
    let variable = search.api_key_env.trim();
    if variable.is_empty() {
        return Err(BackendError::Misconfigured(
            "web.search.api_key_env is empty; no API key can be read".to_string(),
        ));
    }
    match env::var(variable) {
        Ok(key) if !key.trim().is_empty() => Ok(key),
        // The variable name is safe to name; the value never is.
        _ => Err(BackendError::Misconfigured(format!(
            "environment variable {variable} is not set"
        ))),
    }
}

#[cfg(test)]
#[path = "search_backend_test.rs"]
mod search_backend_test;
