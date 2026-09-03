//! Integration tests for the WebFetch tool (TC-1.5-01 through TC-1.5-14).
//!
//! Black-box: everything goes through the public `Tool` surface against a mock
//! HTTP server. Written from the spec in `agentrs-Web工具移植执行方案.md`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, header_regex, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use agentrs_config::web::WebConfig;
use agentrs_tools::Tool;
use agentrs_tools::web::fetch_tool::WebFetchTool;
use agentrs_types::summarizer::TextSummarizer;

/// Echoes the page text back so assertions can inspect what the model would
/// have received, and counts calls.
struct EchoSummarizer {
    calls: AtomicUsize,
}

impl EchoSummarizer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl TextSummarizer for EchoSummarizer {
    async fn summarize(&self, _instruction: &str, content: &str) -> Result<String, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(content.to_string())
    }
}

/// Mock servers bind to loopback, which the host policy refuses by default and
/// an ambient proxy would otherwise intercept.
fn test_config() -> WebConfig {
    WebConfig {
        allow_private_network: true,
        timeout_secs: 5,
        ..WebConfig::default()
    }
}

fn tool(config: WebConfig) -> (WebFetchTool, TempDir) {
    let dir = TempDir::new().expect("temp dir");
    let tool = WebFetchTool::new(&config, dir.path().to_path_buf(), Some(EchoSummarizer::new())).expect("tool builds");
    (tool, dir)
}

async fn fetch(tool: &WebFetchTool, url: String) -> agentrs_types::tool::ToolResult {
    tool.execute(json!({ "url": url, "prompt": "summarize" })).await
}

// --- TC-1.5-01 / TC-1.5-02: content types ---

#[tokio::test]
async fn html_is_returned_as_markdown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"<h1>Title</h1><p>Some body text.</p>".to_vec(), "text/html"),
        )
        .mount(&server)
        .await;

    let (tool, _dir) = tool(test_config());
    let result = fetch(&tool, format!("{}/page", server.uri())).await;

    assert!(!result.is_error, "got: {}", result.content);
    assert!(result.content.contains("# Title"), "got: {}", result.content);
    assert!(result.content.contains("Some body text."), "got: {}", result.content);
    assert!(!result.content.contains("<h1>"), "raw tags leaked: {}", result.content);
}

#[tokio::test]
async fn plain_text_is_returned_unchanged() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/raw"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"line one\nline two".to_vec(), "text/plain"))
        .mount(&server)
        .await;

    let (tool, _dir) = tool(test_config());
    let result = fetch(&tool, format!("{}/raw", server.uri())).await;

    assert!(!result.is_error);
    assert!(result.content.contains("line one\nline two"), "got: {}", result.content);
}

// --- TC-1.5-03 through TC-1.5-07: redirects ---

#[tokio::test]
async fn same_host_redirects_are_followed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/one"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/two"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/two"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"final page".to_vec(), "text/plain"))
        .mount(&server)
        .await;

    let (tool, _dir) = tool(test_config());
    let result = fetch(&tool, format!("{}/one", server.uri())).await;

    assert!(!result.is_error, "got: {}", result.content);
    assert!(result.content.contains("final page"));
}

#[tokio::test]
async fn cross_host_redirects_are_reported_with_retry_instructions() {
    for (status, expected_text) in [
        (301, "Moved Permanently"),
        (302, "Found"),
        (307, "Temporary Redirect"),
        (308, "Permanent Redirect"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/go"))
            .respond_with(ResponseTemplate::new(status).insert_header("location", "https://other.example/dest"))
            .expect(1)
            .mount(&server)
            .await;

        let (tool, _dir) = tool(test_config());
        let result = fetch(&tool, format!("{}/go", server.uri())).await;

        assert!(
            !result.is_error,
            "a reported redirect is not a failure: {}",
            result.content
        );
        assert!(result.content.contains("REDIRECT DETECTED"), "got: {}", result.content);
        assert!(result.content.contains("https://other.example/dest"));
        assert!(
            result.content.contains(&format!("{status} {expected_text}")),
            "status line wrong for {status}: {}",
            result.content
        );
        assert!(
            result.content.contains("use WebFetch again"),
            "the model needs an explicit retry instruction: {}",
            result.content
        );
    }
}

#[tokio::test]
async fn a_same_host_redirect_loop_terminates_instead_of_hanging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/a"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/b"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/b"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/a"))
        .mount(&server)
        .await;

    let config = WebConfig {
        max_redirects: 4,
        ..test_config()
    };
    let (tool, _dir) = tool(config);
    let result = fetch(&tool, format!("{}/a", server.uri())).await;

    assert!(result.is_error);
    assert!(result.content.contains("Too many redirects"), "got: {}", result.content);
}

#[tokio::test]
async fn a_redirect_without_a_location_header_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(302))
        .mount(&server)
        .await;

    let (tool, _dir) = tool(test_config());
    let result = fetch(&tool, format!("{}/x", server.uri())).await;

    assert!(result.is_error);
    assert!(
        result.content.contains("Redirect missing Location header"),
        "got: {}",
        result.content
    );
}

