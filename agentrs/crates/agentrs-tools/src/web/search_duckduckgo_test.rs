use std::time::Duration;

use reqwest::Client;
use wiremock::matchers::{header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{DuckDuckGoBackend, parse_results, resolve_result_url};
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

/// A result page in DuckDuckGo's markup: redirector-wrapped hrefs, `<b>`
/// highlight tags inside titles and snippets, HTML entities, and a sponsored
/// block that must not be reported as an organic result.
const RESULT_PAGE: &str = r##"<!DOCTYPE html><html><body>
<div class="results">
  <div class="result results_links result--ad">
    <div class="links_main">
      <h2 class="result__title">
        <a rel="nofollow" class="result__a" href="//duckduckgo.com/y.js?ad_provider=x">Buy Rust Courses</a>
      </h2>
      <a class="result__snippet" href="//duckduckgo.com/y.js">Sponsored offer</a>
    </div>
  </div>

  <div class="result results_links results_links_deep web-result ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title">
        <a rel="nofollow" class="result__a"
           href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust%2Dlang.org%2F&amp;rut=deadbeef">Rust Programming Language</a>
      </h2>
      <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust%2Dlang.org%2F">A language
         empowering <b>everyone</b> to build reliable &amp; efficient software.</a>
    </div>
  </div>

  <div class="result results_links results_links_deep web-result ">
    <div class="links_main links_deep result__body">
      <h2 class="result__title">
        <a rel="nofollow" class="result__a" href="https://doc.rust-lang.org/book/">The <b>Rust</b> Book</a>
      </h2>
      <a class="result__snippet" href="https://doc.rust-lang.org/book/">Official book.</a>
    </div>
  </div>
</div>
</body></html>"##;

const EMPTY_PAGE: &str = r#"<!DOCTYPE html><html><body>
<div class="no-results">No results found for <b>zxqwv</b>.</div>
</body></html>"#;

// --- Parsing ---

#[test]
fn extracts_title_url_and_snippet_from_a_result_page() {
    let hits = parse_results(RESULT_PAGE, 10).expect("page parses");

    assert_eq!(hits.len(), 2, "the sponsored block must not be counted: {hits:?}");
    assert_eq!(hits[0].title, "Rust Programming Language");
    assert_eq!(hits[0].url, "https://www.rust-lang.org/");
    assert_eq!(
        hits[0].snippet.as_deref(),
        Some("A language empowering everyone to build reliable & efficient software."),
        "highlight tags are dropped, entities decoded, whitespace collapsed"
    );
}

#[test]
fn a_direct_href_is_used_as_is() {
    let hits = parse_results(RESULT_PAGE, 10).unwrap();

    assert_eq!(hits[1].url, "https://doc.rust-lang.org/book/");
    assert_eq!(
        hits[1].title, "The Rust Book",
        "highlight markup must not reach the title"
    );
}

#[test]
fn sponsored_results_are_excluded() {
    let hits = parse_results(RESULT_PAGE, 10).unwrap();

    assert!(
        !hits.iter().any(|hit| hit.title.contains("Buy Rust Courses")),
        "ads are not search results: {hits:?}"
    );
    assert!(!hits.iter().any(|hit| hit.snippet.as_deref() == Some("Sponsored offer")));
}

#[test]
fn respects_the_result_limit() {
    assert_eq!(parse_results(RESULT_PAGE, 1).unwrap().len(), 1);
}

#[test]
fn a_page_that_declares_no_results_is_an_empty_success() {
    assert_eq!(parse_results(EMPTY_PAGE, 10).unwrap(), Vec::new());
}

// This is the property that makes a scraper survivable: an unreadable page must
// not masquerade as "the web has nothing", which the model would report as fact.
#[test]
fn an_unparseable_page_is_an_error_not_an_empty_result_set() {
    let changed_markup = r#"<html><body><div class="serp__results">
        <article><a class="c-result__link" href="https://example.com">Something</a></article>
    </div></body></html>"#;

    let error = parse_results(changed_markup, 10).expect_err("silence here would be a lie");
    assert!(error.contains("could not parse"), "got: {error}");
    assert!(
        error.contains("brave") && error.contains("searxng"),
        "the user needs a stable way out: {error}"
    );
}

#[test]
fn a_bot_challenge_page_is_reported_as_a_failure() {
    let challenge = r#"<html><body><h1>Please verify you are human</h1></body></html>"#;

    assert!(parse_results(challenge, 10).is_err());
}

#[test]
fn results_missing_a_usable_href_are_skipped() {
    let page = r#"<html><body><div class="result">
        <a class="result__a" href="javascript:void(0)">Bad</a>
        <a class="result__a" href="https://good.example/">Good</a>
    </div></body></html>"#;

    let hits = parse_results(page, 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].url, "https://good.example/");
}

// --- Redirector unwrapping ---

#[test]
fn unwraps_the_duckduckgo_redirector() {
    let resolved = resolve_result_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fserde&rut=abc");

    assert_eq!(
        resolved.as_deref(),
        Some("https://docs.rs/serde"),
        "citing the redirector instead of the page would make every source point at the search engine"
    );
}

#[test]
fn passes_through_a_plain_absolute_url() {
    assert_eq!(
        resolve_result_url("https://example.com/a?b=c").as_deref(),
        Some("https://example.com/a?b=c")
    );
}

#[test]
fn rejects_non_http_and_malformed_hrefs() {
    assert_eq!(resolve_result_url("javascript:alert(1)"), None);
    assert_eq!(resolve_result_url("/relative/path"), None);
    assert_eq!(resolve_result_url(""), None);
}

// --- Transport ---

#[tokio::test]
async fn sends_the_query_and_a_browser_user_agent() {
    // The endpoint serves a stripped page to clients it does not recognise.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/"))
        .and(query_param("q", "rust lang"))
        .and(header_regex("user-agent", "Mozilla"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(RESULT_PAGE.as_bytes().to_vec(), "text/html"))
        .expect(1)
        .mount(&server)
        .await;

    let backend = DuckDuckGoBackend::new(test_client(Duration::from_secs(5)), format!("{}/html/", server.uri()));
    let hits = backend
        .search("rust lang", &DomainFilter::default(), 10)
        .await
        .expect("request matched the expected shape");

    assert_eq!(hits.len(), 2);
}

#[tokio::test]
async fn reports_its_name_and_that_it_cannot_filter_server_side() {
    let backend = DuckDuckGoBackend::new(test_client(Duration::from_secs(5)), String::new());

    assert_eq!(backend.name(), "duckduckgo");
    assert!(
        !backend.supports_domain_filter(),
        "the HTML endpoint takes no domain parameter, so the tool must filter locally"
    );
}

#[tokio::test]
async fn a_rate_limit_status_is_surfaced() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/html/"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let backend = DuckDuckGoBackend::new(test_client(Duration::from_secs(5)), format!("{}/html/", server.uri()));
    let error = backend.search("rust", &DomainFilter::default(), 10).await.unwrap_err();

    assert!(error.contains("429"), "got: {error}");
}

#[tokio::test]
async fn an_empty_base_url_falls_back_to_the_public_endpoint() {
    let backend = DuckDuckGoBackend::new(test_client(Duration::from_millis(1)), String::new());

    let error = backend
        .search("rust", &DomainFilter::default(), 1)
        .await
        .expect_err("no network in tests");
    assert!(
        !error.contains("relative URL") && !error.contains("builder"),
        "an empty base_url must not produce a malformed request: {error}"
    );
}
