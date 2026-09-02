//! SSRF 判定：一个 URL 该不该去取。
//!
//! # 这是 `WebFetch` 的主要风险，不是附带条款
//!
//! 一个能取任意 URL 的工具，参数由**模型**填。模型读过的每一段网页、每一个仓库
//! 里的 README 都可能在教它去取 `http://169.254.169.254/`（云上的元数据服务，
//! 那里有临时凭据）或者 `http://127.0.0.1:19121/`（本机的模型端点）。工具本身
//! 忠实地执行，然后把内容交回模型——一次完整的读取，全程没有任何一步"出错"。
//!
//! # 判定在这边，解析在端口那边
//!
//! **哪些地址不许去**是策略，属于这里；**这个域名解析成什么**是机制，
//! 走 [`super::Http::resolve`]。反过来做的话，一个换了 adapter 的宿主就能悄悄
//! 换掉安全判定。
//!
//! # 两层，因为第二层不是到处都有
//!
//! 不依赖 DNS 的一层（协议、字面 IP、内部主机名表）任何环境下都成立；
//! 依赖 DNS 的一层（解析出的每个地址 + 钉住连接）只在直连时可用。
//! 走代理时**只剩第一层**，见 [`Refusal`] 的文档。

use std::net::IpAddr;

/// 最多跟随几跳重定向。
pub const MAX_HOPS: usize = 5;

/// 一个 URL 为什么不许取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// URL 语法不对。
    Malformed(String),
    /// 协议不在允许表内。
    Scheme(String),
    /// 域名解析不出来。
    Unresolvable(String),
    /// 解析结果指向内网、本机或元数据地址。
    Internal(String),
    /// 重定向跳太多次。
    TooManyHops,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(u) => write!(f, "URL 无法解析：{u}"),
            Self::Scheme(s) => write!(f, "只支持 http 与 https，不支持 {s}"),
            Self::Unresolvable(h) => write!(f, "域名解析不出来：{h}"),
            Self::Internal(a) => write!(
                f,
                "{a} 指向本机或内网。这类地址上常有凭据服务与内部接口，\
                 取回来的内容会直接进入我的上下文，所以一律不取"
            ),
            Self::TooManyHops => write!(f, "重定向超过 {MAX_HOPS} 跳"),
        }
    }
}

/// 这个 IP 是内部地址吗？
///
/// **正面列举全部不许去的段**，而不是"排除掉几个明显的"。漏一个的代价是
/// 一次凭据泄露；多拦一个的代价是一次取不到公网上某个冷门地址。
pub fn internal_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()                       // 127/8
                || v4.is_private()                 // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local()              // 169.254/16 —— 云元数据服务在这里
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()             // 0.0.0.0
                || v4.is_multicast()
                || o[0] == 100 && (64..128).contains(&o[1])  // 100.64/10 运营商级 NAT
                || o[0] == 198 && (o[1] == 18 || o[1] == 19) // 198.18/15 基准测试
                || o[0] >= 240                     // 240/4 保留
        }
        IpAddr::V6(v6) => {
            // IPv4 映射地址（::ffff:127.0.0.1）必须按它内含的 v4 判——
            // 不展开的话，这一条就是绕过上面全部规则的现成后门。
            if let Some(v4) = v6.to_ipv4_mapped() {
                return internal_address(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || seg[0] & 0xfe00 == 0xfc00       // fc00::/7 唯一本地
                || seg[0] & 0xffc0 == 0xfe80       // fe80::/10 链路本地
        }
    }
}

/// 一望即知是内部的主机名。
///
/// 这一层**不依赖 DNS**，所以在解析不出来的环境（比如只有 HTTP 代理、没有直连
/// DNS 的机器）里照样有效。`metadata.google.internal` 是这里最要紧的一条：
/// 它和 `169.254.169.254` 是同一个东西的两个名字，只拦 IP 会漏掉它。
pub fn internal_host(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h.ends_with(".local")
        || h.ends_with(".internal")
        || h.ends_with(".localdomain")
        || h.ends_with(".home.arpa")
}

