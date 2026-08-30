//! Pure event projection used by both live rendering and durable replay.

use std::collections::BTreeSet;

use agentrs_contracts::event::{EventPayload, RunEventEnvelope};
use agentrs_contracts::ids::{EventId, RunId};
use agentrs_types::{ContentBlock, Message, Role, TokenUsage};

use crate::sanitize::terminal_text;

/// Coarse run state shown in the header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunStatus {
    /// No run has been submitted yet.
    #[default]
    Idle,
    /// Runtime is active.
    Running,
    /// Runtime is waiting for human approval.
    AwaitingApproval,
    /// Runtime completed normally.
    Completed,
    /// Runtime was canceled.
    Canceled,
    /// Runtime needs recovery or another explicit action.
    NeedsAction,
    /// Runtime failed.
    Failed,
}

impl RunStatus {
    /// Stable, compact status label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Running => "RUNNING",
            Self::AwaitingApproval => "APPROVAL",
            Self::Completed => "COMPLETED",
            Self::Canceled => "CANCELED",
            Self::NeedsAction => "NEEDS ACTION",
            Self::Failed => "FAILED",
        }
    }
}

/// A committed transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// Message role.
    pub role: Role,
    /// Sanitized visible text.
    pub text: String,
}

/// A compact tool lifecycle row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRow {
    /// Tool call or execution identifier.
    pub id: String,
    /// Current lifecycle label.
    pub state: &'static str,
    /// Sanitized detail suitable for terminal display.
    pub detail: String,
}

/// Entire deterministic UI projection.
#[derive(Debug, Default)]
pub struct AppState {
    /// Current run identifier.
    pub run_id: Option<RunId>,
    /// Runtime status.
    pub status: RunStatus,
    /// Committed Surface transcript.
    pub transcript: Vec<TranscriptEntry>,
    /// Uncommitted live assistant text.
    pub streaming: String,
    /// Tool activity, newest last.
    pub tools: Vec<ToolRow>,
    /// Provider token usage once the run summary arrives.
    pub usage: TokenUsage,
    /// Live events dropped by the bounded sink.
    pub dropped_live: u64,
    /// Latest host-facing error or recovery diagnostic.
    pub notice: Option<String>,
    seen_durable: BTreeSet<EventId>,
}

impl AppState {
    /// Applies one runtime event. Duplicate durable event IDs are ignored.
    pub fn apply(&mut self, event: &RunEventEnvelope) {
        if event.is_durable() && !self.seen_durable.insert(event.event_id.clone()) {
            return;
        }
        self.run_id = Some(event.run_id.clone());

        match &event.payload {
            EventPayload::RunStarted => self.status = RunStatus::Running,
            EventPayload::RunCompleted => self.status = RunStatus::Completed,
            EventPayload::RunCanceled => self.status = RunStatus::Canceled,
            EventPayload::RunNeedsUserAction => self.status = RunStatus::NeedsAction,
            EventPayload::RunFailed => self.status = RunStatus::Failed,
            EventPayload::TextDelta { text } => self.streaming.push_str(&terminal_text(text)),
            EventPayload::SurfaceMessageRecorded { message } => {
                if let Ok(message) = serde_json::from_value::<Message>(message.clone()) {
                    self.commit_message(message);
                }
            }
            EventPayload::ToolProposed { call_id } => self.tools.push(ToolRow {
                id: call_id.to_string(),
                state: "PROPOSED",
                detail: String::new(),
            }),
            EventPayload::ApprovalRequested { call_id } => {
                self.status = RunStatus::AwaitingApproval;
                self.update_tool(&call_id.to_string(), "APPROVAL", "waiting for user");
            }
            EventPayload::ToolStarted { call_id } => {
                self.status = RunStatus::Running;
                self.update_tool(&call_id.to_string(), "RUNNING", "");
            }
            EventPayload::StepIntentRecorded { intent } => self.update_tool(
                &intent.call_id.to_string(),
                "INTENT",
                &format!("{} execution={}", intent.tool_name, intent.execution_id),
            ),
            EventPayload::StepResultRecorded { result } => self.update_tool(
                &result.call_id.to_string(),
                "DONE",
                &format!("{:?}", result.outcome),
            ),
            EventPayload::ApprovalTimedOut { call_id, .. } => {
                self.status = RunStatus::NeedsAction;
                self.update_tool(&call_id.to_string(), "TIMED OUT", "approval expired");
            }
            _ => {}
        }
    }

