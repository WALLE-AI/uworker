//! 内置工具：声明、编排、措辞。
//!
//! # 为什么工具住在内核侧的 crate 里
//!
//! 一个工具由三部分组成：**它长什么样**（名字、描述、schema）、**它做什么**
//! （先读再翻页再措辞）、以及**它怎么碰 OS**（`read(2)`）。前两样是这个项目最该
//! 攥在手里的东西——描述写坏了模型就选错工具，措辞写坏了模型读完不知道怎么改正。
//! 只有第三样是执行权。
//!
//! 所以本模块放前两样，第三样经 [`WorkspaceIo`] / [`Shell`] / [`Http`] 三个端口
//! 注入。内核"无执行权"这条一字未改：这里没有一行 `std::fs`、`Command` 或
//! `reqwest`，两条门禁脚本照样管着它。
//!
//! 从前这些散在 `agentrs-dev-adapter` 里——声明在 `catalog.rs`、编排在
//! `sandbox.rs` 的一个 match、判定在 `exec_guard.rs`。那不是设计，是上一轮加工具时
//! 顺着已有位置长出来的。副作用是想知道"Read 到底做什么"得读三个文件，
//! 而且每条工具语义测试都要先建一个临时目录。
//!
//! # 判定留在这边，机制注入
//!
//! 这是全模块的关键。以 SSRF 为例：**哪些地址不许去**是策略，留在
//! [`net_guard`]；**这个域名解析成什么**是机制，走 [`Http::resolve`]。
//! 反过来做的话，一个换了 adapter 的宿主就能悄悄换掉安全判定。
//!
//! 同理，cwd 围栏**不在这里**：它是 L0 的一条，属宿主义务（H2），
//! 由 [`WorkspaceIo`] 的实现方负责，本模块只认它回的
//! [`IoError::OutsideWorkspace`]。

use std::net::SocketAddr;
use std::time::Duration;

use agentrs_contracts::sandbox::{ExecutionOutcome, RejectReason};
use agentrs_types::ToolDef;
use async_trait::async_trait;
use serde_json::Value;

pub mod bash;
pub mod delete;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod net_guard;
pub mod read;
pub mod shape_guard;
pub mod text;
pub mod web_fetch;
pub mod web_search;
pub mod write;

/// 文件访问失败的三种形状。
///
/// `OutsideWorkspace` 与 `NotFound` 分开，是因为它们**该让模型做不同的事**：
/// 前者是"换个路径"，后者是"先确认文件在不在"。并成一个 `Other` 会把这个区别
/// 抹掉，而模型只能靠这段文字决定下一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoError {
    /// 目标落在工作区之外（cwd 围栏拦下）。
    OutsideWorkspace,
    /// 文件不存在，或在本 ChangeSet 里已被删除。
    NotFound,
    /// 其他已脱敏的失败。
    Other(String),
}

/// 工作区文件访问。
///
/// **实现方负责两件本模块管不了的事**：cwd 围栏（L0 的一条，H2），
/// 以及 ChangeSet overlay 的读己之写一致性（H7）——包括删除的墓碑必须
/// 表现得和"文件不存在"完全一样，否则删除只是看起来生效了。
#[async_trait]
pub trait WorkspaceIo: Send + Sync {
    /// 读一个文件。overlay 优先于磁盘。
    async fn read(&self, path: &str) -> Result<String, IoError>;
    /// 写一个文件。**只进 ChangeSet，不落盘**——提交是宿主的显式动作。
    async fn write(&self, path: &str, content: String) -> Result<(), IoError>;
    /// 删一个文件（在 overlay 里立墓碑）。
    async fn delete(&self, path: &str) -> Result<(), IoError>;
    /// 本 ChangeSet 视角下的全部文件，工作区相对路径，**已排序**。
    ///
    /// 排序由实现方保证：`Glob` 与 `Grep` 都走它，两者对"有哪些文件"的看法
    /// 不该分叉，输出顺序也不该随目录遍历的偶然次序变。
    async fn list(&self) -> Result<Vec<String>, IoError>;
}

/// 一条命令跑完之后的样子。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellOutcome {
    /// 进程压根没起来。
    ///
    /// 与"跑了但失败了"分开：前者没有任何副作用，后者可能已经改了盘。
    Spawn(String),
    /// 跑完了或被强杀了。
    Settled {
        /// 是否被超时强杀。
        timed_out: bool,
        /// 退出码；被信号杀死时为 -1。
        exit_code: i32,
        /// 标准输出的原始字节。
        stdout: Vec<u8>,
        /// 标准错误的原始字节。**分开交回**，措辞由 [`bash`] 决定。
        stderr: Vec<u8>,
    },
}

