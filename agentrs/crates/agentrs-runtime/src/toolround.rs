//! 工具回合与审批挂起（架构 §6.2、§8，任务 T04B）。
//!
//! ## 五个不可绕过的固定阶段
//!
//! ```text
//! ★StepIntent → schema 校验 → guard → Hook → ★Policy/grant → ★Sandbox → ★StepResult
//! ```
//!
//! schema 校验排在 `StepIntent` **之后**而不是之前，这与直觉相反，理由是单一出口：
//! 每条拒绝路径都必须有 `StepResult` 与之配对，否则 `ToolPaths` 投影会把它们误判成
//! "有意图无结果"，恢复时逐个去 reconcile 一件根本没发生的事（架构 §18.4）。
//! 畸形提议**也是提议**，把它记下来正是"为什么这个 Step 什么也没做"能被回答的原因。
//! 它不跨任何副作用边界，所以先落盘再校验没有代价。
//!
//! ★ 表示不可绕过。middleware 只能环绕非授权关注点，**不得**改变授权结论、
//! 跳过或重排任何固定阶段、延长 grant 有效期或抑制 `StepResult` 提交。
//!
//! ## 审批只有一条路径
//!
//! ```text
//! ApprovalRequested (durable)
//!   → await_approval(deadline = min(approval_timeout, 剩余预算))
//!        → Decided  ⇒ 就地继续
//!        → Pending  ⇒ 写 ApprovalTimedOut + checkpoint
//!                     → 释放全部 live resource
//!                     → 终态 RunNeedsUserAction{approval: token}
//! ```
//!
//! **绝不允许无限期阻塞**：阻塞期间 Run 的全部 live resource 驻留内存——
//! provider 连接、OperationOwner、ChildRun、tool lease、文件句柄。桌面场景下
//! 用户离开一晚，等价于句柄与内存泄漏，且与"取消必须收敛"的不变式冲突。

use std::sync::Arc;

use agentrs_contracts::event::EventPayload;
use agentrs_contracts::ids::{
    ApprovalToken, ChangeSetId, Deadline, Digest, ExecutionId, StepId, Timestamp, ToolCallId,
};
use agentrs_contracts::policy::{
    ApprovalOutcome, ApprovalRequest, DenyCode, InputHash, PolicyDecision, SandboxGrant, ToolProposal,
};
use agentrs_contracts::ports::{
    HookEvaluator, HookOutcome, HookPayload, HookPoint, PolicyEnforcer, SandboxExecutor,
};
use agentrs_contracts::sandbox::{ExecutionOutcome, ExecutionRequest, IsolationLevel};
use agentrs_contracts::{StepIntent, StepOutcome as StepResultOutcome, StepResult};
use agentrs_types::ToolDef;

/// 模型提出的一次调用（已通过 schema 校验）。
#[derive(Debug, Clone, PartialEq)]
pub struct ProposedCall {
    /// 调用标识。
    pub call_id: ToolCallId,
    /// 工具名。
    pub tool_name: String,
    /// 规范化后的参数。
    pub arguments: serde_json::Value,
}

/// 工具回合的结局。
#[derive(Debug, Clone, PartialEq)]
pub enum ToolRoundOutcome {
    /// 全部调用已 settlement，可以进入下一个 Step。
    Settled {
        /// 结果，按原始 call order。
        results: Vec<StepResult>,
    },
    /// 审批超时——整个 Run 转入挂起。
    Suspended {
        /// 恢复令牌。凭它继续**同一个** `StepIntent`。
        token: ApprovalToken,
        /// 已落盘、等待恢复后执行的意图。
        intent: Box<StepIntent>,
    },
}

/// 工具回合所需的依赖。
pub struct ToolRoundDeps {
    /// 授权裁决。
    pub policy: Arc<dyn PolicyEnforcer>,
    /// 受限执行。
    pub sandbox: Arc<dyn SandboxExecutor>,
    /// 单调 guard。**只能收紧，不能放宽**（见 [`ToolGuard`]）。
    pub guards: Vec<Arc<dyn ToolGuard>>,
    /// 宿主 Hook。缺省为 `None`，此时视同全部 `Proceed`。
    pub hooks: Option<Arc<dyn HookEvaluator>>,
    /// 本 Run 的工具目录，用于 schema 校验与"没这个工具"的判定。
    ///
    /// 空目录表示**不校验**：装配方没有交出目录时，凭空拒绝一切调用比放过畸形
    /// 参数更糟。引擎恒会交（`with_tools` 之后按权限模式投影过的那一份）。
    pub catalog: Vec<ToolDef>,
}

impl ToolRoundDeps {
    /// 只装配授权与执行两个必需端口，不带 guard 与 hook。
    pub fn minimal(policy: Arc<dyn PolicyEnforcer>, sandbox: Arc<dyn SandboxExecutor>) -> Self {
        Self {
            policy,
            sandbox,
            guards: Vec::new(),
            hooks: None,
            catalog: Vec::new(),
        }
    }

    /// 交出本 Run 的工具目录。
    pub fn with_catalog(mut self, catalog: Vec<ToolDef>) -> Self {
        self.catalog = catalog;
        self
    }

    /// 装上一个 guard。可以叠加；它们**只能让结论更严**（见 [`ToolGuard`]）。
    pub fn with_guard(mut self, guard: Arc<dyn ToolGuard>) -> Self {
        self.guards.push(guard);
        self
    }
}

/// 校验一次调用的参数，返回该回灌给模型的错误消息。
///
/// 两类问题：目录里没有这个工具，或者参数不符合它的 schema。两者都是**模型说了句
/// 不合语法的话**，不是任何一层的策略拒绝——所以结局是 `Failed` 而不是 `Denied`，
/// 让它读到错误自己改正，这正是"工具错误作为结构化结果回灌"这条设计的用途。
fn check_schema(catalog: &[ToolDef], call: &ProposedCall) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    let Some(def) = catalog.iter().find(|tool| tool.name == call.tool_name) else {
        let mut names: Vec<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();
        names.sort_unstable();
        return Some(format!(
            "没有名为 {} 的工具；可用的是：{}",
            call.tool_name,
            names.join("、")
        ));
    };
    let violations = agentrs_types::validate(&def.parameters, &call.arguments);
    (!violations.is_empty()).then(|| {
        format!(
            "{} 的参数不合法——{}",
            call.tool_name,
            agentrs_types::describe_all(&violations)
        )
    })
}

