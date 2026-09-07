//! Engine-side integration for the agent's task tracking.
//!
//! Holds whichever store the configured mode writes into, decides when the
//! model should be nudged back toward it, and owns the plan's lifecycle across
//! user turns.

use std::sync::Arc;

use agentrs_protocol::events::TodoSnapshot;
use agentrs_tools::task::{TaskStore, to_snapshots as task_snapshots};
use agentrs_tools::todo::{TODO_WRITE_TOOL_NAME, TodoItem, TodoStatus, TodoStore, to_snapshots as todo_snapshots};
use agentrs_types::message::{ContentBlock, Message};
use tracing::{debug, warn};

use crate::task_tools::TASK_TOOL_NAMES;

/// Where the engine reads the agent's current plan from.
///
/// The two modes are alternatives, never both at once, so this is an enum
/// rather than a pair of options that could disagree.
pub(crate) enum PlanSource {
    /// Flat checklist held in memory and replaced whole (`TodoWrite`).
    List(Arc<TodoStore>),
    /// Task graph held on disk and edited per item (`Task*`).
    Graph(Arc<TaskStore>),
}

impl PlanSource {
    fn snapshot(&self) -> Vec<TodoSnapshot> {
        match self {
            Self::List(store) => todo_snapshots(&store.snapshot()),
            Self::Graph(store) => match store.list() {
                Ok(tasks) => task_snapshots(&tasks),
                Err(error) => {
                    // An unreadable graph must not take the turn down; the
                    // model still has TaskList to diagnose it.
                    warn!(target: "agentrs_agent", %error, "task graph could not be read for publication");
                    Vec::new()
                }
            },
        }
    }

    /// Tool names whose use means the model is still tracking its work.
    fn tracking_tools(&self) -> &'static [&'static str] {
        match self {
            Self::List(_) => &[TODO_WRITE_TOOL_NAME],
            Self::Graph(_) => TASK_TOOL_NAMES,
        }
    }

    fn reminder_hint(&self) -> &'static str {
        match self {
            Self::List(_) => "The TodoWrite tool hasn't been used recently.",
            Self::Graph(_) => "The task tools haven't been used recently.",
        }
    }
}

/// Everything the engine needs to keep the plan alive across a run.
pub(crate) struct TodoRuntime {
    source: PlanSource,

    /// Turns without a tracking call before a reminder fires, and the minimum
    /// spacing between reminders. `0` disables reminders.
    reminder_turns: usize,

    /// Assistant turns since the model last used a tracking tool.
    turns_since_write: usize,
    /// Assistant turns since a reminder was last injected.
    turns_since_reminder: usize,

    /// The plan as it was last handed to the output sink, so an unchanged one
    /// does not re-publish on every tool round.
    last_published: Vec<TodoSnapshot>,
}

impl TodoRuntime {
    pub(crate) fn new(source: PlanSource, reminder_turns: usize) -> Self {
        Self {
            source,
            reminder_turns,
            turns_since_write: 0,
            turns_since_reminder: 0,
            last_published: Vec::new(),
        }
    }

    /// The checklist items, when the flat mode is active.
    ///
    /// Graph tasks live on disk and are not mirrored into the session file, so
    /// there is nothing to hand back for them.
    pub(crate) fn checklist(&self) -> Vec<TodoItem> {
        match &self.source {
            PlanSource::List(store) => store.snapshot(),
            PlanSource::Graph(_) => Vec::new(),
        }
    }

    /// Rebuild the checklist from a resumed or forked session.
    ///
    /// Only meaningful in list mode: the graph is already durable on disk and
    /// is not reconstructed from the conversation.
    pub(crate) fn rehydrate(&self, messages: &[Message], persisted: &[TodoItem]) {
        if let PlanSource::List(store) = &self.source {
            store.rehydrate(messages, persisted);
        }
    }

    /// Called when a new user message opens a turn.
    ///
    /// A checklist whose entries are all completed has served its purpose, so
    /// it is retired here rather than the moment the last box is ticked. That
    /// timing is what lets the user see the finished, all-green list at the end
    /// of the turn that finished it.
    ///
    /// An unfinished list survives: the user course-correcting mid-plan must
    /// not silently lose the agent's outstanding work. Graph tasks are never
    /// retired automatically — they are addressable by id, so dropping one
    /// behind the model's back would strand every dependency naming it.
    pub(crate) fn on_user_turn_start(&self) {
        let PlanSource::List(store) = &self.source else {
            return;
        };
        let todos = store.snapshot();
        if !todos.is_empty() && todos.iter().all(|todo| todo.status == TodoStatus::Completed) {
            debug!(
                target: "agentrs_agent",
                count = todos.len(),
                "retiring a fully completed todo list at the start of a new turn"
            );
            store.clear();
        }
    }

    /// Account for one completed assistant turn.
    pub(crate) fn record_turn(&mut self, tool_calls: &[ContentBlock]) {
        if self.reminder_turns == 0 {
            return;
        }
        self.turns_since_reminder = self.turns_since_reminder.saturating_add(1);
        if calls_tracking_tool(tool_calls, self.source.tracking_tools()) {
            self.turns_since_write = 0;
        } else {
            self.turns_since_write = self.turns_since_write.saturating_add(1);
        }
    }

    /// The current plan, but only when it differs from the last published one.
    ///
    /// Starting from an empty baseline means a session that never tracks
    /// anything publishes nothing at all.
    pub(crate) fn take_changed_snapshot(&mut self) -> Option<Vec<TodoSnapshot>> {
        let current = self.source.snapshot();
        if current == self.last_published {
            return None;
        }
        self.last_published = current.clone();
        Some(current)
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
            "injecting task tracking reminder"
        );
        Some(render_reminder(self.source.reminder_hint(), &self.source.snapshot()))
    }
}

fn calls_tracking_tool(tool_calls: &[ContentBlock], tracked: &[&str]) -> bool {
    tool_calls
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { name, .. } if tracked.contains(&name.as_str())))
}

/// The reminder body, including the current plan so the model can judge whether
/// it has gone stale rather than merely being told that it might have.
fn render_reminder(hint: &str, plan: &[TodoSnapshot]) -> String {
    let mut text = format!(
        "<system-reminder>\n\
         {hint} If you're working on tasks that would benefit from tracking progress, consider \
         using it. Also consider cleaning up the list if it has become stale and no longer \
         matches what you are working on. Only use it if it's relevant to the current work. This \
         is just a gentle reminder — ignore it if it doesn't apply. Never mention this reminder \
         to the user.\n"
    );

    if plan.is_empty() {
        text.push_str("\nYour task list is currently empty.\n");
    } else {
        text.push_str("\nYour current task list:\n");
        for (index, entry) in plan.iter().enumerate() {
            text.push_str(&format!("{}. [{}] {}\n", index + 1, entry.status, entry.content));
        }
    }

    text.push_str("</system-reminder>");
    text
}

#[cfg(test)]
#[path = "todo_reminder_test.rs"]
mod todo_reminder_test;
