use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use agentrs_protocol::events::ToolCategory;
use agentrs_types::team::{TeamError, TeamRuntime};
use agentrs_types::tool::{JsonSchema, ToolResult};

use crate::Tool;
use crate::team::result::failure;

pub const TEAM_CREATE_TOOL_NAME: &str = "TeamCreate";

pub struct TeamCreateTool {
    runtime: Arc<dyn TeamRuntime>,
}

impl TeamCreateTool {
    pub fn new(runtime: Arc<dyn TeamRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for TeamCreateTool {
    fn name(&self) -> &str {
        TEAM_CREATE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Create a named team led by this agent. Only one team can be active at a time; delete the current team before creating another."
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "team_name": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Name for the team. It is normalized for use in agent ids and filesystem paths."
                },
                "description": {
                    "type": "string",
                    "description": "Optional concise purpose of the team."
                },
                "agent_type": {
                    "type": "string",
                    "description": "Optional agent definition associated with the team lead."
                }
            },
            "required": ["team_name"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let Some(name) = input.get("team_name").and_then(Value::as_str) else {
            return failure(TeamError::InvalidName {
                kind: "team",
                reason: "`team_name` must be a string".to_string(),
            });
        };
        if name.trim().is_empty() {
            return failure(TeamError::InvalidName {
                kind: "team",
                reason: "`team_name` cannot be empty".to_string(),
            });
        }
        let description = input.get("description").and_then(Value::as_str);
        let agent_type = input.get("agent_type").and_then(Value::as_str);
        match self.runtime.create_team(name, description, agent_type).await {
            Ok(team) => ToolResult {
                content: format!(
                    "Created team '{}'.\nteam_name: {}\nteam_file_path: {}\nlead_agent_id: {}",
                    team.id,
                    team.id,
                    team.team_file_path.display(),
                    team.lead_agent_id,
                ),
                is_error: false,
            },
            Err(error) => failure(error),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Team
    }

    fn is_deferred(&self) -> bool {
        true
    }

    fn describe(&self, input: &Value) -> String {
        match input.get("team_name").and_then(Value::as_str) {
            Some(name) => format!("Create team {name}"),
            None => "Create team".to_string(),
        }
    }
}

#[cfg(test)]
#[path = "create_tool_test.rs"]
mod create_tool_test;
