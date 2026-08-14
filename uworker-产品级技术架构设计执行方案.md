# uworker 桌面 AI 产品技术架构与执行方案

> 版本：v2.0，桌面优先
>
> 定位：uworker 是面向个人与团队知识工作的商业 AI 桌面产品，形态参考 AionUi 的本地 Agent 桌面宿主，并吸收 WorkBuddy/QoderWork 的产品能力。首发目标不是云端 Agent 平台，而是在用户电脑上安全地完成文件、文档、浏览器、Shell 和办公系统任务；云端只提供账号、订阅、模型网关、同步、更新和团队能力。

---

## 1. 架构结论

uworker 由 `agentui`、`agentcore`、`agentrs`、`sandboxrs` 四个模块组成。前两者是桌面产品和本地控制面，后两者是并列的 Rust 执行模块；它们随同一个桌面安装包分发。

```text
用户桌面
  +-------------------- AgentUI ---------------------+
  | Electron/Tauri 主进程 | React Renderer | Preload  |
  | 工作台、对话、任务、审批、文件、技能、设置、更新    |
  +------------------------+--------------------------+
                           | 受限 IPC + localhost API / WebSocket
  +------------------------v--------------------------+
  |                  AgentCore（本地后台）              |
  | API、会话、任务编排、文件索引、策略、审批、SQLite    |
  | 本地 MCP、技能、浏览器、Shell、更新/账号服务适配器   |
  +------------------------+--------------------------+
                           | 进程内 Rust trait / 受控 IPC
              +------------+-------------+
              v                          v
  +------------------------+  +--------------------------+
  | AgentRS（推理执行内核）  |  | SandboxRS（安全执行内核）  |
  | loop、模型、上下文、调度 |  | 隔离、资源、进程、网络、  |
  | 子 Agent、压缩、事件    |  | 文件挂载、凭据、销毁审计  |
  +-----------+------------+  +------------+-------------+
              |                            |
       云端/本地模型                 文件/Shell/浏览器/MCP/Office

可选 uworker Cloud：账号、订阅授权、模型网关、设备同步、团队共享、远程查看。
它不是首发任务执行路径，也不应拥有用户本地文件的默认读取权限。
```

| 模块 | 产品定位 | 首发职责 | 禁止承担的职责 |
|---|---|---|---|
| `agentui` | AI 桌面产品与可信宿主 | 交互、系统集成、窗口/托盘、文件选择、审批、设置、更新、渲染实时状态 | 直接执行任意 Shell、保存明文密钥、做最终安全决策 |
| `agentcore` | 随安装包分发的本地控制面 | 会话/任务权威状态、策略与审批、SQLite、文件与技能索引、运行时编排、审计 | 将 Agent loop、模型协议、工具实现塞进 HTTP handler |
| `agentrs` | 推理执行内核 | Agent loop、Provider、上下文预算、工具调度、技能加载、子 Agent、流式输出 | Electron API、账户订阅、UI 状态、直接创建 OS 进程或绕过沙盒 |
| `sandboxrs` | Agent 命令安全执行环境 | 命令隔离、文件变更暂存/预览/提交/撤销、进程回收、资源配额与执行审计 | LLM 推理、任务规划、策略裁决、直接不可恢复删除用户数据 |
| `uworker Cloud` | 可选商业服务 | 登录、许可证、模型代理、同步、遥控中继、团队服务、遥测汇总 | 默认收集工作区文件、替代本地执行器 |

关键决策：桌面版可离线工作。未登录、无云端或使用本地模型时，用户仍能使用本地文件、技能、会话和 Agent；云端仅在用户显式启用的功能上参与。

## 2. 为什么不能直接照搬 Aion

Aion 的 `AionUi -> AionCore -> aionrs` 分层正确，尤其是 Electron 仅通过受控接口使用本地 Rust 后台，aionrs 以库的方式嵌入 Core。但其主要形态仍是“多 Agent 后端的本地编排器”，产品化不足集中在以下方面：安装/升级原子性、桌面权限体验、商业账号与计费、隐私说明、崩溃恢复、文件记忆质量、技能产品化、诊断与遥测。

uworker 应继承三层结构，重点优化为商业桌面产品：

| 参考设计 | 保留 | uworker 产品化增强 |
|---|---|---|
| AionUi 自动拉起本地 AionCore | 保留 | 将 Core 和 Runner 固定打包、签名校验、随机端口与一次性启动令牌、版本握手、崩溃自动重启 |
| AionCore 的 HTTP/WS + 统一事件流 | 保留 | IPC 作为首选控制通道，localhost 仅绑定 `127.0.0.1`；事件可重放、带 `seq/run_id/event_id` |
| aionrs 的 `OutputSink`、Provider/Tool 抽象 | 保留 | 增加持久 checkpoint、工具副作用语义、权限上下文和桌面资源限制 |
| 本地 SQLite 会话 | 保留 | JSONL 追加日志 + 加密快照 + 迁移版本 + 损坏恢复，避免单库损坏导致历史丢失 |
| `SKILL.md` 技能 | 保留 | `skill-manifest.yaml` 参数化模板、示例任务、依赖与权限声明、签名与版本管理 |
| 文件记忆/Markdown 注入 | 升级 | Awareness：文件索引、FTS5、重要性、哈希增量、JIT 检索、可审查反思蒸馏 |
| 工具审批 | 升级 | 在 UI 给出影响摘要、文件 diff、命令、网络目标、密钥范围；审批绑定实际输入哈希 |
| 进程组清理 | 升级 | 最小权限执行环境、资源配额、可取消进程树、Shell 环境快照、受控网络出口 |

## 3. 产品范围与首发体验

### 3.1 首发用户流程

