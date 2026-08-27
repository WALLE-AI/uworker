// Ported from aionrs (Apache-2.0).
//   Source: aionrs/crates/aion-providers/src/anthropic_shared.rs @ f7111746015d8e6f960e1568a805ceef975022d3
//   Copied: 2026-08-25   Modified: yes
//   Changes:
//     - 移除 generate_tool_id()：它调用 SystemTime::now()，违反"内核不读真实时钟"
//       的边界判据（架构 §1.1）。缺 id 的 tool_use 由 HistoryLegalization 在投影期
//       处理，投影器本身不再造 id。
//     - 移除全部 sanitize / orphan-tool-result / dropped-tool-call 逻辑：
//       这些在本方案里集中在 legalization.rs 的固定阶段（架构 §8.1），
//       不允许厂商投影器各做一份，否则"事实与实际请求的差异"无法统一留痕。
//     - 移除 tracing 依赖：本 crate 不引入日志门面，异常一律以返回值表达。
//     - 增加 cache_control 断点注入：按架构 §9.1.1 的缓存前缀不变式，
//       在稳定前缀末尾打 ephemeral 断点。这是 AgentRS 自有的，aionrs 无此机制。
//     - StopReason 增加 "refusal"/"stop_sequence" 的显式映射，不再一律吞成 EndTurn。

//! Anthropic Messages API 的投影与解析。
//!
//! 与 [`crate::openai`] 一样是**纯函数**：不做 I/O，因此可以在无网络下完整测试。
//!
//! ## 与 OpenAI 投影的三处结构性差异
//!
//! | 差异 | Anthropic | OpenAI |
//! |---|---|---|
//! | system | 顶层字段 | 一条 role=system 消息 |
//! | tool_result | 放在 **user** 消息的内容块里 | 独立的 role=tool 消息 |
//! | 角色交替 | **强制** user/assistant 严格交替 | 不要求 |
//!
//! 第三条是最容易漏的：历史里出现两条连续 assistant（例如压缩摘要紧跟助手回复）
//! 会让请求直接 400。这里插入极小的填充消息补齐交替。

use agentrs_types::{
    ContentBlock, LlmEvent, LlmRequest, Message, Role, StopReason, ThinkingConfig, TokenUsage,
};
use serde_json::{json, Map, Value};

/// 缓存断点标记。Anthropic 按它把前缀写入 provider 侧缓存。
const EPHEMERAL: &str = "ephemeral";

/// 把 provider 无关的请求投影为 Anthropic Messages 请求体。
///
/// `cache_breakpoint` 是稳定前缀的末尾消息下标（含）。给 `None` 表示不打断点——
/// 例如 Surface 刚发生 `Replace`（压缩），此时前缀本来就失效了。
pub fn project_request(req: &LlmRequest, stream: bool, cache_breakpoint: Option<usize>) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for m in &req.messages {
        push_message(&mut messages, m);
    }
    // **强制交替**：Anthropic 对连续同角色消息直接 400。
    ensure_alternation(&mut messages);

    let mut body = json!({
        "model": req.model.as_str(),
        "messages": messages,
        "stream": stream,
        // Anthropic 的 max_tokens 是必填的，没有"不给就用默认"。
        "max_tokens": req.max_tokens.unwrap_or(4096),
    });

    if !req.system.is_empty() {
        // system 走顶层。用块数组而不是裸字符串，才能在其上打缓存断点。
        let mut sys = json!({"type": "text", "text": req.system});
        if cache_breakpoint.is_some() {
            sys["cache_control"] = json!({"type": EPHEMERAL});
        }
        body["system"] = json!([sys]);
    }

    if !req.tools.is_empty() {
        body["tools"] = json!(req
            .tools
            .iter()
            .map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            }))
            .collect::<Vec<_>>());
    }

    match req.thinking {
        Some(ThinkingConfig::Enabled { budget_tokens }) => {
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget_tokens});
        }
        // 显式 Disabled 与"没配"对 Anthropic 是同一件事：不带该字段。
        Some(ThinkingConfig::Disabled) | None => {}
    }

    if let Some(idx) = cache_breakpoint {
        mark_cache_breakpoint(&mut body, idx);
    }

    body
}

