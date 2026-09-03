use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

use crate::web::search_backend::{DomainFilter, SearchBackend, SearchHit};
use crate::web::search_brave::{describe_transport_error, error_kind};

const DEFAULT_BASE_URL: &str = "https://api.tavily.com/search";

/// Tavily Search API backend.
///
/// Unlike Brave, Tavily accepts include/exclude domain lists, so the tool's
/// domain filter is pushed down instead of applied locally.
pub struct TavilyBackend {
    client: Client,
    api_key: String,
    base_url: String,
}

impl TavilyBackend {
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
impl SearchBackend for TavilyBackend {
    fn name(&self) -> &str {
        "tavily"
    }

    fn supports_domain_filter(&self) -> bool {
        true
    }

    async fn search(&self, query: &str, filter: &DomainFilter, limit: usize) -> Result<Vec<SearchHit>, String> {
        let body = json!({
            "query": query,
            "max_results": limit,
            "include_domains": filter.allowed,
            "exclude_domains": filter.blocked,
        });

        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(describe_transport_error)?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("Tavily search failed with HTTP {}", status.as_u16()));
        }

        let body: Value = response
            .json()
            .await
            .map_err(|error| format!("Tavily search returned a malformed response: {}", error_kind(&error)))?;
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
#[path = "search_tavily_test.rs"]
mod search_tavily_test;
