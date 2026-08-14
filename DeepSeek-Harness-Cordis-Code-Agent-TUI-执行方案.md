# 基于 DeepSeek Harness 与 Cordis 的 Code Agent TUI 执行方案

> 文档状态：可执行设计稿  
> 编制日期：2026-08-14  
> 源码基线：`deepseek-harness-master` `5d28ca9`（`@deepseek-ai/dsh-root` `0.1.0-rc.5`）；独立 Cordis `8cc9e33`  
> 目标：在保留 DeepSeek Harness 插件体系、Agent loop、会话日志、工具流水线和安全策略的前提下，构建一个键盘优先、面向真实代码仓库工作的交互式 TUI 产品。

## 0. 执行摘要

建议将产品实现为 DeepSeek Harness 的一个**同进程 TUI profile**，而不是重新实现 Agent loop，也不是在第一版中通过现有 SDK 启动一个外部 Harness 子进程。

最终运行形态：

```text
dsh --profile tui [--resume <session-id>] [--model <route>] [--permission <mode>]
  -> dsh-base
  -> dsh-tui bundle
  -> tui-startup（参数解析）
  -> tui-runtime（会话控制、事件投影、审批桥接）
  -> tui-app（Ink 渲染与输入）
```

核心选择如下：

| 决策 | 选择 | 原因 |
|---|---|---|
| 集成方式 | 同进程 Cordis 插件 | 可直接驱动 `ctx.agents`，订阅完整事件并响应审批；避免扩展当前缺少取消和审批的 SDK 协议 |
| TUI 技术 | TypeScript + React + Ink | 与仓库 Node/TS 技术栈一致，支持声明式组件、键盘输入、测试渲染和 npm 分发 |
| Agent 内核 | 原样复用 Harness | 不复制 loop、session、LLM、tools、compaction、subagent |
| 状态权威 | `SessionEvent` 日志 | transcript、恢复和 UI 统一从持久事件重建；实时 `agent/*` 只补充运行态 |
| 安全默认值 | `workspace-write + ask` | 与 `dsh-base` 一致；审批不可用时拒绝，不能降级为自动放行 |
| Cordis 用法 | 组合、作用域、依赖、effect 生命周期 | Cordis 不作为 OS 沙箱、持久工作流或 UI store |
| 首版范围 | 单工作区、单前台主会话、可观察子 Agent | 先保证代码修改闭环、恢复、安全与终端兼容性 |

第一版成功标准不是“在终端里复刻 Web UI”，而是打通以下代码工作闭环：

1. 在当前 Git 仓库启动，识别工作区和指令文件。
2. 创建或恢复会话，流式展示回答、推理摘要、工具调用和 diff。
3. 用户能随时补充消息、steer、取消当前执行，并清楚看到 Agent 状态。
4. Shell、文件写入和提权请求有明确审批；拒绝后 Agent 能继续工作。
5. 退出后会话可恢复，异常退出不留下 PTY、子进程、监听器或未刷盘事件。
6. Linux/macOS 达到发布质量，Windows 至少完成兼容性验证和明确降级提示。

## 1. 产品定义

### 1.1 目标用户

- 主要用户：长期在终端、Git、编辑器和远程开发环境中工作的工程师。
- 次要用户：希望在 SSH、容器、无桌面环境中使用代码 Agent 的团队。
- 不以第一次接触命令行的用户为 P0 目标；但错误信息、权限提示和帮助必须自解释。

### 1.2 核心任务

- 理解仓库：搜索代码、读取文件、解释架构、定位缺陷。
- 修改代码：先读后改、展示 diff、执行格式化、测试和静态检查。
- 调试问题：运行命令、持续观察输出、终止后台任务、读取日志。
- 代码评审：按严重度展示发现，提供文件定位，不自动修改。
- 计划与执行：在 plan 模式形成可审查方案，切换回执行模式后落地。
- 长任务：展示 todo、子 Agent、token、上下文压缩和当前阻塞点。

### 1.3 P0、P1、P2 范围

| 阶段 | 纳入 | 不纳入 |
|---|---|---|
| P0 | 新建/恢复会话、流式对话、工具卡片、Shell 输出、diff、审批、ask-user、steer、取消、模型/权限模式、会话持久化、命令面板、日志诊断 | 多工作区并行、鼠标优先 UI、远程协作、插件市场 |
| P1 | 会话选择器、全文搜索、文件提及、图片附件、PTY 面板、子 Agent 详情、主题、自定义键位、复制/导出、自动更新 | 团队共享会话、云同步 |
| P2 | 多会话并发、远程 runner、SSH workspace、插件管理 UI、团队策略、企业审计导出 | 在 TUI 内建设完整 IDE |

### 1.4 明确不做

- 不 fork 一套新的 Agent 状态机。
- 不让 TUI 直接执行 Shell 或直接写工作区；它只驱动 Harness 能力。
- 不把 Cordis `ctx.effect()` 当成撤销外部写入的事务机制。
- 不默认展示模型私有 chain-of-thought；展示可持久、可审计的文本、工具和状态事实。
- 不在 P0 实现内置代码编辑器；用户可从文件定位跳转到 `$EDITOR`。
- 不让 UI 内存状态成为恢复依据。

## 2. 对现有源码的判断

### 2.1 可直接复用的能力

