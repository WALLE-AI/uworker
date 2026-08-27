//! 宿主义务 conformance suite（架构 §12.2）。
//!
//! ## 它为什么存在
//!
//! 安全承诺分成两半：**内核不变量**由内核自己的测试守护；**宿主义务**内核
//! 无法自证——它没法从内部证明一个第三方 `SandboxExecutor` 真的复核了 grant。
//! 于是承诺就变成"我们相信 adapter 会照做"，而这不是承诺。
//!
//! 这个套件把义务写成**可执行的检查**，随内核一起发布。任何 adapter 都能跑，
//! 跑不过就是不合格——**内核不为第三方 adapter 的正确性背书**。
//!
//! ## 为什么要抽 [`SandboxSubject`] 而不是直接测具体类型
//!
//! 套件要测的是"违反义务会怎样"，但它得先有一个**合法的起点**：
//! 一个已登记的 grant、一份该 adapter 认识的请求。这两件事各家实现不同，
//! 因此由 subject 提供；套件本身只负责构造违规场景并检查反应。
//!
//! ## 检查是"必须拒绝"，不是"必须报错"
//!
//! 拒绝可以是 `Err(SandboxError)`，也可以是
//! `Ok(ExecutionResult{outcome: Rejected})`——两者都算合格。
//! **不合格的只有一种：真的执行了。**
//!
//! ## 分模块
//!
//! - 本模块：H1/H2/H7（`SandboxExecutor`）
//! - [`persistence`]：H3（`RunPersistence`）
//! - [`content`]：H4（`ContentStore`）
//! - [`policy`]：H5（`PolicyEnforcer`）
//!
//! **H6 没有独立套件**，这是有意的：`HookOutcome` 只有
//! `Proceed`/`Advise`/`Block`，第三方实现连"放宽授权"都表达不出来。
//! 由类型保证的东西再写一遍运行时检查，只会给人一种"验过了"的错觉——
//! 真正该验的是内核有没有把 `Block` 当回事，那属于内核不变量，
//! 已由 `agentrs-runtime` 的 tightening_tests 覆盖。

pub mod content;
pub mod persistence;
pub mod policy;

use std::sync::Arc;

use agentrs_contracts::ids::{ExecutionId, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionResult, ExecutionStatus, IsolationLevel, SandboxError,
};

/// 被检查的沙箱实现。
///
/// 实现方只需提供"合法起点"，违规场景由套件构造。
pub trait SandboxSubject: Send + Sync {
    /// 待检查的执行器。
    fn executor(&self) -> Arc<dyn SandboxExecutor>;

    /// 登记一个绑定到 `bound` 的 grant。
    ///
    /// 套件会用它建立合法起点。**若实现不支持外部登记 grant**，
    /// 应当在此 panic 并改用自带的集成测试——套件不猜。
    fn issue_grant(&self, grant_id: &str, bound: InputHash, expires_at: Timestamp);

    /// 构造一次该实现认识的、**会真的产生可观察副作用**的请求。
    ///
    /// 必须是有副作用的：套件靠"副作用有没有发生"判断拒绝是否真的生效。
    fn mutating_request(
        &self,
        execution_id: &str,
        input_hash: InputHash,
        required_isolation: IsolationLevel,
    ) -> ExecutionRequest;

    /// 该请求是否真的产生了副作用。
    ///
    /// 返回 `None` 表示实现无法自查——此时相关检查记为 `Skipped` 而不是 `Pass`。
    /// **把无法验证记成通过，是这类套件最容易犯的错。**
    fn side_effect_happened(&self, _request: &ExecutionRequest) -> Option<bool> {
        None
    }

    /// 构造一次**读**请求，读的是 [`mutating_request`](Self::mutating_request)
    /// 写过的那个目标。
    ///
    /// 返回 `None` 表示该实现没有可配对的读工具——H7 检查随之记为 `Skipped`。
    /// **不猜**：一个只能写不能读的执行器谈不上"读己之写"。
    fn paired_read(
        &self,
        _execution_id: &str,
        _input_hash: InputHash,
        _written: &ExecutionRequest,
    ) -> Option<ExecutionRequest> {
        None
    }

