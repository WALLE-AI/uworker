use std::time::Duration;

use super::SubAgentConfig;

#[test]
fn defaults_match_runtime_contract() {
    let config = SubAgentConfig::default();
    assert!(config.enabled);
    assert_eq!(config.max_per_call, 5);
    assert_eq!(config.max_concurrent, 5);
    assert_eq!(config.max_turns, 200);
    assert_eq!(config.max_tokens, 4096);
    assert_eq!(config.depth, 1);
    assert_eq!(config.turn_output_budget, None);
    assert_eq!(config.cancel_grace, Duration::from_secs(5));
    assert!(config.persist_sessions);
    assert!(config.builtin_agents);
}

#[test]
fn duration_is_configured_in_milliseconds() {
    let config: SubAgentConfig = toml::from_str("cancel_grace = 250").expect("parse config");
    assert_eq!(config.cancel_grace, Duration::from_millis(250));
}

#[test]
fn project_overlay_is_field_level_and_can_restore_defaults() {
    let global: SubAgentConfig = toml::from_str("enabled = false\nmax_concurrent = 2\nmax_turns = 50").unwrap();
    let project: SubAgentConfig = toml::from_str("enabled = true\nmax_concurrent = 4").unwrap();
    let merged = global.overlay(project);

    assert!(merged.enabled);
    assert_eq!(merged.max_concurrent, 4);
    assert_eq!(merged.max_turns, 50);
}
