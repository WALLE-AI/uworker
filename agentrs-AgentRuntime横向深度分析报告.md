# AgentRS Agent Runtime / Agent Loop 技术设计深度分析与横向对比

对比对象：`opensource/pi`、`opensource/claude-code-main`、`opensource/opencode`、`opensource/deepseek-harness`
分析日期：2026-09-03
分析方式：直接通读五套实现的循环主干源码（非文档推测）

---

## 0. 结论摘要

| 维度 | agentrs | claude-code | pi | opencode | deepseek-harness |
|---|---|---|---|---|---|
| 循环形态 | 单体 `while` + 内联状态 | 单体 `while` + 显式 `State` 记录 | 纯函数双层循环 + 回调配置 | Effect 双层循环 + defect 转移 | 状态机 `Phase` + turn/step 两级 |
| 状态真相 | 内存 `Vec<Message>`，快照落盘 | 内存消息数组，转录旁路 | 内存 `AgentContext` | **事件溯源 DB，历史为投影** | **事件日志，请求由日志派生** |
| 扩展方式 | 编译期 trait + hooks 配置 | feature flag + deps 注入 | config 回调函数 | Effect Layer/Service | **Cordis 插件 + waterfall 事件** |
| 循环终止治理 | **5 类断路器（业界最完整）** | 恢复转移 + maxTurns | `terminate` 标志 + `shouldStopAfterTurn` | step 上限 | 插件式提醒（不否决） |
| 工具并发 | 相邻同质分批 | 相邻同质分批 + 流式抢跑 | 全并行/全串行 | 流内 fiber 抢跑 | **有界滚动池 + 模型序提交** |
| 上下文治理 | micro→auto→emergency 三级 | budget→snip→micro→collapse→auto→reactive 六级 | transformContext 外置 | pre-request + overflow 两点 | 插件式能力（basic + pruner） |
| 可中断/可恢复 | 取消令牌 + 合成结果 | AbortController + 合成结果 | AbortSignal | Effect 中断 + durable | 三源熔断 signal + 合成结果 |
| 断点续跑 | 会话快照重放 | 会话文件 resume | 计划中（AgentHarness） | **原生 durable** | **原生 replay** |

一句话定位：**agentrs 的 loop 是 claude-code `query.ts` 的高质量 Rust 重构，在"循环病理治理"这一项上做到了五者中最强，但在"状态真相"（事件溯源）和"运行时可扩展性"（插件化）这两项上落后 opencode 与 deepseek-harness 整整一代。**

---

## 1. agentrs Agent Runtime 深度剖析

### 1.1 分层与落点

```
agentrs-cli  ──► bootstrap.rs（组装 provider/tools/skills/mcp/hooks）
                    │
agentrs-agent ──► AgentEngine（engine.rs, 1767 行）── 唯一的 loop 拥有者
                    ├── turn.rs         轮次分类 + 断路器聚合（TurnGuards）
                    ├── tool_call.rs    指纹 / 循环检测 / 结果归并
                    ├── orchestration.rs 工具批次调度 + 审批 + hooks
                    ├── compact/        micro / auto / emergency
                    ├── context_usage.rs 上下文会计（provider 精确 vs 本地估算）
                    ├── session.rs      持久化 + fork（turn_id 锚点）
                    ├── spawner.rs      子代理
                    └── output/         Sink 抽象（terminal / protocol / null）
```

设计上的关键选择：**engine 是唯一的可变状态所有者**。`messages`、`total_usage`、`context_state`、`plan_state`、`compact_state`、`allow_list` 全部是 `AgentEngine` 的私有字段，没有任何共享可变状态跨模块传递。这与 claude-code 把 `toolUseContext` 到处 spread 复制形成鲜明对比，是 Rust 所有权带来的实打实的收益。

### 1.2 主循环：`run_inner`（engine.rs:517-666）

```
run() ──► 斜杠命令拦截（不触发任何 LLM 调用）
       └► run_with_blocks() ──► run_inner()
              │
              ├─ 重置取消令牌、分配/接管 turn_id、发 stream_start
              ├─ push 用户消息、记账、存档
              └─ loop {
                    ① 轮次预算检查 → MaxTurns 直接返回
                    ② run_turn(Normal)
                         ├ run_compaction()   ← 每次 API 调用前
                         ├ build_request()    ← plan 模式过滤工具 + 追加指令
                         ├ provider.stream()
                         └ consume_stream()   ← 边流边发事件
                    ③ TurnOutcome::from_stream 四路分类
                    ④ execute_tool_round()
                    ⑤ apply_context_modifiers()  ← skill 可改 model/effort/工具白名单/plan 模式
                    ⑥ guards.after_tool_round() → Continue / Warn / Finalize / Stop
                    ⑦ push tool_results（+ follow_up_blocks），存档
                 }
```

**轮次分类**（turn.rs:22-32）是 agentrs 相对其它实现的一个显著清晰点：

```rust
if !tool_calls.is_empty()                      → ToolRound   // 继续循环
StopReason::EndTurn && text 非空                → Final       // 正常结束
StopReason::MaxTokens                          → Truncated   // 截断收尾
其余（含 EndTurn 但文本为空）                    → EmptyFinal  // 空回答
```

注意最后一条：**"stop_reason=EndTurn 但没有可见文本"被单独建模**。这是本地/开源模型的高频病态（把答案或工具调用塞进 reasoning 里）。agentrs 的处理是给一次"带工具的普通重试"（engine.rs:68 的 `EMPTY_FINAL_RETRY_PROMPT`，613-619 注入），而不是直接进入无工具收尾——注释里明确写了理由：无工具收尾会把想调工具的模型逼死。这个细节在 pi / opencode 中完全没有对应物。

### 1.3 收尾轮：`TurnKind::Finalization`（turn.rs:60-98 + engine.rs:925-980）

