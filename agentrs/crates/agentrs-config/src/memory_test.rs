use super::MemoryConfig;

#[test]
fn defaults_preserve_existing_behavior_without_auto_extraction() {
    let config = MemoryConfig::default();
    assert!(config.enabled);
    assert!(config.recall.enabled);
    assert_eq!(config.recall.limit, 5);
    assert_eq!(config.recall.timeout_ms, 3_000);
    assert!(!config.extract.enabled);
}

#[test]
fn nested_partial_tables_keep_defaults() {
    let config: MemoryConfig =
        toml::from_str("enabled = false\ndir = 'custom'\n[recall]\nlimit = 3\n[extract]\nenabled = true").unwrap();
    assert!(!config.enabled);
    assert_eq!(config.dir.unwrap().to_string_lossy(), "custom");
    assert!(config.recall.enabled);
    assert_eq!(config.recall.limit, 3);
    assert!(config.extract.enabled);
    assert_eq!(config.extract.max_turns, 5);
}

#[test]
fn project_overlay_is_field_level() {
    let global: MemoryConfig = toml::from_str("enabled = false\n[recall]\nlimit = 9\ntimeout_ms = 8000").unwrap();
    let project: MemoryConfig = toml::from_str("enabled = true\n[recall]\nlimit = 5").unwrap();
    let merged = global.overlay(project);
    assert!(merged.enabled);
    assert_eq!(merged.recall.limit, 5);
    assert_eq!(merged.recall.timeout_ms, 8_000);
}
