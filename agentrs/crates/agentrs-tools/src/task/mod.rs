//! The task-graph tools and the store behind them.
//!
//! An alternative to the flat [`crate::todo`] checklist for work whose steps
//! genuinely depend on each other: tasks carry ids, owners, and `blocks` /
//! `blocked_by` edges, and are edited one at a time rather than replaced whole.

mod model;
mod prompt;
mod store;
mod tools;

pub use model::{Task, TaskStatus};
pub use store::{TaskStore, task_dir};
pub use tools::{
    TASK_CREATE_TOOL_NAME, TASK_GET_TOOL_NAME, TASK_LIST_TOOL_NAME, TASK_UPDATE_TOOL_NAME, TaskCreateTool, TaskGetTool,
    TaskListTool, TaskUpdateTool,
};
