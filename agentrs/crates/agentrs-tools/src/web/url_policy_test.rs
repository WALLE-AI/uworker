use agentrs_config::web::WebConfig;
use url::Url;

use super::{RejectReason, UrlPolicy};

fn policy() -> UrlPolicy {
    UrlPolicy::new(&WebConfig::default())
}

fn policy_with(mutate: impl FnOnce(&mut WebConfig)) -> UrlPolicy {
    let mut config = WebConfig::default();
    mutate(&mut config);
    UrlPolicy::new(&config)
}

fn assert_rejected(url: &str) -> RejectReason {
    policy().check(url).expect_err(&format!("{url} must be rejected"))
}

// --- TC-1.1-01 / TC-1.1-02: accepted public URLs, with http upgraded ---

#[test]
fn accepts_public_https_url() {
    let accepted = policy().check("https://example.com/a").expect("public https");
    assert_eq!(accepted.as_str(), "https://example.com/a");
}

#[test]
fn upgrades_http_to_https() {
    let accepted = policy().check("http://example.com/a").expect("public http");
    assert_eq!(
        accepted.scheme(),
        "https",
        "plain http must be promoted, not sent in the clear"
    );
    assert_eq!(accepted.host_str(), Some("example.com"));
    assert_eq!(accepted.path(), "/a");
}

// --- TC-1.1-03 / TC-1.1-04: length limit and its boundary ---

#[test]
fn rejects_url_over_length_limit() {
    let long = format!("https://example.com/{}", "a".repeat(2001));
    assert!(matches!(policy().check(&long), Err(RejectReason::UrlTooLong { .. })));
}

#[test]
fn accepts_url_at_exactly_the_length_limit() {
    let prefix = "https://example.com/";
    let url = format!("{prefix}{}", "a".repeat(2000 - prefix.len()));
    assert_eq!(url.len(), 2000);
    assert!(policy().check(&url).is_ok(), "2000 is inclusive");
}

// --- TC-1.1-05 / TC-1.1-06: embedded credentials ---

#[test]
fn rejects_url_with_username_and_password() {
    assert_eq!(
        assert_rejected("https://user:pw@example.com/"),
        RejectReason::CredentialsInUrl
    );
}

#[test]
fn rejects_url_with_username_only() {
    assert_eq!(
        assert_rejected("https://user@example.com/"),
        RejectReason::CredentialsInUrl
    );
}

// --- TC-1.1-07 through TC-1.1-10: non-public names ---

#[test]
fn rejects_localhost_and_local_suffixes() {
    for url in [
        "https://localhost/x",
        "https://api.localhost/x",
        "https://foo.local/x",
        "https://foo.internal/x",
        "https://box.localdomain/x",
    ] {
        assert!(
            matches!(assert_rejected(url), RejectReason::PrivateHost { .. }),
            "{url} must be refused as a private host"
        );
    }
}

#[test]
fn rejects_dotless_hostname() {
    assert!(matches!(
        assert_rejected("https://intranet/x"),
        RejectReason::NotPublicHostname { .. }
    ));
}

// --- TC-1.1-11 through TC-1.1-24: literal addresses ---

#[test]
fn rejects_private_and_loopback_ipv4_literals() {
    for url in [
        "https://127.0.0.1/x",
        "https://127.255.255.254/x",
        "https://10.0.0.1/x",
        "https://172.16.0.1/x",
        "https://172.31.255.254/x",
        "https://192.168.1.1/x",
        "https://0.0.0.0/x",
        "https://100.64.0.1/x",
    ] {
        assert!(
            matches!(assert_rejected(url), RejectReason::PrivateHost { .. }),
            "{url} must be refused"
        );
    }
}

// The cloud metadata endpoint is the highest-value SSRF target reachable from
// an agent that fetches attacker-supplied URLs.
#[test]
fn rejects_cloud_metadata_endpoint() {
    assert!(matches!(
        assert_rejected("https://169.254.169.254/latest/meta-data/"),
        RejectReason::PrivateHost { .. }
    ));
}

#[test]
fn accepts_addresses_just_outside_the_rfc1918_block() {
    for url in ["https://172.15.0.1/x", "https://172.32.0.1/x"] {
        assert!(
            policy().check(url).is_ok(),
            "{url} is public; an over-broad 172/8 rule would wrongly block it"
        );
    }
}

#[test]
fn rejects_private_ipv6_literals() {
    for url in [
        "https://[::1]/x",
        "https://[fc00::1]/x",
        "https://[fd00::1]/x",
        "https://[fe80::1]/x",
        "https://[::ffff:127.0.0.1]/x",
    ] {
        assert!(
            matches!(assert_rejected(url), RejectReason::PrivateHost { .. }),
            "{url} must be refused"
        );
    }
}