agentrs 把"停止"建模成了**一次显式的、禁用工具的额外模型调用**，而不是硬切断：

| 触发原因 | 注入的控制提示 | 失败兜底文案 |
|---|---|---|
| `TurnBudget` | 别再调工具，用已有结果给最终答案 | 达到轮次预算 |
| `ToolFailure` | 别再调工具，说明具体阻塞点和用户该提供什么 | 工具反复失败 |
| `MaxTokens` | 上一轮被截断，现在补完 | 被 token 上限截断 |
| `EmptyFinal` | 上一轮没有可见文本，给简短可见答案 | 重试后仍无可见回答 |

`disable_tools()` 在 `tool_definitions_for_turn` 里直接返回空工具表（engine.rs:706-724），从协议层面杜绝模型再次调工具。收尾成功则返回 `EndTurn`，失败则用兜底文案组装一个 assistant 消息塞回历史（保证历史合法）。

**这个设计在 opencode 里有同构物**：`isLastStep` 时 `toolChoice: "none"` 且追加 `MAX_STEPS_PROMPT`（llm.ts:202-213）。但 opencode 只覆盖"step 上限"一种原因，agentrs 覆盖四种。claude-code 的对应物是 `max_output_tokens_recovery` 的 meta 消息，pi 和 deepseek 则完全没有"收尾轮"概念。

### 1.4 循环病理治理：`TurnGuards`（turn.rs:129-343 + tool_call.rs）

这是 agentrs 最有辨识度的部分，五条独立断路器：

| 断路器 | 判据 | 阈值 | 动作 |
|---|---|---|---|
| `TurnTracker` | 计数轮次 | `max_turns` | Finalize(TurnBudget) |
| `ToolCallMalformedTracker` | **整轮全部畸形**且指纹连续相同（空函数名/空 id） | 3 | **Stop（硬错误）** |
| `ToolCallFailureTracker` | 失败调用指纹（name+args）连续相同 | 3 | Finalize(ToolFailure) |
| `ToolCallAllErrorRoundTracker` | 整轮工具全错的连续轮数 | 8 | Finalize(ToolFailure) |
| `ToolCallCycleTracker` | **周期 2..4 的失败指纹序列重复** | 3 次 | Finalize(ToolFailure) |

循环检测算法（tool_call.rs:365-395）值得单独说：维护最近 12 条失败指纹，对周期 2..4 逐一尝试匹配尾部模式向前重复次数，取重复次数最多（同分取周期最小）的作为结果。这能抓住 `A→B→A→B→A→B` 这种两工具互相甩锅的死循环——**其它四套实现都抓不到**（claude-code 只有各类 recovery 计数器，deepseek 的 `repeat-tool-reminder` 只检测"连续相同调用"，抓不到周期性交替）。

更关键的是**分级干预**：在硬停之前先注入自然语言告警。`append_tool_loop_warning`（engine.rs:1704-1721）把告警文本追加到**最后一条错误 tool_result 的尾部**，而不是新开一条消息：

```
[Tool recovery required: a 2-round tool-call cycle has repeated 2/3 times
 without progress. Break the cycle by changing strategy or explain the
 blocker in the final answer.]
```

追加到 tool_result 而非新消息，是为了不破坏 Anthropic 系 `tool_use ↔ tool_result` 的严格配对。这是个很实际的工程细节。

对照 deepseek-harness 的 `repeat-tool-reminder`（guard/repeat-tool-reminder/src/index.ts）：阈值 `[3,5,8]` 多级提醒、可配置 include/exclude 通配符、参数预览截断 500 字符——**但它是纯 advisory，不否决不改写**，且是可插拔插件。设计哲学差异明显：agentrs 内置且会强制终止，deepseek 外挂且只提醒。

### 1.5 工具编排（orchestration.rs）

**分批策略**（orchestration.rs:585-611）：线性扫描，把相邻的 `is_concurrency_safe` 调用聚成并发批，其余各自成串行批。与 claude-code 的 `partitionToolCalls`（toolOrchestration.ts:84-115）逻辑完全一致——**这是直接对标移植**。

**双执行路径**：
- 终端模式 → `execute_tool_calls_with_output_limit`：`ToolConfirmer` 同步确认
- 宿主模式 → `execute_tool_calls_with_approval_and_output_limit`：发 `ToolRequest` 事件、`await` 审批通道、发 `ToolRunning`/`ToolResult`

两条路径各自实现了一遍循环（engine.rs:770-821 分发），代码有明显重复——注意这是**当前实现的一个结构性缺陷**：并发分批只在终端路径生效，审批路径是纯串行的（orchestration.rs:364 单层 `for`）。宿主（AgentrsUI）场景下拿不到并发收益。

**结果加工管线**（orchestration.rs:234-290）：
```
pre_tool_use hook → tool.execute_with_follow_up(cancel)
  → deferred 工具失败时追加 ToolSearch 提示
  → compact_output(level) → 可选 TOON 编码
  → truncate_result(min(tool.max_result_size, config.tool_output_max_bytes))
  → post_tool_use hook
```
截断采用**头尾各半 + 中间标记**（orchestration.rs:541-568），并做 UTF-8 边界回退。头尾保留优于纯头部截断（错误信息通常在尾部）。

**Skill hooks 动态合入**（orchestration.rs:478-497）：`Skill` 工具成功后把该 skill 声明的 hooks 合并进 `HookEngine`，实现"技能激活后改变后续工具行为"。

### 1.6 上下文治理（compact/ + context_usage.rs）

**三级流水线**（engine.rs:1157-1259），每次 API 调用前跑：

