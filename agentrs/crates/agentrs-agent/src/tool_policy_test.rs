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

// --- Write confinement -------------------------------------------------------

use serde_json::json;

fn confined() -> (tempfile::TempDir, std::path::PathBuf, ToolPolicy) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("memory");
    std::fs::create_dir_all(&root).unwrap();
    let policy = ToolPolicy::allow_only_within(["Read", "Grep", "Glob", "Write", "Edit"], root.clone());
    (temp, root, policy)
}

#[test]
fn a_confinement_still_gates_on_tool_name() {
    let (_temp, _root, policy) = confined();

    assert!(policy.allows("Write"));
    assert!(policy.allows("Read"));
    assert!(
        !policy.allows("ExecCommand"),
        "a shell would write outside the root without ever naming a file_path"
    );
    assert!(!policy.allows("Spawn"));
}

#[test]
fn writes_inside_the_root_are_permitted() {
    let (_temp, root, policy) = confined();
    let inside = json!({ "file_path": root.join("user_role.md") });

    assert!(!policy.escapes_write_root("Write", &inside));
    assert!(!policy.escapes_write_root("Edit", &inside));
    assert!(!policy.escapes_write_root("Write", &json!({ "file_path": root.join("nested/deep.md") })));
}

// AC4-2: the confinement is what stops an extraction agent from persisting
// anywhere it likes.
#[test]
fn writes_outside_the_root_escape() {
    let (temp, root, policy) = confined();

    assert!(policy.escapes_write_root("Write", &json!({ "file_path": temp.path().join("secrets.env") })));
    assert!(policy.escapes_write_root("Edit", &json!({ "file_path": temp.path().join("src/main.rs") })));
    assert!(
        policy.escapes_write_root("Write", &json!({ "file_path": root.join("..").join("escape.md") })),
        "traversal out of the root is still out of the root"
    );
}

// Reading is deliberately unconfined: the agent has to be able to check a claim
// against the code before recording it.
#[test]
fn reads_are_not_confined() {
    let (temp, _root, policy) = confined();
    let outside = json!({ "file_path": temp.path().join("src/main.rs") });

    assert!(!policy.escapes_write_root("Read", &outside));
    assert!(!policy.escapes_write_root("Grep", &outside));
}

// Under a confinement an unreadable destination is indistinguishable from one
// outside it, so it is refused rather than waved through.
#[test]
fn a_write_without_a_readable_path_is_refused() {
    let (_temp, _root, policy) = confined();

    assert!(policy.escapes_write_root("Write", &json!({})));
    assert!(policy.escapes_write_root("Write", &json!({ "file_path": 42 })));
    assert!(policy.escapes_write_root("Edit", &serde_json::Value::Null));
}

#[test]
fn policies_without_a_confinement_never_report_an_escape() {
    let anywhere = json!({ "file_path": "/etc/passwd" });

    assert!(ToolPolicy::Unrestricted.write_root().is_none());
    assert!(!ToolPolicy::Unrestricted.escapes_write_root("Write", &anywhere));
    assert!(!ToolPolicy::allow_only(["Write"]).escapes_write_root("Write", &anywhere));
}
