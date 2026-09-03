# 审批与权限控制：横向技术设计对比

对比对象：`agentrs` / `claude-code-main` / `pi` / `opencode` / `deepseek-harness`
日期：2026-09-03
方法：通读五套实现的权限判定与审批链路源码，非文档推测

---

## 0. 结论摘要

| 维度 | agentrs | claude-code | pi | opencode | deepseek-harness |
|---|---|---|---|---|---|
| 是否有权限体系 | ✅ | ✅ 最完整 | ❌ **无** | ✅ | ✅ |
| 决策三态 | ask/allow（**无 deny**） | allow/ask/deny/passthrough | — | allow/ask/deny | 四态 outcome |
| 规则粒度 | **工具类别** | 工具 + 参数模式 | — | (action, resource) 通配符 | answerer 自定 |
| 规则冲突 | 无规则概念 | 多源分层 + deny 优先 | — | `findLast` + deny>ask>allow | waterfall 先 claim 先赢 |
| 参数改写放行 | ❌ | ✅ `updatedInput` | — | ❌ | ❌ |
| 持久化 | ⚠️ 进程内存 | ✅ 五级 destination | — | ✅ project saved rules | ✅ 会话事件 |
| 审计 | ❌ | ⚠️ analytics | — | ✅ Asked/Replied 事件 | ✅ asked/decided 成对 |
| 答复者缺失 | 挂起等待 | 降级为 ask | — | 降级为 ask | ✅ **fail-closed** |
| 拒绝的循环语义 | error result 续跑 | 续跑 | — | **中断整个循环** | 由 outcome 决定 |

一句话：**agentrs 的审批链路结构清晰、通道设计正确，但权限模型停留在"类别级开关"，没有规则、没有 deny、没有持久化、没有审计；且存在三处实现缺陷（scope 丢弃、pending 泄漏、取消不生效）。** claude-code 是五者中唯一把权限做成完整子系统的；pi 干脆没有权限，只有一个可 block 的扩展钩子。

---

## 1. agentrs 现状

### 1.1 四道闸及其顺序

工具调用从模型出来到真正执行，要过四道闸。注意**顺序**，这是后面若干问题的根源：

```
模型返回 tool_use
   │
   ├─① 畸形检查        tool_call.rs:103   空函数名 / 空 call_id
   │     └─ 不通过 → 合成 error result，不发审批
   │
   ├─② 可用性 + 能力    engine.rs:755-760
   │     ToolPolicy::allows(name)                    // 运行时工具授权
   │     requires_image_input && !supports_images    // 模型能力
   │     └─ 不通过 → "Tool 'X' is not available in this runtime"，不发审批
   │
   ├─③ 审批            orchestration.rs:374-413（宿主）/ confirm.rs:63（终端）
   │     └─ 拒绝 → "Tool denied: {reason}" error result
   │
   └─④ PreToolUse hook  orchestration.rs:220-232
         └─ 阻断 → "Blocked by hook: {e}" error result
```

**②是静态可用性，不是权限。** `ToolPolicy`（`tool_policy.rs`）只有两态：

```rust
pub enum ToolPolicy {
    Unrestricted,
    AllowOnly(BTreeSet<String>),
}
```

它同时作用于两处：**工具广播**（`tool_definitions_for_turn`，`engine.rs:706`——模型压根看不到被禁工具）和**执行前拦截**（防模型凭记忆调用已下线工具）。子代理继承父策略且**只能收窄不能放宽**（`spawner.rs:192-203`）：

```rust
ToolPolicy::allow_only(
    allowed_tools.iter().filter(|n| parent.allows(n)).cloned()
)
```

这一层设计是对的，问题在③。

### 1.2 审批的两条路径

**宿主路径**（`orchestration.rs:374-413`）：

```
needs_approval = !auto_approve
              && !allow_list.contains(name)
              && !approval_manager.is_auto_approved(category)

若需要审批：
  Agent → ToolRequest { msg_id, call_id, tool: { name, category, args, description } }
          request_approval(call_id, category)   // 建 oneshot，存进 pending map
          rx.await                              // ← 阻塞点
  Client → ToolApprove { call_id, scope: once|always }
         / ToolDeny    { call_id, reason }
  Agent → ToolRunning → ToolResult   /   ToolCancelled { reason }
```

