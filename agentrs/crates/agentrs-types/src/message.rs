// Ported from aionrs (Apache-2.0).
//   Source: aionrs/crates/aion-types/src/message.rs @ a5df989d110fb424bcd496b413e7ce7e20754414
//   Copied: 2026-08-25   Modified: yes
//   Changes:
//     - 时间戳由 chrono::DateTime<Utc> 改为 agentrs-contracts::Timestamp，
//       并移除 Message::now()——它调用 Utc::now()，违反"内核不读真实时钟"的边界判据；
//       时刻一律由 Clock port 提供。
//     - 移除 turn_id 字段：归属关系改由事件 envelope 的 Causality 承载，
//       避免消息与事件两处各存一份归属。
//     - ToolUseId 由裸 String 改为 contracts::ToolCallId newtype。
//     - 增加 PartialEq/Eq 以支持契约测试。
//     - 移除 base64 解码校验（依赖 base64 crate），仅保留结构与媒体类型判定。

//! provider 无关的消息与内容块模型。
//!
//! 这一层刻意保留 provider 私有元数据（`extra`、`signature`、`ProviderItem`），
//! 因为工具调用与 reasoning 签名必须能 round-trip。它们同时也是
//! `HistoryLegalization`（架构 §8.1）要处理的那批阻抗来源——签名会过期、
//! 跨 provider 会失效，修复只允许发生在投影期，不回写事实流。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use agentrs_contracts::ids::{Timestamp, ToolCallId};

/// 消息中的单个内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// 纯文本。
    #[serde(rename = "text")]
    Text {
        /// 文本内容。
        text: String,
    },

    /// 图像（data URI）。
    #[serde(rename = "image_url")]
    Image {
        /// 图像地址。
        image_url: ImageUrl,
    },

    /// 助手发起的工具调用。
    #[serde(rename = "tool_use")]
    ToolUse {
        /// 调用标识。
        id: ToolCallId,
        /// 工具名。
        name: String,
        /// 调用参数。
        input: Value,
        /// provider 私有元数据（如 Gemini thought_signature）。
        ///
        /// **原样 round-trip**，使 provider 能在后续请求中带回。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra: Option<Value>,
    },

    /// 工具执行结果，以用户消息回灌。
    #[serde(rename = "tool_result")]
    ToolResult {
        /// 对应的调用标识。
        tool_use_id: ToolCallId,
        /// 结果正文。
        content: String,
        /// 是否为错误结果。**错误是结构化结果，不是异常。**
        is_error: bool,
    },

    /// 思考 / 推理块。
    #[serde(rename = "thinking")]
    Thinking {
        /// 思考内容。
        thinking: String,
        /// provider 签名。
        ///
        /// round-trip Anthropic thinking 块时必需，但它是**有生命周期的不透明令牌**——
        /// 跨 provider fallback、跨压缩、跨长时间挂起后会失效，届时由
        /// `HistoryLegalization` 在投影期丢弃并留痕。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// 不透明的 provider 输出项，后续请求必须原样重放。
    ///
    /// 非其归属 provider 必须忽略此块。
    #[serde(rename = "provider_item")]
    ProviderItem {
        /// 归属 provider。
        provider: String,
        /// 原样载荷。
        item: Value,
    },
}

impl ContentBlock {
    /// 便捷构造纯文本块。
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    /// 该块是否为空内容（空文本块不进入派生历史）。
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text { text } => text.is_empty(),
            Self::Thinking { thinking, .. } => thinking.is_empty(),
            _ => false,
        }
    }
}

/// 图像地址。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUrl {
    /// data URI 或远程地址。
    pub url: String,
}

/// 主流 provider 普遍接受的图像媒体类型。
pub const SUPPORTED_IMAGE_MEDIA_TYPES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// 由文件扩展名映射到受支持的图像媒体类型。
pub fn extension_to_image_media_type(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// 目标模型的图像输入能力。
///
/// 不支持图像的模型在 `HistoryLegalization` 阶段把图像块降级为文本占位
/// （`DroppedUnsupportedBlock`），而不是直接报错。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageInputCapability {
    /// 支持。
    Supported,
    /// 不支持。
    Unsupported,
}

