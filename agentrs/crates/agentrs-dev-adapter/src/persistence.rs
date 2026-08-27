//! 本地 JSONL 持久化。
//!
//! **宿主义务 H3 的真实被测对象**：`event_id` 幂等、`seq` 单调、epoch 围栏、checkpoint 写序。
//! 与 `agentrs-testkit` 的内存 fake 实现同一套语义，区别只在落盘。

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use agentrs_contracts::event::RunEventEnvelope;
use agentrs_contracts::ids::{EventId, EventSequence, RunEpoch};
use agentrs_contracts::ports::{PersistError, RunPersistence};
use agentrs_contracts::spec::RunCheckpoint;
use agentrs_contracts::{StepIntent, StepResult};
use async_trait::async_trait;

#[derive(Default)]
struct State {
    by_id: HashMap<EventId, EventSequence>,
    next_seq: u64,
    epoch: Option<RunEpoch>,
    checkpoint: Option<RunCheckpoint>,
}

/// 追加式 JSONL 事件日志。
pub struct JsonlPersistence {
    path: PathBuf,
    state: Mutex<State>,
}

impl JsonlPersistence {
    /// 在指定文件上打开（不存在则创建）。
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(Self {
            path,
            state: Mutex::new(State::default()),
        })
    }

    /// 事件文件路径。
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 已写入的事件数。
    pub fn event_count(&self) -> u64 {
        self.state.lock().unwrap().next_seq
    }

    /// 从文件读回全部 durable 事件，按写入顺序。
    ///
    /// **无法解析的行被跳过而不是让整次加载失败**——一条写坏的记录
    /// 不该让整个 Run 变得不可恢复；能读回多少就恢复到哪里，
    /// 缺口由 `validate` 报出来。
    pub fn load_events(&self) -> std::io::Result<Vec<RunEventEnvelope>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            // 文件还不存在 = 空日志，不是错误。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some("event"))
            .filter_map(|v| serde_json::from_value(v.get("event")?.clone()).ok())
            .collect())
    }

    /// 读回最后一个 checkpoint。
    pub fn load_checkpoint(&self) -> std::io::Result<Option<RunCheckpoint>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        Ok(text
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some("checkpoint"))
            .filter_map(|v| serde_json::from_value(v.get("checkpoint")?.clone()).ok())
            .next_back())
    }

    /// 统计日志里**写坏的行**。`validate` 用它报缺口。
    pub fn corrupt_line_count(&self) -> std::io::Result<usize> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_err())
            .count())
    }

    fn guard(state: &mut State, epoch: RunEpoch) -> Result<(), PersistError> {
        match state.epoch {
            Some(cur) if epoch < cur => Err(PersistError::Fenced),
            _ => {
                state.epoch = Some(epoch);
                Ok(())
            }
        }
    }

    fn append_line(&self, value: &serde_json::Value) -> Result<(), PersistError> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| PersistError::Backend {
                message: e.kind().to_string(),
            })?;
        writeln!(f, "{value}").map_err(|e| PersistError::Backend {
            message: e.kind().to_string(),
        })
    }
}

#[async_trait]
impl RunPersistence for JsonlPersistence {
    async fn begin_step(&self, epoch: RunEpoch, intent: StepIntent) -> Result<(), PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard(&mut s, epoch)?;
        drop(s);
        self.append_line(&serde_json::json!({"kind":"intent","intent":intent}))
    }

    async fn append_event(
        &self,
        epoch: RunEpoch,
        event: RunEventEnvelope,
    ) -> Result<EventSequence, PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard(&mut s, epoch)?;

        // 幂等：重复投递返回首次分配的序号，不写第二条。
        if let Some(seq) = s.by_id.get(&event.event_id) {
            return Ok(*seq);
        }

        s.next_seq += 1;
        let seq = EventSequence(s.next_seq);
        s.by_id.insert(event.event_id.clone(), seq);
        drop(s);

        let mut stored = event;
        stored.seq = Some(seq);
        self.append_line(&serde_json::json!({"kind":"event","event":stored}))?;
        Ok(seq)
    }

    async fn finish_step(&self, epoch: RunEpoch, result: StepResult) -> Result<(), PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard(&mut s, epoch)?;
        drop(s);
        self.append_line(&serde_json::json!({"kind":"result","result":result}))
    }

    async fn save_checkpoint(&self, epoch: RunEpoch, checkpoint: RunCheckpoint) -> Result<(), PersistError> {
        let mut s = self.state.lock().unwrap();
        Self::guard(&mut s, epoch)?;
        // checkpoint 不得早于其引用的事件。
        if checkpoint.up_to_seq.0 > s.next_seq {
            return Err(PersistError::CheckpointAhead);
        }
        s.checkpoint = Some(checkpoint.clone());
        drop(s);
        self.append_line(&serde_json::json!({"kind":"checkpoint","checkpoint":checkpoint}))
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::event::{Causality, Durability, EventPayload, Visibility};
    use agentrs_contracts::ids::Timestamp;

    use super::*;

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
            causality: Causality::default(),
            surface: None,
            payload: EventPayload::RunStarted,
        }
    }

    #[tokio::test]
    async fn h3_幂等且单调_落盘版本() {
        let dir = tempdir::TempDir::new("agentrs-jsonl").unwrap();
        let p = JsonlPersistence::open(dir.path().join("events.jsonl")).unwrap();

        let a = p.append_event(RunEpoch(1), 事件("e1")).await.unwrap();
        let b = p.append_event(RunEpoch(1), 事件("e1")).await.unwrap();
        assert_eq!(a, b, "重复投递返回同一序号");
        assert_eq!(p.event_count(), 1, "只写一条");

        let c = p.append_event(RunEpoch(1), 事件("e2")).await.unwrap();
        assert!(c > a, "seq 单调");

        // 落盘内容确实只有两行事件。
        let text = std::fs::read_to_string(p.path()).unwrap();
        assert_eq!(
            text.lines().filter(|l| l.contains("\"kind\":\"event\"")).count(),
            2
        );
    }

    #[tokio::test]
    async fn h3_epoch_围栏_落盘版本() {
        let dir = tempdir::TempDir::new("agentrs-jsonl").unwrap();
        let p = JsonlPersistence::open(dir.path().join("e.jsonl")).unwrap();
        p.append_event(RunEpoch(2), 事件("new")).await.unwrap();
        assert!(matches!(
            p.append_event(RunEpoch(1), 事件("stale")).await,
            Err(PersistError::Fenced)
        ));
        assert_eq!(p.event_count(), 1, "被围栏的写入不得落盘");
    }

    #[tokio::test]
    async fn h3_checkpoint_不得超前() {
        let dir = tempdir::TempDir::new("agentrs-jsonl").unwrap();
        let p = JsonlPersistence::open(dir.path().join("e.jsonl")).unwrap();
        p.append_event(RunEpoch(1), 事件("e1")).await.unwrap();
        let ahead = RunCheckpoint {
            spec_version: agentrs_contracts::version::SpecVersion(1),
            up_to_seq: EventSequence(99),
            pending_approval: None,
        };
        assert!(matches!(
            p.save_checkpoint(RunEpoch(1), ahead).await,
            Err(PersistError::CheckpointAhead)
        ));
    }
}
