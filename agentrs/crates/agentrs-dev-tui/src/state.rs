//! Pure event projection used by both live rendering and durable replay.
//!
//! The projection keeps the **causal shape** of a run rather than flattening it:
//! a tool node sits after the text that led to it and before the answer it made
//! possible, because that is the order the events arrive in and the order a
//! reader needs. The engine records the assistant message (text blocks and
//! `tool_use` blocks together) before it emits `ToolProposed`, so appending in
//! arrival order and upserting tool nodes by call id is enough — no reordering
//! pass, and no dependence on which of the two arrives first.
//!
//! Everything here is a function of the events plus an injected `now_ms`.
//! Replay passes zero for the clock, which is why every timing is optional: a
//! rebuilt run must not invent durations the durable log never recorded.

use std::collections::{BTreeMap, BTreeSet};

use agentrs_contracts::event::{EventPayload, RunEventEnvelope};
use agentrs_contracts::ids::{EventId, RunId};
use agentrs_contracts::surface::SurfaceOp;
use agentrs_types::{ContentBlock, Message, Role, TokenUsage};

use crate::text::sanitize_text;

/// Coarse run state shown in the status row.
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
            Self::Idle => "idle",
            Self::Running => "running",
            Self::AwaitingApproval => "approval",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::NeedsAction => "needs action",
            Self::Failed => "failed",
        }
    }

    /// True once no further work can happen without an explicit user action.
    pub const fn terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Canceled | Self::NeedsAction | Self::Failed
        )
    }
}

/// What a text node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// Ordinary prose.
    Prose,
    /// Model reasoning.
    Reasoning,
}

/// One block of text somebody said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextNode {
    /// Who said it.
    pub role: Role,
    /// Prose or reasoning.
    pub kind: TextKind,
    /// Sanitized text.
    pub text: String,
    /// Turn this belongs to, counted from one.
    pub turn: usize,
}

/// Where a tool call has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    /// The model asked for it; nothing has happened yet.
    Proposed,
    /// A human has to decide before it can run.
    AwaitingApproval,
    /// Running now.
    Running,
    /// Finished successfully.
    Succeeded,
    /// The tool reported an error.
    Failed,
    /// Policy, a guard, or a hook refused it.
    Denied,
    /// Cancelled before it finished.
    Canceled,
}

impl ToolStatus {
    /// True once the call can no longer change.
    pub const fn settled(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Denied | Self::Canceled
        )
    }

    /// True when the call finished without any kind of refusal or error.
    pub const fn succeeded(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// One tool call, from proposal to result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolNode {
    /// Stable call identifier.
    pub call_id: String,
    /// Tool name, once the proposal has been seen.
    pub name: String,
    /// Arguments as the model wrote them.
    pub input: serde_json::Value,
    /// Lifecycle position.
    pub status: ToolStatus,
    /// Sanitized small output, when the tool returned one.
    pub output: Option<String>,
    /// Failure or refusal text.
    pub message: Option<String>,
    /// Stable refusal code, when the call was denied.
    pub deny_code: Option<String>,
    /// Content references produced instead of inline output.
    pub artifacts: Vec<String>,
    /// The isolation level that actually took effect.
    pub isolation: Option<String>,
    /// Sandbox execution identifier, used for reconcile.
    pub execution_id: Option<String>,
    /// Wall clock at which the call started, when a live clock was available.
    pub started_at_ms: Option<u64>,
    /// Wall clock at which the call settled.
    pub ended_at_ms: Option<u64>,
    /// Turn this belongs to, counted from one.
    pub turn: usize,
}

impl ToolNode {
    /// How long the call has been running, or ran for.
    pub fn elapsed_ms(&self, now_ms: u64) -> Option<u64> {
        let started = self.started_at_ms?;
        let end = if self.status.settled() {
            self.ended_at_ms?
        } else {
            now_ms
        };
        Some(end.saturating_sub(started))
    }
}

/// A durable event worth one line of its own: compaction, an approval audit, a
/// mode change. Never something the run merely did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerNode {
    /// What happened, already sanitized.
    pub text: String,
    /// Turn this belongs to.
    pub turn: usize,
}

