//! End-to-end behaviour of the sub-agent layer: what a child can reach, how a
//! child hands work back, and what the parent sees when one of several
//! children fails.
//!
//! The unit tests next to `spawner.rs` cover registry assembly in isolation;
//! these drive a whole child engine through a scripted provider so the wiring
//! between spawner, engine, tool registry and task store is exercised together.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::mpsc;

use agentrs_agent::spawn_tool::SpawnTool;
use agentrs_agent::spawner::{AgentSpawner, SubAgentSpec};
use agentrs_agent::tool_policy::ToolPolicy;
use agentrs_config::todo::TodoMode;
use agentrs_providers::{LlmProvider, ProviderError};
use agentrs_tools::Tool;
use agentrs_tools::task::{TaskDraft, TaskStore, task_dir};
use agentrs_types::llm::{LlmEvent, LlmRequest};
use agentrs_types::message::{ContentBlock, Message, StopReason, TokenUsage};
use common::test_config;

/// A provider that answers according to the sub-agent's own prompt.
///
/// Sub-agents are spawned concurrently and share one provider, so a queue of
/// canned responses cannot say which child got which answer. Keying on the
/// prompt text makes every assertion below independent of scheduling order.
struct ScriptedProvider {
    /// `(prompt substring, events for the first turn)`.
    scripts: Vec<(String, Vec<LlmEvent>)>,
    /// Tool names advertised on the most recent request, sorted.
    advertised_tools: Mutex<Vec<String>>,
    /// System prompt of the most recent request.
    system: Mutex<String>,
}

impl ScriptedProvider {
    fn new(scripts: Vec<(&str, Vec<LlmEvent>)>) -> Self {
        Self {
            scripts: scripts.into_iter().map(|(key, ev)| (key.to_string(), ev)).collect(),
            advertised_tools: Mutex::new(Vec::new()),
            system: Mutex::new(String::new()),
        }
    }

    /// Whether this turn already carries a tool result, i.e. the scripted tool
    /// call has been executed and the child should now finish.
    fn tool_result_seen(messages: &[Message]) -> bool {
        messages
            .iter()
            .flat_map(|message| message.content.iter())
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    }

    fn prompt_text(messages: &[Message]) -> String {
        messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn end_turn(text: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::TextDelta(text.to_string()),
        LlmEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::default(),
        },
    ]
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn stream(&self, request: &LlmRequest) -> Result<mpsc::Receiver<LlmEvent>, ProviderError> {
        let mut names: Vec<String> = request.tools.iter().map(|tool| tool.name.clone()).collect();
        names.sort();
        *self.advertised_tools.lock().unwrap() = names;
        *self.system.lock().unwrap() = request.system.clone();

        let prompt = Self::prompt_text(&request.messages);
        let events = if Self::tool_result_seen(&request.messages) {
            end_turn("done")
        } else {
            self.scripts
                .iter()
                .find(|(key, _)| prompt.contains(key.as_str()))
                .map(|(_, events)| events.clone())
                .unwrap_or_else(|| end_turn("no script"))
        };

        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            for event in events {
                let _ = tx.send(event).await;
            }
        });
        Ok(rx)
    }
}

fn sub_config(name: &str, prompt: &str) -> SubAgentSpec {
    SubAgentSpec {
        name: name.to_string(),
        agent_type: None,
        prompt: prompt.to_string(),
        max_turns: Some(5),
        max_tokens: Some(1024),
        system_prompt: None,
        depth: 0,
        resume: None,
        persistent: false,
        isolation: Default::default(),
    }
}

/// Graph mode is the only channel a child has back to the parent's plan: the
/// store is file-backed, so a claim the child makes is visible to the parent
/// once the child returns.
#[tokio::test]
async fn a_graph_mode_child_claims_a_task_the_parent_planned() {
    let workspace = tempdir().expect("tempdir");
    let parent_store = TaskStore::new(task_dir(workspace.path()));
    parent_store
        .create(vec![TaskDraft {
            subject: "wire the parser".to_string(),
            description: String::new(),
            active_form: None,
            owner: None,
            blocked_by: Vec::new(),
        }])
        .expect("parent plans the task");

    let provider = Arc::new(ScriptedProvider::new(vec![(
        "claim task 1",
        vec![
            LlmEvent::ToolUse {
                id: "call-1".to_string(),
                name: "TaskUpdate".to_string(),
                input: json!({ "taskId": "1", "status": "in_progress", "owner": "child" }),
                extra: None,
            },
            LlmEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: TokenUsage::default(),
            },
        ],
    )]));

    let mut config = test_config();
    config.todo.mode = TodoMode::Graph;
    let spawner = AgentSpawner::new(
        provider,
        config,
        workspace.path().to_path_buf(),
        ToolPolicy::Unrestricted,
    );

    let result = spawner.spawn_one(sub_config("worker", "claim task 1")).await;
    assert!(!result.status.is_error(), "child failed: {}", result.text);

    let task = parent_store.get("1").expect("read graph").expect("task 1 still exists");
    assert_eq!(task.status.as_str(), "in_progress");
    assert_eq!(task.owner.as_deref(), Some("child"), "the parent must see the claim");
}

