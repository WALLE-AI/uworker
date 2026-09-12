//! Tools for creating, deleting, and communicating within an in-process team.

mod create_tool;
mod delete_tool;
mod result;
mod send_tool;

pub use create_tool::{TEAM_CREATE_TOOL_NAME, TeamCreateTool};
pub use delete_tool::{TEAM_DELETE_TOOL_NAME, TeamDeleteTool};
pub use send_tool::{SEND_MESSAGE_TOOL_NAME, SendMessageTool};

#[cfg(test)]
mod test_support;