| 产品能力 | 复用模块/机制 | TUI 的职责 |
|---|---|---|
| Agent 生命周期 | `@deepseek-ai/dsh-agent`、`dsh-agent-loop` | create/resume、followup/steer/inject/cancel、状态展示 |
| 持久会话 | `@deepseek-ai/dsh-session`、JSONL persistence | 事件投影、恢复入口、退出前 flush |
| 模型流 | `@deepseek-ai/dsh-llm` 与 provider adapters | 渲染 chunk、用量、错误和 retry 状态 |
| 工具调用 | `@deepseek-ai/dsh-tools` pipeline | 使用工具自己的 `presentCall`/`presentResult` 意图渲染 |
| 文件系统 | `dsh-fs`、`tool-fs`、observation policy | 展示读取范围、版本冲突、diff 和文件定位 |
| Shell | `dsh-shell`、sandbox provider、`tool-bash`/`tool-pwsh` | 终端卡片、退出码、截断/溢出提示、停止后台任务 |
| 持久 PTY | `dsh-terminal` | P1 独立终端面板；所有权仍归 Agent |
| 权限 | `dsh-user-approval`、permission presets、sandbox policy | 实现 `approval/request` 的人机回答器 |
| 用户提问 | `dsh-user-questions`、`tool-ask-user` | 单选/多选/文本输入 modal |
| 计划/Todo | `dsh-plan-mode`、`dsh-todo`、`dsh-goal` | 状态栏和侧栏投影，不自建第二套任务状态 |
| 上下文 | compaction、token meter | 展示容量、压缩发生和失败，不在 UI 拼模型消息 |
| 子 Agent | subagent seam/providers | 展示父子关系、活动状态和结果摘要 |
| 用户命令 | `dsh-commands` | `/` 命令发现、补全和执行 |
| 组合与配置 | Cordis Loader、bundle/profile/patch | 提供 `tui` bundle，并允许用户 patch |

### 2.2 Cordis 的正确使用边界

Cordis 在本产品中解决四类问题：

1. **组合**：TUI、approval answerer、主题、快捷键、工具 renderer 都是插件或注册项。
2. **作用域**：会话/Agent 局部行为挂到 `agent.ctx`，避免跨会话污染。
3. **依赖可用性**：TUI runner 声明注入 `agents`、`sessions`、`commands`、`approval` 等服务，缺失时启动失败而不是静默降级。
4. **实时生命周期**：监听器、raw-mode、resize handler、定时器和后台渲染任务都通过 `ctx.effect()` 注册 disposer，根 fiber 销毁时逆序清理。

Cordis 不解决以下问题：

- Session 的 durable replay；由 append-only `SessionEvent` 负责。
- Shell/文件安全；由 sandbox、policy、approval 和能力 provider 负责。
- 外部副作用回滚；命令已执行、文件已写入或网络请求已发出时，disposer 不能撤销事实。
- UI 状态管理；TUI 使用纯 reducer 和显式 controller，避免把任意可变状态塞进 `ctx`。

### 2.3 为什么 P0 不走 SDK 子进程

当前 JSON-RPC SDK 适合自动化，但不满足完整交互式 TUI：

- 没有 prompt cancel 或 session close。
- server 不会向 client 发起审批请求。
- `session/prompt` 只返回入队 receipt，结果归属需由 client 自己按 idle 区间判断。
- runtime 的所有 session event 默认无过滤广播，客户端再做作用域过滤。

因此 P0 用同进程插件。P2 若需要远程 runner，再为协议补充 `session/cancel`、`session/close`、`approval.request`、`question.request`、能力协商、协议版本和断线恢复游标，然后复用同一套 TUI reducer。

### 2.4 上游风险

- Harness 和独立 Cordis 都处于 pre-release/active development，API 无稳定承诺。
- 产品仓库应固定 commit，不跟随浮动版本；每次升级单独建立 compatibility PR。
- 不建议同时依赖 `opensource/deepseek-harness/cordis` 和 Harness 自带 `vendor/cordis`。运行时以 Harness vendored Cordis 为唯一版本，独立 Cordis 仅用于源码研究和上游同步。
- 所有对核心循环的改动都应视为最后手段；优先使用已有事件、Service Definition/Provider/Consumer seam 和 profile patch。

## 3. 用户体验设计

### 3.1 启动命令

```bash
# 在当前目录创建新会话
dsh --profile tui

# 带初始任务启动
dsh --profile tui "修复登录模块的竞态并运行相关测试"

# 恢复指定会话
dsh --profile tui --resume <session-id>

# 只读评审
dsh --profile tui --permission read-only "review current changes"

# 临时覆盖配置
dsh --profile tui --patch ./team-policy.patch.yml
```

参数归属必须保持现有 launcher 规则：`--profile`、`--patch` 属于 `dsh`；第一个未识别 token 之后的 `--resume`、`--model`、`--permission`、任务文本属于 `tui-startup`。

### 3.2 主界面

```text
┌ repo: uworker  branch: feature/tui  model: deepseek-v4  mode: workspace-write ┐
│ Session: 修复并发写入问题                         ctx 42%   $0.18   running │
├──────────────────────────────────────────────────────────────────────────────┤
│ User  修复 session flush 期间可能丢事件的问题，并运行相关测试                │
│                                                                              │
│ Agent 我先检查 flush 和事件提交的所有权关系。                                │
│                                                                              │
│ ▾ Read packages/core/session/src/index.ts                        8.4 KB       │
│ ✓ Search "session/flush"                                        17 matches   │
│ ▾ Edit packages/core/session/src/index.ts                        +12 -4       │
│   @@ ...                                                                     │
│   + await pendingWrites...                                                    │
│ ▾ Bash pnpm vitest ...                                           running 12s │
│   ✓ session flush ...                                                       │
│                                                                              │
├──────────────────────────────────────────────────────────────────────────────┤
│ Plan 2/4   Tools 4   Files +12 -4   subagents 1   approvals 0                │
├──────────────────────────────────────────────────────────────────────────────┤
│ > 补充覆盖 dispose 与 flush 并发的测试_                                      │
└ Enter send · Alt+Enter newline · Esc steer/stop · Ctrl+P commands · ? help ─┘
```

