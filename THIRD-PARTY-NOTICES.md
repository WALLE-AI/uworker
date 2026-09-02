# 第三方来源与归属

本仓库部分模块自 **agentrs**（Apache-2.0）移植。移植是一次性的，不与上游建立 rebase 关系。

## Apache-2.0 义务落实

| 条款 | 落实方式 |
|---|---|
| §4(a) 附带许可证副本 | 仓库根目录 [`LICENSE-APACHE`](LICENSE-APACHE) |
| §4(b) 修改文件须显著标注 | 每个移植文件头部标注来源/commit/修改摘要，由 `scripts/check-port-attribution.sh` 在 CI 强制 |
| §4(c) 保留原有版权声明 | 不删除源文件任何版权头 |
| §4(d) NOTICE | agentrs 无 NOTICE 文件，无此项义务；本文件为主动披露 |
| §6 商标 | 不在 crate 名、模块名、公开 API 或文档中使用 `agentrs` 相关标识 |

文件头模板：

```rust
// Ported from agentrs (Apache-2.0).
//   Source: <repo-url>/crates/agentrs-providers/src/framing.rs @ <commit>
//   Copied: 2026-xx-xx   Modified: yes
//   Changes: 替换错误类型为 agentrs-contracts::ProviderError；移除全局 config 依赖。
```


## agentrs 全量并入（Apache-2.0）

`crates/agentrs-*` 十三个 crate 自 agentrs **原样并入**，未做任何改写：

    agentrs-types  agentrs-protocol  agentrs-compact  agentrs-process  agentrs-config
    agentrs-providers  agentrs-tools  agentrs-mcp  agentrs-skills  agentrs-memory
    agentrs-agent  agentrs-tui  agentrs-cli

- 来源：`https://github.com/iOfficeAI/agentrs` @ `f7111746015d8e6f960e1568a805ceef975022d3`
- 许可：Apache-2.0（与本仓库同）。原 `LICENSE` 随 `workspace-hack/` 一并保留。
- 改动（Apache-2.0 §4b 要求标注）：**仅两处，均为构建所需，不涉及逻辑**
  1. 各 crate 的 `edition.workspace = true` 改为 `edition = "2024"`、
     `rust-version` 改为 `1.88`——agentrs 用 edition 2024（let-chain），
     本仓库其余部分是 2021，两者在同一 workspace 里必须各自声明；
  2. workspace 依赖里 `unicode-width` 由 `0.2.2` 收为 `=0.2.0`——
     `ratatui 0.29`（`agentrs-dev-tui` 用）精确锁定该版本。
- 门禁：`scripts/check-no-env.sh` 与 `check-port-attribution.sh` 对
  `crates/agentrs-*` 整体豁免。它们按自己的架构直接碰 fs/进程/网络，那是其设计。
  **`agentrs-*` 那 15 个 crate 的判据一个字也没放松。**

---

## 移植清单

**清单在移植发生时逐条维护，不允许发布前突击补写**——事后回忆哪些文件是搬的必然会漏。

### A 类：直接复制（约 6,800 行）

源仓库 commit：`a5df989d110fb424bcd496b413e7ce7e20754414`

