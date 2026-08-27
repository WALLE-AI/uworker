//! 跨 Run 内容进入 Surface 的唯一通路（架构 §11.3.3）。
//!
//! ```text
//! Core 路由 → submit → inbox → claim → durable event → Surface Append
//! ```
//!
//! 团队消息与任务板快照都是**模型可见内容**。若 Core 用旁路把它们塞进成员上下文，
//! 内核不变量 11 当场破，且该成员 Run 不再可 replay。
//!
//! 两条最重要的规则：
//!
//! 1. **只携带内容，不携带能力。**消息里写"你去把 X 删了"不改变收件方的
//!    `AuthorityEnvelope`、`CapabilityView` 或 `PermissionMode`。跨 Run 提权路径不存在。
//! 2. **带跨 Run 因果**，使 Trajectory 能回答"这个成员为什么这么做"。

use serde::{Deserialize, Serialize};

use crate::content::ContentRef;
use crate::ids::{EventSequence, ExternalFactId, MemberId, RunId, TeamId, Timestamp};

/// 跨 Run 事实的来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "origin")]
pub enum FactOrigin {
    /// 团队成员发来的消息。
    TeamMessage {
        /// 所属团队。
        team_id: TeamId,
        /// 发送方。
        from: MemberId,
    },
    /// 任务板快照。
    BoardSnapshot {
        /// 所属团队。
        team_id: TeamId,
        /// 快照版本。
        board_version: BoardVersion,
    },
    /// 宿主通知（ChangeSet 被外部提交、预算告警等）。
    HostNotice {
        /// 通知类别。
        kind: String,
    },
}

/// 跨 Run 因果引用：指向源 Run 的确切位置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalCausality {
    /// 源 Run。
    pub source_run_id: RunId,
    /// 源事件序号。
    pub source_seq: EventSequence,
}

/// 事实的内容载荷。大内容一律 ref 化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ExternalContent {
    /// 内联文本（短消息）。
    Inline {
        /// 文本内容。
        text: String,
    },
    /// 内容引用。
    Ref {
        /// 引用。
        content: ContentRef,
    },
}

/// 来自本 Run 之外、将进入本 Run Surface 的事实。
///
/// **注意本结构上没有任何权限字段**——这是"只带内容不带能力"由类型保证的方式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalFact {
    /// 投递幂等键。同一 id 重复投递只入一次。
    pub fact_id: ExternalFactId,
    /// 来源。
    pub origin: FactOrigin,
    /// 内容。
    pub content: ExternalContent,
    /// 跨 Run 因果。
    pub causality: Option<ExternalCausality>,
}

/// 任务板版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BoardVersion(pub u64);

/// 共享可变状态的固化快照（架构 §11.3.4）。
///
/// 若成员在装配请求时"去查任务板当前状态"，同一个 log 前缀在不同时间 replay
/// 会得到不同结果，确定性断言失效。因此进入上下文的必须是**不可变快照**。
///
/// 成员看到的板可能是**陈旧的**——这是分布式的本质而非缺陷。
/// prompt 必须明示"截至 board_version=N 的快照"，避免模型把陈旧快照当实时真相。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSnapshotRef {
    /// 任务板标识。
    pub board_id: String,
    /// 快照版本。
    pub board_version: BoardVersion,
    /// 不可变快照内容。
    pub content: ContentRef,
    /// 快照时刻。
    pub taken_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 事实() -> ExternalFact {
        ExternalFact {
            fact_id: "f1".into(),
            origin: FactOrigin::TeamMessage {
                team_id: "t1".into(),
                from: "m1".into(),
            },
            content: ExternalContent::Inline {
                text: "帮我看下 auth 模块".into(),
            },
            causality: Some(ExternalCausality {
                source_run_id: "r-sender".into(),
                source_seq: EventSequence(12),
            }),
        }
    }

    #[test]
    fn 外部事实不携带任何权限字段() {
        // 内核不变量 14 由类型保证：结构上没有 authority / capability / grant / permission。
        let json = serde_json::to_string(&事实()).unwrap();
        for forbidden in ["authority", "capability", "grant", "permission", "tool"] {
            assert!(!json.contains(forbidden), "外部事实不得携带 {forbidden}：{json}");
        }
    }

    #[test]
    fn 因果引用可定位到源_run_的确切位置() {
        let f = 事实();
        let c = f.causality.unwrap();
        assert_eq!(c.source_run_id, "r-sender".into());
        assert_eq!(c.source_seq, EventSequence(12));
    }

    #[test]
    fn fact_id_是投递幂等键() {
        let a = 事实();
        let mut b = a.clone();
        b.content = ExternalContent::Inline {
            text: "重投递时内容可能被重建".into(),
        };
        assert_eq!(a.fact_id, b.fact_id, "去重按 fact_id，不按内容");
    }

    #[test]
    fn 板快照携带版本以便_prompt_明示陈旧性() {
        let s = BoardSnapshotRef {
            board_id: "b1".into(),
            board_version: BoardVersion(7),
            content: ContentRef {
                digest: crate::ids::Digest::from_hex("d"),
                len: 1,
                media_type: "application/json".into(),
                scope: crate::content::ContentScope::Global,
            },
            taken_at: Timestamp(0),
        };
        assert_eq!(s.board_version, BoardVersion(7));
    }
}