`is_auto_approved`（`approval.rs:75-96`）两级：

```
SessionMode::Yolo      → 全放行
SessionMode::AutoEdit  → category ∈ {"info", "edit"}
SessionMode::Default   → 否
      ↓ 都不命中
per-category "always" 集合（HashSet<String>）
```

类别只有五个（`events.rs:98`）：`info` / `edit` / `exec` / `mcp` / `network`。

**终端路径**（`confirm.rs:63-88`）：同步阻塞 stdin，`y`/`n`/`a`/`q`。`a` 把**工具名**加入白名单。

白名单额外支持域名粒度规则 `WebFetch:domain:example.com`（`confirm.rs:42-60`）：

```rust
// host 从结构化 input 的 url 字段解析，不是从渲染后的 JSON 做子串匹配：
// 否则 prompt 里提到某域名就能解锁对别处的抓取。
let Some(host) = target_host(input) else { return false };
rule.strip_prefix(&prefix).is_some_and(|domain| {
    host == domain || host.ends_with(&format!(".{domain}"))
})
```

这段的安全考量是对的，也是 agentrs 权限代码里**唯一的参数级规则**。但它硬编码只认 `input["url"]`（`confirm.rs:92-98`），且只在终端路径生效——宿主路径根本不查 `allow_list` 的域名规则（`orchestration.rs:375` 只做 `allow_list.contains(&name.to_string())` 的裸工具名比较）。

### 1.3 协议面

```rust
// Client → Agent（commands.rs）
ToolApprove { call_id: String, scope: ApprovalScope }   // once | always
ToolDeny    { call_id: String, reason: String }
SetMode     { mode: SessionMode }                       // default | auto_edit | yolo

// Agent → Client（events.rs）
ToolRequest   { msg_id, call_id, tool: ToolInfo }
ToolRunning   { msg_id, call_id, tool_name }
ToolResult    { msg_id, call_id, tool_name, status, output, output_type, metadata }
ToolCancelled { msg_id, call_id, reason }
```

通道是 per-call_id 的 `tokio::sync::oneshot`，存在 `pending: Mutex<HashMap<String, PendingApproval>>`。客户端断连导致 sender 被 drop → `rx.await` 返回 `Err` → `ExecutionControl::Quit` → `AgentError::UserAborted`。

---

## 2. agentrs 的实现缺陷（可验证）

### 2.1 P0 — `scope: "always"` 在 JSON stream 路径被静默丢弃

只有 `ToolApprovalManager::approve(call_id, scope)` 会在 `scope == Always` 时调 `add_auto_approve(category)`（`approval.rs:58-67`）。而 JSON stream 的两个入口都绕过了它：

```rust
// dispatch.rs:29（空闲期命令）
ProtocolCommand::ToolApprove { call_id, scope: _ } => {
    ctx.approval_manager.resolve(&call_id, ToolApprovalResult::Approved);
}

// message.rs:71（run 进行中命令）
ProtocolCommand::ToolApprove { call_id, scope: _ } => {
    ctx.approval_manager.resolve(&call_id, ToolApprovalResult::Approved);
}
```

`approve()` 的**唯一调用点是 TUI**（`app.rs:553-554`）。结论：AgentrsUI 一类 JSON-stream 宿主发 `scope:"always"`，行为等同 `once`，且无任何提示。协议声明了字段，宿主实现没接。

修法：两处改成 `ctx.approval_manager.approve(&call_id, scope)`。

### 2.2 P0 — `rx.await` 不观察取消令牌

```rust
// orchestration.rs:391-392
let rx = approval_manager.request_approval(id, &category);
match rx.await {           // ← 裸 await，没有 select! 配 cancel.cancelled()
```

`turn_cancel` 已经传进 `execute_tool_calls_with_approval_and_output_limit`（参数 `cancel: &CancellationToken`），但**只往下传给 `execute_single`，审批等待本身不监听它**。因此 `cancel_running_tools()`（`engine.rs:1606`）无法解除一个挂起的审批。

