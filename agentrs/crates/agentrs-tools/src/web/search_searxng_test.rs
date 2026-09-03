use std::time::Duration;

use reqwest::Client;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::SearxngBackend;
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

fn backend_with_base(base_url: String) -> SearxngBackend {
    SearxngBackend::new(test_client(Duration::from_secs(5)), base_url)
}

#[tokio::test]
async fn parses_a_well_formed_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .and(query_param("format", "json"))
        .and(query_param("q", "rust"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "title": "Rust", "url": "https://rust-lang.org", "content": "A language" }
            ]
        })))
        .mount(&server)
        .await;

    let hits = backend_with_base(server.uri())
        .search("rust", &DomainFilter::default(), 10)
        .await
        .expect("parses");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Rust");
    assert_eq!(hits[0].snippet.as_deref(), Some("A language"));
}

// A configured base URL with a trailing slash must not produce `//search`,
// which many reverse proxies reject.
#[tokio::test]
async fn a_trailing_slash_in_the_base_url_is_normalized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": [] })))
        .expect(1)
        .mount(&server)
        .await;

    backend_with_base(format!("{}/", server.uri()))
        .search("rust", &DomainFilter::default(), 10)
        .await
        .expect("path was built correctly");
}

#[tokio::test]
async fn declares_that_it_does_not_filter_server_side() {
    assert!(
        !backend_with_base("https://searx.example".into()).supports_domain_filter(),
        "SearXNG has no domain parameter, so the tool must filter locally"
    );
}

#[tokio::test]
async fn an_error_status_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&server)
        .await;

    let error = backend_with_base(server.uri())
        .search("rust", &DomainFilter::default(), 10)
        .await
        .unwrap_err();
    assert!(error.contains("502"), "got: {error}");
}

#[tokio::test]
async fn a_malformed_body_is_an_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<html>not json</html>")
                .insert_header("content-type", "application/json"),
        )
        .mount(&server)
        .await;

    let error = backend_with_base(server.uri())
        .search("rust", &DomainFilter::default(), 10)
        .await
        .unwrap_err();
    assert!(error.contains("malformed"), "got: {error}");
}
