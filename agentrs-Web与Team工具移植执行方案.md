# agentrs-tools：WebSearch / WebFetch / Team 工具移植执行方案

> 目标：将 `opensource/claude-code-main` 中的 **WebSearchTool**、**WebFetchTool**、**Team 三件套（TeamCreate / TeamDelete / SendMessage）** 在 `agentrs/crates/agentrs-tools` 中落地实现。
>
> 文档日期：2026-09-02
> 适用仓库：`agentrs`（Rust 2021，Cargo workspace）
> 约束基准：`agentrs/AGENTS.md`

> **实施状态（2026-09-12 更新）**：Web 与 Team 两条实现线均已落地。Team 线复用 Task 级 SubAgent 注册表，支持持久具名 teammate、边界收件箱注入、结构化退出和会话结束清理。
> Team 功能默认关闭，通过 `[team] enabled = true` 显式启用；持久 teammate 当前仅支持共享工作区隔离。

---

## 0. 决策摘要（Decisions）

本方案在以下三个分歧点上已作出选择。若需推翻，请在 Phase 0 启动前提出——Phase 1 之后返工代价显著上升。

| # | 决策点 | 选择 | 理由 |
|---|--------|------|------|
| D1 | WebSearch 实现路线 | **可插拔搜索后端**（`SearchBackend` trait + Brave/Tavily/SearXNG） | Claude Code 原版依赖 Anthropic 服务端工具 `web_search_20250305`，原样移植会在 4 个 provider 中硬编码 Anthropic 特性，直接违反 `AGENTS.md` 的头号规则「No Hardcoded Provider Quirks」 |
| D2 | Team 执行后端 | **仅 in-process**，放弃 tmux / iTerm2 | agentrs 是单进程 Rust + tokio 运行时；pane 后端属于终端 UI 编排，与内核无关，且引入大量平台分支（违反「Centralize Platform Differences」） |
| D3 | WebFetch 的内容提炼 | 新增 `TextSummarizer` trait，由 `agentrs-agent` 注入；未注入时**降级为截断原文** | `agentrs-tools` 不得依赖同层的 `agentrs-providers`；复用已有 `agentrs-types::spawner::Spawner` 的注入先例 |

**范围外（明确不做，列为 Phase 5 候选）**：`uds:` / `bridge:` 跨进程/跨机寻址、tmux/iTerm2 pane 后端、Task\* 系列工具（TaskCreate/Update/List/Get/Stop/Output）、plan_approval 协议、git worktree 隔离、Anthropic 服务端工具透传。

---

## 1. 现状分析

### 1.1 `agentrs-tools` 结构

`agentrs/crates/agentrs-tools/` 现有 8 个工具，全部为「纯本地、无网络、无 LLM 调用」型：

| 文件 | 工具 | Category | deferred |
|------|------|----------|----------|
| `read.rs` / `write.rs` / `edit.rs` | Read / Write / Edit | Info / Edit | 否 |
| `exec_command.rs` | ExecCommand | Exec | 否 |
| `grep.rs` / `glob.rs` | Grep / Glob | Info | 否 |
| `view_image.rs` | ViewImage | Info | 否 |
| `tool_search.rs` | ToolSearch | Info | — |

依赖仅 `agentrs-types / -protocol / -config / -process` + `tokio / serde / glob / lru / base64`。
**当前未启用 `reqwest`**，但 workspace 根 `Cargo.toml:48` 已声明 `reqwest 0.12 (json, stream, rustls-tls, default-features = false)`，可直接以 `workspace = true` 引入。

### 1.2 `Tool` trait 的能力缺口（`agentrs-tools/src/tool.rs:44`）

现有签名：

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> JsonSchema;
    fn is_concurrency_safe(&self, input: &Value) -> bool;
    async fn execute(&self, input: Value) -> ToolResult;
    async fn execute_with_follow_up(&self, input: Value) -> ToolExecutionOutput { ... }
    fn requires_image_input(&self) -> bool { false }
    fn context_modifier_for(&self, _input: &Value) -> Option<ContextModifier> { None }
    fn skill_hooks_for(&self, _input: &Value) -> Option<HooksConfig> { None }
    fn max_result_size(&self) -> usize { 50_000 }
    fn category(&self) -> ToolCategory;
    fn is_deferred(&self) -> bool { false }
    fn describe(&self, input: &Value) -> String { ... }
}
```

对本次移植而言存在 **5 个关键缺口**：

1. **无取消信号**。`execute()` 不接收 `AbortSignal` / `CancellationToken`。`agentrs-agent/src/engine.rs:1597` 的 `abort_current_turn()` 仅做转录记账，无法中断正在执行的 tool future。而 WebFetch / WebSearch 是分钟级网络操作，Claude Code 原版全程依赖 `abortController` 传播。
2. **无进度回调**。原版 `WebSearchTool.call(input, ctx, canUseTool, parentMsg, onProgress)` 会推送 `query_update` / `search_results_received` 事件。agentrs 的 Tool 无此通道。
3. **结果只有纯文本**。`agentrs-types/src/tool.rs:44` 的 `ToolResult { content: String, is_error: bool }`，无 `outputSchema` / 结构化 data 概念。原版 WebSearch 的 `Output { query, results[], durationSeconds }` 需自行序列化为文本。
4. **`&self` 不可变 + 无执行期上下文注入**。原版 tool 可访问 `context.getAppState() / setAppState / options.mainLoopModel`。agentrs 只能靠构造期注入 `Arc<...>`——`SpawnTool::new(spawner)`（`agentrs-agent/src/spawn_tool.rs:21`）、`SkillTool::new(...)`（`bootstrap.rs:345`）已是该模式。
5. **权限粒度只到工具名**。`agentrs-agent/src/confirm.rs:36` 是 `allow_list.contains(tool_name)`；原版 WebFetch 使用 **domain 粒度**规则（`domain:example.com`）+ `preapproved.ts` 主机白名单。

### 1.3 分层约束（来自 `AGENTS.md`）

- `agentrs-tools` 与 `agentrs-providers` 同为 Mid 层，**tools 不得依赖 providers**（破坏「依赖向下流动」）。
- 已有正确先例：`agentrs-types/src/spawner.rs` 定义 `Spawner` trait → `agentrs-agent` 实现 `AgentSpawner` → `SpawnTool` 仅持 `Arc<dyn Spawner>`。**本次三类工具全部复用该模式。**
- 「No Hardcoded Provider Quirks」是本仓最高优先级规则，直接决定 D1。
- 业务逻辑不得写在 `mod.rs` / `lib.rs`；测试须抽到同目录 `*_test.rs` 并以 `#[cfg(test)] #[path = "..."] mod ...;` 挂载。

### 1.4 Team 相关现状

全仓 grep 无 `team` / `mailbox` / `inbox` 的实现代码（仅测试与 memory prompt 中出现字面量）。唯一的多 agent 能力是：

- `SpawnTool`（`agentrs-agent/src/spawn_tool.rs`）：最多 5 个子 agent、200 轮、4096 输出 token
- `AgentSpawner::spawn_parallel`（`agentrs-agent/src/spawner.rs:105`）：**阻塞式、一次性、无名字、无常驻**，结果在父 agent 处一次性汇总

也就是说 Team 工具**不是移植三个 tool 文件**，而是需要先建立一整套**常驻具名 agent 运行时**。这是本方案中工作量最大的一块（约 60~70%）。

---

## 2. 源码分析与移植差异

### 2.1 WebFetchTool

源码：`opensource/claude-code-main/src/tools/WebFetchTool/`（核心 `utils.ts` 531 行）

**原版执行链路：**

```
validateURL(len ≤ 2000, 无 username/password, hostname 含 '.')
  → URL_CACHE 命中?  (LRU, 50MB, TTL 15min)
  → http: → https: 升级
  → checkDomainBlocklist()          ← 调 api.anthropic.com/api/web/domain_info
                                       (DOMAIN_CHECK_CACHE: 128 条 / TTL 5min，仅缓存 allowed)
  → getWithPermittedRedirects()     ← maxRedirects=0 手工跟跳，上限 10 跳
       允许：同 protocol + 同 port + strip "www." 后同 host
       否则：返回 RedirectInfo，由模型重新调用 WebFetch
  → 限制：10MB body / 60s 超时 / 自定义 User-Agent
  → isBinaryContentType? → persistBinaryContent() 落盘
  → content-type 含 text/html? → turndown() → Markdown
  → applyPromptToMarkdown(): 截断至 100K → **queryHaiku 小模型**按 prompt 提炼
```