/// 受围栏的命令执行。
#[async_trait]
pub trait Shell: Send + Sync {
    /// 围栏能力自检。
    ///
    /// `Err` 里是**缺失的条目**，不是一句"不支持"——那句话对排查的人毫无用处，
    /// 他需要知道是内核关了用户命名空间还是压根没装 `unshare`。
    async fn containment(&self) -> Result<(), Vec<String>>;

    /// 在围栏内跑一条 shell 命令。
    async fn run(&self, command: &str, timeout: Duration) -> ShellOutcome;
}

/// 一次 GET 的参数。
#[derive(Debug, Clone)]
pub struct HttpGet<'a> {
    /// 目标 URL，**已经过 [`net_guard`] 判定**。
    pub url: &'a str,
    /// `Accept` 头。
    pub accept: &'a str,
    /// 把连接钉在这个地址上（防 DNS rebinding）；`None` 表示交给实现方解析。
    pub pin: Option<SocketAddr>,
    /// 超时。
    pub timeout: Duration,
    /// 响应体字节上限。**边读边数，不是先收完再判**——先收完等于让对方决定
    /// 我们分配多少内存。
    pub max_bytes: usize,
}

/// 一次 GET 的结果。
///
/// 带着 `status` 与 `location` 而不是替调用方跟随重定向：**逐跳重判**是
/// [`web_fetch`] 最要紧的一段逻辑，它必须留在判定这一侧。
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// `Location` 头（若有）。
    pub location: Option<String>,
    /// `Content-Type` 头，取不到时为空串。
    pub content_type: String,
    /// 响应体，已按 `max_bytes` 截断。
    pub body: Vec<u8>,
}

/// 出站 HTTP。
///
/// **SSRF 判定不在这里。** 它在 [`net_guard`]，由 [`web_fetch`] 在每次调用
/// （含每一跳重定向）之前做完。本端口只提供两样机制：解析域名、发请求。
#[async_trait]
pub trait Http: Send + Sync {
    /// 解析一个域名。`Err(())` 表示本机解析不出来。
    ///
    /// 判定要它：解析得到的每一个地址都得干净。一个域名同时解析出公网与
    /// 127.0.0.1 时，只看第一个就等于让对方决定我们看哪个。
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, ()>;

    /// 这个 URL 会走代理吗？
    ///
    /// 走代理时解析是代理做的，我们既钉不住也看不见结果，本机甚至可能没有直连
    /// DNS。判定据此决定"解析不出来"算异常还是算常态。
    fn proxied(&self, url: &str) -> bool;

    /// 发一次 GET。**不跟随重定向**。
    async fn get(&self, req: HttpGet<'_>) -> Result<HttpResponse, String>;

    /// 向配置好的搜索端点发一次带鉴权的 POST。
    ///
    /// `key` 只往请求头去。它不进日志、不进 trajectory、不进 TUI 状态。
    async fn post_json(&self, url: &str, key: &str, body: Value) -> Result<Value, String>;
}

/// 搜索端点。宿主从环境读出来后注入。
#[derive(Debug, Clone)]
pub struct SearchEndpoint {
    /// 端点 URL。
    pub url: String,
    /// API key。
    pub key: String,
}

impl std::fmt::Display for SearchEndpoint {
    /// 只印端点，**永不印 key**。
    ///
    /// 这个类型会出现在诊断输出里，而默认的 `Debug` 会把 key 一起印出来——
    /// 所以 `Debug` 之外必须有这一个，且它是唯一该被用来展示的形式。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SearchEndpoint({})", self.url)
    }
}

/// 一次工具执行的结果。
///
/// `outcome` 与 `text` 分开：前者是稳定码（管线据此决定 Failed/Denied/Rejected），
/// 后者是**工具自己的话**。两者都要送到模型面前——只给码，模型只能原样重试同一个
/// 错误；只给话，管线无从判断这次算不算成功。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// 稳定结局。
    pub outcome: ExecutionOutcome,
    /// 已脱敏的输出或说明。
    pub text: Option<String>,
}