1. 用户安装并启动 uworker，桌面主进程先验证签名、启动本地 Core，并展示加载状态。
2. 用户选择或授权工作区，Agent 不会扫描未授权目录；系统建立轻量文件索引和 Shell 环境快照。
3. 用户使用“任务”而不是空白聊天框发起工作，例如整理文件、生成周报、填充 Word 模板、分析表格、检索资料、浏览网页或运行代码。
4. Agent 先展示计划和所需能力；读取可以自动允许，写入、命令、网络上传、登录态浏览器操作和不可逆动作按策略要求审批。
5. UI 实时展示文本、计划、工具步骤、文件 diff、产物与可恢复的执行时间线；中断、退出或崩溃后可继续、重试或回滚。
6. 用户可将可复用任务保存为技能，并从技能库选择带参数表单的模板任务。

### 3.2 首发能力优先级

| P0，必须可用 | P1，首发后 | P2，商业增强 |
|---|---|---|
| 本地工作区、对话/任务、文件读写、Shell、审批、会话恢复、技能、Office 文档/表格、模型配置、自动更新 | 浏览器 Agent、MCP/连接器、项目记忆、定时任务、远程查看、团队共享 | 多 Agent 团队、云同步、企业 SSO、私有部署、插件市场、设备策略 |

不将复杂 swarm、全自动长期记忆、跨设备实时协同放入 P0。桌面 Agent 的首要质量是稳定、可控、结果可见和不破坏用户环境。特别是，Agent 的命令不能因模型错误直接删除用户本地数据：文件写入先形成可预览变更集，删除只进入可恢复回收区，提交与清理分离。

### 3.3 六层能力模型

参考 WorkBuddy 的分层方式，uworker 在三模块部署架构之上，采用六层能力模型。三模块回答“代码和进程放在哪里”，六层模型回答“产品能力由谁治理”。二者互补，不能混为一谈。

```text
第 1 层  用户交互层：AgentUI 的任务、对话、计划、审批、产物和设置
第 2 层  Agent 推理层：AgentRS 主 Agent、路由 Agent、Plan/Explore/Compact
第 3 层  工具执行层：SandboxRS 驱动文件、Shell、Office、Browser、MCP、Artifact
第 4 层  扩展能力层：Skills、Plugins、MCP Connectors、Hooks、模板任务
第 5 层  记忆与上下文层：会话、Awareness、检索、压缩、checkpoint
第 6 层  安全治理层：工作区授权、沙盒、Policy、审批、审计、密钥、隐私
```

安全治理层横切并约束前五层，而不是只在工具层追加一个确认弹窗。每个能力必须声明资源范围、风险等级、数据出站规则和审计事件；Agent 只能在已授予的范围内规划和调用。

### 3.4 P1/P2 的多 Agent 与上下文能力

P1 实现低风险、函数式子 Agent：例如 `Explore`、`Plan`、`memorySelector`、`compact`、`contextSummary` 和 `promptHookEvaluator`。它们由主 Agent 或 Runtime 内部同步调用，输入受限、输出结构化、默认无工具权限，不创建持久身份，不直接与用户或其他 Agent 通信。

P2 才实现协作式 Team：多个有持久状态的 Worker 围绕共享任务板协作。它适用于长任务和可并行工作，不替代 P1 的函数式子 Agent。

## 4. 部署、进程与信任边界

### 4.1 本地进程拓扑

```text
uworker.app / uworker.exe
  |
  +-- AgentUI Main Process
  |     - 单实例锁、窗口、托盘、自动更新、原生文件选择、Keychain
  |     - spawn agentcore --port 0 --bootstrap-token <one-time-token>
  |
  +-- AgentUI Renderer（无 Node 权限）
  |     - 仅通过 preload 的白名单方法访问本地能力
  |
  +-- AgentCore Local Service（Rust）
        - 随包 binary，127.0.0.1 随机端口或 Unix Domain Socket/Named Pipe
        - 校验 bootstrap token 后签发短期本地 session token
        - 静态链接 agentrs 与 sandboxrs，按 trait 组合两者
        - AgentRS 负责推理/调度；SandboxRS 管理工具、浏览器和 MCP 子进程
```

桌面主进程只能传入固定参数，不能把用户输入拼入启动命令。Core 绝不监听 `0.0.0.0`；远程访问必须是显式开关，默认通过云端受控中继或设备配对，不开放局域网裸端口。

### 4.2 进程间通信

推荐两条通道并存：

1. `Main <-> Core`：Unix Domain Socket/Windows Named Pipe 优先；不方便时使用仅绑定 loopback 的 HTTP/WS。启动时使用一次性 token 双向认证。
2. `Renderer <-> Main`：Electron `contextBridge` 白名单 IPC；Renderer 不直接接触 Core 的管理密钥、文件绝对路径或系统 keychain。

`agentcore` 对 Renderer 暴露版本化 API 和事件，不将 Rust 内部结构或 aionrs DTO 直接跨边界共享。所有实时事件含 `run_id`、递增 `seq`、`event_id`、可见性和时间戳；Renderer 断线后按 `after_seq` 从 SQLite 补齐。

### 4.3 文件系统布局

```text
<app-data>/uworker/
  app/                       可执行文件版本与更新元数据
  db/uworker.sqlite          业务数据库，WAL + 加密密钥引用
  sessions/<session-id>/     JSONL、压缩快照、checkpoint、tool-outputs
  projects/<project-id>/     工作区元数据、awareness、技能绑定、索引
  artifacts/<hash>/          文件产物和大型工具输出，内容寻址
  shell-snapshots/           已脱敏的 Shell 环境快照
  extensions/                已验证的插件/技能包
  logs/                      轮转日志、崩溃报告待上传队列
  runtime/                   PID、端点、短期本地令牌和锁文件
```