| 目标文件 | 源路径 | 行数 | 状态 |
|---|---|---:|---|
| `crates/agentrs-types/src/message.rs` | `crates/agentrs-types/src/message.rs` | 237 | ✅ W2 |
| `crates/agentrs-types/src/llm.rs` | `crates/agentrs-types/src/llm.rs` | 53 | ✅ W2 |
| `crates/agentrs-provider/src/sse.rs` | `crates/agentrs-providers/src/framing.rs` @ `f711174` | 25 | ✅ **仅 `bedrock_payload_to_frame` 一函数**。SSE 行解帧本仓库独立写成（更早、且是纯函数无 I/O），不移植 |
| — | `crates/agentrs-providers/src/parser.rs` | | **不移植**：其职责已由本仓库的 `sse.rs` + 各投影器覆盖 |
| `crates/agentrs-provider/src/anthropic.rs` | `crates/agentrs-providers/src/anthropic_shared.rs` @ `f711174` | 441 | ✅ Phase B（剥离 sanitize/tracing/generate_tool_id，增加 cache_control 断点） |
| — | `crates/agentrs-providers/src/openai.rs` | | **不移植**：本仓库 `openai.rs` 已独立写成且更完整（488 行 vs 269） |
| `crates/agentrs-provider/src/anthropic_wire.rs` | `crates/agentrs-providers/src/{bedrock,vertex}.rs` @ `f711174` | 707 | ✅ Phase B（**只取线格式**；凭据链与 SigV4 签名按 §1.1 归 Core，未移植） |
| `crates/agentrs-provider/src/openai_responses.rs` | `crates/agentrs-providers/src/openai_responses{,_projector}.rs` @ `f711174` | 447 | ✅ Phase B（剥离 generate_call_id / orphan 清理 / 静默降级） |
| `crates/agentrs-provider/src/compat.rs` | `crates/agentrs-config/src/compat.rs` | 106 | ✅ 借鉴形态，剥离配置加载（配置归 Core 注入）——见该文件头部说明 |
| `crates/agentrs-context/src/cache_diagnostics.rs` | `crates/agentrs-agent/src/cache_diagnostics.rs` @ `f711174` | 164 | ✅ Phase B（归因改走 cache.rs 分段；新增 Unsupported 判定） |
| `crates/agentrs-context/src/compact/mod.rs` | `crates/agentrs-compact/src/{lib,api}.rs` @ `f711174` | 40 | ✅（两文件合一；补上"哪一级会改内容、因此不能用在 Read 上"的判据） |
| `crates/agentrs-context/src/compact/level.rs` | `crates/agentrs-compact/src/level.rs` @ `f711174` | 45 | ✅（`Default` 改手写以便把"为什么默认 Safe"写在旁边） |
| `crates/agentrs-context/src/compact/sanitize.rs` | `crates/agentrs-compact/src/sanitize.rs` @ `f711174` | 60 | ✅（ANSI 剥离改手写状态机，**去掉 `regex` 依赖**；`merge_blank_lines` 三分支重写为等价的两分支） |
| `crates/agentrs-context/src/compact/fold.rs` | `crates/agentrs-compact/src/fold.rs` @ `f711174` | 55 | ✅（**修了一处缺陷**：相似度分子数字符、分母数字节，中文行比值恒大于 1，任意两行中文都判为相似） |
| `crates/agentrs-context/src/compact/json.rs` | `crates/agentrs-compact/src/json.rs` @ `f711174` | 60 | ✅（let-chain 改嵌套 if，本仓库是 edition 2021） |
| `crates/agentrs-context/src/compact/toon.rs` | `crates/agentrs-compact/src/toon.rs` @ `f711174` | 105 | ✅（let-chain 改写；消除对已校验值的二次 `unwrap`；说明文本改中文） |

### B 类：复制逻辑，改造接缝（约 2,900 行）

