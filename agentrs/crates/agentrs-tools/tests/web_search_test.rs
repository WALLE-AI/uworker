//! Integration tests for the WebSearch tool (TC-2.1-\*, TC-2.2-\*).
//!
//! Black-box: the tool is driven through the public `Tool` surface with a real
//! backend pointed at a mock provider, so parsing, filtering, and formatting
//! are all exercised together.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use agentrs_config::web::{SearchBackendKind, WebConfig};
use agentrs_tools::Tool;
use agentrs_tools::web::search_tool::WebSearchTool;
use agentrs_tools::web::{BackendError, build_backend};

const REMINDER: &str =
    "REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.";

/// Mock servers listen on loopback. `build_backend` deliberately honours the
/// ambient proxy settings — a real search provider is a public endpoint that a
/// corporate proxy should route — so the loopback exemption is declared here
/// rather than weakened in production code.
fn exempt_loopback_from_the_proxy() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
        std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
    });
}

/// Build a Brave-backed tool aimed at the mock server.
///
/// Brave is the key-bearing, no-server-side-filter case, so it exercises both
/// the credential path and the local domain filter.
fn brave_tool(server: &MockServer, max_results: usize) -> WebSearchTool {
    exempt_loopback_from_the_proxy();
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Brave;
    config.search.api_key_env = "AGENTRS_IT_BRAVE_KEY".to_string();
    config.search.base_url = format!("{}/res/v1/web/search", server.uri());
    config.search.timeout_secs = 5;
    unsafe { std::env::set_var("AGENTRS_IT_BRAVE_KEY", "integration-key") };

    let backend = build_backend(&config).expect("backend builds");
    WebSearchTool::new(backend, max_results)
}

async fn mount_results(server: &MockServer, results: Value) {
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "web": { "results": results } })))
        .mount(server)
        .await;
}

fn parse_links(content: &str) -> Vec<Value> {
    let line = content
        .lines()
        .find(|line| line.starts_with("Links: "))
        .unwrap_or_else(|| panic!("no Links line in:\n{content}"));
    serde_json::from_str(line.trim_start_matches("Links: ")).expect("Links payload is valid JSON")
}

#[tokio::test]
async fn returns_titles_and_urls_in_the_documented_format() {
    let server = MockServer::start().await;
    mount_results(
        &server,
        json!([
            { "title": "The Rust Book", "url": "https://doc.rust-lang.org/book/", "description": "Learn Rust" },
            { "title": "docs.rs", "url": "https://docs.rs" }
        ]),
    )
    .await;

    let result = brave_tool(&server, 10)
        .execute(json!({ "query": "rust documentation" }))
        .await;

    assert!(!result.is_error, "got: {}", result.content);
    assert!(
        result
            .content
            .starts_with("Web search results for query: \"rust documentation\""),
        "got: {}",
        result.content
    );
    let links = parse_links(&result.content);
    assert_eq!(links.len(), 2);
    assert_eq!(links[0]["url"], "https://doc.rust-lang.org/book/");
    assert_eq!(links[0]["snippet"], "Learn Rust");
    assert!(
        result.content.ends_with(REMINDER),
        "the attribution reminder must terminate the result: {}",
        result.content
    );
}

#[tokio::test]
async fn an_empty_result_set_is_a_successful_no_hits_answer() {
    let server = MockServer::start().await;
    mount_results(&server, json!([])).await;

    let result = brave_tool(&server, 10).execute(json!({ "query": "zzz" })).await;

    assert!(!result.is_error);
    assert!(result.content.contains("No links found."), "got: {}", result.content);
    assert!(result.content.ends_with(REMINDER));
}

#[tokio::test]
async fn blocked_domains_are_removed_from_the_answer() {
    let server = MockServer::start().await;
    mount_results(
        &server,
        json!([
            { "title": "Keep", "url": "https://good.example/1" },
            { "title": "Drop", "url": "https://spam.example/1" },
            { "title": "Drop sub", "url": "https://a.spam.example/1" }
        ]),
    )
    .await;

    let result = brave_tool(&server, 10)
        .execute(json!({ "query": "rust", "blocked_domains": ["spam.example"] }))
        .await;

    let links = parse_links(&result.content);
    assert_eq!(links.len(), 1);
    assert_eq!(links[0]["url"], "https://good.example/1");
}

#[tokio::test]
async fn results_are_capped_at_the_configured_maximum() {
    let server = MockServer::start().await;
    let results: Vec<Value> = (0..25)
        .map(|i| json!({ "title": format!("r{i}"), "url": format!("https://a.example/{i}") }))
        .collect();
    mount_results(&server, json!(results)).await;

    let result = brave_tool(&server, 4).execute(json!({ "query": "rust" })).await;

    assert_eq!(parse_links(&result.content).len(), 4);
}

#[tokio::test]
async fn the_api_key_is_sent_as_a_header_and_never_appears_in_output() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(wiremock::matchers::header("x-subscription-token", "integration-key"))
        .and(query_param("q", "rust"))
        .respond_with(ResponseTemplate::new(401).set_body_string("integration-key is invalid"))
        .expect(1)
        .mount(&server)
        .await;

    let result = brave_tool(&server, 10).execute(json!({ "query": "rust" })).await;

    assert!(result.is_error);
    assert!(result.content.contains("401"), "got: {}", result.content);
    assert!(
        !result.content.contains("integration-key"),
        "credentials must never reach the transcript: {}",
        result.content
    );
}

#[tokio::test]
async fn a_provider_timeout_is_reported_without_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    exempt_loopback_from_the_proxy();
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Brave;
    config.search.api_key_env = "AGENTRS_IT_BRAVE_KEY".to_string();
    config.search.base_url = format!("{}/res/v1/web/search", server.uri());
    config.search.timeout_secs = 1;
    unsafe { std::env::set_var("AGENTRS_IT_BRAVE_KEY", "integration-key") };
    let tool = WebSearchTool::new(build_backend(&config).expect("builds"), 10);

    let started = std::time::Instant::now();
    let result = tool.execute(json!({ "query": "confidential topic" })).await;

    assert!(result.is_error);
    assert!(result.content.contains("timed out"), "got: {}", result.content);
    assert!(
        !result.content.contains("confidential topic"),
        "transport errors must not echo the query: {}",
        result.content
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_malformed_provider_response_is_an_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html>gateway error</html>")
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let result = brave_tool(&server, 10).execute(json!({ "query": "rust" })).await;

    assert!(result.is_error);
    assert!(result.content.contains("malformed"), "got: {}", result.content);
}

// --- Input validation, through the public surface ---

#[tokio::test]
async fn mutually_exclusive_domain_filters_are_rejected_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let result = brave_tool(&server, 10)
        .execute(json!({
            "query": "rust",
            "allowed_domains": ["a.example"],
            "blocked_domains": ["b.example"],
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
async fn a_too_short_query_is_rejected_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let result = brave_tool(&server, 10).execute(json!({ "query": "a" })).await;
    assert!(result.is_error, "got: {}", result.content);
}

// --- Backend selection ---

#[tokio::test]
async fn no_backend_configured_reports_search_as_disabled() {
    let error = build_backend(&WebConfig::default()).map(|_| ()).unwrap_err();
    assert!(
        matches!(error, BackendError::Disabled),
        "the default build must be a clean 'off', not a failure: {error:?}"
    );
}

#[tokio::test]
async fn a_searxng_backend_needs_no_credentials() {
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Searxng;
    config.search.base_url = "https://searx.example".to_string();

    let backend: Arc<_> = build_backend(&config).expect("builds without a key");
    assert_eq!(backend.name(), "searxng");
}
