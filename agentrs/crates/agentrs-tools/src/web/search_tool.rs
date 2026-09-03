use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Utc};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use agentrs_protocol::events::ToolCategory;
use agentrs_types::tool::{JsonSchema, ToolResult};

use crate::Tool;
use crate::web::search_backend::{DomainFilter, SearchBackend, SearchHit};

/// Static part of the tool description. The current month is appended at
/// construction — without it models search for a year they were trained on and
/// silently return stale documentation.
const DESCRIPTION_BODY: &str = "\
Search the web and use the results to answer. Use it for anything that may have changed since your \
training cutoff: current events, releases, prices, APIs, or documentation.

Usage notes:
- `allowed_domains` and `blocked_domains` filter results and are mutually exclusive.
- Prefer primary and authoritative sources, and say so when you are inferring rather than quoting.

CRITICAL - you MUST do this:
- After answering, end your response with a \"Sources:\" section listing every result you relied on as a \
markdown link: [Title](URL).
- Link to the page that supports the claim, never to a search result page or a bare URL.
- Never skip the Sources section.

  Example:

    [your answer]

    Sources:
    - [Source Title 1](https://example.com/1)
    - [Source Title 2](https://example.com/2)";

/// Appended to every result set. The original tool relies on this line to get
/// the model to attribute its sources, so it is reproduced verbatim.
const SOURCE_REMINDER: &str =
    "REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.";

const MIN_QUERY_LENGTH: usize = 2;

/// Searches the web through a configured provider.
pub struct WebSearchTool {
    backend: Arc<dyn SearchBackend>,
    max_results: usize,
    /// `DESCRIPTION_BODY` plus the current month, resolved once per session.
    description: String,
}

impl WebSearchTool {
    pub fn new(backend: Arc<dyn SearchBackend>, max_results: usize) -> Self {
        Self {
            backend,
            max_results: max_results.max(1),
            description: build_description(&Utc::now()),
        }
    }

    fn parse(&self, input: &Value) -> Result<(String, DomainFilter), String> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if query.is_empty() {
            return Err("Error: Missing query".to_string());
        }
        if query.chars().count() < MIN_QUERY_LENGTH {
            return Err(format!("Error: query must be at least {MIN_QUERY_LENGTH} characters"));
        }

        let allowed = string_list(input, "allowed_domains")?;
        let blocked = string_list(input, "blocked_domains")?;
        if !allowed.is_empty() && !blocked.is_empty() {
            return Err(
                "Error: Cannot specify both allowed_domains and blocked_domains in the same request".to_string(),
            );
        }

        Ok((query.to_string(), DomainFilter { allowed, blocked }))
    }

    async fn run(&self, query: &str, filter: &DomainFilter) -> ToolResult {
        let hits = match self.backend.search(query, filter, self.max_results).await {
            Ok(hits) => hits,
            Err(error) => {
                return ToolResult {
                    content: format!("Web search failed: {error}"),
                    is_error: true,
                };
            }
        };

        // Backends that filter server-side have already applied the rules;
        // re-applying locally would be redundant but harmless, so it is skipped
        // to keep provider-side ranking intact.
        let hits = if self.backend.supports_domain_filter() {
            hits
        } else {
            filter.retain(hits)
        };
        let hits: Vec<SearchHit> = hits.into_iter().take(self.max_results).collect();

        ToolResult {
            content: format_results(query, &hits),
            is_error: false,
        }
    }
}

/// Read a domain list argument.
///
/// Models routinely send this field as a JSON-encoded string
/// (`"[\"a.com\"]"`) or as a single bare domain rather than the declared array,
/// so both are accepted. Anything else is an error: silently dropping a filter
/// the caller asked for would return the very results they meant to exclude,
/// with nothing in the output to say the filter was ignored.
/// Tell the model what "now" is, so date-sensitive queries use the right year.
fn build_description(now: &DateTime<Utc>) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let month = MONTHS[(now.month0() as usize).min(11)];
    let year = now.year();
    format!(
        "{DESCRIPTION_BODY}\n\n\
         Use the correct year in queries: it is currently {month} {year}. When searching for recent \
         information or the latest documentation, search for {year}, not the year you were trained on."
    )
}

fn string_list(input: &Value, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };

    let items: Vec<String> = match value {
        Value::Array(values) => {
            let mut domains = Vec::with_capacity(values.len());
            for entry in values {
                let Some(domain) = entry.as_str() else {
                    return Err(format!("Error: {key} must contain only domain strings"));
                };
                domains.push(domain.to_string());
            }
            domains
        }
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.starts_with('[') {
                let parsed: Value = serde_json::from_str(trimmed)
                    .map_err(|_| format!("Error: {key} is not a valid array of domain strings"))?;
                return string_list(&json!({ key: parsed }), key);
            }
            vec![trimmed.to_string()]
        }
        _ => return Err(format!("Error: {key} must be an array of domain strings")),
    };

    Ok(items
        .into_iter()
        .map(|domain| domain.trim().to_string())
        .filter(|domain| !domain.is_empty())
        .collect())
}

fn format_results(query: &str, hits: &[SearchHit]) -> String {
    let mut output = format!("Web search results for query: \"{query}\"\n\n");
    if hits.is_empty() {
        output.push_str("No links found.\n\n");
    } else {
        let links: Vec<Value> = hits
            .iter()
            .map(|hit| match &hit.snippet {
                Some(snippet) => json!({ "title": hit.title, "url": hit.url, "snippet": snippet }),
                None => json!({ "title": hit.title, "url": hit.url }),
            })
            .collect();
        let encoded = serde_json::to_string(&links).unwrap_or_else(|_| "[]".to_string());
        output.push_str(&format!("Links: {encoded}\n\n"));
    }
    output.push_str(SOURCE_REMINDER);
    output
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": MIN_QUERY_LENGTH,
                    "description": "The search query to use"
                },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only include search results from these domains"
                },
                "blocked_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Never include search results from these domains"
                }
            },
            "required": ["query"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn is_deferred(&self) -> bool {
        true
    }

    fn max_result_size(&self) -> usize {
        100_000
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Network
    }

    async fn execute(&self, input: Value) -> ToolResult {
        self.execute_cancellable(input, CancellationToken::new()).await
    }

    async fn execute_cancellable(&self, input: Value, cancel: CancellationToken) -> ToolResult {
        let (query, filter) = match self.parse(&input) {
            Ok(parsed) => parsed,
            Err(message) => {
                return ToolResult {
                    content: message,
                    is_error: true,
                };
            }
        };

        tokio::select! {
            biased;
            () = cancel.cancelled() => ToolResult {
                content: "Web search cancelled before it completed".to_string(),
                is_error: true,
            },
            result = self.run(&query, &filter) => result,
        }
    }

    fn describe(&self, input: &Value) -> String {
        match input.get("query").and_then(Value::as_str) {
            Some(query) => format!("Search the web for \"{query}\""),
            None => "Search the web".to_string(),
        }
    }
}

#[cfg(test)]
#[path = "search_tool_test.rs"]
mod search_tool_test;
