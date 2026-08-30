//! # AgentRS Contracts
//!
//! AgentRS 的公共契约层：ID、事件、Surface、内容引用、授权、端口 trait。
//!
//! **本 crate 不依赖任何业务 crate**，也不触碰 OS / 网络 / 磁盘 / 真实时钟。
//!
//! ## 四个平面
//!
//! 理解 AgentRS 只需抓住一件事：同一份 Run 状态被切成四个平面，各有各的规则，
//! 互相不可替代。大部分设计缺陷的根因都是把两个平面混为一谈。
//!
//! | 平面 | 规则 | 崩溃后 | 本 crate 中的位置 |
//! |---|---|---|---|
//! | ① Durable 事实 | 只追加、epoch 围栏、`event_id` 幂等 | 权威来源 | [`event`]、[`ports::RunPersistence`] |
//! | ② 模型可见 Surface | log 上的投影；压缩以 `Replace` 遮蔽而非改写 | 由 ① 重建 | [`surface`]、[`manifest`] |
//! | ③ Live 资源 | 有 owner，`cancel → drain → 反序 cleanup` | **不恢复**，重建 | 运行时 crate |
//! | ④ 读模型 | 确定性纯 fold，带 `state_version` | 由 ① 重放 | observability crate |
//!
//! 三条不可替代性：
//!
//! 1. `cleanup / release` 只释放 ③ 的资源，**不宣称历史未发生**——外部副作用只能靠
//!    `reconcile` 处理；
//! 2. ② 的任何变化都必须先是 ① 的一条事件；
//! 3. ④ 从不回写 ①。
//!
//! ## 边界
//!
//! 一个新能力归 AgentRS 还是 AgentCore，按四个正交测试判定：碰环境？需权威？
//! 跨 Run？都否则归 AgentRS。可执行判据是——**如果加入它之后本 workspace 的单测
//! 需要网络、文件系统、真实时钟或 OS 进程，那么它放错地方了**。

#![forbid(unsafe_code)]

pub mod authority;
pub mod component;
pub mod content;
pub mod event;
pub mod external;
pub mod ids;
pub mod manifest;
pub mod policy;
pub mod ports;
pub mod sandbox;
pub mod spec;
pub mod surface;
pub mod version;

use serde::{Deserialize, Serialize};

use crate::content::ContentRef;
use crate::ids::{ChangeSetId, ExecutionId, StepId, Timestamp, ToolCallId};
use crate::policy::{DenyCode, InputHash};
use crate::sandbox::IsolationLevel;

/// 副作用意图。**跨出任何有副作用的调用边界之前必须先落盘**（内核不变量 3）。
///
/// 恢复时凭它向 Sandbox `reconcile`，而不是盲目重跑。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepIntent {
    /// Step 标识。
    pub step_id: StepId,
    /// 对应的工具调用。
    pub call_id: ToolCallId,
    /// 工具名。
    pub tool_name: String,
    /// 输入指纹。
    pub input_hash: InputHash,
    /// 目标 ChangeSet。
    pub change_set_id: ChangeSetId,
    /// 沙箱执行标识，用于 reconcile。
    pub execution_id: ExecutionId,
    /// 落盘时刻。
    pub at: Timestamp,
}

/// 副作用结果。写入后**绝不再次执行**，恢复时直接回灌（内核不变量 4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    /// Step 标识。
    pub step_id: StepId,
    /// 对应的工具调用。
    pub call_id: ToolCallId,
    /// 结果。
    pub outcome: StepOutcome,
    /// 实际生效的隔离级别（若经过 Sandbox）。
    pub effective_isolation: Option<IsolationLevel>,
    /// 产出物引用。
    #[serde(default)]
    pub artifacts: Vec<ContentRef>,
    /// 已脱敏的小输出，直接回灌模型；大输出走 `artifacts`。
    #[serde(default)]
    pub output: Option<String>,
    /// 完成时刻。
    pub at: Timestamp,
}

/// Step 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum StepOutcome {
    /// 成功。
    Succeeded,
    /// 工具报错。**作为结构化结果回灌模型，不是异常。**
    Failed {
        /// 已脱敏的错误描述。
        message: String,
    },
    /// 被拒绝（Policy Deny、guard、Hook Block）。同样结构化回灌。
    Denied {
        /// 稳定拒绝码。
        code: DenyCode,
        /// 已脱敏说明。
        message: String,
    },
    /// 被取消。
    Canceled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Digest;

    fn 意图() -> StepIntent {
        StepIntent {
            step_id: "s1".into(),
            call_id: "c1".into(),
            tool_name: "Write".into(),
            input_hash: InputHash(Digest::from_hex("h")),
            change_set_id: "cs1".into(),
            execution_id: "e1".into(),
            at: Timestamp(0),
        }
    }

    #[test]
    fn 意图携带_reconcile_所需的执行标识() {
        // 没有 execution_id 就无法 reconcile，只能盲目重跑——那正是要避免的。
        assert_eq!(意图().execution_id, "e1".into());
    }

    #[test]
    fn 意图与结果可_round_trip() {
        let i = 意图();
        let back: StepIntent = serde_json::from_str(&serde_json::to_string(&i).unwrap()).unwrap();
        assert_eq!(back, i);

        let r = StepResult {
            step_id: "s1".into(),
            call_id: "c1".into(),
            outcome: StepOutcome::Denied {
                code: DenyCode::PermissionMode,
                message: "plan mode".into(),
            },
            effective_isolation: Some(IsolationLevel::L0BasicContainment),
            artifacts: vec![],
            output: None,
            at: Timestamp(1),
        };
        let back: StepResult = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn 拒绝是结构化结果而非异常() {
        // Denied 是 StepOutcome 的一个变体，会被写入 StepResult 并回灌模型。
        let o = StepOutcome::Denied {
            code: DenyCode::GuardDenied,
            message: "no".into(),
        };
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("\"outcome\":\"denied\""), "{json}");
    }
}
