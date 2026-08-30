//! OpenAI 兼容端点的投影与解析。
//!
//! **厂商差异全部集中在这里，绝不进主循环分支**（ADR 3）。新增一个
//! OpenAI 兼容端点应当只需要加 compat/profile，不改本文件之外的代码。
//!
//! 本模块是纯函数：请求投影与帧解析都不做 I/O，因此可以在没有网络的情况下
//! 被完整测试；传输在 [`crate::transport`]。

use agentrs_types::{LlmEvent, LlmRequest, Message, Role, StopReason, TokenUsage};
use serde_json::{json, Value};

use agentrs_types::ContentBlock;

/// 把 provider 无关的请求投影为 OpenAI Chat Completions 请求体。
pub fn project_request(req: &LlmRequest, stream: bool) -> Value {
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({"role": "system", "content": req.system}));
    }
    for m in &req.messages {
        messages.extend(project_message(m));
    }

    let mut body = json!({
        "model": req.model.as_str(),
        "messages": messages,
        "stream": stream,
    });
    if !req.tools.is_empty() {
        body["tools"] = json!(req
            .tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            }))
            .collect::<Vec<_>>());
    }
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = json!(max);
    }
    if let Some(effort) = &req.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    if stream {
        // 不请求用量就拿不到 cache_read_tokens，缓存命中率无从归因。
        body["stream_options"] = json!({"include_usage": true});
    }
    body
}

fn project_message(m: &Message) -> Vec<Value> {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    };

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut out = Vec::new();

    for block in &m.content {
        match block {
            ContentBlock::Text { text: t } => text.push_str(t),
            ContentBlock::ToolUse { id, name, input, .. } => tool_calls.push(json!({
                "id": id.as_str(),
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()},
            })),
            ContentBlock::ToolResult {
                tool_use_id, content, ..
            } => out.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id.as_str(),
                "content": content,
            })),
            // thinking 块不回传：签名有生命周期，跨请求重放会被拒。
            // 失效处理归 HistoryLegalization，此处只做投影。
            ContentBlock::Thinking { .. } | ContentBlock::ProviderItem { .. } => {}
            ContentBlock::Image { image_url } => out.push(json!({
                "role": role,
                "content": [{"type": "image_url", "image_url": {"url": image_url.url}}],
            })),
        }
    }

    if !text.is_empty() || !tool_calls.is_empty() {
        let mut msg = json!({"role": role, "content": text});
        if !tool_calls.is_empty() {
            msg["tool_calls"] = json!(tool_calls);
        }
        out.insert(0, msg);
    }
    out
}

