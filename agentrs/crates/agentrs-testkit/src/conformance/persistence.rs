//! H3：`RunPersistence` 的宿主义务检查。
//!
//! > `event_id` 幂等、`seq` 单调、epoch 围栏、checkpoint 写序。
//! > 违反后果：**恢复与 replay 全部不可信**。
//!
//! 这一条的后果比其余几条都严重，因为它破坏的是"durable log 是唯一事实源"
//! 这个前提本身。前提一破，其余所有不变量都失去了依据——
//! 你没法在一份自相矛盾的日志上讨论"恢复时以它为准"。
//!
//! ## 四条各自防什么
//!
//! | 检查 | 防什么 |
//! |---|---|
//! | `event_id` 幂等 | 重试造成重复事实。消费端去重键是 `(run_id, event_id)`，存储侧再重复一份就白搭 |
//! | `seq` 单调 | 投影与分页游标失效——两条事件谁先谁后无法回答 |
//! | epoch 围栏 | 两个 writer 同时写。**这是最隐蔽的一条**：不围栏时双写不报错，日志静静地烂掉 |
//! | checkpoint 写序 | checkpoint 指向尚未落盘的事件，恢复时读到一个不存在的位置 |

use std::sync::Arc;

use agentrs_contracts::event::{Causality, Durability, EventPayload, RunEventEnvelope, Visibility};
use agentrs_contracts::ids::{EventSequence, RunEpoch, RunId, Timestamp};
use agentrs_contracts::ports::{PersistError, RunPersistence};
use agentrs_contracts::spec::RunCheckpoint;
use agentrs_contracts::version::SpecVersion;

use super::{Check, Outcome, Report};

/// 被检查的持久化实现。
pub trait PersistenceSubject: Send + Sync {
    /// 待检查的实现。**每次调用应返回同一个实例。**
    fn persistence(&self) -> Arc<dyn RunPersistence>;

    /// 本次检查使用的 Run 标识。
    fn run_id(&self) -> RunId {
        RunId::new("conf-run")
    }

    /// 分配一个比 `current` 更新的 epoch，模拟"另一个 writer 接管了"。
    ///
    /// 返回 `None` 表示实现不支持外部推进 epoch——
    /// 围栏检查随之记为 `Skipped` 而非 `Pass`。
    fn advance_epoch(&self, _current: RunEpoch) -> Option<RunEpoch> {
        None
    }
}

fn 事件(run_id: &RunId, event_id: &str) -> RunEventEnvelope {
    RunEventEnvelope {
        run_id: run_id.clone(),
        epoch: RunEpoch(1),
        event_id: event_id.into(),
        seq: None,
        live_seq: None,
        at: Timestamp(0),
        durability: Durability::DurableFact,
        visibility: Visibility::User,
        causality: Causality::default(),
        surface: None,
        payload: EventPayload::UsageUpdated,
    }
}

