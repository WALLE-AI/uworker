use serde::{Deserialize, Serialize};

/// Terminal UI presentation settings.
///
/// These affect only how the interactive TUI renders a session; nothing here
/// reaches the provider or changes agent behaviour.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TuiConfig {
    /// How reasoning/thinking blocks are shown in the transcript.
    #[serde(default)]
    pub thinking: ThinkingDisplay,
}

/// Presentation mode for model reasoning blocks.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    /// Stream the reasoning live in a bounded window, then leave a one-line
    /// summary in the scrollback once the block ends.
    #[default]
    Collapsed,
    /// Keep the whole reasoning block in the transcript.
    Full,
    /// Drop reasoning entirely.
    Off,
}

impl ThinkingDisplay {
    /// Whether reasoning should be recorded at all.
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether a finished reasoning block shrinks to a summary line.
    pub fn collapses_when_finished(self) -> bool {
        matches!(self, Self::Collapsed)
    }
}

#[cfg(test)]
#[path = "tui_test.rs"]
mod tui_test;
