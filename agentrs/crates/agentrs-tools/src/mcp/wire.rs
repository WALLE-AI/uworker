// Ported from aionrs (Apache-2.0), crates/aion-mcp.
//   Source: crates/aion-mcp/src/protocol.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 全部字段改为 `pub` 且去掉 `#[allow(dead_code)]`——本 crate 只出
//            线格式，"用不上"由宿主决定，不由这里；两个方向都实现
//            `Serialize + Deserialize`（上游按自己的收发方向各只实现一个，
//            于是宿主没法回放一份录下来的会话）；补上 `McpContent::Text` 之外
//            两种内容的取值方法；协议版本提升为常量。

//! MCP 的线格式。
//!
//! **只有类型，没有传输。** 进程怎么起、stdio 怎么接、凭据从哪来——全归 Core
//! （见 [`crate::mcp`] 的模块文档）。这里出的是一组 serde 类型，
//! 让宿主能把 JSON-RPC 报文解出来、把请求拼出去。
//!
//! 与 [`super::McpCatalog`] 的分工：那边管**名字空间与反向路由**
//! （`mcp::{server}::{tool}` ↔ 远端名），这边管**报文长什么样**。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 本实现对话的 MCP 协议版本。
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// 一条 JSON-RPC 2.0 请求。
///
/// `id` 为 `None` 时它是一条**通知**——通知没有回应，发出去就完了。
/// 把两者混成一个类型是刻意的：它们在线上只差一个字段，分成两个类型
/// 会让调用方在两个几乎一样的构造函数之间选。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// 恒为 `"2.0"`。
    pub jsonrpc: String,
    /// 请求标识；通知没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// 方法名。
    pub method: String,
    /// 参数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// 一条要回应的请求。
    pub fn new(id: u64, method: &str, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: method.into(),
            params,
        }
    }

    /// 一条通知：没有 id，也不会有回应。
    pub fn notification(method: &str, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params,
        }
    }

    /// 这是通知吗？
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// 一条 JSON-RPC 2.0 回应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// 恒为 `"2.0"`。
    pub jsonrpc: String,
    /// 对应请求的标识。
    pub id: Option<u64>,
    /// 成功时的结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 失败时的错误。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// 取结果；带错误时返回 `Err`。
    ///
    /// **两个字段都可能同时缺**——一份不合规的实现会回一条既无 result 也无
    /// error 的报文。那种情况当错误处理，而不是当成"成功但结果是 null"。
    pub fn outcome(&self) -> Result<&Value, String> {
        match (&self.result, &self.error) {
            (_, Some(e)) => Err(format!("MCP 错误 {}：{}", e.code, e.message)),
            (Some(v), None) => Ok(v),
            (None, None) => Err("回应里既没有 result 也没有 error".into()),
        }
    }
}

/// JSON-RPC 错误体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// 错误码。
    pub code: i64,
    /// 错误说明。
    pub message: String,
    /// 附加数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// `tools/list` 里的一条工具。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolDef {
    /// 远端工具名。
    pub name: String,
    /// 描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 参数 schema。
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// `tools/list` 的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsListResult {
    /// 工具表。
    pub tools: Vec<McpToolDef>,
}

/// 一次工具调用的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    /// 内容块。
    pub content: Vec<McpContent>,
}

impl McpToolResult {
    /// 把全部文本块拼起来。
    ///
    /// 图像与资源块**跳过而不是报错**：一个返回图文混排的工具，
    /// 它的文字部分仍然有用，而本仓库的工具结果通道是纯文本的。
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                McpContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 有没有非文本内容被丢掉了。
    ///
    /// 调用方据此决定要不要告诉模型"这里还有一张图，但这条通道传不了"——
    /// 默默丢掉会让模型以为工具什么也没返回。
    pub fn has_non_text(&self) -> bool {
        self.content.iter().any(|c| !matches!(c, McpContent::Text { .. }))
    }
}

/// 工具结果里的一块内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    /// 一段文本。
    Text {
        /// 正文。
        text: String,
    },
    /// 一张图（base64）。
    Image {
        /// base64 数据。
        data: String,
        /// MIME 类型。
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// 一份资源引用。
    Resource {
        /// 原样保留。
        resource: Value,
    },
}

/// `initialize` 的参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    /// 协议版本。
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// 客户端能力。
    pub capabilities: ClientCapabilities,
    /// 客户端信息。
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

impl InitializeParams {
    /// 一份声明"我要工具"的握手参数。
    pub fn tools_only(client: &str, version: &str) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.into(),
            capabilities: ClientCapabilities {
                tools: Some(serde_json::json!({})),
            },
            client_info: ClientInfo {
                name: client.into(),
                version: version.into(),
            },
        }
    }
}

/// 客户端能力。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientCapabilities {
    /// 工具能力。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Value>,
}

/// 客户端信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// 名字。
    pub name: String,
    /// 版本。
    pub version: String,
}

/// `initialize` 的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    /// 服务端认的协议版本。
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// 服务端能力。
    pub capabilities: Value,
    /// 服务端信息。
    #[serde(rename = "serverInfo", default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<Value>,
}

/// `resources/list` 里的一条资源。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResource {
    /// 资源 URI。
    pub uri: String,
    /// 名字。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// MIME 类型。
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// `resources/list` 的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesListResult {
    /// 资源表。
    pub resources: Vec<McpResource>,
}

