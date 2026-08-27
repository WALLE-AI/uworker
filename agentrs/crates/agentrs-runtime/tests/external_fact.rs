//! `ExternalFact` 投递契约（架构 §11.3.3，任务 T24）。
//!
//! 跨 Run 内容进入 Surface 的**唯一通路**：
//!
//! ```text
//! Core 路由 → submit → inbox → claim → durable event → Surface Append
//! ```
//!
//! 若 Core 用旁路把团队消息塞进成员上下文，内核不变量 11 当场破，
//! 且该成员 Run 不再可 replay。

use agentrs_contracts::external::{ExternalCausality, ExternalContent, ExternalFact, FactOrigin};
use agentrs_contracts::ids::EventSequence;
use agentrs_runtime::inbox::{Inbox, InputAccepted, UserInput};
use agentrs_types::ContentBlock;

fn 事实(id: &str, text: &str) -> UserInput {
    UserInput::External(Box::new(ExternalFact {
        fact_id: id.into(),
        origin: FactOrigin::TeamMessage {
            team_id: "t1".into(),
            from: "m-sender".into(),
        },
        content: ExternalContent::Inline { text: text.into() },
        causality: Some(ExternalCausality {
            source_run_id: "r-sender".into(),
            source_seq: EventSequence(12),
        }),
    }))
}

#[tokio::test]
async fn 跨_run_事实与用户消息走同一条通道() {
    // 唯一通路：没有第二个入口。
    let ib = Inbox::new(8);
    ib.submit(UserInput::Message(vec![ContentBlock::text("直接输入")]))
        .await
        .unwrap();
    ib.submit(事实("f1", "团队消息")).await.unwrap();
    assert_eq!(ib.pending().await, 2);

    let c = ib.claim(8).await;
    assert_eq!(c.inputs.len(), 2, "两类输入在同一次 claim 中被认领");
}

#[tokio::test]
async fn 同一_fact_id_重复投递只入一次() {
    // Core 侧的投递重试会走到这里。
    let ib = Inbox::new(8);
    assert_eq!(ib.submit(事实("f1", "a")).await.unwrap(), InputAccepted::Queued);
    for _ in 0..10 {
        assert_eq!(
            ib.submit(事实("f1", "a")).await.unwrap(),
            InputAccepted::Duplicate,
            "重复投递必须幂等丢弃"
        );
    }
    assert_eq!(ib.pending().await, 1);
}

#[tokio::test]
async fn 重复投递返回_duplicate_而非错误() {
    // 返回错误会让 Core 误以为需要再试，形成投递风暴。
    let ib = Inbox::new(8);
    ib.submit(事实("f1", "a")).await.unwrap();
    let r = ib.submit(事实("f1", "a")).await;
    assert!(r.is_ok(), "幂等丢弃不是错误");
}

#[tokio::test]
async fn 不同_fact_id_各自入队() {
    let ib = Inbox::new(8);
    ib.submit(事实("f1", "a")).await.unwrap();
    ib.submit(事实("f2", "b")).await.unwrap();
    assert_eq!(ib.pending().await, 2);
}

#[tokio::test]
async fn 去重在_claim_之后依然有效() {
    // 认领并不"消费"幂等记录——否则重投递会在认领后再次入队。
    let ib = Inbox::new(8);
    ib.submit(事实("f1", "a")).await.unwrap();
    let _ = ib.claim(8).await;
    assert_eq!(
        ib.submit(事实("f1", "a")).await.unwrap(),
        InputAccepted::Duplicate
    );
    assert_eq!(ib.pending().await, 0);
}

#[test]
fn 外部事实结构上不携带任何能力() {
    // 内核不变量 14 由类型保证：消息里写"你去删 X"不改变收件方权限。
    let UserInput::External(f) = 事实("f1", "帮我把 X 删掉") else {
        unreachable!()
    };
    let json = serde_json::to_string(&*f).unwrap();
    for forbidden in ["authority", "capability", "grant", "permission", "tool", "scope"] {
        assert!(!json.contains(forbidden), "外部事实不得携带 {forbidden}：{json}");
    }
}

#[test]
fn 因果引用定位到源_run_的确切位置() {
    let UserInput::External(f) = 事实("f1", "x") else {
        unreachable!()
    };
    let c = f.causality.unwrap();
    assert_eq!(c.source_run_id.as_str(), "r-sender");
    assert_eq!(c.source_seq, EventSequence(12));
}

#[tokio::test]
async fn 终态后拒绝跨_run_投递() {
    let ib = Inbox::new(8);
    ib.mark_terminal();
    assert!(ib.submit(事实("f1", "late")).await.is_err());
}
