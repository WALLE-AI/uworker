use std::time::Duration;

use reqwest::Client;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::BraveBackend;
use crate::web::search_backend::{DomainFilter, SearchBackend};

/// Mock servers listen on loopback, which an ambient `HTTP_PROXY` would
/// otherwise intercept.
fn test_client(timeout: Duration) -> Client {
    Client::builder()
        .timeout(timeout)
        .no_proxy()
        .build()
        .expect("client builds")
}

fn backend(server: &MockServer) -> BraveBackend {
    BraveBackend::new(
        test_client(Duration::from_secs(5)),
        "test-key".to_string(),
        format!("{}/search", server.uri()),
    )
}

#[tokio::test]
async fn parses_a_well_formed_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": { "results": [
                { "title": "Rust", "url": "https://rust-lang.org", "description": "A language" },
                { "title": "Docs", "url": "https://docs.rs" }
            ]}
        })))
        .mount(&server)
        .await;

    let hits = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .expect("parses");

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].title, "Rust");
    assert_eq!(hits[0].url, "https://rust-lang.org");
    assert_eq!(hits[0].snippet.as_deref(), Some("A language"));
    assert_eq!(hits[1].snippet, None, "a missing description is not fatal");
}

#[tokio::test]
async fn sends_the_subscription_token_and_query_parameters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(header("x-subscription-token", "test-key"))
        .and(query_param("q", "rust lang"))
        .and(query_param("count", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "web": { "results": [] }})))
        .expect(1)
        .mount(&server)
        .await;

    backend(&server)
        .search("rust lang", &DomainFilter::default(), 5)
        .await
        .expect("request matched the expected shape");
}

#[tokio::test]
async fn a_response_without_results_yields_an_empty_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "query": "rust" })))
        .mount(&server)
        .await;

    let hits = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .expect("a shape change must not be an error");
    assert!(hits.is_empty());
}

#[tokio::test]
async fn rows_missing_a_title_or_url_are_skipped_rather_than_faked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": { "results": [
                { "title": "No url" },
                { "url": "https://no-title.example" },
                { "title": "Good", "url": "https://good.example" }
            ]}
        })))
        .mount(&server)
        .await;

    let hits = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://good.example");
}

#[tokio::test]
async fn respects_the_result_limit_even_if_the_provider_overshoots() {
    let server = MockServer::start().await;
    let results: Vec<_> = (0..10)
        .map(|i| json!({ "title": format!("t{i}"), "url": format!("https://a.com/{i}") }))
        .collect();
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "web": { "results": results }})))
        .mount(&server)
        .await;

    let hits = backend(&server)
        .search("rust", &DomainFilter::default(), 3)
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
}

#[tokio::test]
async fn an_auth_failure_reports_the_status_without_echoing_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":\"bad token test-key\"}"))
        .mount(&server)
        .await;

    let error = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .expect_err("401 is a failure");
    assert!(error.contains("401"), "got: {error}");
    assert!(
        !error.contains("test-key"),
        "the provider's body may echo credentials and must not be forwarded, got: {error}"
    );
}

#[tokio::test]
async fn a_rate_limit_is_reported_with_its_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let error = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .unwrap_err();
    assert!(error.contains("429"), "got: {error}");
}

#[tokio::test]
async fn a_malformed_body_is_an_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{not json")
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let error = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .unwrap_err();
    assert!(error.contains("malformed"), "got: {error}");
}

#[tokio::test]
async fn a_stalled_provider_times_out_without_echoing_the_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;

    let backend = BraveBackend::new(
        test_client(Duration::from_millis(200)),
        "test-key".to_string(),
        format!("{}/search", server.uri()),
    );
    let error = backend
        .search("secret query", &DomainFilter::default(), 10)
        .await
        .unwrap_err();
    assert!(error.contains("timed out"), "got: {error}");
    assert!(
        !error.contains("secret query"),
        "transport errors must not carry the query string, got: {error}"
    );
}

// --- Backend identity and defaults ---

#[test]
fn reports_its_name_for_diagnostics() {
    let backend = BraveBackend::new(test_client(Duration::from_secs(5)), "k".into(), String::new());
    assert_eq!(backend.name(), "brave");
    assert!(
        !backend.supports_domain_filter(),
        "Brave has no domain parameter, so the tool must filter locally"
    );
}

#[tokio::test]
async fn an_empty_base_url_falls_back_to_the_public_endpoint() {
    // Configuring only an API key must yield a working backend, not a request
    // to an empty URL.
    let backend = BraveBackend::new(test_client(Duration::from_millis(1)), "k".into(), String::new());

    let error = backend
        .search("rust", &DomainFilter::default(), 1)
        .await
        .expect_err("no network in tests");
    assert!(
        !error.contains("relative URL") && !error.contains("builder"),
        "an empty base_url must not produce a malformed request: {error}"
    );
}

// --- Transport error classification ---

#[tokio::test]
async fn a_connection_failure_is_described_without_the_endpoint() {
    // Port 1 on loopback refuses immediately, giving a deterministic connect error.
    let backend = BraveBackend::new(
        test_client(Duration::from_secs(5)),
        "secret-key".into(),
        "http://127.0.0.1:1/search".into(),
    );

    let error = backend
        .search("private query", &DomainFilter::default(), 1)
        .await
        .expect_err("connection refused");

    assert!(
        error.contains("could not connect") || error.contains("connection failure"),
        "got: {error}"
    );
    assert!(!error.contains("secret-key"), "credentials leaked: {error}");
    assert!(!error.contains("private query"), "query leaked: {error}");
}
