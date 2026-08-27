//! H3 conformance suite 自身的验证：每条检查都要有能让它失败的实现。
//!
//! 与 sandbox 那组同一套路。H3 的检查尤其需要这层验证——
//! **它的四条里有三条在"违反了也不报错"的情况下才危险**：
//! 重复事实、序号乱序、双 writer 都不会当场炸，只会让日后的恢复读出一份
//! 自相矛盾的历史。套件是唯一能在事发前发现它们的东西。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agentrs_contracts::event::RunEventEnvelope;
use agentrs_contracts::ids::{EventSequence, RunEpoch};
use agentrs_contracts::ports::{PersistError, RunPersistence};
use agentrs_contracts::spec::RunCheckpoint;
use agentrs_contracts::{StepIntent, StepResult};
use agentrs_testkit::conformance::persistence::{check_persistence, PersistenceSubject};
use agentrs_testkit::conformance::Report;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct 缺陷 {
    重复投递分配新序号: bool,
    序号不单调: bool,
    不做_epoch_围栏: bool,
    接受超前_checkpoint: bool,
}

#[derive(Default)]
struct 状态 {
    seen: HashMap<String, EventSequence>,
    next: u64,
    highest_epoch: u64,
}

struct 可控存储 {
    缺陷: 缺陷,
    st: Mutex<状态>,
}

impl 可控存储 {
    fn new(缺陷: 缺陷) -> Arc<Self> {
        Arc::new(Self {
            缺陷,
            st: Mutex::new(状态 {
                highest_epoch: 1,
                ..Default::default()
            }),
        })
    }
}

#[async_trait::async_trait]
impl RunPersistence for 可控存储 {
    async fn begin_step(&self, _e: RunEpoch, _i: StepIntent) -> Result<(), PersistError> {
        Ok(())
    }

    async fn append_event(
        &self,
        epoch: RunEpoch,
        event: RunEventEnvelope,
    ) -> Result<EventSequence, PersistError> {
        let mut st = self.st.lock().unwrap();
        if !self.缺陷.不做_epoch_围栏 {
            if epoch.0 < st.highest_epoch {
                return Err(PersistError::Fenced);
            }
            st.highest_epoch = epoch.0;
        }

        let key = event.event_id.as_str().to_string();
        if !self.缺陷.重复投递分配新序号 {
            if let Some(s) = st.seen.get(&key) {
                return Ok(*s);
            }
        }

        st.next += 1;
        // 序号不单调那一款：每次倒着发，制造"后写的反而更小"。
        let seq = if self.缺陷.序号不单调 {
            EventSequence(1000 - st.next)
        } else {
            EventSequence(st.next)
        };
        st.seen.insert(key, seq);
        Ok(seq)
    }

    async fn finish_step(&self, _e: RunEpoch, _r: StepResult) -> Result<(), PersistError> {
        Ok(())
    }

    async fn save_checkpoint(&self, _e: RunEpoch, c: RunCheckpoint) -> Result<(), PersistError> {
        if self.缺陷.接受超前_checkpoint {
            return Ok(());
        }
        let st = self.st.lock().unwrap();
        if c.up_to_seq.0 > st.next {
            return Err(PersistError::CheckpointAhead);
        }
        Ok(())
    }
}

struct 受检对象(Arc<可控存储>);

impl PersistenceSubject for 受检对象 {
    fn persistence(&self) -> Arc<dyn RunPersistence> {
        self.0.clone()
    }

    fn advance_epoch(&self, current: RunEpoch) -> Option<RunEpoch> {
        Some(RunEpoch(current.0 + 1))
    }
}

async fn 跑(缺陷: 缺陷) -> Report {
    check_persistence(&受检对象(可控存储::new(缺陷))).await
}

fn 只有这些不合格(r: &Report, 期望: &[&str]) {
    let 实际: Vec<&str> = r.failures().iter().map(|c| c.name).collect();
    assert_eq!(实际, 期望, "捕获的违规项与预期不符\n{}", r.render());
    assert!(r.skipped().is_empty(), "本组不应产生跳过项\n{}", r.render());
}

#[tokio::test]
async fn 合格的存储实现全部通过() {
    let r = 跑(缺陷::default()).await;
    assert!(r.passed(), "合格实现被误判\n{}", r.render());
    assert!(r.skipped().is_empty(), "{}", r.render());
}

#[tokio::test]
async fn 捕获_重复投递分配了新序号() {
    // 消费端的去重键是 (run_id, event_id)。存储侧再重复一份，
    // 去重也救不回来——两条记录的 event_id 相同、seq 不同，谁是真的？
    只有这些不合格(
        &跑(缺陷 {
            重复投递分配新序号: true,
            ..Default::default()
        })
        .await,
        &["event_id 幂等"],
    );
}

#[tokio::test]
async fn 捕获_序号不单调() {
    只有这些不合格(
        &跑(缺陷 {
            序号不单调: true,
            ..Default::default()
        })
        .await,
        &["seq 严格单调递增"],
    );
}

#[tokio::test]
async fn 捕获_没有_epoch_围栏() {
    // **最隐蔽的一条**：不围栏时双写不报错，日志静静地烂掉。
    只有这些不合格(
        &跑(缺陷 {
            不做_epoch_围栏: true,
            ..Default::default()
        })
        .await,
        &["epoch 围栏拒绝旧 writer"],
    );
}

#[tokio::test]
async fn 捕获_接受超前的_checkpoint() {
    只有这些不合格(
        &跑(缺陷 {
            接受超前_checkpoint: true,
            ..Default::default()
        })
        .await,
        &["拒绝超前的 checkpoint"],
    );
}

#[tokio::test]
async fn 不支持推进_epoch_时围栏检查记为跳过而不是通过() {
    // 把"没法验"记成"验过了"是这类套件最容易犯的错。
    struct 不能推进(Arc<可控存储>);
    impl PersistenceSubject for 不能推进 {
        fn persistence(&self) -> Arc<dyn RunPersistence> {
            self.0.clone()
        }
        // 不覆盖 advance_epoch，默认返回 None。
    }

    let r = check_persistence(&不能推进(可控存储::new(缺陷::default()))).await;
    let 跳过: Vec<&str> = r.skipped().iter().map(|c| c.name).collect();
    assert_eq!(跳过, ["epoch 围栏拒绝旧 writer"]);
    assert!(r.render().contains("跳过不等于通过"), "{}", r.render());
}
