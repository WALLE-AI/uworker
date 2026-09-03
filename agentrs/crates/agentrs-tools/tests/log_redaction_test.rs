//! TC-3.2-01: the network tools must not log sensitive payloads.
//!
//! Fetched page bodies, search queries, and provider credentials all pass
//! through these tools. `AGENTS.md` forbids any of them reaching a
//! production-visible log, so this captures everything the tools emit at
//! `debug` and above and asserts the secrets are absent.

use std::io;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::TempDir;
use tracing::Level;
use tracing_subscriber::fmt::MakeWriter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use agentrs_config::web::{SearchBackendKind, WebConfig};
use agentrs_tools::Tool;
use agentrs_tools::web::build_backend;
use agentrs_tools::web::fetch_tool::WebFetchTool;
use agentrs_tools::web::search_tool::WebSearchTool;

/// Collects every formatted log line emitted while a subscriber is active.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("poisoned")).into_owned()
    }
}

impl io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("poisoned").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `body` with all tool logging captured, then return what was logged.
async fn capture_logs<F, Fut>(body: F) -> String
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .with_writer(logs.clone())
        .with_ansi(false)
        .finish();

    // Scoped rather than global: other tests in this binary must not inherit it.
    let guard = tracing::subscriber::set_default(subscriber);
    body().await;
    drop(guard);

    logs.text()
}

fn test_config() -> WebConfig {
    WebConfig {
        allow_private_network: true,
        timeout_secs: 5,
        ..WebConfig::default()
    }
}

// No underscores: the markdown converter escapes them, which would make the
// "did it reach the model" assertion brittle without changing what is logged.
const SECRET_BODY: &str = "PATIENTRECORD9be21f confidential-page-contents";

#[tokio::test]
async fn web_fetch_never_logs_the_page_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private-report"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(format!("<p>{SECRET_BODY}</p>").into_bytes(), "text/html"),
        )
        .mount(&server)
        .await;
    let url = format!("{}/private-report", server.uri());

    let dir = TempDir::new().expect("temp dir");
    let logged = capture_logs(|| async {
        let tool = WebFetchTool::new(&test_config(), dir.path().to_path_buf(), None).expect("builds");
        let result = tool.execute(json!({ "url": &url, "prompt": "summarize" })).await;
        assert!(!result.is_error, "fetch should succeed: {}", result.content);
        // The body must reach the model...
        assert!(result.content.contains(SECRET_BODY), "got: {}", result.content);
    })
    .await;

    // ...but never the logs.
    assert!(
        !logged.contains(SECRET_BODY),
        "page contents leaked into logs:\n{logged}"
    );
    assert!(
        !logged.contains("PATIENTRECORD"),
        "page contents leaked into logs:\n{logged}"
    );
}

#[tokio::test]
async fn web_fetch_does_not_log_the_body_of_an_oversized_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!("{SECRET_BODY}{}", "x".repeat(50_000)).into_bytes(),
            "text/plain",
        ))
        .mount(&server)
        .await;
    let url = format!("{}/big", server.uri());

    let dir = TempDir::new().expect("temp dir");
    let logged = capture_logs(|| async {
        let config = WebConfig {
            max_content_bytes: 512,
            ..test_config()
        };
        let tool = WebFetchTool::new(&config, dir.path().to_path_buf(), None).expect("builds");
        let result = tool.execute(json!({ "url": &url, "prompt": "x" })).await;
        assert!(result.is_error, "oversized bodies are refused");
    })
    .await;

    assert!(
        !logged.contains("PATIENTRECORD"),
        "a rejected body must not be logged either:\n{logged}"
    );
}

#[tokio::test]
async fn web_search_never_logs_the_api_key_or_the_query() {
    const SECRET_KEY: &str = "sk-brave-3f9a2c-do-not-log";
    const SECRET_QUERY: &str = "acquisition target codename bluejay";

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!("bad token {SECRET_KEY}")))
        .mount(&server)
        .await;

    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Brave;
    config.search.api_key_env = "AGENTRS_REDACTION_TEST_KEY".to_string();
    config.search.base_url = format!("{}/search", server.uri());
    config.search.timeout_secs = 5;
    unsafe {
        std::env::set_var("AGENTRS_REDACTION_TEST_KEY", SECRET_KEY);
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
        std::env::set_var("no_proxy", "127.0.0.1,localhost,::1");
    }

    let logged = capture_logs(|| async {
        let backend = build_backend(&config).expect("backend builds");
        let tool = WebSearchTool::new(backend, 10);
        let result = tool.execute(json!({ "query": SECRET_QUERY })).await;
        assert!(result.is_error);
        // The provider's body may echo the key; it must not be forwarded either.
        assert!(!result.content.contains(SECRET_KEY), "got: {}", result.content);
    })
    .await;

    assert!(!logged.contains(SECRET_KEY), "API key leaked into logs:\n{logged}");
    assert!(
        !logged.contains(SECRET_QUERY),
        "search query leaked into logs:\n{logged}"
    );

    unsafe { std::env::remove_var("AGENTRS_REDACTION_TEST_KEY") };
}

#[tokio::test]
async fn a_refused_host_is_named_in_logs_but_carries_no_payload() {
    let dir = TempDir::new().expect("temp dir");
    let logged = capture_logs(|| async {
        let tool = WebFetchTool::new(&WebConfig::default(), dir.path().to_path_buf(), None).expect("builds");
        let result = tool
            .execute(json!({
                "url": "https://169.254.169.254/latest/meta-data/iam/security-credentials/",
                "prompt": "dump",
            }))
            .await;
        assert!(result.is_error);
    })
    .await;

    assert!(
        !logged.contains("security-credentials"),
        "the refused URL's path must not be logged:\n{logged}"
    );
}
