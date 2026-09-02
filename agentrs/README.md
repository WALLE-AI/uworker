# AgentRS

uworker 的推理与工作流内核：Rust 编写、可嵌入、**无执行权**。

它把"用户目标 + 已授权上下文 + 可用能力"推进为可审计的步骤流。它**不**拥有 UI、
数据库、用户身份、策略规则、文件写权限或 OS 进程——改变世界的动作必须经 AgentCore
的裁决与 SandboxRS 的执行。

## 当前状态

AgentRS 内核已具备可运行的 provider、上下文装配、工具管线、持久化恢复、
ContentRef、memory/skills、函数式子 Agent、MemberRun 派生、轨迹/脱敏/OTel 投影，
component generation/Inventory/profile 原子换代，以及
`run` / `resume-run` / `serve --jsonl` / conformance CLI。

仍属于外部交付的部分不能由本仓库单独宣称完成：真实 SandboxRS 的 L1 隔离与 PTY、
AgentCore 的团队编排/审批预算，以及真实 MCP 进程的生命周期和凭据管理。
本仓库为这些能力提供冻结 port、MCP 目录映射和 conformance suite；接入实现必须通过
H1/H2/H3/H4/H5/H7 验收。

外部实现的逐项接入门见 [External Integration Gates](docs/external-integration-gates.md)。

## 本地校验

```sh
cargo test --workspace          # 单测。不需要网络、文件系统、真实时钟或 OS 进程
cargo clippy --workspace --all-targets -- -D warnings
./scripts/check-no-env.sh       # 边界判据
./scripts/check-port-attribution.sh   # Apache-2.0 §4b 移植合规
```

## Dev TUI（非生产测试宿主）

`agentrs-tui` 用于在终端中端到端测试 AgentRS runtime、真实 OpenAI 兼容模型、
工具审批、ChangeSet 和 durable replay。它使用 `agentrs-dev-adapter` 的 L0 基础围栏，
不是 AgentUI，也不是生产 SandboxRS。

界面与键盘模型自 **dsh-code-agent**（MIT）逐模块移植，见
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

```sh
export AGENTRS_BASE_URL=https://api.siliconflow.cn/v1
export AGENTRS_API_KEY=...                 # 只通过进程环境注入
export AGENTRS_MODEL=Pro/deepseek-ai/DeepSeek-R1
cargo run -p agentrs-dev-tui -- --workspace . "Inspect this workspace"
```

命令行：`--workspace` / `--log` / `--resume` / `--permission <plan|default|accepted>` /
`--no-color`。非法取值在进入 raw mode **之前**报错。

### 读屏

无边框，一列流式转录：

- `> text` —— 你的消息，truecolor 下带底色，方便在成屏工具输出里回找；
- `● text` —— 助手，流式；助手正文按 markdown 渲染（标题/列表/引用/表格按显示宽度
  对齐/代码围栏），**语法被消费而不是展示**；
- `∴ Thinking` —— 模型的推理，默认折叠，`Ctrl+O` 展开。只有端点真的分离出
  reasoning 才会出现：OpenAI 兼容端点看 `delta.reasoning_content`，Anthropic 族看
  `thinking` 块。本机两个 vLLM 端点都不分离，在它们上面跑看不到这一行；
- `▸ / ✓ / ✗ / ⚠ name  [badge]` —— 工具卡：进行中 / 成功 / 失败或拒绝 / 被取消。
  状态符的颜色取自这次调用**做什么**（读/搜/改/跑/取），失败一律红；
  badge 携带 `42 lines`、`+12 -4`、拒绝码。卡体挂在 ` ⎿ ` 檐线下；
- `• text` —— durable 标记（压缩、审批审计、模式变更）；
- `── turn N` —— 一个 turn 的收尾。

