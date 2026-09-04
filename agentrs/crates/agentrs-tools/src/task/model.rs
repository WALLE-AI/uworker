use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::todo::TodoStatus;

/// Lifecycle state of a task.
///
/// The same three states as the flat checklist, reused rather than redefined so
/// a deployment can move between the two modes without the model relearning the
/// vocabulary.
pub type TaskStatus = TodoStatus;

/// One node of the task graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Stable identifier, assigned by the store and never reused.
    pub id: String,

    /// One-line imperative summary, e.g. "Add the --verbose flag".
    pub subject: String,

    /// Longer detail. Empty when the subject says everything.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    /// Present-continuous form shown while the task is in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,

    /// Agent currently responsible for the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    pub status: TaskStatus,

    /// Tasks that cannot start until this one is done. Mirror of `blocked_by`;
    /// the store keeps the two sides consistent so neither can drift.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,

    /// Tasks that must finish before this one may start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

impl Task {
    /// Whether the task still needs work.
    pub fn is_open(&self) -> bool {
        self.status != TaskStatus::Completed
    }

    /// Text to show while this task is the active one.
    pub fn display_active(&self) -> &str {
        self.active_form.as_deref().unwrap_or(&self.subject)
    }
}

/// A task as the model asks for it, before the store assigns an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDraft {
    pub subject: String,
    pub description: String,
    pub active_form: Option<String>,
    pub owner: Option<String>,
    pub blocked_by: Vec<String>,
}

/// The fields one `TaskUpdate` call may change.
///
/// Every field is optional and `None` means "leave alone", so an update that
/// touches one field cannot silently blank the others.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPatch {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub owner: Option<String>,
    pub status: Option<TaskStatus>,
    pub add_blocked_by: Vec<String>,
    pub remove_blocked_by: Vec<String>,
}

impl TaskPatch {
    pub fn is_empty(&self) -> bool {
        self.subject.is_none()
            && self.description.is_none()
            && self.active_form.is_none()
            && self.owner.is_none()
            && self.status.is_none()
            && self.add_blocked_by.is_empty()
            && self.remove_blocked_by.is_empty()
    }
}

/// Why a task operation was refused.
///
/// Each message is handed to the model verbatim, so it has to say what to do
/// next rather than merely what went wrong.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaskError {
    #[error("task {id} not found. Call TaskList to see the current tasks.")]
    NotFound { id: String },

    #[error("invalid task: `subject` must be a non-empty string")]
    EmptySubject,

    #[error(
        "task {id} cannot be {status} while it is blocked by {blockers}. \
         Finish those tasks first, or remove the dependency with remove_blocked_by."
    )]
    Blocked {
        id: String,
        status: &'static str,
        blockers: String,
    },

    #[error("task {id} cannot block itself")]
    SelfDependency { id: String },

    #[error(
        "adding that dependency would create a cycle: {cycle}. \
         A task cannot end up waiting on itself."
    )]
    DependencyCycle { cycle: String },

    #[error("unknown status {status:?} (expected pending, in_progress or completed)")]
    UnknownStatus { status: String },

    #[error("task storage is unavailable: {reason}")]
    Storage { reason: String },
}

/// Format a list of ids for an error message.
pub(crate) fn join_ids(ids: &[String]) -> String {
    ids.iter().map(String::as_str).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
#[path = "model_test.rs"]
mod model_test;
