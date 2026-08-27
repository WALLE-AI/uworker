//! `compact` 子 Agent：为一段待遮蔽的历史生成结构化摘要（架构 §9.3、§11.1）。
//!
//! ## 这里的失败必须 fail-closed
//!
//! 压缩的动作是"用一段摘要**遮蔽**一段历史"。这两步是绑在一起的：
//! 摘要没生成出来还照样遮蔽，就等于**把一段历史换成了空白**——
//! 那比不压缩糟得多，模型会突然失忆而且没人知道为什么。
//!
//! 所以本模块的每条失败路径都返回 `Err`，**绝不返回一个"尽力而为"的部分摘要**。
//! 调用方拿不到摘要就不该遮蔽，宁可让请求继续超长、由溢出触发去处理。
//!
//! ## 为什么要校验必填项
//!
//! §9.3 列的十项不是建议清单。少一项就意味着恢复后模型丢掉一类信息，
//! 而那类信息往往正是它接着要用的——比如"未决审批"漏了，
//! 模型会以为那个操作已经批过了。
//!
//! 摘要由模型生成，**模型会漏**。提示词里写了不等于它照做，所以要检查。

use agentrs_context::compaction::{missing_sections, SUMMARY_REQUIRED_SECTIONS};
use agentrs_types::Message;

use crate::functional::{SubagentError, SubagentInput, SubagentOutput};

