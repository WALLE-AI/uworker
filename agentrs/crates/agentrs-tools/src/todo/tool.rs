use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{debug, warn};

use agentrs_protocol::events::ToolCategory;
use agentrs_types::tool::{JsonSchema, ToolResult};

use crate::Tool;
use crate::todo::item::{RawTodoItem, TodoCounts, TodoError, TodoItem, normalize};
use crate::todo::prompt::description;
use crate::todo::store::TodoStore;

/// Name advertised to the model. Also the key `TodoStore::rehydrate` looks for
/// when replaying history, so the two must not drift.
pub const TODO_WRITE_TOOL_NAME: &str = "TodoWrite";

/// Whole-list replacement of the session task checklist.
pub struct TodoWriteTool {
    store: Arc<TodoStore>,
    allow_parallel_in_progress: bool,
    /// Rendered once at construction: the text is fixed for the lifetime of the
    /// tool and `Tool::description` has to hand back a borrow.
    description: String,
}

impl TodoWriteTool {
    pub fn new(store: Arc<TodoStore>, allow_parallel_in_progress: bool) -> Self {
        Self {
            store,
            allow_parallel_in_progress,
            description: description(allow_parallel_in_progress),
        }
    }

    fn parse(&self, input: &Value) -> Result<Vec<TodoItem>, TodoError> {
        let todos = input.get("todos").ok_or(TodoError::MalformedList)?;
        let raw: Vec<RawTodoItem> = serde_json::from_value(todos.clone()).map_err(|_| TodoError::MalformedList)?;
        normalize(raw, self.allow_parallel_in_progress)
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        TODO_WRITE_TOOL_NAME
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The COMPLETE task list, replacing any previous list.",
                    "items": {
                        "type": "object",
                        // Refusing unknown keys is deliberate: the stored snapshot
                        // must equal what the model believes it wrote, so an
                        // extended item shape fails loudly at the schema boundary
                        // instead of being silently flattened.
                        "additionalProperties": false,
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "What the task is, as a short imperative line."
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "pending (not started) | in_progress (now) | completed (done)."
                            },
                            "activeForm": {
                                "type": "string",
                                "description": "Present continuous form shown while the task is in progress."
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    /// Whole-list replacement is order-sensitive: two calls resolved in an
    /// arbitrary order would leave a nondeterministic list. The tool does no
    /// I/O, so serializing it costs nothing worth recovering.
    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    /// Never deferred. A checklist the model has to go looking for is a
    /// checklist it will not use.
    fn is_deferred(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let todos = match self.parse(&input) {
            Ok(todos) => todos,
            Err(error) => {
                warn!(target: "agentrs_tools", kind = %error_kind(&error), "TodoWrite rejected");
                return ToolResult {
                    content: error.to_string(),
                    is_error: true,
                };
            }
        };

        let counts = TodoCounts::of(&todos);
        debug!(
            target: "agentrs_tools",
            total = todos.len(),
            pending = counts.pending,
            in_progress = counts.in_progress,
            completed = counts.completed,
            "todo list updated"
        );
        self.store.replace(todos);

        ToolResult {
            content: format!(
                "Updated todo list: {} pending, {} in progress, {} completed.\n\
                 Continue to use TodoWrite as you make progress. Proceed with the current task.",
                counts.pending, counts.in_progress, counts.completed
            ),
            is_error: false,
        }
    }

    fn category(&self) -> ToolCategory {
        // Touches neither the filesystem nor the network. Info also makes the
        // checklist available while plan mode restricts the tool set.
        ToolCategory::Info
    }

    fn describe(&self, input: &Value) -> String {
        let Ok(todos) = self.parse(input) else {
            return "Update todo list".to_string();
        };
        let counts = TodoCounts::of(&todos);
        format!(
            "Update todo list ({} items, {} in progress)",
            todos.len(),
            counts.in_progress
        )
    }
}

/// Stable label for logs. Never includes task text: production logs must not
/// carry tool input.
fn error_kind(error: &TodoError) -> &'static str {
    match error {
        TodoError::EmptyContent { .. } => "empty_content",
        TodoError::DuplicateContent { .. } => "duplicate_content",
        TodoError::TooManyInProgress { .. } => "too_many_in_progress",
        TodoError::UnknownStatus { .. } => "unknown_status",
        TodoError::NotAString { .. } => "not_a_string",
        TodoError::MalformedList => "malformed_list",
    }
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tool_test;