**折叠预算按终端行数算，不按逻辑行数**——一行两万字符的 JSON 在 80 列下占 250 行，
按"一行"放行就会把整屏挤掉。成功卡 3 行、diff 卡 8 行、未成功或进行中的卡 8 行；
助手正文**从不因为长而折叠**。`Ctrl+O` 展开。连续三次以上的成功读/搜合并成
`✓ 8 reads · 2 searches` 一行——只有读和搜会合并，命令输出、diff、失败与进行中的
调用永不隐藏。

底部依次是：working line（转轮 + 在做什么 + 耗时 + 30 秒后的 token）、通知行
（按优先级排队、各自过期、`+2` 说明还有几条在等）、上下文压力条、composer、状态行
（左侧模型/权限/状态/ctx/tools/staged/tok，窄终端**整段丢弃而不折行**，
权限与模型永不丢；右侧工作区）。

### 键

`?` 打开快捷键表——**表本身就是解析器读的那张表**，重绑定之后两边同时变。

| 键 | 作用 |
|---|---|
| `Enter` | 发送；补全打开时先接受补全 |
| `Alt+Enter` / `Ctrl+J` | 换行 |
| `←→` `Ctrl+←→` `Alt+B/F` `Ctrl+A/E` | 光标：字符 / 词 / 行首尾 |
| `Ctrl+W` `Alt+Backspace` `Ctrl+U` `Ctrl+K` | 删词 / 删到行首 / 删到行尾 |
| `↑↓` | 多行草稿内移动，到头后走草稿历史；补全打开时选候选 |
| `/` | 草稿开头补全命令；直接回车即执行（见下表） |
| `@` | 任意位置补全工作区路径；接受目录后光标留在目录里继续打 |
| `Tab` | 接受补全 |
| `PgUp/PgDn`、`Alt+↑↓`、滚轮 | 滚动；离开尾部即暂停跟随，状态行报未读数 |
| `Ctrl+O` | 折叠/展开视野里的卡 |
| `Ctrl+T` | 全屏可搜索转录：`/` 搜索、`n`/`N` 跳匹配、`r` 恢复到草稿、`q` 关闭 |
| `Ctrl+P` | 命令面板 |
| `Ctrl+R` | durable 日志浏览器；`Enter` 重放并续跑（运行中的 run 不会被顶掉） |
| `Ctrl+X` | 用 `$EDITOR` 打开卡片的第一个文件位置 |
| `Shift+Tab` | 循环权限模式，**对下一次 run 生效** |
| `y` / `n` / `1`–`9` / `↑↓`+`Enter` | 审批：选项行，默认停在第一条拒绝行（fail-closed） |
| `Ctrl+S` | 提交 ChangeSet。丢弃与退出是 `/discard`、`/quit` |
| `Ctrl+H` / `Backspace` | 删除前一个字符。`stty erase ^H` 的终端发 0x08，crossterm 解成 `ctrl+h` |
| `Esc` | 先清草稿，无可清时中断当前 run |
| `Ctrl+C` | 两段式取消，再按一次强制退出 |

**composer 里没有裸字母键。** 曾经 `c`/`d`/`q` 直接是提交/丢弃/退出，于是打
`commit this` 会先提交再在草稿里留下 `ommit this`，打 `quick` 会直接退出程序——
一个始终在线的输入框付不起这个代价。审批面板可以用裸 `y`/`n`，因为那时没有草稿可打。

光标是画出来的（当前单元格反显），宽字符落在它自己的起始列。composer 用**硬折行**
而不是按词折行：只有硬折行满足"折前缀得到的行 = 折全文得到的前缀行"，光标才定得住。

### 命令

`Ctrl+P` 打开面板，或在草稿开头打 `/`（Tab 补全，回车执行）。

