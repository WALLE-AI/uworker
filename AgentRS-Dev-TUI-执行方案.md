# AgentRS Dev TUI 执行方案

> 文档状态：可执行设计稿  
> 编制日期：2026-08-30  
> 适用源码：`agentrs/` 当前工作区  
> 参考文档：[DeepSeek Harness/Cordis Code Agent TUI 执行方案](DeepSeek-Harness-Cordis-Code-Agent-TUI-执行方案.md)  
> 定位：用于开发、验证和 dogfooding AgentRS 的非生产终端测试宿主

## 0. 执行摘要

本方案建设的不是 uworker 产品 UI，也不是 AgentCore 的替代品，而是一个与 AgentRS
同进程运行的 Rust Dev TUI。它的任务是尽早、持续地验证以下真实链路：

1. `RuntimeHost` 创建、驱动、取消和恢复 Run；
2. provider 流式输出、工具提议、Policy、Sandbox 和 ChangeSet；
3. durable replay 与 live 事件在终端中的一致投影；
4. steering、审批、挂起、恢复、崩溃和有界关闭；
5. 真实 LLM 与开发 adapter 组合后能否完成真实代码任务。

推荐运行形态：

```text
agentrs-tui [--workspace <path>] [--resume <event-log>]
            [--model <model>] [--permission <mode>]

  -> Dev TUI bootstrap
  -> RuntimeHost + RunHandle
  -> AgentRS Engine
  -> ProviderPort / InteractiveDevPolicy / LocalFileSandbox
  -> JsonlPersistence + RunEventSink
  -> reducer -> Ratatui renderer
```

核心决策：

| 决策 | 选择 | 理由 |
|---|---|---|
| 产品定位 | 非生产测试宿主 | 不承担账户、密钥、正式授权、安装器或商业 UI 职责 |
| 集成方式 | Rust 同进程嵌入 AgentRS | 直接使用 `RuntimeHost` 和 `RunHandle`，能真实测试取消与 steering |
| 交付形态 | 独立 `agentrs-dev-tui` crate，二进制名 `agentrs-tui` | 隔离 TUI 依赖，不把 UI 责任塞进内核或生产 CLI |
| TUI 技术 | Ratatui + Crossterm，版本在 Phase 0 spike 后锁定 | 与 Rust workspace 一致，便于 PTY 测试和跨平台终端控制 |
| durable 权威 | `JsonlPersistence` 中的 durable `RunEvent` | 恢复、审计和 transcript 不依赖 UI 内存 |
| live 状态 | 独立 `RunEventSink` | live delta 不写 durable 日志，不与 durable `seq` 混排 |
| 审批 | TUI 实现测试用 `InteractiveDevPolicy` | 人工 Allow once/Reject；无 UI、超时或退出时 fail closed |
| 执行 | 复用 `LocalFileSandbox`，保持 L0 非生产标识 | 继续验证 H1/H2/H7，不给 TUI 直接文件或进程执行权 |
| 首版范围 | 单工作区、单前台 Run | 先验证 AgentRS，不建设完整终端 IDE |

## 1. 目标与边界

### 1.1 目标用户

- AgentRS 内核开发者；
- provider、Policy、Persistence、Sandbox adapter 开发者；
- 需要复现恢复、取消、审批和工具链路问题的测试人员；
- 在接入 AgentCore/AgentUI 前执行真实模型 dogfooding 的维护者。

### 1.2 核心测试任务

- 发送真实任务并观察 provider 增量、最终文本和 token usage；
- 在 Run 运行时提交 steering 输入，验证安全边界领取语义；
- 取消 Run，确认 driver、工具任务、监听器和终端状态全部收敛；
- 查看工具提议、输入指纹、审批、执行结果和 ChangeSet；
- 在退出前选择 Commit 或 Discard，且默认不提交；
- 从 JSONL 日志恢复，显示 RecoveryPlan，并验证不会重复副作用；
- 运行真实 DeepSeek-R1 或其他 OpenAI-compatible 模型完成代码任务；
- 导出可复现的脱敏诊断摘要，而不是复制终端画面猜问题。

### 1.3 明确不做