impl ToolOutput {
    /// 成功，带一段输出。
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            outcome: ExecutionOutcome::Completed { exit_code: 0 },
            text: Some(text.into()),
        }
    }

    /// 工具自己判定的失败。
    ///
    /// **不是 `Rejected`。** `Rejected` 是授权层的话，模型看到会停下；
    /// 失败是工具的话，模型读得懂，会改用别的办法。一个模型选错了参数，
    /// 不该让整个 Run 看起来像撞上了权限墙。
    pub fn failed(text: impl Into<String>) -> Self {
        Self {
            outcome: ExecutionOutcome::Completed { exit_code: 1 },
            text: Some(text.into()),
        }
    }

    /// 参数问题。
    ///
    /// 单独一个退出码，因为它**不该发生**——固定管线的 schema 校验先于执行拦掉了
    /// 畸形调用。留着是兜底，而且它必须说清楚是参数问题：从前这类失败伪装成
    /// `ChangeSetUnavailable`，排查的人会去查变更集，而那里什么问题也没有。
    pub fn bad_args(name: &str) -> Self {
        Self {
            outcome: ExecutionOutcome::Completed { exit_code: 2 },
            text: Some(format!("缺少参数 {name}")),
        }
    }

    /// 执行器层面的拒绝。
    pub fn rejected(reason: RejectReason, text: impl Into<String>) -> Self {
        Self {
            outcome: ExecutionOutcome::Rejected { reason },
            text: Some(text.into()),
        }
    }
}

impl From<IoError> for ToolOutput {
    fn from(e: IoError) -> Self {
        match e {
            IoError::OutsideWorkspace => Self {
                outcome: ExecutionOutcome::Rejected {
                    reason: RejectReason::OutsideWorkspace,
                },
                text: None,
            },
            IoError::NotFound => Self::failed("文件不存在"),
            IoError::Other(why) => Self::failed(format!("读写失败：{why}")),
        }
    }
}

/// 宿主交给本次执行的能力。
///
/// `shell` / `http` / `search` 是 `Option`：宿主可能一个也不提供（比如一个只做
/// 代码审阅的嵌入方）。目录由同一组能力推出来，所以"声明了却跑不动的工具"
/// 在构造上不可能存在。
pub struct ToolCtx<'a> {
    /// 工作区文件访问。**必需**——没有它连 Read 都没有。
    pub files: &'a dyn WorkspaceIo,
    /// 命令执行。
    pub shell: Option<&'a dyn Shell>,
    /// 出站 HTTP。
    pub http: Option<&'a dyn Http>,
    /// 搜索端点。`http` 为 `None` 时它没有意义。
    pub search: Option<&'a SearchEndpoint>,
}

impl<'a> ToolCtx<'a> {
    /// 只有文件能力。
    pub fn files_only(files: &'a dyn WorkspaceIo) -> Self {
        Self {
            files,
            shell: None,
            http: None,
            search: None,
        }
    }

    /// 本上下文实际具备的能力，用来推目录。
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            shell: self.shell.is_some(),
            http: self.http.is_some(),
            search: self.http.is_some() && self.search.is_some(),
        }
    }
}

/// 宿主提供了哪些能力。目录据此裁剪。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// 能不能跑命令。
    pub shell: bool,
    /// 能不能取网页。
    pub http: bool,
    /// 搜索端点配好了没有。
    ///
    /// **没有免密的网页搜索**——参考实现用的 Exa / Parallel 都要 key。
    /// 没配就根本不进目录：注册一个必定失败的工具比没有它更糟，
    /// 模型会反复试、换着法子重写查询，把一整个 Run 耗在一个不通的出口上。
    pub search: bool,
}

impl Capabilities {
    /// 只有文件工具。
    pub const FILES_ONLY: Self = Self {
        shell: false,
        http: false,
        search: false,
    };

    /// 全都有。
    pub const ALL: Self = Self {
        shell: true,
        http: true,
        search: true,
    };
}

/// 本次可用的工具目录。
///
/// 顺序即模型看到的顺序：发现（Glob/Grep）在前，读在中间，改在后面，
/// **Bash 在最后**——模型按顺序读目录，先看到专用工具才不会一上来就用
/// `cat`/`sed` 去做 Read/Edit 的事。那样每一步都要人审批，而且改动不进
/// ChangeSet，人也就无从在提交前过目。
pub fn catalog(caps: Capabilities) -> Vec<ToolDef> {
    let mut out = vec![
        glob::def(),
        grep::def(),
        read::def(),
        write::def(),
        edit::def(),
        delete::def(),
    ];
    if caps.http {
        out.push(web_fetch::def());
    }
    if caps.shell {
        out.push(bash::def());
    }
    if caps.search {
        out.push(web_search::def());
    }
    out
}