| 目标文件 | 源路径 | commit | 改造要点 | 状态 |
|---|---|---|---|---|
| `crates/agentrs-skills/src/pack/types.rs` | `crates/agentrs-skills/src/types.rs` | `f711174` | 去掉 `SkillSource`/`LoadedFrom`/`skill_root`——**"技能从哪个目录发现的"归 Core**，内核不做发现；删掉两个从未被读取的字段 | ✅ |
| `crates/agentrs-skills/src/pack/frontmatter.rs` | `crates/agentrs-skills/src/frontmatter.rs` | `f711174` | 解析失败改为返回 `ParseOutcome` 三态，不再 `tracing::warn!` 后吞掉（内核不持有日志设施，且"技能装上了却不生效"该是调用方看得见的事）；花括号展开加 256 条上限；`content_length` 改按字符数（原按字节，中文高估三倍）；let-chain 改写为 edition 2021 语法 | ✅ |
| `crates/agentrs-skills/src/pack/mod.rs` | `crates/agentrs-skills/src/frontmatter.rs`（模块文档） | `f711174` | 新写的模块文档，说明"只解析不发现"这条边界 | ✅ |
| `crates/agentrs-skills/src/pack/permissions.rs` | `crates/agentrs-skills/src/permissions.rs` | `f711174` | **修了一处缺陷**：`Prefix` 规则改为要求前缀以分隔符结尾——上游把 `db*` 也存成 `Prefix("db")`，于是它命中 `database`，而文档说要防的正是这件事；`auto_approve` 改名 `assume_yes` 并写明它绕不过 deny | ✅ |
| `crates/agentrs-skills/src/pack/listing.rs` | `crates/agentrs-skills/src/prompt.rs` | `f711174` | `SkillSource::Bundled` 换成调用方传入的 `pinned` 名字集（"哪些优先"是优先级，不是发现来源）；预算改显式传入，不再第二次反算 token；两处各写一遍的截断合并为一个函数（原来 `>=`/`-1` 边界不一致，同一描述在两种降级模式下截在不同位置） | ✅ |
| `crates/agentrs-skills/src/pack/bridge.rs` | `crates/agentrs-skills/src/context_modifier.rs` | `f711174` | 本仓库 `ContextModifier` 只表达**单调收窄**，没有 model/effort 字段；`allowed_tools` 映到 `tool_subset`，model/effort 单独返回——它们不是收窄，混进去会让合并语义含糊 | ✅ |
| `crates/agentrs-skills/src/pack/substitution.rs` | `crates/agentrs-skills/src/substitution.rs` | `f711174` | 三遍正则改写为**一遍扫描**，去掉 `regex` 依赖，并**修掉一处缺陷**：上游前一遍替换进去的值会被后一遍再扫一次，参数值里含 `$1` 就会被二次替换；占位符前缀改 `AGENTRS_` | ✅ |
| `crates/agentrs-skills/src/pack/conditional.rs` | `crates/agentrs-skills/src/conditional.rs` | `f711174` | 用本仓库的 `agentrs_types::glob` 而非 `glob` crate（两份 glob 语义会分叉）；去掉 `cwd` 与相对化——内核不知道 cwd，直接收工作区相对路径；`HashMap` 改 `BTreeMap` 使**激活顺序确定**（上游明说不保证，而不确定的顺序让 trajectory 不可重放） | ✅ |
| `crates/agentrs-memory/src/index.rs` | `crates/agentrs-memory/src/{index,types}.rs` | `f711174` | **只取纯逻辑那半**：`truncate_index` + `IndexTruncation`；三个碰磁盘的函数不移植（索引读写归 Core）；不稳定的 `floor_char_boundary` 换成 `is_char_boundary` 回退 | ✅ |
| `crates/agentrs-types/src/glob.rs` | — | | 非移植：本仓库原有的 glob 匹配器从 `agentrs-tools` 上移到 `agentrs-types`，好让工具与技能只用一份 | — |
| `crates/agentrs-tools/src/mcp/wire.rs` | `crates/agentrs-mcp/src/protocol.rs` | `f711174` | **只取线格式**：进程生命周期、stdio 传输、凭据按 §1.1 归 Core，未移植；两个方向都实现 `Serialize + Deserialize`（上游各只实现一个，宿主没法回放录下来的会话）；补 `McpToolResult::text()` 与 `has_non_text()`——图文混排时默默丢图会让模型以为工具什么也没返回 | ✅ |
| `crates/agentrs-types/src/compact.rs` | `crates/agentrs-types/src/compact.rs` | `f711174` | 逐字复制，仅改 crate 路径 | ✅ |
| `crates/agentrs-tools/src/tool_policy.rs` | `crates/agentrs-agent/src/tool_policy.rs` | `f711174` | 逐字复制 | ✅ |
| `crates/agentrs-context/src/strategy/mod.rs` | `crates/agentrs-agent/src/compact/mod.rs` | `f711174` | 合并 `compact/mod.rs`：并入 `context_usage` 与 config | ✅ |
| `crates/agentrs-context/src/strategy/estimate.rs` | `crates/agentrs-agent/src/compact/estimate.rs` | `f711174` | 逐字复制；`ImageUrl` 无 `decoded_byte_size()`，改为就地从 data URI 的 base64 段估算 | ✅ |
| `crates/agentrs-context/src/strategy/state.rs` | `crates/agentrs-agent/src/compact/state.rs` | `f711174` | 逐字复制 | ✅ |
| `crates/agentrs-context/src/strategy/auto.rs` | `crates/agentrs-agent/src/compact/auto.rs` | `f711174` | `LlmProvider` 换成注入的 `Summarizer` trait（context 依赖 provider 会把分层反过来且拖进 reqwest——与 runtime 用 `StepDriver` 同一理由）；`mpsc::Receiver` 换 `Vec<LlmEvent>`（本仓库 provider 契约一次返回全部事件，于是不再需要 tokio）；let-chain 改 edition 2021 写法 | ✅ |
| `crates/agentrs-context/src/strategy/micro.rs` | `crates/agentrs-agent/src/compact/micro.rs` | `f711174` | `Utc::now()` 换成调用方传入的 `Timestamp`；`HashMap<String,_>` 改 `HashMap<ToolCallId,_>` 以匹配本仓库类型 | ✅ |
| `crates/agentrs-context/src/strategy/emergency.rs` | `crates/agentrs-agent/src/compact/emergency.rs` | `f711174` | 逐字复制；`agentrs_config::compact` 路径改写 | ✅ |
| `crates/agentrs-context/src/strategy/prompt.rs` | `crates/agentrs-agent/src/compact/prompt.rs` | `f711174` | 逐字复制 | ✅ |
| `crates/agentrs-context/src/strategy/usage.rs` | `crates/agentrs-agent/src/context_usage.rs` | `f711174` | 逐字复制后 `DateTime<Utc>`→`Timestamp`、`Utc::now()`→调用方传入——内核经 Clock port 拿时刻，直接读真实时钟会让 `cargo test --workspace` 不再确定 | ✅ |
| `crates/agentrs-context/src/strategy/config.rs` | `crates/agentrs-config/src/compact.rs` | `f711174` | 逐字复制。落在 context 而不是新开 config crate——本仓库配置一律注入 | ✅ |
| `crates/agentrs-runtime/src/plan_state.rs` | `crates/agentrs-agent/src/plan/state.rs` | `f711174` | 逐字复制 | ✅ |
| `crates/agentrs-prompts/src/plan_prompt.rs` | `crates/agentrs-agent/src/plan/prompt.rs` | `f711174` | 逐字复制 | ✅ |
| `crates/agentrs-dev-adapter/src/vcr.rs` | `crates/agentrs-agent/src/vcr.rs` | `f711174` | 落在 dev-adapter（要读写磁盘，testkit 受门禁管）；`anyhow`→`std::io`，去 `tracing`，录制时刻由调用方给 | ✅ |
| `crates/agentrs-types/src/skill_types.rs` | `crates/agentrs-types/src/skill_types.rs` | `f711174` | 逐字复制 | ✅ |
| `crates/agentrs-types/src/file_state.rs` | `crates/agentrs-types/src/file_state.rs` | `f711174` | 逐字复制 | ✅ |
| _(技能发现归 Core，不移植)_ | `discovery.rs` / `loader.rs` / `watcher.rs` / `paths.rs` | | 文件在哪、哪层优先、监视变化——按 §1.1 归 Core | 不移植 |