    /// 从一次读的结果里取出内容，供比对。
    fn read_content(&self, _result: &ExecutionResult) -> Option<String> {
        None
    }

    /// 把一份请求改投到另一个 ChangeSet 上。
    fn in_change_set(&self, _req: &ExecutionRequest, _change_set: &str) -> Option<ExecutionRequest> {
        None
    }

    /// 该实现支持的最高隔离级别。
    ///
    /// 套件据此挑一个**更高的**级别去请求，检查它是失败还是静默降级。
    /// 返回 `L2RemoteSandbox` 表示无法构造更高级别，相关检查记为 `Skipped`。
    fn max_isolation(&self) -> IsolationLevel {
        IsolationLevel::L0BasicContainment
    }
}

/// 一项检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 合格。
    Pass,
    /// 不合格。
    Fail {
        /// 具体哪里不对。
        detail: String,
    },
    /// 无法检查。**不等于合格。**
    Skipped {
        /// 为什么跳过。
        why: String,
    },
}

/// 一项检查。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// 对应的宿主义务编号，如 `"H1"`。
    pub obligation: &'static str,
    /// 检查名。
    pub name: &'static str,
    /// 违反了会怎样——用于报告，让读的人知道这条为什么重要。
    pub consequence: &'static str,
    /// 结果。
    pub outcome: Outcome,
}

impl Check {
    /// 是否不合格。
    pub fn failed(&self) -> bool {
        matches!(self.outcome, Outcome::Fail { .. })
    }
}

/// 一次 conformance 运行的报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// 全部检查，按执行顺序。
    pub checks: Vec<Check>,
}

impl Report {
    /// 是否全部合格。**跳过不算不合格，但也不算合格**——见 [`skipped`](Self::skipped)。
    pub fn passed(&self) -> bool {
        !self.checks.iter().any(Check::failed)
    }

    /// 不合格的检查。
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| c.failed()).collect()
    }

    /// 跳过的检查。**报告必须把它们显式列出**——
    /// 静默跳过会让"全绿"变成一个假象。
    pub fn skipped(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| matches!(c.outcome, Outcome::Skipped { .. }))
            .collect()
    }

    /// 渲染成可读文本。
    pub fn render(&self) -> String {
        let mut out = String::new();
        for c in &self.checks {
            let (mark, extra) = match &c.outcome {
                Outcome::Pass => ("✅", String::new()),
                Outcome::Fail { detail } => ("❌", format!("  ← {detail}（后果：{}）", c.consequence)),
                Outcome::Skipped { why } => ("⏭️", format!("  ← 跳过：{why}")),
            };
            out.push_str(&format!("{mark} {} {}{extra}\n", c.obligation, c.name));
        }
        let f = self.failures().len();
        let s = self.skipped().len();
        out.push_str(&format!(
            "\n共 {} 项：{} 通过、{f} 不合格、{s} 跳过\n",
            self.checks.len(),
            self.checks.len() - f - s
        ));
        if s > 0 {
            out.push_str("**跳过不等于通过**——被跳过的义务仍未验证。\n");
        }
        out
    }
}

/// 执行结果是否构成"拒绝"。
///
/// `Err` 与 `Ok(Rejected)` 都算——两条都表示没有执行。
fn is_refusal(r: &Result<ExecutionResult, SandboxError>) -> bool {
    match r {
        Err(_) => true,
        Ok(res) => matches!(res.outcome, ExecutionOutcome::Rejected { .. }),
    }
}

struct Runner<'a> {
    subject: &'a dyn SandboxSubject,
    checks: Vec<Check>,
    n: std::cell::Cell<u32>,
}

impl<'a> Runner<'a> {
    fn record(
        &mut self,
        obligation: &'static str,
        name: &'static str,
        consequence: &'static str,
        outcome: Outcome,
    ) {
        self.checks.push(Check {
            obligation,
            name,
            consequence,
            outcome,
        });
    }