**权限模型：** `domain:<hostname>` 规则（deny → ask → allow 顺序）+ `preapproved.ts` 白名单。白名单主机 + `text/markdown` + 长度 < 100K 时**跳过小模型**，直接返回原文。

**移植差异：**

| 原版 | agentrs 侧处理 |
|------|----------------|
| `turndown` (JS) | Rust 需选型：**`htmd`**（Turndown 的 Rust 移植，MIT，API 最接近）；备选 `html2text` |
| `queryHaiku` | 无对应能力 → 新增 `TextSummarizer` trait（D3） |
| `api.anthropic.com` 域名预检 | **必须移除**（provider-neutral + 私有端点）→ 替换为本地 `allow_domains` / `deny_domains` 配置 |
| SSRF 防护 | 原版仅「hostname 含 `.`」弱校验 + 远端 blocklist 兜底。agentrs 无远端兜底，**必须自建私网/环回/链路本地地址拒绝**，否则是真实漏洞（见 §5 风险 R1） |
| `logEvent` 分析埋点 | 不移植；改为 `tracing` 结构化日志，且不得记录 URL 全文以外的敏感 payload |

### 2.2 WebSearchTool

源码：`opensource/claude-code-main/src/tools/WebSearchTool/WebSearchTool.ts`（`call()` 在 254 行）

**原版本质：** 起一个**嵌套的 LLM 流式会话**，把 Anthropic 服务端工具

```ts
{ type: 'web_search_20250305', name: 'web_search', allowed_domains, blocked_domains, max_uses: 8 }
```

通过 `extraToolSchemas` 注入，然后从返回的 `server_tool_use` / `web_search_tool_result` content block 中抽取 `{ title, url }`。`isEnabled()` 显式写死仅 `firstParty` / `vertex(claude-4+)` / `foundry` 可用。

**架构冲突：** agentrs 的 `LlmRequest`（`agentrs-types/src/llm.rs:8`）与 `ToolDef`（`agentrs-types/src/tool.rs:35`）**完全没有「服务端工具」概念**，`LlmEvent` 也无 `ServerToolUse` / `WebSearchResult` 变体。

**路线对比（D1 依据）：**

| | A. Provider 原生服务端工具 | B. 可插拔搜索后端 ✅ |
|---|---|---|
| 做法 | 扩展 `ToolDef`/`LlmRequest`/`LlmEvent`，Anthropic provider 透传 server tool | 定义 `SearchBackend` trait，HTTP 调 Brave / Tavily / SearXNG |
| Provider 中立 | ✗ OpenAI / Bedrock 直接不可用 | ✓ 完全中立 |
| 改动面 | types + 4 个 provider + parser + engine + stream_runner | 仅 agentrs-tools + agentrs-config |
| 结果质量 | 带 citation，模型侧已优化 | 原始 title / url / snippet |
| 成本模型 | 走模型 token | 走搜索 API 配额 |

**保留的原版语义（对模型行为影响大，务必照搬）：**

- `query` 最小长度 2
- `allowed_domains` 与 `blocked_domains` **互斥**（原版 `errorCode: 2`）
- 结果尾部固定提醒：
  `REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.`

### 2.3 Team 三件套

| 工具 | 原版依赖的运行时设施 |
|------|---------------------|
| `TeamCreateTool` | `teamHelpers.ts`（team file：`~/.claude/teams/<team>/config.json`）、`AppState.teamContext`、`tasks.ts`（Team = Project = TaskList）、`teammateLayoutManager`（配色）、session 结束清理注册 |
| `TeamDeleteTool` | 读 team file，过滤 `name !== TEAM_LEAD_NAME` 且 `isActive !== false` 的成员；有活跃成员则**拒绝**并列出成员名；清理目录 + worktree + 颜色 + AppState |
| `SendMessageTool` | `teammateMailbox.ts`（1183 行：`~/.claude/teams/<team>/inboxes/<agent>.json` + `lockfile` 并发锁，retries=10/5~100ms 退避）、`agentNameRegistry`、`LocalAgentTask` 挂起消息队列、停止 agent 的自动 resume、`uds:` / `bridge:` 跨进程寻址、`shutdown_request` / `shutdown_response` / `plan_approval_response` 结构化协议 |

后端形态（`utils/swarm/backends/types.ts`）：`'tmux' | 'iterm2' | 'in-process'`。

**关键行为语义（须保留）：**

- 一个 leader 同时只能带一个 team；重复创建报错并提示先 `TeamDelete`
- team 名冲突时**自动改名**（原版 `generateWordSlug()`），而非失败
- lead agent ID 形如 `team-lead@<teamName>`（`formatAgentId`）
- 消息「在接收方下一个工具轮次入队并 drain」，而非实时中断
- 收件以 `<teammate-message from="...">` 形式注入接收方上下文
- 广播 `"*"` 开销线性于团队规模，原版在 prompt 中明确警告

---

## 3. 目标架构

### 3.1 依赖关系（新增部分）

```
agentrs-types
  ├── summarizer.rs   (新) trait TextSummarizer
  └── team.rs         (新) TeamId / AgentId / TeamFile / InboxMessage / trait TeamRuntime
        ▲                        ▲
        │                        │
agentrs-tools                agentrs-agent
  ├── web.rs (新)              ├── summarizer.rs (新) ProviderSummarizer: impl TextSummarizer
  └── team.rs (新)             └── team/         (新) InProcessTeamRuntime: impl TeamRuntime
        │                             │
        └───── 构造期注入 Arc<dyn ...> ┘
```

**不新增 crate**（遵循 AGENTS.md：不为少量共享代码新建 crate）。

### 3.2 `TextSummarizer`（`agentrs-types/src/summarizer.rs`）

```rust
use async_trait::async_trait;

/// Applies a natural-language instruction to a body of text using a
/// small/fast model. Injected by the agent layer so that `agentrs-tools`
/// never depends on `agentrs-providers`.
#[async_trait]
pub trait TextSummarizer: Send + Sync {
    async fn summarize(&self, instruction: &str, content: &str) -> Result<String, String>;
}
```

`agentrs-agent/src/summarizer.rs` 提供 `ProviderSummarizer`，持 `Arc<dyn LlmProvider>` + 小模型名，做一次非流式 collect。

### 3.3 `TeamRuntime`（`agentrs-types/src/team.rs`）

```rust
#[async_trait]
pub trait TeamRuntime: Send + Sync {
    async fn create_team(&self, name: &str, description: Option<&str>, agent_type: Option<&str>)
        -> Result<TeamInfo, TeamError>;
    async fn delete_team(&self) -> Result<TeamDeleteReport, TeamError>;
    async fn send(&self, to: Recipient, message: InboxMessage)
        -> Result<DeliveryReport, TeamError>;
    fn current_team(&self) -> Option<TeamInfo>;
    fn members(&self) -> Vec<TeamMember>;
}
```

### 3.4 Tool trait 扩展（三块共用，带默认实现，不破坏现有 8 个工具）

```rust
/// Execute with cooperative cancellation. Long-running network or
/// multi-agent tools override this; the default ignores the token so
/// existing tools stay source-compatible.
async fn execute_cancellable(&self, input: Value, _cancel: CancellationToken) -> ToolResult {
    self.execute(input).await
}
```

`ToolCategory`（`agentrs-protocol/src/events.rs:98`）新增两个 variant：

```rust
pub enum ToolCategory { Info, Edit, Exec, Mcp, Network, Team }
```

须同步更新 `Display` 实现（`events.rs:105`）与 `docs/json-stream-protocol.md`。

---

## 4. 分阶段执行计划

### Phase 0 — 地基（2~3 人日）

| # | 交付物 | 位置 |
|---|--------|------|
| 0.1 | `ToolCategory::{Network, Team}` + `Display` 分支 + 协议文档同步 | `agentrs-protocol/src/events.rs`、`docs/json-stream-protocol.md` |
| 0.2 | `Tool::execute_cancellable` 默认实现；engine / tool_call 传入 `CancellationToken` | `agentrs-tools/src/tool.rs`、`agentrs-agent/src/tool_call.rs`、`engine.rs` |
| 0.3 | `TextSummarizer` trait | `agentrs-types/src/summarizer.rs`（`lib.rs` 仅加 `pub mod summarizer;`） |
| 0.4 | 依赖引入：`reqwest`(workspace) / `url` / `tokio-util`(CancellationToken) / `htmd` | `agentrs-tools/Cargo.toml` |
| 0.5 | `WebConfig` 配置节 + JSON schema 更新 | `agentrs-config/src/web.rs`（新文件），`config.rs` 内嵌 `pub web: WebConfig`，`schema.rs` 同步 |

