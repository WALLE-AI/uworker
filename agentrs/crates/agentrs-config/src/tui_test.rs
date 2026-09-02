use super::{ThinkingDisplay, TuiConfig};

#[test]
fn thinking_defaults_to_collapsed_when_the_section_is_absent() {
    let config: TuiConfig = toml::from_str("").expect("an empty table is a valid TuiConfig");

    assert_eq!(config.thinking, ThinkingDisplay::Collapsed);
    assert!(config.thinking.is_visible());
    assert!(config.thinking.collapses_when_finished());
}

#[test]
fn thinking_accepts_each_documented_mode() {
    for (raw, expected) in [
        ("collapsed", ThinkingDisplay::Collapsed),
        ("full", ThinkingDisplay::Full),
        ("off", ThinkingDisplay::Off),
    ] {
        let config: TuiConfig = toml::from_str(&format!("thinking = \"{raw}\"")).expect("documented mode should parse");
        assert_eq!(config.thinking, expected, "mode {raw}");
    }
}

#[test]
fn unknown_thinking_mode_is_rejected_rather_than_silently_defaulted() {
    let parsed: Result<TuiConfig, _> = toml::from_str("thinking = \"sometimes\"");

    // Silently falling back would hide a typo that changes what the operator sees.
    assert!(parsed.is_err());
}

#[test]
fn full_and_off_do_not_collapse() {
    assert!(!ThinkingDisplay::Full.collapses_when_finished());
    assert!(!ThinkingDisplay::Off.collapses_when_finished());
    assert!(ThinkingDisplay::Full.is_visible());
    assert!(!ThinkingDisplay::Off.is_visible());
}
