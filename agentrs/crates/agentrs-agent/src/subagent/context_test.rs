use agentrs_config::config::{CliArgs, Config};

use super::{SubAgentIdentity, build_child_prompt};
use crate::tool_policy::ToolPolicy;

fn config() -> Config {
    Config::resolve(&CliArgs {
        provider: Some("anthropic".to_string()),
        api_key: Some("test".to_string()),
        base_url: None,
        model: Some("test-model".to_string()),
        max_tokens: None,
        thinking: None,
        thinking_budget: None,
        max_turns: None,
        max_tool_call_malformed_turns: None,
        max_tool_call_failure_turns: None,
        system_prompt: Some("PARENT Spawn Skill WebSearch".to_string()),
        profile: None,
        auto_approve: false,
        project_dir: None,
    })
    .expect("resolve config")
}

#[test]
fn child_prompt_is_rebuilt_from_real_capabilities() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let prompt = build_child_prompt(
        &ToolPolicy::allow_only(["Read"]),
        &config(),
        workspace.path(),
        None,
        false,
    )
    .expect("build prompt");
    assert!(!prompt.contains("PARENT Spawn Skill WebSearch"));
    assert!(prompt.contains(&workspace.path().to_string_lossy().to_string()));
    assert!(prompt.contains("final text message is the return value"));
    assert!(prompt.contains("list their paths"));
}

#[test]
fn identity_is_appended_for_persistent_agents() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let identity = SubAgentIdentity {
        name: "researcher".to_string(),
        team: Some("alpha".to_string()),
        teammates: vec!["builder".to_string()],
    };
    let prompt = build_child_prompt(
        &ToolPolicy::default(),
        &config(),
        workspace.path(),
        Some(&identity),
        false,
    )
    .expect("build prompt");
    assert!(prompt.contains("researcher"));
    assert!(prompt.contains("alpha"));
    assert!(prompt.contains("builder"));
}