**`WebConfig` 草案：**

```toml
[web]
enabled            = true
timeout_secs       = 60
max_content_bytes  = 10485760      # 10 MB，对齐原版 MAX_HTTP_CONTENT_LENGTH
max_redirects      = 10            # 对齐原版 MAX_REDIRECTS
max_url_length     = 2000          # 对齐原版 MAX_URL_LENGTH
cache_ttl_secs     = 900           # 15 min
cache_max_bytes    = 52428800      # 50 MB
max_markdown_chars = 100000        # 对齐原版 MAX_MARKDOWN_LENGTH
allow_private_network = false      # SSRF 总开关，默认关闭
allow_domains      = []            # 空 = 不限制
deny_domains       = []
preapproved_domains = []           # 对齐原版 preapproved.ts：命中则跳过摘要模型
user_agent         = ""            # 空 = "agentrs/<version>"

[web.search]
backend      = "none"              # brave | tavily | searxng | none
api_key_env  = "BRAVE_API_KEY"
base_url     = ""                  # searxng 自建实例必填
max_results  = 10
timeout_secs = 30
```

**验收：** `cargo build` 通过，现有 8 个工具零改动编译，`cargo test` 全绿。

---

### Phase 1 — WebFetch（3~4 人日）

**新增文件**（`lib.rs` 仅加 `pub mod web;`，`web.rs` 仅作模块声明与再导出）：

```
agentrs-tools/src/web.rs                  // 仅 pub mod / pub use
agentrs-tools/src/web/http_client.rs      // reqwest 封装：超时 / 大小上限 / UA / 手工重定向
agentrs-tools/src/web/url_policy.rs       // URL 校验 + SSRF 防护 + allow/deny + 重定向裁决
agentrs-tools/src/web/html_to_md.rs       // HTML → Markdown
agentrs-tools/src/web/url_cache.rs        // LRU + TTL（复用已有 lru 依赖）
agentrs-tools/src/web/fetch_tool.rs       // WebFetchTool
+ 同目录 url_policy_test.rs / url_cache_test.rs / html_to_md_test.rs / fetch_tool_test.rs
```

**1.1 `url_policy.rs`** —— 比原版更严格（安全增量）

- URL 长度 ≤ `max_url_length`；拒绝含 username / password；hostname 必须含 `.`
- `http:` → `https:` 自动升级
- **SSRF 防护（原版缺失，本仓必须实现）**：拒绝 IP 字面量落在
  `127.0.0.0/8`、`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`、`169.254.0.0/16`（含云 metadata `169.254.169.254`）、`0.0.0.0/8`、`::1`、`fc00::/7`、`fe80::/10`；
  拒绝 `localhost` / `*.local` / `*.internal`；
  **DNS 解析后二次校验**（防 DNS rebinding）。
  仅当 `web.allow_private_network = true` 时豁免。
- `deny_domains` 命中即拒；`allow_domains` 非空时白名单模式
- `is_permitted_redirect(orig, target)`：同 protocol + 同 port + strip `www.` 后同 host；不满足则返回 `Redirect { original, target, status }`，由工具层生成原版那段引导文本让模型重新调用

**1.2 `http_client.rs`**

- `reqwest::Client` 配置 `redirect::Policy::none()`，手工循环跟跳，`depth > max_redirects` 报错
- `Accept: text/markdown, text/html, */*`，`User-Agent` 来自配置
- 流式读取并在超过 `max_content_bytes` 时中断（不要先全量下载再判断）
- 支持 `CancellationToken`：`tokio::select!` 竞速

**1.3 `html_to_md.rs`**

- 选型 **`htmd`**（Turndown 的 Rust 移植，MIT）；备选 `html2text`
- 仅当 `content-type` 含 `text/html` 时转换，否则原样透传
- **实施前先做体积 / 许可对比，并更新 `THIRD-PARTY-NOTICES.md`**

**1.4 `url_cache.rs`**

- `lru::LruCache` + 手工 TTL 时间戳（复用现有 `lru` 依赖，不新增）
- 按 `content` 字节数计权，逼近原版的 `maxSize` 语义；空响应权重钳位为 1
- 以**原始 URL**为 key（不是升级/重定向后的 URL），对齐原版

**1.5 `fetch_tool.rs`**

```rust
pub struct WebFetchTool {
    client: HttpClient,
    policy: UrlPolicy,
    cache: Arc<Mutex<UrlCache>>,
    summarizer: Option<Arc<dyn TextSummarizer>>,
    config: WebConfig,
}
```

- `name = "WebFetch"`，`category = Network`，`is_concurrency_safe = true`，`is_deferred = true`，`max_result_size = 100_000`
- `describe()` 返回 `Fetch <hostname>`（供确认提示展示）
- 二进制 content-type：落盘 `<session_dir>/webfetch/`，结果尾部追加
  `[Binary content (<mime>, <size>) also saved to <path>]`
- `preapproved_domains` 命中 + `text/markdown` + 长度 < `max_markdown_chars` → 跳过摘要，直接返回原文
- `summarizer == None` → 截断至 `max_markdown_chars` 原样返回，并在结果中注明「未启用摘要模型」
- description 前缀保留原版的鉴权告警：
  `IMPORTANT: WebFetch WILL FAIL for authenticated or private URLs...`

**1.6 `ProviderSummarizer`** —— `agentrs-agent/src/summarizer.rs`，在 `bootstrap.rs::register_agent_tools`（`bootstrap.rs:335` 附近）构造并注入。

**1.7 权限扩展** —— `agentrs-agent/src/confirm.rs` 支持 `WebFetch:domain:<host>` 形式的 allow-list 条目，`check()` 增加按前缀匹配的分支。

**测试：**

- 单测：`url_policy` 全分支（各私网段、DNS rebinding、重定向裁决矩阵、长度 / 凭据 / 无点 hostname）、`url_cache` TTL 与容量驱逐、`html_to_md` 基本元素
- 集成测试 `agentrs-tools/tests/web_fetch_test.rs`：用 `wiremock` 覆盖跨 host 重定向、同 host 多跳、超大 body、超时、二进制、非 HTML、301/302/307/308

---

### Phase 2 — WebSearch（2~3 人日）

**新增文件：**

```
agentrs-tools/src/web/search_backend.rs   // trait SearchBackend + SearchHit
agentrs-tools/src/web/search_brave.rs
agentrs-tools/src/web/search_tavily.rs
agentrs-tools/src/web/search_searxng.rs
agentrs-tools/src/web/search_tool.rs      // WebSearchTool
+ 同目录 *_test.rs
```

**2.1 `SearchBackend`**

```rust
pub struct SearchHit { pub title: String, pub url: String, pub snippet: Option<String> }

#[async_trait]
pub(crate) trait SearchBackend: Send + Sync {
    fn name(&self) -> &str;
    async fn search(&self, query: &str, filter: &DomainFilter, limit: usize)
        -> Result<Vec<SearchHit>, String>;
    /// Whether the backend applies domain filtering server-side.
    fn supports_domain_filter(&self) -> bool { false }
}
```

- 后端原生支持域名过滤则下推，否则在工具层做 post-filter

**2.2 `search_tool.rs`**

- `name = "WebSearch"`，`category = Network`，`is_concurrency_safe = true`，`is_deferred = true`，`max_result_size = 100_000`
- 输入 schema 与原版一致：`query`（min 2）、`allowed_domains?`、`blocked_domains?`
- 校验：query 非空且 ≥2；`allowed_domains` 与 `blocked_domains` 互斥
- 输出格式**照搬原版**：

  ```
  Web search results for query: "<q>"

  Links: [{"title":"...","url":"..."}, ...]

  REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.
  ```

**2.3 `isEnabled` 等价物** —— agentrs 的 `Tool` trait 无 `isEnabled`，改为**注册期判断**：`bootstrap.rs` 中当 `web.search.backend == "none"` 或 API key 环境变量缺失时**不注册** `WebSearchTool`。

**测试：** `mock` backend + `wiremock` 回放 Brave / Tavily 真实响应样本；域名互斥校验；post-filter 正确性；结果格式化快照测试。

---