- 不建设正式 AgentUI、账户系统、订阅、插件市场或自动更新；
- 不保存 API key，不读取系统 Keychain，不把完整环境变量写进日志；
- 不让 TUI 直接执行 Shell、直接写工作区或自行签发生产 grant；
- 不提供 `allow always`；每次人工放行只产生一次性 grant；
- 不显示或持久化模型私有 chain-of-thought；
- 不自建第二套 Agent loop、恢复状态机、工具协议或权限模型；
- P0 不做鼠标优先操作、多工作区、多会话并发和远程 runner；
- P0 不做持久 PTY 面板；PTY 仅用于驱动 TUI E2E，真实持久终端仍依赖 SandboxRS。

## 2. 当前源码基线与必须先补的接缝

### 2.1 已可直接复用

| 能力 | 当前实现 | Dev TUI 用法 |
|---|---|---|
| Run 生命周期 | `agentrs-runtime::RuntimeHost` | create/resume，持有 `StartedRun` |
| steering/cancel | `RunHandle::submit/cancel` | UI controller 直接调用，不绕 JSONL 命令协议 |
| durable 恢复 | `resume_from_events` + `RecoveryPlanner` | 先加载日志、展示计划，再执行合法恢复动作 |
| provider | `RoutingProvider` + OpenAI-compatible transport | 使用现有环境注入方式，TUI state 不保存 key |
| 持久化 | `JsonlPersistence` | 作为 durable 事实唯一来源 |
| Policy 契约 | `PolicyEnforcer` + `ApprovalRequest/Outcome` | 实现交互式测试 Policy |
| Sandbox | `LocalFileSandbox` | 继续验证一次性 grant、输入 hash 和 overlay |
| 投影 | `agentrs-observability` | transcript/tool/approval/cache 视图不自建第二事实源 |
| 真实模型测试 | provider/subagent live tests | 扩展为 TUI PTY E2E 的 opt-in 门禁 |

### 2.2 Phase 0 必须修正的缺口

#### G1：durable persistence 与 live delivery 混在一个 port

当前 Engine 把 durable 和 live 事件都调用 `RunPersistence::append_event`。开发 JSONL
实现会给 live 事件分配 durable `seq` 并写盘，这与 contracts 中“live 可丢失、不参与恢复、
使用独立 `live_seq`”的定义不一致。

修正：新增窄接口 `RunEventSink`，职责只包括向宿主发布已成形事件。

```rust
#[async_trait]
pub trait RunEventSink: Send + Sync {
    async fn publish(&self, event: RunEventEnvelope) -> Result<(), EventSinkError>;
}
```

约束：

- durable 事件先成功写入 Persistence、取得 `seq`，再投递 Sink；
- live 事件不进入 Persistence，由 Engine 分配 `live_seq` 后投递 Sink；
- sink 不可用不得回滚已经提交的 durable 事实；
- bounded channel 满时，durable 通知可从 Persistence 重放，live delta 可合并或丢弃并计数；
- 所有 drop/coalesce 都产生不含正文的诊断计数。

#### G2：`TextDelta` 没有正文

当前 `EventPayload::TextDelta` 是 unit variant，TUI 无法流式渲染。修改为仅存在于 live
plane 的结构化载荷：

```rust
TextDelta { text: String }
```

约束：

- `text` 只发往 live sink，不进入 JSONL durable log；
- 首个非空 delta 之前仍提交 `PartialOutputStarted` durable 事实；
- sink 丢失后，最终 `SurfaceMessageRecorded` 必须纠正 UI；
- `ThinkingDelta` P0 只显示活动状态，不暴露私有推理正文。

#### G3：缺少交互式开发 Policy

新增 `InteractiveDevPolicy`，建议位于 `agentrs-dev-adapter`，以便独立执行
conformance 和单元测试。行为：

- read-only allowlist 可按配置自动签发一次性 grant；
- mutating 工具返回 `RequireApproval`；
- `await_approval` 通过 bounded channel 将完整 `ApprovalRequest` 交给 TUI；
- 用户 Allow once 后，Policy 向 `LocalFileSandbox` 登记绑定 input hash 的一次性 grant；
- Reject、超时、channel 关闭、TUI panic、Run cancel 一律不签发 grant；
- `redeem` 只接受本实例真实签发并仍存活的 token；
- 不支持 `allow always`，不把裁决写入全局配置。