所有路径通过内部 `ProjectId`/`ArtifactId` 寻址。UI 默认不获取绝对路径，只有用户明确打开、定位或导出时由 Main Process 解析；日志和云同步绝不上传绝对路径、文件正文、命令输出或密钥。

## 5. AgentUI：商业桌面产品层

### 5.1 技术形态

首发建议 Electron + React + TypeScript，理由是跨平台桌面生态、系统集成、浏览器容器和自动更新链路成熟；若团队已有 Rust 前端能力，也可用 Tauri，但不得因为体积目标牺牲浏览器自动化、窗口管理和更新成熟度。

进程边界必须严格遵守：

| 进程 | 允许 | 禁止 |
|---|---|---|
| Renderer | React、DOM、受限 IPC API | Node、`fs`、Shell、原生模块、长期 token |
| Preload | 参数验证、`contextBridge`、事件转发 | 暴露通用 `ipcRenderer`、执行动态代码 |
| Main | 窗口、系统 API、Keychain、Core 生命周期、升级 | Agent 业务循环、直接拼接 Shell 命令 |
| Core | 数据、文件能力、策略、Agent 编排 | UI 组件状态、Electron 私有 API |

### 5.2 核心界面

1. 工作台：最近项目、进行中的任务、待审批、失败恢复、快捷技能。
2. 任务会话：对话、计划、流式文本、工具步骤、文件 diff、附件和产物预览。
3. 工作区：授权目录、文件改动、索引状态、项目记忆、忽略规则和清理入口。
4. 技能库：内置/用户/扩展技能，模板任务、参数表单、依赖状态、权限说明和版本。
5. 审批中心：动作、风险、影响资源、命令/网络目标、diff、有效期、批准范围。
6. 设置：账号订阅、模型、本地/云端选择、连接器、隐私、更新、诊断和开发者模式。

UI 的状态分为服务端快照、事件投影和纯 UI 临时状态。Run 成功与否只以 Core 返回的终态为准，不能从“最后一条文本”推断。所有危险操作都使用 Core 返回的 `allowed_actions` 和风险说明渲染，提交时再由 Core 复核。

### 5.3 商业桌面能力

- 账号与订阅：设备码/OAuth 登录；许可证离线宽限期；功能 flag 由签名 entitlement 控制，不能只靠 UI 隐藏。
- 更新：签名安装包、增量更新、强制最低安全版本、分批灰度、失败回滚；Core/AgentRS 与 UI 必须使用兼容性矩阵。
- 隐私：首次启动完成数据流说明；遥测 opt-in；云端模型发送前显示提供商、数据范围和保留提示。
- 诊断：一键导出脱敏诊断包；crash report 在用户同意后发送；本地日志可清理。
- 可访问性与国际化：键盘路径、读屏、缩放、简中/英文优先，文案使用产品词汇而非底层协议名。

## 6. AgentCore：本地控制面

### 6.1 责任与模块边界

Core 是本机上的权威状态服务，不是云端 API 的镜像。建议 Rust workspace 按领域拆分：

```text
agentcore/
  app/                 本地启动、配置、依赖注入、health/version
  api-types/           IPC/HTTP DTO、错误码、事件 schema
  identity/            本地 profile、cloud entitlement、token refresh
  workspace/           工作区授权、路径 containment、文件监控
  conversation/        对话、消息、session/run 创建与恢复
  run/                 Run/Step 状态机、租约、取消、重试、checkpoint
  policy/              本地 PDP、审批、规则、风险分类、审计
  memory/              Awareness 索引、检索、反思、保留和删除
  skill/               发现、manifest、模板任务、依赖和安装治理
  tool/                本地工具、MCP、Office/Browser/Shell adapter
  artifact/            文件产物、哈希、预览、保留、导出
  integration/         OAuth 连接器、云模型和本地模型适配
  persistence/         SQLite、迁移、JSONL、加密、备份恢复
  realtime/            事件订阅、重放、背压、事件压缩
  telemetry/           脱敏指标、崩溃与诊断队列
```

每个 service 只依赖 trait/port；HTTP 或 IPC handler 只做认证和请求转换；SQLite、文件系统、模型、MCP 进程都在 adapter 层。这样保留 AionCore 的分层优点，且能单测风险逻辑。

Core 是四模块的唯一装配点：它将 Policy/Approval 结果转为不可扩权的 `SandboxGrant`，再把 `ToolExecutionRequest` 交给 AgentRS；AgentRS 经 `SandboxExecutor` port 交给 SandboxRS。Core 不能把“已批准”简化为可任意执行 Shell 的布尔开关，授权必须带 Run、Step、工作区、路径、网络、资源和到期范围。

### 6.2 本地持久化与恢复

会话采用三层设计，吸收 QoderWork 的优点：

| 层 | 格式 | 目的 |
|---|---|---|
| 追加事件 | `sessions/<id>/events.jsonl` | 写入成本低，崩溃后恢复到最后完整行 |
| 结构快照 | SQLite 行 + `checkpoint.zst` | 快速打开会话、保存 Agent 可恢复状态 |
| 加密保护 | AES-256-GCM，密钥存系统 Keychain/DPAPI/Keychain Services | 防止本地磁盘直接读取敏感会话和凭据 |

写入顺序必须是：追加 JSONL -> SQLite 事务更新 Run/Step 和索引 -> 写 checkpoint -> 发出实时事件。任何一步失败都留下可诊断状态；启动时扫描未终态 Run，校验 JSONL、快照和数据库，选择恢复、重试或标记为需要用户处理。

SQLite 使用 WAL、外键、busy timeout、定期一致性检查和自动备份；迁移幂等且具有 `schema_version`。会话正文、索引和日志必须具有独立保留与清理策略。