JSON-stream 宿主目前侥幸无事：`Stop` 命令是 `break` 出 `select!`（`message.rs:77-80`）丢弃整个 `run()` future，`rx` 随之析构。但这意味着**取消依赖"丢弃整个 future"而不是"取消令牌"**，与 `turn_cancel` 的设计意图（长耗时操作能就地退出、turn 可继续收尾）直接矛盾。任何"取消当前工具但保留 run"的调用方会挂死。

修法：

```rust
let rx = approval_manager.request_approval(id, &category);
let decision = tokio::select! {
    r = rx => r,
    _ = cancel.cancelled() => {
        approval_manager.drop_pending(id);
        Ok(ToolApprovalResult::Denied { reason: "cancelled".into() })
    }
};
```

### 2.3 P1 — `drop_pending` 是死代码，pending map 只增不减

全仓库除测试外零调用点。审批被拒、turn 结束、run 被丢弃时都不清理，`(call_id, tx)` 永久驻留。长会话反复中断会持续累积。修 2.2 时顺带在 `Denied` / `Quit` / turn 结束三处调用。

### 2.4 P1 — hook 在审批之后才跑，无法参与决策

顺序是 ③审批 → ④hook。所以一个本该被 hook 自动拒绝的调用，**先弹窗打扰用户，用户批准后才被 hook 挡掉**；反过来 hook 也无法代替用户自动批准。

claude-code 是反的：hook 的裁决作为 `PermissionDecisionReason` 的一个变体**喂进权限判定**：

```ts
| { type: 'hook'; hookName: string; hookSource?: string; reason?: string }
```

即 hook 先出裁决，权限系统据此决定 allow/ask/deny，之后才可能弹窗。

修法：把 `run_pre_tool_use` 提到审批之前，并让它能返回三态（allow / ask / deny）而不只是 `Result<(), E>`。

---

## 3. agentrs 的模型级问题：`always` 的粒度是"类别"

这不是 bug，是设计问题，也是与另外三家差距最大的地方。

```rust
// approval.rs:62-64
if matches!(scope, ApprovalScope::Always) {
    self.add_auto_approve(&pending.category);   // ← category，不是 tool，更不是参数
}
```

用户批准一次 `ExecCommand("ls")` 并选 always → `add_auto_approve("exec")` → **整个 exec 类别对本会话全开，包括 `rm -rf /`**。同理批准一次 `WebFetch("https://docs.rs/...")` 选 always → 整个 `network` 类别全开，任意 URL 可抓。

终端路径稍好（`a` 只加工具名，不加类别），但两条路径语义不一致本身也是问题：同一个"总是允许"在 TUI 里是工具级，在宿主里是类别级。

另外三家都不这么做：

- **claude-code**：规则是 `toolName(ruleContent)`，如 `Bash(git log:*)`、`Read(src/**)`。`ruleContent` 支持 `:*` 前缀语法与 `*` 通配（`shellRuleMatching.ts:40-139`），带转义（`escapeRuleContent` 处理内容里的括号）。
- **opencode**：规则是 `(action, resource)` 通配符对，`Wildcard.match` 双向匹配（`permission.ts:76-86`）。
- **deepseek**：由 answerer 自己决定粒度，服务层只保证"grants apply only to the requested action"（模块 doc 明写）。

---

## 4. claude-code：五者中唯一的完整权限子系统

### 4.1 五种模式 × 三种行为 × 六个规则来源

```ts
// 模式（types/permissions.ts:16-35）
EXTERNAL: 'default' | 'acceptEdits' | 'bypassPermissions' | 'dontAsk' | 'plan'
INTERNAL: + 'auto'（TRANSCRIPT_CLASSIFIER 开启时）| 'bubble'

// 行为
type PermissionBehavior = 'allow' | 'deny' | 'ask'

// 规则来源（优先级分层）
type PermissionRuleSource =
  | 'userSettings' | 'projectSettings' | 'localSettings'
  | 'flagSettings' | 'policySettings' | 'cliArg' | 'command' | 'session'
```

`policySettings` 是企业策略层，用户改不了；`session` 是本次会话临时授予。

### 4.2 决策结果远比"批/不批"丰富