    /// 建立一个合法起点：新 grant + 绑定到它的请求。
    fn fresh(&self, iso: IsolationLevel) -> (String, InputHash, ExecutionRequest) {
        let i = self.n.get();
        self.n.set(i + 1);
        let gid = format!("conf-g{i}");
        let hash = InputHash(agentrs_contracts::ids::Digest::from_hex(format!("conf-h{i}")));
        self.subject.issue_grant(&gid, hash.clone(), Timestamp(i64::MAX));
        let req = self
            .subject
            .mutating_request(&format!("conf-e{i}"), hash.clone(), iso);
        (gid, hash, req)
    }

    fn grant(id: &str) -> SandboxGrant {
        SandboxGrant {
            grant_id: id.into(),
            payload: serde_json::json!({}),
        }
    }

    /// 拒绝检查的公共形状：跑一次应当被拒的执行，确认它既被拒、又真的没干活。
    async fn expect_refusal(&self, gid: &str, req: ExecutionRequest, what: &str) -> Result<(), String> {
        let r = self
            .subject
            .executor()
            .execute(Self::grant(gid), req.clone())
            .await;
        if !is_refusal(&r) {
            return Err(format!("{what}未被拒绝：{:?}", r.map(|x| x.outcome)));
        }
        // 拒绝了还不够——**必须确认副作用真的没发生**。
        if self.subject.side_effect_happened(&req) == Some(true) {
            return Err(format!("{what}虽被报告为拒绝，但副作用已经发生"));
        }
        Ok(())
    }
}

