//! Names of the task-graph tools, in one place.
//!
//! The engine needs these to tell "the model is still tracking its work" from
//! "the model has gone quiet"; they live here rather than in the reminder so
//! adding a fifth task tool is a one-line change with an obvious home.

use agentrs_tools::task::{TASK_CREATE_TOOL_NAME, TASK_GET_TOOL_NAME, TASK_LIST_TOOL_NAME, TASK_UPDATE_TOOL_NAME};

/// Every tool that counts as the model maintaining its task graph.
pub(crate) const TASK_TOOL_NAMES: &[&str] = &[
    TASK_CREATE_TOOL_NAME,
    TASK_LIST_TOOL_NAME,
    TASK_GET_TOOL_NAME,
    TASK_UPDATE_TOOL_NAME,
];