```ts
type PermissionDecision =
  | { behavior: 'allow';  updatedInput?, userModified?, decisionReason?, acceptFeedback?, contentBlocks? }
  | { behavior: 'ask';    message, updatedInput?, suggestions?, blockedPath?,
                          pendingClassifierCheck?, contentBlocks? }
  | { behavior: 'deny';   message, decisionReason }
type PermissionResult = PermissionDecision | { behavior: 'passthrough', ... }
```

三个 agentrs 完全没有的能力：

1. **`updatedInput`**：批准的同时**改写参数**。用户可以把 `rm -rf build` 改成 `rm -rf ./build` 再放行，`userModified: true` 标记这次改动。
2. **`suggestions: PermissionUpdate[]`**：弹窗时附带"建议保存的规则"，用户点一下就把 `Bash(git log:*)` 写进 `projectSettings`。规则粒度由**发起方**建议，不是由类别硬定。
3. **`pendingClassifierCheck`**：弹窗的同时**异步跑分类器**，分类器可能在用户回答之前自动批准。这是"降低打扰"的工程手段。

`decisionReason` 是个联合类型，记录**为什么**这么判：`rule` / `mode` / `subcommandResults`（复合 shell 命令逐子命令判定后聚合）/ `permissionPromptTool`（外部审批工具）/ `hook` / `asyncAgent` / `sandboxOverride`。这让 UI 能解释"因为 projectSettings 里的 `Bash(npm:*)` 规则，所以自动放行"。

### 4.3 持久化写回

```ts
type PermissionUpdate =
  | { type: 'addRules' | 'replaceRules' | 'removeRules'
      destination: 'userSettings'|'projectSettings'|'localSettings'|'session'|'cliArg'
      rules: PermissionRuleValue[], behavior: PermissionBehavior }
  | { type: 'setMode'; destination; mode }
  | { type: 'addDirectories' | 'removeDirectories'; destination; directories: string[] }
```

注意 `addDirectories`——**工作目录也是一种权限资源**，可以授予对某目录树的访问。agentrs 没有任何路径级授权概念。

---

## 5. opencode：规则即数据，拒绝即中断

### 5.1 求值

```ts
// permission.ts:76-86
export function evaluate(action, resource, ...rulesets): Rule {
  return rulesets.flat()
    .findLast(rule => Wildcard.match(action, rule.action)
                   && Wildcard.match(resource, rule.resource))
    ?? { action, resource: "*", effect: "ask" }   // 默认 ask
}
```

`findLast` = **后到先得**，靠 ruleset 拼接顺序表达优先级。合并规则时对多个 resource 求值后取最严：

```ts
// permission.ts:157-161
const effects = input.resources.map(r => evaluate(input.action, r, all).effect)
const effect = effects.includes("deny") ? "deny"
             : effects.includes("ask")  ? "ask"
             : "allow"
```

两级来源：agent 自带的 `agent.permissions`（配置态，deny 在这一层就短路）+ `savedRules()`（用户 always 攒下来的，落 project 存储）。

### 5.2 三个 API 的分工

```ts
ask(input)    → { id, effect }        // 只求值 + 建请求，不阻塞
assert(input) → Effect<void, Error>   // 求值 + 阻塞等待 + 抛错
reply(input)  → Effect<void>          // 应答
```

`assert` 的 deny 分支抛 `BlockedError({ rules: relevant(...) })`——**把命中的规则原样带回**，调用方能解释拒绝原因。

### 5.3 拒绝即中断整个循环

```ts
// permission.ts:207-209
return yield* restore(Deferred.await(item.deferred)).pipe(
  EffectRuntime.catchTag("PermissionV2.DeclinedError", (error) => EffectRuntime.die(error)),
  ...
)
```

`DeclinedError` 被提升为 defect，runner 在 `isUserDeclined` 里识别后 `Effect.interrupt` 终止整个 session 循环（`runner/llm.ts:144-150, 297-301`），注释明说这是对齐 V1 行为。**agentrs 是反的**：拒绝生成 `is_error` 的 tool_result 继续喂给模型（`orchestration.rs:194-198`）。

两种语义各有道理：opencode 认为"用户说不就是让你停下"，agentrs 认为"让模型知道被拒后自己换路"。但 agentrs **没有提供选择**，而 opencode 还额外区分了：

