use super::*;

#[cfg(test)]
mod phase7_tests {
    use agentrs_config::config::{CliArgs, Config};

    use super::{ForkOverrides, SubAgentConfig, ToolPolicy, build_tool_registry, effective_child_tool_policy};

    fn test_config() -> Config {
        Config::resolve(&CliArgs {
            provider: Some("anthropic".to_string()),
            api_key: Some("sk-test".to_string()),
            base_url: None,
            model: Some("claude-sonnet-4-20250514".to_string()),
            max_tokens: Some(4096),
            thinking: None,
            thinking_budget: None,
            max_turns: None,
            max_tool_call_malformed_turns: None,
            max_tool_call_failure_turns: None,
            system_prompt: None,
            profile: None,
            auto_approve: false,
            project_dir: None,
        })
        .expect("test config")
    }

    #[test]
    fn tc_7_1_fork_overrides_default_values() {
        let o = ForkOverrides::default();
        assert!(o.model.is_none());
        assert!(o.effort.is_none());
        assert!(o.allowed_tools.is_empty());
    }

    #[test]
    fn tc_7_40_build_tool_registry_unrestricted_registers_all() {
        let (registry, _) = build_tool_registry(&ToolPolicy::Unrestricted, &test_config(), &std::env::temp_dir(), &[]);
        for name in &["Read", "Write", "Edit", "ExecCommand", "Grep", "Glob", "TodoWrite"] {
            assert!(registry.get(name).is_some(), "tool '{name}' should be registered");
        }
    }

    #[test]
    fn tc_7_43_build_tool_registry_filters_to_policy() {
        let policy = ToolPolicy::allow_only(["ExecCommand", "Read"]);
        let (registry, _) = build_tool_registry(&policy, &test_config(), &std::env::temp_dir(), &[]);
        assert!(registry.get("ExecCommand").is_some());
        assert!(registry.get("Read").is_some());
        assert!(registry.get("Write").is_none());
    }

    #[test]
    fn fork_overrides_can_only_narrow_parent_policy() {
        let parent = ToolPolicy::allow_only(["Read", "Grep", "Spawn"]);
        let allowed_tools = vec!["Read".to_string(), "ExecCommand".to_string()];

        let child = effective_child_tool_policy(&parent, &allowed_tools);

        assert!(child.allows("Read"));
        assert!(!child.allows("Grep"));
        assert!(!child.allows("ExecCommand"));
        assert!(!child.allows("Write"));
    }

    #[test]
    fn empty_fork_override_inherits_parent_policy() {
        let parent = ToolPolicy::allow_only(["Read", "Grep", "Spawn"]);

        let child = effective_child_tool_policy(&parent, &[]);

        assert_eq!(child, parent);
    }

    #[test]
    fn tc_7_sub_agent_config_original_fields_intact() {
        let config = SubAgentConfig {
            name: "test-agent".to_string(),
            prompt: "do the task".to_string(),
            max_turns: 5,
            max_tokens: 1024,
            system_prompt: Some("you are helpful".to_string()),
        };
        assert_eq!(config.name, "test-agent");
        assert_eq!(config.max_turns, 5);
    }

    // --- TC-3.0-05: a child cannot regain a tool the parent was denied ---

    #[test]
    fn a_child_cannot_recover_a_network_tool_its_parent_was_denied() {
        let parent = ToolPolicy::allow_only(["Read", "WebSearch"]);
        let requested = vec!["Read".to_string(), "WebFetch".to_string(), "WebSearch".to_string()];

        let child = effective_child_tool_policy(&parent, &requested);

        assert!(child.allows("Read"));
        assert!(child.allows("WebSearch"));
        assert!(
            !child.allows("WebFetch"),
            "a fork override narrows the parent policy; it must never widen it"
        );
    }

    #[test]
    fn a_child_with_no_overrides_inherits_the_parent_network_restrictions() {
        let parent = ToolPolicy::allow_only(["Read"]);

        let child = effective_child_tool_policy(&parent, &[]);

        assert!(!child.allows("WebFetch"));
        assert!(!child.allows("WebSearch"));
    }
}

#[cfg(test)]
mod tests_todo_isolation {
    use std::path::Path;

    use agentrs_config::config::{CliArgs, Config};

    use crate::tool_policy::ToolPolicy;

    fn config() -> Config {
        Config::resolve(&CliArgs {
            provider: Some("anthropic".to_string()),
            api_key: Some("sk-test".to_string()),
            base_url: None,
            model: Some("claude-sonnet-4-20250514".to_string()),
            max_tokens: Some(4096),
            thinking: None,
            thinking_budget: None,
            max_turns: None,
            max_tool_call_malformed_turns: None,
            max_tool_call_failure_turns: None,
            system_prompt: None,
            profile: None,
            auto_approve: false,
            project_dir: None,
        })
        .expect("test config")
    }

    #[test]
    fn a_sub_agent_gets_its_own_checklist() {
        let (registry, store) = super::build_tool_registry(&ToolPolicy::default(), &config(), Path::new("/tmp"), &[]);

        assert!(registry.tool_names().contains(&"TodoWrite".to_string()));
        assert!(store.is_some(), "the engine needs the store to drive reminders");
    }

    // Each sub-agent runs its own engine and so builds its own store. Nothing
    // is shared with the parent, which is what makes the isolation structural
    // rather than something a key space has to get right.
    #[test]
    fn two_sub_agents_do_not_share_a_checklist() {
        let config = config();
        let (_, first) = super::build_tool_registry(&ToolPolicy::default(), &config, Path::new("/tmp"), &[]);
        let (_, second) = super::build_tool_registry(&ToolPolicy::default(), &config, Path::new("/tmp"), &[]);

        let first = first.expect("store");
        let second = second.expect("store");
        first.replace(vec![agentrs_tools::todo::TodoItem {
            content: "first agent task".to_string(),
            status: agentrs_tools::todo::TodoStatus::InProgress,
            active_form: None,
        }]);

        assert_eq!(first.snapshot().len(), 1);
        assert!(second.is_empty(), "a sibling must not see another agent's plan");
    }

    #[test]
    fn a_disabled_todo_section_reaches_sub_agents_too() {
        let mut config = config();
        config.todo.enabled = false;

        let (registry, store) = super::build_tool_registry(&ToolPolicy::default(), &config, Path::new("/tmp"), &[]);
        assert!(!registry.tool_names().contains(&"TodoWrite".to_string()));
        assert!(store.is_none());
    }

    // A fork override that narrows the child's tools must be able to take the
    // checklist away like any other tool.
    #[test]
    fn a_policy_that_denies_todo_write_wins() {
        let policy = ToolPolicy::allow_only(["Read"]);
        let (registry, store) = super::build_tool_registry(&policy, &config(), Path::new("/tmp"), &[]);

        assert!(!registry.tool_names().contains(&"TodoWrite".to_string()));
        assert!(store.is_none());
    }
}
