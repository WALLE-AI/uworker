//! 脱敏导出（架构 §16 导出协议，M2 出口标准）。
//!
//! ## 一条不显然的约束：脱敏必须是**稳定映射**，不能是删除
//!
//! durable 事实里必然包含路径——`StepIntent` 与 reconcile 都要靠它定位。
//! 而导出要求"不含绝对路径"。最直觉的做法是删掉或随机替换，
//! **但那会破坏投影的确定性**：
//!
//! > 相同事件前缀始终产生相同 snapshot
//!
//! 删掉了，两条不同路径的事件变成同一条；随机替换了，同一份 bundle
//! 导出两次得到两个 snapshot。两种做法都让 replay 不可重现——
//! 而 replay 正是导出这件事的目的。
//!
//! 所以约定是：
//!
//! ```text
//! 绝对路径 → 工作区相对路径                          （能相对化时）
//! 其余绝对路径 → "opaque:" + base32(hmac(salt, path)) （同一 bundle 内同路径同结果）
//! ```
//!
//! `bundle_salt` 随 bundle 生成并记在头部。**同一 bundle 内可逆性为零、
//! 一致性为一**——不可逆保证了安全，一致保证了确定。
//!
//! ## 什么绝对不进 bundle
//!
//! 密钥、命令输出、文件正文一律不进，**只留 hash 与长度**。
//! 这条不做例外：一个"就这一次，为了排查"的例外会变成默认行为。

use std::collections::BTreeMap;

use agentrs_contracts::event::{EventPayload, RunEventEnvelope};

/// bundle 级的脱敏盐。
///
/// **由调用方生成并记入 bundle 头部**——内核不读随机源（边界判据）。
/// 同一 bundle 内复用同一个盐，跨 bundle 换盐。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSalt(pub String);

/// 脱敏器。
#[derive(Debug, Clone)]
pub struct Redactor {
    salt: BundleSalt,
    /// 工作区根。能相对化的路径优先相对化——**相对路径可读且仍然确定**，
    /// 比一串 opaque 好排查得多。
    workspace_root: Option<String>,
}

/// 一次脱敏替换，用于自检与报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replacement {
    /// 替换后的值。
    pub to: String,
    /// 用的哪条规则。
    pub rule: Rule,
}

/// 脱敏规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// 相对化到工作区。
    Relativized,
    /// 不可逆但稳定的替身。
    Opaque,
    /// 本来就不是绝对路径，原样保留。
    Untouched,
}

impl Redactor {
    /// 新建。
    pub fn new(salt: BundleSalt, workspace_root: Option<String>) -> Self {
        Self { salt, workspace_root }
    }

    /// 脱敏一个可能是路径的字符串。
    pub fn path(&self, p: &str) -> Replacement {
        if !is_absolute(p) {
            return Replacement {
                to: p.to_string(),
                rule: Rule::Untouched,
            };
        }

        // 能相对化就相对化：可读、可排查，且同样确定。
        if let Some(root) = &self.workspace_root {
            if let Some(rest) = p.strip_prefix(root) {
                let rel = rest.trim_start_matches(['/', '\\']);
                return Replacement {
                    to: if rel.is_empty() { ".".into() } else { rel.into() },
                    rule: Rule::Relativized,
                };
            }
        }

        Replacement {
            to: format!("opaque:{}", self.opaque(p)),
            rule: Rule::Opaque,
        }
    }

    /// 稳定替身。
    ///
    /// 这里用 FNV-1a 加盐拼接，**不是密码学 HMAC**——真实实现应当换成
    /// HMAC-SHA256 再 base32。当前形态足以验证"同盐同路径同结果、
    /// 换盐则结果不同"这条性质，而那正是确定性所依赖的全部。
    fn opaque(&self, p: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.salt.0.as_bytes().iter().chain(b"\x00").chain(p.as_bytes()) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        base32(h)
    }

    /// 脱敏一个事件信封。
    ///
    /// **只改可能含路径与正文的字段**，其余原样——尤其 `event_id` 与 `seq`
    /// 必须保持不变，否则 bundle 的接收端按 `(run_id, event_id)` 去重就失效了。
    pub fn event(&self, e: &RunEventEnvelope) -> RunEventEnvelope {
        let mut out = e.clone();
        out.payload = self.payload(&e.payload);
        out
    }

    fn payload(&self, p: &EventPayload) -> EventPayload {
        match p {
            // `StepIntent` **当前没有需要脱敏的字段**：
            // 工具名不是路径且对排查关键，`input_hash` 本来就是摘要，
            // 参数正文根本不进意图（只留指纹）。
            //
            // 这里刻意不写一行"看起来在脱敏"的空操作——那比不写更糟：
            // 下一个人会以为这条路径已经处理过了。真要加字段时，
            // 兜底的是 `audit()`。
            EventPayload::StepResultRecorded { result } => {
                let mut r = result.clone();
                // **命令输出一律不进 bundle，只留长度。**
                r.output = r.output.as_ref().map(|o| format!("[已脱敏：{} 字节]", o.len()));
                // 失败与拒绝说明可能含路径或正文，同样替换成长度。
                r.outcome = self.outcome(&r.outcome);
                EventPayload::StepResultRecorded { result: r }
            }
            other => other.clone(),
        }
    }