### 经核查后判定不移植

下列模块在 agentrs 中存在，但移植进来会**削弱**本仓库已有的机制。逐条记录，
免得下一个人再查一遍：

| agentrs 模块 | 不移植的理由 |
|---|---|
| `agentrs-providers/src/retry.rs` | `routing.rs` 已有重试，且 `provider/lib.rs` 有一条更强的不变量：**已产出可见文本后不自动重试**。上游的 `with_retry` 只看 `is_retryable()`，会重复已输出的内容 |
| `agentrs-providers/src/tool_call_sanitize.rs` | `legalization.rs`（576 行）覆盖同一职责，且带四条不变量与 manifest 留痕。本仓库移植 `anthropic.rs` 时**已显式剥离** sanitize |
| `agentrs-memory/src/{store,index,paths}.rs` | `agentrs-memory` 的边界：索引、权限、保留策略全归 Core，内核只从候选里挑 |
| `agentrs-skills/src/{discovery,loader,watcher,paths}.rs` | 同上——文件在哪、哪层优先、监视变化归 Core |
| `agentrs-config` 整个 crate | 本仓库配置一律注入，不设配置 crate（架构 §1.1） |
| `agentrs-protocol` 整个 crate | 它是 agentrs 与 AionCore 之间的 IPC 契约。本仓库有自己的事件契约与 `serve --jsonl`——移植它等于引入**第二套**互不兼容的对外协议 |
| `agentrs-providers/src/{composed,stream_runner,stream_process}.rs` | 内部装配，本仓库的 `transport.rs` + `routing.rs` 已覆盖同一职责 |
| `agentrs-providers/src/stream_diagnostics.rs` | 依赖 `reqwest::HeaderMap` 与 `tracing`；本仓库诊断走 `agentrs-observability`，内核库不持有日志设施 |
| `agentrs-types/src/spawner.rs` | `ForkOverrides{model,effort,allowed_tools}` 与本仓库 `pack::SkillOverrides` 同形；派生规则归 `agentrs-subagents`，那里带授权校验 |
| `agentrs-types/src/file_state.rs` + `agentrs-tools/src/file_cache.rs` | 读去重缓存依赖文件 mtime，属宿主状态；本仓库 ChangeSet overlay 已保证读己之写 |
| `agentrs-skills/src/mcp.rs` | 依赖 `agentrs_mcp::manager`（进程生命周期）与 `loader`（发现），两者均属 C 类 |
| `agentrs-skills/src/hooks.rs` | 依赖 `agentrs_config::hooks`；本仓库 hooks 走 `HookEvaluator` port，形状不同 |
| `agentrs-memory/src/prompt.rs` | 主体是 2,500 token 的提示词正文 + 读盘。提示词归 `agentrs-prompts` 与 Core，不随 memory 移植 |

