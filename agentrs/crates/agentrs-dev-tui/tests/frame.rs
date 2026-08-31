//! One whole frame, from a durable event log to the cells on screen.
//!
//! The unit tests cover each module; this covers the assembly, which is where
//! the four defects the M0 write-up describes were actually found. The log below
//! is the shape of the task the dev TUI exists for — read three files, summarize,
//! write one — so a regression in ordering, collapsing, folding or the status
//! row shows up here as a changed picture rather than as a passing suite.

use std::collections::{HashMap, HashSet};

use agentrs_contracts::authority::PermissionMode;
use agentrs_contracts::event::{Causality, Durability, EventPayload, RunEventEnvelope, Visibility};
use agentrs_contracts::ids::{EventId, RunEpoch, Timestamp};
use agentrs_contracts::{StepOutcome, StepResult};
use agentrs_dev_tui::collapse::collapse_runs;
use agentrs_dev_tui::composer::Composer;
use agentrs_dev_tui::glyphs::{ASCII_GLYPHS, UNICODE_GLYPHS};
use agentrs_dev_tui::notices::NoticeQueue;
use agentrs_dev_tui::state::AppState;
use agentrs_dev_tui::text::display_width;
use agentrs_dev_tui::theme::{ColorLevel, Theme};
use agentrs_dev_tui::transcript::{build_entries, transcript_rows, TranscriptOptions, TranscriptRow};
use agentrs_dev_tui::ui::{render, View};
use agentrs_types::{ContentBlock, Message, Role};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn event(id: &str, payload: EventPayload) -> RunEventEnvelope {
    RunEventEnvelope {
        run_id: "run-demo".into(),
        epoch: RunEpoch(1),
        event_id: EventId::new(id),
        seq: None,
        live_seq: None,
        at: Timestamp(0),
        durability: Durability::DurableFact,
        visibility: Visibility::User,
        causality: Causality::default(),
        surface: None,
        payload,
    }
}

fn message(role: Role, blocks: Vec<ContentBlock>) -> EventPayload {
    EventPayload::SurfaceMessageRecorded {
        message: serde_json::to_value(Message::new(role, blocks)).unwrap(),
    }
}

fn tool_use(id: &str, name: &str, input: serde_json::Value) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: name.into(),
        input,
        extra: None,
    }
}

fn succeeded(call: &str, output: &str) -> EventPayload {
    EventPayload::StepResultRecorded {
        result: Box::new(StepResult {
            step_id: "s".into(),
            call_id: call.into(),
            outcome: StepOutcome::Succeeded,
            effective_isolation: None,
            artifacts: Vec::new(),
            output: Some(output.into()),
            at: Timestamp(0),
        }),
    }
}

/// The log a "read three files, summarize, write one" run leaves behind.
fn demo_log() -> Vec<RunEventEnvelope> {
    let mut log = vec![
        event("start", EventPayload::RunStarted),
        event("turn1", EventPayload::TurnStarted),
        event(
            "user",
            message(Role::User, vec![ContentBlock::text("读三个 md 文件，总结成 summary.md")]),
        ),
        event(
            "assistant1",
            message(
                Role::Assistant,
                vec![
                    ContentBlock::text("I will read the three files first.\nThen I will write the summary."),
                    tool_use("c1", "Read", serde_json::json!({"path": "billing.md"})),
                    tool_use("c2", "Read", serde_json::json!({"path": "auth.md"})),
                    tool_use("c3", "Read", serde_json::json!({"path": "storage.md"})),
                ],
            ),
        ),
    ];
    for call in ["c1", "c2", "c3"] {
        log.push(event(
            &format!("propose-{call}"),
            EventPayload::ToolProposed {
                call_id: call.into(),
            },
        ));
    }
    for (call, body) in [
        ("c1", "# 计费模块\n按 token 计费。"),
        ("c2", "# 鉴权模块\nAPI key 走进程环境注入。"),
        ("c3", "# 存储模块\n事件日志 JSONL 追加写。"),
    ] {
        log.push(event(&format!("result-{call}"), succeeded(call, body)));
    }
    log.extend([
        event("turn1end", EventPayload::TurnEnded),
        event("turn2", EventPayload::TurnStarted),
        event(
            "assistant2",
            message(
                Role::Assistant,
                vec![
                    ContentBlock::text("Now writing **summary.md**."),
                    tool_use(
                        "c4",
                        "Write",
                        serde_json::json!({"path": "summary.md", "content": "# 总结\n三个模块。"}),
                    ),
                ],
            ),
        ),
        event(
            "propose-c4",
            EventPayload::ToolProposed {
                call_id: "c4".into(),
            },
        ),
        event(
            "approval-c4",
            EventPayload::ApprovalRequested {
                call_id: "c4".into(),
            },
        ),
    ]);
    log
}

fn projection() -> AppState {
    let mut state = AppState::default();
    state.replay(&demo_log());
    state
}