### Phase 3 — Team 运行时（8~12 人日，最大块）

#### 3a. 类型与 trait（`agentrs-types/src/team.rs`）

`TeamId` / `AgentName` / `AgentId`（`name@team` 格式，`@` 需 sanitize）/ `TeamInfo` / `TeamMember` / `InboxMessage` / `Recipient`（`Named(String) | Broadcast`）/ `TeamError` / `DeliveryReport` / `TeamDeleteReport`，以及 §3.3 的 `TeamRuntime` trait。

#### 3b. 运行时实现（`agentrs-agent/src/team/`）

```
agentrs-agent/src/team.rs           // 仅 mod 声明
agentrs-agent/src/team/store.rs     // team file 读写：<session_dir>/teams/<team>/config.json + inboxes/
agentrs-agent/src/team/registry.rs  // 常驻 TeammateHandle 表：name → handle，名字唯一性、颜色分配
agentrs-agent/src/team/runtime.rs   // InProcessTeamRuntime: impl TeamRuntime
agentrs-agent/src/team/mailbox.rs   // mpsc 投递 + 文件镜像；广播
agentrs-agent/src/team/inject.rs    // 收件注入：turn 边界包装为 <teammate-message from="...">
+ 同目录 *_test.rs
```

**关键设计：**

- **投递走内存 `mpsc`，文件只作镜像**（便于 TUI 展示 / 跨进程观察 / 崩溃恢复），从而避开原版 `lockfile` 重试锁 + 轮询 inbox 的复杂度
- `AgentSpawner` 新增 `spawn_persistent(name, config) -> TeammateHandle`：子 agent 跑在 `tokio::spawn`，**每轮工具调用边界 drain 一次 inbox**（对齐原版「enqueue and drain at the receiver's next tool round」）
- `TeammateHandle { name, agent_id, join_handle, tx, cancel: CancellationToken, status }`
- 优雅关闭：保留原版 `shutdown_request` / `shutdown_response` 结构化消息语义
- `TeamDelete` 在存在 `is_active != false` 的非 lead 成员时**拒绝**并列出成员名（原版行为）
- session 结束时清理 team 目录（对齐原版 gh-32730 的修复）
- 成员数上限（默认 8），广播时给出成本提示

**跨平台：** team 目录基于 `config.session.directory` 用 `Path::join` 构造，禁止硬编码路径分隔符；名字 sanitize 复用原版规则（非字母数字 → `-`，小写）。

#### 3c. 工具层（`agentrs-tools/src/team/`）

```
agentrs-tools/src/team.rs
agentrs-tools/src/team/create_tool.rs   // TeamCreate
agentrs-tools/src/team/delete_tool.rs   // TeamDelete
agentrs-tools/src/team/send_tool.rs     // SendMessage
+ 同目录 *_test.rs
```

- 三者均通过构造期注入 `Arc<dyn TeamRuntime>`，`category = Team`，`is_deferred = true`
- `TeamCreate`：team_name 必填；重名自动改名；一个 leader 只能带一个 team
- `SendMessage`：`to` 为 `<name>` 或 `"*"`；`summary` 5~10 词，用于 UI 预览
- 单测用 `MockTeamRuntime`，无需真实 agent

#### 3d. 接线

- `bootstrap.rs::register_agent_tools`：`config.team.enabled` 时构造 `InProcessTeamRuntime` 并注册三个工具
- `agentrs-protocol` 新增 `TeamEvent`（`member_joined` / `member_exited` / `message_sent`），供 TUI 渲染
- `agentrs-tui` 侧的 teammate 列表与配色展示，**拆为独立后续任务**，不阻塞本方案

---

### Phase 4 — 收尾（1~2 人日）

- `docs/tools.md`：表格新增 3 行 + 三个章节
- `docs/advanced.md`：新增 Team 章节（与 Sub-agents 并列）
- `docs/getting-started.md`：`[web]` / `[web.search]` / `[team]` 配置说明
- `THIRD-PARTY-NOTICES.md`：补 `htmd`（及其传递依赖）与搜索后端相关依赖
- 日志审查：按 AGENTS.md「Logging」节，确认 `debug`/`info`/`warn`/`error` 分级合理，且**生产日志不含 URL 查询串、页面正文、API key、消息正文**
- 可见性审查：新增 item 优先 `pub(crate)`；仅跨 crate 使用的才 `pub`
- `cargo fmt --all` / `cargo clippy`（零告警）/ `cargo test`
- Windows CI 验证（路径、目录创建）
- `just push`（禁止直接 `git push`）

---

## 5. 风险与缓解

| ID | 风险 | 影响 | 缓解 |
|----|------|------|------|
| R1 | **SSRF** —— WebFetch 失去原版的远端 blocklist 兜底 | 高：可读内网服务 / 云 metadata（169.254.169.254） | Phase 1.1 强制私网拒绝 + DNS 解析后二次校验；`allow_private_network` 默认 `false`；单测覆盖全部网段与 rebinding 场景 |
| R2 | 无取消信号导致 60s 请求挂起并阻塞整轮 | 中 | Phase 0.2 的 `CancellationToken` 必须先落地，**不得跳过**；同时 reqwest 侧设硬超时兜底 |
| R3 | HTML→MD 依赖的体积 / 许可问题 | 中 | 实施前做 `htmd` vs `html2text` 对比；置于 feature flag 后；更新 THIRD-PARTY-NOTICES |
| R4 | Team 常驻 agent 生命周期泄漏 | 高：进程无法退出 / 资源累积 | `TeammateHandle` 同时持 `JoinHandle` + `CancellationToken`；session 退出与 `TeamDelete` 双路径清理；编写泄漏回归测试 |
| R5 | 广播 `"*"` 在大团队上放大 token 消耗 | 中 | 成员数上限（默认 8）；prompt 中保留原版警告；结果中回报实际投递数 |
| R6 | 搜索后端 API 变更 / 配额耗尽 | 中 | `SearchBackend` 抽象隔离；错误以 `is_error` 结果返回而非 panic；wiremock 固化响应样本 |
| R7 | D1 决策被推翻（改走 provider 服务端工具） | 大：Phase 2 全部返工 + types/providers 大改 | **在 Phase 0 启动前确认**；Phase 1（WebFetch）与该决策无关，可并行先行 |
| R8 | 摘要模型调用放大成本 / 延迟 | 中 | `preapproved_domains` 跳过路径；`summarizer` 可为 `None` 降级；使用配置中的小模型 |

---

## 6. 工作量与排期

| 阶段 | 内容 | 人日 |
|------|------|------|
| Phase 0 | 地基（trait 扩展、配置、依赖） | 2 ~ 3 |
| Phase 1 | WebFetch | 3 ~ 4 |
| Phase 2 | WebSearch | 2 ~ 3 |
| Phase 3 | Team 运行时 + 三个工具 | 8 ~ 12 |
| Phase 4 | 文档、许可、CI、收尾 | 1 ~ 2 |
| **合计** | | **16 ~ 24 人日** |

**并行建议：** Phase 1+2（Web 线）与 Phase 3（Team 线）在 Phase 0 完成后可由两人并行推进，二者仅共享 Phase 0 的 trait 扩展，无功能耦合。

**说明：** 上表人日**已包含**测试编写工时（约占各阶段的 35~40%）。完整用例清单见 §8，共 **约 170 条**，分布为：Phase 0 十条、WebFetch 七十余条（其中 `url_policy` 四十余条为安全关键）、WebSearch 三十余条、Team 四十余条、跨阶段回归七条。

---

## 7. 验收标准

**功能**（括号内为 §8 对应用例编号）

- [ ] `WebFetch` 可抓取公网 HTTPS 页面并返回 Markdown；跨 host 重定向返回引导文本；二进制内容落盘并在结果中标注（TC-1.5-01/04/11）
- [ ] `WebFetch` 拒绝 `http://127.0.0.1`、`http://169.254.169.254`、`http://localhost`、IPv6 环回及各私网段（TC-1.1-07~26）
- [ ] `WebSearch` 在配置 Brave / Tavily / SearXNG 任一后端时返回结果，格式含结尾 REMINDER；后端为 `none` 时该工具不出现在 tool list 中（TC-2.1-08、TC-2.3-01/05）
- [x] `TeamCreate` → `SendMessage` → teammate 在下一工具轮次收到 `<teammate-message>` → `TeamDelete` 全链路可跑通（TC-3.5-01）
- [x] `TeamDelete` 在存在活跃成员时拒绝并列出成员名（TC-3.4-06）
- [ ] §8 全部用例通过，覆盖率达 §8.6 门槛（`url_policy.rs` 分支覆盖 100%）

