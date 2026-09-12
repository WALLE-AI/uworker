use super::*;

#[cfg(test)]
mod phase7_tests {
    use agentrs_config::config::{CliArgs, Config};
    use agentrs_types::message::TokenUsage;

    use super::{ForkOverrides, SubAgentSpec, ToolPolicy, build_tool_registry, child_policy, usage_delta};

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
    fn resumed_agent_reports_only_incremental_usage() {
        let baseline = TokenUsage {
            input_tokens: 100,
            output_tokens: 40,
            cache_creation_tokens: 10,
            cache_read_tokens: 20,
        };
        let total = TokenUsage {
            input_tokens: 130,
            output_tokens: 55,
            cache_creation_tokens: 12,
            cache_read_tokens: 27,
        };
        let delta = usage_delta(&total, &baseline);
        assert_eq!(delta.input_tokens, 30);
        assert_eq!(delta.output_tokens, 15);
        assert_eq!(delta.cache_creation_tokens, 2);
        assert_eq!(delta.cache_read_tokens, 7);
    }

    #[test]
    fn tc_7_40_build_tool_registry_unrestricted_registers_all() {
        let (registry, _) = build_tool_registry(&ToolPolicy::Unrestricted, &test_config(), &std::env::temp_dir(), &[]);
        for name in &["Read", "Write", "Edit", "ExecCommand", "Grep", "Glob", "TodoWrite"] {
            assert!(registry.get(name).is_some(), "tool '{name}' should be registered");
        }
        assert!(ToolPolicy::Unrestricted.allows("ToolSearch"));
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

        let child = child_policy(&parent, &allowed_tools);

        assert!(child.allows("Read"));
        assert!(!child.allows("Grep"));
        assert!(!child.allows("ExecCommand"));
        assert!(!child.allows("Write"));
    }

    #[test]
    fn empty_fork_override_inherits_parent_policy() {
        let parent = ToolPolicy::allow_only(["Read", "Grep", "Spawn"]);

        let child = child_policy(&parent, &[]);

        assert_eq!(child, parent);
    }

    #[test]
    fn tc_7_sub_agent_config_original_fields_intact() {
        let config = SubAgentSpec {
            name: "test-agent".to_string(),
            agent_type: None,
            prompt: "do the task".to_string(),
            max_turns: Some(5),
            max_tokens: Some(1024),
            system_prompt: Some("you are helpful".to_string()),
            depth: 0,
            resume: None,
            persistent: false,
            isolation: Default::default(),
        };
        assert_eq!(config.name, "test-agent");
        assert_eq!(config.max_turns, Some(5));
    }

    // --- TC-3.0-05: a child cannot regain a tool the parent was denied ---

    #[test]
    fn a_child_cannot_recover_a_network_tool_its_parent_was_denied() {
        let parent = ToolPolicy::allow_only(["Read", "WebSearch"]);
        let requested = vec!["Read".to_string(), "WebFetch".to_string(), "WebSearch".to_string()];

        let child = child_policy(&parent, &requested);

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

        let child = child_policy(&parent, &[]);

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
        let (registry, source) = super::build_tool_registry(&ToolPolicy::default(), &config(), Path::new("/tmp"), &[]);

        assert!(registry.tool_names().contains(&"TodoWrite".to_string()));
        assert!(source.is_some(), "the engine needs the store to drive reminders");
    }

    // A graph-mode workspace must not see the flat checklist reappear inside a
    // sub-agent: TodoWrite is not a tool that deployment selected.
    #[test]
    fn a_graph_mode_sub_agent_gets_the_task_tools_instead() {
        let mut config = config();
        config.todo.mode = agentrs_config::todo::TodoMode::Graph;
        let workspace = tempfile::TempDir::new().expect("temp workspace");

        let (registry, source) = super::build_tool_registry(&ToolPolicy::default(), &config, workspace.path(), &[]);
        let names = registry.tool_names();

        for tool in ["TaskCreate", "TaskList", "TaskGet", "TaskUpdate"] {
            assert!(names.contains(&tool.to_string()), "{tool} missing from {names:?}");
        }
        assert!(!names.contains(&"TodoWrite".to_string()), "got {names:?}");
        assert!(source.is_some());
    }