/// 目录里全部工具的名字。宿主构造 `RunSpec` 的授权与能力时用它。
pub fn tool_names(caps: Capabilities) -> Vec<String> {
    catalog(caps).into_iter().map(|tool| tool.name).collect()
}

/// 每次调用都要人点头的工具。
///
/// **判据是"后果收不收得回来"，不是"改不改工作区"。** 这两件事看起来是一回事，
/// 加进网络工具之后就不是了：
///
/// - `Write`/`Edit`/`Delete` 改工作区，但改动先进 ChangeSet，人可以在提交前反悔；
///   要审批是因为提交这一步本身需要人负责。
/// - `Bash` 的副作用**直接落盘**，没有 ChangeSet 兜着。
/// - `WebFetch`/`WebSearch` 一个字节也不改工作区，却把一个由模型选定的地址发了
///   出去。请求一旦发出就收不回来，对方的日志里已经有它了。
///
/// 所以不能拿 [`agentrs_types::EffectProfile`] 来派生这张表——它另有职责
/// （Plan 模式该不该放行），一物二用会在只读探索时顺带禁掉查资料，
/// 而那正是 Plan 模式最该允许的事。两件事看起来相关、实际互不蕴含。
pub fn approval_required(name: &str) -> bool {
    matches!(
        name,
        "Write" | "Edit" | "Delete" | "Bash" | "WebFetch" | "WebSearch"
    )
}

/// 按是否需要人工审批切分一份目录。
///
/// 宿主别再手抄工具名。目录里新增一个工具而策略的两张表没跟上，那个工具就
/// **永远调不动**——症状是模型反复提出一个被判 `OutOfAuthority` 的调用，
/// 而目录里明明有它，于是它开始换着法子绕，把一整个 Run 耗在猜上。
pub fn split_for_approval(catalog: &[ToolDef]) -> (Vec<String>, Vec<String>) {
    let (mut auto, mut approval) = (Vec::new(), Vec::new());
    for tool in catalog {
        if approval_required(&tool.name) {
            approval.push(tool.name.clone());
        } else {
            auto.push(tool.name.clone());
        }
    }
    (auto, approval)
}

