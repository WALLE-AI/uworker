// Ported from aionrs (Apache-2.0).
//   Source: aionrs/crates/aion-providers/src/{bedrock.rs,vertex.rs} @ f7111746015d8e6f960e1568a805ceef975022d3
//   Copied: 2026-08-25   Modified: yes
//   Changes:
//     - **只移植线格式，不移植凭据解析**。原实现的绝大部分是 AWS SigV4 凭据链
//       （读 AWS_* 环境变量、读 ~/.aws/credentials、访问 IMDS）与 GCP ADC
//       （读 service-account key 文件、访问 metadata server）。按架构 §1.1，
//       "读环境变量 / 读磁盘 / 解析凭据"一律归 Core：内核只能接收**已解析的凭据**。
//     - 移除 SystemTime::now()：SigV4 签名需要时间戳，但内核不读真实时钟。
//       签名时刻由调用方通过 Clock port 取得后传入 [`SigningInput::at`]。
//     - 移除同步 block_on 解析凭据的那段（原实现为此起了临时 tokio runtime）：
//       内核不做阻塞式凭据获取。
//     - 移除 reqwest HeaderMap 依赖：这里只产出**需要哪些头**的描述，
//       实际组装在 transport 层，保持本模块为纯函数、可无网络测试。

//! Anthropic 家族的三种承载线格式。
//!
//! 同一套 Messages 请求体（见 [`crate::anthropic`]）会以三种方式发出：
//!
//! | 承载 | model 在哪 | anthropic_version | stream 字段 |
//! |---|---|---|---|
//! | 直连 API | body | 不带 | body 里 |
//! | Bedrock | **URL 路径** | `bedrock-2023-05-31` | **不带**（由端点决定） |
//! | Vertex | **URL 路径** | `vertex-2023-10-16` | body 里 |
//!
//! 把 `model` 留在 body 里发给 Bedrock 会直接 400——这是最容易漏的一条，
//! 因为直连和 Vertex 的调试经验都不会暴露它。
//!
//! **本模块不碰凭据。** 见文件头的移植说明。

use serde_json::{json, Value};

/// 承载方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Carrier {
    /// 直连 Anthropic API。
    Direct {
        /// 端点基址，由 `ModelPolicy` 指定。
        base_url: String,
    },
    /// AWS Bedrock。
    Bedrock {
        /// 区域，如 `us-east-1`。
        region: String,
    },
    /// GCP Vertex AI。
    Vertex {
        /// 项目 id。
        project_id: String,
        /// 区域。
        region: String,
    },
}

/// 本次请求所需的认证方式描述。
///
/// **只描述"需要什么"，不产出凭据本身**——凭据由 Core 注入到 transport。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRequirement {
    /// `x-api-key` 头。
    ApiKey,
    /// AWS SigV4 签名。
    SigV4 {
        /// 签名服务名。
        service: &'static str,
        /// 签名区域。
        region: String,
    },
    /// GCP OAuth Bearer。
    Bearer,
}

/// 投影后的线上请求。
#[derive(Debug, Clone, PartialEq)]
pub struct WireRequest {
    /// 完整 URL。
    pub url: String,
    /// 请求体。
    pub body: Value,
    /// 需要的认证方式。
    pub auth: AuthRequirement,
}

impl Carrier {
    /// 把一份**已按 Anthropic Messages 投影好的** body 适配到本承载。
    ///
    /// `body` 应来自 [`crate::anthropic::project_request`]。
    pub fn wire(&self, model: &str, mut body: Value) -> WireRequest {
        match self {
            Self::Direct { base_url } => WireRequest {
                url: format!("{}/v1/messages", base_url.trim_end_matches('/')),
                body,
                auth: AuthRequirement::ApiKey,
            },

            Self::Bedrock { region } => {
                // model 走路径；留在 body 里会 400。
                strip(&mut body, "model");
                // Bedrock 的流式由 endpoint 决定，body 里带 stream 是非法字段。
                strip(&mut body, "stream");
                body["anthropic_version"] = json!("bedrock-2023-05-31");
                WireRequest {
                    url: format!(
                        "https://bedrock-runtime.{region}.amazonaws.com/model/{model}/invoke-with-response-stream"
                    ),
                    body,
                    auth: AuthRequirement::SigV4 {
                        service: "bedrock",
                        region: region.clone(),
                    },
                }
            }

            Self::Vertex { project_id, region } => {
                strip(&mut body, "model");
                body["anthropic_version"] = json!("vertex-2023-10-16");
                WireRequest {
                    url: format!(
                        "https://{region}-aiplatform.googleapis.com/v1/projects/{project_id}\
                         /locations/{region}/publishers/anthropic/models/{model}:streamRawPredict"
                    ),
                    body,
                    auth: AuthRequirement::Bearer,
                }
            }
        }
    }