#### G4：RunSummary 缺少累计 usage

P0 状态栏需要真实 token 使用量。将 provider `Done.usage` 累计到 Run 级账本，并通过
durable usage 事实或 `RunSummary` 暴露。任何费用展示必须明确是“未知”“估算”或“端点报告”，
不能把缺失值显示为 0。

## 3. 目标架构

```text
┌──────────────────────── agentrs-tui process ────────────────────────┐
│ Bootstrap                                                            │
│  ├─ argv/config validation                                           │
│  ├─ terminal guard                                                   │
│  └─ provider/dev-adapter assembly                                    │
│                                                                      │
│ DevTuiController                                                     │
│  ├─ RuntimeHost                                                      │
│  ├─ RunHandle                                                        │
│  ├─ driver task                                                      │
│  ├─ command channel                                                  │
│  └─ shutdown coordinator                                             │
│                                                                      │
│ AgentRS                                                              │
│  ├─ Engine                                                           │
│  ├─ JsonlPersistence ───────────────> durable log                     │
│  ├─ ChannelRunEventSink ────────────> event channel                   │
│  ├─ InteractiveDevPolicy <──────────> approval channel                │
│  └─ LocalFileSandbox ───────────────> ChangeSet overlay               │
│                                                                      │
│ TUI                                                                  │
│  event channel -> pure reducer -> AppState -> Ratatui renderer        │
│  keyboard      -> command router -> controller/policy                 │
└──────────────────────────────────────────────────────────────────────┘
```

### 3.1 建议目录

```text
agentrs/crates/agentrs-dev-tui/
  Cargo.toml
  src/
    main.rs                 # 启动、参数、退出码
    bootstrap.rs            # 依赖装配，不含绘制
    controller.rs           # RunHandle、driver、命令路由
    event_sink.rs           # bounded RunEventSink
    state.rs                # 纯 reducer 和 AppState
    terminal.rs             # raw mode/alternate screen/TerminalGuard
    approval.rs             # modal state 与 Policy 回答桥
    recovery.rs             # RecoveryPlan -> 用户可见动作
    sanitize.rs             # 控制字符/ANSI/OSC 清洗
    ui/
      mod.rs
      frame.rs
      transcript.rs
      tool_card.rs
      approval_modal.rs
      status.rs
      composer.rs
  tests/
    pty_smoke.rs
    live_deepseek.rs
```

内核 crate 不得依赖 `agentrs-dev-tui`。TUI 可以依赖 runtime/contracts/types/provider/
observability/dev-adapter/testkit。

## 4. 状态和事件模型

### 4.1 三层状态

| 状态 | 权威来源 | 持久化 | 示例 |
|---|---|---|---|
| Durable domain state | JSONL `RunEvent` + Projection Registry | 是 | transcript、工具结果、审批审计、终态 |
| Live run state | `RunEventSink` + driver lifecycle | 否 | streaming text、running/canceling、队列深度 |
| View state | TUI reducer | 否 | 焦点、滚动、折叠、composer、modal |

### 4.2 合并规则

- durable 去重键固定为 `(run_id, event_id)`，不能只用 `seq`；
- durable 只按同一 Run 内的 `seq` 排序；
- live 只按 `live_seq` 处理，不能和 durable `seq` 比大小；
- live 使用 `parent_event_id`/causality 挂到最近的 durable 锚点；
- 恢复只读取 durable 事件；live delta 丢失是合法情况；
- `SurfaceMessageRecorded` 到达时，以 committed 内容校正相应 live buffer；
- 未知事件显示 generic diagnostic row；若 `SpecVersion` 超出兼容窗口则拒绝恢复；
- reducer 不保存 API key、完整环境变量或未脱敏诊断正文。

### 4.3 reducer 输入

