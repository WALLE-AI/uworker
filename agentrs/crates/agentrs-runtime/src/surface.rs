//! ModelSurface 投影（架构 §4.3.1，任务 T22）。
//!
//! **durable log 永远 append-only；模型看到的历史是 log 上一个叫 Surface 的有序投影。**
//!
//! ```text
//! durable log            [e1][e2][e3][e4][e5][e6]   ← 永远只追加
//!                         ↓   ↓        ↓
//! Surface（append-origin）[m1][m2]    [m3]
//!                                      └── Replace(range=m1..m3) ──┐
//!                                                                   ↓
//! 模型看到的历史          [摘要节点]
//! 人类看到的 transcript   [m1][m2][m3]   ← 用 append-origin，不受遮蔽
//! ```
//!
//! [`derive_messages`] 是**唯一**的模型历史投影规则。内核内外——trajectory、replay、
//! 外部重建器——必须折叠同一个函数；不允许存在第二份"模型历史构造"逻辑。

use agentrs_contracts::event::RunEventEnvelope;
use agentrs_contracts::ids::EventSequence;
use agentrs_contracts::surface::{SurfaceEventKind, SurfaceOp};
use agentrs_types::Message;

/// 一个 Surface 节点：durable 事件 + 它派生出的消息。
///
/// 消息由 `SurfaceMessageRecorded` 事件载荷承载。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceNode {
    /// durable 序号。
    pub seq: EventSequence,
    /// Surface 种类。
    pub kind: SurfaceEventKind,
    /// 相对 Surface 的操作。
    pub op: SurfaceOp,
    /// 该节点承载的消息。空 content 的 assistant 消息在此仍为 `Some`——
    /// 它不进入派生历史，但事件必须保留（承载 usage / max_tokens）。
    pub message: Message,
}