```ts
// reject 带 message → CorrectedError({ feedback })，不是 DeclinedError
input.message ? new CorrectedError({ feedback: input.message }) : new DeclinedError()
```

即"拒绝并给理由"是纠偏（模型可继续），"纯拒绝"是终止。这个区分很实用。

### 5.4 级联拒绝

```ts
// permission.ts:243-247
if (input.reply === "reject") {
  Deferred.fail(existing.deferred, ...)
  for (const [id, item] of pending) {
    if (item.request.sessionID !== existing.request.sessionID) continue
    publish(Event.Replied, { ..., reply: "reject" })
    Deferred.fail(item.deferred, new DeclinedError())
    pending.delete(id)
  }
}
```

拒一个 → 同 session 所有 pending 全部连带拒绝。避免用户在并发工具批次里被连续弹窗轰炸。**agentrs 因为审批路径是串行的（`orchestration.rs:364` 单层 for），没有多个 pending，也就没这个问题——但代价是失去并发。**

### 5.5 always 的粒度由请求方声明

```ts
Request = { id, sessionID, action, resources, save, metadata, source }
//                                            ^^^^ string[]，选 always 时保存这几条
if (input.reply === "always" && existing.request.save?.length) {
  saved.add({ projectID, action: existing.request.action, resources: existing.request.save })
}
```

工具发起审批时就带上"如果用户选 always，应当保存的规则模式"。这正是 agentrs 该抄的东西。

---

## 6. deepseek-harness：能力接缝 + fail-closed + 全审计

### 6.1 审批是一个 waterfall 事件

```ts
'approval/request'(this: Scoped<ApprovalService>,
                   req: ApprovalRequest,
                   next: () => Promise<ApprovalOutcome>): Promise<ApprovalOutcome>
```

答复者（answerer）组成责任链：返回 outcome 即 claim 该请求，调 `next()` 则委派下一个。**没有任何答复者 → fall through 到 fail-closed 默认值 `'unavailable'`**，调用方必须按拒绝处理。

模块文档写得很直白：

> Missing answerers fail closed; grants apply only to the requested action.

对比：agentrs 无答复者时 `rx.await` **永久挂起**；opencode / claude-code 是降级为 ask（有 UI 就弹）。deepseek 的 fail-closed 在无人值守（ACP、headless、CI）场景下是唯一正确选择。

### 6.2 四态 outcome

```ts
type ApprovalOutcome = 'allowed-once' | 'rejected' | 'cancelled' | 'unavailable'
```

`cancelled`（发起方撤回问题）与 `rejected`（答复者说不）分开；`unavailable`（无人可答）与 `rejected` 分开。agentrs 只有 `Approved` / `Denied{reason}` 两态，靠 reason 字符串区分——不可靠。

注意**没有 `allowed-always`**。文档说明：grants 只作用于所请求的 action，持久化授权由 policy 层管，不由单次审批产生。这是刻意的最小权限设计。

### 6.3 策略是会话事件

```ts
type ApprovalPolicy = 'ask' | 'never'

'approval/policy': {
  policy: ApprovalPolicy
  source?: 'delegation'      // 标记这是委派给子 agent 时种下的 override
}
```

`effectiveApprovalPolicy(events)` 从后往前找**最后一条** `approval/policy` 事件。策略切换是 durable、replayable 的，且明确标注"never in the model transcript"——模型通过 runtime-context 快照和实时切换通知得知策略，而不是从 transcript 里读。

`source: 'delegation'` 让子 agent 的策略来源可追溯。agentrs 的子代理直接 `config.tools.auto_approve = true`（`spawner.rs:72`）——**所有子代理无条件全自动批准，没有任何记录**。这是个安全隐患：主 agent 被限制的操作，换个 Spawn 就绕过了（虽然 `ToolPolicy` 仍然收窄，但审批层完全放开）。

### 6.4 成对审计事件

```ts
'approval/asked':   { id, toolName, callId?, reason? }
'approval/decided': { id, outcome }
```

文档强调：log-only audit，不是 surface event，不带 `surfaceOp`（即不进模型 transcript）；"Exactly one per ask"。加上 `approval/policy`，构成完整的权限审计链。

