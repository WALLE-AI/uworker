// Ported from aionrs (Apache-2.0).
//   Source: aionrs/crates/aion-providers/src/{openai_responses.rs,openai_responses_projector.rs} @ f7111746015d8e6f960e1568a805ceef975022d3
//   Copied: 2026-08-25   Modified: yes
//   Changes:
//     - 移除 generate_call_id()：与 anthropic.rs 同因——它依赖真实时钟造 id，
//       违反边界判据。缺 id 的调用由 legalization 在投影期处理。
//     - 移除 orphan tool_use / tool_result 的就地清理：集中在 legalization.rs。
//     - 移除 tracing 与 compat.emit_tools()/supports_effort() 的静默降级分支：
//       内核不静默丢工具。端点不支持就该由 compat 在装配前决定，而不是投影时吞掉。
//     - reasoning 的 encrypted_content 由 ProviderItem 原样 round-trip，
//       归属串固定为 PROVIDER_ITEM_OWNER（与原实现一致）。

//! OpenAI **Responses API** 的请求投影。
//!
//! 与 [`crate::openai`]（Chat Completions）是**两套线格式**，不是同一个的变体：
//!
//! | | Chat Completions | Responses |
//! |---|---|---|
//! | 系统提示 | `messages[0].role=system` | 顶层 `instructions` |
//! | 历史 | `messages` | `input` |
//! | 输出上限 | `max_tokens` | `max_output_tokens` |
//! | 工具 | `{type, function:{...}}` **嵌套** | `{type:"function", ...}` **扁平** |
//! | 工具调用 | `tool_calls` on assistant | 独立的 `function_call` 项 |
//! | 工具结果 | `role=tool` 消息 | 独立的 `function_call_output` 项 |
//!
//! 工具形状那一行是最容易出错的：把 Chat Completions 的嵌套结构直接发给
//! Responses，端点会当成"没有工具"而不是报错——模型于是开始用文字描述工具调用。

use agentrs_types::{ContentBlock, LlmRequest, Message, Role};
use serde_json::{json, Value};

/// reasoning 加密内容的归属 provider 串。**非其归属者必须忽略。**
pub const PROVIDER_ITEM_OWNER: &str = "openai_responses";

/// 把 provider 无关的请求投影为 Responses API 请求体。
pub fn project_request(req: &LlmRequest, stream: bool) -> Value {
    let mut body = json!({
        "model": req.model.as_str(),
        // 系统提示走顶层 instructions，不是一条 input 项。
        "instructions": req.system,
        "input": project_input(&req.messages),
        "stream": stream,
        // 不让端点存储会话——历史的唯一事实源是本地事件流。
        "store": false,
        // 不要它就拿不回 reasoning，跨 Step 的思考链就断了。
        "include": ["reasoning.encrypted_content"],
    });

    if let Some(max) = req.max_tokens {
        body["max_output_tokens"] = json!(max);
    }

    if !req.tools.is_empty() {
        // **扁平**结构。用 Chat Completions 的嵌套结构会被当成"没有工具"，
        // 端点不报错，模型转而用文字描述调用。
        body["tools"] = json!(req
            .tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
                // 与 Chat Completions 的默认契约保持一致：非严格模式。
                "strict": false,
            }))
            .collect::<Vec<_>>());
    }

    if let Some(effort) = &req.reasoning_effort {
        body["reasoning"] = json!({"effort": effort});
    }

    body
}

