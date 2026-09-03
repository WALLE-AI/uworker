use std::time::Duration;

use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use agentrs_config::web::WebConfig;

use super::{FetchError, FetchOutcome, HttpClient};
use crate::web::url_policy::UrlPolicy;

/// Mock servers bind to 127.0.0.1, which the host policy refuses by design, so
/// tests opt into private-network access explicitly.
fn test_config() -> WebConfig {
    WebConfig {
        allow_private_network: true,
        timeout_secs: 5,
        ..WebConfig::default()
    }
}

fn client_with(config: WebConfig) -> HttpClient {
    let policy = UrlPolicy::new(&config);
    HttpClient::new(&config, policy).expect("client builds")
}

fn client() -> HttpClient {
    client_with(test_config())
}

async fn get(client: &HttpClient, url: &str) -> Result<FetchOutcome, FetchError> {
    client.fetch(url, &CancellationToken::new()).await
}

#[tokio::test]
async fn fetches_a_successful_response_with_its_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/page"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<h1>hi</h1>".to_vec(), "text/html"))
        .mount(&server)
        .await;

    let outcome = get(&client(), &format!("{}/page", server.uri())).await.unwrap();
    let FetchOutcome::Page(page) = outcome else {
        panic!("expected a page");
    };
    assert_eq!(page.status, 200);
    assert_eq!(page.status_text, "OK");
    assert!(page.content_type.contains("text/html"));
    assert_eq!(page.body, b"<h1>hi</h1>");
}

#[tokio::test]
async fn sends_the_configured_user_agent_and_accept_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/h"))
        .and(wiremock::matchers::header("user-agent", "custom-agent/1.0"))
        .and(wiremock::matchers::header_regex("accept", "text/markdown"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .expect(1)
        .mount(&server)
        .await;

    let config = WebConfig {
        user_agent: "custom-agent/1.0".into(),
        ..test_config()
    };
    get(&client_with(config), &format!("{}/h", server.uri()))
        .await
        .expect("headers matched");
}

// --- Redirects ---

#[tokio::test]
async fn follows_a_same_host_redirect() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/from"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/to"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/to"))
        .respond_with(ResponseTemplate::new(200).set_body_string("arrived"))
        .mount(&server)
        .await;

    let FetchOutcome::Page(page) = get(&client(), &format!("{}/from", server.uri())).await.unwrap() else {
        panic!("expected a page");
    };
    assert_eq!(page.body, b"arrived");
    assert_eq!(page.final_url.path(), "/to");
}

#[tokio::test]
async fn reports_a_cross_host_redirect_instead_of_following_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "https://elsewhere.example/final"))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = get(&client(), &format!("{}/start", server.uri())).await.unwrap();
    let FetchOutcome::Redirect(redirect) = outcome else {
        panic!("cross-host redirect must be reported, not followed");
    };
    assert_eq!(redirect.status, 302);
    assert_eq!(redirect.target.as_str(), "https://elsewhere.example/final");
}

#[tokio::test]
async fn stops_a_same_host_redirect_loop_at_the_configured_limit() {
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
        max_redirects: 3,
        ..test_config()
    };
    let error = get(&client_with(config), &format!("{}/a", server.uri()))
        .await
        .expect_err("a loop must terminate");
    assert!(
        matches!(error, FetchError::TooManyRedirects { limit: 3 }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn redirect_without_a_location_header_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/x"))
        .respond_with(ResponseTemplate::new(301))
        .mount(&server)
        .await;

    let error = get(&client(), &format!("{}/x", server.uri())).await.unwrap_err();
    assert!(matches!(error, FetchError::MissingLocationHeader), "got {error:?}");
    assert!(error.to_string().contains("Redirect missing Location header"));
}

// --- Limits ---

#[tokio::test]
async fn rejects_a_body_larger_than_the_limit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(5000)))
        .mount(&server)
        .await;

    let config = WebConfig {
        max_content_bytes: 1000,
        ..test_config()
    };
    let error = get(&client_with(config), &format!("{}/big", server.uri()))
        .await
        .unwrap_err();
    assert!(
        matches!(error, FetchError::ContentTooLarge { limit: 1000 }),
        "got {error:?}"
    );
}

#[tokio::test]
async fn reports_a_timeout_when_the_server_stalls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;

    let config = WebConfig {
        timeout_secs: 1,
        ..test_config()
    };
    let error = get(&client_with(config), &format!("{}/slow", server.uri()))
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Timeout { seconds: 1 }), "got {error:?}");
}

#[tokio::test]
async fn surfaces_error_status_codes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let error = get(&client(), &format!("{}/missing", server.uri())).await.unwrap_err();
    assert!(
        matches!(error, FetchError::HttpStatus { status: 404, .. }),
        "got {error:?}"
    );
    assert!(error.to_string().contains("404"));
}

// --- Cancellation ---

#[tokio::test]
async fn an_already_cancelled_token_short_circuits_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/never"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = client()
        .fetch(&format!("{}/never", server.uri()), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Cancelled), "got {error:?}");
}

#[tokio::test]
async fn cancelling_mid_flight_returns_promptly_rather_than_waiting_out_the_timeout() {
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
    let client = client_with(config);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });

    let started = std::time::Instant::now();
    let error = client
        .fetch(&format!("{}/slow", server.uri()), &cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Cancelled), "got {error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancel must not wait out the 30s request timeout, took {:?}",
        started.elapsed()
    );
}

// --- Policy integration ---

#[tokio::test]
async fn a_policy_rejection_never_reaches_the_network() {
    let error = client_with(WebConfig::default())
        .fetch("https://169.254.169.254/latest/meta-data/", &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::Rejected(_)), "got {error:?}");
}
