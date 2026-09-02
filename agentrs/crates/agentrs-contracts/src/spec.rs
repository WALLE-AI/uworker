//! Run 规格、配置树、派生与分叉（架构 §4.1、§4.1.1、§11.3.5）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use crate::ids::{
    ComponentId, EventSequence, MemberId, ModelId, OperationId, ProviderId, RunEpoch, RunId, TeamId,
};
use crate::version::SpecVersion;

/// 组件配置树。
///
/// "配置不进内核"是对的，但若每加一个配置项就给 `RunSpec` 加一个具名字段，
/// Core 每次都要跟着改——摩擦一大，人就会从侧信道偷渡配置，规矩最终形同虚设。
///
/// 因此用 schema 校验过的**不透明配置树**：内核只校验与分发，不解释语义。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigTree {
    /// 按组件 id 分区。**未知 section 是错误而非忽略**——
    /// "配置写了但没生效"是最难排查的一类问题。
    pub sections: BTreeMap<ComponentId, serde_json::Value>,
}

/// 上下文预算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// 输入窗口上限。
    pub max_input_tokens: u64,
    /// 输出预留。
    pub reserved_output_tokens: u64,
    /// 触发压缩的比例阈值（0–100）。
    pub compaction_threshold_pct: u8,
}

/// 执行预算。
///
/// 团队场景下成员持有的额度是**从团队池借出的**，不是独立配额——
/// 否则 N 个成员各烧满等于 N 倍成本（架构 §11.3.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBudget {
    /// 最大 Turn 数。
    pub max_turns: u32,
    /// 最大工具调用数。
    pub max_tool_calls: u32,
    /// 最大并发工具数。
    pub max_concurrent_tools: u32,
    /// 成本上限（最小货币单位）。
    pub max_cost_units: u64,
    /// 审批等待上限（毫秒）。**超时转挂起，绝不无限阻塞。**
    pub approval_timeout_ms: u64,
    /// 若从团队池借出，记录池标识。
    pub borrowed_from_team: Option<TeamId>,
}

impl Default for ExecutionBudget {
    fn default() -> Self {
        Self {
            max_turns: 64,
            max_tool_calls: 512,
            max_concurrent_tools: 8,
            max_cost_units: u64::MAX,
            approval_timeout_ms: 60_000,
            borrowed_from_team: None,
        }
    }
}

/// 模型策略。由 Core 传入，Agent 不能自行扩大。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPolicy {
    /// 各档位对应的模型 id。**档位语义归内核，映射归 Core。**
    pub tiers: BTreeMap<ModelTier, ModelId>,
    /// 授权范围内的 fallback 目标。范围内的切换由内核进程内完成，
    /// 越权切换才上报 Core（架构 §7）。
    #[serde(default)]
    pub fallback: Vec<ModelId>,
    /// 允许的 provider。
    pub providers: Vec<ProviderId>,
    /// 重试上限。
    pub max_retries: u8,
    /// 是否允许传附件。
    pub allow_attachments: bool,
}

/// 模型档位。语义由内核定义，具体模型由 Core 映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// 零工具、低成本：memorySelector、风险提示、简单分类。
    Lite,
    /// 主任务、计划、探索、压缩。
    Default,
    /// 高价值复杂任务，成本预算更严格。
    Craft,
}

/// 对话快照，作为 Run 的初始 Surface 来源。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSnapshot {
    /// 来源 Run（fork 时为源 Run）。
    pub derived_from: Option<RunId>,
    /// 覆盖到的源事件序号。
    pub up_to_seq: Option<EventSequence>,
}

/// 系统上下文。**身份、语气、品牌、产品说明由 Core 从此注入**，
/// 内核 prompt 只含行为契约与安全规则（架构 §1.1 裁定 2）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemContext {
    /// Core 提供的系统段片段。
    #[serde(default)]
    pub sections: Vec<String>,
    /// 工作区标识。
    pub workspace_id: Option<String>,
}

/// 恢复检查点。**只保存 durable projection 所需的游标与引用**，
/// 不序列化 live task / listener / disposer。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCheckpoint {
    /// schema 版本。
    pub spec_version: SpecVersion,
    /// 覆盖到的事件序号。
    pub up_to_seq: EventSequence,
    /// 挂起的审批（若因审批超时而挂起）。
    pub pending_approval: Option<crate::ids::ApprovalToken>,
}

