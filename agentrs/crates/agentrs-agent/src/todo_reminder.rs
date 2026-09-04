//! Engine-side integration for the `TodoWrite` checklist.
//!
//! Holds the store the tool writes into, decides when the model should be
//! nudged back toward it, and owns the list's lifecycle across user turns.

use std::sync::Arc;

use agentrs_tools::todo::{TODO_WRITE_TOOL_NAME, TodoItem, TodoStatus, TodoStore};
use agentrs_types::message::{ContentBlock, Message};
use tracing::debug;

/// Everything the engine needs to keep the checklist alive across a run.
pub(crate) struct TodoRuntime {
    store: Arc<TodoStore>,

    /// Turns without a `TodoWrite` call before a reminder fires, and the
    /// minimum spacing between reminders. `0` disables reminders.
    reminder_turns: usize,

    /// Assistant turns since the model last called `TodoWrite`.
    turns_since_write: usize,
    /// Assistant turns since a reminder was last injected.
    turns_since_reminder: usize,

    /// The list as it was last handed to the output sink, so an unchanged
    /// checklist does not re-publish on every tool round.
    last_published: Vec<TodoItem>,
}

impl TodoRuntime {
    pub(crate) fn new(store: Arc<TodoStore>, reminder_turns: usize) -> Self {
        Self {
            store,
            reminder_turns,
            turns_since_write: 0,
            turns_since_reminder: 0,
            last_published: Vec::new(),
        }
    }

    /// The current list, but only when it differs from the last published one.
    ///
    /// Starting from an empty baseline means a session that never touches the
    /// checklist publishes nothing at all.
    pub(crate) fn take_changed_snapshot(&mut self) -> Option<Vec<TodoItem>> {
        let current = self.store.snapshot();
        if current == self.last_published {
            return None;
        }
        self.last_published = current.clone();
        Some(current)
    }

    pub(crate) fn snapshot(&self) -> Vec<TodoItem> {
        self.store.snapshot()
    }

    /// Rebuild the checklist from a resumed or forked session.
    pub(crate) fn rehydrate(&self, messages: &[Message], persisted: &[TodoItem]) {
        self.store.rehydrate(messages, persisted);
    }

    /// Called when a new user message opens a turn.
    ///
    /// A checklist whose entries are all completed has served its purpose, so
    /// it is retired here rather than the moment the last box is ticked. That
    /// timing is what lets the user see the finished, all-green list at the end
    /// of the turn that finished it.
    ///
    /// An unfinished list survives: the user course-correcting mid-plan must
    /// not silently lose the agent's outstanding work.
    pub(crate) fn on_user_turn_start(&self) {
        let todos = self.store.snapshot();
        if !todos.is_empty() && todos.iter().all(|todo| todo.status == TodoStatus::Completed) {
            debug!(
                target: "agentrs_agent",
                count = todos.len(),
                "retiring a fully completed todo list at the start of a new turn"
            );
            self.store.clear();
        }
    }

    /// Account for one completed assistant turn.
    pub(crate) fn record_turn(&mut self, tool_calls: &[ContentBlock]) {
        if self.reminder_turns == 0 {
            return;
        }
        self.turns_since_reminder = self.turns_since_reminder.saturating_add(1);
        if calls_todo_write(tool_calls) {
            self.turns_since_write = 0;
        } else {
            self.turns_since_write = self.turns_since_write.saturating_add(1);
        }
    }

    /// The reminder to inject into the next request, if one is due.
    ///
    /// Consuming: the caller injects the text as an ephemeral message that is
    /// never written back to history, so the spacing counter is the only record
    /// that a reminder was delivered.
    pub(crate) fn take_reminder(&mut self) -> Option<String> {
        if self.reminder_turns == 0
            || self.turns_since_write < self.reminder_turns
            || self.turns_since_reminder < self.reminder_turns
        {
            return None;
        }
        self.turns_since_reminder = 0;
        debug!(
            target: "agentrs_agent",
            turns_since_write = self.turns_since_write,
            "injecting todo reminder"
        );
        Some(render_reminder(&self.store.snapshot()))
    }
}

fn calls_todo_write(tool_calls: &[ContentBlock]) -> bool {
    tool_calls
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { name, .. } if name == TODO_WRITE_TOOL_NAME))
}

/// The reminder body, including the current list so the model can judge whether
/// the checklist has gone stale rather than merely being told that it might have.
fn render_reminder(todos: &[TodoItem]) -> String {
    let mut text = String::from(
        "<system-reminder>\n\
         The TodoWrite tool hasn't been used recently. If you're working on tasks that would \
         benefit from tracking progress, consider using it. Also consider cleaning up the list \
         if it has become stale and no longer matches what you are working on. Only use it if \
         it's relevant to the current work. This is just a gentle reminder — ignore it if it \
         doesn't apply. Never mention this reminder to the user.\n",
    );

    if todos.is_empty() {
        text.push_str("\nYour todo list is currently empty.\n");
    } else {
        text.push_str("\nYour current todo list:\n");
        for (index, todo) in todos.iter().enumerate() {
            text.push_str(&format!("{}. [{}] {}\n", index + 1, todo.status.as_str(), todo.content));
        }
    }

    text.push_str("</system-reminder>");
    text
}

#[cfg(test)]
#[path = "todo_reminder_test.rs"]
mod todo_reminder_test;
