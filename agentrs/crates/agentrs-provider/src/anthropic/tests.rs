//! Anthropic 投影与解析的契约测试。
//!
//! 重点覆盖三处**只在真实端点上才会暴露**的失败：强制交替、
//! tool_result 归属 user 消息、input_tokens 不含缓存部分。

use agentrs_types::{ImageUrl, ToolDef};

use super::*;

fn 请求(messages: Vec<Message>) -> LlmRequest {
    LlmRequest {
        request_id: "q1".into(),
        model: "claude".into(),
        system: "你是助手".into(),
        messages,
        tools: vec![],
        max_tokens: Some(1024),
        thinking: None,
        reasoning_effort: None,
        cache_prefix_digest: None,
    }
}

fn 用户(t: &str) -> Message {
    Message::new(Role::User, vec![ContentBlock::text(t)])
}

fn 助手(t: &str) -> Message {
    Message::new(Role::Assistant, vec![ContentBlock::text(t)])
}

// ---- 请求投影 ----

#[test]
fn system_提到顶层而不是消息() {
    let b = project_request(&请求(vec![用户("hi")]), true, None);
    assert_eq!(b["system"][0]["text"], "你是助手");
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn system_角色的消息被跳过而不是降级为_user() {
    // 降级成 user 会让 system 指令变成可被后续内容覆盖的普通输入。
    let m = Message::new(Role::System, vec![ContentBlock::text("影子指令")]);
    let b = project_request(&请求(vec![m, 用户("hi")]), true, None);
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["content"][0]["text"], "hi");
}

#[test]
fn 连续同角色被合并() {
    let b = project_request(&请求(vec![用户("a"), 用户("b")]), true, None);
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1, "相邻同角色应合并成一条");
    assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
}

#[test]
fn 首条为助手时前置填充() {
    // 分叉/压缩后历史可能以 assistant 开头——Anthropic 对此直接 400。
    let b = project_request(&请求(vec![助手("续上文"), 用户("好")]), true, None);
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[2]["role"], "user");
}

#[test]
fn 交替被打断时插入填充() {
    // 构造合并躲不掉的情形：user、assistant、（ProviderItem 被丢弃后为空的 user）、assistant
    let mut msgs = vec![
        json!({"role":"user","content":[]}),
        json!({"role":"assistant","content":[]}),
    ];
    msgs.push(json!({"role":"assistant","content":[]}));
    ensure_alternation(&mut msgs);
    let roles: Vec<_> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
}

#[test]
fn 工具结果作为_user_消息的内容块() {
    // Anthropic 没有 role=tool。放错位置会 400。
    let tool = Message::new(
        Role::Tool,
        vec![ContentBlock::ToolResult {
            tool_use_id: "c1".into(),
            content: "42".into(),
            is_error: false,
        }],
    );
    let b = project_request(&请求(vec![用户("算"), 助手("好"), tool]), true, None);
    let msgs = b["messages"].as_array().unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last["role"], "user");
    assert_eq!(last["content"][0]["type"], "tool_result");
    assert_eq!(last["content"][0]["tool_use_id"], "c1");
}

#[test]
fn 工具用_input_schema_而不是_parameters() {
    let mut r = 请求(vec![用户("hi")]);
    r.tools = vec![ToolDef::read_only("Read", "读文件", json!({"type": "object"}))];
    let b = project_request(&r, true, None);
    assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
    assert!(b["tools"][0]["parameters"].is_null(), "不能沿用 OpenAI 的字段名");
}

#[test]
fn max_tokens_必填时有兜底() {
    // Anthropic 缺 max_tokens 直接 400，没有"用默认值"这回事。
    let mut r = 请求(vec![用户("hi")]);
    r.max_tokens = None;
    let b = project_request(&r, true, None);
    assert!(b["max_tokens"].as_u64().unwrap() > 0);
}

