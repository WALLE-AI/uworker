use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::spawner::{AgentSpawner, SubAgentId, SubAgentIsolation, SubAgentSpec, SubAgentStatus};
use agentrs_protocol::events::ToolCategory;
use agentrs_types::tool::{JsonSchema, ToolResult};

use agentrs_tools::Tool;

pub struct SpawnTool {
    spawner: Arc<AgentSpawner>,
    depth: usize,
}

impl SpawnTool {
    pub fn new(spawner: Arc<AgentSpawner>) -> Self {
        Self { spawner, depth: 0 }
    }

    pub(crate) fn for_depth(spawner: Arc<AgentSpawner>, depth: usize) -> Self {
        Self { spawner, depth }
    }

    async fn execute_with_cancel(&self, input: Value, cancel: CancellationToken) -> ToolResult {
        let tasks = match parse_tasks(&input, self.depth) {
            Ok(tasks) => tasks,
            Err(error) => {
                return ToolResult {
                    content: error,
                    is_error: true,
                };
            }
        };
        if tasks.is_empty() {
            return ToolResult {
                content: "No tasks provided".to_string(),
                is_error: true,
            };
        }
        if tasks.len() > self.spawner.max_per_call() {
            return ToolResult {
                content: format!(
                    "Too many sub-agents: {} (max {})",
                    tasks.len(),
                    self.spawner.max_per_call()
                ),
                is_error: true,
            };
        }

        let (persistent, transient): (Vec<_>, Vec<_>) = tasks.into_iter().partition(|task| task.persistent);
        let mut results = futures::future::join_all(
            persistent
                .into_iter()
                .map(|task| self.spawner.spawn_persistent(task, cancel.child_token())),
        )
        .await;
        results.extend(self.spawner.spawn_parallel(transient, cancel).await);
        let output = results.iter().map(render_result).collect::<Vec<_>>().join("\n");
        ToolResult {
            content: output,
            is_error: results.iter().all(|result| result.status.is_error()),
        }
    }
}

#[async_trait]
impl Tool for SpawnTool {
    fn name(&self) -> &str {
        "Spawn"
    }

    fn description(&self) -> &str {
        "Spawn one or more sub-agents to handle tasks in parallel. \
         Each sub-agent has its own conversation context and tool access.\n\n\
         - Maximum 5 sub-agents per call.\n\
         - Each sub-agent runs up to 200 model turns with a 4096 token output limit.\n\
         - Use for independent, parallelizable tasks (e.g., searching different modules, \
         running separate analyses).\n\
         - Set persistent=true after TeamCreate to start an addressable teammate that can receive SendMessage calls.\n\
         - Do NOT use ordinary one-shot tasks for work that needs sequential coordination."
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "List of tasks for sub-agents to execute in parallel",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {
                                "type": "string",
                                "description": "Short descriptive name for the task"
                            },
                            "prompt": {
                                "type": "string",
                                "description": "The task description / prompt for the sub-agent"
                            },
                            "agent_type": {
                                "type": "string",
                                "description": "Optional agent definition name from the system prompt"
                            },
                            "task_id": {
                                "type": "string",
                                "description": "Optional existing sub-agent id to resume"
                            },
                            "isolation": {
                                "type": "string",
                                "enum": ["shared", "worktree"],
                                "description": "Run in the shared workspace or an isolated Git worktree"
                            },
                            "persistent": {
                                "type": "boolean",
                                "description": "Keep this named sub-agent available as a Team member after its initial task"
                            }
                        },
                        "required": ["name", "prompt"]
                    }
                }
            },
            "required": ["tasks"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false // manages its own concurrency
    }

    fn is_deferred(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        self.execute_with_cancel(input, CancellationToken::new()).await
    }

    async fn execute_cancellable(&self, input: Value, cancel: CancellationToken) -> ToolResult {
        self.execute_with_cancel(input, cancel).await
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Exec
    }

    fn describe(&self, input: &Value) -> String {
        let names = input
            .get("tasks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|task| task.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if names.is_empty() {
            return "Spawn: 0 sub-tasks".to_string();
        }
        let description = format!("Spawn: {} sub-tasks ({})", names.len(), names.join(", "));
        agentrs_tools::truncate_utf8(&description, 120).to_string()
    }
}

fn parse_tasks(input: &Value, depth: usize) -> Result<Vec<SubAgentSpec>, String> {
    let tasks_arr = input["tasks"].as_array().ok_or("Missing or invalid 'tasks' array")?;

    let mut configs = Vec::new();
    for task in tasks_arr {
        let name = task["name"]
            .as_str()
            .ok_or("Each task must have a 'name' string")?
            .to_string();
        let prompt = task["prompt"]
            .as_str()
            .ok_or("Each task must have a 'prompt' string")?
            .to_string();

        configs.push(SubAgentSpec {
            name,
            agent_type: task.get("agent_type").and_then(Value::as_str).map(str::to_string),
            prompt,
            max_turns: None,
            max_tokens: None,
            system_prompt: None,
            depth,
            resume: task.get("task_id").and_then(Value::as_str).map(SubAgentId::new),
            persistent: match task.get("persistent") {
                None => false,
                Some(value) => value
                    .as_bool()
                    .ok_or("Each task's 'persistent' field must be a boolean")?,
            },
            isolation: match task.get("isolation").and_then(Value::as_str) {
                None | Some("shared") => SubAgentIsolation::Shared,
                Some("worktree") => SubAgentIsolation::Worktree,
                Some(other) => return Err(format!("Invalid sub-agent isolation mode: {other}")),
            },
        });
    }

    Ok(configs)
}

fn render_result(result: &crate::spawner::SubAgentResult) -> String {
    let status = match result.status {
        SubAgentStatus::Finished => "completed",
        SubAgentStatus::Running => "running",
        SubAgentStatus::Idle => "idle",
        SubAgentStatus::Cancelled => "cancelled",
        SubAgentStatus::Pending => "pending",
        SubAgentStatus::Failed => "error",
    };
    let text = agentrs_tools::truncate_utf8(&result.text, 100_000);
    let summary = text.lines().next().unwrap_or_default();
    format!(
        "<subagent id=\"{}\" name=\"{}\" status=\"{}\">\n<summary>{}</summary>\n<result>{}</result>\n<usage turns=\"{}\" input=\"{}\" output=\"{}\" />\n</subagent>",
        escape_xml(result.id.as_str()),
        escape_xml(&result.name),
        status,
        escape_xml(summary),
        escape_xml(text),
        result.turns,
        result.usage.input_tokens,
        result.usage.output_tokens,
    )
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