**agentrs 零审计**：批准/拒绝只体现为 tool_result 的文本内容，会话文件里无法回答"这次 `rm -rf` 是谁批的、什么时候、依据什么规则"。

---

## 7. pi：没有权限系统

`beforeToolCall` 是唯一的拦截点（`agent-loop.ts:619-647`）：

```ts
const beforeResult = await config.beforeToolCall({ assistantMessage, toolCall, args, context }, signal)
if (beforeResult?.block) {
  const result = createErrorToolResult(beforeResult.reason || "Tool execution was blocked")
  if (beforeResult.terminate === true) result.terminate = true
  return { kind: "immediate", result, isError: true }
}
```

`coding-agent` 里它被用来转发给扩展系统（`agent-session.ts:479-497`），扩展抛错即阻断。**没有规则、没有模式、没有用户弹窗、没有持久化、没有审计。** 权限完全外包给宿主。

唯一相关的语义是 `terminate` 标志：阻断时可要求终止整个循环——相当于 opencode `DeclinedError` 的轻量版，且是**可选的**（agentrs 连这个都没有）。

pi 的 `AgentHarness` 骨架里有 `before_tool` / `after_tool` hook 名，但同样没有权限模型。

---

## 8. 能力对照表

### 8.1 判定模型

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| allow / ask 两态 | ✅ | ✅ | ❌ | ✅ | ✅ |
| **deny 规则** | ❌ | ✅ | ❌ | ✅ | ✅ |
| deny 优先级 | — | ✅ | — | ✅ | ✅ |
| passthrough（不表态） | ❌ | ✅ | ❌ | ❌ | ✅ `next()` |
| 规则粒度：类别 | ✅ | — | — | — | — |
| 规则粒度：工具名 | ⚠️ 仅终端 | ✅ | — | ✅ | ✅ |
| 规则粒度：参数模式 | ⚠️ 仅 url 域名 | ✅ `Bash(git log:*)` | ❌ | ✅ resource 通配 | ✅ |
| 路径/目录授权 | ❌ | ✅ addDirectories | ❌ | ✅ resource | ✅ fs policy |
| 复合命令逐段判定 | ❌ | ✅ subcommandResults | ❌ | ❌ | ❌ |
| 决策理由可解释 | ❌ | ✅ 7 种 reason | ❌ | ✅ 带回命中规则 | ✅ reason 字段 |

### 8.2 交互与恢复

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| 批准时改写参数 | ❌ | ✅ `updatedInput` | ❌ | ❌ | ❌ |
| 拒绝时附反馈 | ⚠️ reason 进 result | ✅ contentBlocks | ⚠️ reason | ✅ CorrectedError | ✅ |
| 建议规则 | ❌ | ✅ suggestions | ❌ | ✅ save 字段 | ❌ |
| 异步分类器预批 | ❌ | ✅ | ❌ | ❌ | ❌ |
| 级联拒绝 | ❌ | ❌ | ❌ | ✅ | ❌ |
| 拒绝→中断循环 | ❌ 续跑 | ❌ 续跑 | ⚠️ terminate 可选 | ✅ 默认中断 | ✅ 由 outcome 定 |
| 撤回请求 | ⚠️ drop_pending 死代码 | ✅ | ❌ | ✅ | ✅ `cancelled` |
| 取消令牌解除等待 | ❌ **缺陷** | ✅ | — | ✅ | ✅ |

### 8.3 治理

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| always 持久化 | ⚠️ 进程内存 | ✅ 文件，五级 destination | ❌ | ✅ project 存储 | ✅ policy 事件 |
| 企业策略层 | ❌ | ✅ policySettings | ❌ | ❌ | ⚠️ cordis.yml |
| 审计事件 | ❌ | ⚠️ analytics | ❌ | ✅ Asked/Replied | ✅ asked+decided+policy |
| 策略可 replay | ❌ | ⚠️ | ❌ | ✅ | ✅ |
| 子代理策略继承 | ⚠️ **强制全自动批准** | ✅ | — | ✅ agent.permissions | ✅ delegation 标记 |
| 无答复者行为 | ❌ 挂起 | ask | — | ask | ✅ fail-closed |

---

## 9. agentrs 改进方案

### 9.1 P0 — 三处实现缺陷（1-2 天，纯修复无设计争议）