| 命令 | 键 | 作用 |
|---|---|---|
| `/diff` | `Ctrl+G` | 提交前审阅暂存的改动：逐文件 diff 与 `+N -M` |
| `/commit` | `Ctrl+S` | 提交本会话开过的**全部** ChangeSet，按打开顺序 |
| `/discard` | | 丢弃全部暂存 |
| `/clear` | | 丢掉当前对话开一段新的（有未提交改动时先拦住） |
| `/retry` | | 把上一条**消息**放回草稿（命令不进历史） |
| `/cancel` | `Ctrl+C` | 中断进行中的 run |
| `/permission <preset>` | `Shift+Tab` | 带参数指定 `plan`/`default`/`accepted`，不带参数则循环 |
| `/status` | | 本次会话是什么：run、日志、模型、端点、计数、暂存、降级状态 |
| `/logs` | `Ctrl+R` | durable 日志浏览器 |
| `/transcript` | `Ctrl+T` | 可搜索转录 |
| `/export <path>` | | 把转录写成 markdown，默认写到日志同名的 `.md` |
| `/fold` | `Ctrl+O` | 折叠/展开视野里的卡 |
| `/editor` | `Ctrl+X` | 用 `$EDITOR` 打开卡片的文件 |
| `/mouse` | | 把滚轮还给终端（拖选、终端自己的复制），或收回来 |
| `/help` | `?` | 快捷键表 |
| `/quit` | | 退出（等同 `Ctrl+C` 两下） |

命令与键是**同一张表的两种写法**——面板里每一行都标着对应的键，改了绑定两边一起变。
一条命令要么有动作，要么不存在（有测试钉着），不会出现"点了没反应"的行。

`Ctrl+C` 与 `Esc` 是**保留键**，不可重绑定也不可解绑。其余可写
`~/.agentrs/keybindings.json`（`$AGENTRS_HOME` 优先）重绑定：

```json
{ "palette:open": "ctrl+g", "session:browse": ["ctrl+r", "alt+r"], "help:open": null }
```

文件里的任何错误都只进通知行然后被忽略，**绝不会让 TUI 起不来**——因为要改它就得先
进得来。一行坏了不影响其余各行。

### 降级

色深与宽字形从 `TERM`/`COLORTERM`/`NO_COLOR`/`LANG` 探测；truecolor 用哑光十六进制，
其余用 ANSI 名（尊重用户配色），`NO_COLOR` / `TERM=dumb` / `--no-color` 下不发任何
SGR。不能画宽字形的终端整套换 ASCII 替身（`> + x ! *`），两套字形都是一格宽，
所以行预算不受影响。每一处降级都进一条通知，不是静默的。

### 工具

八个工具（配了搜索端点则九个）住在 `crates/agentrs-tools/src/builtin/`，一文件一个：
**声明、编排、措辞在一起**。schema 说 `offset` 是整数、执行侧却按字符串读，
只有真跑一次才会暴露——它们分家过，代价就是这个。两个宿主（TUI 与 CLI）用同一份，
授权清单与审批清单也由它派生。

**内核仍然无执行权。** 工具经三个注入的端口拿到 OS：`WorkspaceIo`（读写工作区）、
`Shell`（跑命令）、`Http`（取网页），实现全在 `agentrs-dev-adapter`。
判据是**判定留在工具侧，机制注入**：比如 SSRF 里"哪些地址不许去"是策略，
在 `builtin/net_guard.rs`；"这个域名解析成什么"是机制，走 `Http::resolve`。
反过来放的话，一个换了 adapter 的宿主就能悄悄换掉安全判定。cwd 围栏是唯一的例外，
它属宿主义务（L0 的一条），由 `WorkspaceIo` 的实现方负责。

这么分的直接收益：`cargo test -p agentrs-tools` 的 132 条工具语义测试跑在内存
fake 上，**一次磁盘也不碰**；`agentrs-dev-adapter` 的测试则只管它自己那部分——
真实文件系统、grant 台账、overlay 一致性。

