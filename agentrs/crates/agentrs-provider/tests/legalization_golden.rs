//! HistoryLegalization 的 golden 序列与跨 provider fixture（架构 §8.1，任务 T08）。
//!
//! ## 为什么要 golden 而不是逐条断言
//!
//! 合法化是**一串有序的修复**，而顺序本身就是语义的一部分：先删孤儿结果
//! 再补残缺调用，和反过来，最终历史不一样。逐条断言"有没有发生某次修复"
//! 抓不住顺序错误；把整条 op 序列钉死才能。
//!
//! golden 变化时**不要直接改期望值**——先回答"这次修复顺序的改变对不对"。
//! 这里的每条期望都对应一个真实的 400。
//!
//! ## 三条不变式在这里被系统性检验
//!
//! | 不变式 | 怎么验 |
//! |---|---|
//! | 确定性 | 同一输入折两次，op 序列与输出逐字相同 |
//! | 幂等 | `legalize(legalize(x)) == legalize(x)`，且第二次不产生新 op |
//! | 不扩大信息 | 输出里出现的每一段文本，要么来自输入，要么是两个已知占位常量 |

use agentrs_contracts::manifest::{LegalizationOp, LegalizationReason};
use agentrs_provider::legalization::{legalize, DROPPED_IMAGE_PLACEHOLDER, SYNTHETIC_TOOL_RESULT};
use agentrs_provider::ProviderCompat;
use agentrs_types::{ContentBlock, ImageUrl, Message, Role};

// ---------------------------------------------------------------------------
// 素材
// ---------------------------------------------------------------------------

fn 用户(t: &str) -> Message {
    Message::new(Role::User, vec![ContentBlock::text(t)])
}

fn 助手(t: &str) -> Message {
    Message::new(Role::Assistant, vec![ContentBlock::text(t)])
}

fn 调用(id: &str) -> Message {
    Message::new(
        Role::Assistant,
        vec![ContentBlock::ToolUse {
            id: id.into(),
            name: "Read".into(),
            input: serde_json::json!({"path": "a.rs"}),
            extra: None,
        }],
    )
}

fn 结果(id: &str) -> Message {
    Message::new(
        Role::User,
        vec![ContentBlock::ToolResult {
            tool_use_id: id.into(),
            content: "文件内容".into(),
            is_error: false,
        }],
    )
}

fn 思考(sig: Option<&str>) -> Message {
    Message::new(
        Role::Assistant,
        vec![ContentBlock::Thinking {
            thinking: "内部推理".into(),
            signature: sig.map(str::to_owned),
        }],
    )
}

fn 图片() -> Message {
    Message::new(
        Role::User,
        vec![ContentBlock::Image {
            image_url: ImageUrl {
                url: "data:image/png;base64,QUJD".into(),
            },
        }],
    )
}

/// **一段真实会出现的坏历史。**
///
/// 来源是一次崩溃后的恢复：压缩截断了前半段（留下孤儿结果）、
/// 崩溃时有个工具调用没等到结果（残缺调用）、还带着 Anthropic 的
/// 有签名与无签名思考各一段，以及一张图。
fn 坏历史() -> Vec<Message> {
    vec![
        用户("帮我看看代码"),
        // 压缩把这条结果对应的 tool_use 截掉了 → 孤儿结果。
        结果("被压缩掉的-tc0"),
        思考(Some("anthropic-sig")),
        // 流被打断，签名没下发 → 无签名思考。
        思考(None),
        调用("tc1"),
        结果("tc1"),
        // 崩溃时这个还没等到结果 → 残缺调用。
        调用("tc2"),
        图片(),
    ]
}