布局策略：

- 高度小于 18 行：隐藏次要状态，只保留 transcript、审批和输入框。
- 宽度小于 80 列：顶栏分两行，工具卡片取消左右布局，diff 不并排。
- 宽度大于等于 120 列：P1 可打开右侧 activity pane，展示 todo、子 Agent、文件变更。
- 非 TTY：不启动交互 UI；有任务时回退 headless 风格，有错误时返回非零退出码。

### 3.3 信息层级

1. **必须立即可见**：当前 Agent 状态、待审批动作、错误、当前输入、工作区和权限模式。
2. **默认展开**：Assistant 正文、短工具结果、diff 摘要、失败命令末尾输出。
3. **默认折叠**：成功的长命令、完整文件内容、重复 tool chunk、子 Agent 中间过程。
4. **诊断视图**：event seq、provider/model、token、retry、compaction、sandbox enforcement、插件错误。

### 3.4 键位

| 键位 | 行为 |
|---|---|
| `Enter` | 发送；有审批时确认当前选项 |
| `Alt+Enter` | 输入换行 |
| `Esc` | 第一次进入 steering/停止提示；再次确认取消当前运行；弹窗中关闭弹窗 |
| `Ctrl+C` | 有选区时复制；运行中第一次请求取消；空闲时退出确认；连续两次强制进入有界关闭流程 |
| `Ctrl+P` | 命令面板 |
| `Ctrl+O` | 展开/折叠当前工具卡片 |
| `Ctrl+D` | 打开当前 diff/文件变更面板 |
| `Ctrl+R` | 会话选择/恢复 |
| `Ctrl+L` | 重绘屏幕，不清空会话 |
| `Tab` / `Shift+Tab` | 焦点在 transcript、activity、input、modal 间移动 |
| `j/k`、`PgUp/PgDn` | 浏览历史；输入框聚焦时保留普通字符输入语义 |
| `?` | 帮助 overlay |

键位应由配置注册表提供默认值并检测冲突；P0 可只允许配置文件覆盖，P1 再做交互式编辑。

### 3.5 核心工作流

#### 新任务

```text
解析参数 -> 验证 TTY/工作区 -> 等待 Loader settle
-> 读取默认 model -> ctx.agents.create(setup)
-> 绑定 scoped listeners -> 输入 task 或等待用户
-> agent.followup(userMessage) -> 投影 session/event -> 渲染
```

#### 中途引导与取消

- Agent running 时发送普通补充，默认调用 `steer()`；空闲时调用 `followup()`。
- UI 必须明确标记“已排队”“已被下一步领取”，分别对应 inbox 实时事件和持久消息事件。
- 取消调用 Harness `agent.cancel()`，等待状态到 idle/terminal；超时只升级关闭 UI/根 context，不直接遗留子进程。

#### 审批

```text
tool pipeline -> approval.request(req)
-> TUI scoped answerer 入队 modal
-> 用户选择 Allow once / Reject
-> Promise resolve
-> approval/decided 持久化
-> tool pipeline 继续或返回拒绝结果
```

- 不提供含糊的“始终允许全部”快捷键。
- 若未来加入规则记忆，必须写入显式 policy 配置并展示资源范围，而不是把一次审批变成全局权限。
- TUI unmount、Agent 取消、信号退出或 request signal abort 时，所有未决审批一律 resolve 为拒绝/不可用。

#### 恢复

- 列表从持久 session metadata/query 获取，不扫描并自行猜测 JSONL。
- `ctx.agents.resume()` 后先从完整日志构建 transcript，再订阅增量事件，使用 seq 去重。
- 如果最后一个 turn 未正常结束，显示“上次运行中断”，由 Harness 的恢复语义决定能否续跑；UI 不伪造 `turn/end`。

## 4. 目标技术架构

### 4.1 组件图

```text
┌──────────────────────────── TUI process ─────────────────────────────┐
│ apps/cli: profile boot, signals, bounded root disposal               │
│                                                                      │
│ Cordis root                                                         │
│ ├─ dsh-base                                                         │
│ │  ├─ sessions / persistence / query                                │
│ │  ├─ agents / agent-loop / llm / compaction / subagent             │
│ │  ├─ tools / fs / shell / terminal / jobs                          │
│ │  └─ sandbox / approval / permission / commands                    │
│ └─ dsh-tui bundle                                                   │
│    ├─ tui-startup: argv -> immutable startup config                 │
│    ├─ tui-runtime                                                   │
│    │  ├─ AgentController                                            │
│    │  ├─ SessionProjection (pure reducer)                           │
│    │  ├─ ApprovalBridge / QuestionBridge                            │
│    │  ├─ CommandController / EditorLauncher                         │
│    │  └─ TuiStore (ephemeral view state)                            │
│    └─ tui-app (Ink)                                                 │
│       ├─ AppFrame / Transcript / Composer                           │
│       ├─ ToolCard renderers / DiffView / TerminalView               │
│       └─ ApprovalModal / QuestionModal / CommandPalette             │
└─────────────────────────────────────────────────────────────────────┘
```

### 4.2 建议目录

遵循 Harness 的 package 分组，不把所有代码塞进 bundle：

