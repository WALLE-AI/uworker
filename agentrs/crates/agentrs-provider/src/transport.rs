//! HTTP 传输。**本 crate 中唯一做 I/O 的模块**——分帧与投影都是纯函数。
//!
//! 凭据由构造时注入，不读环境变量、不读配置文件（见 crate 级文档的四条约束）。

use agentrs_types::{LlmEvent, LlmRequest};
use async_trait::async_trait;
use futures::StreamExt;

use crate::sse::{SseDecoder, SseFrame};
use crate::{openai, ProviderError, ProviderPort};

/// OpenAI 兼容端点的适配器。
///
/// 覆盖 OpenAI、vLLM、DeepSeek、Ollama 等一切遵循 Chat Completions 的端点——
/// 新增一个这样的端点应当只需要换 `base_url`，不改代码（ADR 3）。
pub struct OpenAiCompatProvider {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    /// 构造适配器。
    ///
    /// `base_url` 形如 `http://localhost:8094/v1`；`api_key` 由调用方注入，
    /// **本 crate 不从环境读取凭据**。
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            // 本地端点必须绕过公司代理，否则会被代理拦成 502。
            .no_proxy()
            .build()
            .map_err(|_| ProviderError::Unreachable)?;
        Ok(Self {
            base_url: base_url.into(),
            api_key,
            client,
        })
    }
}

#[async_trait]
impl ProviderPort for OpenAiCompatProvider {
    async fn stream(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, ProviderError> {
        let body = openai::project_request(&req, true);
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut builder = self.client.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        let resp = builder.send().await.map_err(|_| ProviderError::Unreachable)?;
        let status = resp.status();
        if !status.is_success() {
            // 只按状态码分类，不读响应正文——它可能含密钥回显或用户内容。
            return Err(match status.as_u16() {
                401 | 403 => ProviderError::Unauthorized,
                429 => ProviderError::RateLimited,
                413 => ProviderError::ContextTooLong,
                code => ProviderError::Other {
                    code: format!("http_{code}"),
                },
            });
        }

        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        let mut stream = resp.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|_| ProviderError::Unreachable)?;
            let text = String::from_utf8_lossy(&bytes);
            for frame in decoder.push(&text) {
                match frame {
                    SseFrame::Done => return Ok(openai::finalize(events)),
                    SseFrame::Data(payload) => {
                        let parsed = openai::parse_chunk(&payload).map_err(|_| ProviderError::Malformed)?;
                        events.extend(parsed);
                    }
                }
            }
        }

        for frame in decoder.finish() {
            if let SseFrame::Data(payload) = frame {
                let parsed = openai::parse_chunk(&payload).map_err(|_| ProviderError::Malformed)?;
                events.extend(parsed);
            }
        }

        Ok(openai::finalize(events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 构造不读取环境凭据() {
        // api_key 必须由调用方传入；本 crate 没有任何 std::env 调用。
        let p = OpenAiCompatProvider::new("http://localhost:1/v1", None).unwrap();
        assert!(p.api_key.is_none());
    }

    #[tokio::test]
    async fn 端点不可达时返回稳定错误码() {
        let p = OpenAiCompatProvider::new("http://127.0.0.1:1/v1", None).unwrap();
        let req = LlmRequest {
            request_id: "r".into(),
            model: "m".into(),
            system: String::new(),
            messages: vec![],
            tools: vec![],
            max_tokens: Some(1),
            thinking: None,
            reasoning_effort: None,
            cache_prefix_digest: None,
        };
        assert_eq!(p.stream(req).await.unwrap_err(), ProviderError::Unreachable);
    }
}