/// 启动一次 Run 的不可变快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec {
    /// Run 标识。
    pub run_id: RunId,
    /// 父 Run（函数式 ChildRun 使用；MemberRun 是平级，不设父）。
    pub parent_run_id: Option<RunId>,
    /// 初始对话。
    pub conversation: ConversationSnapshot,
    /// 系统上下文。
    pub system_context: SystemContext,
    /// 授权上界。**Run 内不可扩大。**
    pub authority: AuthorityEnvelope,
    /// 初始能力视图。
    pub initial_capabilities: CapabilityView,
    /// 权限模式。
    pub permission_mode: PermissionMode,
    /// 模型策略。
    pub model_policy: ModelPolicy,
    /// 上下文预算。
    pub context_budget: ContextBudget,
    /// 执行预算。
    pub execution_budget: ExecutionBudget,
    /// 本 Run 写入的 ChangeSet。**由宿主命名**，缺省时内核按 `run_id` 派生。
    ///
    /// 加这个字段是因为内核凭 `run_id` 造 ChangeSet id，等于替宿主决定了
    /// "一个 ChangeSet 只活一个 Run"。而 Run 是内核的单位、会话是宿主的单位：
    /// 多轮对话里每一轮是一个新 Run，若每轮都换 ChangeSet，第二轮就**读不到**
    /// 第一轮暂存尚未提交的文件——overlay 按 ChangeSet 隔离——而人按一次提交
    /// 也只会落其中一轮的改动。
    ///
    /// 这不与 fork 规则 5（"不继承任何 live 状态"）冲突：那条列的是 Run 作用域的
    /// 活物（inbox、未决审批、in-flight 工具、grant）。工作区与 ChangeSet 归 Core，
    /// 让宿主给它命名，正是把这份归属还给宿主。
    #[serde(default)]
    pub change_set_id: Option<crate::ids::ChangeSetId>,
    /// 恢复检查点。
    pub checkpoint: Option<RunCheckpoint>,
    /// schema 版本。
    pub spec_version: SpecVersion,
    /// 组件配置树。
    #[serde(default)]
    pub config: ConfigTree,
}

/// 对话分叉（架构 §4.1.1）。
///
/// 两个参考实现都有分叉而本方案初版没有——这是遗漏而非有意省略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkSpec {
    /// 源 Run。
    pub source_run_id: RunId,
    /// 分叉点。**必须落在 durable 事实上**，不能落在 live delta 上。
    /// `None` 表示从当前尾部分叉。
    pub boundary: Option<EventSequence>,
    /// 新 Run 标识。
    pub new_run_id: RunId,
}

/// 团队成员的派生规格（架构 §11.3.5）。
///
/// **MemberRun 是平级 Run，有自己的 `RunEpoch`**，不共享创建者的——
/// 成员生命周期跨越创建者的多个 turn，甚至创建者结束后仍活着。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRunSpec {
    /// 成员标识。
    pub member_id: MemberId,
    /// 所属团队（协调域，**不是所有权域**）。
    pub team_id: TeamId,
    /// 派生深度。创建者 + 1，超过 `AuthorityEnvelope::max_depth` 直接拒绝。
    pub depth: u16,
    /// 成员自己的 Run 规格。其 authority 必须是创建者当前 `CapabilityView` 的子集。
    pub spec: RunSpec,
}

/// 函数式子 Agent 的运行规格（架构 §11.1）。
///
/// **与 [`MemberRunSpec`] 是两种东西**，四个维度都不同：
///
/// | | `ChildRunSpec` | `MemberRunSpec` |
/// |---|---|---|
/// | 所有权 | parent-owned | 平级 Run，归 Core 编排 |
/// | Epoch | **共享父的** | 自己的 |
/// | 生命周期 | 不超过发起它的 operation | 可比父活得久 |
/// | 通信 | 只回一个 [`SubagentSummary`] | 双向 `ExternalFact` |
///
/// 共享父 epoch 这一条对函数式子 Agent 成立、对协作式成员不成立——
/// 成员的生命周期跨越父的多个 turn，父被 Fenced 时它不该跟着倒。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRunSpec {
    /// 子运行标识。
    pub child_run_id: RunId,
    /// 发起它的父 Run。
    pub parent_run_id: RunId,
    /// **共享父的 epoch**：父被围栏时子一并收敛。
    pub epoch: RunEpoch,
    /// 发起它的 operation。子运行的生命周期**不得超过它**。
    pub operation_id: OperationId,
    /// 派生深度。父深度 + 1，超过 `AuthorityEnvelope::max_depth` 直接拒绝。
    pub depth: u16,
    /// 子运行的规格。其能力必须是父当前 `CapabilityView` 的子集。
    pub spec: RunSpec,
}

