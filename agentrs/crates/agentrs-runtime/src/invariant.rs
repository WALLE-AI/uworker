//! 运行时不变式（架构 §4.3.1，任务 T22A）。
//!
//! ## 把文档规定变成可执行断言
//!
//! 「模型可见即已记录」写在架构里是一句话，但没有任何东西阻止某次改动
//! 偷偷加一条旁路——比如把一段临时状态直接拼进请求。等到崩溃恢复时才发现
//! 那段内容凭空消失，代价已经产生。
//!
//! 因此在**每次组装模型请求时**断言：请求中的每条历史消息，都能在
//! log 前缀的 Surface 投影里找到对应节点。
//!
//! ## 检查时点在 legalization 之前
//!
//! `HistoryLegalization`（架构 §8.1）是**唯一**允许对事实做投影期修复的位置，
//! 它会合成 tool_result、丢弃失效签名——这些都会让请求与 Surface 不一致。
//!
//! 所以顺序必须是：
//!
//! ```text
//! Surface 投影 → 预算裁剪 → ★不变式断言★ → HistoryLegalization → 发送
//! ```
//!
//! 断言放在 legalization 之后会把合法的修复误判为违规；放在裁剪之前则
//! 无法覆盖"裁剪时混进了非 Surface 内容"这类错误。
//!
//! ## 违规的处理
//!
//! debug 构建 panic（开发期立刻暴露），release 构建产生
//! `RunFailed{InvariantViolated}`（生产环境宁可失败也不发出不可重建的请求）。

use agentrs_types::Message;

use crate::surface::{derive_messages, SurfaceNode};

/// 不变式违规。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum InvariantViolation {
    /// 请求里有一条历史消息在 Surface 投影中找不到来源。
    ///
    /// **这意味着存在一条绕过 durable log 的旁路**——该内容在崩溃恢复后会消失。
    #[error("model-visible message at index {index} has no source in the logged surface")]
    UnloggedMessage {
        /// 违规消息在请求中的下标。
        index: usize,
        /// 该消息的简短摘要，供诊断。**不含正文**，避免日志泄漏用户内容。
        summary: String,
    },
    /// 请求的历史比 Surface 投影长——多出来的部分无来源。
    #[error("request history has {request} messages but surface derives only {surface}")]
    LongerThanSurface {
        /// 请求中的历史条数。
        request: usize,
        /// Surface 投影出的条数。
        surface: usize,
    },
    /// 历史顺序与 Surface 投影不一致。
    ///
    /// 顺序错乱会破坏缓存前缀，也说明装配逻辑没有直接使用投影结果。
    #[error("request history order diverges from surface at index {index}")]
    OrderDiverged {
        /// 首个不一致的位置。
        index: usize,
    },
}

/// 不变式开关。
///
/// 关闭时全部检查是零开销的——`check` 立即返回。生产环境可按需关闭，
/// 但**默认开启**：这条不变式保护的是可重建性，不是性能。
#[derive(Debug, Clone, Copy)]
pub struct Invariants {
    /// 是否启用。
    pub enabled: bool,
    /// 违规时是否 panic。debug 构建默认为真。
    pub panic_on_violation: bool,
}

impl Default for Invariants {
    fn default() -> Self {
        Self {
            enabled: true,
            panic_on_violation: cfg!(debug_assertions),
        }
    }
}

impl Invariants {
    /// 全部关闭（性能剖析或压测时用）。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            panic_on_violation: false,
        }
    }

    /// 只报告不 panic（生产默认）。
    pub fn report_only() -> Self {
        Self {
            enabled: true,
            panic_on_violation: false,
        }
    }

    /// 断言「模型可见即已记录」。
    ///
    /// `history` 是**即将发送的历史消息**（不含 system 段、记忆与技能片段——
    /// 那些有各自的 `ContentRef` 溯源）；`surface` 是当前 log 前缀的 Surface 节点。
    ///
    /// **必须在 `HistoryLegalization` 之前调用**，理由见模块文档。
    pub fn check_model_visible_is_logged(
        &self,
        history: &[Message],
        surface: &[SurfaceNode],
    ) -> Result<(), InvariantViolation> {
        if !self.enabled {
            return Ok(());
        }

        let derived = derive_messages(surface);

        let result = if history.len() > derived.len() {
            Err(InvariantViolation::LongerThanSurface {
                request: history.len(),
                surface: derived.len(),
            })
        } else {
            history
                .iter()
                .enumerate()
                .find_map(|(i, m)| {
                    if derived[i] == *m {
                        None
                    } else if derived.contains(m) {
                        // 内容有来源，但位置不对。
                        Some(InvariantViolation::OrderDiverged { index: i })
                    } else {
                        Some(InvariantViolation::UnloggedMessage {
                            index: i,
                            summary: summarize(m),
                        })
                    }
                })
                .map_or(Ok(()), Err)
        };

        if let Err(v) = &result {
            if self.panic_on_violation {
                panic!("内核不变式被违反（模型可见即已记录）：{v}");
            }
        }
        result
    }
}