1. **microcompact**（无 LLM 调用）：把 `compactable_tools` 白名单内、除最近 N 条外的 tool_result 内容替换为 `[Tool result cleared]`。双触发器：最近 assistant 消息超时（`micro_gap_seconds`）或可压缩结果数 > `keep_recent*2`。
2. **autocompact**（LLM 摘要）：超阈值时把整段历史交给模型摘要，替换为「边界标记 + 摘要」两条消息。带 **PTL 重试**（提示过长时丢弃最老 20% 重试，最多 2 次）和**熔断器**（连续失败计数）。
3. **emergency**：仍超 `context_window - emergency_buffer` 则直接返回 `ContextTooLong` 错误。

**上下文会计**（context_usage.rs）是个被低估的设计：区分 `ProviderExact`（供应商返回的 usage）和 `LocalProjected`（本地估算）两种来源。provider 返回 usage 就用精确值覆盖；不返回（很多兼容端点不返回）就用本地估算累加。`refresh_local_context_estimate` 会重算 system prompt + 工具定义 + 全部消息。**opencode 只有本地估算**（`Token.estimate(JSON.stringify(...))`），claude-code 有 `tokenCountWithEstimation` 但混得更乱。agentrs 这里更干净。

### 1.7 中断与一致性

- `turn_cancel: CancellationToken` 逐轮重置（engine.rs:1611），传给每个工具的 `execute_with_follow_up`，长耗时网络工具能立即退出而不必等超时。
- `abort_current_turn`（engine.rs:1624-1673）：宿主中途丢弃 `run()` 时，扫描最后一条 assistant 消息里所有未配对的 `tool_use`，**合成 `is_error: true` 的 tool_result 补齐**，保证历史对 Anthropic 协议合法。

同样的问题，其它四套的解法：
- claude-code：`yieldMissingToolResultBlocks`（query.ts:900+）
- opencode：`failInterruptedTools` + `failUnsettledTools`（llm.ts:119-139, 299）
- deepseek：`appendSkippedToolCall` 写合成 error 事件（tool-calls.ts:249-259）
- pi：`failToolCallsFromTruncatedMessage`（agent-loop.ts:381-406）——**pi 还多做一件事**：`stopReason === "length"` 时把该消息**所有**工具调用全部判错，理由是流式 JSON 抢救解析可能产出"能解析但静默不完整"的参数。agentrs 目前只在 `Truncated` 分支走收尾轮，**没有做这层参数可信度判断**（见 §5 建议）。

### 1.8 子代理（spawner.rs）

- 共享父进程的 `Arc<dyn LlmProvider>`（连接复用）
- `NullSink` 丢弃子代理流式输出，只有父进程写 stdout —— 注释里明确说这是对齐 Claude Code
- 强制 `session.enabled = false`、`tools.auto_approve = true`
- **工具策略只能收窄不能放宽**：`effective_child_tool_policy` 对 `allowed_tools` 与父策略取交集（spawner.rs:192-203）
- 子代理工具表是**硬编码 6 件套**（Read/Write/Edit/ExecCommand/Grep/Glob，spawner.rs:206-216）——不含 WebFetch/WebSearch/MCP/Skill，这是个明确的能力缺口

---

## 2. claude-code-main：单体生成器 + 显式状态记录 + 六级上下文管线

`src/query.ts`（1729 行）的 `queryLoop` 是一个 `AsyncGenerator`，用 `while(true)` + 显式 `State` 对象驱动：

```ts
type State = {
  messages, toolUseContext, autoCompactTracking,
  maxOutputTokensRecoveryCount, hasAttemptedReactiveCompact,
  maxOutputTokensOverride, pendingToolUseSummary,
  stopHookActive, turnCount,
  transition: Continue | undefined   // 上一轮为什么继续（测试可断言）
}
```

注释写得很直白：`Continue sites write state = { ... } instead of 9 separate assignments`。**7 个 continue 点**各自构造完整 State：`collapse_drain_retry` / `reactive_compact_retry` / `max_output_tokens_escalate` / `max_output_tokens_recovery` / `stop_hook_blocking` / `token_budget_continuation` / `next_turn`。终止用 `Terminal` 联合类型：`blocking_limit` / `image_error` / `model_error` / `aborted_streaming` / `aborted_tools` / `hook_stopped` / `prompt_too_long` / `stop_hook_prevented` / `max_turns` / `completed`。

**六级上下文管线**（query.ts:370-540），顺序有严格理由：
```
applyToolResultBudget  ← 按 tool_use_id 做内容替换，对 MC 不可见
  → snipCompact        ← tokensFreed 会传给 autocompact 修正阈值
  → microcompact       ← 支持 cache editing，边界消息延后到拿到 cache_deleted_input_tokens
  → contextCollapse    ← 读时投影，摘要存在 collapse store 不在消息数组
  → autocompact        ← LLM 摘要
  → reactiveCompact    ← 413 之后的兜底（错误被 withhold 到恢复判定之后）
```

**错误 withhold 机制**是 agentrs 没有的：`prompt_too_long`、`max_output_tokens`、媒体尺寸错误在流式阶段**不 yield 给 UI**，先押住；等恢复路径（collapse drain → reactive compact → 截断重试）全部失败才补 yield。避免了"用户看到报错但系统其实自愈了"。

**流式工具抢跑**：`StreamingToolExecutor` 在模型还在流式输出时就把已完成的 `tool_use` block 送去执行（query.ts:`addTool` / `getCompletedResults`），流结束后 `getRemainingResults()` 收尾。并发上限 10（`CLAUDE_CODE_MAX_TOOL_USE_CONCURRENCY`）。**agentrs 是严格的"流结束才执行工具"**，这是明确的延迟差距。

**模型 fallback**：`FallbackTriggeredError` 触发时切模型重试整个请求，并对已产生的 assistant 消息发 `tombstone` 事件让 UI 撤回（thinking block 签名与模型绑定，重放会 400）。

---

## 3. pi：纯函数循环 + 回调式扩展 + 面向 durable 的第二代 API

