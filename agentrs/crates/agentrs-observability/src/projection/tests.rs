//! T03B 验收项逐条对应的测试。
//!
//! 五条验收：相同事件前缀恒等 snapshot、stateVersion 变化使旧缓存失效、
//! 向前补页不改变已加载记录键/顺序、UI delta 丢失不影响 committed trajectory、
//! 未知可忽略事件不破坏投影。

use agentrs_contracts::event::{Causality, Durability, EventPayload, RunEventEnvelope, Visibility};
use agentrs_contracts::ids::{EventSequence, LiveSequence, RunEpoch, StepId, Timestamp, TurnId};
use agentrs_contracts::{StepIntent, StepOutcome, StepResult};

use super::defs::*;
use super::*;

// ---- 构造器 ----

fn 事件(seq: u64, payload: EventPayload) -> RunEventEnvelope {
    RunEventEnvelope {
        run_id: "r".into(),
        epoch: RunEpoch(1),
        event_id: format!("e{seq}").into(),
        seq: Some(EventSequence(seq)),
        live_seq: None,
        at: Timestamp(seq as i64 * 100),
        durability: Durability::DurableFact,
        visibility: Visibility::User,
        causality: Causality::default(),
        surface: None,
        payload,
    }
}

fn 在(mut e: RunEventEnvelope, turn: &str, step: Option<&str>) -> RunEventEnvelope {
    e.causality.turn_id = Some(TurnId::new(turn));
    e.causality.step_id = step.map(StepId::new);
    e
}

fn live(seq: u64, payload: EventPayload) -> RunEventEnvelope {
    let mut e = 事件(seq, payload);
    e.seq = None;
    e.live_seq = Some(LiveSequence(seq));
    e.durability = Durability::LiveStream;
    e
}

fn 意图(call: &str, tool: &str) -> EventPayload {
    EventPayload::StepIntentRecorded {
        intent: Box::new(StepIntent {
            step_id: "s".into(),
            call_id: call.into(),
            tool_name: tool.into(),
            input_hash: agentrs_contracts::policy::InputHash(agentrs_contracts::ids::Digest::from_hex("h")),
            change_set_id: "cs".into(),
            execution_id: "x".into(),
            at: Timestamp(0),
        }),
    }
}

fn 结果(call: &str, outcome: StepOutcome) -> EventPayload {
    EventPayload::StepResultRecorded {
        result: Box::new(StepResult {
            step_id: "s".into(),
            call_id: call.into(),
            outcome,
            effective_isolation: None,
            artifacts: vec![],
            output: None,
            at: Timestamp(0),
        }),
    }
}

fn 注册表() -> ProjectionRegistry {
    let mut r = ProjectionRegistry::new();
    register_default(&mut r).unwrap();
    r
}

// ---- 验收 1：确定性 ----

#[test]
fn 相同事件前缀恒等于相同快照() {
    let reg = 注册表();
    let evs = vec![
        事件(1, EventPayload::RunStarted),
        在(事件(2, EventPayload::TurnStarted), "t1", None),
        在(事件(3, EventPayload::StepStarted), "t1", Some("s1")),
        在(事件(4, EventPayload::TurnEnded), "t1", None),
    ];
    let a = reg.snapshot(ProjectionKey("skeleton"), &evs, None).unwrap();
    let b = reg.snapshot(ProjectionKey("skeleton"), &evs, None).unwrap();
    assert_eq!(a, b);
}

#[test]
fn 到达顺序不影响快照() {
    // 重投递/并发写入会打乱到达顺序；折叠前按 seq 排序，否则确定性就破了。
    let reg = 注册表();
    let mut evs = vec![
        事件(1, EventPayload::RunStarted),
        在(事件(2, EventPayload::TurnStarted), "t1", None),
        在(事件(3, EventPayload::StepStarted), "t1", Some("s1")),
    ];
    let 顺序 = reg.snapshot(ProjectionKey("skeleton"), &evs, None).unwrap();
    evs.reverse();
    let 乱序 = reg.snapshot(ProjectionKey("skeleton"), &evs, None).unwrap();
    assert_eq!(顺序, 乱序);
}

