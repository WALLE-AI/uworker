use std::fs;

use agentrs_types::subagent::AgentSource;

use super::AgentDefinitions;

#[test]
fn builtins_include_visible_and_hidden_definitions() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let definitions = AgentDefinitions::load(workspace.path(), true);
    assert!(definitions.resolve(Some("general-purpose")).is_some());
    assert!(definitions.resolve(Some("explore")).is_some());
    assert!(definitions.visible().iter().all(|definition| !definition.hidden));
    assert!(
        !definitions
            .visible()
            .iter()
            .any(|definition| definition.name == "summarize")
    );
}

#[test]
fn project_definition_overrides_builtin_and_invalid_neighbors_are_skipped() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let directory = workspace.path().join(".agentrs").join("agents");
    fs::create_dir_all(&directory).expect("agent directory");
    fs::write(
        directory.join("explore.md"),
        "---\nwhen-to-use: Project exploration\nallowed-tools:\n  - Read\n---\nProject-specific explorer.",
    )
    .expect("definition");
    fs::write(directory.join("broken.md"), "---\ninvalid: [\n---\nbody").expect("broken definition");

    let definitions = AgentDefinitions::load(workspace.path(), true);
    let explore = definitions.resolve(Some("explore")).expect("explore");
    assert_eq!(explore.source, AgentSource::Project);
    assert_eq!(explore.allowed_tools, ["Read"]);
    assert_eq!(explore.system_prompt.as_deref(), Some("Project-specific explorer."));
    assert!(definitions.resolve(Some("broken")).is_none());
}

#[test]
fn builtin_agents_can_be_disabled() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let definitions = AgentDefinitions::load(workspace.path(), false);
    assert!(definitions.visible().is_empty());
}