**工程**

- [ ] `cargo clippy` 零告警；`cargo fmt --all` 无 diff
- [ ] 所有测试子模块位于同目录 `*_test.rs` 并以 `#[path]` 挂载
- [ ] 业务逻辑不出现在 `lib.rs` / `mod.rs`
- [ ] 单文件 < 1000 行
- [ ] 新增 item 的可见性经过审查，优先 `pub(crate)`
- [ ] Linux / macOS / Windows 三平台 CI 通过
- [ ] `docs/tools.md`、`docs/advanced.md`、`THIRD-PARTY-NOTICES.md` 已更新

---

## 8. 测试用例

### 8.0 约定

**命名与编号**：沿用本仓已有的 `TC-<阶段>.<模块>-<序号>` 约定（见 `agentrs-tools/tests/tool_description_test.rs` 首行注释）。

**位置规则**（AGENTS.md「Test Organization」）：

| 类型 | 位置 | 挂载方式 |
|------|------|----------|
| 单元测试（模块内部逻辑） | 同目录 `<module>_test.rs` | 源文件末尾 `#[cfg(test)] #[path = "<module>_test.rs"] mod <module>_test;` |
| 集成测试（公开 API / 功能需求） | `crates/<crate>/tests/` | 常规 `tests/*.rs` |

**依赖**：`wiremock 0.6`、`tokio-test 0.4`、`tempfile 3` **均已在 workspace 根 `Cargo.toml:135-137` 声明**，只需在 `agentrs-tools` / `agentrs-agent` 的 `[dev-dependencies]` 中以 `workspace = true` 引入，不新增第三方依赖。

**原则**（AGENTS.md）：每条用例必须验证有意义的行为或边界；禁止只断言 happy path 而不覆盖边界、错误与非显然逻辑。集成测试**从规格书写，不看实现**。

---

### 8.1 Phase 0 — 地基

**文件**：`agentrs-tools/src/tool_test.rs`（扩充）、`agentrs-protocol/src/events_test.rs`（扩充）

| ID | 用例 | 断言 |
|----|------|------|
| TC-0.1-01 | `ToolCategory::Network` / `Team` 的 `Display` 输出 | 分别等于 `"network"` / `"team"`；序列化为 snake_case |
| TC-0.1-02 | 已有 4 个 variant 的 Display 未被改动 | `info` / `edit` / `exec` / `mcp` 逐一相等（防回归） |
| TC-0.2-01 | 未覆写 `execute_cancellable` 的工具 | 默认实现转发到 `execute()`，结果一致 |
| TC-0.2-02 | 已取消的 `CancellationToken` 传入覆写实现 | 立即返回 `is_error = true`，内容含 `cancelled`，且**未发起网络请求**（用计数型 mock 客户端断言调用数为 0） |
| TC-0.2-03 | 执行中途取消 | 在 mock 服务端延迟响应期间 `cancel()`，工具在 100ms 内返回而非等满超时 |
| TC-0.3-01 | `TextSummarizer` 的 mock 实现可被 `Box<dyn>` / `Arc<dyn>` 持有 | 对象安全性编译期验证 + 一次调用返回值透传 |
| TC-0.5-01 | `WebConfig` 默认值 | `enabled=true`、`timeout_secs=60`、`max_content_bytes=10485760`、`max_redirects=10`、`allow_private_network=false`、`search.backend="none"` |
| TC-0.5-02 | 空 TOML 反序列化 | 全字段落到默认值，不 panic |
| TC-0.5-03 | 部分字段 TOML | 未指定字段仍取默认（`#[serde(default)]` 生效） |
| TC-0.5-04 | 配置级联（global + project） | project 覆盖 global 的标量字段；数组字段按既有策略合并/覆盖，与 `config_test.rs` 现有断言风格一致 |

---

### 8.2 Phase 1 — WebFetch

#### 8.2.1 `url_policy_test.rs`（单元，**安全关键，覆盖率要求 100% 分支**）

| ID | 输入 | 期望 |
|----|------|------|
| TC-1.1-01 | `https://example.com/a` | Accept |
| TC-1.1-02 | `http://example.com/a` | Accept，且返回的 URL 已升级为 `https://` |
| TC-1.1-03 | 长度 2001 的 URL | Reject，原因 `url_too_long` |
| TC-1.1-04 | 长度 2000 的 URL | Accept（边界值） |
| TC-1.1-05 | `https://user:pw@example.com` | Reject，原因 `credentials_in_url` |
| TC-1.1-06 | `https://user@example.com` | Reject（仅 username 也拒） |
| TC-1.1-07 | `https://localhost/x` | Reject，原因 `private_host` |
| TC-1.1-08 | `https://intranet/x`（hostname 不含 `.`） | Reject，原因 `not_public_hostname` |
| TC-1.1-09 | `https://foo.local/x` | Reject |
| TC-1.1-10 | `https://foo.internal/x` | Reject |
| TC-1.1-11 | `https://127.0.0.1/x` | Reject |
| TC-1.1-12 | `https://127.255.255.254/x` | Reject（`127/8` 全段） |
| TC-1.1-13 | `https://10.0.0.1/x` | Reject |
| TC-1.1-14 | `https://172.16.0.1/x` / `172.31.255.254` | Reject |
| TC-1.1-15 | `https://172.15.0.1` / `172.32.0.1` | **Accept**（`172.16/12` 边界外，防误杀） |
| TC-1.1-16 | `https://192.168.1.1/x` | Reject |
| TC-1.1-17 | `https://169.254.169.254/latest/meta-data/` | Reject（云 metadata，**高危**） |
| TC-1.1-18 | `https://0.0.0.0/x` | Reject |
| TC-1.1-19 | `https://[::1]/x` | Reject |
| TC-1.1-20 | `https://[fc00::1]/x` / `[fd00::1]` | Reject（`fc00::/7`） |
| TC-1.1-21 | `https://[fe80::1]/x` | Reject（链路本地） |
| TC-1.1-22 | `https://[2606:4700::1111]/x` | **Accept**（公网 IPv6） |
| TC-1.1-23 | `https://2130706433/x`（127.0.0.1 的十进制形式） | Reject（整数型 IP 绕过） |
| TC-1.1-24 | `https://0x7f000001/x`（十六进制形式） | Reject |
| TC-1.1-25 | DNS 解析到 `127.0.0.1` 的公网域名 | Reject（**DNS rebinding**，解析后二次校验生效） |
| TC-1.1-26 | 同上，但 `allow_private_network = true` | Accept（豁免开关生效） |
| TC-1.1-27 | `deny_domains = ["evil.com"]`，请求 `https://evil.com/x` | Reject，原因 `denied_domain` |
| TC-1.1-28 | `deny_domains = ["evil.com"]`，请求 `https://sub.evil.com/x` | Reject（子域一并拒绝） |
| TC-1.1-29 | `deny_domains = ["evil.com"]`，请求 `https://notevil.com/x` | **Accept**（后缀误匹配防护） |
| TC-1.1-30 | `allow_domains = ["ok.com"]`，请求 `https://other.com/x` | Reject（白名单模式） |
| TC-1.1-31 | `allow_domains` 与 `deny_domains` 同时命中 | Reject（deny 优先） |
| TC-1.1-32 | `ftp://example.com/x` | Reject，原因 `unsupported_scheme` |
| TC-1.1-33 | `file:///etc/passwd` | Reject |
| TC-1.1-34 | 空串 / 非 URL 文本 | Reject，原因 `invalid_url` |

**重定向裁决矩阵** `is_permitted_redirect(orig, target)`：

| ID | orig → target | 期望 |
|----|---------------|------|
| TC-1.1-40 | `https://a.com/1` → `https://a.com/2` | Permit（同 origin 改路径） |
| TC-1.1-41 | `https://a.com/1` → `https://www.a.com/1` | Permit（加 `www.`） |
| TC-1.1-42 | `https://www.a.com/1` → `https://a.com/1` | Permit（去 `www.`） |
| TC-1.1-43 | `https://a.com/1` → `https://b.com/1` | Deny（跨 host） |
| TC-1.1-44 | `https://a.com/1` → `http://a.com/1` | Deny（协议降级） |
| TC-1.1-45 | `https://a.com:443/1` → `https://a.com:8443/1` | Deny（端口变更） |
| TC-1.1-46 | `https://a.com/1` → `https://u:p@a.com/2` | Deny（目标含凭据） |
| TC-1.1-47 | `https://a.com/1` → `/relative/2` | Permit，且解析为 `https://a.com/relative/2` |
| TC-1.1-48 | `https://a.com/1` → `https://wwwa.com/1` | Deny（`www` 前缀剥离不得误伤） |
| TC-1.1-49 | target 为畸形 URL | Deny（不 panic） |

