//! `WebFetch`：取一个公网网页并抽成可读文本。
//!
//! SSRF 判定在 [`super::net_guard`]，**每一跳重定向都重判**——这段循环留在这里
//! 而不是 adapter 里，因为它是这个工具最要紧的一段逻辑。
//!
//! HTML → 文本的抽取器是手写的：与 glob 匹配器、markdown 渲染器同一判断，
//! 要处理的形状固定且少，离线 registry 里也没有现成的转换 crate。

use std::time::Duration;

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use super::net_guard::{self, Refusal};
use super::text;
use super::{arg, HttpGet, ToolCtx, ToolOutput};

/// 默认超时。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// 超时上限。
pub const MAX_TIMEOUT_MS: u64 = 120_000;
/// 响应体大小上限。
pub const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
/// 交回模型的文本上限。5 MiB 的 HTML 抽完文本仍可能有几十万字。
pub const MAX_TEXT_BYTES: usize = 96 * 1024;

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::read_only(
        "WebFetch",
        "取一个公网网页并抽成可读文本。只支持 http/https，且**不会**去取本机与\
         内网地址——那些地方常有凭据服务与内部接口。取回的内容有长度上限，\
         超出会明确告知截了多少。",
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "要取的完整 URL，必须以 http:// 或 https:// 开头",
                    "minLength": 8
                },
                "format": {
                    "type": "string",
                    "description": "markdown 保留段落结构，text 只要纯文字；默认 markdown",
                    "enum": ["markdown", "text"]
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "超时毫秒数，默认 30000，上限 120000",
                    "minimum": 1,
                    "maximum": 120000
                }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
    )
}

/// 把 `timeout_ms` 收进合法区间。
pub fn timeout_of(raw: Option<u64>) -> Duration {
    Duration::from_millis(raw.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1, MAX_TIMEOUT_MS))
}

/// 执行。
pub async fn run(cx: &ToolCtx<'_>, args: &Value) -> ToolOutput {
    let Some(url) = arg(args, "url") else {
        return ToolOutput::bad_args("url");
    };
    let Some(http) = cx.http else {
        return ToolOutput::failed("本宿主没有提供出站网络能力");
    };
    let as_markdown = arg(args, "format").as_deref() != Some("text");
    let timeout = timeout_of(args.get("timeout_ms").and_then(Value::as_u64));

    match fetch(http, &url, as_markdown, timeout).await {
        Ok(body) => ToolOutput::ok(body),
        // 拒绝走"失败"而不是 `Rejected`：`Rejected` 是授权层的话，模型看到会
        // 停下；这是工具自己的话，模型读得懂，会改用别的办法。一个模型选错了
        // URL，不该让整个 Run 看起来像撞上了权限墙。
        Err(why) => ToolOutput::failed(why),
    }
}

async fn fetch(
    http: &dyn super::Http,
    raw: &str,
    as_markdown: bool,
    timeout: Duration,
) -> Result<String, String> {
    let mut current = raw.to_string();
    for _ in 0..net_guard::MAX_HOPS {
        // 每一跳都重判。首跳公网、302 到 127.0.0.1 是最容易漏的一种，
        // 因为第一跳看起来完全正常。
        let (parsed, host, port) = net_guard::vet_without_dns(&current).map_err(|r| r.to_string())?;

        // 能解析就按 IP 判，并把连接钉在核过的那个地址上（防 DNS rebinding）。
        //
        // 解析不出来有两种可能：域名真的不存在，或者本机没有直连 DNS。
        // 走代理时后者是**常态**——解析是代理做的，我们既钉不住也看不见结果。
        // 那种情形下一律拒绝的话，这个工具在此类机器上一条也取不到。
        let pin = match http.resolve(&host, port).await {
            Ok(addrs) if !addrs.is_empty() => {
                net_guard::vet_addresses(&addrs).map_err(|r| r.to_string())?;
                Some(addrs[0])
            }
            _ if http.proxied(&current) => None,
            _ => return Err(Refusal::Unresolvable(host).to_string()),
        };

        let accept = if as_markdown {
            "text/html,text/markdown,text/plain;q=0.9,*/*;q=0.8"
        } else {
            "text/plain,text/html;q=0.9,*/*;q=0.8"
        };
        let resp = http
            .get(HttpGet {
                url: &current,
                accept,
                pin,
                timeout,
                max_bytes: MAX_RESPONSE_BYTES,
            })
            .await?;

        if (300..400).contains(&resp.status) {
            let Some(next) = resp.location.as_deref() else {
                return Err(format!("HTTP {} 重定向但没给 Location", resp.status));
            };
            current = net_guard::next_hop(&parsed, next).map_err(|r| r.to_string())?;
            continue;
        }
        if !(200..300).contains(&resp.status) {
            return Err(format!("HTTP {}", resp.status));
        }

        let body = String::from_utf8_lossy(&resp.body).into_owned();
        let out = if resp.content_type.contains("html") || 像_html(&body) {
            html_to_text(&body)
        } else {
            body
        };
        return Ok(text::truncate_bytes(out.trim(), MAX_TEXT_BYTES, ""));
    }
    Err(Refusal::TooManyHops.to_string())
}

fn 像_html(text: &str) -> bool {
    let head = text.trim_start().to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html") || head.starts_with("<?xml")
}

/// 会让内容换行的块级标签。
const 块级: [&str; 17] = [
    "p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6", "section", "article",
    "header", "footer", "blockquote", "pre",
];

/// 内容整段丢弃的标签。
const 丢弃: [&str; 6] = ["script", "style", "head", "noscript", "svg", "iframe"];

