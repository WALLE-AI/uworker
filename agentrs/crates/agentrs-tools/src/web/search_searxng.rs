use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::web::search_backend::{DomainFilter, SearchBackend, SearchHit};
use crate::web::search_brave::{describe_transport_error, error_kind};

/// SearXNG backend, pointed at a self-hosted instance.
///
/// SearXNG has no API key and no domain-filter parameter, so the tool applies
/// the filter locally.
pub struct SearxngBackend {
    client: Client,
    base_url: String,
}

impl SearxngBackend {
    pub fn new(client: Client, base_url: String) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    fn search_endpoint(&self) -> String {
        format!("{}/search", self.base_url)
    }
}

#[async_trait]
impl SearchBackend for SearxngBackend {
    fn name(&self) -> &str {
        "searxng"
    }

    async fn search(&self, query: &str, _filter: &DomainFilter, limit: usize) -> Result<Vec<SearchHit>, String> {
        let response = self
            .client
            .get(self.search_endpoint())
            .query(&[("q", query), ("format", "json")])
            .send()
            .await
            .map_err(describe_transport_error)?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("SearXNG search failed with HTTP {}", status.as_u16()));
        }

        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("SearXNG search returned a malformed response: {}", error_kind(&error)))?;
        Ok(parse_hits(&body, limit))
    }
}

fn parse_hits(body: &Value, limit: usize) -> Vec<SearchHit> {
    body.get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|result| {
                    Some(SearchHit {
                        title: result.get("title").and_then(Value::as_str)?.to_string(),
                        url: result.get("url").and_then(Value::as_str)?.to_string(),
                        snippet: result.get("content").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "search_searxng_test.rs"]
mod search_searxng_test;