fn project_input(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        let role = match m.role {
            Role::User | Role::Tool => "user",
            Role::Assistant => "assistant",
            // system 已提到 instructions，这里跳过而不是降级。
            Role::System => continue,
        };

        // 一条内部消息可能拆成多个 input 项——function_call 与
        // function_call_output 在 Responses 里是**同级项**，不是消息内的块。
        let mut text_blocks: Vec<Value> = Vec::new();

        for b in &m.content {
            match b {
                ContentBlock::Text { text } => {
                    if !text.is_empty() {
                        text_blocks.push(json!({
                            "type": if role == "assistant" { "output_text" } else { "input_text" },
                            "text": text,
                        }));
                    }
                }
                ContentBlock::Image { image_url } => text_blocks.push(json!({
                    "type": "input_image",
                    "image_url": image_url.url,
                })),
                ContentBlock::ToolUse { id, name, input, .. } => {
                    flush(&mut out, role, &mut text_blocks);
                    out.push(json!({
                        "type": "function_call",
                        "call_id": id.as_str(),
                        "name": name,
                        // Responses 要求 arguments 是**字符串化的 JSON**，不是对象。
                        "arguments": input.to_string(),
                    }));
                }
                ContentBlock::ToolResult {
                    tool_use_id, content, ..
                } => {
                    flush(&mut out, role, &mut text_blocks);
                    out.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_use_id.as_str(),
                        "output": content,
                    }));
                }
                ContentBlock::ProviderItem { provider, item } => {
                    // 只重放归属本 provider 的项。
                    if provider == PROVIDER_ITEM_OWNER {
                        flush(&mut out, role, &mut text_blocks);
                        out.push(item.clone());
                    }
                }
                // thinking 在 Responses 里由 ProviderItem 承载，明文块不重放。
                ContentBlock::Thinking { .. } => {}
            }
        }

        flush(&mut out, role, &mut text_blocks);
    }
    out
}

fn flush(out: &mut Vec<Value>, role: &str, blocks: &mut Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    out.push(json!({
        "type": "message",
        "role": role,
        "content": std::mem::take(blocks),
    }));
}

#[cfg(test)]
mod tests {
    use agentrs_types::{ImageUrl, ToolDef};

    use super::*;

    fn 请求(messages: Vec<Message>) -> LlmRequest {
        LlmRequest {
            request_id: "q1".into(),
            model: "gpt-5".into(),
            system: "你是助手".into(),
            messages,
            tools: vec![],
            max_tokens: Some(2048),
            thinking: None,
            reasoning_effort: None,
            cache_prefix_digest: None,
        }
    }

    fn 用户(t: &str) -> Message {
        Message::new(Role::User, vec![ContentBlock::text(t)])
    }

    #[test]
    fn 系统提示走顶层_instructions() {
        let b = project_request(&请求(vec![用户("hi")]), true);
        assert_eq!(b["instructions"], "你是助手");
        assert!(b["messages"].is_null(), "Responses 没有 messages 字段");
        assert_eq!(b["input"][0]["type"], "message");
    }

    #[test]
    fn 输出上限用_max_output_tokens() {
        let b = project_request(&请求(vec![用户("hi")]), true);
        assert_eq!(b["max_output_tokens"], 2048);
        assert!(
            b["max_tokens"].is_null(),
            "沿用 Chat Completions 的字段名会被忽略"
        );
    }

    #[test]
    fn 工具是扁平结构而不是嵌套() {
        // **端点不会因嵌套结构报错**，只会当成"没有工具"，
        // 模型转而用文字描述调用——这类故障极难从日志看出来。
        let mut r = 请求(vec![用户("hi")]);
        r.tools = vec![ToolDef::read_only("Read", "读文件", json!({"type": "object"}))];
        let b = project_request(&r, true);
        let t = &b["tools"][0];
        assert_eq!(t["type"], "function");
        assert_eq!(t["name"], "Read", "name 必须在顶层，不能包在 function 里");
        assert!(t["function"].is_null());
        assert_eq!(t["strict"], false);
    }