/// 把 HTML 抽成可读文本。
///
/// 一个状态机，不建 DOM：要处理的形状固定（丢脚本样式、块级换行、解实体、
/// 折叠空白），建 DOM 的那份复杂度换不来对应的收益。
pub fn html_to_text(html: &str) -> String {
    let chars: Vec<char> = html.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    // 当前正处在哪个"整段丢弃"的标签里；`None` 表示在正文中。
    let mut 丢弃中: Option<String> = None;

    while i < chars.len() {
        if chars[i] != '<' {
            if 丢弃中.is_none() {
                out.push(chars[i]);
            }
            i += 1;
            continue;
        }
        // 注释整段跳过。
        if chars[i..].starts_with(&['<', '!', '-', '-']) {
            match 找(&chars, i, "-->") {
                Some(end) => i = end + 3,
                None => break,
            }
            continue;
        }
        let Some(end) = chars[i..].iter().position(|c| *c == '>').map(|p| i + p) else {
            break;
        };
        let tag: String = chars[i + 1..end].iter().collect();
        let 闭合 = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        match &丢弃中 {
            Some(open) if 闭合 && *open == name => 丢弃中 = None,
            Some(_) => {}
            None if !闭合 && 丢弃.contains(&name.as_str()) => {
                // 自闭合的 `<svg/>` 不该把后面整篇都吃掉。
                if !tag.trim_end().ends_with('/') {
                    丢弃中 = Some(name.clone());
                }
            }
            None if 块级.contains(&name.as_str()) => out.push('\n'),
            None => {}
        }
        i = end + 1;
    }
    折叠(&解实体(&out))
}

fn 找(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    let n: Vec<char> = needle.chars().collect();
    (from..chars.len().saturating_sub(n.len() - 1)).find(|&k| chars[k..k + n.len()] == n[..])
}

/// 解掉常见实体。**不全，而且不必全**：漏掉的会原样留在文本里，
/// 模型读得懂 `&hellip;`，读不懂一段被解错的乱码。
fn 解实体(text: &str) -> String {
    let out = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…");
    // `&amp;` 最后解：先解它的话，`&amp;lt;` 会被两次解成 `<`。
    out.replace("&amp;", "&")
}

/// 折叠空白：行内连续空白压成一个空格，空行全部去掉。
///
/// 空行全去而不是"压成一个"：块级标签的开与闭各出一个换行，`<p>a</p><p>b</p>`
/// 于是天然带一个空行。留着的话，一篇正常的网页抽出来每段之间都空一行，
/// 白白占掉一半上下文，而段落边界靠单个换行已经说清楚了。
fn 折叠(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;
    use agentrs_contracts::sandbox::ExecutionOutcome;

    #[test]
    fn 脚本与样式整段丢掉() {
        let html = "<html><head><title>T</title></head><body>\
                    <script>var x = '正文里不该出现这个';</script>\
                    <style>body{color:red}</style>\
                    <p>真正的正文</p></body></html>";
        assert_eq!(html_to_text(html), "真正的正文");
    }

    #[test]
    fn 块级标签变成换行() {
        assert_eq!(
            html_to_text("<p>一</p><p>二</p><ul><li>甲</li><li>乙</li></ul>"),
            "一\n二\n甲\n乙"
        );
    }

    #[test]
    fn 实体被解开且_amp_不会二次解() {
        assert_eq!(html_to_text("<p>a &lt; b &amp;&amp; c</p>"), "a < b && c");
        // 先解 &amp; 的话这里会变成 `<`，把一段展示用的文本解成标签。
        assert_eq!(html_to_text("<p>&amp;lt;</p>"), "&lt;");
        assert_eq!(html_to_text("<p>a&nbsp;b</p>"), "a b");
    }

    #[test]
    fn 空白被折叠() {
        assert_eq!(html_to_text("<p>  a     b  </p>\n\n\n<p>c</p>"), "a b\nc");
    }

    #[test]
    fn 注释与属性不进正文() {
        assert_eq!(
            html_to_text(r#"<!-- 注释 --><a href="http://x" title="标题">链接</a>"#),
            "链接"
        );
    }

    #[test]
    fn 自闭合的丢弃标签不会吃掉后文() {
        // `<svg/>` 之后没有 `</svg>`；把它当成开标签的实现会把整篇都当成
        // svg 内容丢掉。
        assert_eq!(html_to_text("<svg/><p>后文还在</p>"), "后文还在");
    }

    #[test]
    fn 未闭合的标签不会崩() {
        assert_eq!(html_to_text("<p>a"), "a");
        assert_eq!(html_to_text("<script>x"), "");
        assert_eq!(html_to_text("<!-- 没闭合"), "");
        assert_eq!(html_to_text(""), "");
    }

    #[test]
    fn 超时参数被夹进合法区间() {
        assert_eq!(timeout_of(None), Duration::from_millis(DEFAULT_TIMEOUT_MS));
        assert_eq!(timeout_of(Some(0)), Duration::from_millis(1));
        assert_eq!(timeout_of(Some(u64::MAX)), Duration::from_millis(MAX_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn 宿主没给_http_时明说() {
        let files = FakeWorkspace::new([]);
        let out = run(
            &ToolCtx::files_only(&files),
            &json!({"url": "https://example.com/"}),
        )
        .await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 1 });
        assert!(out.text.unwrap().contains("出站网络"));
    }

    #[tokio::test]
    async fn 缺参数是参数问题() {
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 2 });
        assert!(out.text.unwrap().contains("url"));
    }
}
