use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};

use agentrs_protocol::events::ToolCategory;
use agentrs_types::team::{AgentId, InboxMessage, Recipient, TeamError, TeamMessageKind, TeamRuntime};
use agentrs_types::tool::{JsonSchema, ToolResult};

use crate::Tool;
use crate::team::result::failure;

pub const SEND_MESSAGE_TOOL_NAME: &str = "SendMessage";

static MESSAGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub struct SendMessageTool {
    runtime: Arc<dyn TeamRuntime>,
}

impl SendMessageTool {
    pub fn new(runtime: Arc<dyn TeamRuntime>) -> Self {
        Self { runtime }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        SEND_MESSAGE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Send a message to one teammate or broadcast to all teammates with `to: \"*\"`. Broadcast cost grows linearly with team size."
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "to": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Teammate name, or `*` to broadcast to every other team member."
                },
                "message": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Message delivered at the receiver's next tool-turn boundary."
                },
                "summary": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Short 5-10 word preview for activity displays."
                },
                "type": {
                    "type": "string",
                    "enum": ["message", "shutdown_request", "shutdown_response"],
                    "description": "Structured message kind; defaults to message."
                },
                "request_id": {
                    "type": "string",
                    "description": "Correlation id for shutdown_request and shutdown_response."
                },
                "approve": {
                    "type": "boolean",
                    "description": "Whether a shutdown_response approves the request."
                }
            },
            "required": ["to", "message", "summary"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let Some(to) = non_empty_string(&input, "to") else {
            return invalid_input("`to` must be a non-empty string");
        };
        let Some(message) = non_empty_string(&input, "message") else {
            return invalid_input("`message` must be a non-empty string");
        };
        let Some(summary) = non_empty_string(&input, "summary") else {
            return invalid_input("`summary` must be a non-empty string");
        };
        let recipient = match Recipient::parse(to) {
            Ok(recipient) => recipient,
            Err(error) => return failure(error),
        };
        let Some(team) = self.runtime.current_team() else {
            return failure(TeamError::NoCurrentTeam);
        };
        let id = format!(
            "{}-{}",
            Utc::now().timestamp_micros(),
            MESSAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let kind = match input.get("type").and_then(Value::as_str).unwrap_or("message") {
            "message" => TeamMessageKind::Text,
            "shutdown_request" => TeamMessageKind::ShutdownRequest {
                request_id: input
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(&id)
                    .to_string(),
            },
            "shutdown_response" => {
                let Some(request_id) = non_empty_string(&input, "request_id") else {
                    return invalid_input("`request_id` is required for shutdown_response");
                };
                let Some(approved) = input.get("approve").and_then(Value::as_bool) else {
                    return invalid_input("`approve` must be a boolean for shutdown_response");
                };
                TeamMessageKind::ShutdownResponse {
                    request_id: request_id.to_string(),
                    approved,
                }
            }
            _ => return invalid_input("`type` must be message, shutdown_request, or shutdown_response"),
        };
        let inbox_message = InboxMessage {
            id,
            from: AgentId::team_lead(&team.id),
            message: message.to_string(),
            summary: summary.to_string(),
            kind,
            sent_at: Utc::now(),
        };
        match self.runtime.send(recipient, inbox_message).await {
            Ok(report) => ToolResult {
                content: if report.broadcast {
                    format!("Broadcast delivered to {} teammate(s).", report.delivered)
                } else {
                    format!("Message delivered to {} teammate(s).", report.delivered)
                },
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
        match input.get("to").and_then(Value::as_str) {
            Some("*") => "Broadcast message to team".to_string(),
            Some(recipient) => format!("Send message to {recipient}"),
            None => "Send team message".to_string(),
        }
    }
}

fn non_empty_string<'a>(input: &'a Value, field: &str) -> Option<&'a str> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn invalid_input(reason: &str) -> ToolResult {
    failure(TeamError::Runtime {
        reason: reason.to_string(),
    })
}

#[cfg(test)]
#[path = "send_tool_test.rs"]
mod send_tool_test;