#### 8.2.2 `url_cache_test.rs`（单元）

| ID | 用例 | 断言 |
|----|------|------|
| TC-1.2-01 | 写入后立即读取 | 命中，内容一致 |
| TC-1.2-02 | 超过 TTL 后读取 | Miss（用可注入时钟推进，**禁止 `sleep`**） |
| TC-1.2-03 | TTL 边界（恰好等于 ttl） | 按实现定义的一侧断言，明确记录语义 |
| TC-1.2-04 | 写入总量超过 `cache_max_bytes` | 最早条目被驱逐，最新条目仍在 |
| TC-1.2-05 | 空响应体（0 字节） | 权重钳位为 1，不导致除零 / 无限驱逐 |
| TC-1.2-06 | key 为**原始 URL** 而非升级后 URL | 以 `http://x.com` 写入后，用 `http://x.com` 可命中，用 `https://x.com` 不命中 |
| TC-1.2-07 | 单条目超过 `cache_max_bytes` | 不缓存（或立即驱逐），且不影响已有条目 |
| TC-1.2-08 | 并发读写 | `tokio::spawn` 16 个任务混合读写，无死锁、无 panic |

#### 8.2.3 `html_to_md_test.rs`（单元）

| ID | 输入 | 期望 |
|----|------|------|
| TC-1.3-01 | `<h1>T</h1><p>body</p>` | `# T` + 段落 |
| TC-1.3-02 | `<a href="/x">l</a>` | Markdown 链接语法保留 |
| TC-1.3-03 | `<ul><li>a</li><li>b</li></ul>` | 无序列表 |
| TC-1.3-04 | `<pre><code>fn main(){}</code></pre>` | 代码块围栏保留 |
| TC-1.3-05 | `<script>` / `<style>` 内容 | 不出现在输出中 |
| TC-1.3-06 | 非 HTML content-type（`text/plain`） | **原样透传，不做转换** |
| TC-1.3-07 | `application/json` | 原样透传（保持可解析） |
| TC-1.3-08 | 畸形 / 未闭合 HTML | 不 panic，尽力输出 |
| TC-1.3-09 | 含多字节 UTF-8（中文 / emoji） | 不出现字符截断，`is_char_boundary` 安全 |
| TC-1.3-10 | 深度嵌套 1000 层的 div | 不栈溢出（如实现递归则需迭代化或加深度上限） |

#### 8.2.4 `fetch_tool_test.rs`（单元，用 mock summarizer）

| ID | 用例 | 断言 |
|----|------|------|
| TC-1.4-01 | 缺少 `url` 参数 | `is_error = true`，提示 missing |
| TC-1.4-02 | 缺少 `prompt` 参数 | `is_error = true` |
| TC-1.4-03 | `url` 非法 | `is_error = true`，内容含 `Invalid URL` |
| TC-1.4-04 | `summarizer = None` | 返回截断原文，且结果中注明「未启用摘要模型」 |
| TC-1.4-05 | `summarizer = Some(mock)` | 返回 mock 输出；mock 收到的 content 长度 ≤ `max_markdown_chars` |
| TC-1.4-06 | 内容超过 `max_markdown_chars` | 传给 summarizer 前已截断，且尾部含截断标记 |
| TC-1.4-07 | `preapproved_domains` 命中 + `text/markdown` + 短内容 | **不调用 summarizer**（mock 调用计数 = 0），直接返回原文 |
| TC-1.4-08 | 同上但内容超长 | **调用** summarizer（计数 = 1） |
| TC-1.4-09 | summarizer 返回 `Err` | `is_error = true`，错误信息透出但不含 API key |
| TC-1.4-10 | `describe(input)` | 返回 `Fetch <hostname>` 形式 |
| TC-1.4-11 | `describe` 输入 URL 非法 | 不 panic，返回降级文案 |
| TC-1.4-12 | trait 元数据 | `name()=="WebFetch"`、`category()==Network`、`is_deferred()==true`、`is_concurrency_safe()==true`、`max_result_size()==100_000` |
| TC-1.4-13 | description 内容 | 含 `WILL FAIL for authenticated or private URLs` 告警前缀 |

#### 8.2.5 `tests/web_fetch_test.rs`（集成，wiremock）

| ID | 场景 | 断言 |
|----|------|------|
| TC-1.5-01 | 200 + `text/html` | 返回 Markdown，`is_error = false` |
| TC-1.5-02 | 200 + `text/plain` | 原样返回 |
| TC-1.5-03 | 301 → 同 host 不同路径 | 自动跟随，返回最终内容 |
| TC-1.5-04 | 302 → 跨 host | **不跟随**；结果含 `REDIRECT DETECTED`、原 URL、目标 URL、状态码，以及「用新 URL 重新调用 WebFetch」的引导 |
| TC-1.5-05 | 307 / 308 跨 host | 同上，状态文案分别为 `Temporary Redirect` / `Permanent Redirect` |
| TC-1.5-06 | 同 host 重定向环（`/a`→`/b`→`/a`） | 在 `max_redirects` 跳后终止并报错，**不无限挂起** |
| TC-1.5-07 | 重定向响应缺 `Location` 头 | 报错 `Redirect missing Location header` |
| TC-1.5-08 | body 超过 `max_content_bytes` | 报错；且**流式中断**——mock 断言实际传输字节数远小于声明的总长 |
| TC-1.5-09 | 服务端延迟 > `timeout_secs` | 超时报错，耗时接近配置值 |
| TC-1.5-10 | 404 / 500 | `is_error = true`，含状态码 |
| TC-1.5-11 | `application/pdf` | 落盘到临时目录，结果尾部含 `[Binary content (application/pdf, ...) also saved to <path>]`，且文件确实存在 |
| TC-1.5-12 | 连续两次请求同一 URL | 第二次命中缓存，mock 收到的请求数 = 1 |
| TC-1.5-13 | 请求头 | mock 断言 `User-Agent` 为配置值、`Accept` 含 `text/markdown, text/html` |
| TC-1.5-14 | 执行中 `cancel()` | 及时返回，未继续读取响应体 |

#### 8.2.6 `agentrs-agent` 侧

| ID | 用例 | 位置 |
|----|------|------|
| TC-1.6-01 | `ProviderSummarizer` 用 mock provider，返回文本被正确 collect | `agentrs-agent/src/summarizer_test.rs` |
| TC-1.6-02 | provider 返回 `LlmEvent::Error` | 返回 `Err`，不 panic | 同上 |
| TC-1.6-03 | `confirm.rs` allow-list 含 `WebFetch:domain:example.com`，工具输入该域名 | `ConfirmResult::Approved` | `confirm_test.rs` |
| TC-1.6-04 | 同上，输入 `other.com` | 走确认流程（非自动批准） | 同上 |
| TC-1.6-05 | allow-list 仅含 `WebFetch`（无 domain 限定） | 任意域名均 Approved（向后兼容） | 同上 |
| TC-1.6-06 | `web.enabled = false` | bootstrap 后 registry 中**不含** `WebFetch` | `bootstrap_test.rs` |

---

### 8.3 Phase 2 — WebSearch

#### 8.3.1 `search_tool_test.rs`（单元，mock backend）