    /// Replays a durable event prefix through the same reducer used for live data.
    pub fn replay<'a>(&mut self, events: impl IntoIterator<Item = &'a RunEventEnvelope>) {
        for event in events {
            if event.is_durable() {
                self.apply(event);
            }
        }
    }

    fn commit_message(&mut self, message: Message) {
        let text = message
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text),
                ContentBlock::ToolUse { name, input, .. } => Some(format!("{name} {input}")),
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => Some(if is_error {
                    format!("error: {content}")
                } else {
                    content
                }),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            if message.role == Role::Assistant {
                self.streaming.clear();
            }
            self.transcript.push(TranscriptEntry {
                role: message.role,
                text: terminal_text(&text),
            });
        }
    }

    fn update_tool(&mut self, id: &str, state: &'static str, detail: &str) {
        if let Some(row) = self.tools.iter_mut().rev().find(|row| row.id == id) {
            row.state = state;
            row.detail = terminal_text(detail);
        } else {
            self.tools.push(ToolRow {
                id: id.to_string(),
                state,
                detail: terminal_text(detail),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrs_contracts::event::{Causality, Durability, Visibility};
    use agentrs_contracts::ids::{RunEpoch, Timestamp};

    fn event(id: &str, durability: Durability, payload: EventPayload) -> RunEventEnvelope {
        RunEventEnvelope {
            run_id: "run".into(),
            epoch: RunEpoch(1),
            event_id: EventId::new(id),
            seq: None,
            live_seq: None,
            at: Timestamp(0),
            durability,
            visibility: Visibility::User,
            causality: Causality::default(),
            surface: None,
            payload,
        }
    }

    #[test]
    fn live_delta_is_replaced_by_committed_surface() {
        let mut state = AppState::default();
        state.apply(&event(
            "d",
            Durability::LiveStream,
            EventPayload::TextDelta { text: "hel".into() },
        ));
        assert_eq!(state.streaming, "hel");
        let message = Message::new(Role::Assistant, vec![ContentBlock::text("hello")]);
        state.apply(&event(
            "m",
            Durability::DurableFact,
            EventPayload::SurfaceMessageRecorded {
                message: serde_json::to_value(message).unwrap(),
            },
        ));
        assert!(state.streaming.is_empty());
        assert_eq!(state.transcript[0].text, "hello");
    }

    #[test]
    fn duplicate_durable_event_is_idempotent() {
        let message = Message::new(Role::User, vec![ContentBlock::text("once")]);
        let event = event(
            "same",
            Durability::DurableFact,
            EventPayload::SurfaceMessageRecorded {
                message: serde_json::to_value(message).unwrap(),
            },
        );
        let mut state = AppState::default();
        state.apply(&event);
        state.apply(&event);
        assert_eq!(state.transcript.len(), 1);
    }

    #[test]
    fn replay_ignores_live_events() {
        let events = vec![
            event("start", Durability::DurableFact, EventPayload::RunStarted),
            event(
                "delta",
                Durability::LiveStream,
                EventPayload::TextDelta { text: "lost".into() },
            ),
            event("done", Durability::DurableFact, EventPayload::RunCompleted),
        ];
        let mut state = AppState::default();
        state.replay(&events);
        assert_eq!(state.status, RunStatus::Completed);
        assert!(state.streaming.is_empty());
    }
}