```text
packages/
  tui/
    runtime/                 # 不依赖 React/Ink；controller、projection、store contracts
      src/
        agent-controller.ts
        session-reducer.ts
        approval-bridge.ts
        question-bridge.ts
        command-controller.ts
        types.ts
    app/                     # Ink 组件、键盘路由、终端能力探测
      src/
        index.tsx
        app.tsx
        components/
        renderers/
        input/
        theme/
    startup/                 # TUI 自有 argv 解析与 startup service
      src/index.ts
  bundle/
    tui/
      cordis.patch.yml
      src/index.ts
      package.json
packages/boot/app-boot/src/
  profile.ts                 # 在 PROFILE_TEMPLATES 中登记内置 tui profile
apps/cli/
  package.json               # 声明 dsh-tui bundle 为安装内依赖
```

如果仓库的 workspace 约束暂不允许新增 `packages/tui/*` group，则先在 `packages/bundle/tui` 内建立 `runtime/` 与 `ui/` 内部目录，验证后再拆包；不要为了目录整齐提前建立不稳定的公共 API。

### 4.3 包职责

| 包 | 允许依赖 | 禁止职责 |
|---|---|---|
| `dsh-tui-runtime` | Agent/Session/Tools/Commands/Approval 的公共类型，Cordis | ANSI 绘制、React 组件、直接 fs/spawn |
| `dsh-tui-app` | runtime、Ink、终端渲染库 | 直接调用 provider、修改 session log、执行 Shell |
| `dsh-tui-startup` | cmdline、schema/参数解析 | 创建 Agent、渲染 UI |
| `dsh-tui` bundle | 上述插件和 patch | 承载复杂业务逻辑 |

### 4.4 状态分层

| 状态 | 来源 | 是否持久 | 示例 |
|---|---|---|---|
| Durable domain state | `SessionEvent[]` | 是 | user/assistant、tool call/result、turn、approval audit、todo、plan |
| Live Agent state | `agent/*` | 否 | running/idle、inbox、当前 request、实时 ownership |
| View state | TUI store | 否 | 焦点、滚动位置、折叠项、modal、命令面板查询 |
| User settings | `ctx.settings`/profile patch | 是 | 主题、键位、默认权限、模型 |

严禁用 live event 推导本应持久的 transcript；严禁把滚动位置写进 session log。

### 4.5 事件投影模型

定义纯函数 reducer：

```ts
type ProjectionAction =
  | { type: 'replay'; events: readonly SessionEvent[] }
  | { type: 'session-event'; event: SessionEvent }
  | { type: 'agent-status'; status: AgentStatus }
  | { type: 'inbox-state'; items: readonly InboxItem[] }
  | { type: 'subagent-state'; value: SubagentView }
  | { type: 'ui'; action: ViewAction }

function reduceTuiState(state: TuiState, action: ProjectionAction): TuiState
```

关键不变量：

- session event 按 `seq` 单调应用；相同 seq 幂等，跳号触发重放/诊断而不是继续猜测。
- `assistant/chunk` 提供实时保真，`assistant/message` 完成最终归并；重放结果与直播结束结果相同。
- `tool/call` 与 `tool/result` 按 call id 配对；Code Mode 子调用按 `tool/code-dispatch-start`/`tool/code-dispatch` 配对。
- 未完成 tool call 在恢复后显示 interrupted/pending，不能伪装为成功。
- `turn/end` 是一个轮次最终状态；Agent idle 只是实时状态，二者不能互相替代。
- 所有未知且标记 ignorable 的事件保留为 generic diagnostic row；未知 required event 必须拒绝读取并提示版本不兼容。

### 4.6 Tool renderer 注册表

Harness 已要求每个工具声明 UI render intent（`generic`、`terminal`、`diff` 和 `locations`）。TUI 应消费这套语义，不按工具名称堆 `switch`：

```ts
interface TuiToolRenderer {
  kind: 'generic' | 'terminal' | 'diff'
  renderCall(view: ToolCallPresentation): TuiNode
  renderResult(view: ToolResultPresentation): TuiNode
}
```

- 通用 renderer 是必备 fallback。
- terminal renderer 处理 stdout/stderr、exit code、signal、timeout、lossy/spill file。
- diff renderer 支持 unified diff、文件列表、行号和 `$EDITOR +line file`。
- renderer 必须纯化，不读取文件来“补齐”结果，避免 UI 看到与 session log 不同的事实。

### 4.7 生命周期与关闭

所有以下资源必须注册到 TUI plugin 自己的 fiber：

- `stdin.setRawMode(true)` 的恢复 disposer。
- `SIGWINCH`/resize listener。
- `session/event`、`agent/status`、approval/question listener。
- Ink render instance 的 `unmount()`。
- 未决 modal promise 的拒绝/不可用处理。
- clipboard/editor 子进程句柄。
- debounce、spinner、elapsed timer。

关闭顺序：

```text
停止接收新输入
-> settle 未决审批/提问为 unavailable
-> 请求取消活跃 Agent
-> 卸载 Ink 并恢复终端 raw mode/cursor
-> dispose TUI scoped effects
-> sessions.flush()
-> root fiber.dispose()（回收 tools/jobs/PTY/subprocess）
-> 恢复终端屏幕并按原因设置退出码
```

要复用 launcher 的 SIGINT/SIGTERM 有界关闭语义，不能在组件中直接 `process.exit()`。

## 5. 安全与权限设计

### 5.1 信任边界

```text
不可信：模型输出、工具参数、仓库内容、终端转义序列、插件文本
受控：TUI reducer、tool presentation、policy/approval、sandbox provider
可信宿主：profile boot、凭据服务、session persistence、根生命周期
```

### 5.2 必须实现的防护

