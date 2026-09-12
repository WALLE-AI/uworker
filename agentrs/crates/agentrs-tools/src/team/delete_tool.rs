use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use agentrs_protocol::events::ToolCategory;
use agentrs_types::team::TeamRuntime;
use agentrs_types::tool::{JsonSchema, ToolResult};

use crate::Tool;
use crate::team::result::failure;

pub const TEAM_DELETE_TOOL_NAME: &str = "TeamDelete";

pub struct TeamDeleteTool {
    runtime: Arc<dyn TeamRuntime>,
}

impl TeamDeleteTool {
    pub fn new(runtime: Arc<dyn TeamRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for TeamDeleteTool {
    fn name(&self) -> &str {
        TEAM_DELETE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delete the current team after all non-lead members have shut down. The operation is idempotent when no team exists."
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, _input: Value) -> ToolResult {
        match self.runtime.delete_team().await {
            Ok(report) if report.deleted => ToolResult {
                content: match report.team {
                    Some(team) => format!("Deleted team '{team}'."),
                    None => "Deleted the current team.".to_string(),
                },
                is_error: false,
            },
            Ok(_) => ToolResult {
                content: "No active team; nothing to clean up.".to_string(),
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

    fn describe(&self, _input: &Value) -> String {
        "Delete current team".to_string()
    }
}

#[cfg(test)]
#[path = "delete_tool_test.rs"]
mod delete_tool_test;
