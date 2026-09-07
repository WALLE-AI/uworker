use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::warn;

use agentrs_protocol::events::ToolCategory;
use agentrs_types::tool::{JsonSchema, ToolResult};

use crate::Tool;
use crate::task::model::{Task, TaskDraft, TaskError, TaskPatch, TaskStatus};
use crate::task::prompt;
use crate::task::store::TaskStore;

pub const TASK_CREATE_TOOL_NAME: &str = "TaskCreate";
pub const TASK_LIST_TOOL_NAME: &str = "TaskList";
pub const TASK_GET_TOOL_NAME: &str = "TaskGet";
pub const TASK_UPDATE_TOOL_NAME: &str = "TaskUpdate";

/// Every task tool answers in the same shape, so the model reads one format
/// regardless of which call produced it.
fn render(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "No tasks.".to_string();
    }
    let mut text = String::new();
    for task in tasks {
        text.push_str(&format!("#{} [{}] {}", task.id, task.status.as_str(), task.subject));
        if let Some(owner) = &task.owner {
            text.push_str(&format!(" (owner: {owner})"));
        }
        if !task.blocked_by.is_empty() {
            text.push_str(&format!(" (blocked by: {})", task.blocked_by.join(", ")));
        }
        if !task.description.is_empty() {
            text.push_str(&format!("\n    {}", task.description));
        }
        text.push('\n');
    }
    text.trim_end().to_string()
}

fn failure(error: TaskError) -> ToolResult {
    warn!(target: "agentrs_tools", kind = %error_kind(&error), "task operation rejected");
    ToolResult {
        content: error.to_string(),
        is_error: true,
    }
}

/// Stable label for logs. Never carries task text: production logs must not
/// include tool input.
fn error_kind(error: &TaskError) -> &'static str {
    match error {
        TaskError::NotFound { .. } => "not_found",
        TaskError::EmptySubject => "empty_subject",
        TaskError::Blocked { .. } => "blocked",
        TaskError::SelfDependency { .. } => "self_dependency",
        TaskError::DependencyCycle { .. } => "dependency_cycle",
        TaskError::UnknownStatus { .. } => "unknown_status",
        TaskError::Storage { .. } => "storage",
    }
}

fn parse_status(raw: &str) -> Result<TaskStatus, TaskError> {
    match raw {
        "pending" => Ok(TaskStatus::Pending),
        "in_progress" => Ok(TaskStatus::InProgress),
        "completed" => Ok(TaskStatus::Completed),
        other => Err(TaskError::UnknownStatus {
            status: other.to_string(),
        }),
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// TaskCreate
// ---------------------------------------------------------------------------

/// Adds tasks to the graph.
pub struct TaskCreateTool {
    store: Arc<TaskStore>,
}

impl TaskCreateTool {
    pub fn new(store: Arc<TaskStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &str {
        TASK_CREATE_TOOL_NAME
    }

    fn description(&self) -> &str {
        prompt::CREATE
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "description": "Tasks to add, in the order you plan to do them.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "subject": {
                                "type": "string",
                                "description": "One-line imperative summary of the task."
                            },
                            "description": {
                                "type": "string",
                                "description": "Optional detail that does not fit in the subject."
                            },
                            "activeForm": {
                                "type": "string",
                                "description": "Present continuous form shown while the task is in progress."
                            },
                            "owner": {
                                "type": "string",
                                "description": "Optional agent responsible for the task."
                            },
                            "blockedBy": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Ids of tasks that must finish first. May name a task created earlier in this same call."
                            }
                        },
                        "required": ["subject"]
                    }
                }
            },
            "required": ["tasks"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let Some(items) = input.get("tasks").and_then(Value::as_array) else {
            return failure(TaskError::Storage {
                reason: "`tasks` must be an array".to_string(),
            });
        };

        let drafts: Vec<TaskDraft> = items
            .iter()
            .map(|item| TaskDraft {
                subject: item
                    .get("subject")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                description: item
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                active_form: item.get("activeForm").and_then(Value::as_str).map(str::to_string),
                owner: item.get("owner").and_then(Value::as_str).map(str::to_string),
                blocked_by: string_list(item.get("blockedBy")),
            })
            .collect();

        match self.store.create(drafts) {
            Ok(created) => ToolResult {
                content: format!("Created {} task(s):\n{}", created.len(), render(&created)),
                is_error: false,
            },
            Err(error) => failure(error),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }

    fn describe(&self, input: &Value) -> String {
        let count = input.get("tasks").and_then(Value::as_array).map_or(0, Vec::len);
        format!("Create {count} task(s)")
    }
}

// ---------------------------------------------------------------------------
// TaskList
// ---------------------------------------------------------------------------

/// Reads the whole graph, optionally filtered.
pub struct TaskListTool {
    store: Arc<TaskStore>,
}