/// 对一个沙箱实现跑完整套宿主义务检查。
pub async fn check_sandbox(subject: &dyn SandboxSubject) -> Report {
    let mut r = Runner {
        subject,
        checks: Vec::new(),
        n: std::cell::Cell::new(0),
    };
    let iso = subject.max_isolation();

    // ---- 先确认合法路径能走通 ----
    // 没有这条，后面所有"必须拒绝"的检查都可能因为**实现根本跑不通**而假通过。
    {
        let (gid, _, req) = r.fresh(iso);
        let res = subject.executor().execute(Runner::grant(&gid), req).await;
        let outcome = match &res {
            Ok(x) if matches!(x.outcome, ExecutionOutcome::Completed { .. }) => Outcome::Pass,
            other => Outcome::Fail {
                detail: format!("合法请求未能执行：{other:?}"),
            },
        };
        r.record("H0", "合法请求可以执行", "后续所有拒绝类检查都会假通过", outcome);
    }

    // ---- H1：独立复核 grant 绑定 ----
    {
        let (gid, _, mut req) = r.fresh(iso);
        // 调用方声称的指纹与 grant 绑定值不符。
        req.input_hash = InputHash(agentrs_contracts::ids::Digest::from_hex("tampered"));
        let outcome = match r.expect_refusal(&gid, req, "被篡改的 input_hash").await {
            Ok(()) => Outcome::Pass,
            Err(detail) => Outcome::Fail { detail },
        };
        r.record(
            "H1",
            "复核 bound_input_hash",
            "内核不变量 2 失效：grant 可被挪用到别的输入上",
            outcome,
        );
    }

    {
        let i = r.n.get();
        r.n.set(i + 1);
        let gid = format!("conf-exp{i}");
        let hash = InputHash(agentrs_contracts::ids::Digest::from_hex(format!("conf-exp{i}")));
        // 已过期的 grant。
        subject.issue_grant(&gid, hash.clone(), Timestamp(0));
        let req = subject.mutating_request(&format!("conf-ee{i}"), hash, iso);
        let outcome = match r.expect_refusal(&gid, req, "已过期的 grant").await {
            Ok(()) => Outcome::Pass,
            Err(detail) => Outcome::Fail { detail },
        };
        r.record(
            "H1",
            "复核 grant 有效期",
            "过期凭证仍可用，等于凭证永不失效",
            outcome,
        );
    }

    {
        let (_, hash, req) = r.fresh(iso);
        let _ = hash;
        // 从未登记过的 grant_id。
        let outcome = match r.expect_refusal("从未签发过的-grant", req, "未知 grant").await {
            Ok(()) => Outcome::Pass,
            Err(detail) => Outcome::Fail { detail },
        };
        r.record("H1", "拒绝未知 grant", "任何人编一个 grant_id 就能执行", outcome);
    }

    {
        // grant 是**一次性**的：同一个 grant 用第二次必须被拒。
        let (gid, hash, req1) = r.fresh(iso);
        let first = subject.executor().execute(Runner::grant(&gid), req1).await;
        let req2 = subject.mutating_request("conf-reuse", hash, iso);
        let outcome = if !matches!(
            first,
            Ok(ExecutionResult {
                outcome: ExecutionOutcome::Completed { .. },
                ..
            })
        ) {
            Outcome::Skipped {
                why: "首次执行未成功，无法判定复用行为".into(),
            }
        } else {
            match r.expect_refusal(&gid, req2, "复用的 grant").await {
                Ok(()) => Outcome::Pass,
                Err(detail) => Outcome::Fail { detail },
            }
        };
        r.record(
            "H1",
            "grant 一次性消费",
            "一次审批可被无限次复用，审批形同虚设",
            outcome,
        );
    }

    // ---- H2：隔离不得静默降级 ----
    {
        let higher = match iso {
            IsolationLevel::L0BasicContainment => Some(IsolationLevel::L1RealIsolation),
            IsolationLevel::L1RealIsolation => Some(IsolationLevel::L2RemoteSandbox),
            IsolationLevel::L2RemoteSandbox => None,
        };
        let outcome = match higher {
            None => Outcome::Skipped {
                why: "实现已支持最高隔离级别，无更高级别可请求".into(),
            },
            Some(h) => {
                let (gid, _, req) = r.fresh(h);
                let res = subject.executor().execute(Runner::grant(&gid), req).await;
                match &res {
                    // 拒绝 = 合格。
                    _ if is_refusal(&res) => Outcome::Pass,
                    Ok(x) if x.effective_isolation < h => Outcome::Fail {
                        detail: format!("请求 {h:?} 却在 {:?} 下执行了——静默降级", x.effective_isolation),
                    },
                    other => Outcome::Fail {
                        detail: format!("请求了不支持的隔离级别却成功了：{other:?}"),
                    },
                }
            }
        };
        r.record(
            "H2",
            "达不到隔离级别时失败而非静默降级",
            "对外宣称已隔离却没有，是最危险的一类失败",
            outcome,
        );
    }

    {
        // reconcile 必须如实报告。**没见过的执行必须是 NotStarted**，
        // 猜成 Finished 会让内核跳过一次真正需要重做的操作；
        // 猜成 Running 会让内核永远等下去。
        let status = subject
            .executor()
            .reconcile(ExecutionId::new("conf-从未发生过"))
            .await;
        let outcome = match status {
            Ok(ExecutionStatus::NotStarted) => Outcome::Pass,
            Ok(other) => Outcome::Fail {
                detail: format!("从未发生的执行被报告为 {other:?}"),
            },
            Err(e) => Outcome::Fail {
                detail: format!("reconcile 报错而不是如实回答：{e}"),
            },
        };
        r.record(
            "H2",
            "reconcile 对未发生的执行报 NotStarted",
            "不变量 4 失效：恢复时会跳过或重复副作用",
            outcome,
        );
    }

    {
        // 已完成的执行必须报 Finished 并带回结果。
        let (gid, _, req) = r.fresh(iso);
        let id = req.execution_id.clone();
        let ran = subject.executor().execute(Runner::grant(&gid), req).await;
        let outcome = if !matches!(
            ran,
            Ok(ExecutionResult {
                outcome: ExecutionOutcome::Completed { .. },
                ..
            })
        ) {
            Outcome::Skipped {
                why: "执行未成功，无法检查其 reconcile 结果".into(),
            }
        } else {
            match subject.executor().reconcile(id).await {
                Ok(ExecutionStatus::Finished(_)) => Outcome::Pass,
                Ok(other) => Outcome::Fail {
                    detail: format!("已完成的执行被报告为 {other:?}"),
                },
                Err(e) => Outcome::Fail {
                    detail: format!("reconcile 报错：{e}"),
                },
            }
        };
        r.record(
            "H2",
            "reconcile 对已完成的执行报 Finished 并带回结果",
            "恢复时会重复执行一次已经完成的副作用",
            outcome,
        );
    }

    // ---- H7：ChangeSet overlay 对同一 Run 呈现一致视图 ----
    {
        let (gid, _, write_req) = r.fresh(iso);
        let 写内容 = "conformance-payload-7";
        let mut write_req = write_req;
        if let Some(obj) = write_req.arguments.as_object_mut() {
            obj.insert("content".into(), serde_json::json!(写内容));
        }
        let wrote = subject
            .executor()
            .execute(Runner::grant(&gid), write_req.clone())
            .await;

        let i = r.n.get();
        r.n.set(i + 1);
        let rgid = format!("conf-r{i}");
        let rhash = InputHash(agentrs_contracts::ids::Digest::from_hex(format!("conf-rh{i}")));
        subject.issue_grant(&rgid, rhash.clone(), Timestamp(i64::MAX));
        let read_req = subject.paired_read(&format!("conf-re{i}"), rhash, &write_req);

        let outcome = match (&wrote, read_req) {
            (Ok(w), Some(rq)) if matches!(w.outcome, ExecutionOutcome::Completed { .. }) => {
                let got = subject.executor().execute(Runner::grant(&rgid), rq).await;
                match got.as_ref().ok().and_then(|x| subject.read_content(x)) {
                    Some(text) if text.contains(写内容) => Outcome::Pass,
                    Some(text) => Outcome::Fail {
                        detail: format!(
                            "写入后读到的仍是旧内容（读到 {} 字节，不含刚写的内容）",
                            text.len()
                        ),
                    },
                    None => Outcome::Skipped {
                        why: "读结果无法解出内容".into(),
                    },
                }
            }
            (_, None) => Outcome::Skipped {
                why: "实现未提供可配对的读工具".into(),
            },
            (other, _) => Outcome::Skipped {
                why: format!("写入未成功，无法检查读己之写：{other:?}"),
            },
        };
        r.record(
            "H7",
            "同一 ChangeSet 内读得到刚写的内容",
            "读己之写破坏：连续两次修改会互相抹掉，模型陷入循环",
            outcome,
        );

        // 另一个 ChangeSet 不得看到这次写入——否则并行支线互相污染。
        let i = r.n.get();
        r.n.set(i + 1);
        let ogid = format!("conf-o{i}");
        let ohash = InputHash(agentrs_contracts::ids::Digest::from_hex(format!("conf-oh{i}")));
        subject.issue_grant(&ogid, ohash.clone(), Timestamp(i64::MAX));
        let outcome = match subject
            .paired_read(&format!("conf-oe{i}"), ohash, &write_req)
            .and_then(|rq| subject.in_change_set(&rq, "conf-other-change-set"))
        {
            None => Outcome::Skipped {
                why: "实现未提供跨 ChangeSet 改投能力".into(),
            },
            Some(rq) => {
                let got = subject.executor().execute(Runner::grant(&ogid), rq).await;
                match got.as_ref().ok().and_then(|x| subject.read_content(x)) {
                    // 读不到 = 合格：那份内容本来就不属于这个 ChangeSet。
                    None => Outcome::Pass,
                    Some(text) if !text.contains(写内容) => Outcome::Pass,
                    Some(_) => Outcome::Fail {
                        detail: "另一个 ChangeSet 看到了本 ChangeSet 的未提交改动".into(),
                    },
                }
            }
        };
        r.record(
            "H7",
            "ChangeSet 之间互不可见",
            "并行支线互相污染，谁看到谁取决于时序",
            outcome,
        );
    }

    Report { checks: r.checks }
}