    /// 该承载对应的 provider 判别串，用于缓存归因（`CacheBreakCause::ProviderSwitched`）。
    pub fn provider_key(&self) -> &'static str {
        match self {
            Self::Direct { .. } => "anthropic",
            Self::Bedrock { .. } => "bedrock",
            Self::Vertex { .. } => "vertex",
        }
    }
}

fn strip(body: &mut Value, key: &str) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove(key);
    }
}

/// SigV4 签名所需的、**由调用方提供**的全部输入。
///
/// 内核既不解析凭据也不读时钟；这个结构存在的意义就是把两者都变成显式参数，
/// 从而让"谁提供了凭据、用的哪一刻"在类型上可见。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningInput {
    /// 访问密钥 id。
    pub access_key_id: String,
    /// 会话令牌（STS 临时凭据才有）。
    pub session_token: Option<String>,
    /// 签名时刻（epoch 秒）。**由 Clock port 提供。**
    pub at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 体() -> Value {
        json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
            "stream": true,
            "max_tokens": 1024
        })
    }

    #[test]
    fn 直连保留_model_与_stream() {
        let w = Carrier::Direct {
            base_url: "https://api.anthropic.com/".into(),
        }
        .wire("claude-3-5-sonnet", 体());
        // 尾部斜杠不能变成双斜杠。
        assert_eq!(w.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(w.body["model"], "claude-3-5-sonnet");
        assert_eq!(w.body["stream"], true);
        assert!(w.body["anthropic_version"].is_null());
        assert_eq!(w.auth, AuthRequirement::ApiKey);
    }

    #[test]
    fn bedrock_把_model_移到路径并删掉_stream() {
        // **最容易漏的一条**：直连与 Vertex 的调试经验都不会暴露它。
        let w = Carrier::Bedrock {
            region: "us-east-1".into(),
        }
        .wire("anthropic.claude-3-5-sonnet-v1:0", 体());
        assert!(w.body["model"].is_null(), "model 留在 body 里会 400");
        assert!(w.body["stream"].is_null(), "Bedrock 的 stream 由端点决定");
        assert_eq!(w.body["anthropic_version"], "bedrock-2023-05-31");
        assert!(w
            .url
            .contains("/model/anthropic.claude-3-5-sonnet-v1:0/invoke-with-response-stream"));
        assert!(w.url.starts_with("https://bedrock-runtime.us-east-1."));
    }

    #[test]
    fn vertex_把_model_移到路径但保留_stream() {
        let w = Carrier::Vertex {
            project_id: "proj".into(),
            region: "us-central1".into(),
        }
        .wire("claude-3-5-sonnet@20240620", 体());
        assert!(w.body["model"].is_null());
        assert_eq!(w.body["stream"], true, "Vertex 与 Bedrock 在这一点上相反");
        assert_eq!(w.body["anthropic_version"], "vertex-2023-10-16");
        assert_eq!(
            w.url,
            "https://us-central1-aiplatform.googleapis.com/v1/projects/proj\
             /locations/us-central1/publishers/anthropic/models/claude-3-5-sonnet@20240620:streamRawPredict"
        );
        assert_eq!(w.auth, AuthRequirement::Bearer);
    }

    #[test]
    fn 三种承载的_anthropic_version_互不相同() {
        // 版本串串台会让请求以难懂的方式失败（字段被静默忽略）。
        let 直连 = Carrier::Direct {
            base_url: "https://x".into(),
        }
        .wire("m", 体());
        let bedrock = Carrier::Bedrock { region: "r".into() }.wire("m", 体());
        let vertex = Carrier::Vertex {
            project_id: "p".into(),
            region: "r".into(),
        }
        .wire("m", 体());
        assert!(直连.body["anthropic_version"].is_null());
        assert_ne!(
            bedrock.body["anthropic_version"],
            vertex.body["anthropic_version"]
        );
    }

    #[test]
    fn 承载各有独立的_provider_判别串() {
        // 缓存归因要能把"换了承载"和"改了提示"分开。
        let keys = [
            Carrier::Direct { base_url: "x".into() }.provider_key(),
            Carrier::Bedrock { region: "r".into() }.provider_key(),
            Carrier::Vertex {
                project_id: "p".into(),
                region: "r".into(),
            }
            .provider_key(),
        ];
        let mut uniq = keys.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 3);
    }

    #[test]
    fn 签名输入把凭据与时刻都变成显式参数() {
        // 类型上就看得出"谁提供了凭据、用的哪一刻"——内核两者都不自己取。
        let s = SigningInput {
            access_key_id: "AKIA...".into(),
            session_token: None,
            at: 1_700_000_000,
        };
        assert_eq!(s.at, 1_700_000_000);
    }
}
