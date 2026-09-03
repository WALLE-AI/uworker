use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::web::search_backend::{DomainFilter, SearchBackend, SearchHit};

const DEFAULT_BASE_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// Brave Search API backend.
pub struct BraveBackend {
    client: Client,
    api_key: String,
    base_url: String,
}

impl BraveBackend {
    pub fn new(client: Client, api_key: String, base_url: String) -> Self {
        let base_url = if base_url.trim().is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            base_url
        };
        Self {
            client,
            api_key,
            base_url,
        }
    }
}

#[async_trait]
impl SearchBackend for BraveBackend {
    fn name(&self) -> &str {
        "brave"
    }

    async fn search(&self, query: &str, _filter: &DomainFilter, limit: usize) -> Result<Vec<SearchHit>, String> {
        let response = self
            .client
            .get(&self.base_url)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .query(&[("q", query), ("count", &limit.to_string())])
            .send()
            .await
            .map_err(describe_transport_error)?;

        let status = response.status();
        if !status.is_success() {
            // The key travels in a header, so it cannot leak through the URL;
            // the body is dropped in case the provider echoes credentials.
            return Err(format!("Brave search failed with HTTP {}", status.as_u16()));
        }

        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("Brave search returned a malformed response: {}", error_kind(&error)))?;
        Ok(parse_hits(&body, limit))
    }
}

fn parse_hits(body: &Value, limit: usize) -> Vec<SearchHit> {
    body.get("web")
        .and_then(|web| web.get("results"))
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|result| {
                    Some(SearchHit {
                        title: result.get("title").and_then(Value::as_str)?.to_string(),
                        url: result.get("url").and_then(Value::as_str)?.to_string(),
                        snippet: result.get("description").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

/// Describe a transport failure without echoing the request URL, which can
/// carry the query string.
pub(crate) fn describe_transport_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "search request timed out".to_string()
    } else if error.is_connect() {
        "could not connect to the search provider".to_string()
    } else {
        format!("search request failed: {}", error_kind(&error))
    }
}

pub(crate) fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_decode() {
        "invalid response body"
    } else if error.is_connect() {
        "connection failure"
    } else {
        "transport error"
    }
}

#[cfg(test)]
#[path = "search_brave_test.rs"]
mod search_brave_test;