```rust
enum AppAction {
    Replay(Vec<RunEventEnvelope>),
    Event(RunEventEnvelope),
    DriverStarted { run_id: RunId, epoch: RunEpoch },
    DriverFinished(RunSummary),
    ApprovalRequested(ApprovalRequest),
    ApprovalResolved { call_id: ToolCallId, allowed: bool },
    InputAccepted(InputAccepted),
    RecoveryRequired(RecoveryPlanView),
    SinkLagged { dropped_live: u64 },
    Ui(UiAction),
}
```

相同 durable 前缀必须产生相同 domain projection；焦点和滚动不参与该等价断言。

## 5. 核心工作流

### 5.1 启动

```text
解析参数
-> 校验 TTY、工作区、provider 配置和日志路径
-> 创建 TerminalGuard（此时才进入 raw mode）
-> 装配 Persistence/EventSink/Policy/Sandbox/Provider
-> 新建或读取恢复日志
-> 启动 controller + renderer loop
```

任何配置错误必须在进入 raw mode 前失败。启动后首屏持续显示非生产、L0 隔离和默认
不提交标识。

### 5.2 新建 Run

```text
用户提交 composer
-> RuntimeHost::start_with_tools
-> 保存 RunHandle
-> spawn driver（归 controller owner）
-> RunHandle::submit(UserInput::Message)
-> event sink 驱动 reducer
```

P0 同时只允许一个活动 Run。再次新建前必须让旧 driver terminal 并完成 cleanup。

### 5.3 steering 与取消

- Run 活动时 `Enter` 默认调用 `submit`，显示“已入队”，不谎称已经进入模型上下文；
- `Esc` 打开运行控制层：`Queue message`、`Cancel run`、`Cancel and start new run`；
- “取消并改说”严格执行 `cancel -> 等待 terminal -> 新 Run submit`，不能在旧 Run 中扩权复活；
- 第一次 `Ctrl+C` 请求取消，第二次进入有界关闭；不得直接 `process::exit` 绕过 terminal restore。

### 5.4 审批

```text
PolicyDecision::RequireApproval
-> InteractiveDevPolicy::await_approval
-> approval channel
-> modal 展示工具、风险、工作区、ChangeSet 和规范化参数
-> Allow once / Reject
-> oneshot response
-> Policy 签发或拒绝
-> ToolRound 继续并提交 StepResult
```

modal 关闭、Run cancel、deadline 到期、TUI 退出、channel 断开均等价于拒绝或 Pending，
绝不能默认为 Allow。

### 5.5 ChangeSet

- 状态栏显示 pending 文件数；
- 默认退出不提交；
- Commit 是独立的宿主动作，必须二次确认并显示目标工作区；
- P0 支持 Discard 当前内存 overlay；若 adapter 尚无显式 discard API，先补 API 和测试；
- Commit/Discard 不伪装成 AgentRS 内核事件，必要时记录测试宿主诊断事实。

### 5.6 恢复

```text
读取并 validate durable log
-> Projection replay
-> RecoveryPlanner
-> 展示明确计划
-> 仅对 Fresh / RetryModelRequest / ReuseCompaction 自动允许继续
-> partial output / reconcile / redeem approval / reissue approval 要求人工确认或外部动作
-> RuntimeHost::resume_from_events
```

TUI 不生成假的 `TurnEnded`，不把未决 intent 变成新工具调用，不因用户按 Enter 就跳过
reconcile。

### 5.7 有界关闭

```text
停止接收新输入
-> 拒绝全部 pending approval
-> RunHandle::cancel
-> 等待 driver terminal（有超时）
-> flush durable persistence
-> dispose event/approval channels
-> restore cursor/raw mode/alternate screen/bracketed paste
-> 输出稳定退出码
```

必须使用 RAII `TerminalGuard` 作为 panic/early-return 兜底；正常关闭仍走显式 async drain。

## 6. P0 界面

