//! HistoryLegalization：事实流与 provider 可接受输入之间的固定阶段（架构 §8.1）。
//!
//! ## 为什么必须有这个阶段
//!
//! durable 事实流与"provider 当下能接受的请求"之间存在**不可消除的阻抗**：
//!
//! | 阻抗来源 | 现象 |
//! |---|---|
//! | reasoning 签名是 provider 侧不透明且有生命周期的令牌 | 跨 provider fallback、跨压缩、跨长时间挂起后失效 → 400 |
//! | `tool_use` 必须紧跟 `tool_result`（Anthropic 族） | 取消或崩溃后历史残缺 → 400 |
//! | 图像/附件在目标模型不支持 | 需降级为文本占位 |
//! | 相邻同角色消息 | 各厂商约束不同 |
//!
//! 假装它不存在的代价是：一次 provider fallback 就会因为携带了失效签名而失败，
//! 而那时上下文已经装配完毕。
//!
//! ## 四条不变式
//!
//! 1. **只读事实、只改投影**——不写回事件流，不修改 `ConversationSnapshot`；
//! 2. **必须留痕**——全部 op 进入 `ModelRequestManifest.legalization_ops`，
//!    使"同一段历史为什么这次和上次发给模型的不一样"可解释；
//! 3. **确定性**——相同输入历史 + 相同 compat 必然产生相同 op 序列与相同输出，
//!    否则 replay 不可重现；
//! 4. **不得扩大信息**——只能删除、合成占位、重排；不能引入新的用户或工具内容。
//!
//! 现状参照：aionrs 已有 `tool_call_sanitize.rs` 与 `abort_current_turn()`
//! （补合成 tool_result），AionCore 有 `history_sanitize.rs`。三处分散逻辑
//! 在本模块收敛为一个阶段。

use agentrs_contracts::manifest::{LegalizationOp, LegalizationReason};
use agentrs_types::{ContentBlock, Message, Role};

use crate::compat::ProviderCompat;

/// 合成 `tool_result` 的固定内容。
///
/// **必须与真实结果在 trajectory 上可区分**——否则无法回答
/// "这个结果是工具产出的还是我们补的"。
pub const SYNTHETIC_TOOL_RESULT: &str = "[执行被中断，结果未知]";

/// 图像被降级时的文本占位。
pub const DROPPED_IMAGE_PLACEHOLDER: &str = "[图像已省略：目标模型不支持图像输入]";

/// 合法化产物。
#[derive(Debug, Clone, PartialEq)]
pub struct Legalized {
    /// 可安全发送给目标 provider 的消息。
    pub messages: Vec<Message>,
    /// 本次执行的全部操作，按发生顺序。**留痕，进 manifest。**
    pub ops: Vec<LegalizationOp>,
}

/// 把一段历史合法化为目标 provider 可接受的形式。
///
/// **纯函数**：相同 `(messages, compat)` 必然产生相同结果，这是 replay 可重现的前提。
pub fn legalize(messages: &[Message], compat: &ProviderCompat) -> Legalized {
    let mut ops = Vec::new();

    // 阶段 1：块级过滤——丢弃目标不接受的块。
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    for (idx, m) in messages.iter().enumerate() {
        let mut blocks = Vec::with_capacity(m.content.len());
        for b in &m.content {
            match b {
                ContentBlock::Thinking { .. } if !compat.accepts_thinking_blocks => {
                    // 签名有生命周期；跨 provider 重放会被拒。
                    ops.push(LegalizationOp::DroppedReasoningSignature { message_index: idx });
                }
                // **接受 thinking，但这一块没有签名。**
                // Anthropic 对无签名 thinking 同样拒收。此前这类块由厂商投影器
                // 静默丢弃——投影期丢弃不留痕，于是"为什么这次请求少了一段思考"
                // 无从解释。合法化是唯一允许做投影期修复的位置，留痕在这里。
                ContentBlock::Thinking { signature: None, .. } => {
                    ops.push(LegalizationOp::DroppedReasoningSignature { message_index: idx });
                }
                ContentBlock::Image { .. } if !compat.image_input.supports_images() => {
                    ops.push(LegalizationOp::DroppedUnsupportedBlock {
                        message_index: idx,
                        kind: "image".into(),
                    });
                    // 降级为文本占位而非静默丢弃——模型需要知道"这里本来有张图"。
                    blocks.push(ContentBlock::text(DROPPED_IMAGE_PLACEHOLDER));
                }
                ContentBlock::ProviderItem { .. } if !compat.accepts_provider_items => {
                    ops.push(LegalizationOp::DroppedUnsupportedBlock {
                        message_index: idx,
                        kind: "provider_item".into(),
                    });
                }
                other => blocks.push(other.clone()),
            }
        }
        out.push(Message {
            role: m.role,
            content: blocks,
            timestamp: m.timestamp,
        });
    }

    // 阶段 2：先丢弃孤儿 tool_result，再补齐残缺 tool_use。
    //
    // 这两步**不会互相干扰**：合成结果引用的 `tool_use` 必然在场
    // （它正是因为在场且缺结果才被补的），所以永远不会被当成孤儿。
    // 反过来，孤儿结果的 `tool_use` 不在场，也不会有人去补它。
    //
    // 顺序因此是**为了报告可读性**而非正确性：先清理再补齐，
    // op 序列读起来是"先把不该在的删掉，再把该有的加上"。
    // 交换顺序不会改变最终历史——这一点由 golden fixture 钉住。
    if compat.requires_tool_result_adjacency {
        out = drop_orphan_tool_results(out, &mut ops);
        out = close_orphan_tool_uses(out, &mut ops);
    }

    // 阶段 3：丢弃被掏空的消息。
    //
    // 放在块级过滤**之后**：过滤才是把消息掏空的原因。
    out = drop_empty_messages(out, &mut ops);

    // 阶段 4：合并相邻同角色消息。
    //
    // 放在最后：前面几步会删掉消息，删完之后才知道谁和谁真的相邻。
    if compat.merges_adjacent_same_role {
        out = merge_adjacent(out, &mut ops);
    }

    Legalized { messages: out, ops }
}