    // The graph is file-backed and lockable precisely so a child can claim work
    // the parent planned, rather than keeping a private copy the parent cannot
    // see. Same workspace means the same graph.
    #[test]
    fn a_graph_mode_sub_agent_shares_the_workspace_graph() {
        let mut config = config();
        config.todo.mode = agentrs_config::todo::TodoMode::Graph;
        let workspace = tempfile::TempDir::new().expect("temp workspace");

        let parent = agentrs_tools::task::TaskStore::new(agentrs_tools::task::task_dir(workspace.path()));
        parent
            .create(vec![agentrs_tools::task::TaskDraft {
                subject: "planned by the parent".to_string(),
                description: String::new(),
                active_form: None,
                owner: None,
                blocked_by: Vec::new(),
            }])
            .expect("create");

        let (registry, _) = super::build_tool_registry(&ToolPolicy::default(), &config, workspace.path(), &[]);
        let listed = futures::executor::block_on(
            registry
                .get("TaskList")
                .expect("TaskList registered")
                .execute(serde_json::json!({})),
        );

        assert!(
            listed.content.contains("planned by the parent"),
            "the child must see the parent's graph: {}",
            listed.content
        );
    }

    #[test]
    fn a_policy_that_denies_the_task_tools_leaves_a_graph_child_untracked() {
        let mut config = config();
        config.todo.mode = agentrs_config::todo::TodoMode::Graph;
        let policy = ToolPolicy::allow_only(["Read"]);

        let (registry, source) = super::build_tool_registry(&policy, &config, Path::new("/tmp"), &[]);
        assert!(!registry.tool_names().iter().any(|name| name.starts_with("Task")));
        assert!(source.is_none());
    }

    // Each sub-agent runs its own engine and so builds its own store. Nothing
    // is shared with the parent, which is what makes the isolation structural
    // rather than something a key space has to get right.
    // Each sub-agent runs its own engine and so builds its own store. In list
    // mode nothing is shared with the parent, which is what makes the isolation
    // structural rather than something a key space has to get right.
    #[test]
    fn two_list_mode_sub_agents_do_not_share_a_checklist() {
        let config = config();
        let (_, first) = super::build_tool_registry(&ToolPolicy::default(), &config, Path::new("/tmp"), &[]);
        let (_, second) = super::build_tool_registry(&ToolPolicy::default(), &config, Path::new("/tmp"), &[]);

        let (crate::todo_reminder::PlanSource::List(first), crate::todo_reminder::PlanSource::List(second)) =
            (first.expect("store"), second.expect("store"))
        else {
            panic!("the default mode should hand back flat checklists");
        };

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

#[cfg(test)]
mod persistent_team_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use agentrs_config::config::{CliArgs, Config};
    use agentrs_providers::{LlmProvider, ProviderError};
    use agentrs_types::llm::{LlmEvent, LlmRequest};
    use agentrs_types::message::{ContentBlock, StopReason, TokenUsage};
    use agentrs_types::subagent::{SubAgentIsolation, SubAgentSpec, SubAgentStatus};
    use agentrs_types::team::{AgentId, InboxMessage, Recipient, TeamMessageKind, TeamRuntime};
    use async_trait::async_trait;
    use chrono::Utc;
    use tokio::sync::{Notify, mpsc};
    use tokio_util::sync::CancellationToken;

    use crate::output::null_sink::NullSink;
    use crate::session::SessionManager;
    use crate::team::InProcessTeamRuntime;
    use crate::tool_policy::ToolPolicy;

    use super::AgentSpawner;

    struct RecordingProvider {
        calls: AtomicUsize,
        notify: Notify,
        prompts: Mutex<Vec<String>>,
        tools: Mutex<Vec<Vec<String>>>,
    }

    impl RecordingProvider {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                notify: Notify::new(),
                prompts: Mutex::new(Vec::new()),
                tools: Mutex::new(Vec::new()),
            }
        }