/// Rebuild Surface nodes from a durable event prefix.
pub fn from_events(events: &[RunEventEnvelope]) -> Result<Vec<SurfaceNode>, serde_json::Error> {
    events
        .iter()
        .filter(|event| event.is_durable())
        .filter_map(|event| match &event.payload {
            agentrs_contracts::event::EventPayload::SurfaceMessageRecorded { message } => Some(
                serde_json::from_value(message.clone())
                    .map(|message| SurfaceNode::from_event(event, message)),
            ),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|nodes| nodes.into_iter().flatten().collect())
}

impl SurfaceNode {
    /// 从事件信封与消息构造节点。事件不带 Surface 标记时返回 `None`。
    pub fn from_event(event: &RunEventEnvelope, message: Message) -> Option<Self> {
        let marker = event.surface?;
        Some(Self {
            seq: event.seq?,
            kind: marker.kind,
            op: marker.op,
            message,
        })
    }
}

/// 把一个 log 前缀投影为模型可见的消息序列。
///
/// **这是唯一的投影规则。**相同前缀必然产生相同输出——replay、trajectory
/// 与请求装配折叠同一个函数，因此三者永远一致。
///
/// 规则：
///
/// 1. `Replace{range}` 节点**遮蔽**该 range 内的全部节点，自身进入结果；
/// 2. 后出现的 Replace 可以遮蔽先前的 Replace；
/// 3. 空 content 的消息不进入结果，但其节点仍参与遮蔽计算；
/// 4. 结果按 `seq` 升序。
pub fn derive_messages(nodes: &[SurfaceNode]) -> Vec<Message> {
    let mut shadowed = vec![false; nodes.len()];

    // 从后往前应用遮蔽：后出现的 Replace 优先，且其自身不被更早的 Replace 影响。
    for (i, node) in nodes.iter().enumerate().rev() {
        if shadowed[i] {
            continue;
        }
        if let SurfaceOp::Replace { range, .. } = node.op {
            for (j, other) in nodes.iter().enumerate() {
                if j != i && other.seq >= range.start && other.seq < range.end {
                    shadowed[j] = true;
                }
            }
        }
    }

    nodes
        .iter()
        .enumerate()
        .filter(|(i, _)| !shadowed[*i])
        .map(|(_, n)| &n.message)
        .filter(|m| !m.is_empty())
        .cloned()
        .collect()
}

/// 把一个 log 前缀投影为**人类 transcript**。
///
/// 与 [`derive_messages`] 的关键差别：只取 append-origin 节点，**不应用遮蔽**。
///
/// Surface 刻意遮蔽被替换的 range，因此它是渲染人类对话记录的错误来源——
/// 一次已落地的替换会抹掉用户已经看到的对话。replacement 副本是 model-only。
pub fn derive_transcript(nodes: &[SurfaceNode]) -> Vec<Message> {
    nodes
        .iter()
        .filter(|n| n.op.is_append_origin())
        .map(|n| n.message.clone())
        .collect()
}

/// 计算当前 Surface 导致的缓存前缀失效点。
///
/// `None` 表示前缀完全稳定（全部为 append）。有 Replace 时取**最早**的 range 起点——
/// 从那一点起缓存必然 miss。这个值是可精确计算的，不需要靠"把改写攒到边界"去近似。
pub fn cache_invalidation_point(nodes: &[SurfaceNode]) -> Option<EventSequence> {
    nodes.iter().filter_map(|n| n.op.cache_invalidation_point()).min()
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::surface::{SurfaceGeneration, SurfaceRange};
    use agentrs_types::{ContentBlock, Role};

    use super::*;

    fn 节点(seq: u64, op: SurfaceOp, text: &str) -> SurfaceNode {
        SurfaceNode {
            seq: EventSequence(seq),
            kind: SurfaceEventKind::UserMessage,
            op,
            message: Message::new(Role::User, vec![ContentBlock::text(text)]),
        }
    }

    fn 替换(seq: u64, start: u64, end: u64, text: &str) -> SurfaceNode {
        节点(
            seq,
            SurfaceOp::Replace {
                range: SurfaceRange {
                    start: EventSequence(start),
                    end: EventSequence(end),
                },
                generation: SurfaceGeneration(1),
            },
            text,
        )
    }

    fn 文本(msgs: &[Message]) -> Vec<String> {
        msgs.iter()
            .flat_map(|m| {
                m.content.iter().filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .collect()
    }

    #[test]
    fn 纯追加时投影即原序列() {
        let nodes = vec![
            节点(1, SurfaceOp::Append, "a"),
            节点(2, SurfaceOp::Append, "b"),
            节点(3, SurfaceOp::Append, "c"),
        ];
        assert_eq!(文本(&derive_messages(&nodes)), ["a", "b", "c"]);
    }

    #[test]
    fn replace_遮蔽区间且自身进入结果() {
        // 压缩：用一个摘要节点遮蔽 [1,3)。
        let nodes = vec![
            节点(1, SurfaceOp::Append, "a"),
            节点(2, SurfaceOp::Append, "b"),
            节点(3, SurfaceOp::Append, "c"),
            替换(4, 1, 3, "摘要(a,b)"),
        ];
        assert_eq!(文本(&derive_messages(&nodes)), ["c", "摘要(a,b)"]);
    }

    #[test]
    fn 后出现的_replace_可遮蔽先前的_replace() {
        let nodes = vec![
            节点(1, SurfaceOp::Append, "a"),
            节点(2, SurfaceOp::Append, "b"),
            替换(3, 1, 3, "摘要1"),
            替换(4, 1, 4, "摘要2"),
        ];
        // 摘要2 遮蔽 [1,4)，含摘要1 自身。
        assert_eq!(文本(&derive_messages(&nodes)), ["摘要2"]);
    }

    #[test]
    fn 人类_transcript_不受遮蔽影响() {
        // 这是 Surface 模型最重要的一条：压缩不能抹掉用户已经看到的对话。
        let nodes = vec![
            节点(1, SurfaceOp::Append, "a"),
            节点(2, SurfaceOp::Append, "b"),
            替换(3, 1, 3, "摘要(a,b)"),
        ];
        assert_eq!(文本(&derive_messages(&nodes)), ["摘要(a,b)"]);
        assert_eq!(
            文本(&derive_transcript(&nodes)),
            ["a", "b"],
            "transcript 保留原文"
        );
    }

    #[test]
    fn 空内容消息不进入派生历史但参与遮蔽() {
        let mut 空 = 节点(2, SurfaceOp::Append, "");
        空.message.role = Role::Assistant;
        let nodes = vec![
            节点(1, SurfaceOp::Append, "a"),
            空,
            节点(3, SurfaceOp::Append, "c"),
        ];
        assert_eq!(文本(&derive_messages(&nodes)), ["a", "c"]);
        assert_eq!(derive_transcript(&nodes).len(), 3, "事件本身保留，承载 usage");
    }

    #[test]
    fn 相同前缀始终产生相同投影() {
        // 确定性是 replay、trajectory 与请求装配三者一致的前提。
        let nodes = vec![
            节点(1, SurfaceOp::Append, "a"),
            替换(2, 1, 2, "s"),
            节点(3, SurfaceOp::Append, "c"),
        ];
        let 首次 = derive_messages(&nodes);
        for _ in 0..8 {
            assert_eq!(derive_messages(&nodes), 首次);
        }
    }

    #[test]
    fn 前缀单调性_追加不改变已有投影的前部() {
        let 基础 = vec![节点(1, SurfaceOp::Append, "a"), 节点(2, SurfaceOp::Append, "b")];
        let mut 扩展 = 基础.clone();
        扩展.push(节点(3, SurfaceOp::Append, "c"));
        let a = derive_messages(&基础);
        let b = derive_messages(&扩展);
        assert_eq!(b[..a.len()], a[..], "纯追加不得改写已投影的前部");
    }

    #[test]
    fn 全追加时缓存前缀完全稳定() {
        let nodes = vec![节点(1, SurfaceOp::Append, "a"), 节点(2, SurfaceOp::Append, "b")];
        assert_eq!(cache_invalidation_point(&nodes), None);
    }

    #[test]
    fn 失效点取最早的_replace_起点() {
        let nodes = vec![
            节点(1, SurfaceOp::Append, "a"),
            替换(5, 3, 5, "s1"),
            替换(6, 1, 6, "s2"),
        ];
        assert_eq!(cache_invalidation_point(&nodes), Some(EventSequence(1)));
    }
}
