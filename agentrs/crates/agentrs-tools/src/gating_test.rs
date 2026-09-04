use super::missing_tool_hint;

fn registered(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
    move |name: &str| names.contains(&name)
}

#[test]
fn an_unrecognized_name_gets_no_configuration_advice() {
    assert_eq!(
        missing_tool_hint("Nonsense", registered(&["WebFetch"])),
        None,
        "a genuine typo must not be misdiagnosed as a disabled feature"
    );
    assert_eq!(missing_tool_hint("Read", registered(&[])), None);
}

#[test]
fn a_missing_web_search_names_the_backend_setting() {
    let hint = missing_tool_hint("WebSearch", registered(&["WebFetch"])).expect("a hint");

    assert!(hint.contains("[web.search]"), "got: {hint}");
    for backend in ["duckduckgo", "brave", "tavily", "searxng"] {
        assert!(hint.contains(backend), "{backend} must be offered: {hint}");
    }
    assert!(
        hint.find("duckduckgo") < hint.find("brave"),
        "the keyless option should be reachable first: {hint}"
    );
    assert!(hint.contains("api_key_env"), "got: {hint}");
    assert!(
        hint.contains("agentrs config path"),
        "the user needs to know which file to edit: {hint}"
    );
}

// When neither web tool is present the whole feature is off, so pointing at the
// search backend would send the user to the wrong setting.
#[test]
fn both_web_tools_missing_points_at_the_enable_switch() {
    for name in ["WebFetch", "WebSearch"] {
        let hint = missing_tool_hint(name, registered(&[])).expect("a hint");
        assert!(hint.contains("`enabled = true`"), "{name} got: {hint}");
        assert!(
            !hint.contains("[web.search]"),
            "{name} must not be blamed on the backend: {hint}"
        );
    }
}

#[test]
fn a_registered_web_fetch_needs_no_hint() {
    assert_eq!(
        missing_tool_hint("WebFetch", registered(&["WebFetch", "WebSearch"])),
        None,
        "asking about a tool that exists is not a gating problem"
    );
}

#[test]
fn a_missing_todo_write_points_at_its_own_switch() {
    let hint = missing_tool_hint("TodoWrite", registered(&["Read"])).expect("a hint");

    assert!(hint.contains("[todo]"), "got: {hint}");
    assert!(hint.contains("`enabled = true`"), "got: {hint}");
    assert!(hint.contains("agentrs config path"), "got: {hint}");
    assert!(
        !hint.contains("[web"),
        "TodoWrite must not inherit the web hints: {hint}"
    );
}

// TodoWrite has a single on/off switch, so its answer must not depend on
// whether the unrelated web tools happen to be registered.
#[test]
fn the_todo_write_hint_ignores_which_other_tools_exist() {
    let alone = missing_tool_hint("TodoWrite", registered(&[])).expect("a hint");
    let alongside_web = missing_tool_hint("TodoWrite", registered(&["WebFetch", "WebSearch"])).expect("a hint");
    assert_eq!(alone, alongside_web);
}

// Adding TodoWrite to the gated list must not change what the web tools report.
#[test]
fn todo_write_does_not_disturb_the_web_hints() {
    let hint = missing_tool_hint("WebFetch", registered(&[])).expect("a hint");
    assert!(
        hint.contains("`[web]`"),
        "an unregistered TodoWrite must not make the web tools look configured: {hint}"
    );
}
