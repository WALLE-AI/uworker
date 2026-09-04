use agentrs_protocol::events::TodoSnapshot;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Lifecycle state of a single todo entry.
///
/// Deliberately three states. "Cancelled" is expressed by dropping the entry
/// from the list, which keeps the model-facing vocabulary minimal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    /// The wire form used in the tool schema and in persisted sessions.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

/// One entry of the agent's task checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Imperative form describing the work, e.g. "Run the test suite".
    pub content: String,

    pub status: TodoStatus,

    /// Present-continuous form shown while the entry is in progress, e.g.
    /// "Running the test suite". Optional because requiring it measurably
    /// raises malformed-call rates on smaller local models; renderers fall
    /// back to [`TodoItem::content`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

/// The wire form of one entry, for the protocol and any UI downstream of it.
impl From<&TodoItem> for TodoSnapshot {
    fn from(item: &TodoItem) -> Self {
        Self {
            content: item.content.clone(),
            status: item.status.as_str().to_string(),
            active_form: item.active_form.clone(),
        }
    }
}

/// Snapshot a whole list for publication.
pub fn to_snapshots(todos: &[TodoItem]) -> Vec<TodoSnapshot> {
    todos.iter().map(TodoSnapshot::from).collect()
}

/// Why a model-supplied todo list was rejected.
///
/// Every variant is surfaced to the model verbatim as an error tool result, so
/// the wording has to be enough for it to correct the next call on its own.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum TodoError {
    #[error("invalid todos: `content` must be a non-empty string (item {index})")]
    EmptyContent { index: usize },

    #[error("invalid todos: duplicate content {content:?}")]
    DuplicateContent { content: String },

    #[error(
        "invalid todos: at most one task may be in_progress (got {count}). \
         Mark the task you are actually working on as in_progress and leave the rest pending."
    )]
    TooManyInProgress { count: usize },

    #[error("invalid todos: unknown status {status:?} (expected pending, in_progress or completed)")]
    UnknownStatus { status: String },

    #[error("invalid todos: `{field}` must be a string")]
    NotAString { field: &'static str },

    #[error("invalid todos: `todos` must be an array of objects")]
    MalformedList,
}

/// A todo entry exactly as the model sent it, before normalization.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawTodoItem {
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) status: Option<String>,
    #[serde(default, rename = "activeForm")]
    pub(crate) active_form: Option<String>,
}

/// Counts of each status in a list, for the tool receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TodoCounts {
    pub(crate) pending: usize,
    pub(crate) in_progress: usize,
    pub(crate) completed: usize,
}

impl TodoCounts {
    pub(crate) fn of(todos: &[TodoItem]) -> Self {
        let mut counts = Self {
            pending: 0,
            in_progress: 0,
            completed: 0,
        };
        for todo in todos {
            match todo.status {
                TodoStatus::Pending => counts.pending += 1,
                TodoStatus::InProgress => counts.in_progress += 1,
                TodoStatus::Completed => counts.completed += 1,
            }
        }
        counts
    }
}

fn parse_status(raw: &str) -> Result<TodoStatus, TodoError> {
    match raw {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "completed" => Ok(TodoStatus::Completed),
        other => Err(TodoError::UnknownStatus {
            status: other.to_string(),
        }),
    }
}

/// Validate and canonicalize a model-supplied list.
///
/// Enforces the constraints a JSON Schema cannot express: trimmed non-empty
/// content, no duplicates, and — unless the deployment allows parallel work —
/// at most one in-progress entry. Input order is preserved, because the order
/// the model chose is the plan it intends to follow.
pub(crate) fn normalize(raw: Vec<RawTodoItem>, allow_parallel_in_progress: bool) -> Result<Vec<TodoItem>, TodoError> {
    let mut todos = Vec::with_capacity(raw.len());
    let mut seen = std::collections::HashSet::with_capacity(raw.len());
    let mut in_progress = 0usize;

    for (index, item) in raw.into_iter().enumerate() {
        let content = item
            .content
            .ok_or(TodoError::NotAString { field: "content" })?
            .trim()
            .to_string();
        if content.is_empty() {
            return Err(TodoError::EmptyContent { index });
        }
        if !seen.insert(content.clone()) {
            return Err(TodoError::DuplicateContent { content });
        }

        let status = parse_status(&item.status.ok_or(TodoError::NotAString { field: "status" })?)?;
        if status == TodoStatus::InProgress {
            in_progress += 1;
        }

        // An empty or whitespace-only activeForm carries no information; fold
        // it to None so renderers take the content fallback instead of
        // printing a blank line.
        let active_form = item
            .active_form
            .map(|form| form.trim().to_string())
            .filter(|form| !form.is_empty());

        todos.push(TodoItem {
            content,
            status,
            active_form,
        });
    }

    if !allow_parallel_in_progress && in_progress > 1 {
        return Err(TodoError::TooManyInProgress { count: in_progress });
    }

    Ok(todos)
}

#[cfg(test)]
#[path = "item_test.rs"]
mod item_test;
