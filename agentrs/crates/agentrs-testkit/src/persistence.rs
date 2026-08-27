//! 内存 fake Persistence（任务 T02、T02B）。
//!
//! 它同时是 **宿主义务 H3 的第一个被测对象**：
//!
//! 1. `append_event` 以 `event_id` 幂等——重复投递返回首次分配的序号，不写第二条；
//! 2. `seq` 单调递增；
//! 3. epoch 围栏——拒绝小于当前 epoch 的写入并返回 `Fenced`；
//! 4. checkpoint 不得早于其引用的事件。
//!
//! 没有这四条，"恢复以 durable log 为准"本身不成立：进程假死后被重新拉起、
//! 旧进程复活继续写入时，log 已被两个 writer 交错写坏。

use std::collections::HashMap;
use std::sync::Mutex;

use agentrs_contracts::event::RunEventEnvelope;
use agentrs_contracts::ids::{EventId, EventSequence, RunEpoch};
use agentrs_contracts::ports::{PersistError, RunPersistence};
use agentrs_contracts::spec::RunCheckpoint;
use agentrs_contracts::{StepIntent, StepResult};
use async_trait::async_trait;

#[derive(Default)]
struct State {
    /// 已提交的事件，按写入顺序。
    events: Vec<RunEventEnvelope>,
    /// 幂等索引：event_id -> 首次分配的序号。
    by_id: HashMap<EventId, EventSequence>,
    /// 当前有效 epoch。
    epoch: Option<RunEpoch>,
    /// 已记录的意图。
    intents: Vec<StepIntent>,
    /// 已记录的结果。
    results: Vec<StepResult>,
    /// 最近一次 checkpoint。
    checkpoint: Option<RunCheckpoint>,
    /// 注入的下一次写入错误。
    inject: Option<InjectedFault>,
}

/// 可注入的故障，用于崩溃与围栏测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectedFault {
    /// 下一次写入返回后端错误（模拟崩溃前的失败）。
    BackendError,
}

/// 内存持久化 fake。
#[derive(Default)]
pub struct FakePersistence {
    state: Mutex<State>,
}

impl FakePersistence {
    /// 新建一个空实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 已提交的事件数。
    pub fn event_count(&self) -> usize {
        self.state.lock().unwrap().events.len()
    }

    /// 全部事件的快照。
    pub fn events(&self) -> Vec<RunEventEnvelope> {
        self.state.lock().unwrap().events.clone()
    }

    /// 已记录的意图数。
    pub fn intent_count(&self) -> usize {
        self.state.lock().unwrap().intents.len()
    }

    /// 已记录的结果数。
    pub fn result_count(&self) -> usize {
        self.state.lock().unwrap().results.len()
    }

    /// 当前 checkpoint。
    pub fn checkpoint(&self) -> Option<RunCheckpoint> {
        self.state.lock().unwrap().checkpoint.clone()
    }

    /// 注入一次写入故障。
    pub fn inject(&self, fault: InjectedFault) {
        self.state.lock().unwrap().inject = Some(fault);
    }

    /// 校验 epoch 并推进当前值。旧 epoch 一律 `Fenced`。
    fn guard_epoch(state: &mut State, epoch: RunEpoch) -> Result<(), PersistError> {
        match state.epoch {
            Some(current) if epoch < current => Err(PersistError::Fenced),
            _ => {
                state.epoch = Some(epoch);
                Ok(())
            }
        }
    }

    fn take_fault(state: &mut State) -> Result<(), PersistError> {
        match state.inject.take() {
            Some(InjectedFault::BackendError) => Err(PersistError::Backend {
                message: "injected".into(),
            }),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl RunPersistence for FakePersistence {
    async fn begin_step(&self, epoch: RunEpoch, intent: StepIntent) -> Result<(), PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard_epoch(&mut s, epoch)?;
        Self::take_fault(&mut s)?;
        s.intents.push(intent);
        Ok(())
    }

    async fn append_event(
        &self,
        epoch: RunEpoch,
        event: RunEventEnvelope,
    ) -> Result<EventSequence, PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard_epoch(&mut s, epoch)?;

        // 幂等先于故障注入：已提交的事件重投递必须成功返回原序号，
        // 否则重试路径会在故障恢复后写出第二条记录。
        if let Some(seq) = s.by_id.get(&event.event_id) {
            return Ok(*seq);
        }

        Self::take_fault(&mut s)?;

        let seq = EventSequence(s.events.len() as u64 + 1);
        let mut stored = event;
        stored.seq = Some(seq);
        s.by_id.insert(stored.event_id.clone(), seq);
        s.events.push(stored);
        Ok(seq)
    }

    async fn finish_step(&self, epoch: RunEpoch, result: StepResult) -> Result<(), PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard_epoch(&mut s, epoch)?;
        Self::take_fault(&mut s)?;
        s.results.push(result);
        Ok(())
    }

    async fn save_checkpoint(&self, epoch: RunEpoch, checkpoint: RunCheckpoint) -> Result<(), PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard_epoch(&mut s, epoch)?;

        // checkpoint 不得早于其引用的事件，否则恢复时会读到"指向未来"的游标。
        let latest = s.events.last().and_then(|e| e.seq).unwrap_or(EventSequence(0));
        if checkpoint.up_to_seq > latest {
            return Err(PersistError::CheckpointAhead);
        }

        Self::take_fault(&mut s)?;
        s.checkpoint = Some(checkpoint);
        Ok(())
    }
}