### 3.1 当前循环：`agent-loop.ts`（796 行）

`runLoop` 是**双层循环**：

```ts
while (true) {                                    // 外层：followUp 队列
  while (hasMoreToolCalls || pendingMessages.length > 0) {   // 内层：steering + 工具
    注入 pendingMessages（steering）
    streamAssistantResponse()
    if (stopReason === "length") failToolCallsFromTruncatedMessage()
    else executeToolCalls()
    prepareNextTurn?.()      // 可换 model / thinkingLevel
    shouldStopAfterTurn?.()  // 宿主决定停不停
    pendingMessages = getSteeringMessages()
  }
  followUpMessages = getFollowUpMessages()
  if (有) { pendingMessages = 它; continue }
  break
}
```

**steering / followUp 双队列**是 pi 的招牌设计：`steer` 消息在下一次 assistant 响应**之前**注入（用户打断纠偏），`followUp` 消息只在 agent 本来要停时才生效（追加任务）。队列有 `one-at-a-time` / `all` 两种排空模式（agent.ts:125-159）。**agentrs 引擎层完全没有这个能力**——用户中途输入只能等本轮跑完。

**扩展方式是配置回调**（`AgentLoopConfig`）：`beforeToolCall` / `afterToolCall` / `shouldStopAfterTurn` / `prepareNextTurn` / `transformContext` / `convertToLlm` / `getApiKey`。没有 hook 引擎、没有插件系统，纯函数注入。轻但不可运行时组合。

**`terminate` 标志**：工具结果可以带 `terminate: true`，`shouldTerminateToolBatch` 要求**整批全部** terminate 才停（agent-loop.ts:582-584）。这是"工具主动结束循环"的最简实现，agentrs 无对应物（agentrs 用 `ContextModifier` 改配置，但不能让工具主动终止循环）。

**并发保序**：`executeToolCallsParallel` 把执行包成 thunk 数组，`Promise.all` 后**按模型顺序**生成 tool_result 消息（agent-loop.ts:540-548）——避免完成顺序污染历史。agentrs 用 `join_all` + 按批次顺序 extend，效果等价。

### 3.2 未来循环：`AgentHarness`（agent-harness.ts，骨架态）

这是 pi 正在建的第二代 API，**大部分方法还是 `HarnessNotImplemented`**，但接口已经透露了目标形态，很值得 agentrs 参考：

- **Lane（多泳道）**：`createLane` / `lanes()`，同一 session 树上多条并行推进线
- **Session Tree + 导航**：`navigateTree(targetId, {summarize})`，可回到历史任意节点分叉，并对被离开的分支生成 `BranchSummaryEntry`
- **三队列**：`steer` / `followUp` / `nextRun`
- **确定性动作机**：`peekAction()` / `executeAction()` / `runToCompletion()`，`ActionInfo` 枚举了 `append_entry` / `stream_assistant` / `execute_tool` / `hook` / `sleep` 等——**把循环拆成可单步执行、可录制、可重放的原子动作**
- **挂起/恢复**：`SuspendedOperation { reason: "crash" | "deferred" }`、`MissingIdentities`（恢复时发现工具/模型不存在）
- **replay 语义标注**：`HarnessTool = AgentTool & { replay?: "never" | "safe" }`
- **11 个命名 hook**：`before_run` / `transform_context` / `before_request` / `before_payload` / `after_response` / `before_tool` / `after_tool` / `before_compaction` / `before_navigation` / ...

**这套接口是 agentrs 未来 3-6 个月最值得对标的东西**，尤其是 `peekAction/executeAction` 的确定性动作机——它同时解决了可测试性、可录制回放（agentrs 现在有 `vcr.rs` 但只在 provider 层）、崩溃恢复三个问题。

---

## 4. opencode：Effect 事件溯源 + 流内 fiber 抢跑

`packages/core/src/session/runner/llm.ts`（432 行）。技术栈是 Effect（`Effect.gen` / `Layer` / `FiberSet` / `Semaphore` / `Stream`），整体是**服务化 + 事件溯源**。

### 4.1 双层循环（llm.ts:383-406）

```ts
while (shouldRun) {              // 外层：queue（新用户输入）
  needsContinuation = true; step = 1
  while (needsContinuation) {    // 内层：step（工具轮）
    result = runTurn(sessionID, promotion, step)
    needsContinuation = result.needsContinuation
    step++
    promotion = "steer"
    if (!needsContinuation) needsContinuation = hasPending("steer")  // steer 可续命
  }
  shouldRun = hasPending("queue")
}
```

**`promotion` 机制**：每轮开始时把 durable 队列里的 steer/queue 消息"提升"为正式消息（`promoteSteers` / `promoteNextQueued`），且**提升成功则把 step 重置为 1**（llm.ts:195）——新用户输入重置 step 预算，语义比 agentrs 的"整个 run 一个 max_turns"更合理。

### 4.2 用 defect 做控制流转移（llm.ts:152-166, 355-381）

编译期压缩不掉的巧思：编译不出 `continue`（因为在 Effect 生成器里），于是定义 `TurnTransitionError` 作为 **defect** 抛出，外层 `catchDefect` 捕获后递归重跑：

```ts
ContinueAfterCompaction         → runTurn 重跑（仍可再压缩）
ContinueAfterOverflowCompaction → runAfterOverflowCompaction 重跑（禁止二次溢出恢复）
```

第二次溢出直接 `Effect.die("Post-compaction provider attempt cannot recover another overflow")`——**用类型区分"能再恢复"和"不能再恢复"的重跑**，比 agentrs 用布尔标志优雅。

### 4.3 流内 fiber 抢跑（llm.ts:232-275）