/// 取一个字符串参数。
pub(crate) fn arg(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// 取一个布尔参数，缺省 false。
pub(crate) fn flag(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 取一个正整数参数。
pub(crate) fn count(args: &Value, key: &str, default: u64) -> usize {
    args.get(key).and_then(Value::as_u64).unwrap_or(default).max(1) as usize
}

/// 派发一次工具调用。
///
/// 名字不认识时报 127 并**列出认识哪些**：模型偶尔会发明工具名，
/// 一句"未知工具"让它只能再猜一次。
pub async fn execute(name: &str, args: &Value, cx: &ToolCtx<'_>) -> ToolOutput {
    match name {
        "Glob" => glob::run(cx, args).await,
        "Grep" => grep::run(cx, args).await,
        "Read" => read::run(cx, args).await,
        "Write" => write::run(cx, args).await,
        "Edit" => edit::run(cx, args).await,
        "Delete" => delete::run(cx, args).await,
        "Bash" => bash::run(cx, args).await,
        "WebFetch" => web_fetch::run(cx, args).await,
        "WebSearch" => web_search::run(cx, args).await,
        other => ToolOutput {
            outcome: ExecutionOutcome::Completed { exit_code: 127 },
            text: Some(format!(
                "未知工具：{other}。可用的是：{}",
                tool_names(cx.capabilities()).join("、")
            )),
        },
    }
}

#[cfg(test)]
pub(crate) mod fake;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 目录随能力伸缩() {
        assert_eq!(catalog(Capabilities::FILES_ONLY).len(), 6);
        assert_eq!(catalog(Capabilities::ALL).len(), 9);
        // 没有 shell 就没有 Bash——声明了却跑不动的工具在构造上不该存在。
        assert!(!tool_names(Capabilities::FILES_ONLY).contains(&"Bash".to_string()));
        assert!(tool_names(Capabilities::ALL).contains(&"Bash".to_string()));
    }

    #[test]
    fn 发现类工具排在前面_bash_排在最后() {
        // 模型按顺序读目录。先看到"怎么找文件"才不会去猜路径；
        // 先看到 Read/Edit 才不会用 cat/sed 去做它们的事。
        let names = tool_names(Capabilities::ALL);
        assert_eq!(&names[..2], &["Glob".to_string(), "Grep".to_string()]);
        assert_eq!(names.iter().position(|n| n == "Bash"), Some(7));
    }

    #[test]
    fn 读类只读可并发_写类会改且独占() {
        // fail-closed 的两个默认值，一旦反了：Plan 模式会放过写工具，
        // 或者一批本可并行的读被串起来。
        for tool in catalog(Capabilities::ALL) {
            let 只读 = matches!(
                tool.name.as_str(),
                "Glob" | "Grep" | "Read" | "WebFetch" | "WebSearch"
            );
            assert_eq!(tool.is_read_only(), 只读, "{}", tool.name);
            assert_eq!(tool.concurrency_safe, 只读, "{}", tool.name);
        }
    }

    #[test]
    fn 只读与要审批是两件事() {
        // 这条守着 `approval_required` 的判据。把它退回成 `!is_read_only()`
        // 之后，WebFetch 会变成自动放行——模型就能不经人点头把任意 URL 发出去，
        // 而请求发出去就收不回来了。
        let c = catalog(Capabilities::ALL);
        let fetch = c.iter().find(|t| t.name == "WebFetch").unwrap();
        assert!(fetch.is_read_only(), "WebFetch 不改工作区");
        assert!(approval_required("WebFetch"), "但仍然要人点头");
        for name in ["Glob", "Grep", "Read"] {
            assert!(!approval_required(name), "{name}");
        }
    }

    #[test]
    fn 目录里每个工具都被明确分类() {
        // 漏掉一个的症状是它**永远调不动**：策略两张表都没有它，
        // 每次调用都被判 OutOfAuthority，而目录里明明有它。
        let c = catalog(Capabilities::ALL);
        let (auto, approval) = split_for_approval(&c);
        assert_eq!(auto.len() + approval.len(), c.len(), "{auto:?} {approval:?}");
        assert_eq!(auto, ["Glob", "Grep", "Read"]);
        assert_eq!(approval.len(), 6);
    }

    #[test]
    fn 目录过得了自己的_lint() {
        // 强制点，放在目录旁边而不是某个宿主里：这样每个宿主都被它保护，
        // 而不是谁记得加谁受保护。没有它，`crate::lint` 就只是一个没人调的函数。
        let findings = crate::lint::audit_catalog(&catalog(Capabilities::ALL));
        assert!(findings.is_empty(), "{}", crate::lint::describe_all(&findings));
    }

    #[test]
    fn 每个工具的_schema_都能接住一次畸形调用() {
        // lint 管的是"声明写得好不好"，这条管的是"声明真的能用来校验"——
        // 一份写得很漂亮但 required 为空的 schema 能过 lint，却拦不住任何东西。
        for tool in catalog(Capabilities::ALL) {
            let v = agentrs_types::validate(&tool.parameters, &serde_json::json!({}));
            assert!(!v.is_empty(), "{} 的 schema 连空参数都放行", tool.name);
        }
    }

    #[test]
    fn 搜索端点的_display_不会印出_key() {
        let e = SearchEndpoint {
            url: "https://api.example.com/search".into(),
            key: "sk-绝密".into(),
        };
        let shown = e.to_string();
        assert!(!shown.contains("sk-绝密"), "{shown}");
        assert!(shown.contains("api.example.com"), "{shown}");
    }

    #[tokio::test]
    async fn 未知工具会告诉模型有哪些可用() {
        // 一句"未知工具"让模型只能再猜一次。
        let files = fake::FakeWorkspace::new([]);
        let cx = ToolCtx::files_only(&files);
        let out = execute("Teleport", &serde_json::json!({}), &cx).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 127 });
        let text = out.text.unwrap();
        assert!(text.contains("Teleport"), "{text}");
        assert!(text.contains("Read"), "{text}");
    }

    #[test]
    fn 路径越界映射成拒绝而不是失败() {
        // `Rejected` 是授权层的话。越界是围栏拦的，不是工具挑的，
        // 所以它该走这条——conformance 的 H2 也据此区分。
        let out = ToolOutput::from(IoError::OutsideWorkspace);
        assert_eq!(
            out.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::OutsideWorkspace
            }
        );
    }
}
