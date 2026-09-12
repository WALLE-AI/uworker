use super::TeamConfig;

#[test]
fn defaults_are_bounded_and_opt_in() {
    let config = TeamConfig::default();

    assert!(!config.enabled);
    assert_eq!(config.max_members, 8);
    assert_eq!(config.inbox_capacity, 64);
}

#[test]
fn partial_table_preserves_unspecified_defaults() {
    let config: TeamConfig = toml::from_str("enabled = true\ninbox_capacity = 16").expect("parse team config");

    assert!(config.enabled);
    assert_eq!(config.max_members, TeamConfig::default().max_members);
    assert_eq!(config.inbox_capacity, 16);
}

#[test]
fn project_overlay_is_field_level_and_can_restore_defaults() {
    let global: TeamConfig =
        toml::from_str("enabled = true\nmax_members = 3\ninbox_capacity = 12").expect("parse global config");
    let project: TeamConfig = toml::from_str("enabled = false\nmax_members = 8").expect("parse project config");

    let merged = global.overlay(project);

    assert!(!merged.enabled);
    assert_eq!(merged.max_members, 8);
    assert_eq!(merged.inbox_capacity, 12);
}