impl TaskListTool {
    pub fn new(store: Arc<TaskStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TaskListTool {
    fn name(&self) -> &str {
        TASK_LIST_TOOL_NAME
    }

    fn description(&self) -> &str {
        prompt::LIST
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "Only return tasks in this state."
                },
                "owner": {
                    "type": "string",
                    "description": "Only return tasks with this owner."
                }
            }
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let status = match input.get("status").and_then(Value::as_str).map(parse_status) {
            Some(Ok(status)) => Some(status),
            Some(Err(error)) => return failure(error),
            None => None,
        };
        let owner = input.get("owner").and_then(Value::as_str);

        match self.store.list() {
            Ok(tasks) => {
                let filtered: Vec<Task> = tasks
                    .into_iter()
                    .filter(|task| status.is_none_or(|wanted| task.status == wanted))
                    .filter(|task| owner.is_none_or(|wanted| task.owner.as_deref() == Some(wanted)))
                    .collect();
                ToolResult {
                    content: render(&filtered),
                    is_error: false,
                }
            }
            Err(error) => failure(error),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }

    fn describe(&self, _input: &Value) -> String {
        "List tasks".to_string()
    }
}

// ---------------------------------------------------------------------------
// TaskGet
// ---------------------------------------------------------------------------

/// Reads one task, including its dependencies.
pub struct TaskGetTool {
    store: Arc<TaskStore>,
}

impl TaskGetTool {
    pub fn new(store: Arc<TaskStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TaskGetTool {
    fn name(&self) -> &str {
        TASK_GET_TOOL_NAME
    }

    fn description(&self) -> &str {
        prompt::GET
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "taskId": { "type": "string", "description": "Id of the task to read." }
            },
            "required": ["taskId"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let Some(id) = input.get("taskId").and_then(Value::as_str) else {
            return failure(TaskError::Storage {
                reason: "`taskId` must be a string".to_string(),
            });
        };

        match self.store.get(id) {
            Ok(Some(task)) => {
                let mut content = render(std::slice::from_ref(&task));
                if !task.blocks.is_empty() {
                    content.push_str(&format!("\n    blocks: {}", task.blocks.join(", ")));
                }
                ToolResult {
                    content,
                    is_error: false,
                }
            }
            Ok(None) => failure(TaskError::NotFound { id: id.to_string() }),
            Err(error) => failure(error),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }

    fn describe(&self, input: &Value) -> String {
        match input.get("taskId").and_then(Value::as_str) {
            Some(id) => format!("Read task #{id}"),
            None => "Read task".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// TaskUpdate
// ---------------------------------------------------------------------------

/// Edits or deletes one task.
pub struct TaskUpdateTool {
    store: Arc<TaskStore>,
}

impl TaskUpdateTool {
    pub fn new(store: Arc<TaskStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &str {
        TASK_UPDATE_TOOL_NAME
    }

    fn description(&self) -> &str {
        prompt::UPDATE
    }

    fn input_schema(&self) -> JsonSchema {
        json!({
            "type": "object",
            "properties": {
                "taskId": { "type": "string", "description": "Id of the task to change." },
                "subject": { "type": "string", "description": "New one-line summary." },
                "description": { "type": "string", "description": "New detail." },
                "activeForm": {
                    "type": "string",
                    "description": "Present continuous form shown while the task is in progress."
                },
                "owner": { "type": "string", "description": "Agent responsible for the task." },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "New state. Rejected while an unfinished task still blocks this one."
                },
                "addBlockedBy": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Ids of tasks that must finish before this one."
                },
                "removeBlockedBy": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Dependency ids to drop."
                },
                "delete": {
                    "type": "boolean",
                    "description": "Remove the task entirely, along with every dependency naming it."
                }
            },
            "required": ["taskId"]
        })
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        false
    }

    async fn execute(&self, input: Value) -> ToolResult {
        let Some(id) = input.get("taskId").and_then(Value::as_str) else {
            return failure(TaskError::Storage {
                reason: "`taskId` must be a string".to_string(),
            });
        };

        if input.get("delete").and_then(Value::as_bool).unwrap_or(false) {
            return match self.store.delete(id) {
                Ok(()) => ToolResult {
                    content: format!("Deleted task #{id}."),
                    is_error: false,
                },
                Err(error) => failure(error),
            };
        }

        let status = match input.get("status").and_then(Value::as_str).map(parse_status) {
            Some(Ok(status)) => Some(status),
            Some(Err(error)) => return failure(error),
            None => None,
        };

        let patch = TaskPatch {
            subject: input.get("subject").and_then(Value::as_str).map(str::to_string),
            description: input.get("description").and_then(Value::as_str).map(str::to_string),
            active_form: input.get("activeForm").and_then(Value::as_str).map(str::to_string),
            owner: input.get("owner").and_then(Value::as_str).map(str::to_string),
            status,
            add_blocked_by: string_list(input.get("addBlockedBy")),
            remove_blocked_by: string_list(input.get("removeBlockedBy")),
        };

        if patch.is_empty() {
            return failure(TaskError::Storage {
                reason: "nothing to update: supply at least one field to change, or `delete: true`".to_string(),
            });
        }

        match self.store.update(id, patch) {
            Ok(task) => ToolResult {
                content: format!("Updated:\n{}", render(std::slice::from_ref(&task))),
                is_error: false,
            },
            Err(error) => failure(error),
        }
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }

    fn describe(&self, input: &Value) -> String {
        match input.get("taskId").and_then(Value::as_str) {
            Some(id) => format!("Update task #{id}"),
            None => "Update task".to_string(),
        }
    }
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tools_test;
