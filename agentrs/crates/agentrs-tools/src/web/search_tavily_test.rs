use std::time::Duration;

use reqwest::Client;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::TavilyBackend;
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

fn backend(server: &MockServer) -> TavilyBackend {
    TavilyBackend::new(
        test_client(Duration::from_secs(5)),
        "test-key".to_string(),
        format!("{}/search", server.uri()),
    )
}

#[tokio::test]
async fn parses_a_well_formed_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "title": "Rust", "url": "https://rust-lang.org", "content": "A language" }
            ]
        })))
        .mount(&server)
        .await;

    let hits = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .expect("parses");

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://rust-lang.org");
    assert_eq!(hits[0].snippet.as_deref(), Some("A language"));
}

// The whole point of `supports_domain_filter` is that these lists reach the
// provider instead of being applied after the fact.
#[tokio::test]
async fn pushes_the_domain_filter_down_to_the_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(json!({
            "query": "rust",
            "max_results": 7,
            "include_domains": ["ok.com"],
            "exclude_domains": [],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let filter = DomainFilter {
        allowed: vec!["ok.com".into()],
        blocked: vec![],
    };
    backend(&server)
        .search("rust", &filter, 7)
        .await
        .expect("request body matched the expected shape");
}

#[tokio::test]
async fn declares_that_it_filters_server_side() {
    let server = MockServer::start().await;
    assert!(backend(&server).supports_domain_filter());
}

#[tokio::test]
async fn a_response_without_results_yields_an_empty_list() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "answer": "none" })))
        .mount(&server)
        .await;

    assert!(
        backend(&server)
            .search("rust", &DomainFilter::default(), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_error_status_is_reported_without_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(403).set_body_string("key test-key rejected"))
        .mount(&server)
        .await;

    let error = backend(&server)
        .search("rust", &DomainFilter::default(), 10)
        .await
        .unwrap_err();
    assert!(error.contains("403"), "got: {error}");
    assert!(!error.contains("test-key"), "got: {error}");
}

#[tokio::test]
async fn a_malformed_body_is_an_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("[[[")
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