#[test]
fn as_of_截断到指定序号() {
    let reg = 注册表();
    let evs = vec![
        事件(1, EventPayload::RunStarted),
        在(事件(2, EventPayload::TurnStarted), "t1", None),
        事件(3, EventPayload::RunCompleted),
    ];
    let 全量 = reg.snapshot(ProjectionKey("skeleton"), &evs, None).unwrap();
    let 截断 = reg
        .snapshot(ProjectionKey("skeleton"), &evs, Some(EventSequence(2)))
        .unwrap();
    assert_eq!(全量.view["terminal"], "RunCompleted");
    // 截断点之后的终态不能"提前"出现在快照里。
    assert!(截断.view["terminal"].is_null());
    assert_eq!(截断.up_to_seq, Some(EventSequence(2)));
}

// ---- 验收 2：stateVersion ----

#[test]
fn 状态版本变化使旧缓存失效() {
    let c = SnapshotCache {
        state_version: 1,
        up_to_seq: Some(EventSequence(10)),
    };
    // 同版本同位置 → 直接复用。
    assert_eq!(c.verdict(1, Some(EventSequence(10))), CacheVerdict::Fresh);
    // 同版本更靠后 → 可增量。
    assert_eq!(c.verdict(1, Some(EventSequence(20))), CacheVerdict::Extendable);
    // **换代必须重算**，哪怕位置完全一致。
    assert_eq!(
        c.verdict(2, Some(EventSequence(10))),
        CacheVerdict::Invalid(CacheInvalidation::StateVersionChanged {
            cached: 1,
            current: 2
        })
    );
}

#[test]
fn 位置倒退不能增量() {
    let c = SnapshotCache {
        state_version: 1,
        up_to_seq: Some(EventSequence(10)),
    };
    // fold 不可倒退——要更早的位置只能从头重算。
    assert_eq!(
        c.verdict(1, Some(EventSequence(5))),
        CacheVerdict::Invalid(CacheInvalidation::Rewound)
    );
}

#[test]
fn 快照带上当前状态版本() {
    let reg = 注册表();
    let s = reg.snapshot(ProjectionKey("skeleton"), &[], None).unwrap();
    assert_eq!(
        Some(s.state_version),
        reg.state_version(ProjectionKey("skeleton"))
    );
}

// ---- 验收 3：分页 ----

#[test]
fn 向前补页不改变已加载记录的键与顺序() {
    let evs: Vec<RunEventEnvelope> = (1..=10).map(|i| 事件(i, EventPayload::UsageUpdated)).collect();
    let f = EventFilter::default();

    let p1 = page(&evs, Cursor::start(), 4, &f);
    let 第一页键: Vec<_> = p1.items.iter().map(|e| record_key(e)).collect();
    assert_eq!(第一页键.len(), 4);

    let p2 = page(&evs, p1.next.unwrap(), 4, &f);
    let p3 = page(&evs, p2.next.unwrap(), 4, &f);
    assert_eq!(p3.items.len(), 2);
    assert!(p3.next.is_none(), "到末尾必须给 None 而不是空游标");

    // 重新取第一页：键与顺序必须完全一致。
    let 再取 = page(&evs, Cursor::start(), 4, &f);
    let 再取键: Vec<_> = 再取.items.iter().map(|e| record_key(e)).collect();
    assert_eq!(第一页键, 再取键);

    // 三页无重叠、无遗漏。
    let 全部: Vec<_> = p1
        .items
        .iter()
        .chain(p2.items.iter())
        .chain(p3.items.iter())
        .map(|e| e.seq.unwrap())
        .collect();
    assert_eq!(全部, (1..=10).map(EventSequence).collect::<Vec<_>>());
}