#[test]
fn 无签名的思考块被丢弃() {
    // Anthropic 拒收无签名 thinking。带出去就是一次必然失败的请求。
    let m = Message::new(
        Role::Assistant,
        vec![
            ContentBlock::Thinking {
                thinking: "推理".into(),
                signature: None,
            },
            ContentBlock::text("结论"),
        ],
    );
    let b = project_request(&请求(vec![用户("q"), m]), true, None);
    let blocks = b["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
}

#[test]
fn 有签名的思考块原样带回() {
    let m = Message::new(
        Role::Assistant,
        vec![ContentBlock::Thinking {
            thinking: "推理".into(),
            signature: Some("sig".into()),
        }],
    );
    let b = project_request(&请求(vec![用户("q"), m]), true, None);
    assert_eq!(b["messages"][1]["content"][0]["signature"], "sig");
}

#[test]
fn 图像拆成_media_type_与_base64() {
    let m = Message::new(
        Role::User,
        vec![ContentBlock::Image {
            image_url: ImageUrl {
                url: "data:image/png;base64,QUJD".into(),
            },
        }],
    );
    let b = project_request(&请求(vec![m]), true, None);
    let src = &b["messages"][0]["content"][0]["source"];
    assert_eq!(src["media_type"], "image/png");
    assert_eq!(src["data"], "QUJD");
}

#[test]
fn 非归属_provider_的不透明项被忽略() {
    let m = Message::new(
        Role::Assistant,
        vec![
            ContentBlock::ProviderItem {
                provider: "openai".into(),
                item: json!({"x": 1}),
            },
            ContentBlock::text("正文"),
        ],
    );
    let b = project_request(&请求(vec![用户("q"), m]), true, None);
    let blocks = b["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["text"], "正文");
}

// ---- 缓存断点 ----

#[test]
fn 断点打在稳定前缀末尾的内容块上() {
    let b = project_request(&请求(vec![用户("a"), 助手("b"), 用户("c")]), true, Some(1));
    // system 也要打，否则最长的那段稳定前缀白丢。
    assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["content"][0]["cache_control"]["type"], "ephemeral");
    assert!(msgs[2]["content"][0]["cache_control"].is_null());
}

#[test]
fn 不给断点时完全不注入_cache_control() {
    // Surface 刚发生 Replace（压缩）时前缀本就失效，打断点是白费一次写入。
    let b = project_request(&请求(vec![用户("a")]), true, None);
    assert!(b["system"][0]["cache_control"].is_null());
    assert!(b["messages"][0]["content"][0]["cache_control"].is_null());
}

#[test]
fn 越界断点被钳制而不是恐慌() {
    // 填充消息会改变下标。宁可少缓存一段，也不能发不出请求。
    let b = project_request(&请求(vec![用户("a")]), true, Some(99));
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(
        msgs.last().unwrap()["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
}

// ---- 流式解析 ----

fn 跑(frames: &[(&str, &str)]) -> Vec<LlmEvent> {
    let mut st = StreamState::new();
    frames
        .iter()
        .flat_map(|(t, d)| parse_sse_data(t, d, &mut st))
        .collect()
}

#[test]
fn 文本增量按序产出() {
    let ev = 跑(&[
        (
            "content_block_delta",
            r#"{"delta":{"type":"text_delta","text":"你"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"type":"text_delta","text":"好"}}"#,
        ),
    ]);
    assert_eq!(
        ev,
        [LlmEvent::TextDelta("你".into()), LlmEvent::TextDelta("好".into())]
    );
}

#[test]
fn 工具参数跨帧拼接后才产出() {
    let ev = 跑(&[
        (
            "content_block_start",
            r#"{"content_block":{"type":"tool_use","id":"c1","name":"Read"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}"#,
        ),
        ("content_block_stop", "{}"),
    ]);
    // 中途一个事件都不该产出——半截 JSON 解析不了。
    assert_eq!(ev.len(), 1);
    assert_eq!(
        ev[0],
        LlmEvent::ToolUse {
            id: "c1".into(),
            name: "Read".into(),
            input: json!({"path": "a.txt"}),
            extra: None,
        }
    );
}

#[test]
fn 参数片段损坏时仍产出调用() {
    // 丢弃调用会让模型永远等不到 tool_result，整个 Turn 卡死。
    let ev = 跑(&[
        (
            "content_block_start",
            r#"{"content_block":{"type":"tool_use","id":"c1","name":"Read"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
        ),
        ("content_block_stop", "{}"),
    ]);
    assert_eq!(ev.len(), 1);
    match &ev[0] {
        LlmEvent::ToolUse { input, .. } => assert_eq!(input, &json!({})),
        other => panic!("期望 ToolUse，得到 {other:?}"),
    }
}

#[test]
fn 用量把缓存部分加回输入总数() {
    // Anthropic 的 input_tokens **不含**缓存读取与创建。不加回去，
    // 命中率分母偏小，缓存看上去像"让总量变多了"。
    let ev = 跑(&[
        (
            "message_start",
            r#"{"message":{"usage":{"input_tokens":10,"cache_read_input_tokens":900,"cache_creation_input_tokens":90}}}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
        ),
    ]);
    match ev.last().unwrap() {
        LlmEvent::Done { usage, .. } => {
            assert_eq!(usage.input_tokens, 1000);
            assert_eq!(usage.cache_read_tokens, 900);
            assert_eq!(usage.output_tokens, 5);
            assert_eq!(usage.cache_hit_rate(), Some(0.9));
        }
        other => panic!("期望 Done，得到 {other:?}"),
    }
}

#[test]
fn 停止原因逐项映射() {
    for (s, want) in [
        ("tool_use", StopReason::ToolUse),
        ("max_tokens", StopReason::MaxTokens),
        ("end_turn", StopReason::EndTurn),
        ("stop_sequence", StopReason::EndTurn),
    ] {
        let d = format!(r#"{{"delta":{{"stop_reason":"{s}"}},"usage":{{"output_tokens":1}}}}"#);
        match 跑(&[("message_delta", &d)]).pop().unwrap() {
            LlmEvent::Done { stop_reason, .. } => assert_eq!(stop_reason, want, "{s}"),
            other => panic!("期望 Done，得到 {other:?}"),
        }
    }
}

#[test]
fn 错误帧只透出稳定类型不透出正文() {
    // message 可能含密钥或用户内容。
    let ev = 跑(&[(
        "error",
        r#"{"error":{"type":"overloaded_error","message":"key sk-abc123 rejected"}}"#,
    )]);
    assert_eq!(ev, [LlmEvent::Error("overloaded_error".into())]);
}

#[test]
fn 损坏帧被跳过而不终止流() {
    let mut st = StreamState::new();
    assert!(parse_sse_data("content_block_delta", "{不是 json", &mut st).is_empty());
    // 后续帧照常处理。
    let ev = parse_sse_data(
        "content_block_delta",
        r#"{"delta":{"type":"text_delta","text":"ok"}}"#,
        &mut st,
    );
    assert_eq!(ev, [LlmEvent::TextDelta("ok".into())]);
}

#[test]
fn 未知事件类型被忽略() {
    let mut st = StreamState::new();
    assert!(parse_sse_data("ping", "{}", &mut st).is_empty());
    assert!(parse_sse_data("message_stop", "{}", &mut st).is_empty());
}

#[test]
fn 思考签名单独成事件() {
    let ev = 跑(&[
        (
            "content_block_delta",
            r#"{"delta":{"type":"thinking_delta","thinking":"想"}}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"type":"signature_delta","signature":"sig"}}"#,
        ),
    ]);
    assert_eq!(
        ev,
        [
            LlmEvent::ThinkingDelta("想".into()),
            LlmEvent::ThinkingSignature("sig".into())
        ]
    );
}
