//! 沙箱执行契约（架构 §8、迭代计划 §4）。
//!
//! 内核**没有任何执行权**：不 `spawn`、不 `fs::write`。它只能提出请求、消费结果。
//! 真正的隔离与 ChangeSet 生成由 SandboxRS 负责（宿主义务 H1/H2/H7）。

use serde::{Deserialize, Serialize};

use crate::content::ContentRef;
use crate::ids::{ChangeSetId, ExecutionId, Timestamp};
use crate::policy::InputHash;

/// 实际生效的隔离级别（迭代计划 §4 的分级隔离决策）。
///
/// 排期上存在硬冲突：内核在 Phase B 就需要 `ExecCommand`，但真正的
/// landlock/seccomp 是数千行平台相关代码。为赶进度让命令裸跑，就退回了
/// aionrs 的做法（其 `containment.rs` 只做 kill 传播，不是隔离）——正是本架构否定的。
///
/// 因此分级，并让 conformance 的 H2 按级评定。**L0 是真实约束，不是占位**：
/// 它挡得住写越界、跑满 CPU、偷偷联网这三类最常见的越界，只是挡不住有意的提权攻击。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel {
    /// 基础围栏：进程组、rlimit、cwd 限制、env 清洗、默认断网、超时强杀。
    ///
    /// 作为 `Default`：**默认取最弱级别是安全的**——请求方要更强必须显式声明，
    /// 而执行方达不到时必须失败而非静默降级。
    #[default]
    L0BasicContainment,
    /// 真实隔离：Linux landlock+seccomp / macOS sandbox_init / Windows Job Object + 受限令牌。
    L1RealIsolation,
    /// 远程沙箱：容器或微 VM。不在当前计划内。
    L2RemoteSandbox,
}

/// 执行请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRequest {
    /// 执行标识，用于 `cancel` 与 `reconcile`。
    pub execution_id: ExecutionId,
    /// 工具名。
    pub tool_name: String,
    /// 规范化后的参数。
    pub arguments: serde_json::Value,
    /// 目标 ChangeSet。**缺失即错误，不默认走工作区**（架构 §8.2）。
    pub change_set_id: ChangeSetId,
    /// 输入指纹，Sandbox 必须与 grant 中的绑定值独立复核。
    pub input_hash: InputHash,
    /// 要求的最低隔离级别。Sandbox 达不到时必须失败，**不得静默降级**。
    pub required_isolation: IsolationLevel,
}

/// 执行结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// 执行标识。
    pub execution_id: ExecutionId,
    /// 退出状态。
    pub outcome: ExecutionOutcome,
    /// **实际生效**的隔离级别。内核据此在 trajectory 标注，
    /// 用户能看到"这条命令是在 L0 下执行的"。
    pub effective_isolation: IsolationLevel,
    /// 产出内容的引用（大输出一律 ref 化）。
    #[serde(default)]
    pub artifacts: Vec<ContentRef>,
    /// **已脱敏的小输出**，可直接内联回灌模型。
    ///
    /// 超过大小上限的输出必须走 `artifacts` 引用——内联大输出会让上下文
    /// 无限增长，也让事件体积失控（架构 §9.3 第 1 段）。
    #[serde(default)]
    pub output: Option<String>,
    /// 本次执行产生的 ChangeSet（若有文件变更）。
    pub change_set: Option<ChangeSetId>,
    /// 完成时刻。
    pub finished_at: Timestamp,
}

/// 执行结局。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum ExecutionOutcome {
    /// 正常结束。
    Completed {
        /// 退出码。
        exit_code: i32,
    },
    /// 超时被强杀。
    TimedOut,
    /// 被取消。
    Canceled,
    /// 执行器拒绝（hash 不符、grant 过期、隔离级别不满足）。
    Rejected {
        /// 稳定原因码。
        reason: RejectReason,
    },
}

/// 执行器拒绝的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// `input_hash` 与 grant 绑定值不符（宿主义务 H1）。
    InputHashMismatch,
    /// grant 已过期。
    GrantExpired,
    /// grant 已被消费过。
    GrantAlreadyConsumed,
    /// 无法满足要求的隔离级别。
    IsolationUnavailable,
    /// 目标 ChangeSet 不存在或已提交/丢弃。
    ChangeSetUnavailable,
}

/// `reconcile` 的返回。**必须如实报告**（宿主义务 H2）——
/// 内核据此决定重试、等待还是停下问人，猜测会导致重复副作用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ExecutionStatus {
    /// 明确未开始。**唯一允许重试的情形之一。**
    NotStarted,
    /// 仍在执行。继续等待 settlement。
    Running,
    /// 已完成，携带结果。
    Finished(Box<ExecutionResult>),
    /// 状态未知。**必须停在 `RunNeedsUserAction`，不得重试。**
    Unknown,
}

/// 沙箱端口的错误。
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// 执行器不可达。
    #[error("sandbox unavailable")]
    Unavailable,
    /// 未提供 grant 或 grant 非法。
    #[error("invalid grant")]
    InvalidGrant,
    /// 其他已脱敏错误。
    #[error("sandbox error: {message}")]
    Other {
        /// 已脱敏的错误描述。
        message: String,
    },
}

/// ChangeSet 的可用性通知。
///
/// 用户提交或丢弃 ChangeSet 是 Run 外的动作。若在 Run 进行中发生，
/// Core 必须在下一个安全边界通知内核，内核将受影响文件的工作集条目标记为
/// `Stale` 并显式告知模型，**不得静默沿用旧内容**（架构 §8.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ChangeSetNotice {
    /// 已被外部提交。
    Committed {
        /// ChangeSet 标识。
        change_set_id: ChangeSetId,
    },
    /// 已被外部丢弃。
    Discarded {
        /// ChangeSet 标识。
        change_set_id: ChangeSetId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 隔离级别可比较且_l0_最弱() {
        assert!(IsolationLevel::L0BasicContainment < IsolationLevel::L1RealIsolation);
        assert!(IsolationLevel::L1RealIsolation < IsolationLevel::L2RemoteSandbox);
    }

    #[test]
    fn 执行请求必须携带_change_set() {
        // change_set_id 非 Option：类型上就不允许"默认走工作区"。
        let json = serde_json::to_string(&ExecutionRequest {
            execution_id: "e1".into(),
            tool_name: "Read".into(),
            arguments: serde_json::json!({}),
            change_set_id: "cs1".into(),
            input_hash: InputHash(crate::ids::Digest::from_hex("h")),
            required_isolation: IsolationLevel::L0BasicContainment,
        })
        .unwrap();
        assert!(json.contains("change_set_id"), "{json}");
    }

    #[test]
    fn reconcile_三态齐备且_unknown_可区分() {
        for s in [
            ExecutionStatus::NotStarted,
            ExecutionStatus::Running,
            ExecutionStatus::Unknown,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: ExecutionStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn 结果必须报告实际生效的隔离级别() {
        // effective_isolation 非 Option：执行器不能对隔离级别保持沉默。
        let r = ExecutionResult {
            execution_id: "e1".into(),
            outcome: ExecutionOutcome::Completed { exit_code: 0 },
            effective_isolation: IsolationLevel::L0BasicContainment,
            artifacts: vec![],
            output: None,
            change_set: None,
            finished_at: Timestamp(0),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("effective_isolation"), "{json}");
    }
}