```ts
llm.stream(request).pipe(Stream.runForEach(event => {
  if (event.type === "tool-call" && !event.providerExecuted) {
    needsContinuation = true
    Effect.uninterruptibleMask(restore =>
      restore(toolMaterialization.settle({ ... }))
        .pipe(Effect.flatMap(settlement => publish(LLMEvent.toolResult({...}))))
    ).pipe(FiberSet.run(toolFibers))    // ← 立即起 fiber，不等流结束
  }
}))
// 流结束后
awaitToolFibers(toolFibers)   // raceFirst(join, awaitEmpty)
```

与 claude-code 的 `StreamingToolExecutor` 同一思路（流式抢跑），但 opencode 用 `uninterruptibleMask` 保证工具**执行阶段不可中断**（避免留下半成品），只有等待阶段可中断。所有事件发布用 `Semaphore(1).withPermit` 串行化，保证日志顺序。

### 4.4 上下文溢出双点治理

- `compactIfNeeded`（请求前）：`estimate({system, messages, tools}) > context - max(output, buffer)` 则压缩
- `compactAfterOverflow`（413 后）：`isContextOverflowFailure(event) && !publisher.hasAssistantStarted()` 才触发——**已经开始出文本了就不重来**

摘要模板是固定六段式（Objective / Important Details / Work State{Completed,Active,Blocked} / Next Move / Relevant Files），并支持**增量更新前一份摘要**（`previousSummary` + "Preserve still-true details, remove stale details, and merge in the new facts"）。agentrs 的 `compact/prompt.rs` 是一次性全量摘要，**没有摘要迭代更新语义**。

### 4.5 权限即中断

`PermissionV2.DeclinedError` / `QuestionV2.RejectedError` 被识别为 `isUserDeclined`，直接 `Effect.interrupt` 终止整个循环，而不是变成"工具被拒绝"的 tool_result 喂回模型（llm.ts:144-150, 297-301）。注释明确说这是对齐 V1 行为。**agentrs 是反过来的**：拒绝生成 `is_error` 的 tool_result 继续跑（orchestration.rs:194-198）。两种语义各有道理，但 agentrs 没有"拒绝即终止"的选项。

---

## 5. deepseek-harness：Cordis 插件化 + turn/step 双层状态机 + 日志即真相

`packages/core/agent-loop/`（1643 行）。这是五者中**架构约束最严格**的一套。

### 5.1 铁律：Model-visible ⟺ logged

来自 AGENTS.md：

> **Model-visible ⟺ logged**: anything that reaches a model request must be reconstructable from the session log; a new model-visible input requires a session event.
> **Plugins, not loop changes**: new behavior goes on documented extension points; changing `agent-loop` requires updating docs/architecture.md.

`step()` 里请求消息来自 `this.session.deriveMessages()`（agent.ts:341）——**不是内存数组，是日志派生**。连模型路由变化都要落事件：`request/header`（initial/resume/change 三种 reason）、`request/context`（provider/model/contextWindow）。

对比 agentrs：`self.messages.clone()` 直接给 provider（engine.rs:684）。会话文件是**快照**不是日志，模型切换只在 `apply_config_update` 里改字段 + 存档，没有独立事件。**这是 agentrs 与 deepseek/opencode 之间最本质的代际差异。**

### 5.2 Phase 状态机 + turn/step 两级（agent.ts:38-46, 246-401）

```ts
type Phase =
  | { kind: 'idle'; lastTurn }
  | { kind: 'maintenance'; abort; lastTurn; wakeRequested }   // 压缩等维护作业独占
  | { kind: 'running'; abort; turn; step; wakeRequested }
```

`maintenance` 是个 agentrs 没有的相位：压缩、标题生成等作业需要独占 agent 但**不算 running**，且期间到达的唤醒会被 `wakeRequested` 闩住，维护结束后补发。agentrs 的 autocompact 是内联在 `run_turn` 里的，无法从外部观测或独占调度。

`turn()` / `step()` 双层：一个 turn 内可以有多个 step（每个 step 一次模型调用 + 一轮工具）。`max-tokens` 结束原因是**粘性的**：一旦某 step 触顶，后续正常完成的 step 不能把 turn 结果降级回 `completed`（agent.ts:290）。

### 5.3 Inbox 三通道（agent.ts:113-132）

```ts
followup(m) → send(m, 'next-turn', wakeup=true)   // 新一轮
steer(m)    → send(m, 'next-step', wakeup=true)   // 本轮下一步
inject(m)   → send(m, 'next-step', wakeup=false)  // 不唤醒，搭下一步的车
```

`inject` 是三者中独有的：**工具结果的附加上下文**走这条路——`executeToolCalls` 的 `acceptContext` 回调把 `additionalContexts` 塞进 `next-step` inbox（agent.ts:397）。agentrs 的对应物是 `follow_up_blocks`，但它是直接 push 到历史（engine.rs:649-651），没有"投递通道"的抽象。

还有个并发正确性细节：`wakingAfterAbort` 在**插入 inbox 之前**就计算好（agent.ts:116），注释说明是防止 splice 观察者重入触发 cancel 后重新分类。

### 5.4 工具调度器（tool-calls.ts）—— 五者中最复杂也最正确

```
executeToolCalls:
  按 executionMode 逐个分组：exclusive → 单元素 barrier；parallel → 后续全部候选
  runGroup:
    fillPool: 有界滚动池（maxParallelToolCalls，运行时可改）
              每次启动前【重新分类】——注册表变化能立刻造出新 barrier
    Promise.race(inFlight) 逐个收敛
    commitReady: 只沿【连续的模型序槽位】推进 committed 指针
                 → 结果、additionalContexts、concludesTurn 严格模型序提交
    abort: 停止新启动 → 排空已启动 → 未启动的补 appendSkippedToolCall 合成错误
    scheduler failure: 停止新 dispatch → 排空 → 抛首个错误，【不伪造 tool result】
```