/// A list-mode child plans privately: its checklist never reaches the parent's
/// workspace, so there is nothing to reconcile when it returns.
#[tokio::test]
async fn a_list_mode_child_leaves_no_plan_behind_in_the_workspace() {
    let workspace = tempdir().expect("tempdir");

    let provider = Arc::new(ScriptedProvider::new(vec![(
        "track some work",
        vec![
            LlmEvent::ToolUse {
                id: "call-1".to_string(),
                name: "TodoWrite".to_string(),
                input: json!({ "todos": [{ "content": "private step", "status": "in_progress" }] }),
                extra: None,
            },
            LlmEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: TokenUsage::default(),
            },
        ],
    )]));

    let mut config = test_config();
    config.todo.mode = TodoMode::List;
    let spawner = AgentSpawner::new(
        provider,
        config,
        workspace.path().to_path_buf(),
        ToolPolicy::Unrestricted,
    );

    let result = spawner.spawn_one(sub_config("worker", "track some work")).await;
    assert!(!result.status.is_error(), "child failed: {}", result.text);

    assert!(
        !task_dir(workspace.path()).join("tasks.json").exists(),
        "a list-mode child must not write a task graph into the workspace"
    );
}

/// A child is a leaf: it may not spawn further agents, invoke skills, or reach
/// the network, even when the parent policy allows everything.
#[tokio::test]
async fn an_unrestricted_child_still_cannot_spawn_or_reach_the_network() {
    let provider = Arc::new(ScriptedProvider::new(vec![("survey", end_turn("surveyed"))]));
    let spawner = AgentSpawner::new(
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
        test_config(),
        std::env::temp_dir(),
        ToolPolicy::Unrestricted,
    );

    let result = spawner.spawn_one(sub_config("surveyor", "survey")).await;
    assert!(!result.status.is_error(), "child failed: {}", result.text);

    let advertised = provider.advertised_tools.lock().unwrap().clone();
    for forbidden in ["Spawn", "Skill", "WebFetch", "WebSearch"] {
        assert!(
            !advertised.contains(&forbidden.to_string()),
            "a sub-agent must not be handed '{forbidden}', got {advertised:?}"
        );
    }
    assert!(advertised.contains(&"Read".to_string()), "got {advertised:?}");
    assert!(advertised.contains(&"ToolSearch".to_string()), "got {advertised:?}");
}

/// One failing child must not take the batch down: the parent still receives
/// every sibling's answer, in the order the tasks were requested.
#[tokio::test]
async fn a_failing_sibling_does_not_discard_the_other_results() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        ("task alpha", end_turn("alpha result")),
        ("task beta", vec![LlmEvent::Error("provider exploded".to_string())]),
        ("task gamma", end_turn("gamma result")),
    ]));
    let spawner = Arc::new(AgentSpawner::new(
        provider,
        test_config(),
        std::env::temp_dir(),
        ToolPolicy::Unrestricted,
    ));

    let tool = SpawnTool::new(spawner);
    let output = tool
        .execute(json!({
            "tasks": [
                { "name": "alpha", "prompt": "task alpha" },
                { "name": "beta", "prompt": "task beta" },
                { "name": "gamma", "prompt": "task gamma" },
            ]
        }))
        .await;

    assert!(
        !output.is_error,
        "the batch is only an error when every child failed: {}",
        output.content
    );
    assert!(output.content.contains("alpha result"), "{}", output.content);
    assert!(output.content.contains("gamma result"), "{}", output.content);
    assert!(
        output.content.contains("name=\"beta\" status=\"error\""),
        "{}",
        output.content
    );

    let alpha = output.content.find("name=\"alpha\"").expect("alpha section");
    let beta = output.content.find("name=\"beta\"").expect("beta section");
    let gamma = output.content.find("name=\"gamma\"").expect("gamma section");
    assert!(
        alpha < beta && beta < gamma,
        "results must keep the requested order despite concurrent completion"
    );
}

/// The batch is reported as an error only when nothing came back usable.
#[tokio::test]
async fn a_batch_where_every_child_failed_is_reported_as_an_error() {
    let provider = Arc::new(ScriptedProvider::new(vec![
        ("task alpha", vec![LlmEvent::Error("down".to_string())]),
        ("task beta", vec![LlmEvent::Error("down".to_string())]),
    ]));
    let spawner = Arc::new(AgentSpawner::new(
        provider,
        test_config(),
        std::env::temp_dir(),
        ToolPolicy::Unrestricted,
    ));

    let output = SpawnTool::new(spawner)
        .execute(json!({
            "tasks": [
                { "name": "alpha", "prompt": "task alpha" },
                { "name": "beta", "prompt": "task beta" },
            ]
        }))
        .await;

    assert!(output.is_error, "{}", output.content);
}
