use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use agentrs_protocol::events::ToolCategory;

use super::{SOURCE_REMINDER, WebSearchTool};
use crate::Tool;
use crate::web::search_backend::{DomainFilter, SearchBackend, SearchHit};

/// Backend stub that records what the tool asked for and replays a fixed reply.
struct StubBackend {
    hits: Result<Vec<SearchHit>, String>,
    pushes_down_filter: bool,
    seen: Mutex<Vec<(String, DomainFilter, usize)>>,
}

impl StubBackend {
    fn returning(hits: Vec<SearchHit>) -> Arc<Self> {
        Arc::new(Self {
            hits: Ok(hits),
            pushes_down_filter: false,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            hits: Err(message.to_string()),
            pushes_down_filter: false,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn with_server_side_filter(hits: Vec<SearchHit>) -> Arc<Self> {
        Arc::new(Self {
            hits: Ok(hits),
            pushes_down_filter: true,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn last_call(&self) -> (String, DomainFilter, usize) {
        self.seen.lock().expect("poisoned").last().cloned().expect("a call")
    }
}

#[async_trait]
impl SearchBackend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    fn supports_domain_filter(&self) -> bool {
        self.pushes_down_filter
    }

    async fn search(&self, query: &str, filter: &DomainFilter, limit: usize) -> Result<Vec<SearchHit>, String> {
        self.seen
            .lock()
            .expect("poisoned")
            .push((query.to_string(), filter.clone(), limit));
        self.hits.clone()
    }
}

fn hit(title: &str, url: &str) -> SearchHit {
    SearchHit {
        title: title.to_string(),
        url: url.to_string(),
        snippet: None,
    }
}

fn tool(backend: Arc<dyn SearchBackend>) -> WebSearchTool {
    WebSearchTool::new(backend, 10)
}

/// Pull the `Links: [...]` payload back out so format assertions check real
/// JSON rather than substrings.
fn parse_links(content: &str) -> Vec<Value> {
    let line = content
        .lines()
        .find(|line| line.starts_with("Links: "))
        .expect("a Links line");
    serde_json::from_str(line.trim_start_matches("Links: ")).expect("Links payload is valid JSON")
}

// --- TC-2.1-01 through TC-2.1-05: input validation ---

#[tokio::test]
async fn missing_query_is_an_error() {
    let result = tool(StubBackend::returning(vec![])).execute(json!({})).await;
    assert!(result.is_error);
    assert!(result.content.contains("Missing query"), "got: {}", result.content);
}

#[tokio::test]
async fn blank_query_is_an_error() {
    let result = tool(StubBackend::returning(vec![]))
        .execute(json!({ "query": "   " }))
        .await;
    assert!(result.is_error);
}

#[tokio::test]
async fn single_character_query_is_rejected_but_two_is_accepted() {
    let backend = StubBackend::returning(vec![]);
    let short = tool(backend.clone()).execute(json!({ "query": "a" })).await;
    assert!(short.is_error, "one character is below the minimum");

    let ok = tool(backend).execute(json!({ "query": "ab" })).await;
    assert!(!ok.is_error, "two characters is the inclusive boundary");
}

#[tokio::test]
async fn domain_filters_are_mutually_exclusive() {
    let result = tool(StubBackend::returning(vec![]))
        .execute(json!({
            "query": "rust",
            "allowed_domains": ["a.com"],
            "blocked_domains": ["b.com"],
        }))
        .await;

    assert!(result.is_error);
    assert!(
        result.content.contains("Cannot specify both"),
        "got: {}",
        result.content
    );
}

#[tokio::test]
async fn an_empty_filter_list_does_not_trigger_the_exclusivity_check() {
    let result = tool(StubBackend::returning(vec![]))
        .execute(json!({
            "query": "rust",
            "allowed_domains": ["a.com"],
            "blocked_domains": [],
        }))
        .await;

    assert!(!result.is_error, "got: {}", result.content);
}

// --- TC-2.1-06 through TC-2.1-09: output shape ---

#[tokio::test]
async fn output_opens_with_the_query_and_closes_with_the_reminder() {
    let backend = StubBackend::returning(vec![hit("Rust", "https://rust-lang.org")]);
    let result = tool(backend).execute(json!({ "query": "rust lang" })).await;

    assert!(
        result
            .content
            .starts_with("Web search results for query: \"rust lang\""),
        "got: {}",
        result.content
    );
    assert!(
        result.content.ends_with(SOURCE_REMINDER),
        "the source reminder drives attribution and must be last, got: {}",
        result.content
    );
}

#[tokio::test]
async fn links_are_emitted_as_a_json_array_of_title_and_url() {
    let backend = StubBackend::returning(vec![
        hit("First", "https://a.com/1"),
        SearchHit {
            title: "Second".into(),
            url: "https://b.com/2".into(),
            snippet: Some("context".into()),
        },
    ]);
    let result = tool(backend).execute(json!({ "query": "rust" })).await;

    let links = parse_links(&result.content);
    assert_eq!(links.len(), 2);
    assert_eq!(links[0]["title"], "First");
    assert_eq!(links[0]["url"], "https://a.com/1");
    assert!(links[0].get("snippet").is_none(), "absent snippets are omitted");
    assert_eq!(links[1]["snippet"], "context");
}

#[tokio::test]
async fn an_empty_result_set_is_reported_without_being_an_error() {
    let result = tool(StubBackend::returning(vec![]))
        .execute(json!({ "query": "obscure" }))
        .await;

    assert!(!result.is_error, "no hits is a valid outcome, not a failure");
    assert!(result.content.contains("No links found."), "got: {}", result.content);
    assert!(result.content.ends_with(SOURCE_REMINDER));
}

// --- TC-2.1-10 through TC-2.1-12: filtering and limits ---

#[tokio::test]
async fn blocked_domains_are_filtered_locally_when_the_backend_cannot() {
    let backend = StubBackend::returning(vec![
        hit("Keep", "https://good.com/1"),
        hit("Drop", "https://bad.com/1"),
        hit("Drop sub", "https://sub.bad.com/1"),
    ]);
    let result = tool(backend)
        .execute(json!({ "query": "rust", "blocked_domains": ["bad.com"] }))
        .await;

    let links = parse_links(&result.content);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["url"], "https://good.com/1");
}

#[tokio::test]
async fn allowed_domains_are_filtered_locally_when_the_backend_cannot() {
    let backend = StubBackend::returning(vec![
        hit("Keep", "https://ok.com/1"),
        hit("Keep sub", "https://api.ok.com/1"),
        hit("Drop", "https://notok.com/1"),
    ]);
    let result = tool(backend)
        .execute(json!({ "query": "rust", "allowed_domains": ["ok.com"] }))
        .await;

    let links = parse_links(&result.content);
    assert_eq!(links.len(), 2, "suffix matching must anchor on a dot boundary");
    assert!(
        links
            .iter()
            .all(|link| link["url"].as_str().unwrap().contains("ok.com"))
    );
    assert!(!links.iter().any(|link| link["url"] == "https://notok.com/1"));
}

#[tokio::test]
async fn a_server_side_filter_is_pushed_down_and_not_reapplied() {
    // The backend claims it filtered; the tool must trust that rather than
    // second-guessing the provider's ranking.
    let backend = StubBackend::with_server_side_filter(vec![hit("Kept", "https://bad.com/1")]);
    let result = tool(backend.clone())
        .execute(json!({ "query": "rust", "blocked_domains": ["bad.com"] }))
        .await;

    let (_, filter, _) = backend.last_call();
    assert_eq!(filter.blocked, vec!["bad.com".to_string()]);
    assert_eq!(parse_links(&result.content).len(), 1);
}

#[tokio::test]
async fn results_are_capped_at_max_results() {
    let hits: Vec<SearchHit> = (0..20)
        .map(|i| hit(&format!("t{i}"), &format!("https://a.com/{i}")))
        .collect();
    let backend = StubBackend::returning(hits);
    let result = WebSearchTool::new(backend.clone(), 3)
        .execute(json!({ "query": "rust" }))
        .await;

    assert_eq!(parse_links(&result.content).len(), 3);
    let (_, _, limit) = backend.last_call();
    assert_eq!(limit, 3, "the cap is also communicated to the backend");
}

// --- TC-2.1-13: backend failure ---

#[tokio::test]
async fn a_backend_failure_is_reported_without_leaking_credentials() {
    let backend = StubBackend::failing("Brave search failed with HTTP 401");
    let result = tool(backend).execute(json!({ "query": "rust" })).await;

    assert!(result.is_error);
    assert!(result.content.contains("401"), "got: {}", result.content);
    assert!(
        !result.content.to_lowercase().contains("token") && !result.content.to_lowercase().contains("api_key"),
        "error text must not carry credentials, got: {}",
        result.content
    );
}

// --- TC-2.1-15: escaping ---

#[tokio::test]
async fn special_characters_in_results_stay_valid_json() {
    let backend = StubBackend::returning(vec![hit(
        "Quote \" and newline \n and 中文 🎉",
        "https://a.com/?q=a%20b&c=\"d\"",
    )]);
    let result = tool(backend).execute(json!({ "query": "rust" })).await;

    let links = parse_links(&result.content);
    assert_eq!(links[0]["title"], "Quote \" and newline \n and 中文 🎉");
}

// --- Cancellation and metadata ---

#[tokio::test]
async fn an_already_cancelled_token_stops_the_search() {
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let result = tool(StubBackend::returning(vec![]))
        .execute_cancellable(json!({ "query": "rust" }), cancel)
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("cancelled"), "got: {}", result.content);
}

#[tokio::test]
async fn advertises_the_expected_metadata() {
    let tool = tool(StubBackend::returning(vec![]));
    assert_eq!(tool.name(), "WebSearch");
    assert_eq!(tool.category(), ToolCategory::Network);
    assert!(tool.is_deferred());
    assert!(tool.is_concurrency_safe(&json!({})));
    assert_eq!(tool.max_result_size(), 100_000);
    assert_eq!(
        tool.describe(&json!({ "query": "rust lang" })),
        "Search the web for \"rust lang\""
    );
}

#[tokio::test]
async fn input_schema_marks_query_required_with_a_minimum_length() {
    let schema = tool(StubBackend::returning(vec![])).input_schema();
    assert_eq!(schema["required"].as_array().unwrap(), &[json!("query")]);
    assert_eq!(schema["properties"]["query"]["minLength"], 2);
}

// --- Malformed domain-list arguments ---
//
// Regression: a model sent `"blocked_domains": "[\"spam.example\"]"` and the
// filter was silently dropped, returning the very domain it was meant to
// exclude with nothing in the output to say so.

#[tokio::test]
async fn a_json_encoded_array_of_domains_is_accepted() {
    let backend = StubBackend::returning(vec![
        hit("Keep", "https://good.com/1"),
        hit("Drop", "https://spam.example/1"),
    ]);
    let result = tool(backend)
        .execute(json!({ "query": "rust", "blocked_domains": "[\"spam.example\"]" }))
        .await;

    assert!(!result.is_error, "got: {}", result.content);
    let links = parse_links(&result.content);
    assert_eq!(links.len(), 1, "the stringified filter must still be applied");
    assert_eq!(links[0]["url"], "https://good.com/1");
}

#[tokio::test]
async fn a_single_bare_domain_is_accepted_as_a_one_element_list() {
    let backend = StubBackend::returning(vec![
        hit("Keep", "https://good.com/1"),
        hit("Drop", "https://spam.example/1"),
    ]);
    let result = tool(backend)
        .execute(json!({ "query": "rust", "blocked_domains": "spam.example" }))
        .await;

    let links = parse_links(&result.content);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["url"], "https://good.com/1");
}

#[tokio::test]
async fn an_uninterpretable_domain_list_is_an_error_rather_than_being_ignored() {
    for bad in [json!(42), json!({ "a": 1 }), json!([1, 2]), json!("[not json")] {
        let backend = StubBackend::returning(vec![hit("Anything", "https://spam.example/1")]);
        let result = tool(backend)
            .execute(json!({ "query": "rust", "blocked_domains": bad }))
            .await;

        assert!(
            result.is_error,
            "{bad} must be rejected, not silently dropped — got: {}",
            result.content
        );
        assert!(result.content.contains("blocked_domains"), "got: {}", result.content);
    }
}

#[tokio::test]
async fn a_null_or_absent_domain_list_is_simply_no_filter() {
    for input in [
        json!({ "query": "rust" }),
        json!({ "query": "rust", "blocked_domains": null }),
    ] {
        let backend = StubBackend::returning(vec![hit("Any", "https://a.com/1")]);
        let result = tool(backend).execute(input.clone()).await;
        assert!(!result.is_error, "{input} got: {}", result.content);
        assert_eq!(parse_links(&result.content).len(), 1);
    }
}

#[tokio::test]
async fn a_stringified_empty_array_does_not_trigger_the_exclusivity_check() {
    let backend = StubBackend::returning(vec![]);
    let result = tool(backend)
        .execute(json!({
            "query": "rust",
            "allowed_domains": "[\"ok.com\"]",
            "blocked_domains": "[]",
        }))
        .await;

    assert!(!result.is_error, "got: {}", result.content);
}

// --- Borrowed from Claude Code: date anchoring and mandatory attribution ---

// Without a stated "now", models search for their training year and return
// documentation that is silently a release or two behind.
#[tokio::test]
async fn description_states_the_current_month_and_year() {
    use chrono::{Datelike, Utc};

    let description = tool(StubBackend::returning(vec![])).description().to_string();
    let now = Utc::now();

    assert!(
        description.contains(&now.year().to_string()),
        "the current year must appear so queries are not anchored to the training cutoff: {description}"
    );
    assert!(
        description.to_lowercase().contains("year you were trained on")
            || description.to_lowercase().contains("not the year"),
        "got: {description}"
    );
}

#[test]
fn the_description_date_tracks_the_supplied_clock() {
    use chrono::TimeZone;

    let january = chrono::Utc.with_ymd_and_hms(2031, 1, 15, 0, 0, 0).unwrap();
    let december = chrono::Utc.with_ymd_and_hms(2031, 12, 15, 0, 0, 0).unwrap();

    assert!(super::build_description(&january).contains("January 2031"));
    assert!(super::build_description(&december).contains("December 2031"));
}

#[tokio::test]
async fn description_demands_a_sources_section_with_a_concrete_format() {
    let description = tool(StubBackend::returning(vec![])).description().to_string();

    assert!(description.contains("Sources:"), "got: {description}");
    assert!(
        description.contains("[Title](URL)") || description.contains("](https://example.com/1)"),
        "a worked example makes the format stick: {description}"
    );
    assert!(
        description.contains("never to a search result page") || description.contains("never skip"),
        "got: {description}"
    );
}