```text
┌ AgentRS Dev TUI · NON-PRODUCTION · L0 · no auto-commit ─────────────┐
│ run r-...  epoch 2  model DeepSeek-R1  mode Default  running       │
├─────────────────────────────────────────────────────────────────────┤
│ User  检查缓存代际归因并运行相关测试                               │
│                                                                     │
│ Agent 我先读取相关实现。▌                                          │
│                                                                     │
│ ▾ Read  crates/agentrs-context/src/cache.rs                 allowed │
│ ▾ Edit  crates/agentrs-context/src/cache.rs                 pending │
│   approval required · ChangeSet cs-dev-tui                         │
│ ✓ Test  cargo test -p agentrs-context                         65 ok │
│                                                                     │
├─────────────────────────────────────────────────────────────────────┤
│ turns 1  steps 3  pending 1  input 1.2k  output 340  sink-drop 0   │
├─────────────────────────────────────────────────────────────────────┤
│ > 补充一个恢复测试_                                                │
└ Enter send · Esc run control · Ctrl+P actions · Ctrl+C cancel ─────┘
```

### 6.1 信息优先级

1. 必须可见：非生产标识、隔离级别、Run 状态、待审批、错误、是否有未提交改动；
2. 默认展开：用户/助手文本、失败工具、短结果、审批；
3. 默认折叠：成功长输出、完整工具参数、重复 delta；
4. 诊断层：event id/seq/live_seq、epoch、causality、generation、cache attribution。

### 6.2 P0 键位

| 键位 | 行为 |
|---|---|
| `Enter` | 发送；modal 中确认当前选择 |
| `Alt+Enter` | composer 换行 |
| `Esc` | 关闭 modal，或打开 steering/cancel 控制层 |
| `Ctrl+C` | 请求取消；再次按进入有界退出确认 |
| `Ctrl+P` | 动作面板：new/resume/mode/commit/discard/export/quit |
| `Ctrl+O` | 展开/折叠当前工具卡片 |
| `Ctrl+R` | 选择并验证恢复日志 |
| `Ctrl+L` | 重绘，不清空 Run |
| `Tab` / `Shift+Tab` | transcript、composer、modal 之间移动焦点 |
| `PgUp/PgDn` | 浏览历史，向上滚动后暂停自动跟随 |
| `?` | 帮助和当前能力说明 |

## 7. 安全要求

- 所有 provider、工具和文件文本写终端前移除 C0/C1 控制字符和危险 ANSI/OSC；
- 不渲染模型提供的原始终端 escape；样式只由 TUI 生成；
- 路径必须相对工作区展示，绝对路径只在显式诊断视图中脱敏显示；
- approval modal 使用结构化参数，不拼 Shell 文本；
- TUI 不读取或显示 `AGENTRS_API_KEY` 的值；错误只输出稳定错误码；
- crash dump、snapshot 和录屏 fixture 使用 fake key 与合成内容；
- 默认 `PermissionMode::Default`，允许显式切到 `Plan`；进入更宽的 `Accepted` 必须走
  AgentRS 已有的用户来源 transition 规则；
- TUI 显示 AgentRS 实际 `PermissionMode`，不发明 `workspace-write/danger-full-access` 映射；
- `LocalFileSandbox` 实际隔离为 L0，任何地方不得标成 L1；
- 无 Policy、Sandbox、Persistence 或 EventSink 时启动失败，不静默替换为自动放行实现；
- commit 是宿主显式动作，退出、取消或 RunCompleted 都不自动 commit。

## 8. 分阶段执行计划

单人预计 24 至 34 人日，另加 20% 的终端平台和现有接缝修正缓冲。P0 目标是测试工具，
不以产品视觉完整度扩张范围。

### Phase 0：内核接缝修正与技术 spike（4-6 人日）

任务：

- `TUI-001`：ADR，冻结 Dev TUI 定位和 crate 边界；
- `TUI-002`：拆分 `RunPersistence` 与 `RunEventSink`；
- `TUI-003`：为 live delta 增加正文和独立 `live_seq`；
- `TUI-004`：修正 JSONL 不持久化 live 事件；
- `TUI-005`：实现 `InteractiveDevPolicy` 和一次性 grant；
- `TUI-006`：Ratatui/Crossterm spike，验证 Windows Terminal、中文、resize、panic restore；
- `TUI-007`：采集 keyless durable/live/approval/recovery fixtures。

退出条件：无 UI 的测试中能同时收到带正文的 live delta 和带 `seq` 的 durable 事件；
JSONL 中不存在 live 事件；审批 allow/reject/timeout/cancel 均满足 H1 和 fail-closed。