### 6.3 Run 与审批状态机

```text
Draft -> Queued -> Running -> WaitingApproval -> Running -> Succeeded
                     |                 |              -> Failed
                     |                 |              -> Canceled
                     +-> Interrupted -> Recovering -> Running / NeedsUserAction
```

- 每个 Run 有 `run_id`、递增 `seq`、`idempotency_key`、工作区和策略快照。
- 每个有外部副作用的动作是持久化 `Step`，保存 `input_hash`、`effect_id`、重试策略和产物引用。
- 崩溃后的网络/写入动作默认是“结果未知”，进入 reconcile；不得盲目再次执行邮件发送、覆盖写入或付款类工具。
- 审批记录绑定 `approval_id + run_id + step_id + input_hash`；批准 A 不能复用于参数变化后的 B。
- 退出应用时，UI 显示运行中任务：等待、取消、安全暂停或后台继续。默认不静默杀掉写入中的任务。

### 6.4 本地策略与权限

Agent 不拥有用户完整桌面权限。每次工具调用经过本地 Policy Decision Point：

| 风险类别 | 默认策略 | UI 呈现 |
|---|---|---|
| 已授权工作区内读取 | 可自动允许 | 工具时间线 |
| 工作区内创建/修改 | 每 Run 或按目录批准 | 文件列表和 diff |
| 删除/覆盖/批量重命名 | 必须批准 | 数量、路径、可回滚性 |
| Shell 执行 | 需要风险判定 | 完整命令、cwd、环境、超时 |
| 网络访问/上传 | 默认询问 | 域名、请求摘要、上传文件 |
| 密钥/登录态使用 | 必须明确 scope | 使用哪个连接器和权限 |
| 工作区外路径 | 默认拒绝 | 用户须通过原生选择器重新授权 |

策略由“能力授予 + 路径 containment + 规则 + 当前审批”共同决定。提示词不构成安全边界。所有 `allow always` 都需要明确作用域和可撤销入口，不能变成无限制 YOLO 模式。

### 6.5 Hook 风险评估与安全控制闭环

规则引擎处理确定性规则：路径是否授权、域名是否允许、工具是否存在、命令是否命中高风险模式、预算是否超限。对规则难以覆盖的语义风险，增加零工具、低成本的 `promptHookEvaluator`：它只接收经过脱敏的“用户意图 + 计划动作摘要 + 工具风险元数据”，返回固定枚举 `allow | require_approval | deny | escalate` 及理由码。

该评估 Agent 不能成为安全唯一依据：高风险类别仍由确定性 Policy 强制审批或拒绝；模型评估只可提高风险等级，不能降低硬规则的限制。完整链路如下：

```text
Agent 提议 ToolCall
  -> 静态 Policy（scope、路径、工具、预算、网络）
  -> Hook 风险评估（可选，只能加严）
  -> Allow / Deny / Approval / Step-up authentication
  -> 受限执行环境
  -> 审计、artifact、可恢复 Step
```

个人文件保护应额外定义默认排除目录和敏感文件模式，例如系统目录、密钥目录、浏览器 profile、SSH/GPG 凭据、密码库、钱包文件和应用数据目录。用户可添加规则，但不能以一次“总是允许”解除对这些高敏范围的保护。

## 7. AgentRS：可恢复的本地 Agent 内核

### 7.1 保留和新增

保留 aionrs 的 provider-neutral message、Provider trait、Tool trait、工具并发标记、context compaction、memory/skills、OutputSink、子 Agent 和 MCP 适配。新增产品级接口：

```rust
pub struct LocalRunSpec {
    pub run_id: RunId,
    pub workspace: AuthorizedWorkspace,
    pub conversation: ConversationSnapshot,
    pub model_policy: ModelPolicy,
    pub capability_grant: CapabilityGrant,
    pub policy_snapshot: PolicySnapshot,
    pub budget: RunBudget,
    pub checkpoint: Option<RunCheckpoint>,
}

pub trait RunPersistence {
    async fn append_event(&self, event: RunEvent) -> Result<EventSequence, RuntimeError>;
    async fn save_step(&self, step: StepRecord) -> Result<(), RuntimeError>;
    async fn save_checkpoint(&self, checkpoint: RunCheckpoint) -> Result<(), RuntimeError>;
}

pub trait PolicyEnforcer {
    async fn check(&self, proposed: ProposedToolCall) -> PolicyDecision;
    async fn wait_for_approval(&self, request: ApprovalRequest) -> ApprovalDecision;
}
```

AgentRS 不直接操作 SQLite、Electron 或 Keychain；Core 实现这些 port。这样在未来可以原样将 AgentRS 放入受控远程 Runner，但首发仍以进程内调用为最快、最可靠的路径。

### 7.2 工具执行与环境一致性

Shell 是桌面 Agent 的高风险与高价值能力。每次新会话在用户确认后捕获 Shell 快照：PATH、环境变量、alias、function、当前解释器/conda 信息和工作目录。快照必须脱敏，排除 token、cookie、私钥和敏感环境变量；执行时使用解析后的安全环境，而不是 `source ~/.zshrc` 后执行任意文本。

工具 manifest 必须包含：

```text
tool_id、version、input/output schema、risk_class、required_scopes、
idempotency、retry_policy、concurrency、sandbox_profile、timeout、
network_policy、artifact_policy、human_summary
```

Shell、浏览器、文档转换和第三方 MCP 的实际运行统一下沉到 SandboxRS；AgentRS 不能自行 spawn 子进程。P0 不承诺 VM 级隔离，但 SandboxRS 必须提供路径隔离、环境净化、命令审查、进程树收割、文件变更暂存/撤销和默认无额外网络权限；企业版再引入容器或微 VM。