/// 单调 guard。
///
/// **注意它没有 `Allow`。** 这不是遗漏，是内核不变量 15 的类型化表达：
/// 任何可插拔的东西只能让结论更严，不能让结论更松。若 guard 能返回 `Allow`，
/// 一个装错的 guard 就能绕过 Policy——而"装错的插件不该能提权"正是
/// 这套设计要保证的第一件事。
pub trait ToolGuard: Send + Sync {
    /// guard 名称，进入拒绝说明便于定位是**哪个** guard 拦的。
    fn name(&self) -> &str;

    /// 对一次提议表态。
    fn check(&self, call: &ProposedCall) -> GuardVerdict;
}

/// guard 的表态。**只有拒绝与弃权两种。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// 拒绝。
    Deny {
        /// 稳定拒绝码。
        code: DenyCode,
        /// 已脱敏说明。
        message: String,
    },
    /// 不表态。**弃权不等于放行**——最终是否放行只由 Policy 决定。
    Abstain,
}

/// 工具回合的执行上下文。
pub struct ToolRoundCtx {
    /// 所属 Step。
    pub step_id: StepId,
    /// 目标工作区。
    pub workspace_id: String,
    /// 目标 ChangeSet。
    pub change_set_id: ChangeSetId,
    /// 审批截止时刻 = min(approval_timeout, 剩余 ExecutionBudget)。
    pub approval_deadline: Deadline,
    /// 要求的最低隔离级别。
    pub required_isolation: IsolationLevel,
    /// 当前时刻。
    pub now: Timestamp,
}

/// 计算输入指纹。
///
/// 覆盖 `工具名 + 规范化参数 + workspace + change_set`——
/// **同一命令在不同 ChangeSet 上是不同的意图**，reconcile 时不可混淆（架构 §8.2）。
pub fn input_hash(call: &ProposedCall, workspace: &str, change_set: &ChangeSetId) -> InputHash {
    // FNV-1a：确定性即可，此处不需要密码学强度；
    // 真实实现用 blake3，但那是 Phase B 随 ContentStore 一起落地。
    let material = format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        call.tool_name, call.arguments, workspace, change_set
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in material.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    InputHash(Digest::from_hex(format!("{h:016x}")))
}

/// 执行一个调用的固定管线。
///
/// 返回 `Err(token)` 表示审批超时，整个 Run 应转入挂起。
pub async fn execute_call(
    deps: &ToolRoundDeps,
    ctx: &ToolRoundCtx,
    call: &ProposedCall,
    emit: &mut (dyn FnMut(EventPayload) + Send),
) -> Result<(StepIntent, StepResult), (ApprovalToken, Box<StepIntent>)> {
    let out = execute_call_inner(deps, ctx, call, emit).await;
    // ---- ★ 阶段 4：StepResult 提交 ----
    // **单一出口**。此前每条拒绝路径各自 return，结果是 guard/Hook/Policy
    // 拒掉的调用有 StepIntent 却没有 StepResult——投影会把它们全部误判成
    // "有意图无结果"，恢复时逐个去 reconcile 一件根本没发生的事。
    if let Ok((_, result)) = &out {
        emit(EventPayload::StepResultRecorded {
            result: Box::new(result.clone()),
        });
    }
    out
}

/// 并发执行**一个批次**内的全部调用。
///
/// 返回项按 `indices` 的顺序排列——与完成顺序无关。这是并发执行与
/// 事件顺序之间的解耦点：谁先跑完不影响事实流里的事件序，
/// 否则同一份历史重放会得到不同的事件序列，恢复的确定性就没了。
///
/// 事件不在这里落盘：`execute_call` 保持纯粹，由调用方负责写入事实流。
///
/// **引擎与测试走同一条实现**——两处各写一份调度循环迟早会漂移。
pub async fn execute_batch(
    deps: &ToolRoundDeps,
    ctx: &ToolRoundCtx,
    calls: &[ProposedCall],
    indices: &[usize],
) -> Vec<SettledCall> {
    futures::future::join_all(indices.iter().map(|&i| {
        let call = &calls[i];
        async move {
            let mut events = Vec::new();
            let outcome = execute_call(deps, ctx, call, &mut |e| events.push(e)).await;
            SettledCall {
                index: i,
                events,
                outcome,
            }
        }
    }))
    .await
}

/// 一个已 settlement 的调用。
pub struct SettledCall {
    /// 在原始 proposal 序列中的下标。
    pub index: usize,
    /// 待写入事实流的事件，按产生顺序。
    pub events: Vec<EventPayload>,
    /// 结局；`Err` 表示审批超时。
    pub outcome: Result<(StepIntent, StepResult), (ApprovalToken, Box<StepIntent>)>,
}