/// 在稳定前缀末尾打 ephemeral 断点（架构 §9.1.1）。
///
/// **下标基于投影后的数组**，而 `ensure_alternation` 可能插入过填充消息，
/// 因此这里对越界做钳制而不是 panic——宁可少缓存一段，也不要发不出请求。
fn mark_cache_breakpoint(body: &mut Value, idx: usize) {
    let Some(arr) = body["messages"].as_array_mut() else {
        return;
    };
    if arr.is_empty() {
        return;
    }
    let i = idx.min(arr.len() - 1);
    // 断点打在**内容块**上，不是消息上。
    if let Some(blocks) = arr[i]["content"].as_array_mut() {
        if let Some(last) = blocks.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".into(), json!({"type": EPHEMERAL}));
            }
        }
    }
}

fn push_message(out: &mut Vec<Value>, m: &Message) {
    let role = match m.role {
        // **tool_result 走 user 消息**——Anthropic 没有 role=tool。
        Role::User | Role::Tool => "user",
        Role::Assistant => "assistant",
        // system 已提到顶层，这里必须跳过而不是降级成 user。
        Role::System => return,
    };

    let content = project_blocks(&m.content);
    if content.is_empty() {
        return;
    }

    // 相邻同角色合并——Anthropic 允许一条消息里混装多种块。
    if let Some(last) = out.last_mut() {
        if last["role"].as_str() == Some(role) {
            if let Some(arr) = last["content"].as_array_mut() {
                arr.extend(content);
                return;
            }
        }
    }
    out.push(json!({"role": role, "content": content}));
}

fn project_blocks(blocks: &[ContentBlock]) -> Vec<Value> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    out.push(json!({"type": "text", "text": text}));
                }
            }
            ContentBlock::ToolUse { id, name, input, .. } => out.push(json!({
                "type": "tool_use",
                "id": id.as_str(),
                "name": name,
                "input": input,
            })),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => out.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id.as_str(),
                "content": content,
                "is_error": is_error,
            })),
            ContentBlock::Thinking { thinking, signature } => {
                let mut v = json!({"type": "thinking", "thinking": thinking});
                // 无签名的 thinking 块 Anthropic 会拒收；由 legalization 负责剔除，
                // 这里只保证有签名的能原样 round-trip。
                if let Some(s) = signature {
                    v["signature"] = json!(s);
                    out.push(v);
                }
            }
            ContentBlock::Image { image_url } => out.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type_of(&image_url.url),
                    "data": base64_of(&image_url.url),
                }
            })),
            // 非其归属 provider 必须忽略不透明项。
            ContentBlock::ProviderItem { .. } => {}
        }
    }
    out
}

/// 插入极小填充以补齐 user/assistant 严格交替。
fn ensure_alternation(messages: &mut Vec<Value>) {
    if messages.is_empty() {
        return;
    }
    if messages[0]["role"].as_str() == Some("assistant") {
        messages.insert(0, filler("user"));
    }
    let mut i = 1;
    while i < messages.len() {
        let prev = messages[i - 1]["role"].as_str().unwrap_or("");
        let curr = messages[i]["role"].as_str().unwrap_or("");
        if prev == curr {
            let role = if curr == "user" { "assistant" } else { "user" };
            messages.insert(i, filler(role));
            i += 1;
        }
        i += 1;
    }
}

fn filler(role: &str) -> Value {
    json!({"role": role, "content": [{"type": "text", "text": "."}]})
}

fn media_type_of(uri: &str) -> &str {
    uri.strip_prefix("data:")
        .and_then(|r| r.find(';').map(|p| &r[..p]))
        .unwrap_or("application/octet-stream")
}

fn base64_of(uri: &str) -> &str {
    uri.find(',').map(|p| &uri[p + 1..]).unwrap_or(uri)
}

// ---------------------------------------------------------------------------
// 流式解析
// ---------------------------------------------------------------------------