### 7.3 SandboxRS：独立的安全执行模块

`sandboxrs` 与 `agentrs` 并列，不是 `agentrs-sandbox` 子 crate。它是智能体命令的安全执行环境，首要目标是避免错误命令破坏用户本地数据。两者的演进目标、可信边界和测试方法不同：AgentRS 的问题是“如何合理规划与调度”；SandboxRS 的问题是“即使规划或工具存在缺陷，如何将命令的文件副作用限制为可预览、可提交、可撤销的变更”。

建议 workspace 结构：

```text
sandboxrs/
  sandboxrs-types/         SandboxGrant、ExecutionRequest/Result、审计证明
  sandboxrs-policy/        grant 结构校验、资源/网络/命令约束的本地执行检查
  sandboxrs-fs/            授权根目录、realpath/symlink containment、copy-on-write 变更集
  sandboxrs-changes/       diff、提交、撤销、回收区、undo journal、崩溃恢复
  sandboxrs-process/       环境净化、进程组/Job Object、取消、超时、资源限制
  sandboxrs-command/       命令风险解析、受控文件操作替代、破坏性命令拦截
  sandboxrs-backends/      staged-native、container、microvm、remote executor 适配器
  sandboxrs-runtime/       lifecycle、事件、清理、recover/reconcile
  sandboxrs-testkit/       恶意路径/命令、假时钟、假网络和后端一致性测试
```

SandboxRS 对 AgentRS 暴露窄接口：

```rust
pub struct SandboxGrant {
    pub grant_id: GrantId,
    pub run_id: RunId,
    pub step_id: StepId,
    pub expires_at: Timestamp,
    pub filesystem: FilesystemGrant,
    pub network: NetworkGrant,
    pub process: ProcessLimits,
    pub secrets: SecretGrant,
    pub allowed_tool: ToolIdentity,
    pub input_hash: Sha256,
}

pub trait SandboxExecutor {
    async fn execute(
        &self,
        grant: SandboxGrant,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, SandboxError>;
    async fn cancel(&self, execution_id: ExecutionId) -> Result<(), SandboxError>;
    async fn reconcile(&self, execution_id: ExecutionId) -> ExecutionStatus;
}
```

SandboxRS 必须拒绝已过期、Run/Step 不匹配、工具不匹配、输入哈希变化、路径逃逸、未允许网络目标和超过资源上限的请求。`ExecutionResult` 返回退出状态、受限 stdout/stderr、artifact 引用、资源用量、变更集 ID、diff、回收记录和清理证明；大输出经 Artifact 服务保存，不直接进入模型上下文或 IPC 事件。

#### 文件安全不变量

1. **默认不在用户工作区原地执行写入命令。** SandboxRS 为每个 `run_id/step_id` 创建隔离执行根；读取从已授权工作区镜像或受控只读挂载获得，写入进入 copy-on-write overlay/暂存目录。
2. **用户目录只接受显式提交。** Agent 命令完成后先生成 `ChangeSet`：新增、修改、重命名、删除、二进制产物及其 diff/摘要。只有 Core 依据策略和审批提交该 ChangeSet，才原子应用到工作区。
3. **删除永不直接不可恢复执行。** 对文件删除，提交阶段使用应用管理的回收区移动语义，并写入 undo journal；UI 提供按任务撤销。物理清理只由独立保留策略在到期后执行，且不得由 Agent 命令触发。
4. **覆盖先保留原件。** 修改/覆盖在提交前保存内容寻址备份或原子 rename 备份；失败或中断可恢复，不能留下半写入文件。
5. **不可信 Shell 不能绕过变更层。** 对 `rm`、`find -delete`、重定向覆盖、`git clean/reset`、递归权限修改等命令，SandboxRS 必须拒绝、改写为受控文件操作，或只在隔离副本运行并要求展示 ChangeSet 后再次确认。P0 不提供“任意命令直接写宿主工作区”模式。

文件变更生命周期：

```text
Agent 提议命令
  -> SandboxRS 在隔离执行根运行
  -> 收集文件系统变更并生成 ChangeSet + diff
  -> AgentUI 预览，Core Policy 判断是否需要审批
  -> 原子 commit 到用户工作区 或 discard
  -> 删除项移入 uworker 回收区，保留 undo journal
  -> 用户撤销 / 到期后的独立清理任务
```

这保证“命令执行成功”不等同于“已修改用户文件”。对无法可靠捕获副作用的工具（例如直接操作外部数据库、发送消息），SandboxRS 标记为不可回滚 effect；它们不适用文件暂存模型，必须采用单独的高风险审批和幂等/reconcile 机制。

执行后端按强度渐进，接口保持不变：

| 后端 | 阶段 | 适用范围 | 最低约束 |
|---|---|---|---|
| `staged-native` | P0 | 受信本机工作区内的文件/Shell | copy-on-write 变更集、diff/commit/undo、containment、净化 env、进程树、超时 |
| `container` | P1/P2 | 文档转换、脚本、第三方依赖 | 最小挂载、非 root、只读镜像、CPU/内存/磁盘/网络限制，产物仅导出为 ChangeSet |
| `microvm` | 企业版 | 高风险代码、未知插件 | 强隔离、临时磁盘、受控网络、销毁证明 |
| `remote` | P2/企业版 | 企业私网或远程设备执行 | mTLS、设备身份、短期 grant、远端审计回传 |

SandboxRS 不是最终策略裁决点：Core 的 Policy 负责“是否允许”，SandboxRS 负责“只执行被允许的那一次动作”。两层都检查 grant，形成防御纵深。

### 7.4 上下文、记忆与 Awareness