/// 把 op 序列压成便于对照的短名，避免期望值被字段细节淹没。
fn 摘要(ops: &[LegalizationOp]) -> Vec<String> {
    ops.iter()
        .map(|o| match o {
            LegalizationOp::SyntheticToolResult { tool_use_id, .. } => {
                format!("补结果({tool_use_id})")
            }
            LegalizationOp::DroppedOrphanToolResult { tool_use_id } => {
                format!("删孤儿结果({tool_use_id})")
            }
            LegalizationOp::DroppedReasoningSignature { message_index } => {
                format!("删思考(@{message_index})")
            }
            LegalizationOp::DroppedUnsupportedBlock { kind, .. } => format!("删块({kind})"),
            LegalizationOp::DroppedEmptyMessage { message_index } => {
                format!("删空消息(@{message_index})")
            }
            LegalizationOp::MergedAdjacentMessages { start, end } => format!("合并({start}..{end})"),
            LegalizationOp::RewrittenToolCallId { from, to } => format!("改id({from}→{to})"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// golden：同一段坏历史，三种 provider
// ---------------------------------------------------------------------------

#[test]
fn golden_anthropic() {
    let r = legalize(&坏历史(), &ProviderCompat::ANTHROPIC);

    assert_eq!(
        摘要(&r.ops),
        [
            // 阶段 1（块级过滤）：无签名思考同样被 Anthropic 拒收；
            // 有签名的那条保留。
            "删思考(@3)",
            // 阶段 2a：删孤儿结果——它引用的 tool_use 已被压缩截掉。
            "删孤儿结果(被压缩掉的-tc0)",
            // 阶段 2b：补残缺调用的结果，保持 tool_use / tool_result 紧邻。
            // 与 2a 互不干扰；这里的先后只影响报告可读性。
            "补结果(tc2)",
            // 阶段 3：前两步掏空了两条消息（无签名思考那条、孤儿结果那条）。
            "删空消息(@1)",
            "删空消息(@3)",
            // 阶段 4：最后合并相邻同角色——删完消息之后才知道谁和谁真的相邻。
            "合并(1..3)",
            "合并(5..7)",
        ],
        "op 序列变了。先回答顺序的改变对不对，再改期望值。"
    );

    let d = format!("{:?}", r.messages);
    assert!(d.contains("anthropic-sig"), "有签名的思考应当保留");
    assert!(!d.contains("被压缩掉的-tc0"), "孤儿结果必须清除");
    assert!(d.contains(SYNTHETIC_TOOL_RESULT), "残缺调用必须补上合成结果");
    assert!(!d.contains(DROPPED_IMAGE_PLACEHOLDER), "Anthropic 支持图像");
}

#[test]
fn golden_openai_compat() {
    let r = legalize(&坏历史(), &ProviderCompat::OPENAI_COMPAT);

    assert_eq!(
        摘要(&r.ops),
        [
            // 不接受 thinking —— 两条都删，**包括带签名的那条**：
            // 签名是 Anthropic 侧的不透明令牌，换端点就失效。
            "删思考(@2)",
            "删思考(@3)",
            "删空消息(@2)",
            "删空消息(@3)",
        ],
        "op 序列变了"
    );

    let d = format!("{:?}", r.messages);
    assert!(!d.contains("anthropic-sig"), "跨端点的失效签名必须清除");
    // **不要求 adjacency**：孤儿结果与残缺调用都原样保留。
    assert!(d.contains("被压缩掉的-tc0"));
    assert!(!d.contains(SYNTHETIC_TOOL_RESULT));
}

#[test]
fn golden_text_only() {
    let r = legalize(&坏历史(), &ProviderCompat::TEXT_ONLY);

    assert_eq!(
        摘要(&r.ops),
        [
            "删思考(@2)",
            "删思考(@3)",
            "删块(image)",
            "删空消息(@2)",
            "删空消息(@3)"
        ],
        "op 序列变了"
    );

    let d = format!("{:?}", r.messages);
    // 图像**降级为占位而非静默丢弃**——模型需要知道"这里本来有张图"。
    assert!(d.contains(DROPPED_IMAGE_PLACEHOLDER));
    assert!(!d.contains("QUJD"), "图像数据必须清除");
}

#[test]
fn 三种_provider_的修复量依次递增() {
    // 一个横向对照：越保守的端点需要越多修复。
    // 这条抓的是"某个 compat 档位被误配成了另一档"。
    let a = legalize(&坏历史(), &ProviderCompat::ANTHROPIC).ops.len();
    let o = legalize(&坏历史(), &ProviderCompat::OPENAI_COMPAT).ops.len();
    let t = legalize(&坏历史(), &ProviderCompat::TEXT_ONLY).ops.len();
    assert!(t > o, "TEXT_ONLY 至少要比 OPENAI_COMPAT 多一次图像降级");
    assert!(a > 0 && o > 0);
}

// ---------------------------------------------------------------------------
// 不变式
// ---------------------------------------------------------------------------

#[test]
fn 不变式3_确定性() {
    // replay 可重现的前提。
    for compat in [
        ProviderCompat::ANTHROPIC,
        ProviderCompat::OPENAI_COMPAT,
        ProviderCompat::TEXT_ONLY,
    ] {
        let a = legalize(&坏历史(), &compat);
        let b = legalize(&坏历史(), &compat);
        assert_eq!(a.ops, b.ops, "op 序列不确定");
        assert_eq!(a.messages, b.messages, "输出不确定");
    }
}

#[test]
fn 幂等_合法化过的历史再过一遍不再产生修复() {
    // **这条不是锦上添花。** 重试、fallback、压缩之后重新装配，都会让
    // 同一段历史被合法化不止一次。若不幂等，每过一遍就多一层合成结果，
    // 历史会越修越长、越修越假。
    for compat in [
        ProviderCompat::ANTHROPIC,
        ProviderCompat::OPENAI_COMPAT,
        ProviderCompat::TEXT_ONLY,
    ] {
        let 第一遍 = legalize(&坏历史(), &compat);
        let 第二遍 = legalize(&第一遍.messages, &compat);

        assert_eq!(第二遍.messages, 第一遍.messages, "{compat:?}：第二遍改变了历史");
        assert!(
            第二遍.ops.is_empty(),
            "{compat:?}：第二遍仍在修复 {:?}",
            摘要(&第二遍.ops)
        );
    }
}

#[test]
fn 不变式4_不得引入新信息() {
    // 只能删除、合成占位、重排。输出里出现的每一段文本，
    // 要么来自输入，要么是两个**已知的**占位常量。
    let 输入 = 坏历史();
    let 原文: Vec<String> = 输入
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            ContentBlock::Thinking { thinking, .. } => Some(thinking.clone()),
            _ => None,
        })
        .collect();

    for compat in [
        ProviderCompat::ANTHROPIC,
        ProviderCompat::OPENAI_COMPAT,
        ProviderCompat::TEXT_ONLY,
    ] {
        let r = legalize(&输入, &compat);
        for m in &r.messages {
            for b in &m.content {
                let t = match b {
                    ContentBlock::Text { text } => text.clone(),
                    ContentBlock::ToolResult { content, .. } => content.clone(),
                    ContentBlock::Thinking { thinking, .. } => thinking.clone(),
                    _ => continue,
                };
                let 已知 = 原文.contains(&t) || t == SYNTHETIC_TOOL_RESULT || t == DROPPED_IMAGE_PLACEHOLDER;
                assert!(已知, "{compat:?}：输出里出现了输入中没有的文本 {t:?}");
            }
        }
    }
}

#[test]
fn 合成结果与真实结果在_trajectory_上可区分() {
    // 否则无法回答"这个结果是工具产出的还是我们补的"。
    let r = legalize(&坏历史(), &ProviderCompat::ANTHROPIC);
    let 合成的: Vec<_> = r
        .ops
        .iter()
        .filter_map(|o| match o {
            LegalizationOp::SyntheticToolResult { tool_use_id, reason } => {
                Some((tool_use_id.to_string(), *reason))
            }
            _ => None,
        })
        .collect();
    assert_eq!(合成的, [("tc2".to_string(), LegalizationReason::Interrupted)]);

    // 真实结果的内容不得与合成占位相同。
    let d = format!("{:?}", r.messages);
    assert!(d.contains("文件内容"), "真实结果应当保留");
}

// ---------------------------------------------------------------------------
// 边界
// ---------------------------------------------------------------------------

#[test]
fn 干净的历史不产生任何修复() {
    // 对照组：上面那些 op 不是"合法化总会做点什么"。
    //
    // 严格交替、无工具、无思考、无图。
    //
    // 注意**带工具调用的历史很难对 Anthropic 干净**：`tool_result` 在本模型里
    // 是 user 消息，它后面再跟一条用户输入就构成相邻同角色，必然触发合并。
    // 这不是缺陷，是 Anthropic 的约束——见下一条。
    let 干净 = vec![用户("你好"), 助手("你好"), 用户("再见"), 助手("再见")];
    for compat in [
        ProviderCompat::ANTHROPIC,
        ProviderCompat::OPENAI_COMPAT,
        ProviderCompat::TEXT_ONLY,
    ] {
        let r = legalize(&干净, &compat);
        assert!(r.ops.is_empty(), "{compat:?}：{:?}", 摘要(&r.ops));
        assert_eq!(r.messages, 干净);
    }
}

#[test]
fn 对一个端点干净的历史对另一个端点未必干净() {
    // 两条相邻的 assistant：OpenAI 兼容端点照单全收，Anthropic 直接 400。
    // **"干净"不是历史的属性，是历史与目标端点的关系。**
    // 把它当成历史自身的属性，就会在 fallback 时踩坑。
    let msgs = vec![用户("问"), 助手("答一"), 助手("答二")];

    let o = legalize(&msgs, &ProviderCompat::OPENAI_COMPAT);
    assert!(o.ops.is_empty(), "OpenAI 兼容端点无需修复");
    assert_eq!(o.messages.len(), 3);

    let a = legalize(&msgs, &ProviderCompat::ANTHROPIC);
    assert_eq!(摘要(&a.ops), ["合并(1..3)"]);
    assert_eq!(a.messages.len(), 2, "两条 assistant 被合并成一条");
}

#[test]
fn 空历史不恐慌() {
    for compat in [
        ProviderCompat::ANTHROPIC,
        ProviderCompat::OPENAI_COMPAT,
        ProviderCompat::TEXT_ONLY,
    ] {
        let r = legalize(&[], &compat);
        assert!(r.messages.is_empty());
        assert!(r.ops.is_empty());
    }
}

#[test]
fn 全部内容都被丢弃时得到空历史而不是空消息() {
    // 一条只有图片的历史发给纯文本端点：图片降级为占位，消息不空。
    let r = legalize(&[图片()], &ProviderCompat::TEXT_ONLY);
    assert_eq!(r.messages.len(), 1);

    // 一条只有无签名思考的历史：整条消息被删掉，不留空壳。
    let r = legalize(&[思考(None)], &ProviderCompat::ANTHROPIC);
    assert!(r.messages.is_empty());
}

#[test]
fn 同一个调用有多个孤儿结果时逐条留痕() {
    // 留痕要覆盖每一次修复，不是只记"发生过"。
    let msgs = vec![结果("幽灵"), 结果("幽灵"), 用户("继续")];
    let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
    assert_eq!(
        摘要(&r.ops),
        [
            "删孤儿结果(幽灵)",
            "删孤儿结果(幽灵)",
            "删空消息(@0)",
            "删空消息(@1)"
        ]
    );
}

#[test]
fn 补出来的合成结果不会被当成孤儿再删一遍() {
    // 两个阶段互不干扰的直接检验：合成结果引用的 `tool_use` 必然在场
    // （它正是因为在场且缺结果才被补的），因此永远不是孤儿。
    //
    // 这条**不依赖阶段顺序**——交换顺序后最终历史不变，只有 op 序列变。
    // 顺序由 `golden_anthropic` 钉住，理由是报告可读性，不是正确性。
    let msgs = vec![用户("跑一下"), 调用("tc9")];
    let r = legalize(&msgs, &ProviderCompat::ANTHROPIC);
    assert!(format!("{:?}", r.messages).contains(SYNTHETIC_TOOL_RESULT));
    assert!(!r
        .ops
        .iter()
        .any(|o| matches!(o, LegalizationOp::DroppedOrphanToolResult { .. })));
}