三个 agentrs 没有的性质：
1. **有界滚动池**而非"整批 join_all"——大批量工具调用不会一次性打满
2. **运行时重分类**——池填充时对每个后续调用重新查 `executionMode`，注册表变了立刻生效
3. **区分 abort 与 scheduler failure**——abort 补合成结果（保证 replay 合法），scheduler 内部故障**不伪造结果**（不污染日志真相）

agentrs 的 `partition` 是**一次性静态分批**（orchestration.rs:585），批内 `join_all` 无上限，且不区分"用户中断"与"编排器自身故障"。

### 5.5 扩展点即事件（waterfall / serial）

| 扩展点 | 类型 | 用途 |
|---|---|---|
| `agent/pre-step` | waterfall | 改写/拒绝本 step 的输入消息（plan 模式、压缩注入走这里） |
| `agent/request` | waterfall | 提议本次请求配置（provider/model/effort/maxTokens） |
| `agent/request-error` | waterfall | 决定重试策略 |
| `agent/turn-stopping` | serial | turn 即将结束时的最后干预 |
| `agent/status` `agent/error` `agent/inbox/*` | emit | 观测 |

**所有能力都是插件**：compaction、guard、plan、todo、skill、subagent、web、shell、lsp 各是独立包，通过 `ctx.effect()` 注册且返回 disposer（HMR 安全，有专门的测试门禁）。

agentrs 的对应物是 `HookEngine`（外部进程 hook）+ 编译期 trait。**hook 只能观测和阻断，不能改写请求配置**；skill 的 `ContextModifier` 能改 model/effort/白名单，但那是硬编码在 engine 里的特例，不是通用扩展点。

---

## 6. 横向对比矩阵

### 6.1 循环控制

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| 轮次上限 | ✅ max_turns | ✅ maxTurns | ❌ | ✅ agent.steps | ❌（插件可加） |
| 无工具收尾轮 | ✅ **4 种原因** | 部分（otk recovery） | ❌ | ✅ 仅 step 上限 | ❌ |
| 空回答重试 | ✅ 带工具单次重试 | ❌ | ❌ | ❌ | ❌ |
| 畸形调用断路 | ✅ 指纹×3 硬停 | ❌ | ❌ | ❌ | ❌ |
| 相同失败断路 | ✅ 指纹×3 | ❌ | ❌ | ❌ | ⚠️ 提醒不停 |
| 全错轮断路 | ✅ ×8 | ❌ | ❌ | ❌ | ❌ |
| **周期性循环检测** | ✅ **周期2-4×3** | ❌ | ❌ | ❌ | ❌ |
| 分级告警注入 | ✅ 追加到 tool_result | ❌ | ❌ | ❌ | ✅ 独立消息 |
| 工具主动终止循环 | ❌ | ✅ hook_stopped | ✅ terminate | ✅ | ✅ concludesTurn |
| 模型 fallback | ❌ | ✅ +tombstone | ❌ | ❌ | ⚠️ 中间件 |
| turn/step 两级 | ❌ 单级 | ❌ | ❌ | ✅ | ✅ |

### 6.2 输入与交互

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| steering（轮内插话） | ❌ | ⚠️ queue drain | ✅ | ✅ durable | ✅ next-step |
| followUp（停后续跑） | ❌ | ⚠️ | ✅ | ✅ durable | ✅ next-turn |
| inject（不唤醒） | ⚠️ follow_up_blocks | ✅ attachments | ❌ | ❌ | ✅ |
| 队列排空模式 | ❌ | ❌ | ✅ one/all | ✅ | ✅ |
| 新输入重置预算 | ❌ | ❌ | ❌ | ✅ step=1 | ✅ 新 turn |
| 斜杠命令拦截 | ✅ 引擎内 | ✅ 引擎外 | ✅ 模板 | ✅ | ✅ 插件 |

### 6.3 工具执行

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| 相邻同质分批 | ✅ | ✅ | ❌ 全并/全串 | — | ✅ 动态 |
| 并发上限 | ❌ 无界 | ✅ 10 | ❌ 无界 | ❌ 无界 fiber | ✅ 可配 |
| 运行时重分类 | ❌ | ❌ | ❌ | ❌ | ✅ |
| 流式抢跑 | ❌ | ✅ | ❌ | ✅ | ❌ |
| 模型序提交 | ✅ 批序 | ✅ | ✅ | ⚠️ 事件序 | ✅ 严格 |
| 审批路径也并发 | ❌ **串行** | ✅ | ✅ | ✅ | ✅ |
| 中断补合成结果 | ✅ | ✅ | ✅ | ✅ | ✅ |
| 区分中断/编排故障 | ❌ | ❌ | ❌ | ⚠️ | ✅ |
| 截断策略 | ✅ 头尾各半 | ✅ budget | ⚠️ | ✅ 头部 | ✅ |
| 输出压缩（TOON） | ✅ | ❌ | ❌ | ❌ | ❌ |

### 6.4 上下文治理

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| 无 LLM 微压缩 | ✅ | ✅ +cache edit | ⚠️ | ❌ | ✅ pruner 插件 |
| LLM 摘要压缩 | ✅ | ✅ | ✅ | ✅ | ✅ |
| 摘要增量更新 | ❌ 全量 | ⚠️ | ⚠️ | ✅ | ⚠️ |
| 413 后被动压缩 | ❌ | ✅ reactive | ❌ | ✅ overflow | ✅ |
| 分支摘要 | ❌ | ✅ collapse | 🚧 harness | ❌ | ❌ |
| PTL 重试降级 | ✅ 丢 20%×2 | ✅ | ❌ | ❌ | ❌ |
| 压缩熔断器 | ✅ | ✅ | ❌ | ❌ | ✅ |
| 紧急硬失败 | ✅ | ✅ blocking | ❌ | ❌ | ❌ |
| provider 精确用量 | ✅ **双源** | ⚠️ | ⚠️ | ❌ 纯估算 | ✅ token-meter |
| 缓存命中诊断 | ✅ | ✅ | ❌ | ⚠️ cacheKey | ❌ |