| ID | 用例 | 断言 |
|----|------|------|
| TC-2.1-01 | `query` 缺失 | `is_error = true` |
| TC-2.1-02 | `query` 长度 1 | `is_error = true`（min 2 边界） |
| TC-2.1-03 | `query` 长度 2 | 通过校验（边界） |
| TC-2.1-04 | 同时提供 `allowed_domains` 与 `blocked_domains`（均非空） | `is_error = true`，信息含 `Cannot specify both`（对齐原版 errorCode 2） |
| TC-2.1-05 | 二者其一为空数组 | **通过**（仅「均非空」才互斥） |
| TC-2.1-06 | 正常搜索 | 输出以 `Web search results for query: "<q>"` 开头 |
| TC-2.1-07 | 输出格式 | 含 `Links: ` 后跟 JSON 数组，元素为 `{"title","url"}` |
| TC-2.1-08 | 输出结尾 | **精确包含** `REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.` |
| TC-2.1-09 | 后端返回 0 条结果 | 输出含 `No links found.`，`is_error = false` |
| TC-2.1-10 | 后端 `supports_domain_filter() == false` + `blocked_domains` | 本地 post-filter 生效，被屏蔽域名不出现在输出中 |
| TC-2.1-11 | 后端 `supports_domain_filter() == true` | 过滤参数下推，mock 断言收到该参数；工具层不重复过滤 |
| TC-2.1-12 | 结果数超过 `max_results` | 截断到上限 |
| TC-2.1-13 | 后端返回 `Err` | `is_error = true`，**错误信息不含 API key** |
| TC-2.1-14 | trait 元数据 | `name()=="WebSearch"`、`category()==Network`、`is_deferred()==true`、`is_concurrency_safe()==true` |
| TC-2.1-15 | 结果含特殊字符（引号 / 换行 / 中文） | JSON 正确转义，输出可被 `serde_json` 反解 |

#### 8.3.2 后端实现（`tests/web_search_backend_test.rs`，wiremock + 真实响应样本）

| ID | 后端 | 场景 |
|----|------|------|
| TC-2.2-01 | Brave | 正常响应样本 → 正确解析出 `title` / `url` / `snippet` |
| TC-2.2-02 | Brave | 响应缺 `web.results` 字段 → 返回空列表而非 panic |
| TC-2.2-03 | Brave | 401（key 无效）→ `Err`，信息含状态码，**不回显 key** |
| TC-2.2-04 | Brave | 429（限流）→ `Err`，信息可辨识为配额问题 |
| TC-2.2-05 | Brave | 请求头断言：`X-Subscription-Token` 已设置 |
| TC-2.2-06 | Tavily | 正常响应样本解析 |
| TC-2.2-07 | Tavily | 域名过滤参数下推（`include_domains` / `exclude_domains`）断言 |
| TC-2.2-08 | SearXNG | 正常响应样本解析 |
| TC-2.2-09 | SearXNG | `base_url` 未配置 → 构造期返回配置错误 |
| TC-2.2-10 | 任一后端 | 超时 → `Err`，耗时接近 `search.timeout_secs` |
| TC-2.2-11 | 任一后端 | 响应体为畸形 JSON → `Err`，不 panic |

#### 8.3.3 注册期启用判定（`bootstrap_test.rs`）

| ID | 配置 | 断言 |
|----|------|------|
| TC-2.3-01 | `search.backend = "none"` | registry 中**不含** `WebSearch` |
| TC-2.3-02 | `backend = "brave"` 但 `api_key_env` 指向的环境变量未设置 | **不注册**，且发出 `warn` 级日志 |
| TC-2.3-03 | `backend = "brave"` + key 已设置 | 注册成功，`tool_names()` 含 `WebSearch` |
| TC-2.3-04 | `backend = "searxng"` + `base_url` 已配置（无需 key） | 注册成功 |
| TC-2.3-05 | 未注册时模型侧不可见 | `to_tool_defs()` 中无 `WebSearch` 条目 |

---

### 8.4 Phase 3 — Team

#### 8.4.1 `store_test.rs`（team file 持久化，单元 + tempfile）

| ID | 用例 | 断言 |
|----|------|------|
| TC-3.1-01 | 写入后读取 `TeamFile` | 字段完全一致（round-trip） |
| TC-3.1-02 | 读取不存在的 team | 返回 `None`，不报错 |
| TC-3.1-03 | 团队目录不存在时写入 | 自动递归创建 |
| TC-3.1-04 | 名称 sanitize | `"My Team!"` → 目录名 `my-team-`，与原版规则一致 |
| TC-3.1-05 | 名称含路径穿越 `"../../etc"` | sanitize 后**不逃逸**团队根目录（路径穿越防护） |
| TC-3.1-06 | agent 名含 `@` | sanitize 为 `-`，避免 `name@team` 歧义 |
| TC-3.1-07 | 损坏的 JSON | 返回错误，记 `warn` 日志，不 panic |
| TC-3.1-08 | 路径构造 | 全部经 `Path::join`，Windows 下断言无 `/` 硬编码（`#[cfg(windows)]` 分支） |

#### 8.4.2 `registry_test.rs` / `mailbox_test.rs`（单元）

| ID | 用例 | 断言 |
|----|------|------|
| TC-3.2-01 | 注册两个同名 teammate | 第二次返回 `TeamError::DuplicateName` |
| TC-3.2-02 | 颜色分配 | 前 N 个成员颜色互不相同；超出色板后按序回绕 |
| TC-3.2-03 | 向已注册成员投递 | `DeliveryReport.delivered == 1`；接收端 channel 收到 |
| TC-3.2-04 | 向不存在的成员投递 | `TeamError::UnknownRecipient`，信息列出可用成员名 |
| TC-3.2-05 | 广播 `"*"` 到 3 名成员 | 全部收到；`delivered == 3`；**发送者自身不收到** |
| TC-3.2-06 | 广播时团队仅 leader | `delivered == 0`，返回提示而非错误 |
| TC-3.2-07 | 成员数达上限（默认 8）后再加 | 拒绝并给出上限提示 |
| TC-3.2-08 | 投递后文件镜像 | inbox JSON 文件内容与内存消息一致 |
| TC-3.2-09 | 接收方 channel 已满 | 不阻塞发送方（有界队列 + 明确的背压/丢弃语义并断言之） |
| TC-3.2-10 | 接收方已退出 | 投递返回 `TeamError::RecipientGone`，不 panic |
| TC-3.2-11 | 并发投递 | 16 个任务并发发送，全部消息不丢不重 |

#### 8.4.3 `inject_test.rs`（收件注入，单元）

| ID | 用例 | 断言 |
|----|------|------|
| TC-3.3-01 | 单条未读消息 | 注入内容形如 `<teammate-message from="alice">…</teammate-message>` |
| TC-3.3-02 | 多条未读 | 按时间序拼接，各自独立包裹 |
| TC-3.3-03 | 无未读 | 不注入任何内容（不产生空 user 消息） |
| TC-3.3-04 | drain 后再次调用 | 已读消息**不重复注入** |
| TC-3.3-05 | 消息正文含 `<`/`>`/`&` | 正确转义，不破坏标签结构 |
| TC-3.3-06 | 注入时机 | 发生在工具轮次边界，而非打断进行中的工具调用（用状态机断言） |

#### 8.4.4 工具层（`create_tool_test.rs` / `delete_tool_test.rs` / `send_tool_test.rs`，mock `TeamRuntime`）

| ID | 工具 | 用例 | 断言 |
|----|------|------|------|
| TC-3.4-01 | TeamCreate | `team_name` 缺失 | `is_error = true` |
| TC-3.4-02 | TeamCreate | `team_name` 为空白串 | `is_error = true`（对齐原版 errorCode 9） |
| TC-3.4-03 | TeamCreate | 正常创建 | 返回 `team_name` / `team_file_path` / `lead_agent_id`；`lead_agent_id` 形如 `team-lead@<team>` |
| TC-3.4-04 | TeamCreate | 名称已存在 | **自动改名**并返回新名，不报错 |
| TC-3.4-05 | TeamCreate | 已在带队中再次创建 | `is_error = true`，信息含 `Already leading team` 与 `TeamDelete` 提示 |
| TC-3.4-06 | TeamDelete | 存在 1 个活跃非 lead 成员 | `is_error`/`success=false`，信息**列出成员名**并提示先 `requestShutdown` |
| TC-3.4-07 | TeamDelete | 成员 `is_active = false`（空闲） | 允许删除 |
| TC-3.4-08 | TeamDelete | 仅剩 lead | 允许删除，目录被清理 |
| TC-3.4-09 | TeamDelete | 当前无 team | `success = true`，信息为 `nothing to clean up`（幂等） |
| TC-3.4-10 | SendMessage | `to` 缺失 / `message` 缺失 | `is_error = true` |
| TC-3.4-11 | SendMessage | `to` 为未知成员 | `is_error = true`，列出可用成员名 |
| TC-3.4-12 | SendMessage | `to = "*"` | 调用 runtime 的广播路径，返回投递数 |
| TC-3.4-13 | SendMessage | `summary` 缺失 | 按 schema 规则处理（必填则报错 / 选填则用正文截断），断言与 schema 一致 |
| TC-3.4-14 | 三工具 | trait 元数据 | `category() == Team`、`is_deferred() == true` |
| TC-3.4-15 | 三工具 | `team.enabled = false` 时 | bootstrap 后 registry 中均不存在 |