| # | 位置 | 修法 |
|---|---|---|
| 1 | `dispatch.rs:29`、`message.rs:71` | 改 `resolve(Approved)` 为 `approve(&call_id, scope)` |
| 2 | `orchestration.rs:392` | `rx.await` 换 `tokio::select!` 配 `cancel.cancelled()`，取消时 `drop_pending` 并按 `Denied{reason:"cancelled"}` 处理 |
| 3 | `approval.rs:118` | 在 `Denied` / `Quit` / turn 结束三处调 `drop_pending`，消除 map 泄漏 |

### 9.2 P0 — 规则化权限模型（1-2 周，安全语义修复）

这是最要紧的一项。目标：把 `always` 从类别级抬到规则级。

**协议改动**：

```rust
// events.rs — ToolInfo 增加字段
pub struct ToolInfo {
    pub name: String,
    pub category: ToolCategory,
    pub args: Value,
    pub description: String,
    /// 选择 Always 时应当保存的规则模式，由工具自身声明。
    /// 例：ExecCommand → ["ExecCommand(git log:*)"]
    ///     WebFetch    → ["WebFetch(domain:docs.rs)"]
    pub save: Vec<String>,
}
```

**工具侧**：`Tool` trait 增加 `fn permission_rules(&self, input: &Value) -> Vec<String>`，默认返回 `vec![self.name().to_string()]`（等价于现在的工具级），`ExecCommand` / `WebFetch` / `Read` / `Write` 各自实现参数模式提取。

**管理器侧**：`auto_approved: HashSet<String>` 从"类别集合"改为"规则集合"，求值时按 `(tool_name, rule_content)` 匹配。引入 `deny` 规则集且优先于 allow：

```rust
enum Effect { Allow, Ask, Deny }

fn evaluate(&self, tool: &str, input: &Value) -> Effect {
    if self.deny_rules.matches(tool, input) { return Effect::Deny; }   // deny 最优先
    if self.session_mode.covers(category)   { return Effect::Allow; }
    if self.allow_rules.matches(tool, input){ return Effect::Allow; }
    Effect::Ask
}
```

规则语法建议直接复用 claude-code 的 `ToolName(content)` 形式（`permissionRuleParser.ts:93` 的解析逻辑很干净，含转义处理），前缀语法用 `:*`，通配用 `*`。这样配置可以跨工具迁移，也便于用户理解。

**统一两条路径**：终端 `ToolConfirmer` 与宿主 `ToolApprovalManager` 共用同一套规则求值，消除"TUI 里 always 是工具级、宿主里是类别级"的不一致。现在 `confirm.rs:42` 的域名规则应当被规则引擎吸收，不再硬编码 `input["url"]`。

### 9.3 P1 — hook 参与决策（3-5 天）

把 `run_pre_tool_use` 从"审批之后的阻断器"提到"审批之前的裁决者"：

```rust
enum HookDecision { Allow { reason: String }, Ask, Deny { reason: String } }
```

顺序变为 ①畸形 → ②可用性 → **④hook 裁决** → ③审批（仅当 hook 返回 Ask 且规则判定为 Ask）。这样 hook 既能自动放行（少打扰）也能自动拒绝（不打扰）。

### 9.4 P1 — 审计事件（与主线的事件日志一起做）

补三个会话事件，对齐 deepseek 的成对审计：

```
approval/asked   { id, tool_name, call_id, category, matched_rule?, reason? }
approval/decided { id, outcome: approved_once|approved_always|denied|cancelled|unavailable }
approval/policy  { mode, source? }
```

标记为 log-only（不进模型 transcript）。这三个事件正好是上一份 runtime 报告里 8.8「事件日志旁路」的天然首批候选——权限审计是最需要 durable 记录、又最不依赖消息派生的一类事件。

### 9.5 P1 — 子代理审批策略（2-3 天）

现在 `spawner.rs:72` 无条件 `config.tools.auto_approve = true`。应改为：

- 默认**继承**父的审批策略与规则集（与 `ToolPolicy` 一致的"只能收窄"原则）
- 提供显式的 `SubAgentConfig.approval: Inherit | AutoApprove | Deny` 三档
- 选择 `AutoApprove` 时落一条 `approval/policy { mode: yolo, source: "delegation" }` 事件