### 6.5 架构与可演进性

| 能力 | agentrs | claude-code | pi | opencode | deepseek |
|---|:-:|:-:|:-:|:-:|:-:|
| 状态真相 | 内存+快照 | 内存+转录 | 内存 | **事件溯源DB** | **事件日志** |
| 请求由日志派生 | ❌ | ❌ | ❌ | ✅ | ✅ |
| 运行时插件 | ❌ 编译期 | ⚠️ feature | ❌ 回调 | ✅ Layer | ✅ **Cordis** |
| 请求配置扩展点 | ⚠️ skill 特例 | ❌ | ✅ prepareNextTurn | ❌ | ✅ waterfall |
| 会话分叉 | ✅ turn_id 锚点 | ✅ | 🚧 tree | ✅ | ✅ |
| 崩溃恢复 | ⚠️ 快照重放 | ✅ resume | 🚧 suspended | ✅ durable | ✅ replay |
| 确定性动作机 | ❌ | ❌ | 🚧 peek/exec | ❌ | ⚠️ |
| 录制回放 | ⚠️ provider VCR | ✅ dumpPrompts | ❌ | ✅ | ✅ snapshot |
| 子代理 | ✅ 共享 provider | ✅ Task | ⚠️ | ✅ | ✅ 插件 |
| 多泳道并行 | ❌ | ✅ coordinator | 🚧 lanes | ❌ | ❌ |

---

## 7. agentrs 的相对优势（应当保持）

1. **循环病理治理是全场最强**。五条断路器 + 周期检测 + 分级自然语言告警 + 四原因收尾轮，这套组合拳其它四家都不完整。尤其周期检测和 EmptyFinal 重试，是**面向非 Anthropic 模型/本地模型**的真实工程经验沉淀——claude-code 不需要（自家模型），opencode/pi 还没踩到。
2. **上下文会计双源**。`ProviderExact` / `LocalProjected` 显式区分，配合 `refresh_local_context_estimate` 在工具集/plan 模式变化时重算。opencode 是纯估算，容易在兼容端点上失准。
3. **所有权清晰**。engine 单一可变状态所有者，无跨模块共享可变态。这是 Rust 的结构性收益，重构成本远低于 TS 那几套。
4. **协议一致性防御到位**。`abort_current_turn` 合成 tool_result、`merge_tool_results` 按原序回填畸形/拒绝结果、告警追加到既有 tool_result 而非新消息——这些细节说明作者对 Anthropic 消息协议的边界很熟。
5. **性能底座**。共享 provider 的子代理、`CancellationToken` 逐轮传递、Rust 本身的启动与内存优势。

---

## 8. 明确差距与建议（按 ROI 排序）

### P0 — 结构性缺陷，应尽快修

**8.1 审批路径不并发**
`execute_tool_calls_with_approval_and_output_limit`（orchestration.rs:364）是单层 `for`，宿主模式下所有工具串行执行。终端路径的 `partition` 并发收益在 AgentrsUI 里完全拿不到。
→ 建议：抽出统一的 `execute_batches(calls, gate: impl Gate)`，把"确认"抽象成 `Gate` trait（`Confirmer` / `ApprovalManager` 两个实现），两条路径共用分批逻辑。顺带消除当前 ~200 行的重复。

**8.2 并发批无上限**
`futures::future::join_all`（orchestration.rs:107）对整批无界并发。模型一次吐 30 个 Read 就是 30 个并发文件 IO。
→ 建议：引入 `max_parallel_tool_calls` 配置（对齐 claude-code 的 10 / deepseek 的可配），用 `futures::stream::iter(...).buffered(n)` 实现有界池。

**8.3 截断消息的工具参数不可信**
`TurnOutcome::Truncated` 目前直接走收尾轮，但如果那条被截断的 assistant 消息里**已经带了 tool_calls**，`from_stream` 会先命中 `!tool_calls.is_empty()` 分支当成正常 ToolRound 执行。参数可能被流式 JSON 抢救解析补全成"能解析但静默不完整"。
→ 建议：照抄 pi 的 `failToolCallsFromTruncatedMessage`（agent-loop.ts:381-406）——`stop_reason == MaxTokens` 时把该轮**所有**工具调用判为错误结果并要求重发，而不是执行。这是**正确性 bug 级别**的差距。

### P1 — 能力缺口，影响产品体验

**8.4 缺 steering / followUp 队列**
用户中途输入必须等整个 `run()` 跑完。pi / opencode / deepseek 三家都有。
→ 建议：在 `AgentEngine` 上加两个 `Arc<Mutex<VecDeque<Vec<ContentBlock>>>>`，主循环第 ⑦ 步之后排空 steering 队列注入历史；`Final` 分支返回前检查 followUp 队列决定是否继续。排空模式对齐 pi 的 `one-at-a-time` / `all`。协议层加 `steer` / `follow_up` 两个 command。

**8.5 缺 413 后被动压缩**
现在只有"请求前主动压缩"，一旦供应商实际拒绝（估算不准、多模态开销），直接失败。claude-code 有 reactiveCompact，opencode 有 compactAfterOverflow。
→ 建议：在 `run_turn` 捕获 `ProviderError::PromptTooLong`，若本轮尚未产出 assistant 文本，则触发一次 autocompact 后重试（单次，用布尔标志防循环）。opencode 的 `!publisher.hasAssistantStarted()` 判据可以直接借鉴。

**8.6 子代理工具集硬编码 6 件套**
`build_tool_registry`（spawner.rs:206-216）只给 Read/Write/Edit/ExecCommand/Grep/Glob。子代理拿不到 WebFetch/WebSearch/Skill/MCP。
→ 建议：改为从父 `ToolRegistry` 按 `ToolPolicy` 过滤克隆，而不是重新构造固定列表。