| 工具 | 画像 | 要点 |
|---|---|---|
| `Glob` | 只读·可并发 | `**/*.md` 式通配。`*` 不跨 `/`，`**` 跨且可匹配零层 |
| `Grep` | 只读·可并发 | 字面子串，可限 `path` / `glob` / `ignore_case` / `max_results` |
| `Read` | 只读·可并发 | `offset`/`limit` 按行翻页；整份读回逐字节一致（含结尾换行） |
| `Write` | 会改·独占 | 区分新建与覆盖，覆盖时报出会盖掉多少字节 |
| `Edit` | 会改·独占 | **`old` 必须唯一**；多处命中会拒绝并说清怎么办，`replace_all` 显式全改 |
| `Delete` | 会改·独占 | 文件不存在报失败，不再无条件立墓碑 |
| `WebFetch` | 只读·可并发 | 取公网网页抽成文本。**只读却要审批**，见下 |
| `Bash` | 会改·独占 | L0 六条围栏内跑 shell；六条缺一即 `IsolationUnavailable` |
| `WebSearch` | 只读·可并发 | 仅当 `AGENTRS_SEARCH_URL`/`_KEY` 都配好时才注册 |

三条贯穿其中的判据：

**输出宁可截断也不撒谎。** `Read` 超限时保留真实的前几行并在末尾说明"还有 N 行未显示，
用 offset=M 继续读"；从前是把**整份内容**换成一句"输出过大：N 字节"，模型会把那句话
当成文件内容读下去。`Grep`/`Glob` 超上限时同样报出总数。

**错误消息要能指导改正。** 判据不是"返回了什么码"，而是模型读完能不能自己改对。
`Edit` 撞多处时说的是"命中 2 处，请补足上下文让它唯一，或显式传 replace_all"，
而不是一句"失败"。工具说的话会被原样带到模型面前——内核从前只写 `exit_code=1`，
把唯一有用的那半句扔了。

**命令输出先清洗再进上下文。** 工具输出是上下文里最容易失控的一段——它的长度由
外部程序决定，不由模型也不由我们决定。`agentrs-context::compact` 自 aionrs 的
`aion-compact` 移植（Apache-2.0，见 THIRD-PARTY-NOTICES），`Bash` 的输出走
**无损**那一级：去 ANSI、把被回车反复重画的进度行折成最后一帧。实测一次带进度条的
构建输出 1929 → 712 字节，内容一个字节没少。

`Full` 那一级会折叠相似行、重排 JSON——**会改变内容，所以绝不用在 `Read` 上**：
`Read` 承诺整份读回逐字节一致，基于折叠过的内容做 Edit 会把文件改成谁也没要求的
样子，而且全程没有任何一步报错。这条判据在 `compact/mod.rs` 里有一条专门的测试钉着。

**声明有 lint，参数有校验。** `agentrs-tools::lint` 审描述与 schema
（描述太短、缺 `additionalProperties:false`、字段没有 description、`required` 指向
不存在的字段……），目录旁边有一条测试强制它过；固定管线的第一道关口按 schema 校验参数，
不合法的调用**跨不出副作用边界**，模型收到的是"path: 必填字段缺失"这样的结构化错误。
校验器只实现子集，**不认识的关键字一律放行**——看不懂就拒会把第三方（含 MCP）注册的
工具整个挡在门外。

**要不要人点头，看的是"后果收不收得回来"，不是"改不改工作区"。** 这两件事看起来
是一回事，加进网络工具之后就不是了：`WebFetch` 一个字节也不碰工作区，却把一个由
模型选定的地址发了出去，请求发出去就收不回来。所以审批清单是
`builtin::approval_required` 显式列出的，不是从 `EffectProfile` 派生的——后者另有
职责（Plan 模式该不该放行），一物二用会在只读探索时顺带禁掉查资料。这与
`EffectProfile` / `concurrency_safe` "看起来相关、实际互不蕴含"是同构的第三例。

#### 命令与网络

内核仍然**没有**执行权：`clippy.toml` 从类型层面禁掉 `std::process::Command`，
`scripts/check-no-env.sh` 兜住它覆盖不到的写法，两道门禁只对
`agentrs-dev-adapter` 开口——那正是 `SandboxExecutor` 这个 port 存在的理由。
真实隔离（landlock/seccomp/seatbelt）归 SandboxRS，接入门在
`docs/external-integration-gates.md`。