- 所有模型/工具文本在写终端前清理控制字符和危险 ANSI 序列；仅允许 TUI 自己产生样式码。
- OSC 8 链接需配置开关，默认仅为本地规范化路径生成；不渲染模型提供的任意 URL escape。
- 粘贴启用 bracketed paste，粘贴内容永不因包含换行而自动发送。
- approval modal 展示工具名、原因、工作目录、sandbox mode 和关键参数；超长参数按头尾截断并允许展开。
- 默认 `workspace-write + ask`；sandbox unavailable 时 fail closed，不自动切换 `danger-full-access`。
- 凭据来自 Harness credentials service；不得进入 TUI state dump、日志、session event 或错误上报。
- 打开 `$EDITOR` 前对路径做 workspace/location 校验；参数用 argv 数组传递，不拼 Shell 字符串。
- terminal renderer 对大输出采用有界 ring buffer；完整输出只引用 Harness 已产生的 spill file，不在内存无限累积。
- 插件热重载期间若存在待审批请求，旧 listener dispose 时必须结束该请求，防止悬挂。

### 5.3 权限交互

P0 只暴露三个现有 preset：

| 模式 | 文件能力 | 审批 | UI 色彩语义 |
|---|---|---|---|
| `read-only` | 只读 | 需要 | 中性 |
| `workspace-write` | 工作区可写 | 需要 | 默认强调 |
| `danger-full-access` | 不限制文件副作用 | 默认不询问 | 持续高风险标识 |

切换权限必须通过 permission service 写入该 session 的持久 knob events，不能只改顶栏文字。危险模式进入前做一次明确确认；恢复会话时从事件 fold 恢复实际模式。

## 6. 配置与插件模型

### 6.1 `dsh-tui` patch 草案

```yaml
- id: system-prompt
  config:
    persona: >-
      You are a coding agent powered by the {{model}} model.
      Your working directory is {{cwd}}.

- id: hmr
  disabled: true

- id: tools
  config:
    mode: !!js process.env.DSH_TOOLS_MODE

- insert:
    - id: code-runtime
      name: '@deepseek-ai/dsh-code-runtime-worker-thread'

    - id: tui-startup
      name: '@deepseek-ai/dsh-tui-startup'

    - id: tui-runtime
      name: '@deepseek-ai/dsh-tui-runtime'
      inject: [agents, sessions, commands, approval, userQuestions, agentDefaultModel]

    - id: tui-app
      name: '@deepseek-ai/dsh-tui-app'
      inject: [tuiRuntime, tuiStartup]
```

实际 service key 必须以源码导出的名称为准，实施时通过 typecheck 和 config verifier 校正；不要用可选注入掩盖必须能力。

### 6.2 TUI 配置

```ts
interface TuiConfig {
  alternateScreen: boolean
  mouse: boolean
  color: 'auto' | '16' | '256' | 'truecolor' | 'none'
  theme: string
  maxRenderedEvents: number
  maxInlineOutputBytes: number
  cancelGraceMs: number
  editor?: readonly string[]
  keymap: Record<ActionId, readonly KeyChord[]>
}
```

所有可部署调节项进入 schema 并在加载时验证；协议常量、安全上限的硬最小值和事件不变量保持固定。配置错误应在进入 raw mode 前失败。

## 7. 分阶段实施计划

以下按 3 名工程师（2 名核心/后端、1 名前端/TUI）估算；单人实施约需 12 至 16 周，3 人并行约 8 至 10 周完成可发布 P0。

### 阶段 0：技术验证与基线冻结（第 1 周）

交付物：可运行的 spike、ADR、事件样本、依赖决策。

- 固定 Harness/vendored Cordis commit，记录 Node/pnpm 版本。
- 建立 `tui` profile 最小 bundle，验证从 `dsh --profile tui` 启动。
- 用一个临时 runner 直接 `ctx.agents.create()`，发送消息并订阅 `session/event`。
- 验证 Ink 与当前 ESM、Node 22/24、tsdown、TypeScript 6 构建兼容性。
- 验证 raw mode、resize、Unicode 宽度、中文输入法、SSH、tmux、非 TTY。
- 验证 `approval/request` 可由 agent-scoped TUI listener回答，取消时无悬挂 Promise。
- 采集真实 run 的 event fixture：纯文本、并行工具、Code Mode、失败 Shell、diff、compaction、子 Agent、审批拒绝。

退出条件：最小 TUI 能完成“发送任务 -> 允许一次 Bash -> 流式显示 -> 正常退出”，并证明 root dispose 后无子进程。

### 阶段 1：无 UI 的 TUI runtime（第 2 至 3 周）

交付物：`dsh-tui-runtime`、纯 reducer 测试、controller 测试。

- 定义 `TuiState`、`TranscriptNode`、`ToolNode`、`ModalState`、`AgentActivity`。
- 实现 replay + live event reducer，保证重放和直播同构。
- 实现 AgentController：create、resume、followup、steer、cancel、whenIdle、dispose。
- 实现事件 gap/duplicate 检测和有界重同步。
- 实现 ApprovalBridge 与 QuestionBridge，所有 promise 受 AbortSignal 和 fiber dispose 控制。
- 实现 Commands adapter，区分 `/command` 与发给模型的普通文本。
- 实现工具 presentation adapter，保留 generic fallback。
- 建立 fixture-driven snapshot，不依赖真实 API key。

退出条件：对同一 fixture，逐事件直播与一次性 replay 得到深度相等的领域投影；取消、dispose 和异常 observer 均不泄漏。

### 阶段 2：主界面与输入系统（第 3 至 5 周）

交付物：可日常使用的单会话 TUI。