/// 子 Agent 回给主 Agent 的**全部内容**（架构 §11.1）。
///
/// 过程推理、草稿、token 流与原始上下文**不自动回灌**——理由不是省钱，
/// 是注意力：把子 Agent 的思考灌回主上下文，等于让主 Agent 在一堆
/// 它没参与的推理里找结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentSummary {
    /// 交给它的任务。
    pub task: String,
    /// 结论。
    pub conclusion: String,
    /// 证据引用。**大输出一律 ref 化**，不内联。
    #[serde(default)]
    pub evidence: Vec<crate::content::ContentRef>,
    /// 风险提示。
    #[serde(default)]
    pub risks: Vec<String>,
    /// 置信度，0–100。
    pub confidence: u8,
    /// 建议的下一步。
    #[serde(default)]
    pub next_steps: Vec<String>,
    /// 成本归集：这次子运行花了多少。
    pub usage: TokenUsage,
}

/// 子运行的 token 用量，用于成本归集到父 Run。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// 输入。
    pub input_tokens: u64,
    /// 输出。
    pub output_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::AuthorityEnvelope;

    fn 信封() -> AuthorityEnvelope {
        AuthorityEnvelope {
            id: "e".into(),
            workspaces: vec!["ws".into()],
            tools: vec!["Read".into()],
            providers: vec!["p".into()],
            models: vec!["m".into()],
            max_depth: 2,
        }
    }

    #[test]
    fn 审批超时默认为有界的六十秒() {
        let b = ExecutionBudget::default();
        assert_eq!(b.approval_timeout_ms, 60_000);
        assert!(b.approval_timeout_ms > 0, "绝不允许无限期阻塞");
    }

    #[test]
    fn 配置树按组件分区且默认为空() {
        let c = ConfigTree::default();
        assert!(c.sections.is_empty());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "{}", "transparent 表示，避免多一层嵌套");
    }

    #[test]
    fn fork_的分叉点落在_durable_序号上() {
        let f = ForkSpec {
            source_run_id: "r1".into(),
            boundary: Some(EventSequence(10)),
            new_run_id: "r2".into(),
        };
        // 类型是 EventSequence 而非 LiveSequence——分叉点不可能落在 live delta 上。
        assert_eq!(f.boundary, Some(EventSequence(10)));
    }

    #[test]
    fn 成员深度必须受信封上限约束() {
        let env = 信封();
        let m = MemberRunSpec {
            member_id: "m1".into(),
            team_id: "t1".into(),
            depth: 3,
            spec: RunSpec {
                run_id: "r".into(),
                parent_run_id: None,
                conversation: Default::default(),
                system_context: Default::default(),
                authority: env.clone(),
                initial_capabilities: CapabilityView {
                    tools: vec![],
                    providers: vec![],
                    models: vec![],
                },
                permission_mode: PermissionMode::Default,
                model_policy: ModelPolicy {
                    tiers: BTreeMap::new(),
                    fallback: vec![],
                    providers: vec![],
                    max_retries: 2,
                    allow_attachments: false,
                },
                context_budget: ContextBudget {
                    max_input_tokens: 100_000,
                    reserved_output_tokens: 8_000,
                    compaction_threshold_pct: 80,
                },
                execution_budget: ExecutionBudget::default(),
                change_set_id: None,
                checkpoint: None,
                spec_version: SpecVersion(1),
                config: Default::default(),
            },
        };
        assert!(m.depth > env.max_depth, "本例超限，运行时必须拒绝而非静默截断");
    }

    #[test]
    fn member_不设父_run_它是平级的() {
        // parent_run_id 用于函数式 ChildRun；MemberRun 通过 team_id 关联，不通过父子。
        let json = serde_json::to_string(&MemberRunSpec {
            member_id: "m".into(),
            team_id: "t".into(),
            depth: 1,
            spec: RunSpec {
                run_id: "r".into(),
                parent_run_id: None,
                conversation: Default::default(),
                system_context: Default::default(),
                authority: 信封(),
                initial_capabilities: CapabilityView {
                    tools: vec![],
                    providers: vec![],
                    models: vec![],
                },
                permission_mode: PermissionMode::Plan,
                model_policy: ModelPolicy {
                    tiers: BTreeMap::new(),
                    fallback: vec![],
                    providers: vec![],
                    max_retries: 0,
                    allow_attachments: false,
                },
                context_budget: ContextBudget {
                    max_input_tokens: 1,
                    reserved_output_tokens: 1,
                    compaction_threshold_pct: 50,
                },
                execution_budget: ExecutionBudget::default(),
                change_set_id: None,
                checkpoint: None,
                spec_version: SpecVersion(1),
                config: Default::default(),
            },
        })
        .unwrap();
        assert!(json.contains("\"team_id\""), "{json}");
    }
}
