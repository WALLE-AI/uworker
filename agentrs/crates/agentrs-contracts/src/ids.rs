//! 标识符与序号。
//!
//! 全部为 newtype，禁止裸 `String`/`u64` 在契约中流通——它们会在调用点被互相混淆。
//!
//! 两个序空间必须区分（架构 §4.3）：
//! - [`EventSequence`] 是 durable 事实的全序，由 `RunPersistence` 分配；
//! - [`LiveSequence`] 是进程内 live 流的局部顺序，由内核分配。
//!
//! 二者**不可比较**，因此不实现互转，也不共用类型。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 声明一个字符串 newtype 标识符。
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// 从任意字符串构造。
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// 借出内部字符串。
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(
    /// 一次 Run 的标识。MemberRun 与 fork 产生的都是**独立的** `RunId`。
    RunId
);
string_id!(
    /// 一个 Turn 的标识。Turn 可以花零个 Step（被拒绝的 claim，见架构 §6.0）。
    TurnId
);
string_id!(
    /// 一个 Step 的标识：一次模型请求 + 它提出的全部工具调用。
    StepId
);
string_id!(
    /// 一次 operation 的标识（provider 流、工具执行等 live 单元）。
    OperationId
);
string_id!(
    /// 事件标识。**这是 durable 写入的幂等键**，由内核确定性派生。
    ///
    /// 消费端去重键是 `(RunId, EventId)`，不是 `(RunId, EventSequence)`——
    /// 后者在重试分配新 seq 时会失效（架构 §4.3）。
    EventId
);
string_id!(
    /// 请求标识，与 [`crate::manifest::ModelRequestManifest`] 一一对应。
    RequestId
);
string_id!(
    /// 作用域标识。**Scope 管可见性与生命周期，不是权限边界**（架构 §5.1）。
    ScopeId
);
string_id!(
    /// 组件标识。Phase D 之前只用于 `ConfigTree` 分区。
    ComponentId
);
string_id!(
    /// 工具调用标识（provider 侧的 `tool_use` id）。
    ToolCallId
);
string_id!(
    /// 一次沙箱执行的标识，用于 `cancel` 与 `reconcile`。
    ExecutionId
);
string_id!(
    /// ChangeSet 标识。进入 `input_hash`，因此同一命令在不同 ChangeSet 上是不同意图（架构 §8.2）。
    ChangeSetId
);
string_id!(
    /// 团队标识。Team 是**协调域**，不是所有权域（架构 §11.3.1）。
    TeamId
);
string_id!(
    /// 团队成员标识。
    MemberId
);
string_id!(
    /// 跨 Run 事实的标识，作为投递幂等键。
    ExternalFactId
);
string_id!(
    /// 记忆片段标识。
    MemoryId
);
string_id!(
    /// 技能标识。
    SkillId
);
string_id!(
    /// 审批恢复令牌。内核视其为**不透明值**，保管与唤醒归 Core（宿主义务 H5）。
    ApprovalToken
);
string_id!(
    /// 授权信封标识。
    AuthorityEnvelopeId
);
string_id!(
    /// 模型标识。档位到具体 id 的映射归 Core（架构 §1.1 裁定 4）。
    ModelId
);
string_id!(
    /// Provider 标识。
    ProviderId
);
string_id!(
    /// 投影键。
    ProjectionKey
);

/// durable 事实的全序序号，由 `RunPersistence` 分配，单调递增。
///
/// 与 [`LiveSequence`] 分属两个序空间，**不可比较**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventSequence(pub u64);

impl fmt::Display for EventSequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 进程内 live 流的局部顺序，由内核分配。
///
/// live 事件（`TextDelta` 等）不进入 durable 序空间。UI 若要交错渲染，
/// 按 `parent_event_id` 挂到最近的 durable 锚点，而不是假设两个 seq 可排序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LiveSequence(pub u64);

/// 写者围栏（架构 §4.1）。
///
/// `start`/`resume` 各分配一个单调递增的 epoch。同一 `RunId` 上的所有 durable 写入
/// 必须携带 epoch；存储侧拒绝小于当前 epoch 的写入并返回 `Fenced`。
///
/// 没有这条不变式，"恢复以 durable log 为准"本身不成立——进程假死后被重新拉起、
/// 旧进程复活继续写入时，log 已被两个 writer 交错写坏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunEpoch(pub u64);

impl RunEpoch {
    /// 下一个 epoch。
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for RunEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 内容摘要（blake3-256 的十六进制串）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Digest(String);

impl Digest {
    /// 从已计算好的十六进制串构造。
    pub fn from_hex(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    /// 借出十六进制串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 逻辑时间戳（Unix 毫秒）。由 `Clock` port 提供，内核不读真实时钟。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

/// 截止时刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Deadline(pub Timestamp);

/// durable 事件区间，闭开：`[start, end)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRange {
    /// 起始序号（含）。
    pub start: EventSequence,
    /// 结束序号（不含）。
    pub end: EventSequence,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 字符串_id_序列化为裸字符串() {
        let id = RunId::new("run-1");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"run-1\"");
        let back: RunId = serde_json::from_str("\"run-1\"").unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn epoch_单调递增() {
        let e = RunEpoch(3);
        assert_eq!(e.next(), RunEpoch(4));
        assert!(e.next() > e);
    }

    #[test]
    fn durable_与_live_序号是不同类型() {
        // 这个测试的价值在于它"编译得过"——两个序空间无法互相赋值或比较。
        let durable = EventSequence(1);
        let live = LiveSequence(1);
        assert_eq!(durable.0, live.0, "底层数值可以相同");
        // 下面这行若取消注释必须编译失败：
        // assert!(durable < live);
    }
}