/// One thing in the transcript, in causal order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    /// Somebody said something.
    Text(TextNode),
    /// A tool call.
    ///
    /// Boxed because it is much the largest variant, and the transcript is a
    /// `Vec` of these: paying its width on every marker row would be most of
    /// the projection's memory for nothing.
    Tool(Box<ToolNode>),
    /// A durable marker.
    Marker(MarkerNode),
    /// The end of a turn.
    TurnEnd {
        /// The turn that ended, counted from one.
        turn: usize,
    },
}

/// Running totals the status row reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Tool calls proposed, whatever became of them.
    pub tools: usize,
    /// Approvals requested.
    pub approvals: usize,
    /// Cache prefix breaks observed.
    pub cache_breaks: usize,
    /// Compactions completed.
    pub compactions: usize,
}

/// Entire deterministic UI projection.
#[derive(Debug, Default)]
pub struct AppState {
    /// Current run identifier.
    pub run_id: Option<RunId>,
    /// Runtime status.
    pub status: RunStatus,
    /// The transcript, in causal order.
    pub nodes: Vec<Node>,
    /// Uncommitted live assistant text.
    pub streaming: String,
    /// Uncommitted live reasoning.
    ///
    /// Kept apart from [`Self::streaming`]: they are two different things the
    /// model said, and merging them would put the reasoning inside the answer.
    pub streaming_thinking: String,
    /// Provider token usage once the run summary arrives.
    pub usage: TokenUsage,
    /// Live events dropped by the bounded sink.
    pub dropped_live: u64,
    /// Turn counter, incremented on `TurnStarted`.
    pub turn: usize,
    /// Totals for the status row.
    pub counters: Counters,
    /// Wall clock of the most recent visible output, when a live clock existed.
    pub last_output_ms: Option<u64>,
    seen_durable: BTreeSet<EventId>,
    tool_index: BTreeMap<String, usize>,
}

