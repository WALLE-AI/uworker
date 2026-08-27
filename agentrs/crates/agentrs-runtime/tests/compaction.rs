//! 压缩落到 Surface 上的端到端验证（架构 §4.3.1、§9.3）。
//!
//! 规划本身在 `agentrs-context::compaction` 里有单元测试。这里验的是
//! **压缩落地之后那三条推论是否成立**：
//!
//! 1. 模型看到的历史被遮蔽了；
//! 2. **人类 transcript 不受影响**——用户已经看到的对话不能被抹掉；
//! 3. 缓存前缀从 `range.start` 起失效，且这个位置是精确算出来的。
//!
//! 第 2 条是 `Replace` 这个设计的全部意义。若压缩是"改写历史"，
//! 用户滚回去会发现自己说过的话不见了。

use std::sync::Arc;

use agentrs_context::compaction::{CompactionPlan, Tier, Trigger};
use agentrs_contracts::event::EventPayload;
use agentrs_contracts::ids::EventSequence;
use agentrs_contracts::ports::RunPersistence;
use agentrs_contracts::surface::{SurfaceEventKind, SurfaceGeneration, SurfaceOp, SurfaceRange};
use agentrs_runtime::surface::{cache_invalidation_point, derive_messages, derive_transcript, SurfaceNode};
use agentrs_types::{ContentBlock, Message, Role};

// ---------------------------------------------------------------------------
// Surface 层：三条推论
// ---------------------------------------------------------------------------

fn 追加(seq: u64, text: &str) -> SurfaceNode {
    SurfaceNode {
        seq: EventSequence(seq),
        kind: SurfaceEventKind::UserMessage,
        op: SurfaceOp::Append,
        message: Message::new(Role::User, vec![ContentBlock::text(text)]),
    }
}

fn 遮蔽(seq: u64, start: u64, end: u64, 摘要: &str) -> SurfaceNode {
    SurfaceNode {
        seq: EventSequence(seq),
        kind: SurfaceEventKind::AssistantMessage,
        op: SurfaceOp::Replace {
            range: SurfaceRange {
                start: EventSequence(start),
                end: EventSequence(end),
            },
            generation: SurfaceGeneration(1),
        },
        message: Message::new(Role::Assistant, vec![ContentBlock::text(摘要)]),
    }
}