不采用“全部 MEMORY.md 永久塞进 prompt”的做法。项目记忆设计为本地、可审查的 Awareness 子系统：

```text
projects/<project-id>/awareness/
  MEMORY.md                 人工可读、稳定规则和用户偏好
  USER.md                   用户显式偏好
  daily/YYYY-MM-DD.md       当日工作日志
  .index.sqlite             文件/chunk/FTS5 索引，可删除重建
  memory_meta.json          importance: critical / normal / low，来源与更新时间
  hash-state.json           文件增量检测状态
  memory_evicted.log        被蒸馏/驱逐内容的可追溯记录
```

检索流程：工作区变更由 hash/mtime 触发增量切分；会话开始和关键步骤根据当前任务检索 FTS5（中文使用 trigram）候选；按重要性、BM25、时间衰减和权限过滤；在明确 token 预算内注入片段与来源。FTS 失败时回退 LIKE 搜索，索引损坏可以从 Markdown 和日志重建。

反思蒸馏默认关闭或仅在用户启用后运行。它必须先创建备份、进行并发变更检查、验证内容保留率、记录驱逐内容，并在 UI 显示变更 diff。任何自动生成的记忆都可查看来源、编辑、删除和禁止再次写入。这样保留 QoderWork 的记忆质量设计，同时避免黑盒自动修改用户资料。

### 7.5 Skills 产品化

每个技能采用目录包而非孤立 `SKILL.md`：

```text
skills/<skill-id>/
  SKILL.md
  skill-manifest.yaml
  templates/
  scripts/
  tests/
  signature.json            第三方/商店技能必须存在
```

`skill-manifest.yaml` 至少定义名称、版本、描述、图标、参数 schema、示例任务、支持语言、所需工具/依赖/权限、适用模型、输入输出 artifact 类型。UI 根据 schema 渲染表单，例如“Markdown 转 Word”“用表格填充合同”“批量重命名图片”，不要求普通用户写 Prompt。

技能来源为内置、用户自建和扩展商店三层。第三方技能安装前显示发布者、签名、版本、依赖、文件/网络/Shell 权限；更新需要版本锁、变更说明和可回滚机制。

### 7.6 子 Agent：两种模式、最小权限与通信边界

uworker 明确区分两种 SubAgent，不能用一套“多 Agent 通信”同时解决：

| 模式 | 适用场景 | 生命周期与通信 | 首发阶段 |
|---|---|---|---|
| 函数式 `asTool` | 搜索、规划、记忆选择、压缩、风险评估、单一专项分析 | 输入 -> 执行 -> 结构化结果；调用者不接收过程推理 | P1 |
| 协作式 `Team Worker` | 并行文档处理、研究、跨步骤项目任务 | 持久 Worker、TaskList、邮箱/通知、artifact 共享 | P2 |

函数式 Agent 的输出必须是有 schema 的摘要，而不是完整 chain-of-thought。主 Agent 可见的只有任务目标、结论、证据引用、产物、风险和下一步建议。子 Agent 的草稿、推理文本、工具细节和 token 流保存在其独立 Run 中，默认不回灌主上下文。这样避免上下文膨胀、注意力稀释和子 Agent 推理风格污染主 Agent；用户在诊断视图中可按权限查看执行摘要与审计记录。

每类 Agent 使用 `AgentProfile` 描述模型档位、工具、通信与预算：

| Profile | 模型档位 | 工具 | 通信 | 典型角色 |
|---|---|---|---|---|
| `selector` | lite | 无 | 无 | memorySelector、工具/技能推荐 |
| `guard` | lite | 无 | 无 | promptHookEvaluator |
| `planner` | default | 只读检索 | 只向父 Run 返回结果 | Plan、Explore |
| `worker` | default/craft | 按任务最小授权 | P2 可投递结构化消息 | 文档/数据处理 Worker |
| `primary` | craft/default | 当前 Run 的显式授权集 | 仅向用户、Core 和已授权 Team 通信 | 主 Agent |

工具、通信、模型预算均从父 Run 继承交集，子 Agent 永远不能扩权。`compact`、`contextSummary`、`memorySelector` 和风险评估 Agent 固定为零工具；这不是优化项，而是权限模型。

### 7.7 Team Worker 的黑板协调模型

P2 多 Agent 使用共享 `TaskList` 黑板和依赖图协调，而不是让 Worker 任意点对点互调。黑板是 Core 中持久化的领域对象，记录任务状态、前置依赖、owner、租约、产物、预算和阻塞原因。

```text
Team Lead 创建 TaskList DAG
  -> Worker 领取已满足依赖的 Task（lease + capability subset）
  -> Worker 写入 status、artifact ref、结构化 summary
  -> Core 解锁依赖任务并向 Lead 发送 AgentNotification
  -> Lead 只接收摘要、状态和产物引用，决定后续拆分/汇总/取消
```

四条正交通道必须显式建模：

1. `TaskList`：任务、依赖、领取和状态，是协调真相源。
2. `ArtifactRef`：文件、表格、报告、工具产物等大对象共享，不把正文塞进消息。
3. `AgentNotification`：向 Lead/父 Run 上报的结构化摘要、风险和完成状态。
4. `DirectMessage`：仅 P2 已授权 Worker 的受控短消息，用于澄清；不作为任务分发和权威状态来源。

每条消息和任务更新带 `team_id/run_id/task_id/seq`，有审计与大小限制。Worker 失联由 lease 到期回收，已产生外部副作用的任务进入 reconcile，不能直接重新领取。

### 7.8 上下文选择、延迟加载与结构化压缩

主 Agent 的上下文不是“所有历史 + 所有工具 + 所有子 Agent 输出”的拼接。采用三道前置过滤和两类压缩：

