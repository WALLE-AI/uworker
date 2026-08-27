//! 宿主义务 H3 的契约测试（架构 §12.2）。
//!
//! > `RunPersistence` 必须实现 `event_id` 幂等、`seq` 单调、epoch 围栏、checkpoint 写序。
//! > 违反后果：恢复与 replay 全部不可信。
//!
//! 内核**无法**阻止一个错误的 adapter 实现，因此这些必须由宿主自测。
//! 本文件是 host conformance suite 的雏形——将来抽成可被任意宿主 adapter 复用的套件，
//! 现在先以 `FakePersistence` 为被测对象把用例写实。

use agentrs_contracts::event::{Durability, EventPayload, RunEventEnvelope, Visibility};
use agentrs_contracts::ids::{Digest, EventSequence, RunEpoch, Timestamp};
use agentrs_contracts::policy::InputHash;
use agentrs_contracts::ports::{PersistError, RunPersistence};
use agentrs_contracts::spec::RunCheckpoint;
use agentrs_contracts::version::SpecVersion;
use agentrs_contracts::StepIntent;
use agentrs_testkit::{FakePersistence, InjectedFault};

fn 事件(id: &str) -> RunEventEnvelope {
    RunEventEnvelope {
        run_id: "r1".into(),
        epoch: RunEpoch(1),
        event_id: id.into(),
        seq: None,
        live_seq: None,
        at: Timestamp(0),
        durability: Durability::DurableFact,
        visibility: Visibility::User,
        causality: Default::default(),
        surface: None,
        payload: EventPayload::RunStarted,
    }
}

fn 意图() -> StepIntent {
    StepIntent {
        step_id: "s1".into(),
        call_id: "c1".into(),
        tool_name: "Read".into(),
        input_hash: InputHash(Digest::from_hex("h")),
        change_set_id: "cs1".into(),
        execution_id: "e1".into(),
        at: Timestamp(0),
    }
}

#[tokio::test]
async fn h3_1_重复投递同一事件只写一条且序号不变() {
    let p = FakePersistence::new();
    let first = p.append_event(RunEpoch(1), 事件("e-1")).await.unwrap();

    // 重投递 100 次——网络重试、崩溃恢复重放都会产生这种情形。
    for _ in 0..100 {
        let again = p.append_event(RunEpoch(1), 事件("e-1")).await.unwrap();
        assert_eq!(again, first, "重复投递必须返回首次分配的序号");
    }

    assert_eq!(p.event_count(), 1, "只能有一条记录；否则消费端去重失效");
}

#[tokio::test]
async fn h3_2_序号单调递增() {
    let p = FakePersistence::new();
    let mut prev = EventSequence(0);
    for i in 0..10 {
        let seq = p
            .append_event(RunEpoch(1), 事件(&format!("e-{i}")))
            .await
            .unwrap();
        assert!(seq > prev, "seq 必须单调：{seq:?} 应大于 {prev:?}");
        prev = seq;
    }
}

#[tokio::test]
async fn h3_3_旧_epoch_写入被围栏拒绝() {
    let p = FakePersistence::new();

    // 新 writer 以 epoch 2 接管（模拟 resume 后重新分配 epoch）。
    p.append_event(RunEpoch(2), 事件("new-1")).await.unwrap();

    // 旧进程"复活"，仍持 epoch 1 —— 必须被拒绝。
    let 结果 = p.append_event(RunEpoch(1), 事件("stale-1")).await;
    assert!(matches!(结果, Err(PersistError::Fenced)), "旧 epoch 必须 Fenced");

    // 意图与结果写入同样受围栏保护，否则副作用记录会被污染。
    assert!(matches!(
        p.begin_step(RunEpoch(1), 意图()).await,
        Err(PersistError::Fenced)
    ));

    assert_eq!(p.event_count(), 1, "被围栏的写入不得落盘");
}

#[tokio::test]
async fn h3_3b_同_epoch_与更新_epoch_均可写入() {
    let p = FakePersistence::new();
    p.append_event(RunEpoch(2), 事件("a")).await.unwrap();
    p.append_event(RunEpoch(2), 事件("b")).await.unwrap();
    p.append_event(RunEpoch(3), 事件("c")).await.unwrap();
    assert_eq!(p.event_count(), 3);
}

#[tokio::test]
async fn h3_4_checkpoint_不得早于其引用的事件() {
    let p = FakePersistence::new();
    p.append_event(RunEpoch(1), 事件("e-1")).await.unwrap();

    // 指向尚未持久化的序号 —— 恢复时会读到"指向未来"的游标。
    let 超前 = RunCheckpoint {
        spec_version: SpecVersion(1),
        up_to_seq: EventSequence(99),
        pending_approval: None,
    };
    assert!(matches!(
        p.save_checkpoint(RunEpoch(1), 超前).await,
        Err(PersistError::CheckpointAhead)
    ));

    // 指向已提交的序号 —— 允许。
    let 合法 = RunCheckpoint {
        spec_version: SpecVersion(1),
        up_to_seq: EventSequence(1),
        pending_approval: None,
    };
    p.save_checkpoint(RunEpoch(1), 合法).await.unwrap();
    assert!(p.checkpoint().is_some());
}

#[tokio::test]
async fn h3_5_故障后重试不产生重复记录() {
    let p = FakePersistence::new();

    // 第一次写入失败（模拟崩溃前的后端错误）。
    p.inject(InjectedFault::BackendError);
    assert!(p.append_event(RunEpoch(1), 事件("e-1")).await.is_err());
    assert_eq!(p.event_count(), 0, "失败的写入不得落盘");

    // 重试成功。
    let seq = p.append_event(RunEpoch(1), 事件("e-1")).await.unwrap();
    assert_eq!(p.event_count(), 1);

    // 恢复后的重放再次投递同一事件 —— 幂等兜住。
    let again = p.append_event(RunEpoch(1), 事件("e-1")).await.unwrap();
    assert_eq!(again, seq);
    assert_eq!(p.event_count(), 1, "恢复重放不得写出第二条");
}

#[tokio::test]
async fn 坏_adapter_能被本套件识别() {
    // 反向验证：本套件的用例确实在检查行为，而不是空跑。
    // 一个"不做幂等"的实现会在 h3_1 的第二次投递处产生第二条记录。
    let p = FakePersistence::new();
    p.append_event(RunEpoch(1), 事件("x")).await.unwrap();
    p.append_event(RunEpoch(1), 事件("y")).await.unwrap();
    assert_eq!(p.event_count(), 2, "不同 event_id 本就应该写两条");

    let 同一 = p.append_event(RunEpoch(1), 事件("x")).await.unwrap();
    assert_eq!(同一, EventSequence(1));
    assert_eq!(p.event_count(), 2, "相同 event_id 不增加记录");
}
