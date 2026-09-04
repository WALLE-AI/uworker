use serde::{Deserialize, Serialize};

/// Number of assistant turns without a `TodoWrite` call before the engine
/// nudges the model with a reminder.
const DEFAULT_REMINDER_TURNS: usize = 10;

/// How the agent tracks its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoMode {
    /// A flat checklist replaced whole on every write (`TodoWrite`).
    #[default]
    List,
    /// A task graph with ids, owners and dependencies, edited one item at a
    /// time (`TaskCreate` / `TaskList` / `TaskGet` / `TaskUpdate`).
    ///
    /// Costs four tool descriptions instead of one, so it earns its keep only
    /// when steps genuinely depend on each other rather than merely running in
    /// order.
    Graph,
}

/// Configuration for the `TodoWrite` task checklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoConfig {
    /// Whether the `TodoWrite` tool is registered.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Which tracking tools are registered.
    #[serde(default)]
    pub mode: TodoMode,

    /// Whether several tasks may be `in_progress` at once.
    ///
    /// This single switch drives both the runtime validation and the wording of
    /// the tool description, so the instructions the model reads can never
    /// contradict the rule it is judged against. Leave it off for sequential
    /// work; turn it on for deployments that genuinely fan out.
    #[serde(default)]
    pub allow_parallel_in_progress: bool,

    /// Assistant turns without a `TodoWrite` call before a reminder is
    /// injected, and the minimum spacing between two reminders. `0` disables
    /// reminders entirely.
    #[serde(default = "default_reminder_turns")]
    pub reminder_turns: usize,
}

impl Default for TodoConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            mode: TodoMode::default(),
            allow_parallel_in_progress: false,
            reminder_turns: default_reminder_turns(),
        }
    }
}

// --- Default value functions ---

fn default_true() -> bool {
    true
}

fn default_reminder_turns() -> usize {
    DEFAULT_REMINDER_TURNS
}

#[cfg(test)]
#[path = "todo_test.rs"]
mod todo_test;