```text
用户输入
  -> memorySelector：从记忆索引选择少量候选（默认最多 5 条）
  -> ToolSearch/SkillSearch：仅加载相关工具或技能摘要
  -> DeferExecute：用户确认或 Agent 确认需要时才加载完整 schema/body
  -> Primary Agent
  -> compact：接近 token 阈值时保留近期对话并压缩早期执行细节
  -> contextSummary：恢复/跨 Run 延续时生成完整结构化工作摘要
```

`memorySelector` 是 lite、零工具分类 Agent，只读取记忆文件的标题、描述、重要性和可见范围，返回 JSON 文件 ID 列表；不确定时宁可少选。它只能辅助 FTS/BM25/规则排序，检索失败必须可退回确定性搜索。

`ToolSearch` 和 `SkillSearch` 首先提供名称、描述、风险和权限摘要；完整 JSON Schema、脚本和参考正文只在需要执行/加载时拉入上下文。这样让能力扩展不线性占用 token。

两类压缩的输出必须可审查，至少包含：用户原始意图、关键约束、已授权范围、关键文件/产物、已执行与待执行步骤、错误及修复、未决审批、当前工作状态和下一步。`contextSummary` 额外保留全部用户消息的可检索索引，而不是把其全文永久塞回主上下文。压缩 Agent 固定零工具，并记录压缩前后 token、摘要版本和来源 event range。

## 8. 云端商业服务：可选而非执行依赖

云端采用最小数据原则，按功能拆分，不将本地 Agent 变成云端控制的薄客户端：

| 云端能力 | 是否首发 | 上传内容 | 禁止上传 |
|---|---|---|---|
| 账号、授权、订阅 | 是 | 账号、设备公钥/设备标识、entitlement | 本地文件、会话正文 |
| 模型网关 | 可选 | 用户明确发送给模型的 prompt/附件 | 工作区全量索引、无关文件 |
| 自动更新 | 是 | 版本、平台、更新结果（需同意遥测） | 目录、文档、命令 |
| 崩溃/诊断 | 可选 | 脱敏 stack、版本、匿名性能指标 | Prompt、文件路径、密钥 |
| 云同步 | P2 | 用户选定并端到端加密的会话/技能/设置 | 默认全部本地数据 |
| 远程查看/控制 | P1/P2 | 加密事件中继、设备配对信息 | 开放本地 HTTP 端口、长期远控 token |
| 团队服务 | P2 | 共享技能、审批、团队任务元数据 | 未授权的个人工作区内容 |

如果提供远程控制，必须使用设备配对、短期会话密钥、显式在线指示、每次控制授权、活动审计和随时断开。禁止静默后台远控。

## 9. 商业级安全、隐私、质量与运营

### 9.1 安全基线

1. 安装包、Core、插件和更新均代码签名；启动时验证 Core 二进制和 manifest 哈希。
2. API Key、OAuth refresh token、数据库加密密钥只进入系统 Keychain/DPAPI；不写日志、不入 UI store、不传 Renderer。
3. SQLite 会话和同步包使用 per-device 数据密钥加密，密钥轮换可用；用户可一键删除本地数据。
4. 工作区授权由原生文件选择器发起，路径 containment 与 symlink realpath 校验在 Core 执行。
5. 默认拒绝工作区外访问、网络上传、危险命令和第三方插件高风险能力。
6. 保留 append-only 本地审计记录：谁在何时以何种策略批准或拒绝了什么动作，关联 `run_id/step_id/input_hash`。

### 9.2 观测与诊断

本地保留结构化日志和指标，字段包括 `request_id/run_id/step_id/tool_id`，严禁记录 prompt 正文、文件内容、绝对路径、命令输出、Cookie 与 token。首批指标：启动成功率、Core 崩溃率、首 token 时延、Run 成功/中断/恢复率、审批率、工具耗时、索引耗时、模型错误、更新成功率和资源使用。

产品需提供“诊断包预览”页面：用户在上传前能看到将发送的内容。线上仅聚合匿名或经同意的数据；采用远端 feature flag 时要有本地缓存与离线回退。

### 9.3 测试门禁

| 层级 | 必测项 |
|---|---|
| AgentRS | fake provider/tool、上下文压缩、checkpoint 恢复、工具并发、取消、事件序列 |
| SandboxRS | 路径/symlink 逃逸、grant 绑定与过期、ChangeSet diff/commit/discard、回收区/undo、环境脱敏、进程树回收、资源上限、后端一致性 |
| AgentCore | SQLite 迁移、JSONL 损坏恢复、路径逃逸、审批 input hash、策略回归、密钥不落盘 |
| AgentUI | preload 白名单、IPC 参数校验、审批/diff 交互、断线重放、升级和离线状态 |
| E2E | 首次启动、工作区授权、文件修改、Shell 审批、崩溃恢复、技能安装、自动更新回滚 |
| 安全 | SAST、依赖/许可证扫描、秘密扫描、代码签名验证、插件篡改、越权路径测试 |

发布门槛：关键 E2E 与迁移回归通过，升级/降级矩阵通过，任何高风险工具没有绕过审批或 SandboxGrant 的路径，且离线启动、Core 异常、SandboxRS 回收与索引损坏都可被明确处理。

## 10. 分阶段执行计划

### Phase 0：桌面基础与安全骨架（2-3 周）

- 建立 monorepo：`agentui/`、`agentcore/`、`agentrs/`、`sandboxrs/`、`contracts/`、`installer/`、`docs/adr/`。
- 选定 Electron + React + TypeScript、Rust Core/AgentRS/SandboxRS、SQLite + SQLCipher 或应用层加密、系统 Keychain、签名更新框架。
- 实现主进程拉起 bundled Core、随机本地端点、bootstrap token、健康检查、版本握手、退出回收和崩溃重启。
- 冻结 IPC/API/事件 schema、错误码、数据目录、隐私分类和日志脱敏规范；定义 `SandboxGrant`、`ExecutionRequest/Result`、`ChangeSet`、undo journal 与 staged-native backend 的安全不变量。