/// 一个 URL 里 DNS 之前就能判的部分。
///
/// 返回主机名与端口，供调用方去解析。
pub fn vet_without_dns(raw: &str) -> Result<(url::Url, String, u16), Refusal> {
    let parsed = url::Url::parse(raw).map_err(|_| Refusal::Malformed(raw.to_string()))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(Refusal::Scheme(other.to_string())),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| Refusal::Malformed(raw.to_string()))?
        .to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| Refusal::Malformed(raw.to_string()))?;

    if internal_host(&host) {
        return Err(Refusal::Internal(host));
    }
    // URL 里直接写死的 IP：`[::1]` 会被 url crate 带上方括号，先剥掉。
    if let Ok(ip) = host.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>() {
        if internal_address(ip) {
            return Err(Refusal::Internal(ip.to_string()));
        }
    }
    Ok((parsed, host, port))
}

/// 判定一组解析结果。
///
/// **全部**地址都得干净。一个域名同时解析出公网与 127.0.0.1 时，
/// 只看第一个就等于让对方决定我们看哪个。
pub fn vet_addresses(addrs: &[std::net::SocketAddr]) -> Result<(), Refusal> {
    for a in addrs {
        if internal_address(a.ip()) {
            return Err(Refusal::Internal(a.ip().to_string()));
        }
    }
    Ok(())
}