否则"主 agent 受限、Spawn 一个子代理就全开"是可被模型自主利用的绕过路径。

### 9.6 P2 — 拒绝语义可配 + 无答复者 fail-closed

```rust
enum DenyBehavior {
    ContinueWithError,   // 现状：error result 喂回模型
    AbortRun,            // opencode 语义：终止整个 run
}
```

拒绝时带 message 走 `ContinueWithError`（纠偏），纯拒绝走 `AbortRun`（终止）——直接借鉴 opencode 的 `CorrectedError` / `DeclinedError` 区分。

同时给审批等待加超时与 fail-closed 默认：无 `approval_manager` 又非 `auto_approve` 时，当前是**永久挂起**；应当按 deepseek 的 `unavailable` 处理，返回拒绝并说明"没有可用的审批答复者"。headless / ACP / CI 场景必须有这个兜底。

---

## 10. 落地顺序

| 阶段 | 内容 | 预估 | 风险 |
|---|---|---|---|
| S1 | 9.1 三处缺陷修复 | 1-2 天 | 低 |
| S2 | 9.6 无答复者 fail-closed + 拒绝语义可配 | 3 天 | 低 |
| S3 | 9.2 规则化权限模型（含协议 `save` 字段、Tool trait 扩展、两路径统一） | 1-2 周 | 中（协议变更，需 UI 配合） |
| S4 | 9.5 子代理策略继承 | 2-3 天 | 低（依赖 S3） |
| S5 | 9.3 hook 参与决策 | 3-5 天 | 中（改执行顺序） |
| S6 | 9.4 审计事件 | 与事件日志主线合并 | 中 |

S1、S2 可立即合入。S3 是安全语义的关键修复，也是 S4/S5 的前置。

---

## 附：源码索引

**agentrs**
- 可用性策略 `crates/agentrs-agent/src/tool_policy.rs`
- 四道闸顺序 `crates/agentrs-agent/src/engine.rs:734-829`
- 宿主审批 `crates/agentrs-agent/src/orchestration.rs:346-475`（判定 `:374`，等待 `:391`）
- hook 阻断 `crates/agentrs-agent/src/orchestration.rs:220-232`
- 终端确认 `crates/agentrs-agent/src/confirm.rs:42-98`
- 审批管理器 `crates/agentrs-protocol/src/approval.rs`
- 协议命令 `crates/agentrs-protocol/src/commands.rs:18-79`，类别 `events.rs:98-117`
- 宿主接线 `crates/agentrs-cli/src/json_stream/dispatch.rs:29`、`message.rs:71`
- TUI 接线 `crates/agentrs-tui/src/app.rs:553-568`
- 子代理 `crates/agentrs-agent/src/spawner.rs:64-80`

**claude-code-main**
- 类型 `src/types/permissions.ts`（模式 `:16-35`，行为 `:44`，来源 `:52-62`，更新 `:98-129`，决策 `:177-259`，理由 `:262-300`）
- 规则解析 `src/utils/permissions/permissionRuleParser.ts:93-152`
- Shell 规则匹配 `src/utils/permissions/shellRuleMatching.ts:40-221`
- 主实现 `src/utils/permissions/permissions.ts`（1486 行）
- 消费点 `src/hooks/useCanUseTool.tsx:39-103`

**opencode**
- 权限服务 `packages/core/src/permission.ts`（求值 `:76-86`，合并 `:157-161`，ask/assert/reply `:190-260`，级联拒绝 `:243-247`）
- 保存规则 `packages/core/src/permission/saved.ts`
- 循环侧中断 `packages/core/src/session/runner/llm.ts:144-150, 297-301`

**deepseek-harness**
- 审批服务 `packages/interaction/user-approval/src/index.ts`（事件声明 `:17-72`，策略 `:85-146`）
- outcome 类型 `packages/interaction/user-approval/src/types.ts:26-29`
- 策略预设 `packages/interaction/permission-presets/src/`

**pi**
- 唯一拦截点 `packages/agent/src/agent-loop.ts:600-668`
- 扩展转发 `packages/coding-agent/src/core/agent-session.ts:478-497`