/// `compact` 的系统提示。
///
/// 正文来自 [`agentrs_prompts::registry::COMPACT`]——**提示词集中在一处**，
/// 才能对它施加版本号、snapshot 与安全 lint。必填项列表由本 crate 填入，
/// `SUMMARY_REQUIRED_SECTIONS` 是唯一事实源，提示词模板里不重复写一遍。
pub fn system_prompt() -> String {
    let sections = SUMMARY_REQUIRED_SECTIONS
        .iter()
        .map(|s| format!("- {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    agentrs_prompts::registry::COMPACT
        .render(&[("sections", sections)].into_iter().collect())
        .expect("COMPACT 的输入声明与模板必须一致（registry 测试保证）")
}

/// 本次使用的提示词引用，进 manifest。
pub fn prompt_ref() -> agentrs_prompts::PromptRef {
    agentrs_prompts::registry::COMPACT.as_ref()
}

/// 组装一次 compact 调用的输入。
///
/// `to_summarize` 是**将被遮蔽的那一段**，不是全部历史——
/// 子 Agent 只该看到它要总结的东西。
///
/// ## 历史被裹进一条 user 消息，而不是原样当对话轮次
///
/// 初版直接把历史当 `context` 的多条消息传入，模型**时而**会把它当成
/// 一场进行中的对话接着说下去，完全无视系统提示。
/// 见 [`COMPACT_INPUT_WRAPPER`](agentrs_prompts::registry::COMPACT_INPUT_WRAPPER)。
pub fn input(
    to_summarize: Vec<Message>,
    model: agentrs_contracts::ids::ModelId,
    max_tokens: Option<u32>,
) -> SubagentInput {
    let history = render_history(&to_summarize);
    let wrapped = agentrs_prompts::registry::COMPACT_INPUT_WRAPPER
        .render(&[("history", history)].into_iter().collect())
        .expect("COMPACT_INPUT_WRAPPER 的输入声明与模板必须一致");

    SubagentInput {
        task: "压缩对话历史".into(),
        context: vec![Message::new(
            agentrs_types::Role::User,
            vec![agentrs_types::ContentBlock::text(wrapped)],
        )],
        model,
        max_tokens,
    }
}

/// 把历史摊平成带角色标注的纯文本。
///
/// 角色标注不能省：摘要要能说清"用户要求了什么"与"助手做了什么"，
/// 摊成一坨无主语的文字之后这两件事就分不开了。
fn render_history(ms: &[Message]) -> String {
    ms.iter()
        .map(|m| {
            let role = match m.role {
                agentrs_types::Role::User => "用户",
                agentrs_types::Role::Assistant => "助手",
                agentrs_types::Role::System => "系统",
                agentrs_types::Role::Tool => "工具",
            };
            let body = m
                .content
                .iter()
                .filter_map(|b| match b {
                    agentrs_types::ContentBlock::Text { text } => Some(text.clone()),
                    agentrs_types::ContentBlock::ToolResult { content, .. } => {
                        Some(format!("[工具结果] {content}"))
                    }
                    agentrs_types::ContentBlock::ToolUse { name, input, .. } => {
                        Some(format!("[调用 {name}] {input}"))
                    }
                    // 思考不进摘要材料：它是过程，不是事实。
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{role}：{body}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 校验摘要是否可用。
///
/// **不做修补**：缺项就是缺项，返回 `Err` 让调用方放弃这次压缩。
/// 自动补一句"未决审批：无"是在替模型撒谎——它并不知道有没有。
pub fn validate(out: &SubagentOutput) -> Result<(), SubagentError> {
    let missing = missing_sections(&out.conclusion);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(SubagentError::SchemaViolation {
            missing: missing.into_iter().map(str::to_owned).collect(),
        })
    }
}

/// 校验通过后取出摘要消息。
pub fn accept(out: &SubagentOutput) -> Result<Message, SubagentError> {
    validate(out)?;
    Ok(crate::functional::as_message(out))
}

#[cfg(test)]
mod tests {
    use agentrs_types::{ContentBlock, Role, TokenUsage};

    use super::*;

    fn 产出(text: &str) -> SubagentOutput {
        SubagentOutput {
            conclusion: text.into(),
            usage: TokenUsage::default(),
        }
    }

    fn 完整摘要() -> String {
        SUMMARY_REQUIRED_SECTIONS
            .iter()
            .map(|s| format!("{s}：略"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn 提示词逐条列出必填项() {
        // 列出来是必要条件，不是充分条件——落地靠 validate 检查。
        let p = system_prompt();
        for s in SUMMARY_REQUIRED_SECTIONS {
            assert!(p.contains(s), "提示词漏了要点：{s}");
        }
    }

    #[test]
    fn 提示词明说没有工具() {
        assert!(system_prompt().contains("没有任何工具"));
    }

    #[test]
    fn 提示词禁止编造() {
        // 摘要被编造出来的内容会作为"事实"进入后续全部对话。
        assert!(system_prompt().contains("不要编造"));
    }

    #[test]
    fn 历史被裹成一条_user_消息而不是原样传入() {
        // **这是被真实模型逼出来的。** 原样当对话轮次传入时，
        // 模型时而会把它当成进行中的对话接着说下去。
        let 待压 = vec![
            Message::new(Role::User, vec![ContentBlock::text("改一下超时")]),
            Message::new(Role::Assistant, vec![ContentBlock::text("好的")]),
        ];
        let i = input(待压, "m".into(), Some(512));

        assert_eq!(i.context.len(), 1, "历史应当被裹成一条消息");
        assert_eq!(i.context[0].role, Role::User);
        let t = match &i.context[0].content[0] {
            ContentBlock::Text { text } => text.clone(),
            other => panic!("期望文本块，得到 {other:?}"),
        };
        assert!(t.contains("不是在跟你对话"), "缺少角色消歧：{t}");
        assert!(t.contains("改一下超时"), "历史内容丢了：{t}");
        assert!(t.contains("用户：") && t.contains("助手："), "角色标注丢了：{t}");
    }

    #[test]
    fn 思考块不进摘要材料() {
        // 思考是过程不是事实。把它喂进去会让摘要总结"它当时在想什么"。
        let 待压 = vec![Message::new(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    thinking: "内部推理不该出现".into(),
                    signature: None,
                },
                ContentBlock::text("结论"),
            ],
        )];
        let i = input(待压, "m".into(), None);
        let t = format!("{:?}", i.context[0].content);
        assert!(!t.contains("内部推理不该出现"), "{t}");
        assert!(t.contains("结论"));
    }

    #[test]
    fn 完整摘要通过校验() {
        assert!(validate(&产出(&完整摘要())).is_ok());
        assert!(accept(&产出(&完整摘要())).is_ok());
    }

    #[test]
    fn 缺项被拒绝且逐条点名() {
        let e = validate(&产出("原始意图：修 bug。已做任务：改了两个文件。")).unwrap_err();
        match e {
            SubagentError::SchemaViolation { missing } => {
                assert!(missing.contains(&"未决审批".to_string()), "{missing:?}");
                assert!(missing.contains(&"当前 ChangeSet".to_string()), "{missing:?}");
                assert!(!missing.contains(&"原始意图".to_string()));
            }
            other => panic!("期望 SchemaViolation，得到 {other:?}"),
        }
    }

    #[test]
    fn 缺项不做自动修补() {
        // 自动补一句"未决审批：无"是在替模型撒谎——它并不知道有没有。
        // 而这句谎会让模型以为那个操作已经批过了。
        let 残缺 = 产出("原始意图：修 bug");
        assert!(accept(&残缺).is_err(), "缺项不该被放行");
    }

    #[test]
    fn 空摘要被拒绝() {
        // **压缩的失败必须 fail-closed。** 拿一段空白去遮蔽历史，
        // 模型会突然失忆而且没人知道为什么。
        assert!(accept(&产出("")).is_err());
    }
}