impl ImageInputCapability {
    /// 是否支持图像输入。
    pub fn supports_images(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// 对话中的一条消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// 角色。
    pub role: Role,
    /// 内容块序列。
    pub content: Vec<ContentBlock>,
    /// 创建时刻。**由 Clock port 提供**，内核不读真实时钟。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<Timestamp>,
}

impl Message {
    /// 构造一条消息。时刻由调用方从 Clock port 取得后传入。
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self {
            role,
            content,
            timestamp: None,
        }
    }

    /// 附上时刻。
    pub fn at(mut self, ts: Timestamp) -> Self {
        self.timestamp = Some(ts);
        self
    }

    /// 是否不含任何非空内容。
    ///
    /// 空 content 的 assistant 消息**不进入派生历史，但其事件必须保留**——
    /// 它承载 usage 与 `max_tokens` 之类的终止信息（架构 §4.3.1）。
    pub fn is_empty(&self) -> bool {
        self.content.iter().all(ContentBlock::is_empty)
    }
}

/// 消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 用户。
    User,
    /// 助手。
    Assistant,
    /// 系统。
    System,
    /// 工具。
    Tool,
}

/// 模型停止生成的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 自然结束。
    EndTurn,
    /// 需要调用工具。
    ToolUse,
    /// 触及 max_tokens。
    MaxTokens,
    /// 触及回合上限。
    MaxTurns,
}

/// token 用量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// provider 报告的完整输入，含缓存读取与创建部分。
    pub input_tokens: u64,
    /// 完整输出，含被计入总数的 reasoning token。
    pub output_tokens: u64,
    /// `input_tokens` 中用于创建 provider 侧缓存的部分。
    #[serde(default)]
    pub cache_creation_tokens: u64,
    /// `input_tokens` 中命中 provider 侧缓存的部分。
    #[serde(default)]
    pub cache_read_tokens: u64,
}

impl TokenUsage {
    /// 缓存命中率。无输入时返回 `None`。
    ///
    /// M1 出口标准要求真实 provider 连续 10 轮对话 ≥ 70%。
    pub fn cache_hit_rate(&self) -> Option<f64> {
        if self.input_tokens == 0 {
            return None;
        }
        Some(self.cache_read_tokens as f64 / self.input_tokens as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_私有元数据可_round_trip() {
        // 工具调用与 reasoning 签名必须原样往返，否则后续请求会被 provider 拒绝。
        let block = ContentBlock::ToolUse {
            id: "tc1".into(),
            name: "Read".into(),
            input: serde_json::json!({"path": "a.rs"}),
            extra: Some(serde_json::json!({"thought_signature": "opaque"})),
        };
        let back: ContentBlock = serde_json::from_str(&serde_json::to_string(&block).unwrap()).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn thinking_签名可缺省且缺省时不序列化() {
        let b = ContentBlock::Thinking {
            thinking: "…".into(),
            signature: None,
        };
        let json = serde_json::to_string(&b).unwrap();
        assert!(!json.contains("signature"), "缺省签名不应出现在 wire 上：{json}");
    }

    #[test]
    fn 空内容助手消息可被识别() {
        // 它不进入派生历史，但事件必须保留（承载 usage / max_tokens）。
        let m = Message::new(Role::Assistant, vec![ContentBlock::text("")]);
        assert!(m.is_empty());
        assert!(!Message::new(Role::Assistant, vec![ContentBlock::text("hi")]).is_empty());
    }

    #[test]
    fn 消息时刻由外部注入而非自取() {
        // 原实现有 Message::now() 调用 Utc::now()；移植时移除。
        let m = Message::new(Role::User, vec![]).at(Timestamp(1234));
        assert_eq!(m.timestamp, Some(Timestamp(1234)));
    }

    #[test]
    fn 缓存命中率可计算() {
        let u = TokenUsage {
            input_tokens: 1000,
            cache_read_tokens: 800,
            ..Default::default()
        };
        assert_eq!(u.cache_hit_rate(), Some(0.8));
        assert_eq!(TokenUsage::default().cache_hit_rate(), None);
    }

    #[test]
    fn 扩展名映射覆盖受支持的媒体类型() {
        for ext in ["jpg", "JPEG", "png", "gif", "webp"] {
            let mt = extension_to_image_media_type(ext).unwrap();
            assert!(SUPPORTED_IMAGE_MEDIA_TYPES.contains(&mt), "{ext} -> {mt}");
        }
        assert_eq!(extension_to_image_media_type("bmp"), None);
    }
}
