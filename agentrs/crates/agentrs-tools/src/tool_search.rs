use async_trait::async_trait;
use serde_json::{Value, json};

use agentrs_protocol::events::ToolCategory;
use agentrs_types::tool::{JsonSchema, ToolDef, ToolResult};

use crate::Tool;
use crate::gating::missing_tool_hint;

/// Built-in tool that searches for deferred tools and loads their full schema.
/// Core tool (never deferred itself) — always available to the LLM.
pub struct ToolSearchTool {
    /// Snapshot of all tool definitions (taken at construction time).
    tool_defs: Vec<ToolDef>,
}

impl ToolSearchTool {
    pub fn new(tool_defs: Vec<ToolDef>) -> Self {
        Self { tool_defs }
    }
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "ToolSearch"
    }

    fn description(&self) -> &str {
        "Search for deferred tools and load their full schema. \
         Use this before calling any deferred tool."
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Tool name or keyword to search for"
                }
            },
            "required": ["query"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let query = input["query"].as_str().unwrap_or("");
        if query.is_empty() {
            return ToolResult {
                content: "Error: query is required".to_string(),
                is_error: true,
            };
        }

        let query_lower = query.to_lowercase();
        let matches: Vec<Value> = self
            .tool_defs
            .iter()
            .filter(|d| d.deferred)
            .filter(|d| {
                d.name.to_lowercase().contains(&query_lower) || d.description.to_lowercase().contains(&query_lower)
            })
            .map(|d| {
                json!({
                    "name": d.name,
                    "description": d.description,
                    "parameters": d.input_schema
                })
            })
            .collect();

        if matches.is_empty() {
            // A gated-off tool looks identical to a typo from here, so say
            // which it is rather than leaving the caller to guess.
            let hint = missing_tool_hint(query, |name| self.tool_defs.iter().any(|def| def.name == name));
            let content = match hint {
                Some(hint) => format!("No deferred tools matching \"{query}\" found. {hint}"),
                None => format!("No deferred tools matching \"{query}\" found."),
            };
            return ToolResult {
                content,
                is_error: false,
            };
        }

        ToolResult {
            content: serde_json::to_string_pretty(&matches).unwrap_or_default(),
            is_error: false,
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }
}

#[cfg(test)]
#[path = "tool_search_test.rs"]
mod tool_search_test;