`Bash` 兑现契约里 L0 的**全部六条**（`contracts/src/sandbox.rs`）：

| 条 | 手段 |
|---|---|
| 进程组 | `process_group(0)`，超时时 `kill -- -PGID` 杀掉整组，孙子进程跑不掉 |
| rlimit | `sh -c 'ulimit -t … -f …; exec …'`——shell 内建，不碰 libc |
| cwd 限制 | `current_dir(workspace)` |
| env 清洗 | `env_clear()` + 固定五个变量，值也写死不继承 |
| 默认断网 | `unshare -rn`（无特权用户命名空间） |
| 超时强杀 | tokio 超时 → TERM，宽限后 KILL |

六条**首次用到时真跑一次探测**，缺任何一条一律回
`RejectReason::IsolationUnavailable` 并说明缺哪几条——于是 macOS、Windows、
关掉了用户命名空间的机器上 `Bash` 直接不可用。这是 H2 要的行为：静默降级等于
对外宣称"已隔离"却没有。不用 `pre_exec` 是因为它是 `unsafe`，而 workspace
`unsafe_code = "forbid"`。

命令的副作用**直接落盘**，没有 ChangeSet 兜着。所以 `reconcile` 多一态：派生之前
先记 `Started`，恢复时只见 `Started` 未见结果 → `ExecutionStatus::Unknown`
（停下问人），**不得**报 `NotStarted`——那会把一条可能已经生效的命令重跑一遍。

`CommandShapeGuard` 拦几种一眼可辨的形状（`sudo`、`curl … | sh`、越界的
`rm -rf`、越界重定向、`&` 收尾），拿不准一律 `Abstain`。**它不是安全边界**——
安全边界是上面六条加人工审批；它的作用是省得人对审批面板麻木，点到第三十条时
一句看起来平平无奇的 `curl … | sh` 就会被随手放行。它是 `ToolGuard`，因此只能
`Deny` 或 `Abstain`，没有 `Allow`（内核不变量 15）。

`WebFetch` 的主要风险是 SSRF，不是附带条款：参数由模型填，而它读过的每一篇网页都
可能在教它去取 `169.254.169.254`（云元数据服务）或 `127.0.0.1:19121`（本机模型
端点）。因此 scheme 只许 http/https，解析出的 IP 逐个对照内网/本机/链路本地/
v4 映射的全表，**并把连接钉在已核验的那个地址上**（防 DNS rebinding），
**每一跳重定向都重判**——首跳公网、302 到 127.0.0.1 是最容易漏的一种。
与 `agentrs-provider` 相反，它**尊重**环境里的代理设置：provider 连的是本机端点，
它取的是外网。

判定分两层，因为第二层不是到处都有：

- **不依赖 DNS 的那层**——协议白名单、URL 里写死的字面 IP、以及一张内部主机名表
  （`localhost`、`*.internal`、`*.local`、`metadata.google.internal`……）。
  最后那个名字要紧：它和 `169.254.169.254` 是同一个东西，只拦 IP 会整个漏掉它。
- **依赖 DNS 的那层**——解析出的每一个地址都对照内网/本机/链路本地/v4 映射的全表，
  并把连接钉在核过的那个地址上。

**走 HTTP 代理时只剩第一层，这一点不该被含糊过去。** 解析是代理做的，客户端既钉不住
也看不见结果，机器上甚至可能根本没有直连 DNS。此时一个解析到内网的公网域名，我们在
客户端拦不住它——越界的目标会是**代理所在网络**而不是本机网络，但那仍是一次真实的
弱化。反过来，代理环境下"解析不出来"是常态而非异常，所以那种情形不再一律拒绝，
否则整个工具在这类机器上一条也取不到（这正是它最初的表现）。

### 边界