- 实现 AppFrame、header、transcript viewport、composer、status line。
- 输入支持多行、历史、bracketed paste、IME，不误触发送。
- 实现 focus/key routing，modal 优先消费按键，避免全局快捷键穿透。
- 实现增量渲染、自动跟随底部、用户向上滚动后暂停跟随、未读计数。
- 实现文本、错误、turn/step 分隔、streaming cursor。
- 实现命令面板与 `/help`、`/new`、`/resume`、`/model`、`/permission`、`/compact`、`/quit` 的发现入口；真实命令仍由 `ctx.commands` 执行。
- 实现窄屏/低高度降级和无色模式。

退出条件：在 80x24 和 160x50 下完成 30 分钟会话，无明显闪烁、重排错误或输入丢失；10000 个事件的恢复保持可交互。

### 阶段 3：代码工具体验（第 5 至 6 周）

交付物：代码任务闭环。

- generic、terminal、diff 三类 renderer。
- Shell 卡片显示 command、cwd、sandbox、elapsed、exit code、尾部输出、截断状态。
- diff 支持 unified view、按文件折叠、增删统计、二进制提示和路径定位。
- 文件 read/search 展示范围、匹配数和 location；支持调用 `$EDITOR`。
- Todo/plan/goal 在状态栏和 activity panel 投影。
- Code Mode 的嵌套子调用在父工具下展示，按子 call id 配对。
- 子 Agent 只显示结构化状态、工具摘要与最终结果，默认不灌入主 transcript。

退出条件：用户能在不离开 TUI 的情况下理解 Agent 读了什么、改了什么、命令是否成功、哪个文件可打开。

### 阶段 4：权限、恢复与故障处理（第 6 至 7 周）

交付物：安全可恢复 beta。

- 审批 modal、ask-user modal、AbortSignal、队列、公平性和重复请求保护。
- permission preset 切换与危险模式确认。
- session selector、resume、中断状态标记、flush 失败提示。
- Provider 错误、retry、context overflow、compaction、sandbox unavailable 的专用错误行。
- SIGINT/SIGTERM、EOF、terminal detach、renderer crash 的有界关闭。
- 崩溃后恢复终端：cursor、raw mode、alternate screen、bracketed paste。
- 提供 `--diagnostic-log <path>`，日志默认脱敏且不写 stdout。

退出条件：故障注入矩阵全部通过；任何审批 transport 失败都不能执行受限工具；强制中断后终端可立即正常使用。

### 阶段 5：性能、兼容与发布（第 8 至 10 周）

交付物：`0.1.0` 可安装预览版。

- 对 1k/10k/100k events、10 MB tool output、100 个并行 tool calls 做压力测试。
- transcript virtualization：只构建 viewport 邻域，历史节点保留轻量索引。
- 合并 chunk 的刷新频率控制在 30 至 60 FPS；高频事件不逐个触发全树 render。
- Linux/macOS 主支持；Windows PowerShell、ConPTY、路径和 sandbox 降级测试。
- 测试 xterm、iTerm2、Windows Terminal、tmux、screen、VS Code terminal、SSH。
- 产出 npm package、profile template、changelog、迁移说明和 shell completion。
- 增加关键 keyless transcript snapshot；真实 provider e2e 仅作为补充。

退出条件：发布门禁全部通过，安装后首次启动不要求源码仓库，不污染 stdout，升级/卸载不删除用户 sessions/settings。

## 8. 任务分解与优先级

### P0 必须完成

| Epic | 关键任务 | 估算 |
|---|---|---:|
| Boot/Profile | TUI template、argv、bundle、配置校验、非 TTY 行为 | 5 人日 |
| Runtime | Agent controller、session replay/live reducer、event gap | 10 人日 |
| Rendering | frame、viewport、composer、streaming、responsive | 12 人日 |
| Tools | generic/terminal/diff、Code Mode 子调用 | 10 人日 |
| Interaction | commands、approval、questions、steer/cancel | 10 人日 |
| Recovery | resume、flush、signal、root disposal、terminal restore | 8 人日 |
| Quality | unit/snapshot/e2e/perf/platform matrix | 12 人日 |
| Release | package/profile、docs、diagnostics、install smoke | 6 人日 |

合计约 73 人日，未计上游缺陷修复和设计评审缓冲；计划应预留 20% 风险缓冲。

### P1 后续

- PTY 专用面板和后台 job 管理。
- session 内容搜索与跨会话导航。
- 图片附件的终端协议探测（Kitty/iTerm2/Sixel）及文本 fallback。
- 主题和键位插件化。
- OSC 52 复制与系统 clipboard provider。
- 文件提及补全、Git 状态面板、commit 辅助但不自动 push。
- 会话 export、诊断 bundle、匿名性能指标（继续默认关闭遥测）。

## 9. 测试策略

### 9.1 测试金字塔

| 层级 | 内容 | 工具/方式 |
|---|---|---|
| 纯单元 | reducer、配对、折叠、宽度、截断、按键状态机 | Vitest + fast-check |
| 组件 | 给定 state 的终端输出、modal、窄屏 | ink-testing-library/自建虚拟 stdout |
| 组合 | 真实 Cordis Context + fake provider/tools/session | Vitest |
| Snapshot | 真实可运行 TUI profile 的事件/屏幕规范化快照 | keyless fixtures |
| PTY E2E | raw mode、resize、Ctrl+C、粘贴、退出恢复 | node-pty 驱动 |
| Provider E2E | 真实模型完成 coding task | 有 key 时运行、自跳过 |
| 平台 | Linux/macOS/Windows 构建、smoke、sandbox | CI matrix |