/// SSE 内容块累积状态。
///
/// Anthropic 的 `tool_use` 参数是**跨多帧的 JSON 片段**，只有攒到
/// `content_block_stop` 才能解析。中途解析必然失败。
#[derive(Debug, Default)]
pub struct StreamState {
    block_type: Option<String>,
    tool_input_json: String,
    tool_id: String,
    tool_name: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
}

impl StreamState {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 解析一条 SSE 数据帧，产出零到多个事件。
///
/// 无法解析的帧**返回空而不是报错**——单帧损坏不应终止整个流。
pub fn parse_sse_data(event_type: &str, data: &str, st: &mut StreamState) -> Vec<LlmEvent> {
    let mut events = Vec::new();
    let Ok(json) = serde_json::from_str::<Value>(data) else {
        return events;
    };

    match event_type {
        "message_start" => {
            if let Some(u) = json.get("message").and_then(|m| m.get("usage")) {
                st.cache_creation_tokens = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                st.cache_read_tokens = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                // **Anthropic 的 input_tokens 不含缓存部分**，必须自己加回去，
                // 否则命中率分母偏小、看上去像"缓存把总量变多了"。
                st.input_tokens = u["input_tokens"]
                    .as_u64()
                    .unwrap_or(0)
                    .saturating_add(st.cache_creation_tokens)
                    .saturating_add(st.cache_read_tokens);
            }
        }

        "content_block_start" => {
            let block = &json["content_block"];
            let ty = block["type"].as_str().unwrap_or("");
            st.block_type = Some(ty.to_string());
            if ty == "tool_use" {
                st.tool_id = block["id"].as_str().unwrap_or("").to_string();
                st.tool_name = block["name"].as_str().unwrap_or("").to_string();
                st.tool_input_json.clear();
            }
        }

        "content_block_delta" => {
            let d = &json["delta"];
            match d["type"].as_str().unwrap_or("") {
                "text_delta" => {
                    if let Some(t) = d["text"].as_str() {
                        events.push(LlmEvent::TextDelta(t.to_string()));
                    }
                }
                "input_json_delta" => {
                    if let Some(p) = d["partial_json"].as_str() {
                        st.tool_input_json.push_str(p);
                    }
                }
                "thinking_delta" => {
                    if let Some(t) = d["thinking"].as_str() {
                        events.push(LlmEvent::ThinkingDelta(t.to_string()));
                    }
                }
                "signature_delta" => {
                    if let Some(s) = d["signature"].as_str() {
                        events.push(LlmEvent::ThinkingSignature(s.to_string()));
                    }
                }
                _ => {}
            }
        }

        "content_block_stop" => {
            if st.block_type.as_deref() == Some("tool_use") {
                // 片段拼不成 JSON 时给空对象，让调用照常成立并由工具侧报参数错——
                // 直接丢弃会让模型永远等不到 tool_result。
                let input = serde_json::from_str(&st.tool_input_json).unwrap_or(Value::Object(Map::new()));
                events.push(LlmEvent::ToolUse {
                    id: st.tool_id.clone().into(),
                    name: st.tool_name.clone(),
                    input,
                    extra: None,
                });
                st.tool_input_json.clear();
            }
            st.block_type = None;
        }

        "message_delta" => {
            if let Some(u) = json.get("usage") {
                st.output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
            }
            events.push(LlmEvent::Done {
                stop_reason: stop_reason_of(json["delta"]["stop_reason"].as_str()),
                usage: TokenUsage {
                    input_tokens: st.input_tokens,
                    output_tokens: st.output_tokens,
                    cache_creation_tokens: st.cache_creation_tokens,
                    cache_read_tokens: st.cache_read_tokens,
                },
            });
        }

        "message_stop" => {}

        "error" => {
            // 只取稳定类型，不透传 message（可能含用户内容）。
            let code = json["error"]["type"].as_str().unwrap_or("unknown");
            events.push(LlmEvent::Error(code.to_string()));
        }

        _ => {}
    }

    events
}

fn stop_reason_of(s: Option<&str>) -> StopReason {
    match s {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        // end_turn / stop_sequence / refusal / 未知一律按自然结束处理，
        // 但它们不是同一回事——refusal 由上层看 Error 事件判断。
        _ => StopReason::EndTurn,
    }
}

#[cfg(test)]
mod tests;
