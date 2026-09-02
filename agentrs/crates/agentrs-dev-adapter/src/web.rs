//! 出站 HTTP 的机制部分：解析域名、发请求。
//!
//! **判定不在这里。** SSRF 那套（哪些地址不许去、每一跳重定向重判、下一跳怎么算）
//! 在 `agentrs_tools::builtin::net_guard`，由 `web_fetch` 在调用本模块之前做完。
//! 反过来放的话，一个换了 adapter 的宿主就能悄悄换掉安全判定。
//!
//! 本模块只兑现三件事：
//!
//! 1. **按端口传来的 `pin` 连**——判定已经核过那个地址，这里不能自作主张换一个
//!    （只判不钉的话，判定用的解析与实际连接用的解析是两次，中间足够让一个自己
//!    控制的 DNS 把答案换成 127.0.0.1）；
//! 2. **不跟随重定向**——跟随就等于跳过了逐跳重判；
//! 3. **边读边数**，超过上限就停，不是先收完再判——先收完等于让对方决定我们分配
//!    多少内存。
//!
//! 与 `agentrs-provider` 相反，这里**尊重**环境里的代理设置：provider 连的是本机
//! 端点（那里 `.no_proxy()` 是对的），而 WebFetch 取的是外网。

use std::net::SocketAddr;

use agentrs_tools::builtin::{Http, HttpGet, HttpResponse, SearchEndpoint};
use async_trait::async_trait;
use serde_json::Value;

/// 固定 UA。不伪装成浏览器：对方按 UA 决定给什么内容是它的权利，
/// 骗过它拿到的东西不该由我们代模型接受。
const UA: &str = concat!("AgentRS-DevAdapter/", env!("CARGO_PKG_VERSION"));

/// 从环境读搜索端点；`AGENTRS_SEARCH_URL` 与 `AGENTRS_SEARCH_KEY` 缺任何一个即未配置。
///
/// 读环境的是宿主（本 crate），不是内核——内核库的门禁依旧禁止 `std::env::var`。
pub fn search_endpoint_from_env() -> Option<SearchEndpoint> {
    let url = std::env::var("AGENTRS_SEARCH_URL").ok()?;
    let key = std::env::var("AGENTRS_SEARCH_KEY").ok()?;
    (!url.trim().is_empty() && !key.trim().is_empty()).then(|| SearchEndpoint {
        url: url.trim().to_string(),
        key: key.trim().to_string(),
    })
}

/// 基于 `reqwest` 的出站 HTTP。
#[derive(Debug, Default)]
pub struct ReqwestHttp;

#[async_trait]
impl Http for ReqwestHttp {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, ()> {
        tokio::net::lookup_host((host, port))
            .await
            .map(Iterator::collect)
            .map_err(|_| ())
    }

    fn proxied(&self, url: &str) -> bool {
        proxied(url)
    }

    async fn get(&self, req: HttpGet<'_>) -> Result<HttpResponse, String> {
        let mut builder = reqwest::Client::builder()
            .timeout(req.timeout)
            // 不跟随：跟随就跳过了逐跳重判。
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(UA);
        // 判定已经核过这个地址，钉住它（防 DNS rebinding）。
        // 走代理时钉了也没用（连的是代理），此时判定会传 `None`。
        if let Some(addr) = req.pin {
            let host = reqwest::Url::parse(req.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .ok_or_else(|| "URL 无法解析".to_string())?;
            builder = builder.resolve(&host, addr);
        }
        let client = builder
            .build()
            .map_err(|e| format!("HTTP 客户端构造失败：{e}"))?;

        // 不带 cookie：跨跳携带凭据正是 SSRF 想利用的东西。
        let resp = client
            .get(req.url)
            .header(reqwest::header::ACCEPT, req.accept)
            .send()
            .await
            .map_err(|e| format!("请求失败：{}", 脱敏(&e.to_string())))?;

        let status = resp.status().as_u16();
        let header = |name: reqwest::header::HeaderName| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let location = header(reqwest::header::LOCATION);
        let content_type = header(reqwest::header::CONTENT_TYPE).unwrap_or_default();
        let body = 限量读取(resp, req.max_bytes).await?;
        Ok(HttpResponse {
            status,
            location,
            content_type,
            body,
        })
    }

    async fn post_json(&self, url: &str, key: &str, body: Value) -> Result<Value, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(UA)
            .build()
            .map_err(|e| format!("HTTP 客户端构造失败：{e}"))?;
        let resp = client
            .post(url)
            // key 只在这里出现一次，且只往请求头去。
            .header("x-api-key", key)
            .json(&body)
            .send()
            .await
            // 错误里可能带着完整 URL；万一 key 在 query string 里，脱敏兜住它。
            .map_err(|e| 脱敏(&e.to_string()))?;
        if !resp.status().is_success() {
            return Err(format!("搜索端点返回 HTTP {}", resp.status()));
        }
        resp.json()
            .await
            .map_err(|e| format!("返回的不是 JSON：{}", 脱敏(&e.to_string())))
    }
}

/// 这个 URL 会走代理吗？
///
/// 只看环境变量，与 `reqwest` 默认的取值口径一致。判定据此决定"解析不出来"
/// 算异常还是算常态——没有直连 DNS 的机器上后者是常态。
fn proxied(url: &str) -> bool {
    let 有代理 = [
        "https_proxy",
        "HTTPS_PROXY",
        "http_proxy",
        "HTTP_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ]
    .iter()
    .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()));
    if !有代理 {
        return false;
    }
    let Some(host) = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    for key in ["no_proxy", "NO_PROXY"] {
        let Ok(list) = std::env::var(key) else { continue };
        for entry in list.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            if entry == "*" {
                return false;
            }
            let e = entry.trim_start_matches('.').to_ascii_lowercase();
            if host == e || host.ends_with(&format!(".{e}")) {
                return false;
            }
        }
    }
    true
}

/// 边读边数，超过上限就停——**不是先收完再判**。
async fn 限量读取(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                buf.extend_from_slice(&chunk);
                if buf.len() > max {
                    buf.truncate(max);
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => return Err(format!("读取响应失败：{}", 脱敏(&e.to_string()))),
        }
    }
    Ok(buf)
}

/// 错误文本里可能带着完整 URL。把 query string 抹掉——key 常常在那里。
fn 脱敏(message: &str) -> String {
    message
        .split_whitespace()
        .map(|w| match w.find('?') {
            Some(i) if w.starts_with("http") => format!("{}?…", &w[..i]),
            _ => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 错误文本里的_query_string_被抹掉() {
        let raw = "error sending request for url https://api.example.com/s?key=sk-绝密&q=x";
        let out = 脱敏(raw);
        assert!(!out.contains("sk-绝密"), "{out}");
        assert!(out.contains("https://api.example.com/s?…"), "{out}");
    }

    #[test]
    fn 没有代理配置时不认为走代理() {
        // 这条只在环境干净时有意义；有代理的机器上它会被跳过。
        if std::env::var("http_proxy").is_ok() || std::env::var("HTTP_PROXY").is_ok() {
            return;
        }
        assert!(!proxied("https://example.com/"));
    }
}