#[test]
fn accepts_public_ipv6_literal() {
    assert!(policy().check("https://[2606:4700::1111]/x").is_ok());
}

// Decimal and hex spellings of 127.0.0.1 must not slip through as opaque names.
#[test]
fn rejects_alternate_ipv4_spellings_of_loopback() {
    for url in ["https://2130706433/x", "https://0x7f000001/x", "https://0177.0.0.1/x"] {
        assert!(
            matches!(assert_rejected(url), RejectReason::PrivateHost { .. }),
            "{url} is 127.0.0.1 in disguise and must be refused"
        );
    }
}

// --- TC-1.1-26: the escape hatch ---

#[test]
fn allow_private_network_permits_loopback() {
    let policy = policy_with(|config| config.allow_private_network = true);
    assert!(policy.check("https://127.0.0.1/x").is_ok());
    assert!(policy.check("https://localhost/x").is_ok());
}

// A permitted loopback target usually has no certificate; upgrading it would
// turn a working local URL into a handshake failure.
#[test]
fn permitted_private_hosts_keep_their_http_scheme() {
    let policy = policy_with(|config| config.allow_private_network = true);
    for url in ["http://127.0.0.1:8080/x", "http://localhost:3000/x"] {
        let accepted = policy.check(url).unwrap_or_else(|e| panic!("{url}: {e}"));
        assert_eq!(accepted.scheme(), "http", "{url} must not be promoted");
    }
    // Public hosts are still promoted even with the escape hatch on.
    assert_eq!(policy.check("http://example.com/x").unwrap().scheme(), "https");
}

// --- TC-1.1-27 through TC-1.1-31: domain rules ---

#[test]
fn deny_domain_blocks_host_and_subdomains() {
    let policy = policy_with(|config| config.deny_domains = vec!["evil.com".into()]);
    assert!(matches!(
        policy.check("https://evil.com/x"),
        Err(RejectReason::DeniedDomain { .. })
    ));
    assert!(matches!(
        policy.check("https://sub.evil.com/x"),
        Err(RejectReason::DeniedDomain { .. })
    ));
}

#[test]
fn deny_domain_does_not_match_unrelated_suffix() {
    let policy = policy_with(|config| config.deny_domains = vec!["evil.com".into()]);
    assert!(
        policy.check("https://notevil.com/x").is_ok(),
        "suffix-only matching would wrongly block an unrelated domain"
    );
}

#[test]
fn allow_domains_acts_as_a_whitelist() {
    let policy = policy_with(|config| config.allow_domains = vec!["ok.com".into()]);
    assert!(policy.check("https://ok.com/x").is_ok());
    assert!(policy.check("https://api.ok.com/x").is_ok());
    assert!(matches!(
        policy.check("https://other.com/x"),
        Err(RejectReason::NotAllowedDomain { .. })
    ));
}

#[test]
fn deny_takes_precedence_over_allow() {
    let policy = policy_with(|config| {
        config.allow_domains = vec!["shared.com".into()];
        config.deny_domains = vec!["shared.com".into()];
    });
    assert!(matches!(
        policy.check("https://shared.com/x"),
        Err(RejectReason::DeniedDomain { .. })
    ));
}

#[test]
fn domain_rules_are_case_insensitive_and_tolerate_leading_dots() {
    let policy = policy_with(|config| config.deny_domains = vec![".EVIL.com".into()]);
    assert!(matches!(
        policy.check("https://SUB.Evil.COM/x"),
        Err(RejectReason::DeniedDomain { .. })
    ));
}

// --- TC-1.1-32 through TC-1.1-34: scheme and parse failures ---

#[test]
fn rejects_non_http_schemes() {
    for url in ["ftp://example.com/x", "file:///etc/passwd", "data:text/plain,hi"] {
        assert!(
            matches!(assert_rejected(url), RejectReason::UnsupportedScheme { .. }),
            "{url} must be refused"
        );
    }
}

#[test]
fn rejects_unparseable_input() {
    for url in ["", "not a url", "https://"] {
        assert!(policy().check(url).is_err(), "{url:?} must not be treated as fetchable");
    }
}

// --- TC-1.1-40 through TC-1.1-49: redirect adjudication ---

fn permits_redirect(from: &str, to: &str) -> bool {
    let (Ok(from), Ok(to)) = (Url::parse(from), Url::parse(to)) else {
        return false;
    };
    policy().is_permitted_redirect(&from, &to)
}