async fn execute_call_inner(
    deps: &ToolRoundDeps,
    ctx: &ToolRoundCtx,
    call: &ProposedCall,
    emit: &mut (dyn FnMut(EventPayload) + Send),
) -> Result<(StepIntent, StepResult), (ApprovalToken, Box<StepIntent>)> {
    let hash = input_hash(call, &ctx.workspace_id, &ctx.change_set_id);
    let execution_id = ExecutionId::new(format!("{}-{}", ctx.step_id, call.call_id));

    let proposal = ToolProposal {
        step_id: ctx.step_id.clone(),
        call_id: call.call_id.clone(),
        tool_name: call.tool_name.clone(),
        arguments: call.arguments.clone(),
        workspace_id: ctx.workspace_id.clone(),
        change_set_id: ctx.change_set_id.clone(),
        input_hash: hash.clone(),
    };

    emit(EventPayload::ToolProposed {
        call_id: call.call_id.clone(),
    });

    // ---- ★ 阶段 1：StepIntent 必须先落盘 ----
    // 跨出任何有副作用的调用边界之前。恢复时凭它 reconcile，而不是盲目重跑。
    let intent = StepIntent {
        step_id: ctx.step_id.clone(),
        call_id: call.call_id.clone(),
        tool_name: call.tool_name.clone(),
        input_hash: hash,
        change_set_id: ctx.change_set_id.clone(),
        execution_id: execution_id.clone(),
        at: ctx.now,
    };
    emit(EventPayload::StepIntentRecorded {
        intent: Box::new(intent.clone()),
    });

    // ---- 收紧点 A′：schema 校验 ----
    // 管线的第一道关口。放在 intent 之后是为了保住单一出口（见模块文档）。
    if let Some(message) = check_schema(&deps.catalog, call) {
        return Ok((
            intent.clone(),
            StepResult {
                step_id: ctx.step_id.clone(),
                call_id: call.call_id.clone(),
                outcome: StepResultOutcome::Failed { message },
                effective_isolation: None,
                artifacts: Vec::new(),
                output: None,
                at: ctx.now,
            },
        ));
    }

    // ---- 收紧点 A：单调 guard ----
    // 在 Policy 之前。任一 guard 拒绝即拒绝，**不做投票、不做多数决**——
    // 单调性的含义就是"任何一票否决都算数"。
    for g in &deps.guards {
        if let GuardVerdict::Deny { code, message } = g.check(call) {
            return Ok((
                intent.clone(),
                denied(ctx, call, code, &format!("[{}] {message}", g.name())),
            ));
        }
    }

    // ---- 收紧点 B：PreToolUse Hook ----
    // Hook 只有 Proceed/Advise/Block。求值失败按 Proceed 处理——
    // 宿主 hook 故障不该锁死整个 Run，但这一点必须留痕（HookOutcomeRecorded）。
    let mut advice: Vec<String> = Vec::new();
    if let Some(hooks) = &deps.hooks {
        let payload = HookPayload {
            call_id: Some(call.call_id.clone()),
            data: serde_json::json!({
                "tool_name": call.tool_name,
                "arguments": call.arguments,
            }),
        };
        let outcome = hooks
            .evaluate(HookPoint::PreToolUse, payload)
            .await
            .unwrap_or(HookOutcome::Proceed);
        emit(EventPayload::HookOutcomeRecorded {
            call_id: call.call_id.clone(),
        });
        match outcome {
            HookOutcome::Proceed => {}
            // **建议不改变授权结论**，只附在回灌文本上。
            HookOutcome::Advise(text) => advice.push(text),
            HookOutcome::Block { code, message } => {
                return Ok((intent.clone(), denied(ctx, call, code, &message)));
            }
        }
    }

    // ---- ★ 阶段 2：Policy 裁决 ----
    let decision = match deps.policy.evaluate(proposal.clone()).await {
        Ok(d) => d,
        Err(_) => {
            return Ok((
                intent.clone(),
                denied(ctx, call, DenyCode::OutOfAuthority, "policy unavailable"),
            ))
        }
    };

    let grant: SandboxGrant = match decision {
        PolicyDecision::Deny { code, message } => {
            // 结构化回灌模型，不是异常。
            return Ok((intent.clone(), denied(ctx, call, code, &message)));
        }
        PolicyDecision::Allow {
            grant,
            bound_input_hash,
            expires_at,
        } => {
            // **grant 与本次意图必须严格绑定。**
            // Sandbox 会独立复核同一条（H1），这里是纵深防御的第一道：
            // 若两侧只留一道，一个装错的 Policy 就能签出可复用的通行证。
            if bound_input_hash != intent.input_hash {
                return Ok((
                    intent.clone(),
                    denied(ctx, call, DenyCode::GuardDenied, "grant_input_mismatch"),
                ));
            }
            // 过期未执行则作废，必须重新裁决——不允许"顺手用一下"。
            if expires_at.0 <= ctx.now.0 {
                return Ok((
                    intent.clone(),
                    denied(ctx, call, DenyCode::GuardDenied, "grant_expired"),
                ));
            }
            grant
        }
        PolicyDecision::RequireApproval(req) => {
            emit(EventPayload::ApprovalRequested {
                call_id: call.call_id.clone(),
            });
            match wait_approval(deps, ctx, req).await {
                Approval::Allowed(g) => g,
                Approval::Rejected => {
                    return Ok((
                        intent.clone(),
                        denied(ctx, call, DenyCode::UserRejected, "user rejected"),
                    ))
                }
                Approval::Suspend(token) => {
                    // **有界等待到期**：写 ApprovalTimedOut，整个 Run 转挂起。
                    emit(EventPayload::ApprovalTimedOut {
                        token: token.clone(),
                        call_id: call.call_id.clone(),
                    });
                    return Err((token, Box::new(intent)));
                }
            }
        }
    };

    // ---- ★ 阶段 3：Sandbox 执行 ----
    emit(EventPayload::ToolStarted {
        call_id: call.call_id.clone(),
    });
    let request = ExecutionRequest {
        execution_id,
        tool_name: call.tool_name.clone(),
        arguments: call.arguments.clone(),
        change_set_id: ctx.change_set_id.clone(),
        input_hash: intent.input_hash.clone(),
        required_isolation: ctx.required_isolation,
    };

    let result = match deps.sandbox.execute(grant, request).await {
        Ok(r) => match r.outcome {
            ExecutionOutcome::Completed { exit_code: 0 } => StepResult {
                step_id: ctx.step_id.clone(),
                call_id: call.call_id.clone(),
                outcome: StepResultOutcome::Succeeded,
                effective_isolation: Some(r.effective_isolation),
                artifacts: r.artifacts,
                output: r.output,
                at: ctx.now,
            },
            // 失败时**必须把工具自己的话带上**。从前这里只写 `exit_code=1`，
            // 于是"old 在 a.md 里命中 2 处，请补足上下文"这类话在到达模型之前
            // 就被扔了——模型看到的是一个数字，只能原样重试同一个错误。
            // 退出码留在末尾：它是稳定的、可比对的，而说明是给人和模型看的。
            ExecutionOutcome::Completed { exit_code } => StepResult {
                step_id: ctx.step_id.clone(),
                call_id: call.call_id.clone(),
                outcome: StepResultOutcome::Failed {
                    message: match r.output.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                        Some(said) => format!("{said}（exit_code={exit_code}）"),
                        None => format!("exit_code={exit_code}"),
                    },
                },
                effective_isolation: Some(r.effective_isolation),
                artifacts: r.artifacts,
                output: r.output,
                at: ctx.now,
            },
            ExecutionOutcome::Canceled => StepResult {
                step_id: ctx.step_id.clone(),
                call_id: call.call_id.clone(),
                outcome: StepResultOutcome::Canceled,
                effective_isolation: Some(r.effective_isolation),
                artifacts: vec![],
                output: None,
                at: ctx.now,
            },
            // 超时也要把工具的话与**已经产出的那部分输出**带上。从前这里写死
            // `timed_out` 并把 output 丢掉，于是一条打印了五百行然后卡住的构建，
            // 模型只看得到"timed_out"——恰恰看不到它卡在哪一步。
            ExecutionOutcome::TimedOut => StepResult {
                step_id: ctx.step_id.clone(),
                call_id: call.call_id.clone(),
                outcome: StepResultOutcome::Failed {
                    message: match r.output.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                        Some(said) => format!("{said}（timed_out）"),
                        None => "timed_out".into(),
                    },
                },
                effective_isolation: Some(r.effective_isolation),
                artifacts: r.artifacts,
                output: r.output,
                at: ctx.now,
            },
            // 拒绝同理。`{reason:?}` 只是一个稳定码；执行器往往还附了一句说明
            // （"本机无法兑现 L0 基础围栏，缺：默认断网"），而那句才告诉模型
            // 这条路走不通是因为什么、要不要换条路。
            ExecutionOutcome::Rejected { reason } => StepResult {
                step_id: ctx.step_id.clone(),
                call_id: call.call_id.clone(),
                outcome: StepResultOutcome::Denied {
                    code: DenyCode::GuardDenied,
                    message: match r.output.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                        Some(said) => format!("{said}（{reason:?}）"),
                        None => format!("{reason:?}"),
                    },
                },
                effective_isolation: Some(r.effective_isolation),
                artifacts: vec![],
                output: None,
                at: ctx.now,
            },
        },
        // 执行器不可达 ≠ 未执行。**不得在此重试**——恢复时先 reconcile。
        Err(_) => StepResult {
            step_id: ctx.step_id.clone(),
            call_id: call.call_id.clone(),
            outcome: StepResultOutcome::Failed {
                message: "sandbox_unavailable".into(),
            },
            effective_isolation: None,
            artifacts: vec![],
            output: None,
            at: ctx.now,
        },
    };

    // ---- 收紧点 C：PostToolUse Hook ----
    // **纯建议**。副作用已经发生，此时再"否决"是自欺——
    // 因此这里只允许追加文本，`Block` 与 `Proceed` 同等对待且照样留痕。
    let mut result = result;
    if let Some(hooks) = &deps.hooks {
        let payload = HookPayload {
            call_id: Some(call.call_id.clone()),
            data: serde_json::json!({"tool_name": call.tool_name}),
        };
        if let Ok(outcome) = hooks.evaluate(HookPoint::PostToolUse, payload).await {
            emit(EventPayload::HookOutcomeRecorded {
                call_id: call.call_id.clone(),
            });
            if let HookOutcome::Advise(text) = outcome {
                advice.push(text);
            }
        }
    }

    // 建议附在回灌文本上，不改变 outcome。
    if !advice.is_empty() {
        let base = result.output.clone().unwrap_or_default();
        result.output = Some(format!("{base}\n[hook] {}", advice.join("\n[hook] ")));
    }

    Ok((intent, result))
}