impl AppState {
    /// Applies one runtime event. Duplicate durable event IDs are ignored.
    ///
    /// `now_ms` is the host's clock. Pass zero when replaying: the projection
    /// then records no timings at all rather than pretending the rebuilt run
    /// happened just now.
    pub fn apply(&mut self, event: &RunEventEnvelope, now_ms: u64) {
        if event.is_durable() && !self.seen_durable.insert(event.event_id.clone()) {
            return;
        }
        self.run_id = Some(event.run_id.clone());
        let clock = (now_ms > 0).then_some(now_ms);

        match &event.payload {
            EventPayload::RunStarted => self.status = RunStatus::Running,
            EventPayload::RunCompleted => self.status = RunStatus::Completed,
            EventPayload::RunCanceled => self.status = RunStatus::Canceled,
            EventPayload::RunNeedsUserAction => self.status = RunStatus::NeedsAction,
            EventPayload::RunFailed => self.status = RunStatus::Failed,
            EventPayload::TurnStarted => self.turn += 1,
            EventPayload::TurnEnded => self.nodes.push(Node::TurnEnd { turn: self.turn }),
            EventPayload::TextDelta { text } => {
                self.streaming.push_str(&sanitize_text(text));
                self.last_output_ms = clock;
            }
            EventPayload::ThinkingDelta { text } => {
                self.streaming_thinking.push_str(&sanitize_text(text));
                self.last_output_ms = clock;
            }
            EventPayload::SurfaceMessageRecorded { message } => {
                let replaced = event
                    .surface
                    .as_ref()
                    .is_some_and(|marker| !matches!(marker.op, SurfaceOp::Append));
                if let Ok(message) = serde_json::from_value::<Message>(message.clone()) {
                    self.commit_message(message, replaced, clock);
                }
            }
            EventPayload::ToolProposed { call_id } => {
                self.counters.tools += 1;
                self.tool_mut(&call_id.to_string());
            }
            EventPayload::ApprovalRequested { call_id } => {
                self.status = RunStatus::AwaitingApproval;
                self.counters.approvals += 1;
                let node = self.tool_mut(&call_id.to_string());
                node.status = ToolStatus::AwaitingApproval;
            }
            EventPayload::ToolStarted { call_id } => {
                self.status = RunStatus::Running;
                let node = self.tool_mut(&call_id.to_string());
                node.status = ToolStatus::Running;
                node.started_at_ms = clock;
            }
            EventPayload::StepIntentRecorded { intent } => {
                let execution = intent.execution_id.to_string();
                let name = intent.tool_name.clone();
                let node = self.tool_mut(&intent.call_id.to_string());
                node.execution_id = Some(execution);
                if node.name.is_empty() {
                    node.name = name;
                }
            }
            EventPayload::StepResultRecorded { result } => {
                let outcome = result.outcome.clone();
                let output = result.output.clone();
                let artifacts: Vec<String> =
                    result
                        .artifacts
                        .iter()
                        .map(|reference| {
                            format!(
                                "{} · {} · {} bytes",
                                &reference.digest.as_str()[..reference.digest.as_str().len().min(12)],
                                reference.media_type,
                                reference.len
                            )
                        })
                        .collect();
                let isolation = result
                    .effective_isolation
                    .as_ref()
                    .map(|level| format!("{level:?}").to_lowercase());
                let node = self.tool_mut(&result.call_id.to_string());
                node.output = output.as_deref().map(sanitize_text);
                node.artifacts = artifacts;
                node.isolation = isolation;
                node.ended_at_ms = clock;
                match outcome {
                    agentrs_contracts::StepOutcome::Succeeded => {
                        node.status = ToolStatus::Succeeded;
                    }
                    agentrs_contracts::StepOutcome::Failed { message } => {
                        node.status = ToolStatus::Failed;
                        node.message = Some(sanitize_text(&message));
                    }
                    agentrs_contracts::StepOutcome::Denied { code, message } => {
                        node.status = ToolStatus::Denied;
                        node.deny_code = Some(format!("{code:?}"));
                        node.message = Some(sanitize_text(&message));
                    }
                    agentrs_contracts::StepOutcome::Canceled => {
                        node.status = ToolStatus::Canceled;
                    }
                }
                self.last_output_ms = clock;
            }
            EventPayload::ApprovalTimedOut { call_id, .. } => {
                self.status = RunStatus::NeedsAction;
                let node = self.tool_mut(&call_id.to_string());
                node.status = ToolStatus::AwaitingApproval;
                node.message = Some("approval expired; the step is suspended".into());
                self.mark("approval timed out; resume continues the same step intent");
            }
            EventPayload::CompactionCompleted { source_range } => {
                self.counters.compactions += 1;
                self.mark(&format!(
                    "history compacted for the model over events {}..{}; \
                     this transcript is unchanged",
                    source_range.start.0, source_range.end.0
                ));
            }
            EventPayload::PermissionModeChanged => self.mark("permission mode changed"),
            EventPayload::ExternalFactReceived => self.mark("external fact received"),
            EventPayload::ContentRefUnresolved => {
                self.mark("a content reference could not be resolved; context was degraded");
            }
            EventPayload::CacheBreakObserved => self.counters.cache_breaks += 1,
            _ => {}
        }
    }