### Phase 1：无绘制 runtime（4-5 人日）

- `TUI-101`：纯 `AppState` reducer；
- `TUI-102`：durable replay/live merge 和 committed correction；
- `TUI-103`：`DevTuiController` create/submit/cancel/finish；
- `TUI-104`：approval request/answer channel；
- `TUI-105`：recovery plan adapter；
- `TUI-106`：shutdown coordinator 和资源计数；
- `TUI-107`：headless integration tests。

退出条件：同一 durable fixture 的一次性 replay 与逐条输入产生相同 domain state；live
delta 丢失不改变最终 committed transcript；所有取消/异常路径资源归零。

### Phase 2：基础终端界面（5-7 人日）

- `TUI-201`：TerminalGuard、事件循环和帧率限制；
- `TUI-202`：header、transcript viewport、composer、status line；
- `TUI-203`：多行输入、bracketed paste、中文/宽字符；
- `TUI-204`：焦点和按键状态机；
- `TUI-205`：自动跟随、历史滚动、未读计数；
- `TUI-206`：窄屏、低高度和无色降级；
- `TUI-207`：fake provider PTY smoke。

退出条件：80x24 和 160x50 均可完成 fake Run；resize、Ctrl+C、EOF 和 panic 后终端立即恢复。

### Phase 3：工具、审批与恢复（6-8 人日）

- `TUI-301`：generic tool card 和稳定 call/result 配对；
- `TUI-302`：Read/Grep/Write/Edit/Delete 专用摘要；
- `TUI-303`：approval modal、deadline、cancel 竞态；
- `TUI-304`：ChangeSet pending/commit/discard；
- `TUI-305`：恢复选择、validate、RecoveryPlan 和人工动作；
- `TUI-306`：permission mode 切换；
- `TUI-307`：trajectory/cache/component diagnostics pane。

退出条件：人工批准一次写操作、拒绝第二次操作、恢复中断 Run、查看未提交 diff/摘要，
且不存在重复执行或自动提交。

### Phase 4：真实模型、压力与交付（5-8 人日）

- `TUI-401`：SiliconFlow DeepSeek-R1 opt-in PTY E2E；
- `TUI-402`：真实任务：使用 dev adapter 完成读文件、申请写、修改、显示结果和恢复；
- `TUI-402A`：命令执行分支使用 fake `ExecCommand` 验证 TUI 投影；只有真实 SandboxRS
  注入并通过相应 conformance 后，才增加真实测试命令 E2E；
- `TUI-403`：1k/10k events、大工具输出和高频 delta 压力测试；
- `TUI-404`：恶意 ANSI/OSC/Unicode 安全 fixtures；
- `TUI-405`：Windows/Linux/macOS CI build，Windows PTY 主验证；
- `TUI-406`：README、诊断说明和非生产支持声明；
- `TUI-407`：全 workspace regression 和 conformance。

退出条件：真实 LLM 任务通过；无 key 环境的全部测试稳定通过；终端、Run、Policy、Sandbox
资源在正常、取消、超时和故障路径均收敛。

## 9. 测试策略

### 9.1 测试层级

| 层级 | 内容 | 是否需要 key |
|---|---|---|
| 单元 | reducer、序号、配对、截断、sanitize、按键状态机 | 否 |
| property | replay/live 等价、乱序/重复/gap、任意 Unicode | 否 |
| adapter | InteractiveDevPolicy、EventSink、JSONL、Sandbox | 否 |
| 组合 | RuntimeHost + fake provider + dev adapter + headless controller | 否 |
| PTY E2E | raw mode、resize、输入、取消、退出恢复 | 否 |
| recovery | 五个 durable 边界与 approval token | 否 |
| provider E2E | 真实 DeepSeek-R1 流和 usage | 是，显式 opt-in |
| coding E2E | 真实仓库读改测闭环 | 是，使用临时工作区 |

### 9.2 必测场景