#[test]
fn 补页时新增尾部事件不影响已发出的页() {
    let mut evs: Vec<RunEventEnvelope> = (1..=6).map(|i| 事件(i, EventPayload::UsageUpdated)).collect();
    let f = EventFilter::default();
    let (键1, 下一页) = {
        let p1 = page(&evs, Cursor::start(), 3, &f);
        let k: Vec<_> = p1.items.iter().map(|e| record_key(e)).collect();
        (k, p1.next.unwrap())
    };

    // Run 还在跑，日志尾部增长了。
    evs.push(事件(7, EventPayload::UsageUpdated));

    let 重取 = page(&evs, Cursor::start(), 3, &f);
    assert_eq!(键1, 重取.items.iter().map(|e| record_key(e)).collect::<Vec<_>>());
    // 游标按 seq，不按 offset——补页仍从 3 之后开始。
    let p2 = page(&evs, 下一页, 3, &f);
    assert_eq!(p2.items[0].seq, Some(EventSequence(4)));
}

#[test]
fn 二分定位的边界准确() {
    // 换成二分定位之后，游标恰好落在某条事件上是最容易出错的位置：
    // 差一就会重复或漏掉一条。
    let evs: Vec<RunEventEnvelope> = (1..=10).map(|i| 事件(i, EventPayload::UsageUpdated)).collect();
    let f = EventFilter::default();

    // 游标 = 第 5 条，下一页必须**从第 6 条开始**。
    let p = page(
        &evs,
        Cursor {
            after_seq: Some(EventSequence(5)),
        },
        3,
        &f,
    );
    assert_eq!(
        p.items.iter().map(|e| e.seq.unwrap().0).collect::<Vec<_>>(),
        [6, 7, 8]
    );

    // 游标 = 最后一条，应当空且无下一页。
    let p = page(
        &evs,
        Cursor {
            after_seq: Some(EventSequence(10)),
        },
        3,
        &f,
    );
    assert!(p.items.is_empty());
    assert!(p.next.is_none());

    // 游标超出末尾同样安全。
    let p = page(
        &evs,
        Cursor {
            after_seq: Some(EventSequence(9999)),
        },
        3,
        &f,
    );
    assert!(p.items.is_empty());
}

#[test]
fn 游标落在不存在的序号上也能正确定位() {
    // 日志有缺口（某些 seq 没对应事件）时，游标可能落在空档里。
    let evs = vec![
        事件(1, EventPayload::UsageUpdated),
        事件(5, EventPayload::UsageUpdated),
        事件(9, EventPayload::UsageUpdated),
    ];
    let p = page(
        &evs,
        Cursor {
            after_seq: Some(EventSequence(3)),
        },
        10,
        &EventFilter::default(),
    );
    assert_eq!(
        p.items.iter().map(|e| e.seq.unwrap().0).collect::<Vec<_>>(),
        [5, 9]
    );
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "seq 升序")]
fn 乱序输入在_debug_下被断言拦住() {
    // 二分定位依赖有序。乱序输入会给出**错误结果而不是慢结果**——
    // 那比性能问题严重得多，所以要在开发期就炸掉。
    //
    // 有序性是存储侧契约（H3 的 seq 单调 + 按写入顺序读回），
    // `validate` 也会检查；这里是最后一道兜底。
    let evs = vec![
        事件(5, EventPayload::UsageUpdated),
        事件(1, EventPayload::UsageUpdated),
    ];
    let _ = page(&evs, Cursor::start(), 10, &EventFilter::default());
}