/// 丢弃引用了不存在 `tool_use` 的结果。
///
/// 压缩截断、从中途分叉都会造出孤儿结果。Anthropic 族对
/// "引用了不存在的 tool_use" 直接 400。
fn drop_orphan_tool_results(messages: Vec<Message>, ops: &mut Vec<LegalizationOp>) -> Vec<Message> {
    let 已声明: std::collections::HashSet<_> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();

    messages
        .into_iter()
        .map(|m| {
            let content = m
                .content
                .into_iter()
                .filter(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } if !已声明.contains(tool_use_id) => {
                        ops.push(LegalizationOp::DroppedOrphanToolResult {
                            tool_use_id: tool_use_id.clone(),
                        });
                        false
                    }
                    _ => true,
                })
                .collect();
            Message { content, ..m }
        })
        .collect()
}

/// 丢弃合法化之后内容为空的消息。
///
/// 下标用的是**本阶段入口时**的位置。它与原始历史的下标可能已经不同——
/// 前面的阶段插入过合成结果。留痕的用途是"看懂发生了什么"，
/// 不是"回指原始事件"，后者由事件流本身负责。
fn drop_empty_messages(messages: Vec<Message>, ops: &mut Vec<LegalizationOp>) -> Vec<Message> {
    messages
        .into_iter()
        .enumerate()
        .filter_map(|(i, m)| {
            // 全部块都是空文本，或干脆没有块。
            if m.content.is_empty() || m.content.iter().all(ContentBlock::is_empty) {
                ops.push(LegalizationOp::DroppedEmptyMessage { message_index: i });
                None
            } else {
                Some(m)
            }
        })
        .collect()
}

/// 为没有对应 `tool_result` 的 `tool_use` 补一个合成结果。
///
/// 取消或崩溃后，assistant 的 `tool_use` 可能已经落盘而结果没有——
/// 这时历史对 Anthropic 族是非法的。
fn close_orphan_tool_uses(messages: Vec<Message>, ops: &mut Vec<LegalizationOp>) -> Vec<Message> {
    // 先收集所有已有结果的 id。
    let satisfied: std::collections::HashSet<_> = messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect();

    let mut out = Vec::with_capacity(messages.len());
    for m in messages {
        let orphans: Vec<_> = m
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, .. } if !satisfied.contains(id) => Some(id.clone()),
                _ => None,
            })
            .collect();

        let ts = m.timestamp;
        out.push(m);

        if orphans.is_empty() {
            continue;
        }

        // 紧跟其后补一条 user 消息承载合成结果，保持 adjacency。
        let blocks = orphans
            .into_iter()
            .map(|id| {
                ops.push(LegalizationOp::SyntheticToolResult {
                    tool_use_id: id.clone(),
                    reason: LegalizationReason::Interrupted,
                });
                ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: SYNTHETIC_TOOL_RESULT.to_string(),
                    is_error: true,
                }
            })
            .collect();

        out.push(Message {
            role: Role::User,
            content: blocks,
            timestamp: ts,
        });
    }
    out
}

