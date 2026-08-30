//! # AgentRS Provider
//!
//! LLM seam：`ProviderPort` 定义 + 各厂商适配器 + SSE 分帧 + 请求投影。
//!
//! ## 边界说明（重要）
//!
//! 架构 §1.1 的规则 1 是"碰网络 → 归 Core"。**本 crate 是该规则唯一的
//! 受约束例外**，因为它的全部存在意义就是与模型 API 通话。约束有四条：
//!
//! 1. 只连 `ModelPolicy` 指定的端点，不做任意网络访问；
//! 2. 凭据**由调用方注入**，不读环境变量、不读配置文件；
//! 3. 不碰磁盘、不起进程——`clippy.toml` 的这两条禁令对本 crate 依然生效；
//! 4. 分帧与投影是纯函数（[`sse`]、[`openai`]），可在无网络下完整测试；
//!    I/O 只存在于 [`transport`]。
//!
//! 换句话说：例外的是"网络"这一项，不是"边界"本身。
//!
//! ## 当前进度
//!
//! - ✅ [`sse`] SSE 分帧
//! - ✅ [`openai`] OpenAI 兼容投影与解析
//! - ✅ [`transport`] HTTP 传输 + `ProviderPort`
//! - ✅ [`compat`] ProviderCompat 数据表
//! - ✅ [`legalization`] HistoryLegalization 固定阶段
//! - ✅ [`anthropic`] Anthropic Messages 投影与解析（Bedrock/Vertex 复用其分帧）
//! - ✅ [`anthropic_wire`] 直连 / Bedrock / Vertex 三种承载线格式
//! - ✅ [`openai_responses`] OpenAI Responses API 投影
//!
//! 凭据解析（AWS SigV4 凭据链、GCP ADC）**不在本 crate**——它读环境变量、
//! 读磁盘、访问 metadata server，按 §1.1 归 Core，凭据由 transport 注入。

#![forbid(unsafe_code)]

pub mod anthropic;
pub mod anthropic_wire;
pub mod compat;
pub mod legalization;
pub mod openai;
pub mod openai_responses;
pub mod routing;
pub mod sse;
pub mod transport;

pub use compat::ProviderCompat;
pub use legalization::{legalize, Legalized};

use agentrs_types::{LlmEvent, LlmRequest};
use async_trait::async_trait;

/// LLM seam 的 Service Definition。
///
/// 与 contracts 中的十个 port 不同，本 trait 由**适配器**实现而非宿主，
/// 因此定义在这里而不是 contracts（contracts 只holds 面向宿主的 port）。
#[async_trait]
pub trait ProviderPort: Send + Sync {
    /// 发起一次流式请求。
    ///
    /// 实现方必须保证：已产出可见文本后**不自动重试**（架构 §6.1），
    /// 错误正文不透传（可能含密钥或用户内容）。
    async fn stream(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, ProviderError>;
}

/// 稳定的 provider 错误码。用户可读文案与 i18n 归 Core（架构 §1.1 裁定 5）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProviderError {
    /// 端点不可达。
    #[error("provider unreachable")]
    Unreachable,
    /// 被限流。
    #[error("rate limited")]
    RateLimited,
    /// 上下文超长。**触发一次受控压缩恢复，不是无限重试。**
    #[error("context too long")]
    ContextTooLong,
    /// 认证失败。
    #[error("unauthorized")]
    Unauthorized,
    /// 响应格式非法。
    #[error("malformed response")]
    Malformed,
    /// 其他，已脱敏。
    #[error("provider error: {code}")]
    Other {
        /// 稳定错误码，非响应正文。
        code: String,
    },
}

impl ProviderError {
    /// Whether another attempt on the same route may succeed.
    pub fn retry_same_route(&self) -> bool {
        matches!(self, Self::Unreachable | Self::RateLimited | Self::Other { .. })
    }

    /// Whether policy may move to the next pre-authorized route.
    pub fn allows_fallback(&self) -> bool {
        matches!(
            self,
            Self::Unreachable | Self::RateLimited | Self::Malformed | Self::Other { .. }
        )
    }

    /// Errors handled by another layer or requiring operator action.
    pub fn is_terminal_for_routing(&self) -> bool {
        matches!(self, Self::ContextTooLong | Self::Unauthorized)
    }
}