`crates/agentrs-dev-tui/src/host_io.rs` 是本 crate **唯一**触碰 OS 的文件——时钟、
环境变量、配置文件、目录列举、`$EDITOR`。`scripts/check-no-env.sh` 豁免的是这一个
文件而不是整个 crate，其余二十来个模块仍受门禁保护，且一律是注入值的纯函数
（`now_ms` 是参数，从不自己读表），这也是渲染层可以快照测试的原因。

`--resume <jsonl>` 离线重建 transcript。**重建时不显示任何时长与 diff**：
durable 日志里没有记过这些，凭空补出来就是 UI 冒充内核的事实。

### 多轮对话

内核的 Run 在 inbox 排空后就结束——**Run 不是会话，是一次"把话说完"**。所以运行中
发消息是 steering（进当前 Step），而 run 结束后再发一句是**新的一轮**：宿主用
[`fork`](crates/agentrs-runtime/src/fork.rs) 从上一个 Run 派生新 Run，
`RuntimeHost::start_forked` 把上一轮的 durable 前缀重放成 Surface 再启动。

三条由此而来的性质：

- **每一轮一个日志**：`chat.jsonl` → `chat-2.jsonl` → `chat-3.jsonl`。两个 Run 挤在
  一个文件里会共用序号计数器，`--resume` 会把一个 Run 的历史读成另一个的。
- **每个日志自足**：继承来的前缀会写进新 Run 自己的日志（fork 规则 3），所以
  `--resume chat-3.jsonl` 单独就能重建整段对话。它**不在 live 通道上重播**——
  那段对话宿主早就显示在屏幕上了，再推一遍就是贴第二份。
- **ChangeSet 按 Run 分，提交按会话收**：fork 规则 5 说新 Run 不继承任何 live 状态，
  暂存的写正是 live 状态，所以每一轮开一个新 ChangeSet；但按一次 `c` 必须把这个会话
  开过的**全部** ChangeSet 按顺序提交，否则用户会以为前几轮的改动一起落了盘。

思考进 durable Surface 并带上签名，因此 `--resume` 能重建它。**要不要发回给模型是
端点能力说了算**：Anthropic 族要求带签名原样往返，OpenAI 兼容端点不收，
`legalization` 在装配时按能力剥离。

**已知限制**：跨轮没有读己之写。第二轮 `Read` 一个第一轮暂存过、尚未提交的文件，
读到的是盘上的旧内容——overlay 按 ChangeSet 隔离，而新 Run 拿的是新 ChangeSet。
要打通得让宿主能指定 ChangeSet（`RunSpec` 目前没有这个字段），属于契约改动。

支持矩阵：

| 平台 | 构建/单测 | 交互 PTY | 说明 |
|---|---:|---:|---|
| Windows 11 / PowerShell | 已验证 | 已验证 | 当前真实 LLM 与终端恢复基线 |
| Linux | 已验证 | 已验证 | Ratatui/Crossterm 代码路径受支持 |
| macOS | CI 待接入 | 待验证 | Ratatui/Crossterm 代码路径受支持 |

**没有对应物、因此不做**：向用户提问（AgentRS 无此通道）、todo/plan 面板
（无 todo 投影，凭空造一个就是 UI 冒充内核）、子 Agent 行（本宿主不派生 ChildRun）、
会话内切权限（`with_permission_mode` 是引擎构建期的 builder，运行期切换需要内核补
`UserInput::PermissionMode` 与 turn 边界裁定）、会话内换模型（模型由 `RunSpec` 固定）。

最低 Rust 版本保持 workspace 的 1.85；TUI 固定使用 Ratatui 0.29 和 Crossterm 0.28。
API key 不进入 TUI state、durable JSONL 或屏幕诊断。

## 设计入口

- [架构方案](../agentrs-技术架构设计与实施方案.md) —— 完整契约与不变式
- [架构说明（评审版）](../agentrs-架构说明-评审版.md) —— 五张图 + 关键裁决
- [内核迭代计划](../uworker-内核迭代计划.md) —— 排期与出口标准

## 许可

Apache-2.0。部分模块自 aionrs（Apache-2.0）与 dsh-code-agent（MIT）移植，
见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。