#[test]
fn permits_same_origin_path_change() {
    assert!(permits_redirect("https://a.com/1", "https://a.com/2"));
}

#[test]
fn permits_adding_and_removing_www() {
    assert!(permits_redirect("https://a.com/1", "https://www.a.com/1"));
    assert!(permits_redirect("https://www.a.com/1", "https://a.com/1"));
}

#[test]
fn denies_cross_host_redirect() {
    assert!(!permits_redirect("https://a.com/1", "https://b.com/1"));
}

#[test]
fn denies_scheme_downgrade() {
    assert!(!permits_redirect("https://a.com/1", "http://a.com/1"));
}

#[test]
fn denies_port_change() {
    assert!(!permits_redirect("https://a.com:443/1", "https://a.com:8443/1"));
}

#[test]
fn denies_redirect_target_carrying_credentials() {
    assert!(!permits_redirect("https://a.com/1", "https://u:p@a.com/2"));
}

#[test]
fn permits_relative_redirect_resolved_against_the_original() {
    let original = Url::parse("https://a.com/1").unwrap();
    let target = original.join("/relative/2").unwrap();
    assert_eq!(target.as_str(), "https://a.com/relative/2");
    assert!(policy().is_permitted_redirect(&original, &target));
}

// Stripping "www." must be anchored on the dot, or www-prefixed lookalikes pass.
#[test]
fn denies_www_lookalike_host() {
    assert!(!permits_redirect("https://a.com/1", "https://wwwa.com/1"));
}

#[test]
fn denies_malformed_redirect_target() {
    assert!(!permits_redirect("https://a.com/1", "not-a-url"));
}

// --- Preapproved hosts ---

// Preapproval sends raw remote text into the transcript unreviewed, so a bare
// rule names one exact host. An attacker-controlled subdomain of a trusted site
// must not inherit that trust.
#[test]
fn preapproved_defaults_to_exact_host_matching() {
    let policy = policy_with(|config| config.preapproved_domains = vec!["docs.rs".into()]);

    assert!(policy.is_preapproved(&Url::parse("https://docs.rs/x").unwrap()));
    assert!(
        !policy.is_preapproved(&Url::parse("https://a.docs.rs/x").unwrap()),
        "a subdomain must not be preapproved unless the rule opts in"
    );
    assert!(!policy.is_preapproved(&Url::parse("https://notdocs.rs/x").unwrap()));
}

#[test]
fn preapproved_subdomains_require_an_explicit_wildcard() {
    let policy = policy_with(|config| config.preapproved_domains = vec!["*.rust-lang.org".into()]);

    assert!(policy.is_preapproved(&Url::parse("https://doc.rust-lang.org/book/").unwrap()));
    assert!(policy.is_preapproved(&Url::parse("https://rust-lang.org/").unwrap()));
    assert!(
        !policy.is_preapproved(&Url::parse("https://evil-rust-lang.org/").unwrap()),
        "the wildcard must anchor on a dot boundary"
    );
}

#[test]
fn a_preapproved_rule_may_narrow_to_a_path_prefix() {
    let policy = policy_with(|config| config.preapproved_domains = vec!["github.com/anthropics".into()]);

    assert!(policy.is_preapproved(&Url::parse("https://github.com/anthropics").unwrap()));
    assert!(policy.is_preapproved(&Url::parse("https://github.com/anthropics/agentrs").unwrap()));
    assert!(
        !policy.is_preapproved(&Url::parse("https://github.com/anthropics-evil/malware").unwrap()),
        "the path prefix must match on a segment boundary"
    );
    assert!(!policy.is_preapproved(&Url::parse("https://github.com/someone-else").unwrap()));
}

#[test]
fn a_malformed_preapproved_rule_is_dropped_rather_than_matching_everything() {
    let policy = policy_with(|config| config.preapproved_domains = vec!["".into(), "   ".into(), "*.".into()]);

    assert!(!policy.is_preapproved(&Url::parse("https://anything.example/").unwrap()));
}

#[test]
fn nothing_is_preapproved_by_default() {
    assert!(!policy().is_preapproved(&Url::parse("https://example.com/").unwrap()));
}

// --- TC-1.1-25: DNS rebinding ---

#[tokio::test]
async fn resolved_check_rejects_a_name_pointing_at_loopback() {
    // `localhost` is the one name guaranteed to resolve to a loopback address
    // on every CI platform, so it stands in for a public name whose A record
    // points inward.
    let url = Url::parse("https://localhost/x").unwrap();
    assert!(
        matches!(
            policy().check_resolved(&url).await,
            Err(RejectReason::PrivateHost { .. })
        ),
        "post-resolution check must catch names that resolve inward"
    );
}