    /// Replays a durable event prefix through the same reducer used for live data.
    pub fn replay<'a>(&mut self, events: impl IntoIterator<Item = &'a RunEventEnvelope>) {
        for event in events {
            if event.is_durable() {
                self.apply(event, 0);
            }
        }
    }

    /// The call that is running or awaiting a decision right now, if any.
    pub fn active_tool(&self) -> Option<&ToolNode> {
        self.nodes.iter().rev().find_map(|node| match node {
            Node::Tool(tool) if !tool.status.settled() => Some(tool.as_ref()),
            _ => None,
        })
    }

    /// One tool node by call id.
    pub fn tool(&self, call_id: &str) -> Option<&ToolNode> {
        self.tool_index
            .get(call_id)
            .and_then(|index| match self.nodes.get(*index) {
                Some(Node::Tool(tool)) => Some(tool.as_ref()),
                _ => None,
            })
    }

    fn mark(&mut self, text: &str) {
        self.nodes.push(Node::Marker(MarkerNode {
            text: sanitize_text(text),
            turn: self.turn,
        }));
    }

    /// The tool node for `call_id`, appending one if this is its first mention.
    ///
    /// Upserting rather than requiring a fixed arrival order is what keeps the
    /// projection honest under recovery, where a result can be replayed for a
    /// call whose proposal sits in an earlier segment of the log.
    fn tool_mut(&mut self, call_id: &str) -> &mut ToolNode {
        let index = match self.tool_index.get(call_id) {
            Some(index) => *index,
            None => {
                let index = self.nodes.len();
                self.nodes.push(Node::Tool(Box::new(ToolNode {
                    call_id: call_id.to_string(),
                    name: String::new(),
                    input: serde_json::Value::Null,
                    status: ToolStatus::Proposed,
                    output: None,
                    message: None,
                    deny_code: None,
                    artifacts: Vec::new(),
                    isolation: None,
                    execution_id: None,
                    started_at_ms: None,
                    ended_at_ms: None,
                    turn: self.turn,
                })));
                self.tool_index.insert(call_id.to_string(), index);
                index
            }
        };
        match &mut self.nodes[index] {
            Node::Tool(tool) => tool,
            _ => unreachable!("tool_index only ever points at a tool node"),
        }
    }

    fn commit_message(&mut self, message: Message, replaced: bool, clock: Option<u64>) {
        // Compaction masks history *for the model*. The human transcript keeps
        // everything and gets a marker instead: a reader who scrolls back and
        // finds their own words gone would have no way to know why.
        if replaced {
            self.mark("history replaced for the model by a summary");
        }
        if message.role == Role::Assistant {
            self.streaming.clear();
            self.streaming_thinking.clear();
        }
        let turn = self.turn;
        for block in message.content {
            match block {
                ContentBlock::Text { text } => {
                    let text = sanitize_text(&text);
                    if !text.trim().is_empty() {
                        self.nodes.push(Node::Text(TextNode {
                            role: message.role,
                            kind: TextKind::Prose,
                            text,
                            turn,
                        }));
                        self.last_output_ms = clock;
                    }
                }
                ContentBlock::ToolUse { id, name, input, .. } => {
                    let node = self.tool_mut(&id.to_string());
                    node.name = name;
                    node.input = input;
                }
                ContentBlock::Thinking { thinking, .. } => {
                    let text = sanitize_text(&thinking);
                    if !text.trim().is_empty() {
                        self.nodes.push(Node::Text(TextNode {
                            role: message.role,
                            kind: TextKind::Reasoning,
                            text,
                            turn,
                        }));
                        self.last_output_ms = clock;
                    }
                }
                // A tool result is the same fact as `StepResultRecorded`, which
                // carries the outcome as well. Rendering both would double every
                // card body.
                ContentBlock::ToolResult { .. } => {}
                _ => {}
            }
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

    fn assistant(blocks: Vec<ContentBlock>) -> EventPayload {
        EventPayload::SurfaceMessageRecorded {
            message: serde_json::to_value(Message::new(Role::Assistant, blocks)).unwrap(),
        }
    }

    fn texts(state: &AppState) -> Vec<String> {
        state
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn live_delta_is_replaced_by_committed_surface() {
        let mut state = AppState::default();
        state.apply(
            &event(
                "d",
                Durability::LiveStream,
                EventPayload::TextDelta { text: "hel".into() },
            ),
            0,
        );
        assert_eq!(state.streaming, "hel");
        state.apply(
            &event(
                "m",
                Durability::DurableFact,
                assistant(vec![ContentBlock::text("hello")]),
            ),
            0,
        );
        assert!(state.streaming.is_empty());
        assert_eq!(texts(&state), vec!["hello".to_string()]);
    }

    #[test]
    fn reasoning_is_projected_apart_from_the_answer() {
        // They are two different things the model said; merged, the reasoning
        // ends up inside the answer.
        let mut state = AppState::default();
        state.apply(
            &event(
                "t",
                Durability::LiveStream,
                EventPayload::ThinkingDelta { text: "weighing".into() },
            ),
            0,
        );
        state.apply(
            &event(
                "d",
                Durability::LiveStream,
                EventPayload::TextDelta { text: "answer".into() },
            ),
            0,
        );
        assert_eq!(state.streaming_thinking, "weighing");
        assert_eq!(state.streaming, "answer");

        state.apply(
            &event(
                "m",
                Durability::DurableFact,
                assistant(vec![
                    ContentBlock::Thinking {
                        thinking: "weighing it up".into(),
                        signature: None,
                    },
                    ContentBlock::text("the answer"),
                ]),
            ),
            0,
        );
        // The live buffers hand over to the committed message.
        assert!(state.streaming.is_empty());
        assert!(state.streaming_thinking.is_empty());
        let kinds: Vec<TextKind> = state
            .nodes
            .iter()
            .filter_map(|node| match node {
                Node::Text(text) => Some(text.kind),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec![TextKind::Reasoning, TextKind::Prose]);
    }

    #[test]
    fn duplicate_durable_event_is_idempotent() {
        let event = event(
            "same",
            Durability::DurableFact,
            EventPayload::SurfaceMessageRecorded {
                message: serde_json::to_value(Message::new(
                    Role::User,
                    vec![ContentBlock::text("once")],
                ))
                .unwrap(),
            },
        );
        let mut state = AppState::default();
        state.apply(&event, 0);
        state.apply(&event, 0);
        assert_eq!(texts(&state).len(), 1);
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

    #[test]
    fn a_tool_card_sits_between_the_text_that_led_to_it_and_the_answer() {
        let mut state = AppState::default();
        state.apply(&event("t", Durability::DurableFact, EventPayload::TurnStarted), 0);
        state.apply(
            &event(
                "m1",
                Durability::DurableFact,
                assistant(vec![
                    ContentBlock::text("I will read it."),
                    ContentBlock::ToolUse {
                        id: "c1".into(),
                        name: "Read".into(),
                        input: serde_json::json!({"path": "a.md"}),
                        extra: None,
                    },
                ]),
            ),
            0,
        );
        state.apply(
            &event(
                "p1",
                Durability::DurableFact,
                EventPayload::ToolProposed { call_id: "c1".into() },
            ),
            0,
        );
        state.apply(
            &event(
                "m2",
                Durability::DurableFact,
                assistant(vec![ContentBlock::text("It says hello.")]),
            ),
            0,
        );
        let kinds: Vec<&str> = state
            .nodes
            .iter()
            .map(|node| match node {
                Node::Text(_) => "text",
                Node::Tool(_) => "tool",
                Node::Marker(_) => "marker",
                Node::TurnEnd { .. } => "turn",
            })
            .collect();
        assert_eq!(kinds, vec!["text", "tool", "text"]);
        assert_eq!(state.tool("c1").map(|tool| tool.name.as_str()), Some("Read"));
    }

    #[test]
    fn a_result_without_a_proposal_still_lands_on_one_node() {
        // Recovery replays a result whose proposal may be in an earlier segment.
        let mut state = AppState::default();
        state.apply(
            &event(
                "r",
                Durability::DurableFact,
                EventPayload::StepResultRecorded {
                    result: Box::new(agentrs_contracts::StepResult {
                        step_id: "s1".into(),
                        call_id: "c9".into(),
                        outcome: agentrs_contracts::StepOutcome::Succeeded,
                        effective_isolation: None,
                        artifacts: Vec::new(),
                        output: Some("done".into()),
                        at: Timestamp(0),
                    }),
                },
            ),
            0,
        );
        assert_eq!(state.nodes.len(), 1);
        assert_eq!(state.tool("c9").unwrap().status, ToolStatus::Succeeded);
    }

    #[test]
    fn a_denial_keeps_its_code_and_message() {
        let mut state = AppState::default();
        state.apply(
            &event(
                "r",
                Durability::DurableFact,
                EventPayload::StepResultRecorded {
                    result: Box::new(agentrs_contracts::StepResult {
                        step_id: "s1".into(),
                        call_id: "c1".into(),
                        outcome: agentrs_contracts::StepOutcome::Denied {
                            code: agentrs_contracts::policy::DenyCode::PermissionMode,
                            message: "plan mode forbids writes".into(),
                        },
                        effective_isolation: None,
                        artifacts: Vec::new(),
                        output: None,
                        at: Timestamp(0),
                    }),
                },
            ),
            0,
        );
        let tool = state.tool("c1").unwrap();
        assert_eq!(tool.status, ToolStatus::Denied);
        assert_eq!(tool.deny_code.as_deref(), Some("PermissionMode"));
        assert!(tool.message.as_deref().unwrap().contains("plan mode"));
    }

    #[test]
    fn replay_records_no_timings_at_all() {
        // A rebuilt run must not claim durations the durable log never held.
        let mut state = AppState::default();
        state.replay(&[event(
            "s",
            Durability::DurableFact,
            EventPayload::ToolStarted { call_id: "c1".into() },
        )]);
        let tool = state.tool("c1").unwrap();
        assert_eq!(tool.started_at_ms, None);
        assert_eq!(tool.elapsed_ms(5_000), None);
    }

    #[test]
    fn a_live_call_reports_how_long_it_has_been_running() {
        let mut state = AppState::default();
        state.apply(
            &event(
                "s",
                Durability::DurableFact,
                EventPayload::ToolStarted { call_id: "c1".into() },
            ),
            1_000,
        );
        assert_eq!(state.tool("c1").unwrap().elapsed_ms(4_000), Some(3_000));
    }

    #[test]
    fn compaction_marks_the_transcript_without_removing_anything_from_it() {
        let mut state = AppState::default();
        state.apply(
            &event(
                "m",
                Durability::DurableFact,
                assistant(vec![ContentBlock::text("early words")]),
            ),
            0,
        );
        state.apply(
            &event(
                "c",
                Durability::DurableFact,
                EventPayload::CompactionCompleted {
                    source_range: agentrs_contracts::ids::EventRange {
                        start: agentrs_contracts::ids::EventSequence(1),
                        end: agentrs_contracts::ids::EventSequence(4),
                    },
                },
            ),
            0,
        );
        // The words the user scrolled past are still there; only the model's
        // view was narrowed.
        assert_eq!(texts(&state), vec!["early words".to_string()]);
        assert_eq!(state.counters.compactions, 1);
        assert!(state
            .nodes
            .iter()
            .any(|node| matches!(node, Node::Marker(marker) if marker.text.contains("compacted"))));
    }

    #[test]
    fn counters_track_what_ran_not_what_is_shown() {
        let mut state = AppState::default();
        for (index, call) in ["c1", "c2", "c3"].iter().enumerate() {
            state.apply(
                &event(
                    &format!("p{index}"),
                    Durability::DurableFact,
                    EventPayload::ToolProposed {
                        call_id: (*call).into(),
                    },
                ),
                0,
            );
        }
        state.apply(
            &event(
                "a",
                Durability::DurableFact,
                EventPayload::ApprovalRequested { call_id: "c1".into() },
            ),
            0,
        );
        assert_eq!(state.counters.tools, 3);
        assert_eq!(state.counters.approvals, 1);
    }

    #[test]
    fn the_active_call_is_the_last_unsettled_one() {
        let mut state = AppState::default();
        state.apply(
            &event(
                "s1",
                Durability::DurableFact,
                EventPayload::ToolStarted { call_id: "c1".into() },
            ),
            0,
        );
        state.apply(
            &event(
                "s2",
                Durability::DurableFact,
                EventPayload::ToolStarted { call_id: "c2".into() },
            ),
            0,
        );
        assert_eq!(state.active_tool().map(|tool| tool.call_id.as_str()), Some("c2"));
    }
}