fn 文本(ms: &[Message]) -> Vec<String> {
    ms.iter()
        .map(|m| {
            m.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

#[test]
fn 压缩遮蔽模型看到的历史() {
    let nodes = vec![
        追加(1, "第一轮"),
        追加(2, "第二轮"),
        追加(3, "第三轮"),
        遮蔽(4, 1, 3, "【摘要】前两轮在讨论重构"),
        追加(5, "第四轮"),
    ];
    assert_eq!(
        文本(&derive_messages(&nodes)),
        ["第三轮", "【摘要】前两轮在讨论重构", "第四轮"]
    );
}

#[test]
fn 人类_transcript_不受压缩影响() {
    // **`Replace` 这个设计的全部意义。** 若压缩是"改写历史"，
    // 用户滚回去会发现自己说过的话不见了。
    let nodes = vec![
        追加(1, "第一轮"),
        追加(2, "第二轮"),
        遮蔽(3, 1, 3, "【摘要】"),
        追加(4, "第三轮"),
    ];
    assert_eq!(
        文本(&derive_transcript(&nodes)),
        ["第一轮", "第二轮", "第三轮"],
        "被遮蔽的对话仍应出现在人类 transcript 里"
    );
    // 摘要本身是 model-only，不进 transcript。
    assert!(!文本(&derive_transcript(&nodes)).contains(&"【摘要】".to_string()));
}

#[test]
fn 缓存失效点等于被遮蔽区间的起点() {
    let nodes = vec![追加(1, "a"), 追加(2, "b"), 遮蔽(3, 2, 3, "【摘要】")];
    assert_eq!(cache_invalidation_point(&nodes), Some(EventSequence(2)));
}

#[test]
fn 没有压缩时前缀完全稳定() {
    let nodes = vec![追加(1, "a"), 追加(2, "b"), 追加(3, "c")];
    assert_eq!(cache_invalidation_point(&nodes), None);
}

#[test]
fn 多次压缩取最早的失效点() {
    // 从最早那一点起缓存必然 miss——报更晚的那个是自欺。
    let nodes = vec![
        追加(1, "a"),
        追加(2, "b"),
        追加(3, "c"),
        遮蔽(4, 2, 4, "【摘要二】"),
        追加(5, "d"),
        遮蔽(6, 1, 2, "【摘要一】"),
    ];
    assert_eq!(cache_invalidation_point(&nodes), Some(EventSequence(1)));
}

#[test]
fn 压缩后的历史可由同一个_fold_精确重建() {
    // 「log 前缀 + 同一个 fold 函数」——投影必须是纯的，否则 replay 不成立。
    let nodes = vec![追加(1, "a"), 追加(2, "b"), 遮蔽(3, 1, 3, "【摘要】")];
    assert_eq!(derive_messages(&nodes), derive_messages(&nodes));
}

// ---------------------------------------------------------------------------
// 引擎层：apply_compaction
// ---------------------------------------------------------------------------

use agentrs_runtime::composition::ResourceOwner;
use agentrs_runtime::engine::{AdmitAll, CancelToken, Engine, EngineDeps, StepDriver, TurnGuards};
use agentrs_runtime::inbox::Inbox;
use agentrs_testkit::{FakeClock, FakePersistence};
use agentrs_types::{LlmEvent, LlmRequest, StopReason, TokenUsage};

struct 静默驱动;

#[async_trait::async_trait]
impl StepDriver for 静默驱动 {
    async fn call(&self, _req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
        Ok(vec![
            LlmEvent::TextDelta("好".into()),
            LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            },
        ])
    }
}

fn 计划(start: u64, end: u64) -> CompactionPlan {
    CompactionPlan {
        trigger: Trigger::Pressure,
        tier: Tier::Compact,
        range: SurfaceRange {
            start: EventSequence(start),
            end: EventSequence(end),
        },
        reclaimed_tokens: 400,
        projected_tokens: 200,
    }
}

/// 直接装一个引擎（不启动主循环）。
///
/// `apply_compaction` 是宿主在 Step 边界主动调用的，不需要跑主循环；
/// 直接构造反而让测试确定——不必和主循环抢 Surface 的锁。
fn 起一个() -> (Engine, Arc<FakePersistence>) {
    let persistence = Arc::new(FakePersistence::new());
    let deps = EngineDeps {
        persistence: persistence.clone() as Arc<dyn RunPersistence>,
        clock: Arc::new(FakeClock::new(0)),
        driver: Arc::new(静默驱动),
        admission: Arc::new(AdmitAll),
        tools: None,
    };
    let engine = Engine::new(
        "r-compact".into(),
        agentrs_contracts::ids::RunEpoch(1),
        deps,
        Arc::new(Inbox::new(8)),
        Arc::new(CancelToken::default()),
        ResourceOwner::new("run:r-compact"),
        TurnGuards::default(),
    );
    (engine, persistence)
}

#[tokio::test]
async fn 压缩写下起止两条事实并带上_source_range() {
    // `source_range` 是"复用摘要、不重复压同一段"的依据。
    // 缺了它，同一段历史会被反复摘要，每次都离原文更远。
    let (engine, persistence) = 起一个();

    let gen = engine
        .apply_compaction(
            &计划(1, 3),
            Message::new(Role::Assistant, vec![ContentBlock::text("【摘要】")]),
            &Default::default(),
        )
        .await
        .expect("压缩失败");
    assert_eq!(gen, SurfaceGeneration(1));

    let events = persistence.events();
    assert!(events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::CompactionStarted)));
    let 区间 = events.iter().find_map(|e| match &e.payload {
        EventPayload::CompactionCompleted { source_range } => Some(*source_range),
        _ => None,
    });
    assert_eq!(
        区间.map(|r| (r.start.0, r.end.0)),
        Some((1, 3)),
        "缺少 source_range，同一段历史会被反复摘要"
    );
}

#[tokio::test]
async fn 代际逐次前进() {
    // 溢出触发的重试判据：只有代际**确实前进**才允许再来一轮。
    let (engine, _) = 起一个();
    let 摘要 = || Message::new(Role::Assistant, vec![ContentBlock::text("【摘要】")]);

    let g1 = engine
        .apply_compaction(&计划(1, 3), 摘要(), &Default::default())
        .await
        .unwrap();
    let g2 = engine
        .apply_compaction(&计划(4, 6), 摘要(), &Default::default())
        .await
        .unwrap();

    assert_eq!(g1, SurfaceGeneration(1));
    assert_eq!(g2, SurfaceGeneration(2));
    assert!(g2 > g1, "代际必须严格前进，否则重试循环没有终止判据");
}
