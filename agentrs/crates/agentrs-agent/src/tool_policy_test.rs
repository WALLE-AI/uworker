use super::ToolPolicy;

#[test]
fn unrestricted_policy_allows_every_tool() {
    assert!(ToolPolicy::Unrestricted.allows("ExecCommand"));
}

#[test]
fn allow_only_policy_matches_exact_tool_names() {
    let policy = ToolPolicy::allow_only(["Read", "team_send_message"]);

    assert!(policy.allows("Read"));
    assert!(policy.allows("team_send_message"));
    assert!(!policy.allows("Write"));
    assert!(!policy.allows("read"));
}

// --- TC-3.0-04 / TC-3.0-05: the network tools are policy-gated like any other ---

#[test]
fn allow_only_policy_can_exclude_the_network_tools() {
    let policy = ToolPolicy::allow_only(["Read", "Grep"]);

    assert!(
        !policy.allows("WebFetch"),
        "an allow-list that omits WebFetch must not grant outbound HTTP"
    );
    assert!(!policy.allows("WebSearch"));
    assert!(policy.allows("Read"));
}

#[test]
fn allow_only_policy_can_include_just_one_network_tool() {
    let policy = ToolPolicy::allow_only(["WebSearch"]);

    assert!(policy.allows("WebSearch"));
    assert!(
        !policy.allows("WebFetch"),
        "granting search must not imply granting arbitrary fetching"
    );
}