验收：全新安装可离线启动，Renderer 无 Node 权限，Core 不暴露远程端口，UI 能展示 mock Run 流和 Core 版本状态。

### Phase 1：P0 单 Agent 本地闭环（5-7 周）

- Core：工作区授权、SQLite 会话/Run/Step、JSONL 事件、基础策略、审批、artifact 管理、事件重放。
- AgentRS：单模型 Provider、本地模型与 API Key 配置、基础 Agent loop、Read/Write/Glob/Grep/Exec 工具调度、取消和 checkpoint。
- SandboxRS：staged-native backend、隔离执行根、ChangeSet/diff/commit/discard、回收区与 undo journal、工作区 containment、环境净化、进程树收割、超时/资源上限和 ExecutionResult 审计。
- UI：工作台、任务会话、计划/工具卡片、文件 diff、审批、停止/重试、设置和本地数据清理。
- 安全：SandboxGrant 绑定审批、禁止命令直接删除用户文件、路径 containment、Shell 环境净化、进程树收割、Keychain 密钥、日志脱敏。

验收：用户能在被授权目录内完成真实文件任务；修改、重命名和删除均先以 ChangeSet 预览，删除进入可恢复回收区；Shell 不能直接破坏宿主工作区；强杀 Core 后重启可恢复或提示用户处理，绝不静默重复写入或删除。

### Phase 2：生产力与记忆产品化（4-6 周）

- Office/PDF/XLSX/PPTX 技能和 artifact 预览；参数化示例任务与技能创建向导。
- Awareness：Markdown、哈希增量、SQLite FTS5/trigram、JIT 注入、重要性管理、索引重建和可视化来源。
- Shell snapshot、浏览器 Agent（独立 profile/权限）、MCP 连接器、网络审批；SandboxRS container backend 用于文档转换和第三方依赖。
- 安装更新、崩溃诊断、匿名遥测、账号与订阅 entitlement。

验收：技能不依赖用户写复杂 Prompt；中文工作区检索有效；记忆可追溯和一键清除；更新失败可回滚，隐私设置生效。

### Phase 3：可选云与远程体验（4-6 周）

- 登录、许可证、云模型网关、设备管理与离线宽限。
- 加密的设置/技能同步；远程只读查看和明确授权的远程控制。
- 定时任务与通知；模型/工具用量统计和预算提示。
- 云端 API 保持为可选服务，不改变本地 Run 的权威性和离线能力。

验收：无云端时 P0/P1 不退化；远程访问经过设备配对且可审计；用户可选择哪些数据同步。

### Phase 4：团队与企业版本（持续）

- 团队技能仓库、共享审批、共享任务板、受控多 Agent DAG。
- SSO/SCIM、企业策略、私有模型网关、私有部署/设备管理、审计导出。
- SandboxRS micro VM/remote backend、私有网络连接器、插件签名市场、评测和灰度发布。

验收：企业管理员可定义工具/数据出站策略，个人本地数据与团队数据隔离，团队能力不绕过桌面端审批与审计。

## 11. 首批 ADR

1. ADR-001：桌面优先，`agentui -> agentcore -> {agentrs, sandboxrs}` 随安装包分发；云端为可选能力。
2. ADR-002：Electron Renderer 无 Node 权限，跨进程能力仅经 contextBridge 白名单。
3. ADR-003：Core 仅监听 loopback/本地 socket，启动使用一次性 token，禁止默认 LAN 暴露。
4. ADR-004：SQLite + JSONL + 加密 checkpoint 为本地会话恢复模型；Run/Step 是权威状态。
5. ADR-005：高风险工具执行前必须经过 Core Policy 和绑定输入哈希的审批。
6. ADR-006：工作区外路径默认拒绝，授权由原生文件选择器发起，Core 负责 realpath containment。
7. ADR-007：项目记忆是本地可审查 Awareness；自动反思默认关闭，所有变更可回滚。
8. ADR-008：技能采用带 manifest、参数 schema、权限与签名的目录包，不只是一份 Prompt。
9. ADR-009：更新、插件、Core 二进制必须签名并具备版本兼容矩阵与回滚策略。
10. ADR-010：云端不得默认上传本地文件、会话正文或工作区索引；遥测与同步须显式授权。
11. ADR-011：SandboxRS 是与 AgentRS 并列的独立模块；AgentRS 不可直接创建工具进程或访问未授权 OS 资源。
12. ADR-012：Core 签发一次性、绑定 `run_id/step_id/tool/input_hash` 的 SandboxGrant；SandboxRS 执行前强制复核并回传受证明结果。
13. ADR-013：SandboxRS 默认在隔离执行根产生 ChangeSet；用户工作区只能经显式 commit 改动，删除一律进入可撤销回收区，Agent 不可触发物理删除。

## 12. 实施顺序与判断标准

正确的实施顺序是：先做“安全可信的本地任务闭环”，再做“记忆和技能带来的生产力”，最后才做“云端、团队和多 Agent 的规模化”。

uworker 的核心竞争力不是简单把聊天窗口装进桌面，而是把用户电脑上的真实工作变成可执行、可预览、可审批、可恢复和可复用的任务。Aion 的分层结构提供了坚实起点；通过将 AgentRS 的推理能力与 SandboxRS 的执行隔离并列设计，再结合 QoderWork 的检索化记忆、参数化技能、会话冗余与 Shell 一致性，uworker 才具备商业桌面产品所需的安全与可演进性。