/// `resources/read` 的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesReadResult {
    /// 内容表。
    pub contents: Vec<ResourceContent>,
}

/// 一份资源的内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    /// 资源 URI。
    pub uri: String,
    /// MIME 类型。
    #[serde(rename = "mimeType", default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// 文本内容；二进制资源为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 请求与通知只差一个_id() {
        let r = JsonRpcRequest::new(1, "tools/list", None);
        assert!(!r.is_notification());
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        // 没有 params 时不发这个字段——有些服务端对 `"params": null` 挑剔。
        assert!(v.get("params").is_none());

        let n = JsonRpcRequest::notification("notifications/initialized", None);
        assert!(n.is_notification());
        assert!(serde_json::to_value(&n).unwrap().get("id").is_none());
    }

    #[test]
    fn 回应的成功与失败分得开() {
        let ok: JsonRpcResponse =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}})).unwrap();
        assert!(ok.outcome().is_ok());

        let err: JsonRpcResponse = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}),
        )
        .unwrap();
        let e = err.outcome().unwrap_err();
        assert!(e.contains("-32601"), "{e}");
        assert!(e.contains("Method not found"), "{e}");
    }

    #[test]
    fn 既无结果也无错误当错误处理() {
        // 一份不合规的实现会回这种报文。当成"成功但结果是 null"的话，
        // 后面解析结果的地方会得到一个它没法解释的空值。
        let r: JsonRpcResponse = serde_json::from_value(json!({"jsonrpc":"2.0","id":1})).unwrap();
        assert!(r.outcome().is_err());
    }

    #[test]
    fn tools_list_解得出来() {
        let r: ToolsListResult = serde_json::from_value(json!({
            "tools": [{"name":"search","description":"找东西","inputSchema":{"type":"object"}}]
        }))
        .unwrap();
        assert_eq!(r.tools[0].name, "search");
        assert_eq!(r.tools[0].input_schema["type"], "object");
    }

    #[test]
    fn 没有描述的工具也解得出来() {
        // 描述是可选的。要求它必填会让一整台服务器的工具都注册不上。
        let r: ToolsListResult =
            serde_json::from_value(json!({"tools":[{"name":"x","inputSchema":{}}]})).unwrap();
        assert_eq!(r.tools[0].description, None);
    }

    #[test]
    fn 工具结果取文本() {
        let r: McpToolResult = serde_json::from_value(json!({
            "content": [{"type":"text","text":"第一段"},{"type":"text","text":"第二段"}]
        }))
        .unwrap();
        assert_eq!(r.text(), "第一段\n第二段");
        assert!(!r.has_non_text());
    }

    #[test]
    fn 图文混排时文字仍取得到_但会说有东西没带过来() {
        // 默默丢掉会让模型以为工具什么也没返回。
        let r: McpToolResult = serde_json::from_value(json!({
            "content": [
                {"type":"text","text":"看这张图"},
                {"type":"image","data":"QUJD","mimeType":"image/png"}
            ]
        }))
        .unwrap();
        assert_eq!(r.text(), "看这张图");
        assert!(r.has_non_text(), "调用方得知道还有一张图没带过来");
    }

    #[test]
    fn 资源块也认得() {
        let r: McpToolResult = serde_json::from_value(json!({
            "content": [{"type":"resource","resource":{"uri":"file:///x"}}]
        }))
        .unwrap();
        assert_eq!(r.text(), "");
        assert!(r.has_non_text());
    }

    #[test]
    fn 握手参数带上协议版本与工具能力() {
        let p = InitializeParams::tools_only("agentrs", "0.0.1");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["clientInfo"]["name"], "agentrs");
        assert!(v["capabilities"]["tools"].is_object());
    }

    #[test]
    fn 收发两个方向都能来回转() {
        // 上游按自己的收发方向各只实现一个 trait，于是宿主没法回放一份
        // 录下来的会话——而那正是排查 MCP 问题最有用的手段。
        let req = JsonRpcRequest::new(7, "tools/call", Some(json!({"name":"x"})));
        let back: JsonRpcRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back.method, "tools/call");
        assert_eq!(back.id, Some(7));

        let res = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(7),
            result: Some(json!({"content":[]})),
            error: None,
        };
        let back: JsonRpcResponse =
            serde_json::from_str(&serde_json::to_string(&res).unwrap()).unwrap();
        assert!(back.outcome().is_ok());
    }

    #[test]
    fn 资源相关的报文解得出来() {
        let l: ResourcesListResult = serde_json::from_value(json!({
            "resources":[{"uri":"file:///a","name":"甲","mimeType":"text/plain"}]
        }))
        .unwrap();
        assert_eq!(l.resources[0].uri, "file:///a");

        let r: ResourcesReadResult = serde_json::from_value(json!({
            "contents":[{"uri":"file:///a","text":"内容"}]
        }))
        .unwrap();
        assert_eq!(r.contents[0].text.as_deref(), Some("内容"));
        // 二进制资源没有 text。
        let b: ResourcesReadResult =
            serde_json::from_value(json!({"contents":[{"uri":"file:///b"}]})).unwrap();
        assert_eq!(b.contents[0].text, None);
    }
}