/// 合并相邻同角色消息。
///
/// 每发生一次实际合并记一条 op —— 留痕要求覆盖每一次修复，而不是只记"发生过合并"。
fn merge_adjacent(messages: Vec<Message>, ops: &mut Vec<LegalizationOp>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut run_start = 0usize;

    for (i, m) in messages.into_iter().enumerate() {
        match out.last_mut() {
            Some(prev) if prev.role == m.role => {
                prev.content.extend(m.content);
                ops.push(LegalizationOp::MergedAdjacentMessages {
                    start: run_start,
                    end: i + 1,
                });
            }
            _ => {
                run_start = i;
                out.push(m);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use agentrs_types::ImageUrl;

    use super::*;

    fn 助手工具调用(id: &str) -> Message {
        Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "Read".into(),
                input: serde_json::json!({}),
                extra: None,
            }],
        )
    }

    fn 工具结果(id: &str) -> Message {
        Message::new(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: "ok".into(),
                is_error: false,
            }],
        )
    }

    fn 思考(sig: Option<&str>) -> Message {
        Message::new(
            Role::Assistant,
            vec![ContentBlock::Thinking {
                thinking: "推理中".into(),
                signature: sig.map(str::to_string),
            }],
        )
    }

    fn 文本(role: Role, t: &str) -> Message {
        Message::new(role, vec![ContentBlock::text(t)])
    }

    // ---------- 残缺 tool_use ----------

    #[test]
    fn 残缺的_tool_use_被补上合成结果() {
        // 取消或崩溃后最常见的非法历史形态。
        let msgs = vec![文本(Role::User, "读文件"), 助手工具调用("tc1")];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);

        assert_eq!(r.messages.len(), 3, "补了一条承载合成结果的消息");
        assert_eq!(
            r.ops,
            vec![LegalizationOp::SyntheticToolResult {
                tool_use_id: "tc1".into(),
                reason: LegalizationReason::Interrupted,
            }]
        );
    }

    #[test]
    fn 合成结果与真实结果可区分() {
        // 否则无法回答"这个结果是工具产出的还是我们补的"。
        let msgs = vec![助手工具调用("tc1")];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        match &r.messages[1].content[0] {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => {
                assert_eq!(content, SYNTHETIC_TOOL_RESULT);
                assert!(*is_error, "合成结果必须标记为错误");
            }
            other => panic!("期望 ToolResult，得到 {other:?}"),
        }
    }

    #[test]
    fn 已有结果的_tool_use_不被重复补() {
        let msgs = vec![助手工具调用("tc1"), 工具结果("tc1")];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        assert_eq!(r.messages.len(), 2);
        assert!(r.ops.is_empty(), "无需修复时不产生任何 op");
    }

    #[test]
    fn 不要求_adjacency_的端点不补合成结果() {
        let msgs = vec![助手工具调用("tc1")];
        let r = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
        assert_eq!(r.messages.len(), 1);
        assert!(r.ops.is_empty());
    }

    #[test]
    fn 多个残缺调用各补一条() {
        let msgs = vec![Message::new(
            Role::Assistant,
            vec![
                ContentBlock::ToolUse {
                    id: "a".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                    extra: None,
                },
                ContentBlock::ToolUse {
                    id: "b".into(),
                    name: "Read".into(),
                    input: serde_json::json!({}),
                    extra: None,
                },
            ],
        )];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        assert_eq!(r.ops.len(), 2);
        assert_eq!(r.messages[1].content.len(), 2, "两个合成结果并入一条消息");
    }

    // ---------- reasoning 签名 ----------

    #[test]
    fn 目标不接受时丢弃_thinking_并留痕() {
        // 签名有生命周期；跨 provider 重放会被拒。
        let msgs = vec![思考(Some("sig-abc"))];
        let r = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);

        // 唯一的块被丢掉后消息就空了，**空消息不能留在历史里**——
        // 多数端点直接拒收，且它对模型没有任何信息量。
        assert!(r.messages.is_empty());
        assert_eq!(
            r.ops,
            vec![
                LegalizationOp::DroppedReasoningSignature { message_index: 0 },
                LegalizationOp::DroppedEmptyMessage { message_index: 0 },
            ]
        );
    }

    #[test]
    fn 目标接受时保留_thinking() {
        let msgs = vec![思考(Some("sig"))];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        assert_eq!(r.messages[0].content.len(), 1);
        assert!(r.ops.is_empty());
    }

    #[test]
    fn 丢弃的签名不出现在输出中() {
        let msgs = vec![思考(Some("SECRET-SIGNATURE"))];
        let r = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
        let dumped = format!("{:?}", r.messages);
        assert!(!dumped.contains("SECRET-SIGNATURE"));
    }

    // ---------- 图像 ----------

    #[test]
    fn 不支持图像时降级为占位而非静默丢弃() {
        // 模型需要知道"这里本来有张图"，否则会对缺失的上下文产生错误推断。
        let msgs = vec![Message::new(
            Role::User,
            vec![ContentBlock::Image {
                image_url: ImageUrl {
                    url: "data:image/png;base64,AA".into(),
                },
            }],
        )];
        let r = legalize(&msgs, &ProviderCompat::TEXT_ONLY);

        assert_eq!(
            r.messages[0].content,
            vec![ContentBlock::text(DROPPED_IMAGE_PLACEHOLDER)]
        );
        assert_eq!(
            r.ops,
            vec![LegalizationOp::DroppedUnsupportedBlock {
                message_index: 0,
                kind: "image".into(),
            }]
        );
    }

    #[test]
    fn 支持图像时原样保留() {
        let msgs = vec![Message::new(
            Role::User,
            vec![ContentBlock::Image {
                image_url: ImageUrl { url: "x".into() },
            }],
        )];
        let r = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
        assert!(matches!(r.messages[0].content[0], ContentBlock::Image { .. }));
        assert!(r.ops.is_empty());
    }

    // ---------- 相邻合并 ----------

    #[test]
    fn 相邻同角色消息被合并() {
        let msgs = vec![
            文本(Role::User, "a"),
            文本(Role::User, "b"),
            文本(Role::Assistant, "c"),
        ];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages[0].content.len(), 2, "两条 user 合并为一条");
        assert_eq!(
            r.ops,
            vec![LegalizationOp::MergedAdjacentMessages { start: 0, end: 2 }],
            "每次实际合并都要留痕"
        );
    }

    #[test]
    fn 不要求合并的端点保持原样() {
        let msgs = vec![文本(Role::User, "a"), 文本(Role::User, "b")];
        let r = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
        assert_eq!(r.messages.len(), 2);
    }

    // ---------- 四条不变式 ----------

    #[test]
    fn 不变式3_确定性_相同输入产生相同输出() {
        // replay 可重现的前提。
        let msgs = vec![文本(Role::User, "x"), 思考(Some("s")), 助手工具调用("tc1")];
        let first = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        for _ in 0..8 {
            assert_eq!(legalize(&msgs, &ProviderCompat::ANTHROPIC), first);
        }
    }

    #[test]
    fn 不变式1_只读事实不修改输入() {
        let msgs = vec![助手工具调用("tc1")];
        let 快照 = msgs.clone();
        let _ = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        assert_eq!(msgs, 快照, "输入历史不得被修改");
    }

    #[test]
    fn 不变式4_不引入新的用户或工具内容() {
        // 只能删除、合成占位、重排。合成的 tool_result 是固定占位文本，
        // 不是凭空编造的工具输出。
        let msgs = vec![助手工具调用("tc1")];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        let texts: Vec<String> = r
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            texts.iter().all(|t| t == SYNTHETIC_TOOL_RESULT),
            "除固定占位外不得引入内容：{texts:?}"
        );
    }

    #[test]
    fn 不变式2_每次修复都留痕() {
        let msgs = vec![思考(Some("s")), 助手工具调用("tc1")];
        let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
        // ANTHROPIC 接受 thinking；thinking 与 tool_use 同为 assistant 角色会被合并，
        // 因此这里有「合并 + 补合成结果」两条 op。
        assert!(r
            .ops
            .iter()
            .any(|o| matches!(o, LegalizationOp::SyntheticToolResult { .. })));

        let r2 = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
        // OPENAI_COMPAT 丢 thinking 但不要求 adjacency。
        // thinking 那条被掏空后一并丢弃 —— 两条 op，各留各的痕。
        assert_eq!(
            r2.ops,
            vec![
                LegalizationOp::DroppedReasoningSignature { message_index: 0 },
                LegalizationOp::DroppedEmptyMessage { message_index: 0 },
            ]
        );
    }

    #[test]
    fn 无需修复时不产生任何_op() {
        let msgs = vec![文本(Role::User, "hi"), 文本(Role::Assistant, "hello")];
        let r = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
        assert!(r.ops.is_empty());
        assert_eq!(r.messages, msgs);
    }

    #[test]
    fn 跨_provider_切换的完整场景() {
        // Anthropic 上产生的历史（含带签名的 thinking + 残缺 tool_use），
        // fallback 到 OpenAI 兼容端点时必须能被安全发送。
        let msgs = vec![
            文本(Role::User, "帮我改代码"),
            思考(Some("anthropic-sig")),
            助手工具调用("tc1"),
        ];
        let r = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
        let dumped = format!("{:?}", r.messages);
        assert!(!dumped.contains("anthropic-sig"), "失效签名必须被清除");
        // 丢签名 + 丢被掏空的那条消息。
        assert_eq!(r.ops.len(), 2, "{:?}", r.ops);
    }
}