// --- TC-1.5-08 through TC-1.5-10: limits and failures ---

#[tokio::test]
async fn an_oversized_body_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![b'x'; 200_000], "text/plain"))
        .mount(&server)
        .await;

    let config = WebConfig {
        max_content_bytes: 4096,
        ..test_config()
    };
    let (tool, _dir) = tool(config);
    let result = fetch(&tool, format!("{}/big", server.uri())).await;

    assert!(result.is_error);
    assert!(result.content.contains("4096 byte limit"), "got: {}", result.content);
}

#[tokio::test]
async fn a_stalled_server_times_out_near_the_configured_bound() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    let config = WebConfig {
        timeout_secs: 1,
        ..test_config()
    };
    let (tool, _dir) = tool(config);
    let started = std::time::Instant::now();
    let result = fetch(&tool, format!("{}/slow", server.uri())).await;

    assert!(result.is_error);
    assert!(result.content.contains("timed out"), "got: {}", result.content);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}, should be near the 1s bound",
        started.elapsed()
    );
}

#[tokio::test]
async fn error_statuses_are_surfaced_to_the_model() {
    for status in [404u16, 500] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/e"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;

        let (tool, _dir) = tool(test_config());
        let result = fetch(&tool, format!("{}/e", server.uri())).await;

        assert!(result.is_error, "status {status} should be an error");
        assert!(result.content.contains(&status.to_string()), "got: {}", result.content);
    }
}

// --- TC-1.5-11: binary payloads ---

#[tokio::test]
async fn binary_content_is_saved_and_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/report.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"%PDF-1.7 body".to_vec(), "application/pdf"))
        .mount(&server)
        .await;

    let (tool, dir) = tool(test_config());
    let result = fetch(&tool, format!("{}/report.pdf", server.uri())).await;

    assert!(!result.is_error, "got: {}", result.content);
    assert!(
        result.content.contains("[Binary content (application/pdf"),
        "got: {}",
        result.content
    );

    let saved: Vec<_> = std::fs::read_dir(dir.path())
        .expect("download dir exists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(saved.len(), 1, "exactly one file should be written: {saved:?}");
    assert_eq!(std::fs::read(&saved[0]).unwrap(), b"%PDF-1.7 body");
    assert!(
        result.content.contains(&saved[0].display().to_string()),
        "the reported path must be the real one"
    );
}

// --- TC-1.5-12: caching ---

#[tokio::test]
async fn a_second_identical_fetch_does_not_hit_the_network() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cached"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"payload".to_vec(), "text/plain"))
        .expect(1)
        .mount(&server)
        .await;

    let (tool, _dir) = tool(test_config());
    let url = format!("{}/cached", server.uri());
    let first = fetch(&tool, url.clone()).await;
    let second = fetch(&tool, url).await;

    assert!(!first.is_error);
    assert_eq!(first.content, second.content);
    // The `.expect(1)` above panics on drop if the server was hit twice.
}

// --- TC-1.5-13: request headers ---

#[tokio::test]
async fn requests_carry_the_configured_user_agent_and_accept_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/h"))
        .and(header("user-agent", "agentrs-test/9.9"))
        .and(header_regex("accept", "text/markdown"))
        .and(header_regex("accept", "text/html"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"ok".to_vec(), "text/plain"))
        .expect(1)
        .mount(&server)
        .await;

    let config = WebConfig {
        user_agent: "agentrs-test/9.9".into(),
        ..test_config()
    };
    let (tool, _dir) = tool(config);
    let result = fetch(&tool, format!("{}/h", server.uri())).await;

    assert!(!result.is_error, "headers did not match: {}", result.content);
}

// --- TC-1.5-14: cancellation ---

#[tokio::test]
async fn cancelling_mid_flight_returns_without_waiting_out_the_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    let config = WebConfig {
        timeout_secs: 30,
        ..test_config()
    };
    let (tool, _dir) = tool(config);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });

    let started = std::time::Instant::now();
    let result = tool
        .execute_cancellable(
            json!({ "url": format!("{}/slow", server.uri()), "prompt": "x" }),
            cancel,
        )
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("cancelled"), "got: {}", result.content);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}; an interrupted turn must not wait out the request timeout",
        started.elapsed()
    );
}

// --- Host policy, end to end ---

#[tokio::test]
async fn private_destinations_are_refused_when_not_explicitly_allowed() {
    let (tool, _dir) = tool(WebConfig::default());

    for url in [
        "http://127.0.0.1:8080/x",
        "https://169.254.169.254/latest/meta-data/",
        "http://localhost/x",
        "https://[::1]/x",
    ] {
        let result = tool.execute(json!({ "url": url, "prompt": "x" })).await;
        assert!(result.is_error, "{url} must be refused");
        assert!(
            result.content.contains("Refused to fetch"),
            "{url} got: {}",
            result.content
        );
    }
}