- live delta 缺失、重复或延迟，最终 committed message 仍正确；
- durable event 重投递使用 `event_id` 去重，即使返回不同 seq 也不重复显示；
- replay 期间到达 live event，不与 durable seq 比较；
- 用户编辑多行文本时审批到达，草稿不丢；
- approval allow 与 cancel、deadline、channel close 同时发生，只能有一个结果；
- 首个可见 delta 后崩溃，恢复明确显示 partial output，不静默重发；
- StepIntent 已提交、StepResult 缺失时必须 reconcile；
- ChangeSet 有未提交修改时取消和退出均不落盘；
- terminal resize 为 0x0、stdin EOF、renderer panic、第二次 Ctrl+C；
- 恶意 ANSI、OSC 8/52、超长无空格文本、emoji、组合字符、中文宽字符；
- sink channel 塞满时运行不中断，drop 计数可见，最终消息被 committed 事实纠正；
- API key 不出现在 state debug、错误、snapshot、JSONL 或诊断导出中。

### 9.3 性能预算

| 指标 | P0 门槛 |
|---|---:|
| 按键到本地回显 p95 | < 50 ms |
| live event 到可见 p95 | < 100 ms |
| 10k durable events 恢复到首屏 | < 1.5 s |
| streaming 重绘 | <= 60 FPS，默认合并到 30 FPS |
| Ctrl+C 到 Run terminal | 正常路径 < 3 s |
| Ctrl+C 到终端恢复 | 最迟 < 5 s |
| 单工具卡片内联正文 | 默认 <= 256 KiB |
| 10k events 稳态 RSS（不含 provider 服务） | < 150 MiB |

## 10. 第一批 PR 切分

1. **PR-01 ADR + Event plane correction**：Dev TUI 边界、RunEventSink、live_seq、JSONL 修正。
2. **PR-02 InteractiveDevPolicy**：approval channel、一次性 grant、并发和 conformance tests。
3. **PR-03 Dev TUI skeleton**：crate、bootstrap、TerminalGuard、非生产横幅、help。
4. **PR-04 Runtime reducer/controller**：replay/live、RunHandle、shutdown、headless tests。
5. **PR-05 Base UI**：frame、transcript、composer、status、key routing。
6. **PR-06 Tool and approval UI**：tool cards、modal、ChangeSet、permission mode。
7. **PR-07 Recovery and diagnostics**：validate、RecoveryPlan、trajectory/cache pane、export。
8. **PR-08 PTY/performance/live E2E**：跨平台 smoke、压力门、真实 DeepSeek 测试和文档。

每个 PR 都必须保持：