### 9.2 必测场景

- 空文本、仅空白、超长输入、中文、emoji、组合字符、宽字符和 RTL 文本不破坏布局。
- 模型 chunk 将一个 Unicode 字符拆在字节边界时仍正确显示。
- 工具并行完成顺序不同于模型调用顺序。
- approval 到达时用户正在编辑多行文本，草稿不丢失。
- approval listener 抛错、reject、超时、dispose、AbortSignal 已触发，均 fail closed。
- cancel 与 tool result、turn end、session flush 同时发生。
- renderer 抛错只降级为 generic error row，不杀 Agent 或损坏日志。
- session replay 期间收到 live event，按 seq 合并且无重复。
- 终端 resize 为 0x0、窗口恢复、SSH 断开、stdin EOF。
- 大输出、恶意 ANSI、OSC、控制字符和极长无空格字符串。
- sandbox backend 不可用、权限拒绝、文件 stale version、命令 timeout/nonzero/signal。
- Cordis plugin reload/dispose 时监听器撤销、raw mode 恢复、未决资源清零。

### 9.3 性能预算

| 指标 | P0 目标 |
|---|---:|
| 按键到本地回显 p95 | < 50 ms |
| session event 到可见 p95 | < 100 ms |
| 10k events 恢复到首屏 | < 1.5 s |
| 10k events 稳态 RSS（不含模型 runtime） | < 200 MB |
| streaming 重绘 | <= 60 FPS，低速终端可降到 20 FPS |
| Ctrl+C 到 UI 退出（正常资源） | < 3 s |
| 工具输出内联保留 | 默认 <= 256 KiB/卡片，可配置且有硬上限 |

## 10. 发布与运维

### 10.1 分发

- 将 `tui` 作为随 `@deepseek-ai/dsh` 交付的内置 profile，与 `web`、`headless` 同级。
- 在 `PROFILE_TEMPLATES` 注册 `tui: ['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-tui']`，并让 `apps/cli/package.json` 依赖该 bundle；首次运行由现有 `initProfile()` 生成 `$DSH_HOME/profiles/tui`，不新增虚构的静态 profile 目录。
- npm 包只依赖一份 vendored/rescoped Cordis，避免多实例导致 service/context 身份错误。
- 首次启动自动初始化 `$DSH_HOME/profiles/tui`，用户 patch 与 package 升级分离。
- `dsh --profile tui --dump-config` 必须能看到完整组合来源。
- 发布物做 clean-install smoke，不依赖 monorepo tsconfig paths 或未发布源码。

### 10.2 可观测性

- stdout/stderr 在交互模式由 TUI 管理；普通日志不得直接打断画面。
- 建立有界内存 log ring，诊断面板可查看；显式参数才写诊断文件。
- 指标：首屏时间、event reducer 延迟、render duration、丢帧、会话恢复时间、取消耗时、未决审批数。
- 不记录 prompt、代码、命令输出和凭据到遥测；沿用 Harness 默认关闭遥测立场。
- 崩溃报告先本地生成脱敏摘要，由用户主动选择是否上传。

### 10.3 版本策略

- TUI `0.x` 与固定 Harness commit 绑定，发布说明列出兼容矩阵。
- Session format 由 Harness 拥有；TUI 不私自扩展同名事件。
- TUI 自有设置带独立 schema version，采用显式迁移，不静默丢弃未知键。
- 升级 Harness 时先跑 event fixture compatibility、profile config verifier、typecheck 和 PTY E2E，再处理 UI 变化。

## 11. 风险清单与缓解

| 风险 | 概率/影响 | 缓解 |
|---|---|---|
| Harness/Cordis API 快速变化 | 高/高 | 固定 commit、适配层集中在 runtime、升级 PR、fixture contract tests |
| Ink 对极长 transcript 性能不足 | 中/高 | reducer 与 renderer 分离、viewport virtualization；阶段 0 设淘汰门槛 |
| 终端兼容差异 | 高/中 | 能力探测、无色/无 alternate-screen fallback、PTY 平台矩阵 |
| 审批 UI 与工具执行竞态 | 中/高 | request id、agent scope、AbortSignal、fail closed、durable audit 配对测试 |
| 大输出导致内存/重绘问题 | 高/高 | bounded buffer、chunk coalescing、折叠、spill-file reference |
| UI 对 session event 解释漂移 | 中/高 | 使用公共类型/presentation、未知事件策略、replay/live 等价测试 |
| 同进程 UI 崩溃影响 Agent | 中/高 | error boundary、renderer fallback、根有界关闭、flush-before-exit |
| sandbox 只覆盖文件副作用 | 中/高 | UI 明示 enforcement；网络/凭据策略另建能力，不能宣称完全隔离 |
| Windows 支持成本 | 高/中 | P0 明确支持级别，优先 pwsh/ConPTY smoke，平台能力 fail loud |
| 上游贡献与产品定制冲突 | 中/中 | 通用 runtime/TUI seam 上游化，品牌/默认 profile 保持独立 patch |

### 11.1 Ink 淘汰门槛

阶段 0 若出现以下任一问题，应在正式组件开发前改用更底层 renderer，而不是中途重写：

- 10k event 虚拟列表无法保持输入 p95 < 50 ms。
- raw mode/IME/bracketed paste 无法可靠控制。
- 并行流式更新导致持续明显闪烁。
- Windows ConPTY 无法完成基本输入、resize 和退出恢复。

即使替换 Ink，`dsh-tui-runtime`、事件 reducer、controller 和 renderer intent adapter 保持不变；只替换 `dsh-tui-app`。

## 12. 验收标准

