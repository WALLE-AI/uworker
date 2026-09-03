use super::{SearchBackendKind, WebConfig, WebSearchConfig};

// --- TC-0.5-01: documented defaults ---
#[test]
fn defaults_match_the_documented_limits() {
    let config = WebConfig::default();

    assert!(config.enabled);
    assert_eq!(config.timeout_secs, 60);
    assert_eq!(config.max_content_bytes, 10 * 1024 * 1024);
    assert_eq!(config.max_redirects, 10);
    assert_eq!(config.max_url_length, 2000);
    assert_eq!(config.max_markdown_chars, 100_000);
    assert_eq!(config.cache_ttl_secs, 900);
    assert_eq!(config.cache_max_bytes, 50 * 1024 * 1024);
    assert_eq!(config.search.max_results, 10);
    assert_eq!(config.search.timeout_secs, 30);
    assert_eq!(config.search.backend, SearchBackendKind::None);
}

// --- TC-0.5-01: private-network access must be opt-in ---
#[test]
fn private_network_access_is_denied_by_default() {
    assert!(
        !WebConfig::default().allow_private_network,
        "defaulting this on would expose internal services to a fetched URL"
    );
}

// --- TC-0.5-02: an empty table deserializes to defaults ---
#[test]
fn empty_table_deserializes_to_defaults() {
    let parsed: WebConfig = toml::from_str("").expect("empty table is valid");
    assert_eq!(parsed, WebConfig::default());
}

// --- TC-0.5-03: unspecified fields keep their defaults ---
#[test]
fn partial_table_keeps_defaults_for_unspecified_fields() {
    let parsed: WebConfig = toml::from_str(
        r#"
        timeout_secs = 5
        deny_domains = ["evil.com"]

        [search]
        backend = "brave"
        "#,
    )
    .expect("partial table is valid");

    assert_eq!(parsed.timeout_secs, 5);
    assert_eq!(parsed.deny_domains, vec!["evil.com".to_string()]);
    assert_eq!(parsed.search.backend, SearchBackendKind::Brave);
    // Untouched fields fall back rather than zeroing out.
    assert_eq!(parsed.max_content_bytes, WebConfig::default().max_content_bytes);
    assert_eq!(parsed.search.max_results, WebSearchConfig::default().max_results);
    assert!(parsed.enabled);
}

#[test]
fn backend_kind_uses_snake_case_wire_names() {
    for (text, expected) in [
        ("none", SearchBackendKind::None),
        ("brave", SearchBackendKind::Brave),
        ("tavily", SearchBackendKind::Tavily),
        ("searxng", SearchBackendKind::Searxng),
    ] {
        let parsed: WebSearchConfig =
            toml::from_str(&format!("backend = \"{text}\"")).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(parsed.backend, expected);
    }
}

#[test]
fn unknown_backend_is_rejected_rather_than_silently_defaulted() {
    let parsed: Result<WebSearchConfig, _> = toml::from_str("backend = \"google\"");
    assert!(
        parsed.is_err(),
        "a typo'd backend must fail loudly, not silently disable search"
    );
}

#[test]
fn effective_user_agent_falls_back_when_blank() {
    let mut config = WebConfig::default();
    assert!(config.effective_user_agent().starts_with("agentrs/"));
    // Site operators need something to identify and block; a bare product
    // token gives them nothing to act on.
    assert!(
        config.effective_user_agent().contains("+https://"),
        "the default agent must carry a contact URL: {}",
        config.effective_user_agent()
    );

    config.user_agent = "   ".to_string();
    assert!(
        config.effective_user_agent().starts_with("agentrs/"),
        "whitespace-only must be treated as unset, not sent as a blank header"
    );

    config.user_agent = "custom-agent/1.0".to_string();
    assert_eq!(config.effective_user_agent(), "custom-agent/1.0");
}