enum Approval {
    Allowed(SandboxGrant),
    Rejected,
    Suspend(ApprovalToken),
}

async fn wait_approval(deps: &ToolRoundDeps, ctx: &ToolRoundCtx, req: ApprovalRequest) -> Approval {
    match deps.policy.await_approval(req, ctx.approval_deadline).await {
        Ok(ApprovalOutcome::Decided(d)) if d.allowed => match d.grant {
            Some(g) => Approval::Allowed(g),
            // 放行却没给 grant 是 Core 的错误；按拒绝处理比按放行处理安全。
            None => Approval::Rejected,
        },
        Ok(ApprovalOutcome::Decided(_)) => Approval::Rejected,
        Ok(ApprovalOutcome::Pending { resume_token }) => Approval::Suspend(resume_token),
        Err(_) => Approval::Rejected,
    }
}

fn denied(ctx: &ToolRoundCtx, call: &ProposedCall, code: DenyCode, message: &str) -> StepResult {
    StepResult {
        step_id: ctx.step_id.clone(),
        call_id: call.call_id.clone(),
        outcome: StepResultOutcome::Denied {
            code,
            message: message.to_string(),
        },
        effective_isolation: None,
        artifacts: vec![],
        output: None,
        at: ctx.now,
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::policy::{ApprovalDecision, DecisionSource};
    use agentrs_testkit::{FakePolicy, FakeSandbox, ScriptedExecution};

    use super::*;

    pub(super) fn 上下文() -> ToolRoundCtx {
        ToolRoundCtx {
            step_id: "s1".into(),
            workspace_id: "ws".into(),
            change_set_id: "cs1".into(),
            approval_deadline: Deadline(Timestamp(60_000)),
            required_isolation: IsolationLevel::L0BasicContainment,
            now: Timestamp(0),
        }
    }

    pub(super) fn 调用() -> ProposedCall {
        ProposedCall {
            call_id: "c1".into(),
            tool_name: "Write".into(),
            arguments: serde_json::json!({"path": "a.rs"}),
        }
    }

    /// 装配一个 grant 与 sandbox 绑定值一致的依赖组合。
    pub(super) fn 依赖(policy: FakePolicy) -> (ToolRoundDeps, Arc<FakeSandbox>) {
        let ctx = 上下文();
        let call = 调用();
        let hash = input_hash(&call, &ctx.workspace_id, &ctx.change_set_id);

        let sandbox = Arc::new(FakeSandbox::new());
        // FakePolicy 默认签发 grant-1；Sandbox 侧独立登记同一绑定值（H1）。
        sandbox.issue_grant("grant-1", hash.clone(), Timestamp(i64::MAX));
        sandbox.issue_grant("g-approved", hash, Timestamp(i64::MAX));

        (ToolRoundDeps::minimal(Arc::new(policy), sandbox.clone()), sandbox)
    }

    fn 目录() -> Vec<ToolDef> {
        vec![
            ToolDef::read_only(
                "Read",
                "读文件",
                serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                    "additionalProperties": false
                }),
            ),
            ToolDef::mutating("Write", "写文件", serde_json::json!({"type": "object"})),
        ]
    }

    #[tokio::test]
    async fn 参数不合法在跨出副作用边界之前就被挡下() {
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.catalog = 目录();
        let mut call = 调用();
        call.tool_name = "Read".into();
        call.arguments = serde_json::json!({});
        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &call, &mut |e| events.push(e))
            .await
            .expect("不是审批挂起");

        // 结局是 Failed 而不是 Denied：模型说了句不合语法的话，不是任何一层拒绝了它。
        match result.outcome {
            StepResultOutcome::Failed { ref message } => {
                assert!(message.contains("path"), "{message}");
                assert!(message.contains("必填"), "{message}");
            }
            other => panic!("期望 Failed，得到 {other:?}"),
        }
        assert_eq!(sandbox.execution_count(), 0, "畸形调用不该跨出副作用边界");
    }

    #[tokio::test]
    async fn 畸形调用仍然留下意图与结果这一对() {
        // 单一出口：任何拒绝路径都必须有 StepResult 与 StepIntent 配对，
        // 否则投影会把它误判成"有意图无结果"、恢复时去 reconcile 一件没发生的事。
        let (mut deps, _) = 依赖(FakePolicy::allow_all());
        deps.catalog = 目录();
        let mut call = 调用();
        call.tool_name = "Read".into();
        call.arguments = serde_json::json!({"path": 7});
        let mut events = Vec::new();
        execute_call(&deps, &上下文(), &call, &mut |e| events.push(e))
            .await
            .expect("不是审批挂起");
        let 类型: Vec<&str> = events
            .iter()
            .map(|e| match e {
                EventPayload::ToolProposed { .. } => "proposed",
                EventPayload::StepIntentRecorded { .. } => "intent",
                EventPayload::StepResultRecorded { .. } => "result",
                _ => "other",
            })
            .collect();
        assert_eq!(类型, vec!["proposed", "intent", "result"]);
    }

    #[tokio::test]
    async fn 目录里没有的工具当场说清楚可用的是哪些() {
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.catalog = 目录();
        let mut call = 调用();
        call.tool_name = "Teleport".into();
        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &call, &mut |e| events.push(e))
            .await
            .expect("不是审批挂起");
        match result.outcome {
            StepResultOutcome::Failed { ref message } => {
                assert!(message.contains("Teleport"), "{message}");
                assert!(message.contains("Read"), "要告诉它有什么可用：{message}");
            }
            other => panic!("期望 Failed，得到 {other:?}"),
        }
        assert_eq!(sandbox.execution_count(), 0);
    }

    #[tokio::test]
    async fn 空目录表示不校验() {
        // 装配方没交出目录时，凭空拒绝一切调用比放过畸形参数更糟。
        let (deps, sandbox) = 依赖(FakePolicy::allow_all());
        assert!(deps.catalog.is_empty());
        // 这个调用的工具名不在任何目录里；目录为空时它照样应当执行。
        let mut events = Vec::new();
        execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .expect("不是审批挂起");
        assert_eq!(sandbox.execution_count(), 1);
    }

    #[test]
    fn 输入指纹覆盖_change_set() {
        // 同一命令在不同 ChangeSet 上是不同的意图。
        let c = 调用();
        let a = input_hash(&c, "ws", &"cs1".into());
        let b = input_hash(&c, "ws", &"cs2".into());
        assert_ne!(a, b, "change_set 必须参与指纹");
    }

    #[test]
    fn 输入指纹覆盖工作区() {
        let c = 调用();
        assert_ne!(
            input_hash(&c, "ws1", &"cs".into()),
            input_hash(&c, "ws2", &"cs".into())
        );
    }

    #[tokio::test]
    async fn 放行路径按固定阶段顺序推进() {
        let (deps, sandbox) = 依赖(FakePolicy::allow_all());
        let mut events = Vec::new();
        let (intent, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .unwrap();

        assert_eq!(
            events,
            vec![
                EventPayload::ToolProposed { call_id: "c1".into() },
                EventPayload::StepIntentRecorded {
                    intent: Box::new(intent.clone()),
                },
                EventPayload::ToolStarted { call_id: "c1".into() },
                EventPayload::StepResultRecorded {
                    result: Box::new(result.clone()),
                },
            ],
            "固定阶段顺序不可变"
        );
        assert_eq!(result.outcome, StepResultOutcome::Succeeded);
        assert_eq!(intent.call_id, "c1".into());
        assert_eq!(sandbox.execution_count(), 1);
    }

    #[tokio::test]
    async fn step_intent_在_policy_之前落盘() {
        // 这是内核不变量 3：跨出有副作用的调用边界之前必须先有意图。
        // 即便 Policy 随后拒绝，意图也已经记录——恢复时才知道"曾经想做什么"。
        let (deps, _) = 依赖(FakePolicy::deny_all());
        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .unwrap();

        let intent_pos = events
            .iter()
            .position(|e| matches!(e, EventPayload::StepIntentRecorded { .. }));
        assert_eq!(intent_pos, Some(1), "意图必须紧跟提议，先于任何裁决");
        assert!(matches!(result.outcome, StepResultOutcome::Denied { .. }));
    }

    #[tokio::test]
    async fn 工具失败时它自己的话要带到模型面前() {
        // 从前这里只写 exit_code=1，于是"命中 2 处，请补足上下文"这类唯一能指导
        // 改正的信息在到达模型之前就被扔了。
        let (deps, sandbox) = 依赖(FakePolicy::allow_all());
        sandbox.script(ScriptedExecution::Succeed {
            exit_code: 1,
            output: Some("old 在 a.md 里命中 2 处，必须唯一".into()),
        });
        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .expect("不是审批挂起");
        match result.outcome {
            StepResultOutcome::Failed { ref message } => {
                assert!(message.contains("命中 2 处"), "{message}");
                assert!(message.contains("exit_code=1"), "稳定码也要留着：{message}");
            }
            other => panic!("期望 Failed，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn 超时也要带上被杀之前那部分输出() {
        // 与上一条同源，只是走的是另一条分支——而那条分支从前写死 `timed_out`
        // 并把 output 整个丢掉。一条打印了五百行然后卡住的构建，模型于是
        // 只看得到 "timed_out"，恰恰看不到它卡在哪一步。
        let (deps, sandbox) = 依赖(FakePolicy::allow_all());
        sandbox.script(ScriptedExecution::TimeOut {
            output: Some("Compiling serde v1.0.0\nCompiling tokio v1.53.1".into()),
        });
        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .expect("不是审批挂起");
        let StepResultOutcome::Failed { ref message } = result.outcome else {
            panic!("期望 Failed，得到 {:?}", result.outcome);
        };
        assert!(message.contains("Compiling tokio"), "{message}");
        assert!(message.contains("timed_out"), "稳定码也要留着：{message}");
        // artifacts/output 同样不能丢：卡住之前的产物照样是产物。
        assert!(result.output.is_some());
    }

    #[tokio::test]
    async fn 没有输出时超时仍只报稳定码() {
        let (deps, sandbox) = 依赖(FakePolicy::allow_all());
        sandbox.script(ScriptedExecution::TimeOut { output: None });
        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .expect("不是审批挂起");
        assert_eq!(
            result.outcome,
            StepResultOutcome::Failed {
                message: "timed_out".into()
            }
        );
    }

    #[tokio::test]
    async fn 拒绝是结构化结果不进入执行() {
        let (deps, sandbox) = 依赖(FakePolicy::deny_all());
        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .unwrap();

        assert!(matches!(result.outcome, StepResultOutcome::Denied { .. }));
        assert_eq!(sandbox.execution_count(), 0, "被拒的调用不得触达执行器");
        assert!(!events
            .iter()
            .any(|e| matches!(e, EventPayload::ToolStarted { .. })));
    }

    #[tokio::test]
    async fn 审批通过后继续执行() {
        let policy = FakePolicy::allow_all();
        let ctx = 上下文();
        let call = 调用();
        let hash = input_hash(&call, &ctx.workspace_id, &ctx.change_set_id);
        policy.script(PolicyDecision::RequireApproval(ApprovalRequest {
            step_id: "s1".into(),
            proposal: ToolProposal {
                step_id: "s1".into(),
                call_id: "c1".into(),
                tool_name: "Write".into(),
                arguments: serde_json::json!({}),
                workspace_id: "ws".into(),
                change_set_id: "cs1".into(),
                input_hash: hash.clone(),
            },
            risk_summary: "写文件".into(),
            originating_member: None,
            team_id: None,
        }));
        policy.script_approval(ApprovalOutcome::Decided(ApprovalDecision {
            allowed: true,
            source: DecisionSource::Human { user_id: "u1".into() },
            grant: Some(SandboxGrant {
                grant_id: "g-approved".into(),
                payload: serde_json::json!({}),
            }),
            decided_at: Timestamp(1),
        }));

        let sandbox = Arc::new(FakeSandbox::new());
        sandbox.issue_grant("g-approved", hash, Timestamp(i64::MAX));
        let deps = ToolRoundDeps::minimal(Arc::new(policy), sandbox.clone());

        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &ctx, &call, &mut |e| events.push(e))
            .await
            .unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, EventPayload::ApprovalRequested { .. })));
        assert_eq!(result.outcome, StepResultOutcome::Succeeded);
        assert_eq!(sandbox.execution_count(), 1);
    }

    #[tokio::test]
    async fn 审批超时转挂起并携带令牌与意图() {
        // FakePolicy 无审批脚本时默认返回 Pending —— 安全默认值。
        let policy = FakePolicy::allow_all();
        let ctx = 上下文();
        let call = 调用();
        let hash = input_hash(&call, &ctx.workspace_id, &ctx.change_set_id);
        policy.script(PolicyDecision::RequireApproval(ApprovalRequest {
            step_id: "s1".into(),
            proposal: ToolProposal {
                step_id: "s1".into(),
                call_id: "c1".into(),
                tool_name: "Write".into(),
                arguments: serde_json::json!({}),
                workspace_id: "ws".into(),
                change_set_id: "cs1".into(),
                input_hash: hash,
            },
            risk_summary: "写文件".into(),
            originating_member: None,
            team_id: None,
        }));

        let sandbox = Arc::new(FakeSandbox::new());
        let deps = ToolRoundDeps::minimal(Arc::new(policy), sandbox.clone());

        let mut events = Vec::new();
        let err = execute_call(&deps, &ctx, &call, &mut |e| events.push(e))
            .await
            .unwrap_err();

        let (token, intent) = err;
        assert_eq!(token.as_str(), "fake-token");
        assert_eq!(intent.call_id, "c1".into(), "意图随挂起一并保留");
        assert!(events
            .iter()
            .any(|e| matches!(e, EventPayload::ApprovalTimedOut { .. })));
        assert_eq!(sandbox.execution_count(), 0, "挂起时尚未执行");
    }

    #[tokio::test]
    async fn 放行却不给_grant_按拒绝处理() {
        // Core 的错误。按拒绝处理比按放行处理安全得多。
        let policy = FakePolicy::allow_all();
        let ctx = 上下文();
        let call = 调用();
        let hash = input_hash(&call, &ctx.workspace_id, &ctx.change_set_id);
        policy.script(PolicyDecision::RequireApproval(ApprovalRequest {
            step_id: "s1".into(),
            proposal: ToolProposal {
                step_id: "s1".into(),
                call_id: "c1".into(),
                tool_name: "Write".into(),
                arguments: serde_json::json!({}),
                workspace_id: "ws".into(),
                change_set_id: "cs1".into(),
                input_hash: hash,
            },
            risk_summary: "".into(),
            originating_member: None,
            team_id: None,
        }));
        policy.script_approval(ApprovalOutcome::Decided(ApprovalDecision {
            allowed: true,
            source: DecisionSource::Human { user_id: "u1".into() },
            grant: None, // ← 放行但没给 grant
            decided_at: Timestamp(1),
        }));

        let deps = ToolRoundDeps::minimal(Arc::new(policy), Arc::new(FakeSandbox::new()));
        let (_, result) = execute_call(&deps, &ctx, &call, &mut |_| {}).await.unwrap();
        assert!(matches!(
            result.outcome,
            StepResultOutcome::Denied {
                code: DenyCode::UserRejected,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn 执行器不可达不重试而是记为失败() {
        // "不可达 ≠ 未执行"。重试可能造成重复副作用；
        // 正确做法是记录失败，恢复时先 reconcile。
        let (deps, sandbox) = 依赖(FakePolicy::allow_all());
        sandbox.script(ScriptedExecution::CrashThen(
            agentrs_contracts::sandbox::ExecutionStatus::Unknown,
        ));

        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert!(matches!(result.outcome, StepResultOutcome::Failed { .. }));
        assert_eq!(sandbox.execution_count(), 1, "只执行了一次，未重试");
    }

    #[tokio::test]
    async fn 结果携带实际生效的隔离级别() {
        let (deps, _) = 依赖(FakePolicy::allow_all());
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(
            result.effective_isolation,
            Some(IsolationLevel::L0BasicContainment)
        );
    }
}

#[cfg(test)]
mod tightening_tests {
    //! 收紧点的语义：guard 与 Hook **只能收紧，不能放宽**（内核不变量 15）。

    use agentrs_contracts::ports::HookOutcome;
    use agentrs_testkit::{FakeHooks, FakePolicy, FakeSandbox};

    use super::tests::{上下文, 依赖, 调用};
    use super::*;

    /// 恒拒的 guard。
    struct 恒拒(&'static str);
    impl ToolGuard for 恒拒 {
        fn name(&self) -> &str {
            self.0
        }
        fn check(&self, _c: &ProposedCall) -> GuardVerdict {
            GuardVerdict::Deny {
                code: DenyCode::GuardDenied,
                message: "挡下".into(),
            }
        }
    }

    /// 恒弃权的 guard。
    struct 恒弃权;
    impl ToolGuard for 恒弃权 {
        fn name(&self) -> &str {
            "abstain"
        }
        fn check(&self, _c: &ProposedCall) -> GuardVerdict {
            GuardVerdict::Abstain
        }
    }

    #[tokio::test]
    async fn 单个_guard_拒绝即拒绝且不触达执行器() {
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.guards = vec![Arc::new(恒拒("policy-lint"))];

        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .unwrap();

        match result.outcome {
            StepResultOutcome::Denied { code, message } => {
                assert_eq!(code, DenyCode::GuardDenied);
                // 说明里要能看出是**哪个** guard 拦的。
                assert!(message.contains("policy-lint"), "缺少 guard 名：{message}");
            }
            other => panic!("期望 Denied，得到 {other:?}"),
        }
        assert_eq!(sandbox.execution_count(), 0);
    }

    #[tokio::test]
    async fn 全体弃权不等于放行仍由_policy_决定() {
        // 弃权是"不表态"，不是"我批准"。这里让 Policy 拒绝，结果必须是拒绝。
        let (mut deps, sandbox) = 依赖(FakePolicy::deny_all());
        deps.guards = vec![Arc::new(恒弃权), Arc::new(恒弃权)];

        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert!(matches!(result.outcome, StepResultOutcome::Denied { .. }));
        assert_eq!(sandbox.execution_count(), 0);
    }

    #[tokio::test]
    async fn 一票否决压过其余弃权() {
        // 单调性 = 任何一票否决都算数，**不做多数决**。
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.guards = vec![Arc::new(恒弃权), Arc::new(恒拒("第二个")), Arc::new(恒弃权)];

        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert!(matches!(result.outcome, StepResultOutcome::Denied { .. }));
        assert_eq!(sandbox.execution_count(), 0);
    }

    #[tokio::test]
    async fn guard_在_policy_之前但在意图落盘之后() {
        // 顺序有意为之：被 guard 拦下的调用也必须留下"曾经想做什么"。
        let (mut deps, _) = 依赖(FakePolicy::allow_all());
        deps.guards = vec![Arc::new(恒拒("g"))];

        let mut events = Vec::new();
        execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .unwrap();
        assert!(matches!(events[1], EventPayload::StepIntentRecorded { .. }));
        assert!(!events
            .iter()
            .any(|e| matches!(e, EventPayload::ToolStarted { .. })));
    }

    #[tokio::test]
    async fn hook_block_等价于一次拒绝() {
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.hooks = Some(Arc::new(FakeHooks::proceed().with_pre(HookOutcome::Block {
            code: DenyCode::HookBlocked,
            message: "禁止写这个路径".into(),
        })));

        let mut events = Vec::new();
        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
            .await
            .unwrap();

        assert!(matches!(
            result.outcome,
            StepResultOutcome::Denied {
                code: DenyCode::HookBlocked,
                ..
            }
        ));
        assert_eq!(sandbox.execution_count(), 0);
        // 否决也要留痕，否则"为什么没执行"无从查起。
        assert!(events
            .iter()
            .any(|e| matches!(e, EventPayload::HookOutcomeRecorded { .. })));
        // 仍然写了 StepResult——不是"什么都没发生"。
        assert!(events
            .iter()
            .any(|e| matches!(e, EventPayload::StepResultRecorded { .. })));
    }

    #[tokio::test]
    async fn hook_advise_不改变授权结论只附在回灌文本上() {
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.hooks = Some(Arc::new(
            FakeHooks::proceed().with_pre(HookOutcome::Advise("注意备份".into())),
        ));

        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(result.outcome, StepResultOutcome::Succeeded, "建议不该改变结论");
        assert_eq!(sandbox.execution_count(), 1);
        assert!(result.output.unwrap_or_default().contains("注意备份"));
    }

    #[tokio::test]
    async fn hook_求值失败按_proceed_处理() {
        // 宿主 hook 故障不该锁死整个 Run，也不该放大成 Run 失败。
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.hooks = Some(Arc::new(FakeHooks::proceed().failing()));

        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(result.outcome, StepResultOutcome::Succeeded);
        assert_eq!(sandbox.execution_count(), 1);
    }

    #[tokio::test]
    async fn 事后_hook_的_block_不能翻案() {
        // 副作用已经发生了，此时"否决"是自欺。只允许追加文本。
        let (mut deps, sandbox) = 依赖(FakePolicy::allow_all());
        deps.hooks = Some(Arc::new(FakeHooks::proceed().with_post(HookOutcome::Block {
            code: DenyCode::HookBlocked,
            message: "早说啊".into(),
        })));

        let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(
            result.outcome,
            StepResultOutcome::Succeeded,
            "PostToolUse 不得把已成功的执行改判为拒绝"
        );
        assert_eq!(sandbox.execution_count(), 1);
    }

    #[tokio::test]
    async fn 被拒的调用不会走到事后_hook() {
        let (mut deps, _) = 依赖(FakePolicy::deny_all());
        let hooks = Arc::new(FakeHooks::proceed());
        deps.hooks = Some(hooks.clone());

        execute_call(&deps, &上下文(), &调用(), &mut |_| {})
            .await
            .unwrap();
        assert_eq!(
            hooks.calls(),
            [agentrs_contracts::ports::HookPoint::PreToolUse],
            "只求值了 PreToolUse"
        );
    }

    #[tokio::test]
    async fn grant_绑定的指纹不匹配时拒绝执行() {
        // 纵深防御第一道。Sandbox 会独立复核同一条（H1）；
        // 只留一道的话，一个装错的 Policy 就能签出可复用的通行证。
        let ctx = 上下文();
        let call = 调用();
        let policy = FakePolicy::allow_all();
        policy.script(PolicyDecision::Allow {
            grant: SandboxGrant {
                grant_id: "grant-1".into(),
                payload: serde_json::json!({}),
            },
            // ← 绑到了**别的**输入上
            bound_input_hash: input_hash(&call, "别的工作区", &ctx.change_set_id),
            expires_at: Timestamp(i64::MAX),
        });

        let sandbox = Arc::new(FakeSandbox::new());
        let deps = ToolRoundDeps::minimal(Arc::new(policy), sandbox.clone());
        let (_, result) = execute_call(&deps, &ctx, &call, &mut |_| {}).await.unwrap();

        match result.outcome {
            StepResultOutcome::Denied { message, .. } => {
                assert_eq!(message, "grant_input_mismatch")
            }
            other => panic!("期望 Denied，得到 {other:?}"),
        }
        assert_eq!(sandbox.execution_count(), 0, "不匹配的 grant 不得触达执行器");
    }

    /// 每一条拒绝路径都必须留下 `StepResultRecorded`。
    ///
    /// 少了它，投影里这次调用就是"有意图无结果"——恢复时会去 reconcile
    /// 一件根本没发生的事。这条最初真的漏了，四条路径无一幸免。
    #[tokio::test]
    async fn 每条拒绝路径都写_step_result() {
        /// (路径名, 构造依赖)
        type 拒绝路径 = (&'static str, Box<dyn Fn() -> ToolRoundDeps>);
        let 路径: Vec<拒绝路径> = vec![
            (
                "guard 拒绝",
                Box::new(|| {
                    let (mut d, _) = 依赖(FakePolicy::allow_all());
                    d.guards = vec![Arc::new(恒拒("g"))];
                    d
                }),
            ),
            (
                "Hook 否决",
                Box::new(|| {
                    let (mut d, _) = 依赖(FakePolicy::allow_all());
                    d.hooks = Some(Arc::new(FakeHooks::proceed().with_pre(HookOutcome::Block {
                        code: DenyCode::HookBlocked,
                        message: "no".into(),
                    })));
                    d
                }),
            ),
            ("Policy 拒绝", Box::new(|| 依赖(FakePolicy::deny_all()).0)),
            (
                "grant 指纹不匹配",
                Box::new(|| {
                    let policy = FakePolicy::allow_all();
                    policy.script(PolicyDecision::Allow {
                        grant: SandboxGrant {
                            grant_id: "grant-1".into(),
                            payload: serde_json::json!({}),
                        },
                        bound_input_hash: input_hash(&调用(), "别处", &"cs1".into()),
                        expires_at: Timestamp(i64::MAX),
                    });
                    ToolRoundDeps::minimal(Arc::new(policy), Arc::new(FakeSandbox::new()))
                }),
            ),
        ];

        for (名, 造) in 路径 {
            let deps = 造();
            let mut events = Vec::new();
            let (_, result) = execute_call(&deps, &上下文(), &调用(), &mut |e| events.push(e))
                .await
                .unwrap();
            assert!(
                matches!(result.outcome, StepResultOutcome::Denied { .. }),
                "{名} 应当是 Denied"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, EventPayload::StepResultRecorded { .. })),
                "{名} 缺少 StepResultRecorded —— 投影会把它误判成悬挂意图"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, EventPayload::StepIntentRecorded { .. })),
                "{名} 缺少 StepIntentRecorded"
            );
        }
    }

    #[tokio::test]
    async fn 挂起时不写_step_result() {
        // 挂起意味着这次调用**还没有结局**；此时写 StepResult 等于宣称它结束了，
        // redeem 之后就无法继续同一个意图。
        let policy = FakePolicy::allow_all();
        let ctx = 上下文();
        let call = 调用();
        policy.script(PolicyDecision::RequireApproval(ApprovalRequest {
            step_id: "s1".into(),
            proposal: ToolProposal {
                step_id: "s1".into(),
                call_id: "c1".into(),
                tool_name: "Write".into(),
                arguments: serde_json::json!({}),
                workspace_id: "ws".into(),
                change_set_id: "cs1".into(),
                input_hash: input_hash(&call, &ctx.workspace_id, &ctx.change_set_id),
            },
            risk_summary: "".into(),
            originating_member: None,
            team_id: None,
        }));
        let deps = ToolRoundDeps::minimal(Arc::new(policy), Arc::new(FakeSandbox::new()));

        let mut events = Vec::new();
        execute_call(&deps, &ctx, &call, &mut |e| events.push(e))
            .await
            .unwrap_err();
        assert!(!events
            .iter()
            .any(|e| matches!(e, EventPayload::StepResultRecorded { .. })));
    }

    #[tokio::test]
    async fn 过期的_grant_不得顺手用一下() {
        let ctx = 上下文();
        let call = 调用();
        let policy = FakePolicy::allow_all();
        policy.script(PolicyDecision::Allow {
            grant: SandboxGrant {
                grant_id: "grant-1".into(),
                payload: serde_json::json!({}),
            },
            bound_input_hash: input_hash(&call, &ctx.workspace_id, &ctx.change_set_id),
            // ctx.now = 0，这个 grant 已经过期。
            expires_at: Timestamp(0),
        });

        let sandbox = Arc::new(FakeSandbox::new());
        let deps = ToolRoundDeps::minimal(Arc::new(policy), sandbox.clone());
        let (_, result) = execute_call(&deps, &ctx, &call, &mut |_| {}).await.unwrap();

        match result.outcome {
            StepResultOutcome::Denied { message, .. } => assert_eq!(message, "grant_expired"),
            other => panic!("期望 Denied，得到 {other:?}"),
        }
        assert_eq!(sandbox.execution_count(), 0);
    }
}