### 功能验收

- 全新安装后，在 Git 仓库运行 `dsh --profile tui` 可创建会话。
- 能流式显示文本以及 generic、terminal、diff 工具卡片。
- 能发送 followup、running 时 steer、取消、回答审批和 Agent 提问。
- 能切换 read-only/workspace-write/danger-full-access，实际策略与显示一致。
- 能恢复已持久会话，直播结束状态与 replay 状态一致。
- 能打开 tool location 到 `$EDITOR`，不经 Shell 字符串拼接。
- 子 Agent、plan、todo、compaction、provider error 有可辨识状态。

### 安全验收

- 无 answerer、answerer 故障、TUI dispose 和请求 abort 时审批均不放行。
- 恶意终端 escape 不会改标题、写 clipboard、伪造链接或污染后续 shell。
- 默认模式下不能越过工作区文件边界；sandbox 不可用时拒绝执行。
- TUI 日志和 crash dump 不含 API key、credential value 或完整环境变量。
- 所有进程、PTY、listener、timer 在正常退出和测试故障路径中都归零。

### 质量验收

- 新增源码满足仓库 strict TypeScript、lint、build、package invariants 和文档门禁。
- 核心 runtime 分支有单元和 property test；用户可见行为有 keyless snapshot。
- Linux/macOS PTY E2E 全绿；Windows 达到声明的支持级别。
- 性能预算达到第 9.3 节目标。
- `git diff --check`、clean install、packed artifact smoke 通过。

## 13. 第一批 PR 切分

为了让每个变更可审查、可回退，建议按以下顺序提交：

1. **PR-01 ADR + spike**：记录同进程选择、Ink 基准、事件 fixture，不进入发布 profile。
2. **PR-02 TUI startup/profile skeleton**：参数、模板、bundle、help、非 TTY、构建与安装 smoke。
3. **PR-03 Runtime projection**：纯 event reducer、replay/live 等价、工具配对、fixtures。
4. **PR-04 Agent controller**：create/resume/followup/steer/cancel/dispose 和 scoped listeners。
5. **PR-05 Base UI**：frame、transcript、composer、responsive、key routing。
6. **PR-06 Tool renderers**：generic/terminal/diff、location/editor、Code Mode 子调用。
7. **PR-07 Human interaction**：approval、questions、permission presets、fail-closed tests。
8. **PR-08 Recovery and shutdown**：resume、flush、signals、renderer failure、terminal restoration。
9. **PR-09 Performance**：viewport、chunk coalescing、large-output bounds、bench gates。
10. **PR-10 Release**：内置 profile、文档、shell completion、packed-install 和平台矩阵。

每个非平凡 PR 都应按 Harness 规则添加 Agent Note；涉及产品可见行为的 PR 同时更新 runnable keyless snapshot，不能只交 mock unit test。

## 14. 开工前检查表

- [ ] 产品名、npm scope 和是否向上游 DeepSeek Harness 提 PR 已确定。
- [ ] 固定源码 commit 和许可证/第三方 notice 流程已确认。
- [ ] `packages/tui/*` 是否符合 workspace group 约束已验证。
- [ ] Ink spike 达到性能和平台淘汰门槛。
- [ ] `approval/request` 与 user-question 的准确 service key/API 已由源码 typecheck 固化。
- [ ] create/resume/cancel/flush 的 owner 与关闭顺序已有组合测试。
- [ ] P0 终端和 OS 支持矩阵已写入发布承诺。
- [ ] event fixtures 覆盖普通工具、并行工具、Code Mode、diff、compaction、subagent 和失败路径。
- [ ] 默认安全模式保持 `workspace-write + ask`，无任何 silent fallback。
- [ ] 发布物不混入独立 Cordis 第二实例。

## 15. 建议立即执行的两周计划

### 第 1 周

1. 建立 ADR，确认同进程 TUI 与包边界。
2. 创建未发布的 `tui` bundle/startup skeleton。
3. 完成 Ink 终端兼容和 10k event 性能 spike。
4. 用真实 Harness context 跑通 create、followup、stream、approval、cancel、dispose。
5. 固化 8 类 session event fixtures。

### 第 2 周

1. 完成纯 session reducer 和 replay/live 等价测试。
2. 完成 AgentController 与 ApprovalBridge。
3. 完成最小 transcript、composer、状态栏和 generic tool card。
4. 增加 PTY E2E：发送、resize、Ctrl+C、退出后终端恢复。
5. 做阶段评审：Ink 是否继续、事件 API 是否足够、是否存在必须上游修改的缺口。

两周结束的演示必须是一个真实 coding task，而不是静态 mock：Agent 在 sandbox 中读取文件、请求一次写权限、修改文件、运行测试、展示 diff，然后退出并可恢复该会话。

## 16. 最终建议

这项产品最稳妥的实现路径是“**Harness 做 Agent 产品内核，Cordis 做实时组合内核，TUI 做驱动器和事件投影**”。三层边界保持清晰：

- Harness 决定 Agent 如何运行、记录事实、调用工具和执行策略。
- Cordis 决定能力如何挂载、隔离、热变更和可靠释放。
- TUI 决定用户如何观察、输入、审批、导航和诊断，但不绕过前两层执行任何动作。

按此路线，P0 只需新增 TUI surface 和少量适配层，大部分高风险能力沿用已经存在的 session、tool pipeline、sandbox 与 approval 语义。真正需要优先投入的不是更多 Agent 功能，而是 replay/live 一致性、审批竞态、终端清理、大输出性能和故障恢复；这些指标决定它能否从演示变成工程师每天使用的 code agent 产品。