**8.7 摘要不支持增量更新**
每次 autocompact 都是全量重摘。长会话多次压缩会持续丢失早期细节。
→ 建议：借鉴 opencode 的 `buildPrompt({previousSummary, context})`——检测历史里已有的 compact 边界，把上一份摘要作为 `<previous-summary>` 传入，指令改为"保留仍然成立的、删除过时的、合并新事实"。

### P2 — 架构演进，决定 3-6 个月后的天花板

**8.8 状态真相：快照 → 事件日志**
这是与 opencode / deepseek 的代际差。当前 `save_session` 每步全量 clone `Vec<Message>` 并写盘（engine.rs:1589-1599），长会话下是 O(n²) 的 IO，且无法做部分回滚、无法审计"为什么这条消息进了请求"。
→ 建议路线（渐进，不要一次性重写）：
1. 先加 append-only 事件日志作为**旁路**（`turn/start`、`step/start`、`assistant/message`、`tool/call`、`tool/result`、`request/header`、`compaction/*`），快照仍是主存储；
2. 实现 `derive_messages(log) -> Vec<Message>` 并在测试中断言它与内存 `messages` 等价；
3. 等价性稳定后翻转主从，快照降级为加速缓存。

deepseek 的 `SESSION_FORMAT_VERSION` + "required-on-read by default，除非事件带 `ignorable: true`" 是很好的版本策略，值得照搬。

**8.9 扩展点：从 hook 到 waterfall**
现在 `HookEngine` 只能观测/阻断，skill 的 `ContextModifier` 是硬编码特例。
→ 建议：定义一组 in-process 扩展点 trait（对标 deepseek）：
```rust
trait PreStep    { async fn on_pre_step(&self, ctx) -> PreStepDecision; }  // 改写/拒绝输入
trait RequestCfg { async fn on_request(&self, cfg) -> LlmRequestConfig; }  // 提议 model/effort
trait RequestErr { async fn on_request_error(&self, e) -> RetryAction; }   // 重试决策
```
把 plan 模式、压缩注入、skill modifier 全部改写为这套扩展点的实现，engine 里只留调度。这样"新增行为不改 loop"才能成立。

**8.10 确定性动作机（长期）**
pi 的 `peekAction()` / `executeAction()` / `ActionInfo` 是同时解决**可测试、可回放、可崩溃恢复**的最优解。agentrs 现在的 `vcr.rs` 只在 provider 层录制，工具执行、hook、压缩都不可回放。
→ 建议：把 8.8 的事件日志做成"动作日志"（每个事件既是状态变更也是可重放动作），逐步逼近 pi 的接口形态。

---

## 9. 建议的落地顺序

| 阶段 | 内容 | 预估 | 风险 |
|---|---|---|---|
| S1 | 8.3 截断工具参数判错 + 8.2 并发上限 + 8.1 审批路径合并 | 3-5 天 | 低（有测试覆盖） |
| S2 | 8.4 steering/followUp 队列 + 协议 command | 1 周 | 中（涉及协议与 TUI） |
| S3 | 8.5 413 被动压缩 + 8.7 摘要增量更新 + 8.6 子代理工具集 | 1 周 | 低 |
| S4 | 8.9 扩展点 trait 化，把 plan/skill/compact 迁过去 | 2-3 周 | 中高（engine 重构） |
| S5 | 8.8 事件日志旁路 + 等价性断言 | 3-4 周 | 高（存储格式） |
| S6 | 8.8 翻转主从 + 8.10 动作机 | 长期 | 高 |

S1-S3 是纯增益、可独立合并；S4 是 S5/S6 的前置（扩展点先抽出来，事件日志才有清晰的记录边界）。

---

## 附：关键源码索引

**agentrs**
- 主循环 `crates/agentrs-agent/src/engine.rs:517-666`
- 轮次分类 `crates/agentrs-agent/src/turn.rs:22-32`
- 收尾轮 `turn.rs:60-98` + `engine.rs:925-980`
- 断路器 `turn.rs:186-343`，指纹与循环检测 `tool_call.rs:245-396`
- 工具编排 `orchestration.rs:62-158`（终端）/ `346-475`（审批）/ `585-611`（分批）
- 压缩流水线 `engine.rs:1157-1259`，micro `compact/micro.rs`，auto `compact/auto.rs`
- 上下文会计 `context_usage.rs`，中断补齐 `engine.rs:1624-1673`
- 子代理 `spawner.rs:64-203`

**claude-code-main**
- `src/query.ts:241-1729`（queryLoop），State 定义 `:204-218`
- 工具分批 `src/services/tools/toolOrchestration.ts:19-115`
- 流式抢跑 `src/services/tools/StreamingToolExecutor.ts`

**pi**
- 循环 `packages/agent/src/agent-loop.ts:155-275`
- 截断判错 `:381-406`，并发保序 `:489-554`
- 状态包装与队列 `packages/agent/src/agent.ts:125-159, 409-484`
- 第二代 API `packages/agent/src/harness/agent-harness.ts:182-303`

**opencode**
- Runner `packages/core/src/session/runner/llm.ts:173-406`
- 流内 fiber `:232-275`，defect 转移 `:152-166, 355-381`
- 压缩 `packages/core/src/session/compaction.ts:170-241`

**deepseek-harness**
- 状态机 `packages/core/agent-loop/src/agent.ts:38-401`
- 请求构造与日志 `:407-495`
- 工具调度器 `packages/core/agent-loop/src/tool-calls.ts:59-246`
- 工厂与生命周期 `packages/core/agent-loop/src/index.ts:296-711`
- 循环卫生插件 `packages/guard/repeat-tool-reminder/src/index.ts`