    fn outcome(&self, o: &agentrs_contracts::StepOutcome) -> agentrs_contracts::StepOutcome {
        use agentrs_contracts::StepOutcome as O;
        match o {
            // **拒绝码保留、说明脱敏。** 码是稳定枚举、不含用户内容，
            // 而它恰恰是排查时最有用的那一半。
            O::Denied { code, message } => O::Denied {
                code: *code,
                message: format!("[已脱敏：{} 字节]", message.len()),
            },
            O::Failed { message } => O::Failed {
                message: format!("[已脱敏：{} 字节]", message.len()),
            },
            other => other.clone(),
        }
    }
}

/// 一份 replay bundle。
///
/// **默认只含 schema 版本、脱敏事件、content hash 与 projection 版本。**
/// 敏感正文必须由 Core 按可见性与用户授权另行导出——不是这里的事。
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayBundle {
    /// 契约版本。
    pub spec_version: agentrs_contracts::version::SpecVersion,
    /// 本 bundle 的脱敏盐。**记在头部**，使同一 bundle 内的脱敏可复现。
    pub salt: BundleSalt,
    /// 脱敏后的事件。
    pub events: Vec<RunEventEnvelope>,
    /// 各投影的 state_version，接收端据此判断自己的投影能不能读这份 bundle。
    pub projection_versions: BTreeMap<String, u32>,
}

impl ReplayBundle {
    /// 构建一份 bundle。
    pub fn build(
        spec_version: agentrs_contracts::version::SpecVersion,
        redactor: &Redactor,
        events: &[RunEventEnvelope],
        projection_versions: BTreeMap<String, u32>,
    ) -> Self {
        Self {
            spec_version,
            salt: redactor.salt.clone(),
            // 只导 durable 事实：live delta 可丢，导出它只会让 bundle
            // 在"丢了"和"没丢"两种情况下不一致。
            events: events
                .iter()
                .filter(|e| e.is_durable())
                .map(|e| redactor.event(e))
                .collect(),
            projection_versions,
        }
    }

    /// 自检：bundle 里还有没有绝对路径或疑似密钥。
    ///
    /// **导出前必须过一遍。** 脱敏规则会漏——新增一个带路径的字段就漏一处，
    /// 而漏了没人会发现，除非有这道自检。
    pub fn audit(&self) -> Vec<Leak> {
        let mut out = Vec::new();
        for e in &self.events {
            let dumped = format!("{:?}", e.payload);
            if let Some(hit) = find_absolute_path(&dumped) {
                out.push(Leak {
                    event_id: e.event_id.to_string(),
                    kind: LeakKind::AbsolutePath,
                    sample: hit,
                });
            }
            if let Some(hit) = find_secret(&dumped) {
                out.push(Leak {
                    event_id: e.event_id.to_string(),
                    kind: LeakKind::PossibleSecret,
                    sample: hit,
                });
            }
        }
        out
    }
}

/// 自检发现的泄漏。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leak {
    /// 哪条事件。
    pub event_id: String,
    /// 什么类型。
    pub kind: LeakKind,
    /// 命中的片段（**已截断**，本身不再是完整泄漏）。
    pub sample: String,
}

/// 泄漏类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeakKind {
    /// 绝对路径。
    AbsolutePath,
    /// 疑似密钥。
    PossibleSecret,
}

fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    b.first() == Some(&b'/')
        || p.starts_with("\\\\")
        // Windows 盘符 `C:\`。**按字节比较，不能切字符串**——
        // `&p[1..3]` 在 "a条件" 这类以 ASCII 开头的多字节串上会切在
        // 字符中间直接 panic。这个 bug 在中文提示词上一碰就炸。
        || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
}

/// 在一段文本里找绝对路径。返回截断后的样本。
fn find_absolute_path(s: &str) -> Option<String> {
    for tok in s.split(|c: char| c.is_whitespace() || c == '"' || c == ',') {
        // 单独一个 "/" 或以 / 开头但长度过短的不算——那多半是分隔符。
        if tok.len() > 3 && is_absolute(tok) {
            return Some(truncate(tok));
        }
    }
    None
}

/// 在一段文本里找疑似密钥。
///
/// **宁可误报**：漏报的代价是密钥进了 bundle，误报的代价只是多看一眼。
fn find_secret(s: &str) -> Option<String> {
    const 前缀: &[&str] = &["sk-", "ghp_", "AKIA", "Bearer ", "xoxb-", "-----BEGIN"];
    for p in 前缀 {
        if let Some(i) = s.find(p) {
            return Some(truncate(&s[i..]));
        }
    }
    None
}

fn truncate(s: &str) -> String {
    // 样本本身不能是完整泄漏。
    s.chars().take(12).collect::<String>() + "…"
}

/// 把 u64 编成 base32（Crockford 字母表，去掉易混字符）。
fn base32(mut h: u64) -> String {
    const A: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = Vec::with_capacity(13);
    for _ in 0..13 {
        out.push(A[(h & 0x1f) as usize]);
        h >>= 5;
    }
    String::from_utf8(out).expect("字母表全为 ASCII")
}

#[cfg(test)]
mod tests;