#### 8.4.5 生命周期（`tests/team_lifecycle_test.rs`，集成，**R4 回归**）

| ID | 场景 | 断言 |
|----|------|------|
| TC-3.5-01 | 全链路：create → spawn teammate → send → teammate 下一轮收到 → delete | 各阶段状态正确，最终目录清理 |
| TC-3.5-02 | `TeamDelete` 后 | 所有 `TeammateHandle` 的 `JoinHandle` 已 join 完成，任务数归零 |
| TC-3.5-03 | 未调用 `TeamDelete` 直接结束 session | 清理路径同样触发，无遗留 tokio 任务（`tokio::runtime::Handle::metrics` 或计数器断言） |
| TC-3.5-04 | teammate 内部 panic | 不拖垮 leader；成员标记为 `is_active = false`；`TeamDelete` 可正常执行 |
| TC-3.5-05 | teammate 长时间运行时收到 cancel | 在超时窗口内退出 |
| TC-3.5-06 | 连续 create/delete 10 轮 | 无句柄泄漏、无目录残留、内存不单调增长 |
| TC-3.5-07 | `shutdown_request` → `shutdown_response(approve=true)` | teammate 优雅退出 |
| TC-3.5-08 | `shutdown_response(approve=false)` | teammate 继续运行 |

---

### 8.5 跨阶段：回归与跨平台

| ID | 用例 | 位置 |
|----|------|------|
| TC-4.0-01 | 现有 8 个工具的 `describe` / `category` / `is_deferred` 未被改动 | `tests/tool_description_test.rs`（扩充断言） |
| TC-4.0-02 | 新增工具全部出现在 `to_tool_defs()` 且 schema 可被 `serde_json` 校验为合法 JSON Schema | `tests/tool_description_test.rs` |
| TC-4.0-03 | 新增工具的 description 非空、含使用指引关键字 | 同上（对齐既有 TC-4.2-\* 风格） |
| TC-4.0-04 | `ToolPolicy` 可禁用 `WebFetch` / `WebSearch` / Team 三件套 | `agentrs-agent/src/tool_policy_test.rs` |
| TC-4.0-05 | 子 agent 继承的工具策略不会恢复父级已禁用的网络工具 | 同上 |
| TC-4.1-01 | Windows：team 目录与 webfetch 落盘目录路径正确 | `#[cfg(windows)]` 分支 |
| TC-4.1-02 | Unix：同上 | `#[cfg(unix)]` 分支 |
| TC-4.2-01 | 日志中不出现 API key、页面正文、消息正文 | 用 `tracing-subscriber` 捕获层断言（新增 `tests/log_redaction_test.rs`） |

---

### 8.6 覆盖率与门禁

| 模块 | 要求 |
|------|------|
| `web/url_policy.rs` | **分支覆盖 100%**（安全关键，R1） |
| `web/url_cache.rs`、`web/html_to_md.rs` | 行覆盖 ≥ 90% |
| `web/fetch_tool.rs`、`web/search_tool.rs` | 行覆盖 ≥ 85% |
| `team/*` | 行覆盖 ≥ 85%，生命周期用例（TC-3.5-\*）必须全绿 |

**CI 门禁**：`cargo clippy`（零告警）→ `cargo fmt --all --check` → `cargo test`，三平台（Linux / macOS / Windows）均须通过。推送使用 `just push`。

**测试禁忌**：

- 禁止在单测中发起真实网络请求（一律 wiremock / mock backend）
- 禁止用 `thread::sleep` / `tokio::time::sleep` 验证 TTL 与超时（用可注入时钟或 `tokio::time::pause()`）
- 禁止在测试中写入用户真实 `~/.agentrs` 目录（一律 `tempfile::TempDir`）
- 禁止只断言 `is_error == false` 而不校验内容

### 8.7 测试骨架示例

```rust
// agentrs-tools/src/web/url_policy_test.rs
use super::{UrlPolicy, RejectReason};
use crate::web::WebPolicyConfig;

fn policy() -> UrlPolicy {
    UrlPolicy::new(WebPolicyConfig::default())
}

// --- TC-1.1-17: cloud metadata endpoint must be rejected ---
#[test]
fn rejects_cloud_metadata_endpoint() {
    let rejected = policy().check("https://169.254.169.254/latest/meta-data/");
    assert!(
        matches!(rejected, Err(RejectReason::PrivateHost { .. })),
        "link-local metadata address must be rejected, got {rejected:?}"
    );
}

// --- TC-1.1-29: deny_domains must not match by bare suffix ---
#[test]
fn deny_domain_does_not_match_unrelated_suffix() {
    let mut config = WebPolicyConfig::default();
    config.deny_domains = vec!["evil.com".to_owned()];
    let policy = UrlPolicy::new(config);

    assert!(policy.check("https://evil.com/x").is_err());
    assert!(policy.check("https://sub.evil.com/x").is_err());
    assert!(
        policy.check("https://notevil.com/x").is_ok(),
        "suffix-only match would wrongly block an unrelated domain"
    );
}
```

```rust
// agentrs-tools/tests/web_fetch_test.rs
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// --- TC-1.5-04: cross-host redirect is reported, not followed ---
#[tokio::test]
async fn cross_host_redirect_is_reported_to_the_model() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/start"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://elsewhere.example/final"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let tool = test_web_fetch_tool(&server);
    let result = tool
        .execute(serde_json::json!({
            "url": format!("{}/start", server.uri()),
            "prompt": "summarize",
        }))
        .await;

    assert!(!result.is_error, "a reported redirect is not a tool failure");
    assert!(result.content.contains("REDIRECT DETECTED"));
    assert!(result.content.contains("https://elsewhere.example/final"));
}
```

---

## 附录 A：关键源码位置索引

**agentrs 侧**

| 位置 | 内容 |
|------|------|
| `agentrs-tools/src/tool.rs:44` | `Tool` trait 定义 |
| `agentrs-tools/src/registry.rs:19` | `ToolRegistry::register` |
| `agentrs-tools/src/view_image.rs` | 最接近本次需求的工具实现范例（含 `execute_with_follow_up`） |
| `agentrs-types/src/tool.rs:35` | `ToolDef` / `ToolResult` |
| `agentrs-types/src/llm.rs:8` | `LlmRequest` / `LlmEvent` |
| `agentrs-types/src/spawner.rs` | **trait 注入模式的先例** |
| `agentrs-protocol/src/events.rs:98` | `ToolCategory` 枚举 |
| `agentrs-agent/src/bootstrap.rs:219` | `build_builtin_registry` |
| `agentrs-agent/src/bootstrap.rs:335` | `register_agent_tools`（注入点） |
| `agentrs-agent/src/spawner.rs:65` | `AgentSpawner::spawn_one` |
| `agentrs-agent/src/spawn_tool.rs` | 依赖注入型工具范例 |
| `agentrs-agent/src/confirm.rs:36` | 权限确认（需扩展 domain 粒度） |
| `agentrs-config/src/config.rs:218` | `ToolsConfig` |

**claude-code-main 侧**

| 位置 | 内容 |
|------|------|
| `src/tools/WebFetchTool/utils.ts` | 抓取 / 缓存 / 重定向 / 摘要核心（531 行） |
| `src/tools/WebFetchTool/WebFetchTool.ts:104` | domain 粒度权限判定 |
| `src/tools/WebFetchTool/preapproved.ts` | 预批准主机白名单 |
| `src/tools/WebSearchTool/WebSearchTool.ts:254` | 嵌套 LLM 会话 + 服务端工具 |
| `src/tools/TeamCreateTool/TeamCreateTool.ts:128` | 建队流程 |
| `src/tools/TeamDeleteTool/TeamDeleteTool.ts:71` | 活跃成员检查与清理 |
| `src/tools/SendMessageTool/SendMessageTool.ts:741` | 消息路由（uds / bridge / in-process / mailbox） |
| `src/utils/teammateMailbox.ts` | 文件邮箱 + lockfile（1183 行） |
| `src/utils/swarm/teamHelpers.ts` | `TeamFile` 结构与读写 |
| `src/utils/swarm/backends/types.ts` | 后端抽象（tmux / iterm2 / in-process） |