### C 类：明确不移植

`agentrs-tui`、`engine/session/orchestration/bootstrap/confirm`、`agentrs-config` 全局加载、
`agentrs-skills` 的 shell/executor/discovery、工具执行体、`agentrs-process`。

**C 类不得以"临时/调试用"为由进入仓库**——尤其 `agentrs-process`，它一进来"内核无执行权"就破了。

---

## dsh-code-agent（MIT）—— `agentrs-dev-tui` 的交互形式

`agentrs-dev-tui` 的界面与键盘模型自 **dsh-code-agent**（`packages/dsh-tui`，MIT）
逐模块移植，commit `d7cd008`。移植的是**交互形式**：字形集、语义色板、转录条目与
折叠预算、工具卡片、连续查阅合并、working line、状态行的分优先级丢弃、通知队列、
草稿编辑模型、审批选项行、按键表。原实现是 TypeScript/Ink，本仓库是 Rust/Ratatui，
因此没有一行代码是逐字复制的；但设计、判据与行为是它的，所以按 MIT 的要求署名。

**不移植**：其 `src/ink/` 分叉渲染器、`brand.ts` 的品牌美术（DeepSeek 鲸鱼与字体表，
另有其自身的第三方归属）、`harness-adapter.ts` / `plugin.ts`（对接 DeepSeek Harness，
与 AgentRS 无关）、会话持久化与 `$DSH_HOME` 布局。