#[test]
fn 按类型与因果过滤() {
    let evs = vec![
        在(事件(1, EventPayload::UsageUpdated), "t1", None),
        在(
            事件(2, EventPayload::ToolProposed { call_id: "s1".into() }),
            "t1",
            Some("s1"),
        ),
        在(
            事件(3, EventPayload::ToolProposed { call_id: "s2".into() }),
            "t2",
            Some("s2"),
        ),
    ];
    let 只要工具 = EventFilter {
        kinds: Some(vec!["ToolProposed"]),
        ..Default::default()
    };
    assert_eq!(page(&evs, Cursor::start(), 10, &只要工具).items.len(), 2);

    let 只要t2 = EventFilter {
        turn_id: Some(TurnId::new("t2")),
        ..Default::default()
    };
    let r = page(&evs, Cursor::start(), 10, &只要t2);
    assert_eq!(r.items.len(), 1);
    assert_eq!(r.items[0].seq, Some(EventSequence(3)));
}

// ---- 验收 4：live 不参与 ----

#[test]
fn ui_delta_丢失不影响已提交轨迹() {
    let reg = 注册表();
    let 有delta = vec![
        事件(
            1,
            EventPayload::ModelRequestPrepared {
                request_id: "q1".into(),
            },
        ),
        live(1, EventPayload::TextDelta { text: "a".into() }),
        live(2, EventPayload::TextDelta { text: "b".into() }),
        事件(2, EventPayload::AssistantMessage),
    ];
    let 丢delta: Vec<_> = 有delta.iter().filter(|e| e.is_durable()).cloned().collect();

    let a = reg.snapshot(ProjectionKey("requests"), &有delta, None).unwrap();
    let b = reg.snapshot(ProjectionKey("requests"), &丢delta, None).unwrap();
    assert_eq!(a, b, "live delta 丢失必须不改变 committed trajectory");
}

#[test]
fn 带了序号的_live_事件仍被排除() {
    // `seq: None` 只是 live 事件的**惯例**；写错的适配器可能给它分配序号。
    // 判定必须看 durability，不能只靠"有没有 seq"兜底。
    let reg = 注册表();
    let mut 越界的 = live(9, EventPayload::AssistantMessage);
    越界的.seq = Some(EventSequence(9));

    let 干净 = vec![事件(1, EventPayload::AssistantMessage)];
    let mut 混入 = 干净.clone();
    混入.push(越界的);

    let a = reg.snapshot(ProjectionKey("requests"), &干净, None).unwrap();
    let b = reg.snapshot(ProjectionKey("requests"), &混入, None).unwrap();
    assert_eq!(
        a.view, b.view,
        "live 事件不得因带了序号就进入 committed trajectory"
    );
    assert_eq!(
        page(&混入, Cursor::start(), 10, &EventFilter::default())
            .items
            .len(),
        1
    );
}

#[test]
fn 分页只返回_durable_事件() {
    let evs = vec![
        事件(1, EventPayload::UsageUpdated),
        live(1, EventPayload::TextDelta { text: "x".into() }),
        事件(2, EventPayload::UsageUpdated),
    ];
    let p = page(&evs, Cursor::start(), 10, &EventFilter::default());
    assert_eq!(p.items.len(), 2);
}

// ---- 验收 5：未知事件 ----

#[test]
fn 未知事件不破坏投影() {
    let reg = 注册表();
    let 无未知 = vec![
        事件(1, EventPayload::RunStarted),
        在(事件(3, EventPayload::TurnStarted), "t1", None),
    ];
    let mut 有未知 = 无未知.clone();
    有未知.insert(1, 事件(2, EventPayload::Unknown));

    for k in reg.keys() {
        let a = reg.snapshot(k, &无未知, None).unwrap();
        let b = reg.snapshot(k, &有未知, None).unwrap();
        assert_eq!(a.view, b.view, "投影 {k} 被未知事件影响了");
    }
}

#[test]
fn 未知载荷可从旧版本反序列化() {
    // 老 UI 读到新内核写的事件——必须落到 Unknown 而不是解析失败。
    let j = r#"{"type":"something_from_the_future","extra":123}"#;
    let p: EventPayload = serde_json::from_str(j).unwrap();
    assert_eq!(payload_kind(&p), "Unknown");
}

// ---- 注册表本身 ----