/// 对一个持久化实现跑 H3 的全部检查。
pub async fn check_persistence(subject: &dyn PersistenceSubject) -> Report {
    let p = subject.persistence();
    let run = subject.run_id();
    let e1 = RunEpoch(1);
    let mut checks = Vec::new();

    let mut record = |name: &'static str, consequence: &'static str, outcome: Outcome| {
        checks.push(Check {
            obligation: "H3",
            name,
            consequence,
            outcome,
        });
    };

    // ---- 基线：能不能正常写 ----
    let 基线 = p.append_event(e1, 事件(&run, "conf-base")).await;
    record(
        "可以追加事件",
        "后续所有检查都会假通过",
        match &基线 {
            Ok(_) => Outcome::Pass,
            Err(e) => Outcome::Fail {
                detail: format!("基线写入失败：{e}"),
            },
        },
    );
    let Ok(base_seq) = 基线 else {
        return Report { checks };
    };

    // ---- event_id 幂等 ----
    {
        // 同一个 event_id 投递两次：必须命中同一条记录，序号也必须一样。
        // 返回一个新序号等于凭空多了一条事实。
        let again = p.append_event(e1, 事件(&run, "conf-base")).await;
        let outcome = match again {
            Ok(s) if s == base_seq => Outcome::Pass,
            Ok(s) => Outcome::Fail {
                detail: format!("重复投递分配了新序号 {s:?}（首次为 {base_seq:?}）"),
            },
            Err(e) => Outcome::Fail {
                detail: format!("重复投递报错而不是幂等命中：{e}"),
            },
        };
        record("event_id 幂等", "重试造成重复事实，消费端去重也救不回来", outcome);
    }

    // ---- seq 单调 ----
    {
        let mut seqs = Vec::new();
        let mut err = None;
        for i in 0..4 {
            match p.append_event(e1, 事件(&run, &format!("conf-mono-{i}"))).await {
                Ok(s) => seqs.push(s),
                Err(e) => {
                    err = Some(e.to_string());
                    break;
                }
            }
        }
        let outcome = if let Some(e) = err {
            Outcome::Fail {
                detail: format!("写入中断：{e}"),
            }
        } else if seqs.windows(2).all(|w| w[0] < w[1]) && seqs.first() > Some(&base_seq) {
            Outcome::Pass
        } else {
            Outcome::Fail {
                detail: format!("序号非严格递增：{seqs:?}（基线 {base_seq:?}）"),
            }
        };
        record(
            "seq 严格单调递增",
            "投影与分页游标失效：两条事件谁先谁后无法回答",
            outcome,
        );
    }

    // ---- epoch 围栏 ----
    {
        let outcome = match subject.advance_epoch(e1) {
            None => Outcome::Skipped {
                why: "实现不支持外部推进 epoch".into(),
            },
            Some(newer) => {
                // 新 writer 先写一条，确认它能写。
                let 新的 = p.append_event(newer, 事件(&run, "conf-fence-new")).await;
                if 新的.is_err() {
                    Outcome::Fail {
                        detail: format!("更新的 epoch 反而写不进去：{:?}", 新的.err()),
                    }
                } else {
                    // 旧 writer 再写：**必须被围栏挡住**。
                    let 旧的 = p.append_event(e1, 事件(&run, "conf-fence-old")).await;
                    match 旧的 {
                        Err(PersistError::Fenced) => Outcome::Pass,
                        Err(e) => Outcome::Fail {
                            detail: format!("旧 epoch 被拒但错误码不是 Fenced：{e}"),
                        },
                        Ok(s) => Outcome::Fail {
                            detail: format!("旧 epoch 仍能写入（序号 {s:?}）——双 writer 会把日志写坏"),
                        },
                    }
                }
            }
        };
        record(
            "epoch 围栏拒绝旧 writer",
            "两个 writer 同时写且不报错，日志静静地烂掉",
            outcome,
        );
    }

    // ---- checkpoint 写序 ----
    {
        // checkpoint 指向一个远未落盘的位置：必须拒绝。
        let 越界 = RunCheckpoint {
            spec_version: SpecVersion(1),
            up_to_seq: EventSequence(u64::MAX / 2),
            pending_approval: None,
        };
        // 围栏检查可能已经推进了 epoch，这里用当前有效的那个。
        let epoch = subject.advance_epoch(e1).unwrap_or(e1);
        let outcome = match p.save_checkpoint(epoch, 越界).await {
            Err(PersistError::CheckpointAhead) => Outcome::Pass,
            Err(e) => Outcome::Fail {
                detail: format!("超前 checkpoint 被拒但错误码不是 CheckpointAhead：{e}"),
            },
            Ok(()) => Outcome::Fail {
                detail: "接受了指向未落盘事件的 checkpoint".into(),
            },
        };
        record("拒绝超前的 checkpoint", "恢复时读到一个不存在的位置", outcome);
    }

    Report { checks }
}
