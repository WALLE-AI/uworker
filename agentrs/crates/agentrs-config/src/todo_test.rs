use super::{TodoConfig, TodoMode};

#[test]
fn defaults_enable_sequential_tracking_with_reminders() {
    let config = TodoConfig::default();
    assert!(config.enabled);
    assert!(
        !config.allow_parallel_in_progress,
        "single-active discipline is the safe default"
    );
    assert_eq!(config.reminder_turns, 10);
}

#[test]
fn an_absent_section_deserializes_to_the_defaults() {
    let config: TodoConfig = toml::from_str("").expect("empty table is valid");
    assert_eq!(config, TodoConfig::default());
}

#[test]
fn partial_sections_keep_the_remaining_defaults() {
    let config: TodoConfig = toml::from_str("allow_parallel_in_progress = true").expect("valid");
    assert!(config.allow_parallel_in_progress);
    assert!(config.enabled, "unspecified fields fall back to the default");
    assert_eq!(config.reminder_turns, 10);
}

#[test]
fn every_field_can_be_overridden() {
    let config: TodoConfig = toml::from_str(
        r#"
        enabled = false
        allow_parallel_in_progress = true
        reminder_turns = 3
        "#,
    )
    .expect("valid");

    assert!(!config.enabled);
    assert!(config.allow_parallel_in_progress);
    assert_eq!(config.reminder_turns, 3);
}

#[test]
fn zero_reminder_turns_is_accepted_as_the_off_switch() {
    let config: TodoConfig = toml::from_str("reminder_turns = 0").expect("valid");
    assert_eq!(config.reminder_turns, 0);
}

#[test]
fn round_trips_through_toml() {
    let original = TodoConfig {
        enabled: true,
        allow_parallel_in_progress: true,
        reminder_turns: 7,
    };
    let text = toml::to_string(&original).expect("serialize");
    let back: TodoConfig = toml::from_str(&text).expect("deserialize");
    assert_eq!(back, original);
}

#[test]
fn the_flat_checklist_is_the_default_mode() {
    assert_eq!(TodoConfig::default().mode, TodoMode::List);
}

#[test]
fn graph_mode_is_selectable_by_name() {
    let config: TodoConfig = toml::from_str(r#"mode = "graph""#).expect("valid");
    assert_eq!(config.mode, TodoMode::Graph);
    assert!(config.enabled, "mode must not disturb the other defaults");
}

#[test]
fn an_unknown_mode_is_rejected_rather_than_silently_defaulted() {
    assert!(
        toml::from_str::<TodoConfig>(r#"mode = "swarm""#).is_err(),
        "a typo must fail loudly instead of quietly falling back to list mode"
    );
}

#[test]
fn mode_round_trips_through_toml() {
    let original = TodoConfig {
        mode: TodoMode::Graph,
        ..TodoConfig::default()
    };
    let back: TodoConfig = toml::from_str(&toml::to_string(&original).expect("serialize")).expect("deserialize");
    assert_eq!(back.mode, TodoMode::Graph);
}