/// 解析一帧流式响应，产出零个或多个 [`LlmEvent`]。
///
/// 返回空 `Vec` 表示该帧无语义内容（如仅含 role 的首帧）——**不是错误**。
pub fn parse_chunk(payload: &str) -> Result<Vec<LlmEvent>, String> {
    let v: Value = serde_json::from_str(payload).map_err(|e| format!("malformed chunk: {e}"))?;

    if let Some(err) = v.get("error") {
        // 只取稳定字段，不透传响应体（可能含密钥或用户内容）。
        let code = err
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("provider_error");
        return Ok(vec![LlmEvent::Error(code.to_string())]);
    }

    let mut out = Vec::new();

    if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
        out.push(LlmEvent::Usage(parse_usage(usage)));
    }

    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
        return Ok(out);
    };

    if let Some(delta) = choice.get("delta") {
        if let Some(t) = delta.get("content").and_then(Value::as_str) {
            if !t.is_empty() {
                out.push(LlmEvent::TextDelta(t.to_string()));
            }
        }
        if let Some(t) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !t.is_empty() {
                out.push(LlmEvent::ThinkingDelta(t.to_string()));
            }
        }
    }

    // 工具调用以增量下发：首帧带 id/name，后续帧只带 arguments 片段。
    // 累积由 `accumulate_tool_calls` 在流层面完成；此处只产出原始片段。
    if let Some(calls) = choice
        .get("delta")
        .and_then(|d| d.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for c in calls {
            out.push(LlmEvent::ToolCallDelta {
                index: c.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                id: c
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                name: c
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    // SiliconFlow/DeepSeek may repeat `name: ""` on later
                    // argument frames. Empty strings are absence, not updates.
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                arguments: c
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        let stop = match reason {
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            _ => StopReason::EndTurn,
        };
        let usage = v
            .get("usage")
            .filter(|u| !u.is_null())
            .map(parse_usage)
            .unwrap_or_default();
        out.push(LlmEvent::Done {
            stop_reason: stop,
            usage,
        });
    }

    Ok(out)
}

/// 把一次流的全部事件规整为最终形态。
///
/// **由真实端点冒烟发现的必要步骤**：vLLM 等 OpenAI 兼容端点把 `finish_reason`
/// 与 `usage` 分在**两帧**下发——先是带 `finish_reason` 但 `usage: null` 的帧，
/// 再是 `choices: []` 只带 `usage` 的帧，最后 `[DONE]`。
///
/// 因此逐帧解析必然得到一个用量为零的 `Done`。本函数把尾随的 `Usage` 合并回
/// `Done`，并移除已被合并的独立 `Usage` 事件，使调用方只看到一个权威终止事件。
///
/// 若端点把用量放在同一帧（部分实现如此），本函数是幂等的。
pub fn finalize(mut events: Vec<LlmEvent>) -> Vec<LlmEvent> {
    events = accumulate_tool_calls(events);

    // 取最后一个非零用量作为权威值。
    let trailing = events.iter().rev().find_map(|e| match e {
        LlmEvent::Usage(u) if u.input_tokens > 0 || u.output_tokens > 0 => Some(*u),
        _ => None,
    });

    let Some(usage) = trailing else {
        return events;
    };

    let mut patched = false;
    for e in events.iter_mut() {
        if let LlmEvent::Done { usage: slot, .. } = e {
            if slot.input_tokens == 0 && slot.output_tokens == 0 {
                *slot = usage;
            }
            patched = true;
        }
    }

    if patched {
        events.retain(|e| !matches!(e, LlmEvent::Usage(_)));
    }
    events
}

/// 把 `ToolCallDelta` 累积成完整的 `ToolUse`。
///
/// OpenAI 兼容端点把一次工具调用切成多帧：首帧带 `id`/`name`，
/// 后续帧只带 `arguments` 的片段。**不累积就拿不到完整参数**。
fn accumulate_tool_calls(events: Vec<LlmEvent>) -> Vec<LlmEvent> {
    use std::collections::BTreeMap;

    let mut acc: BTreeMap<usize, (Option<String>, Option<String>, String)> = BTreeMap::new();
    let mut out = Vec::with_capacity(events.len());

    for e in events {
        match e {
            LlmEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments,
            } => {
                let slot = acc.entry(index).or_insert((None, None, String::new()));
                if id.is_some() {
                    slot.0 = id;
                }
                if name.is_some() {
                    slot.1 = name;
                }
                slot.2.push_str(&arguments);
            }
            other => out.push(other),
        }
    }

    // 完整的调用插在 Done 之前，保持"先工具后终止"的顺序。
    let done_at = out
        .iter()
        .position(|e| matches!(e, LlmEvent::Done { .. }))
        .unwrap_or(out.len());
    let mut calls = Vec::new();
    for (i, (id, name, args)) in acc {
        let Some(name) = name else { continue };
        calls.push(LlmEvent::ToolUse {
            id: id.unwrap_or_else(|| format!("call_{i}")).as_str().into(),
            name,
            // 参数解析失败时给空对象——畸形参数在固定管线的 schema 阶段被拦下，
            // 不在这里报错，否则整条流会因一个坏调用而作废。
            input: serde_json::from_str(&args).unwrap_or(serde_json::json!({})),
            extra: None,
        });
    }
    for (k, c) in calls.into_iter().enumerate() {
        out.insert(done_at + k, c);
    }
    out
}

fn parse_usage(u: &Value) -> TokenUsage {
    let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let details = u.get("prompt_tokens_details");
    let cached = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        // DeepSeek 系用顶层 prompt_cache_hit_tokens。
        .or_else(|| u.get("prompt_cache_hit_tokens").and_then(Value::as_u64))
        .unwrap_or(0);

    TokenUsage {
        input_tokens: get("prompt_tokens"),
        output_tokens: get("completion_tokens"),
        cache_read_tokens: cached,
        cache_creation_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use agentrs_types::ContentBlock;

    use super::*;

    fn 请求(msgs: Vec<Message>) -> LlmRequest {
        LlmRequest {
            request_id: "r".into(),
            model: "m".into(),
            system: "sys".into(),
            messages: msgs,
            tools: vec![],
            max_tokens: Some(16),
            thinking: None,
            reasoning_effort: None,
            cache_prefix_digest: None,
        }
    }

    #[test]
    fn 系统段投影为首条_system_消息() {
        let b = project_request(&请求(vec![]), false);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][0]["content"], "sys");
    }

    #[test]
    fn 流式请求必须索取用量否则无法归因缓存() {
        let b = project_request(&请求(vec![]), true);
        assert_eq!(b["stream_options"]["include_usage"], json!(true));
    }

    #[test]
    fn 工具结果投影为独立的_tool_消息() {
        let m = Message::new(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "tc1".into(),
                content: "ok".into(),
                is_error: false,
            }],
        );
        let b = project_request(&请求(vec![m]), false);
        let last = b["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(last["role"], "tool");
        assert_eq!(last["tool_call_id"], "tc1");
    }

    #[test]
    fn thinking_块不回传() {
        // 签名有生命周期，跨请求重放会被 provider 拒绝。
        let m = Message::new(
            Role::Assistant,
            vec![ContentBlock::Thinking {
                thinking: "secret".into(),
                signature: Some("sig".into()),
            }],
        );
        let b = project_request(&请求(vec![m]), false);
        assert!(!b.to_string().contains("secret"));
    }

    #[test]
    fn 首帧仅含_role_时不产出事件() {
        let f = parse_chunk(r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#).unwrap();
        assert!(f.is_empty(), "空内容不是错误，只是无语义");
    }

    #[test]
    fn 文本增量被解析() {
        let f = parse_chunk(r#"{"choices":[{"delta":{"content":"一"}}]}"#).unwrap();
        assert_eq!(f, vec![LlmEvent::TextDelta("一".into())]);
    }

    #[test]
    fn 结束帧携带停止原因与用量() {
        let f = parse_chunk(
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":17,"completion_tokens":2,
                         "prompt_tokens_details":{"cached_tokens":8}}}"#,
        )
        .unwrap();
        // usage 帧 + Done 帧
        assert!(matches!(f[0], LlmEvent::Usage(_)));
        match &f[1] {
            LlmEvent::Done { stop_reason, usage } => {
                assert_eq!(*stop_reason, StopReason::EndTurn);
                assert_eq!(usage.input_tokens, 17);
                assert_eq!(usage.cache_read_tokens, 8);
            }
            other => panic!("期望 Done，得到 {other:?}"),
        }
    }

    #[test]
    fn 缓存命中支持两种字段布局() {
        let a = parse_chunk(r#"{"usage":{"prompt_tokens":10,"prompt_tokens_details":{"cached_tokens":4}}}"#)
            .unwrap();
        let b = parse_chunk(r#"{"usage":{"prompt_tokens":10,"prompt_cache_hit_tokens":4}}"#).unwrap();
        for f in [a, b] {
            match &f[0] {
                LlmEvent::Usage(u) => assert_eq!(u.cache_read_tokens, 4),
                other => panic!("期望 Usage，得到 {other:?}"),
            }
        }
    }

    #[test]
    fn 错误帧不透传响应正文() {
        let f = parse_chunk(r#"{"error":{"type":"rate_limit","message":"key sk-xxx quota"}}"#).unwrap();
        assert_eq!(f, vec![LlmEvent::Error("rate_limit".into())]);
        assert!(!format!("{f:?}").contains("sk-xxx"), "密钥不得进入事件");
    }

    #[test]
    fn vllm_的分帧用量能被合并回_done() {
        // 这段帧序列取自真实 vLLM（Qwen3.8-27B）响应，由 W2 冒烟测试发现。
        let mut ev = Vec::new();
        ev.extend(parse_chunk(r#"{"choices":[{"delta":{"content":"嗨"}}]}"#).unwrap());
        ev.extend(parse_chunk(r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":null}"#).unwrap());
        ev.extend(
            parse_chunk(r#"{"choices":[],"usage":{"prompt_tokens":13,"completion_tokens":8}}"#).unwrap(),
        );

        let out = finalize(ev);
        let done = out.iter().find_map(|e| match e {
            LlmEvent::Done { usage, .. } => Some(*usage),
            _ => None,
        });
        assert_eq!(done.map(|u| u.input_tokens), Some(13), "用量必须被合并回 Done");
        assert!(
            !out.iter().any(|e| matches!(e, LlmEvent::Usage(_))),
            "合并后不再保留独立 Usage 事件，调用方只看到一个权威终止事件"
        );
    }

    #[test]
    fn 同帧用量时_finalize_是幂等的() {
        let ev = parse_chunk(
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":1}}"#,
        )
        .unwrap();
        let out = finalize(ev);
        let done = out.iter().find_map(|e| match e {
            LlmEvent::Done { usage, .. } => Some(*usage),
            _ => None,
        });
        assert_eq!(done.map(|u| u.input_tokens), Some(5));
    }

    #[test]
    fn 畸形帧返回错误而非_panic() {
        assert!(parse_chunk("not json").is_err());
    }

    #[test]
    fn 空工具名增量不覆盖首帧名称() {
        let mut events = Vec::new();
        events.extend(
            parse_chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"Write","arguments":"{\"path\":"}}]}}]}"#,
            )
            .unwrap(),
        );
        events.extend(
            parse_chunk(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"","function":{"name":"","arguments":"\"x.txt\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            )
            .unwrap(),
        );

        let output = finalize(events);
        let tool = output.iter().find_map(|event| match event {
            LlmEvent::ToolUse { id, name, input, .. } => Some((id, name, input)),
            _ => None,
        });
        let (id, name, input) = tool.expect("complete tool call");
        assert_eq!(id.as_str(), "call-1");
        assert_eq!(name, "Write");
        assert_eq!(input, &serde_json::json!({"path":"x.txt"}));
    }
}