/// 把一个 `Location` 头解成下一跳的绝对 URL。
///
/// 单独一个函数，是为了让"重定向换靶"这条**可测**。整条重定向路径没法在单测里
/// 走一遍——起一个本地服务器当跳板，第一跳就会被判定挡住（那正是它该做的事）。
/// 所以拆成两半各自可测：这里保证下一跳算得对，[`vet_without_dns`] 保证算出来的
/// 东西会被重新审一遍。两半合起来就是"每一跳都重判"。
///
/// 相对 `Location` 必须按**当前**这一跳展开，不是按最初那个 URL——跳过两次之后
/// 基地址早就变了，用错基地址会算出一个谁也没审过的地址。
pub fn next_hop(current: &url::Url, location: &str) -> Result<String, Refusal> {
    current
        .join(location)
        .map(|u| u.to_string())
        .map_err(|_| Refusal::Malformed(location.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn addr(s: &str) -> std::net::SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn 每一类内部地址都拦得住() {
        for a in [
            "127.0.0.1",     // 本机
            "127.53.1.9",    // 整个 127/8，不只是 .0.1
            "0.0.0.0",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254", // 云元数据服务，SSRF 的头号目标
            "100.64.0.1",
            "198.18.0.1",
            "255.255.255.255",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "::ffff:127.0.0.1", // v4 映射，不展开就是现成的后门
        ] {
            assert!(internal_address(ip(a)), "{a} 没拦住");
        }
    }

    #[test]
    fn 公网地址不误伤() {
        for a in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "172.32.0.1", // 172.16/12 的**外面**，按 172.x 一刀切会误伤
            "172.15.0.1",
            "100.128.0.1", // 100.64/10 的外面
            "198.20.0.1",
            "2606:4700::1111",
        ] {
            assert!(!internal_address(ip(a)), "{a} 被误伤");
        }
    }

    #[test]
    fn 内部主机名不靠_dns_也拦得住() {
        // 这一层是为解析不出来的环境准备的（只有 HTTP 代理、没有直连 DNS 的
        // 机器就是那样）。`metadata.google.internal` 与 169.254.169.254 是同一个
        // 东西的两个名字——只拦 IP 会整个漏掉它。
        for h in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "foo.localhost",
            "metadata.google.internal",
            "db.internal",
            "printer.local",
            "box.localdomain",
            "gw.home.arpa",
        ] {
            assert!(internal_host(h), "{h} 没拦住");
        }
    }

    #[test]
    fn 正常域名不被主机名规则误伤() {
        for h in [
            "example.com",
            "docs.rs",
            "internal-affairs.gov", // 以 internal 开头不是以 .internal 结尾
            "mylocalhost.com",
            "localhosting.io",
        ] {
            assert!(!internal_host(h), "{h} 被误伤");
        }
    }

    #[test]
    fn 非_http_协议被拒() {
        for u in ["file:///etc/passwd", "ftp://x/y", "gopher://x", "data:text/html,x"] {
            let e = vet_without_dns(u).unwrap_err();
            assert!(
                matches!(e, Refusal::Scheme(_) | Refusal::Malformed(_)),
                "{u}: {e:?}"
            );
        }
    }

    #[test]
    fn 直接写本机地址被拒() {
        for u in [
            "http://127.0.0.1:19121/starvlm/v1",
            "http://localhost:8080/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]:80/",
            "http://metadata.google.internal/",
        ] {
            let e = vet_without_dns(u).unwrap_err();
            assert!(matches!(e, Refusal::Internal(_)), "{u}: {e:?}");
        }
    }

    #[test]
    fn 畸形_url_被拒而不是被当成相对路径() {
        for u in ["", "не url", "http://", "://x"] {
            assert!(vet_without_dns(u).is_err(), "{u}");
        }
    }

    #[test]
    fn 公网_url_过得去并带回主机与端口() {
        let (_, host, port) = vet_without_dns("https://example.com/a").unwrap();
        assert_eq!((host.as_str(), port), ("example.com", 443));
        let (_, _, port) = vet_without_dns("http://example.com:8080/").unwrap();
        assert_eq!(port, 8080);
    }

    #[test]
    fn 一组地址里有一个脏的就整体拒绝() {
        // 只看第一个就等于让对方决定我们看哪个。
        assert!(vet_addresses(&[addr("93.184.216.34:80")]).is_ok());
        assert!(vet_addresses(&[addr("93.184.216.34:80"), addr("127.0.0.1:80")]).is_err());
    }

    #[test]
    fn 重定向换靶到内网会在下一跳被拦住() {
        // 这是 SSRF 里最容易漏的一种：第一跳看起来完全正常（公网、https、
        // 域名也人畜无害），302 之后才换成 127.0.0.1。只判首跳的实现会放它过去。
        let 首跳 = url::Url::parse("https://example.com/start").unwrap();
        for location in [
            "http://127.0.0.1:19121/starvlm/v1/models", // 绝对地址换靶
            "//169.254.169.254/latest/meta-data/",      // 协议相对，最容易被忽略
            "http://metadata.google.internal/",         // 换成名字，DNS 拦不住
        ] {
            let 下一个 = next_hop(&首跳, location).expect("算得出下一跳");
            let e = vet_without_dns(&下一个).unwrap_err();
            assert!(matches!(e, Refusal::Internal(_)), "{location} → {下一个}: {e:?}");
        }
    }

    #[test]
    fn 换协议的重定向也被拦住() {
        let 首跳 = url::Url::parse("https://example.com/x").unwrap();
        let 下一个 = next_hop(&首跳, "file:///etc/passwd").unwrap();
        assert!(matches!(vet_without_dns(&下一个).unwrap_err(), Refusal::Scheme(_)));
    }

    #[test]
    fn 相对_location_按当前这一跳展开() {
        // 跳过两次之后基地址早就变了。用最初那个 URL 当基地址，会算出一个
        // 谁也没审过的地址——而它随后**会**被送去连接。
        let 第二跳 = url::Url::parse("https://cdn.example.net/a/b").unwrap();
        assert_eq!(next_hop(&第二跳, "/c").unwrap(), "https://cdn.example.net/c");
        assert_eq!(next_hop(&第二跳, "d").unwrap(), "https://cdn.example.net/a/d");
    }

    #[test]
    fn 拒绝理由说清楚为什么() {
        // 这段文本会回灌进模型上下文。只说"被拒绝"的话，模型会换个写法再试。
        let why = Refusal::Internal("127.0.0.1".into()).to_string();
        assert!(why.contains("本机或内网"), "{why}");
        assert!(why.contains("凭据"), "{why}");
    }
}