| 目标文件 | 源路径（`packages/dsh-tui/src/`） | 改造要点 |
|---|---|---|
| `crates/agentrs-dev-tui/src/text.rs` | `terminal-text.ts` | 宽度改用 `unicode-width` crate，替换原手写区间表 |
| `crates/agentrs-dev-tui/src/glyphs.rs` | `glyphs.ts` | 增补树形前缀；去掉未用的 `reasoning` |
| `crates/agentrs-dev-tui/src/theme.rs` | `theme.ts` + `styling.ts` | 色调解析为 ratatui `Style`；`ansi256` 并入 `Basic` |
| `crates/agentrs-dev-tui/src/styling.rs` | `styling.ts` | 分段切分按 `char` 而非 UTF-16 码元；随美术一并去掉字面色 |
| `crates/agentrs-dev-tui/src/capabilities.rs` | `terminal-capabilities.ts` | 探测接受已读入的环境快照；终端尺寸交给 ratatui 逐帧上报 |
| `crates/agentrs-dev-tui/src/spinner.rs` | `spinner.ts` | 行为不变 |
| `crates/agentrs-dev-tui/src/markdown.rs` | `markdown.ts` | 七个固定形状手写匹配，不引入正则引擎；优先级与输出不变 |
| `crates/agentrs-dev-tui/src/diff.rs` | `diff-view.ts` | 显式建模"文件被删"；行携带色调枚举而非标记字符 |
| `crates/agentrs-dev-tui/src/tool_card.rs` | `tool-card.ts` | 卡片种类取自显式名字表（`ToolDef` 无展示层字段）；未知名字仍落 `Generic` |
| `crates/agentrs-dev-tui/src/transcript.rs` | `transcript-view.ts` | 去掉 scrollback 切分与行缓存（本宿主保留 alternate screen） |
| `crates/agentrs-dev-tui/src/collapse.rs` | `collapse.ts` | 单条规则直接应用，不做规则注册表 |
| `crates/agentrs-dev-tui/src/working_line.rs` | `working-line.ts` | 省略号取自字形集；去掉子 Agent 行 |
| `crates/agentrs-dev-tui/src/status_line.rs` | `status-line.ts` | 段位按 AgentRS 实有投影调整：无 todo/subagent，增 ChangeSet 与丢弃计数 |
| `crates/agentrs-dev-tui/src/notices.rs` | `notifications.ts` | 每条通知的 `fold` 闭包换成内建重复计数，队列保持可比较的纯值 |
| `crates/agentrs-dev-tui/src/composer.rs` | `composer.ts` | 草稿历史留在进程内；括号粘贴标记无需剥离（crossterm 有独立事件） |
| `crates/agentrs-dev-tui/src/approval.rs` | `approval-options.ts` | "放宽"一行改为切换下一次 run 的 `PermissionMode`（内核无会话内切换通路） |
| `crates/agentrs-dev-tui/src/keymap.rs` | `keymap.ts` | 和弦由 crossterm 事件构造；动作集取本宿主能兑现的子集 |
| `crates/agentrs-dev-tui/src/keybindings.rs` | `keybindings.ts` | 不支持多键序列（无动作需要，且 16 ms 帧循环里再挂一个一秒前缀计时器代价真实） |
| `crates/agentrs-dev-tui/src/overlay.rs` | `overlay.ts` + `session-browser.ts` + `session-selector.ts` + `transcript-mode.ts` | 一个窗口模型服务四个面，不是四份近似副本 |
| `crates/agentrs-dev-tui/src/surfaces.rs` | `overlay.ts`(helpRows) + `session-browser.ts` | 命令集是本宿主的；会话浏览器改为 durable JSONL 日志浏览器 |
| `crates/agentrs-dev-tui/src/completion.rs` | `draft-completion.ts` + `workspace-files.ts` | 候选由调用方提供，匹配器保持纯函数，唯一一次目录读取落在 `host_io` |
| `crates/agentrs-dev-tui/src/ui.rs` | `views/*.tsx` + `app.tsx` | React/Ink → ratatui：同样的布局与丢弃顺序，改为命令式绘制 |

### MIT 许可证副本

```
MIT License

Copyright (c) 2026 dsh-code-agent contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