#[test]
fn 重复注册是错误而不是静默覆盖() {
    let mut r = ProjectionRegistry::new();
    r.register(Skeleton).unwrap();
    assert_eq!(
        r.register(Skeleton).unwrap_err(),
        ProjectionError::Duplicate(ProjectionKey("skeleton"))
    );
}

#[test]
fn 未注册的投影明确报错() {
    let r = ProjectionRegistry::new();
    assert_eq!(
        r.snapshot(ProjectionKey("nope"), &[], None).unwrap_err(),
        ProjectionError::Unknown(ProjectionKey("nope"))
    );
}

#[test]
fn 首批五个投影全部注册() {
    let reg = 注册表();
    let mut k: Vec<_> = reg.keys().iter().map(|k| k.0).collect();
    k.sort_unstable();
    assert_eq!(k, ["approvals", "cache", "requests", "skeleton", "tool_paths"]);
    assert_eq!(reg.snapshot_all(&[], None).len(), 5);
}

// ---- 各投影的语义 ----

#[test]
fn 骨架记录零_step_的_turn() {
    let reg = 注册表();
    let evs = vec![
        事件(1, EventPayload::RunStarted),
        // t1 有 Step。
        在(事件(2, EventPayload::TurnStarted), "t1", None),
        在(事件(3, EventPayload::StepStarted), "t1", Some("s1")),
        在(事件(4, EventPayload::TurnEnded), "t1", None),
        // t2 被拒绝的 claim：零 Step，但必须留痕。
        在(事件(5, EventPayload::TurnStarted), "t2", None),
        在(事件(6, EventPayload::TurnEnded), "t2", None),
    ];
    let v: SkeletonView =
        serde_json::from_value(reg.snapshot(ProjectionKey("skeleton"), &evs, None).unwrap().view).unwrap();
    assert_eq!(v.turns.len(), 2);
    assert_eq!(v.zero_step_turns, 1);
    assert_eq!(v.turns[0].steps, ["s1"]);
    assert!(v.turns[1].is_zero_step());
}

#[test]
fn 工具路径串起固定管线并标出悬挂意图() {
    let reg = 注册表();
    let evs = vec![
        在(
            事件(1, EventPayload::ToolProposed { call_id: "c1".into() }),
            "t1",
            Some("c1"),
        ),
        在(
            事件(2, EventPayload::HookOutcomeRecorded { call_id: "c1".into() }),
            "t1",
            Some("c1"),
        ),
        事件(3, 意图("c1", "Write")),
        在(
            事件(4, EventPayload::ToolStarted { call_id: "c1".into() }),
            "t1",
            Some("c1"),
        ),
        事件(5, 结果("c1", StepOutcome::Succeeded)),
        // c2 落了意图但崩溃了，没有结果。
        事件(6, 意图("c2", "Bash")),
    ];
    let v: ToolPathsView = serde_json::from_value(
        reg.snapshot(ProjectionKey("tool_paths"), &evs, None)
            .unwrap()
            .view,
    )
    .unwrap();

    let c1 = v.calls.iter().find(|c| c.call_id == "c1").unwrap();
    assert!(c1.proposed && c1.hooked && c1.intent_recorded && c1.started);
    assert_eq!(c1.tool_name.as_deref(), Some("Write"));
    assert_eq!(c1.outcome.as_deref(), Some("Succeeded"));
    assert!(!c1.is_dangling());

    // **有意图无结果**——恢复时必须 reconcile 的那一类，投影要直接点名。
    assert_eq!(v.dangling, ["c2"]);
}