- `cargo fmt --all -- --check`；
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`；
- `cargo test --workspace --all-targets`；
- `scripts/check-no-env.sh`；
- `scripts/check-port-attribution.sh`；
- `git diff --check`；
- 凭据扫描无命中。

## 11. 建议立即执行的两周计划

### 第 1 周

1. 完成 ADR 和 `RunEventSink` contract；
2. 修复 live 不落 JSONL、补 `live_seq` 和 `TextDelta { text }`；
3. 完成 keyless event fixtures 和 replay/live property tests；
4. 完成 `InteractiveDevPolicy` allow/reject/timeout/cancel 测试；
5. 完成 Ratatui/Crossterm Windows Terminal spike和 TerminalGuard。

第 1 周演示：headless controller 启动 fake Run，实时收到文本，人工批准 Write，取消 Run，
JSONL 只含 durable 事实，所有 channel/task 归零。

### 第 2 周

1. 建立 `agentrs-dev-tui` crate 和 bootstrap；
2. 完成 reducer、controller、shutdown coordinator；
3. 完成 transcript、composer、status 和基础工具卡片；
4. 增加 PTY E2E：输入、resize、审批、Ctrl+C、终端恢复；
5. 使用真实 DeepSeek-R1 跑只读任务，再跑临时工作区写入审批任务；命令执行先用 fake
   `ExecCommand` 验证显示和取消，不允许 TUI 绕过 Sandbox 自行启动测试进程。

第 2 周演示必须是真实 AgentRS Run：读取临时仓库、请求一次写权限、修改文件、显示
committed 结果、退出后恢复同一日志。测试命令只有在 SandboxRS `ExecCommand` 已注入时才
真实执行，否则使用明确标记的 fake 工具验证 TUI 路径；不得用静态 mock 画面代替整个 Run。

## 12. 完成定义

P0 只有同时满足以下条件才算完成：

- `agentrs-tui` 可新建和恢复 Run；
- 文本真正流式显示，最终 committed transcript 可纠正丢失 delta；
- steering、cancel、审批 Allow once/Reject 全部走 AgentRS 真实路径；
- 工具提议、意图、结果、ChangeSet 和终态可审计；
- 任一审批 transport 故障均不放行；
- 默认不 commit，取消和退出不会落盘；
- 五个危险恢复边界不会重复 provider 可见输出或工具副作用；
- 终端在正常、取消、panic、EOF 和信号退出后恢复；
- 10k 事件和大输出达到性能预算；
- keyless CI 全绿；
- 至少一次真实 DeepSeek-R1 只读测试和一次临时工作区读写审批闭环通过；
- `ExecCommand` 的 fake E2E 必须通过；真实 SandboxRS 可用后再把真实测试命令升级为硬门槛；
- 日志、状态 dump、snapshot 和诊断导出不含 API key；
- 文档持续声明它是 L0、非生产测试宿主，不得作为 AgentCore/AgentUI 发布。

## 13. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| 为 TUI 修改事件契约引发兼容回归 | 高 | 先写 P0 旧事件兼容 fixture；SpecVersion/serde 迁移明确化 |
| sink 背压拖慢 Agent | 高 | bounded channel、delta coalescing、durable 可重放、drop 指标 |
| approval/cancel 竞态误放行 | 极高 | oneshot 单决议、grant 最后时刻签发、并发故障测试 |
| TUI 逐渐变成产品 UI | 中 | 完成定义和非目标守门；账户/更新/多会话不进入 crate |
| dev adapter 被误认为生产隔离 | 高 | 固定横幅、L0 标签、二进制名和文档均带 dev/test |
| Windows raw mode/IME 不稳定 | 中高 | Phase 0 淘汰门；先保证 line-input fallback，再启用高级 composer |
| 大输出导致内存和重绘失控 | 高 | 轻量索引、viewport、硬上限、spill ref、帧率合并 |
| 真实 LLM 测试不稳定或昂贵 | 中 | keyless fixture 是主门；live E2E 限预算、固定任务、显式 opt-in |
| 当前 RunSummary/usage 不完整 | 中 | Phase 0 明确补账本；未知值绝不显示为 0 |

## 14. 开工检查表

- [x] ADR 明确 Dev TUI 非生产定位和 crate 依赖方向；
- [x] `RunEventSink` 与 Persistence 的错误语义通过实现审阅和测试；
- [x] live/durable 序号和去重规则有 contract tests；
- [x] `TextDelta` 正文不会进入 durable JSONL；
- [x] `InteractiveDevPolicy` 能独立通过新增审批 conformance；
- [ ] TerminalGuard 在 panic、EOF 和双 Ctrl+C 下均恢复终端；
- [x] Ratatui/Crossterm 版本和 MSRV 1.85 兼容性已锁定；
- [x] Windows/Linux/macOS 支持矩阵写入 README；
- [x] fake fixtures 覆盖普通文本、工具、审批、恢复、取消和 sink lag；
- [x] 真实 LLM 用例有 token/时间上限且默认不运行；
- [ ] 临时写入 E2E 使用独立临时工作区，不触碰真实用户仓库；
- [x] API key 只通过进程环境注入且从不进入 TUI state。

## 15. 最终建议

AgentRS Dev TUI 应被视为“可交互的集成测试仪器”，而不是“终端版产品”。它最重要的
价值不是界面本身，而是迫使 live event、审批、取消、恢复、ChangeSet 和资源清理在一个
真实宿主中闭环。

实施顺序必须坚持：

```text
先修事件与审批接缝
-> 再做无 UI controller/reducer
-> 再做终端渲染
-> 最后做真实模型与压力验收
```

跳过前两步直接画界面，会得到一个能显示最终文本、却无法可靠测试 AgentRS 的终端壳；
这与建设 Dev TUI 的目的相反。