    #[test]
    fn 工具调用是同级项且参数为字符串() {
        let m = Message::new(
            Role::Assistant,
            vec![
                ContentBlock::text("我查一下"),
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "Read".into(),
                    input: json!({"path": "a.txt"}),
                    extra: None,
                },
            ],
        );
        let b = project_request(&请求(vec![用户("q"), m]), true);
        let input = b["input"].as_array().unwrap();
        // 文本先成一项，function_call 另成一项。
        assert_eq!(input[1]["type"], "message");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "c1");
        // arguments 必须是字符串化的 JSON。
        assert_eq!(input[2]["arguments"], r#"{"path":"a.txt"}"#);
    }

    #[test]
    fn 工具结果是_function_call_output_项() {
        let m = Message::new(
            Role::Tool,
            vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "文件内容".into(),
                is_error: false,
            }],
        );
        let b = project_request(&请求(vec![m]), true);
        assert_eq!(b["input"][0]["type"], "function_call_output");
        assert_eq!(b["input"][0]["call_id"], "c1");
        assert_eq!(b["input"][0]["output"], "文件内容");
    }

    #[test]
    fn 助手文本用_output_text_用户文本用_input_text() {
        // 用错类型会被端点拒收。
        let b = project_request(
            &请求(vec![
                用户("问"),
                Message::new(Role::Assistant, vec![ContentBlock::text("答")]),
            ]),
            true,
        );
        assert_eq!(b["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(b["input"][1]["content"][0]["type"], "output_text");
    }

    #[test]
    fn 归属本_provider_的不透明项原样重放() {
        // reasoning 的 encrypted_content 靠它跨 Step 传下去。
        let item = json!({"type": "reasoning", "encrypted_content": "abc"});
        let m = Message::new(
            Role::Assistant,
            vec![ContentBlock::ProviderItem {
                provider: PROVIDER_ITEM_OWNER.into(),
                item: item.clone(),
            }],
        );
        let b = project_request(&请求(vec![用户("q"), m]), true);
        assert_eq!(b["input"][1], item);
    }

    #[test]
    fn 他家的不透明项被忽略() {
        let m = Message::new(
            Role::Assistant,
            vec![ContentBlock::ProviderItem {
                provider: "anthropic".into(),
                item: json!({"x": 1}),
            }],
        );
        let b = project_request(&请求(vec![用户("q"), m]), true);
        assert_eq!(b["input"].as_array().unwrap().len(), 1, "只剩用户那一项");
    }

    #[test]
    fn 明文思考块不重放() {
        // Responses 的思考只能靠 encrypted_content 走 ProviderItem，
        // 把明文当 output_text 发回去会污染输出。
        let m = Message::new(
            Role::Assistant,
            vec![ContentBlock::Thinking {
                thinking: "内部推理".into(),
                signature: None,
            }],
        );
        let b = project_request(&请求(vec![用户("q"), m]), true);
        let s = serde_json::to_string(&b).unwrap();
        assert!(!s.contains("内部推理"));
    }

    #[test]
    fn 不让端点存储会话() {
        // 历史的唯一事实源是本地事件流；端点侧再存一份就有了第二个事实源。
        let b = project_request(&请求(vec![用户("hi")]), true);
        assert_eq!(b["store"], false);
    }

    #[test]
    fn 请求带回_reasoning_加密内容() {
        let b = project_request(&请求(vec![用户("hi")]), true);
        assert_eq!(b["include"][0], "reasoning.encrypted_content");
    }

    #[test]
    fn 图像用_input_image() {
        let m = Message::new(
            Role::User,
            vec![ContentBlock::Image {
                image_url: ImageUrl {
                    url: "data:image/png;base64,QUJD".into(),
                },
            }],
        );
        let b = project_request(&请求(vec![m]), true);
        assert_eq!(b["input"][0]["content"][0]["type"], "input_image");
    }

    #[test]
    fn system_角色消息被跳过() {
        let m = Message::new(Role::System, vec![ContentBlock::text("影子指令")]);
        let b = project_request(&请求(vec![m, 用户("hi")]), true);
        assert_eq!(b["input"].as_array().unwrap().len(), 1);
    }
}