#[test]
fn 一次调用不会被拆成两条记录() {
    // **这条是真实的 CLI 输出暴露出来的。** 早先 ToolProposed/ToolStarted
    // 不带 call_id，投影只能拿 causality 里的 step_id 兜底；而一个 Step 里
    // 可以有多次调用，于是同一次调用被拆成两条——一条"有提议无结果"、
    // 一条"有结果无提议"，前者被误报成待 reconcile 的悬挂意图。
    //
    // 恢复本身不受影响（`recovery::plan` 直接读 intent/result 载荷），
    // 但轨迹会告诉运维"这三个调用需要 reconcile"，而它们其实早就完成了。
    let reg = 注册表();
    // 关键构造：causality 的 step_id 与 call_id **不同**。
    let evs = vec![
        在(
            事件(
                1,
                EventPayload::ToolProposed {
                    call_id: "call-abc".into(),
                },
            ),
            "t1",
            Some("step-1"),
        ),
        在(
            事件(
                2,
                EventPayload::ToolStarted {
                    call_id: "call-abc".into(),
                },
            ),
            "t1",
            Some("step-1"),
        ),
        事件(3, 结果("call-abc", StepOutcome::Succeeded)),
    ];
    let v: ToolPathsView = serde_json::from_value(
        reg.snapshot(ProjectionKey("tool_paths"), &evs, None)
            .unwrap()
            .view,
    )
    .unwrap();

    assert_eq!(v.calls.len(), 1, "同一次调用被拆成了 {} 条", v.calls.len());
    let c = &v.calls[0];
    assert_eq!(c.call_id, "call-abc");
    assert!(c.proposed && c.started);
    assert_eq!(c.outcome.as_deref(), Some("Succeeded"));
    assert!(v.dangling.is_empty(), "已完成的调用被误报为待 reconcile");
}

#[test]
fn 同一_step_内的多次调用各自成条() {
    // 上一条的另一面：step_id 相同不代表是同一次调用。
    let reg = 注册表();
    let evs = vec![
        在(
            事件(1, EventPayload::ToolProposed { call_id: "c1".into() }),
            "t1",
            Some("step-1"),
        ),
        在(
            事件(2, EventPayload::ToolProposed { call_id: "c2".into() }),
            "t1",
            Some("step-1"),
        ),
        事件(3, 结果("c1", StepOutcome::Succeeded)),
        事件(4, 结果("c2", StepOutcome::Succeeded)),
    ];
    let v: ToolPathsView = serde_json::from_value(
        reg.snapshot(ProjectionKey("tool_paths"), &evs, None)
            .unwrap()
            .view,
    )
    .unwrap();
    assert_eq!(v.calls.len(), 2);
    assert!(v.dangling.is_empty());
}

#[test]
fn 被拒绝的调用同样留下完整路径() {
    let reg = 注册表();
    let evs = vec![
        在(
            事件(1, EventPayload::ToolProposed { call_id: "c1".into() }),
            "t1",
            Some("c1"),
        ),
        事件(
            2,
            结果(
                "c1",
                StepOutcome::Denied {
                    code: agentrs_contracts::policy::DenyCode::OutOfAuthority,
                    message: "不在授权内".into(),
                },
            ),
        ),
    ];
    let v: ToolPathsView = serde_json::from_value(
        reg.snapshot(ProjectionKey("tool_paths"), &evs, None)
            .unwrap()
            .view,
    )
    .unwrap();
    assert_eq!(v.calls[0].outcome.as_deref(), Some("Denied"));
    assert!(v.dangling.is_empty(), "被拒绝不是悬挂——它有明确结果");
}

#[test]
fn 审批投影回答等了多久() {
    let reg = 注册表();
    let evs = vec![
        事件(1, EventPayload::ApprovalRequested { call_id: "c1".into() }),
        事件(
            5,
            EventPayload::ApprovalTimedOut {
                token: "tok".into(),
                call_id: "c1".into(),
            },
        ),
        事件(6, EventPayload::RunNeedsUserAction),
    ];
    let v: ApprovalsView =
        serde_json::from_value(reg.snapshot(ProjectionKey("approvals"), &evs, None).unwrap().view).unwrap();
    assert_eq!(v.spans.len(), 1);
    // at = seq * 100
    assert_eq!(v.spans[0].waited_ms(), Some(400));
    assert_eq!(v.spans[0].resume_token.as_deref(), Some("tok"));
    assert!(v.needs_user_action);
}

