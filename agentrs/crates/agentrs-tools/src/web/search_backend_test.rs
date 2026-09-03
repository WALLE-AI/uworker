use agentrs_config::web::{SearchBackendKind, WebConfig};

use super::{BackendError, DomainFilter, SearchHit, build_backend};

fn hit(url: &str) -> SearchHit {
    SearchHit {
        title: "t".into(),
        url: url.into(),
        snippet: None,
    }
}

// --- DomainFilter ---

#[test]
fn an_empty_filter_keeps_everything() {
    let filter = DomainFilter::default();
    assert!(filter.is_empty());
    let hits = vec![hit("https://a.com/1"), hit("https://b.com/1")];
    assert_eq!(filter.retain(hits.clone()), hits);
}

#[test]
fn block_list_drops_the_host_and_its_subdomains_only() {
    let filter = DomainFilter {
        allowed: vec![],
        blocked: vec!["bad.com".into()],
    };
    let kept = filter.retain(vec![
        hit("https://bad.com/1"),
        hit("https://sub.bad.com/1"),
        hit("https://notbad.com/1"),
    ]);
    assert_eq!(kept, vec![hit("https://notbad.com/1")]);
}

#[test]
fn allow_list_keeps_the_host_and_its_subdomains_only() {
    let filter = DomainFilter {
        allowed: vec!["ok.com".into()],
        blocked: vec![],
    };
    let kept = filter.retain(vec![
        hit("https://ok.com/1"),
        hit("https://api.ok.com/1"),
        hit("https://notok.com/1"),
    ]);
    assert_eq!(kept, vec![hit("https://ok.com/1"), hit("https://api.ok.com/1")]);
}

#[test]
fn an_unparseable_result_url_fails_closed_under_an_allow_list() {
    let allow = DomainFilter {
        allowed: vec!["ok.com".into()],
        blocked: vec![],
    };
    assert!(
        allow.retain(vec![hit("not a url")]).is_empty(),
        "a URL that cannot be shown to be allowed must be dropped"
    );

    let block = DomainFilter {
        allowed: vec![],
        blocked: vec!["bad.com".into()],
    };
    assert_eq!(
        block.retain(vec![hit("not a url")]).len(),
        1,
        "but it cannot be shown to be blocked either, so a block list keeps it"
    );
}

#[test]
fn filter_rules_are_case_insensitive_and_tolerate_leading_dots() {
    let filter = DomainFilter {
        allowed: vec![],
        blocked: vec![".BAD.com".into()],
    };
    assert!(filter.retain(vec![hit("https://SUB.Bad.COM/1")]).is_empty());
}

// --- TC-2.3-01 through TC-2.3-04: backend construction ---

#[test]
fn the_none_backend_reports_search_as_disabled() {
    let config = WebConfig::default();
    assert_eq!(config.search.backend, SearchBackendKind::None);
    assert!(matches!(build_backend(&config), Err(BackendError::Disabled)));
}

#[test]
fn a_key_backed_backend_without_its_key_is_misconfigured() {
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Brave;
    config.search.api_key_env = "AGENTRS_TEST_UNSET_SEARCH_KEY".into();
    unsafe { std::env::remove_var("AGENTRS_TEST_UNSET_SEARCH_KEY") };

    match build_backend(&config).map(|_| ()) {
        Err(BackendError::Misconfigured(message)) => {
            assert!(message.contains("AGENTRS_TEST_UNSET_SEARCH_KEY"), "got: {message}");
        }
        other => panic!("expected a misconfiguration, got {other:?}"),
    }
}

#[test]
fn a_key_backed_backend_builds_once_its_key_is_present() {
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Tavily;
    config.search.api_key_env = "AGENTRS_TEST_SEARCH_KEY_PRESENT".into();
    unsafe { std::env::set_var("AGENTRS_TEST_SEARCH_KEY_PRESENT", "secret") };

    let backend = build_backend(&config).expect("builds with a key");
    assert_eq!(backend.name(), "tavily");
    assert!(backend.supports_domain_filter(), "tavily filters server-side");

    unsafe { std::env::remove_var("AGENTRS_TEST_SEARCH_KEY_PRESENT") };
}

#[test]
fn searxng_requires_a_base_url_but_no_key() {
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Searxng;

    assert!(matches!(build_backend(&config), Err(BackendError::Misconfigured(_))));

    config.search.base_url = "https://searx.example".into();
    let backend = build_backend(&config).expect("builds without an API key");
    assert_eq!(backend.name(), "searxng");
    assert!(!backend.supports_domain_filter(), "searxng filters locally");
}

#[test]
fn an_empty_api_key_variable_name_is_misconfigured() {
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Brave;
    config.search.api_key_env = "  ".into();

    assert!(matches!(build_backend(&config), Err(BackendError::Misconfigured(_))));
}

#[test]
fn the_keyless_backend_builds_without_credentials_or_a_base_url() {
    let mut config = WebConfig::default();
    config.search.backend = SearchBackendKind::Duckduckgo;

    let backend = build_backend(&config).expect("no key and no base_url required");
    assert_eq!(backend.name(), "duckduckgo");
    assert!(!backend.supports_domain_filter());
}