        async fn wait_for_calls(&self, expected: usize) {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while self.calls.load(Ordering::Acquire) < expected {
                    self.notify.notified().await;
                }
            })
            .await
            .expect("persistent teammate did not process its inbox");
        }
    }

    #[async_trait]
    impl LlmProvider for RecordingProvider {
        async fn stream(&self, request: &LlmRequest) -> Result<mpsc::Receiver<LlmEvent>, ProviderError> {
            let prompt = request
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.prompts.lock().unwrap().push(prompt);
            self.tools
                .lock()
                .unwrap()
                .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
            self.calls.fetch_add(1, Ordering::Release);
            self.notify.notify_waiters();
            let (tx, rx) = mpsc::channel(2);
            tx.try_send(LlmEvent::TextDelta("done".to_string())).unwrap();
            tx.try_send(LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            })
            .unwrap();
            Ok(rx)
        }
    }

    fn config(session_dir: &std::path::Path) -> Config {
        let mut config = Config::resolve(&CliArgs {
            provider: Some("anthropic".to_string()),
            api_key: Some("sk-test".to_string()),
            base_url: None,
            model: Some("claude-sonnet-4-20250514".to_string()),
            max_tokens: Some(1024),
            thinking: None,
            thinking_budget: None,
            max_turns: Some(5),
            max_tool_call_malformed_turns: None,
            max_tool_call_failure_turns: None,
            system_prompt: None,
            profile: None,
            auto_approve: true,
            project_dir: None,
        })
        .unwrap();
        config.session.directory = session_dir.to_string_lossy().into_owned();
        config
    }

    #[tokio::test]
    async fn persistent_spawn_receives_follow_up_then_joins_before_team_delete_returns() {
        let temp = tempfile::tempdir().unwrap();
        let provider = Arc::new(RecordingProvider::new());
        let spawner = AgentSpawner::new(
            provider.clone(),
            config(&temp.path().join("sessions")),
            temp.path().to_path_buf(),
            ToolPolicy::Unrestricted,
        );
        let sessions = SessionManager::new(temp.path().join("sessions"), 20);
        let parent = sessions
            .create(
                "anthropic",
                "claude-sonnet-4-20250514",
                &temp.path().to_string_lossy(),
                Some("parent"),
            )
            .unwrap();
        *spawner
            .parent_session_slot()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(parent.id);
        let team = Arc::new(InProcessTeamRuntime::new(
            temp.path().join("teams"),
            8,
            16,
            Arc::new(NullSink),
        ));
        team.attach_registry(&spawner.registry());
        let spawner = spawner.with_team_runtime(Arc::clone(&team));
        team.create_team("core", None, None).await.unwrap();

        let started = spawner
            .spawn_persistent(
                SubAgentSpec {
                    name: "alice".to_string(),
                    agent_type: None,
                    prompt: "inspect the parser".to_string(),
                    max_turns: Some(5),
                    max_tokens: Some(1024),
                    system_prompt: None,
                    depth: 0,
                    resume: None,
                    persistent: true,
                    isolation: SubAgentIsolation::Shared,
                },
                CancellationToken::new(),
            )
            .await;
        assert_eq!(started.status, SubAgentStatus::Running);
        provider.wait_for_calls(1).await;

        let current = team.current_team().unwrap();
        team.send(
            Recipient::Named(agentrs_types::team::AgentName::new("alice").unwrap()),
            InboxMessage {
                id: "follow-up".to_string(),
                from: current.lead_agent_id.clone(),
                message: "check the lexer too".to_string(),
                summary: "check lexer".to_string(),
                kind: TeamMessageKind::Text,
                sent_at: Utc::now(),
            },
        )
        .await
        .unwrap();
        provider.wait_for_calls(2).await;

        {
            let prompts = provider.prompts.lock().unwrap();
            assert!(prompts[1].contains("<teammate-message"));
            assert!(prompts[1].contains("check the lexer too"));
        }
        assert!(provider.tools.lock().unwrap()[1].contains(&"SendMessage".to_string()));

        let alice = agentrs_types::team::AgentName::new("alice").unwrap();
        let alice_runtime = team.for_member(&alice).unwrap();
        alice_runtime
            .send(
                Recipient::Named(agentrs_types::team::AgentName::team_lead()),
                InboxMessage {
                    id: "shutdown".to_string(),
                    from: AgentId::team_lead(&current.id),
                    message: "approved".to_string(),
                    summary: "shutdown approved".to_string(),
                    kind: TeamMessageKind::ShutdownResponse {
                        request_id: "request-1".to_string(),
                        approved: true,
                    },
                    sent_at: Utc::now(),
                },
            )
            .await
            .unwrap();
        let deleted = team.delete_team().await.unwrap();
        assert!(deleted.deleted);
        assert_eq!(deleted.stopped_members, vec![alice]);
        assert!(spawner.registry().list().is_empty());
        assert!(!current.team_file_path.exists());
    }
}