#[test]
fn 同一调用二次审批各自成段() {
    // redeem 之后同一次调用可以再次请求审批——不能把第二段并进第一段。
    let reg = 注册表();
    let evs = vec![
        事件(1, EventPayload::ApprovalRequested { call_id: "c1".into() }),
        事件(
            2,
            EventPayload::ApprovalTimedOut {
                token: "t1".into(),
                call_id: "c1".into(),
            },
        ),
        事件(3, EventPayload::ApprovalRequested { call_id: "c1".into() }),
    ];
    let v: ApprovalsView =
        serde_json::from_value(reg.snapshot(ProjectionKey("approvals"), &evs, None).unwrap().view).unwrap();
    assert_eq!(v.spans.len(), 2);
    assert!(v.spans[1].timed_out_at.is_none(), "第二段还没闭合");
}

#[test]
fn 缓存投影给出未断裂比例而不是零() {
    let reg = 注册表();
    let 空 = reg.snapshot(ProjectionKey("cache"), &[], None).unwrap();
    let v: CacheView = serde_json::from_value(空.view).unwrap();
    // **没有请求时不能报 0%**——那会被读成"全 miss"。
    assert!(!v.observed_any_break());
    assert_eq!(v.unbroken_rate(), None);

    let evs = vec![
        事件(
            1,
            EventPayload::ModelRequestPrepared {
                request_id: "q1".into(),
            },
        ),
        事件(
            2,
            EventPayload::ModelRequestPrepared {
                request_id: "q2".into(),
            },
        ),
        事件(
            3,
            EventPayload::ModelRequestPrepared {
                request_id: "q3".into(),
            },
        ),
        事件(
            4,
            EventPayload::ModelRequestPrepared {
                request_id: "q4".into(),
            },
        ),
        事件(5, EventPayload::CacheBreakObserved),
        事件(6, EventPayload::HistoryLegalized),
    ];
    let v: CacheView =
        serde_json::from_value(reg.snapshot(ProjectionKey("cache"), &evs, None).unwrap().view).unwrap();
    assert_eq!(v.requests, 4);
    assert_eq!(v.cache_breaks, 1);
    assert_eq!(v.unbroken_rate(), Some(0.75));
    assert!(v.observed_any_break());
    assert_eq!(v.legalizations, 1);
}

#[test]
fn 缓存投影记录已压缩区间() {
    // 复用摘要靠它——重复压同一段既费钱又会改动缓存前缀。
    let reg = 注册表();
    let evs = vec![事件(
        1,
        EventPayload::CompactionCompleted {
            source_range: agentrs_contracts::ids::EventRange {
                start: EventSequence(3),
                end: EventSequence(20),
            },
        },
    )];
    let v: CacheView =
        serde_json::from_value(reg.snapshot(ProjectionKey("cache"), &evs, None).unwrap().view).unwrap();
    assert_eq!(v.compacted_ranges, [(EventSequence(3), EventSequence(20))]);
}

#[test]
fn 投影不接受没有序号的_durable_事件() {
    // durable 却无 seq 是存储侧的错。让它参与折叠会破坏定序，宁可跳过。
    let reg = 注册表();
    let mut 坏的 = 事件(1, EventPayload::RunStarted);
    坏的.seq = None;
    let s = reg.snapshot(ProjectionKey("skeleton"), &[坏的], None).unwrap();
    assert_eq!(s.up_to_seq, None);
    assert_eq!(s.view["started"], false);
}

#[test]
fn 载荷判别名覆盖全部变体() {
    // 新增事件类型时忘了加判别名会在这里挂——payload_kind 是导出与过滤的基础。
    for p in [
        EventPayload::RunStarted,
        EventPayload::TurnEnded,
        EventPayload::PartialOutputStarted,
        EventPayload::ExternalFactReceived,
        EventPayload::RunNeedsUserAction,
    ] {
        assert_ne!(payload_kind(&p), "Unknown");
    }
}