fn frame(state: &AppState, width: u16, height: u16, unicode: bool, color: bool) -> Vec<String> {
    let glyphs = if unicode { UNICODE_GLYPHS } else { ASCII_GLYPHS };
    let diffs = HashMap::new();
    let entries = collapse_runs(
        build_entries(
            state,
            &TranscriptOptions {
                glyphs: &glyphs,
                now_ms: 0,
                diffs: &diffs,
            },
        ),
        &glyphs,
    );
    let rows: Vec<TranscriptRow> =
        transcript_rows(&entries, width as usize, &HashSet::new(), &glyphs);
    let composer = Composer::default();
    let notices = NoticeQueue::default();
    let level = if unicode {
        ColorLevel::Truecolor
    } else {
        ColorLevel::Basic
    };
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| {
            let viewport = f.area().height.saturating_sub(3) as usize;
            let scroll = rows.len().saturating_sub(viewport);
            render(
                f,
                &View {
                    state,
                    rows: &rows,
                    scroll,
                    composer: &composer,
                    approval: None,
                    overlay: None,
                    completion: None,
                    working: None,
                    notices: &notices,
                    workspace: "/tmp/agentrs-tui-demo",
                    model: "Qwen3.6-35B-A3B",
                    permission: &PermissionMode::Default,
                    pending_changes: 1,
                    context_window: Some(100_000),
                    glyphs: &glyphs,
                    theme: Theme::resolve(level, color),
                },
            );
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn all_rows(state: &AppState, width: usize, unicode: bool) -> Vec<String> {
    let glyphs = if unicode { UNICODE_GLYPHS } else { ASCII_GLYPHS };
    let diffs = HashMap::new();
    let entries = collapse_runs(
        build_entries(
            state,
            &TranscriptOptions {
                glyphs: &glyphs,
                now_ms: 0,
                diffs: &diffs,
            },
        ),
        &glyphs,
    );
    transcript_rows(&entries, width, &HashSet::new(), &glyphs)
        .iter()
        .map(TranscriptRow::text)
        .collect()
}

#[test]
fn the_transcript_reads_in_causal_order() {
    let rows = all_rows(&projection(), 80, true);
    let at = |needle: &str| {
        rows.iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} is missing from\n{}", rows.join("\n")))
    };
    // The ask, then what the model said it would do, then the calls it made,
    // then the answer that used them.
    assert!(at("读三个 md 文件") < at("I will read the three files"));
    assert!(at("I will read the three files") < at("3 reads"));
    assert!(at("3 reads") < at("Now writing"));
    assert!(at("Now writing") < at("Write summary.md"));
}

#[test]
fn three_reads_collapse_into_one_row_and_the_write_stays_open() {
    let rows = all_rows(&projection(), 80, true);
    assert!(rows.iter().any(|row| row.contains("✓ 3 reads")));
    // Their bodies are not on screen; the row that stands for them is.
    assert!(!rows.iter().any(|row| row.contains("按 token 计费")));
    // A call that has not succeeded is never hidden.
    assert!(rows.iter().any(|row| row.contains("▸ Write summary.md")));
    assert!(rows.iter().any(|row| row.contains("approval required")));
}

#[test]
fn markdown_is_consumed_in_the_assistants_prose() {
    let rows = all_rows(&projection(), 80, true);
    let line = rows
        .iter()
        .find(|row| row.contains("Now writing"))
        .expect("the second turn's prose");
    assert!(line.contains("summary.md"));
    assert!(!line.contains("**"), "the delimiters are styling, not text: {line}");
}

#[test]
fn a_replayed_run_shows_no_durations_it_was_never_told() {
    let rows = all_rows(&projection(), 80, true);
    // `· 12s` on a card would be this host inventing a fact the log never held.
    assert!(!rows.iter().any(|row| row.contains(" · ") && row.contains('s')));
}

#[test]
fn every_row_fits_every_width() {
    let state = projection();
    for width in [24, 40, 60, 80, 120, 200] {
        for row in all_rows(&state, width, true) {
            assert!(display_width(&row) <= width, "width={width}: {row:?}");
        }
    }
}

#[test]
fn the_whole_frame_lands_where_it_should() {
    let rows = frame(&projection(), 120, 16, true, true);
    // The status row is last, carries what the run is, and fits.
    let status = rows.last().expect("a status row");
    assert!(status.contains("Qwen3.6-35B-A3B"), "{status}");
    assert!(status.contains("default"), "{status}");
    assert!(status.contains("tools 4"), "{status}");
    assert!(status.contains("staged 1"), "{status}");
    assert!(display_width(status) <= 120, "{status}");
    // The composer is above it, the context bar above that.
    assert!(rows[14].starts_with("> "), "{:?}", rows[14]);
    assert!(rows[13].starts_with("ctx "), "{:?}", rows[13]);
    // And nothing drew a box.
    let joined = rows.join("\n");
    for boxy in ['┌', '┐', '└', '┘', '├', '┤'] {
        assert!(!joined.contains(boxy), "{boxy} in\n{joined}");
    }
}

#[test]
fn an_ascii_terminal_gets_a_whole_frame_without_wide_glyphs() {
    let state = projection();
    for row in all_rows(&state, 80, false) {
        // The transcript's own furniture is ASCII; the content is the user's.
        let furniture: String = row
            .chars()
            .take_while(|ch| !ch.is_alphanumeric())
            .collect();
        assert!(furniture.is_ascii(), "{row:?}");
    }
    let rows = frame(&state, 80, 16, false, false);
    assert!(rows.iter().any(|row| row.contains("+ 3 reads")), "{rows:?}");
}

#[test]
fn a_narrow_window_drops_status_fields_rather_than_wrapping() {
    let status = frame(&projection(), 34, 12, true, true).pop().unwrap();
    assert!(display_width(&status) <= 34, "{status:?}");
    // What survives at 34 columns is what the run *is*, not what it has done.
    assert!(status.contains("Qwen3.6-35B-A3B"), "{status:?}");
    for dropped in ["tools ", "staged ", "ctx ", "tok "] {
        assert!(!status.contains(dropped), "{dropped:?} survived: {status:?}");
    }
    // Widen it and the counters come back, in priority order.
    let wide = frame(&projection(), 120, 12, true, true).pop().unwrap();
    assert!(wide.contains("default"), "{wide:?}");
    assert!(wide.contains("tools 4"), "{wide:?}");
}