/// 生成不含正文的摘要，避免诊断信息泄漏用户内容。
fn summarize(m: &Message) -> String {
    format!("role={:?} blocks={}", m.role, m.content.len())
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::ids::EventSequence;
    use agentrs_contracts::surface::{SurfaceEventKind, SurfaceGeneration, SurfaceOp, SurfaceRange};
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

    fn 消息(text: &str) -> Message {
        Message::new(Role::User, vec![ContentBlock::text(text)])
    }

    /// 只报告不 panic，便于在测试里检查返回值。
    fn 检查器() -> Invariants {
        Invariants::report_only()
    }

    #[test]
    fn 完全来自_surface_的历史通过检查() {
        let surface = vec![节点(1, SurfaceOp::Append, "a"), 节点(2, SurfaceOp::Append, "b")];
        let history = vec![消息("a"), 消息("b")];
        assert!(检查器().check_model_visible_is_logged(&history, &surface).is_ok());
    }

    #[test]
    fn 绕过_log_直接进请求的内容被捕获() {
        // 这是本不变式存在的唯一理由：某次改动把一段临时状态直接拼进请求，
        // 它在崩溃恢复后会凭空消失。
        let surface = vec![节点(1, SurfaceOp::Append, "a")];
        let history = vec![消息("凭空出现的内容")];

        let err = 检查器()
            .check_model_visible_is_logged(&history, &surface)
            .unwrap_err();
        assert!(matches!(
            err,
            InvariantViolation::UnloggedMessage { index: 0, .. }
        ));
    }

    #[test]
    fn 违规诊断不包含用户正文() {
        // 诊断信息会进日志；泄漏正文违反可观测性要求。
        let surface = vec![节点(1, SurfaceOp::Append, "a")];
        let history = vec![消息("这是敏感的用户内容")];
        let err = 检查器()
            .check_model_visible_is_logged(&history, &surface)
            .unwrap_err();
        let text = format!("{err:?}");
        assert!(!text.contains("敏感"), "诊断不得含正文：{text}");
        assert!(text.contains("role="), "但应保留结构性信息：{text}");
    }

    #[test]
    fn 历史比_surface_长时被捕获() {
        let surface = vec![节点(1, SurfaceOp::Append, "a")];
        let history = vec![消息("a"), 消息("b")];
        assert_eq!(
            检查器().check_model_visible_is_logged(&history, &surface),
            Err(InvariantViolation::LongerThanSurface {
                request: 2,
                surface: 1
            })
        );
    }

    #[test]
    fn 顺序错乱与无来源被区分开() {
        // 二者的修法完全不同：前者是装配逻辑没直接用投影结果，
        // 后者是存在旁路。诊断必须能区分。
        let surface = vec![节点(1, SurfaceOp::Append, "a"), 节点(2, SurfaceOp::Append, "b")];
        let 乱序 = vec![消息("b"), 消息("a")];
        assert_eq!(
            检查器().check_model_visible_is_logged(&乱序, &surface),
            Err(InvariantViolation::OrderDiverged { index: 0 })
        );
    }

    #[test]
    fn 历史短于_surface_是允许的() {
        // 预算裁剪会砍掉低优先级历史——那是正常的，不是违规。
        let surface = vec![
            节点(1, SurfaceOp::Append, "a"),
            节点(2, SurfaceOp::Append, "b"),
            节点(3, SurfaceOp::Append, "c"),
        ];
        let history = vec![消息("a")];
        assert!(检查器().check_model_visible_is_logged(&history, &surface).is_ok());
    }

    #[test]
    fn 压缩后的历史仍能通过检查() {
        // Replace 节点遮蔽一段 range，投影结果随之改变——
        // 装配方只要直接用投影结果，检查自然通过。
        let surface = vec![
            节点(1, SurfaceOp::Append, "a"),
            节点(2, SurfaceOp::Append, "b"),
            节点(
                3,
                SurfaceOp::Replace {
                    range: SurfaceRange {
                        start: EventSequence(1),
                        end: EventSequence(3),
                    },
                    generation: SurfaceGeneration(1),
                },
                "摘要",
            ),
        ];
        let history = vec![消息("摘要")];
        assert!(检查器().check_model_visible_is_logged(&history, &surface).is_ok());
    }

    #[test]
    fn 关闭时是零开销且不报错() {
        let history = vec![消息("完全无来源")];
        assert!(Invariants::disabled()
            .check_model_visible_is_logged(&history, &[])
            .is_ok());
    }

    #[test]
    #[should_panic(expected = "内核不变式被违反")]
    fn 开启_panic_时违规立刻炸() {
        // 开发期的正确姿态：立刻暴露，而不是产出一个不可重建的请求。
        let inv = Invariants {
            enabled: true,
            panic_on_violation: true,
        };
        let _ = inv.check_model_visible_is_logged(&[消息("旁路")], &[]);
    }

    #[test]
    fn 默认配置在_debug_下会_panic() {
        // 这条固化"默认开启"这个决定——它保护的是可重建性，不是性能。
        let d = Invariants::default();
        assert!(d.enabled);
        assert_eq!(d.panic_on_violation, cfg!(debug_assertions));
    }

    #[test]
    fn 空历史与空_surface_通过() {
        assert!(检查器().check_model_visible_is_logged(&[], &[]).is_ok());
    }
}
