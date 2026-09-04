use std::collections::HashSet;
use std::sync::Mutex;

use agentrs_types::message::{ContentBlock, Message};
use tracing::{debug, warn};

use crate::todo::item::{RawTodoItem, TodoItem, normalize};
use crate::todo::tool::TODO_WRITE_TOOL_NAME;

/// The task checklist owned by one agent run.
///
/// There is no key: each `AgentEngine` builds its own registry and therefore
/// its own store, so a spawned sub-agent can never observe or overwrite the
/// parent's list. Isolation is structural rather than enforced by a key space.
#[derive(Debug, Default)]
pub struct TodoStore {
    inner: Mutex<Vec<TodoItem>>,
}

impl TodoStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the whole list. The tool has no partial-update path, so this is
    /// the only mutation.
    pub fn replace(&self, todos: Vec<TodoItem>) {
        match self.inner.lock() {
            Ok(mut guard) => *guard = todos,
            // A poisoned lock means a previous holder panicked. The checklist
            // is advisory state, so recover the list rather than propagating.
            Err(poisoned) => *poisoned.into_inner() = todos,
        }
    }

    pub fn snapshot(&self) -> Vec<TodoItem> {
        match self.inner.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self.inner.lock() {
            Ok(guard) => guard.is_empty(),
            Err(poisoned) => poisoned.into_inner().is_empty(),
        }
    }

    pub fn clear(&self) {
        self.replace(Vec::new());
    }

    /// Rebuild the list when a session is resumed or forked.
    ///
    /// Replay of the conversation is preferred because it is a pure function of
    /// the history: a session forked at an earlier turn replays to exactly the
    /// checklist that was live at that turn, with no separate state to trim.
    ///
    /// Replay cannot always answer, though. A full autocompact replaces the
    /// history with a boundary message plus a summary, discarding every
    /// `ToolUse` block along with it. `persisted` is the snapshot written into
    /// the session file for that case. Microcompact is not a problem: it only
    /// blanks `ToolResult` content and leaves the originating call intact.
    pub fn rehydrate(&self, messages: &[Message], persisted: &[TodoItem]) {
        match replay_last_write(messages) {
            Some(todos) => {
                debug!(
                    target: "agentrs_tools",
                    count = todos.len(),
                    "todo list rebuilt from conversation history"
                );
                self.replace(todos);
            }
            None if !persisted.is_empty() => {
                debug!(
                    target: "agentrs_tools",
                    count = persisted.len(),
                    "todo list restored from persisted session snapshot"
                );
                self.replace(persisted.to_vec());
            }
            None => self.clear(),
        }
    }
}

/// The list written by the most recent `TodoWrite` call that did not fail.
fn replay_last_write(messages: &[Message]) -> Option<Vec<TodoItem>> {
    let failed = failed_tool_use_ids(messages);

    for message in messages.iter().rev() {
        for block in message.content.iter().rev() {
            let ContentBlock::ToolUse { id, name, input, .. } = block else {
                continue;
            };
            if name != TODO_WRITE_TOOL_NAME || failed.contains(id.as_str()) {
                continue;
            }
            return parse_todos_input(input);
        }
    }
    None
}

/// Ids of tool calls whose result came back as an error.
///
/// A rejected `TodoWrite` never reached the store, so replaying it would
/// resurrect a list the agent never actually held.
fn failed_tool_use_ids(messages: &[Message]) -> HashSet<&str> {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                is_error: true,
                ..
            } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect()
}

/// Read a `TodoWrite` input payload back into items.
///
/// Parallel in-progress entries are accepted unconditionally here. The list was
/// already validated under whatever policy was configured when it was written,
/// and re-judging history against today's config would silently drop a
/// legitimate checklist after a config change.
fn parse_todos_input(input: &serde_json::Value) -> Option<Vec<TodoItem>> {
    let raw: Vec<RawTodoItem> = match serde_json::from_value(input.get("todos")?.clone()) {
        Ok(raw) => raw,
        Err(error) => {
            warn!(
                target: "agentrs_tools",
                %error,
                "stored TodoWrite call has an unreadable payload; falling back"
            );
            return None;
        }
    };
    match normalize(raw, true) {
        Ok(todos) => Some(todos),
        Err(error) => {
            warn!(
                target: "agentrs_tools",
                %error,
                "stored TodoWrite call failed revalidation; falling back"
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "store_test.rs"]
mod store_test;