#[tokio::test]
async fn resolved_check_is_skipped_when_private_network_is_allowed() {
    let policy = policy_with(|config| config.allow_private_network = true);
    let url = Url::parse("https://localhost/x").unwrap();
    assert!(policy.check_resolved(&url).await.is_ok());
}

#[tokio::test]
async fn resolved_check_ignores_literal_addresses() {
    // Literals were already judged by `check`; re-resolving them would be a
    // pointless lookup.
    let url = Url::parse("https://93.184.216.34/x").unwrap();
    assert!(policy().check_resolved(&url).await.is_ok());
}

#[tokio::test]
async fn resolution_failure_is_not_treated_as_a_policy_rejection() {
    let url = Url::parse("https://nonexistent.invalid/x").unwrap();
    assert!(
        policy().check_resolved(&url).await.is_ok(),
        "an unresolvable name should surface as a connect error, not a policy denial"
    );
}

// --- Rejection messages ---
//
// These strings are what the model actually reads when a fetch is refused, so
// each variant must render something it can act on.

#[test]
fn every_rejection_reason_renders_an_actionable_message() {
    let cases = vec![
        (RejectReason::InvalidUrl, vec!["Invalid URL"]),
        (
            RejectReason::UrlTooLong {
                length: 2500,
                limit: 2000,
            },
            vec!["2500", "2000"],
        ),
        (RejectReason::CredentialsInUrl, vec!["credentials"]),
        (
            RejectReason::UnsupportedScheme {
                scheme: "ftp".to_string(),
            },
            vec!["ftp", "http"],
        ),
        (
            RejectReason::NotPublicHostname {
                host: "intranet".to_string(),
            },
            vec!["intranet", "publicly resolvable"],
        ),
        (
            RejectReason::PrivateHost {
                host: "127.0.0.1".to_string(),
            },
            // Names the config switch so the user can act without reading source.
            vec!["127.0.0.1", "web.allow_private_network"],
        ),
        (
            RejectReason::DeniedDomain {
                host: "evil.com".to_string(),
            },
            vec!["evil.com", "web.deny_domains"],
        ),
        (
            RejectReason::NotAllowedDomain {
                host: "other.com".to_string(),
            },
            vec!["other.com", "web.allow_domains"],
        ),
    ];

    for (reason, expected_fragments) in cases {
        let rendered = reason.to_string();
        assert!(!rendered.is_empty(), "{reason:?} renders nothing");
        for fragment in expected_fragments {
            assert!(
                rendered.contains(fragment),
                "{reason:?} should mention {fragment:?}, got: {rendered}"
            );
        }
    }
}

// --- Resolved-address adjudication ---

#[test]
fn resolved_addresses_that_are_all_public_are_accepted() {
    use std::net::IpAddr;

    let addresses: Vec<IpAddr> = vec!["93.184.216.34".parse().unwrap(), "2606:4700::1111".parse().unwrap()];
    assert!(super::judge_resolved_addresses("example.com", addresses).is_ok());
}

#[test]
fn one_private_address_among_public_ones_rejects_the_whole_name() {
    use std::net::IpAddr;

    // A rebinding attack only needs one inward answer in the record set.
    let addresses: Vec<IpAddr> = vec!["93.184.216.34".parse().unwrap(), "10.0.0.5".parse().unwrap()];
    let rejected = super::judge_resolved_addresses("evil.example", addresses);

    match rejected {
        Err(RejectReason::PrivateHost { host }) => {
            assert!(host.contains("evil.example"), "got: {host}");
            assert!(host.contains("10.0.0.5"), "the offending address must be named: {host}");
        }
        other => panic!("expected a private-host rejection, got {other:?}"),
    }
}

#[test]
fn a_name_that_resolves_to_nothing_is_not_a_rejection() {
    use std::net::IpAddr;

    let none: Vec<IpAddr> = Vec::new();
    assert!(
        super::judge_resolved_addresses("nothing.example", none).is_ok(),
        "an empty answer should surface as a connect error, not a policy denial"
    );
}

// A Location header can name a hostless scheme; the redirect adjudicator must
// refuse it rather than panicking or comparing None to None.
#[test]
fn denies_a_redirect_to_a_hostless_url() {
    let original = Url::parse("https://a.com/1").unwrap();
    let target = Url::parse("file:///etc/passwd").unwrap();

    assert!(!policy().is_permitted_redirect(&original, &target));
}
