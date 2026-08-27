//! **M1 出口标准**：一个故意写坏的 adapter 能被 conformance suite 逐条捕获。
//!
//! ## 这条为什么必须单独验
//!
//! 一个只会说"全部通过"的检查套件和没有套件是一样的，而且更糟——
//! 它给出一个虚假的保证。所以套件本身也要被验证：
//! **每一项检查都必须有一个能让它失败的具体实现**。
//!
//! 下面每个 `坏_*` 适配器只违反一条义务，其余全部照做。断言是双向的：
//!
//! - 对应的那条检查**必须**报不合格；
//! - 其余检查**必须**照常通过（否则说明检查之间互相污染，
//!   一处坏了满盘皆红，报告就没有定位价值）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agentrs_contracts::ids::{ExecutionId, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionResult, ExecutionStatus, IsolationLevel, RejectReason,
    SandboxError,
};
use agentrs_testkit::conformance::{check_sandbox, Outcome, Report, SandboxSubject};

/// 违规开关。每个坏 adapter 只打开一个。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct 缺陷 {
    不复核指纹: bool,
    不看有效期: bool,
    接受未知_grant: bool,
    grant_可复用: bool,
    静默降级隔离: bool,
    reconcile_乱猜: bool,
    reconcile_忘记已完成: bool,
    读不到自己刚写的: bool,
    overlay_跨_change_set_泄露: bool,
}

#[derive(Default)]
struct 状态 {
    grants: HashMap<String, (InputHash, Timestamp)>,
    consumed: Vec<String>,
    history: HashMap<ExecutionId, ExecutionResult>,
    /// 已经真的发生的副作用，按 execution_id。
    effects: Vec<ExecutionId>,
    /// ChangeSet overlay：(change_set, path) -> content。
    overlay: HashMap<(String, String), String>,
}

/// 一个可按开关变坏的参考沙箱。
struct 可控沙箱 {
    缺陷: 缺陷,
    st: Mutex<状态>,
}

impl 可控沙箱 {
    fn new(缺陷: 缺陷) -> Arc<Self> {
        Arc::new(Self {
            缺陷,
            st: Mutex::new(状态::default()),
        })
    }

    fn 拒绝(id: ExecutionId, reason: RejectReason) -> ExecutionResult {
        ExecutionResult {
            execution_id: id,
            outcome: ExecutionOutcome::Rejected { reason },
            effective_isolation: IsolationLevel::L0BasicContainment,
            artifacts: vec![],
            output: None,
            change_set: None,
            finished_at: Timestamp(0),
        }
    }
}

#[async_trait::async_trait]
impl SandboxExecutor for 可控沙箱 {
    async fn execute(
        &self,
        grant: SandboxGrant,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, SandboxError> {
        let id = grant.grant_id.as_str().to_string();
        let known = { self.st.lock().unwrap().grants.get(&id).cloned() };

        match known {
            None if !self.缺陷.接受未知_grant => return Err(SandboxError::InvalidGrant),
            None => {}
            Some((bound, expires)) => {
                if !self.缺陷.不复核指纹 && bound != request.input_hash {
                    return Ok(Self::拒绝(
                        request.execution_id,
                        RejectReason::InputHashMismatch,
                    ));
                }
                if !self.缺陷.不看有效期 && expires.0 <= 0 {
                    return Ok(Self::拒绝(request.execution_id, RejectReason::GrantExpired));
                }
            }
        }

        if !self.缺陷.grant_可复用 {
            let mut st = self.st.lock().unwrap();
            if st.consumed.contains(&id) {
                return Ok(Self::拒绝(
                    request.execution_id,
                    RejectReason::GrantAlreadyConsumed,
                ));
            }
            st.consumed.push(id);
        }

        if request.required_isolation > IsolationLevel::L0BasicContainment && !self.缺陷.静默降级隔离
        {
            return Ok(Self::拒绝(
                request.execution_id,
                RejectReason::IsolationUnavailable,
            ));
        }

        // ---- 到这里才真的产生副作用 ----
        let path = request.arguments["path"].as_str().unwrap_or_default().to_string();
        let cs = request.change_set_id.as_str().to_string();
        let mut 读出 = None;
        match request.tool_name.as_str() {
            "Write" => {
                let content = request.arguments["content"].as_str().unwrap_or("").to_string();
                let key = if self.缺陷.overlay_跨_change_set_泄露 {
                    // 忽略 change_set —— 所有支线共用一份 overlay。
                    (String::new(), path.clone())
                } else {
                    (cs.clone(), path.clone())
                };
                self.st.lock().unwrap().overlay.insert(key, content);
            }
            "Read" => {
                if !self.缺陷.读不到自己刚写的 {
                    let st = self.st.lock().unwrap();
                    let key = if self.缺陷.overlay_跨_change_set_泄露 {
                        (String::new(), path.clone())
                    } else {
                        (cs.clone(), path.clone())
                    };
                    读出 = st.overlay.get(&key).cloned();
                }
                // 读不到就当空文件——磁盘上本来也没有。
                读出 = 读出.or_else(|| Some(String::new()));
            }
            _ => {}
        }

        let result = ExecutionResult {
            execution_id: request.execution_id.clone(),
            outcome: ExecutionOutcome::Completed { exit_code: 0 },
            // 静默降级的那一款照样声称跑在 L0——这正是危险之处。
            effective_isolation: IsolationLevel::L0BasicContainment,
            artifacts: vec![],
            output: Some(读出.unwrap_or_else(|| "ok".into())),
            change_set: Some(request.change_set_id.clone()),
            finished_at: Timestamp(0),
        };
        let mut st = self.st.lock().unwrap();
        st.effects.push(request.execution_id.clone());
        if !self.缺陷.reconcile_忘记已完成 {
            st.history.insert(request.execution_id, result.clone());
        }
        Ok(result)
    }

    async fn cancel(&self, _id: ExecutionId) -> Result<(), SandboxError> {
        Ok(())
    }

    async fn reconcile(&self, id: ExecutionId) -> Result<ExecutionStatus, SandboxError> {
        if self.缺陷.reconcile_乱猜 {
            // "查不到就当它跑完了"——最要命的一种猜测。
            return Ok(ExecutionStatus::Finished(Box::new(可控沙箱::拒绝(
                id,
                RejectReason::InputHashMismatch,
            ))));
        }
        let st = self.st.lock().unwrap();
        Ok(match st.history.get(&id) {
            Some(r) => ExecutionStatus::Finished(Box::new(r.clone())),
            None => ExecutionStatus::NotStarted,
        })
    }
}

struct 受检对象(Arc<可控沙箱>);

impl SandboxSubject for 受检对象 {
    fn executor(&self) -> Arc<dyn SandboxExecutor> {
        self.0.clone()
    }

    fn issue_grant(&self, grant_id: &str, bound: InputHash, expires_at: Timestamp) {
        self.0
            .st
            .lock()
            .unwrap()
            .grants
            .insert(grant_id.to_string(), (bound, expires_at));
    }

    fn mutating_request(
        &self,
        execution_id: &str,
        input_hash: InputHash,
        required_isolation: IsolationLevel,
    ) -> ExecutionRequest {
        ExecutionRequest {
            execution_id: ExecutionId::new(execution_id),
            tool_name: "Write".into(),
            arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
            change_set_id: "cs-conf".into(),
            input_hash,
            required_isolation,
        }
    }

    fn side_effect_happened(&self, request: &ExecutionRequest) -> Option<bool> {
        Some(self.0.st.lock().unwrap().effects.contains(&request.execution_id))
    }

    fn paired_read(
        &self,
        execution_id: &str,
        input_hash: InputHash,
        written: &ExecutionRequest,
    ) -> Option<ExecutionRequest> {
        Some(ExecutionRequest {
            execution_id: ExecutionId::new(execution_id),
            tool_name: "Read".into(),
            arguments: serde_json::json!({"path": written.arguments["path"]}),
            change_set_id: written.change_set_id.clone(),
            input_hash,
            required_isolation: written.required_isolation,
        })
    }

    fn read_content(&self, result: &ExecutionResult) -> Option<String> {
        result.output.clone()
    }

    fn in_change_set(&self, req: &ExecutionRequest, change_set: &str) -> Option<ExecutionRequest> {
        let mut r = req.clone();
        r.change_set_id = change_set.into();
        Some(r)
    }
}

async fn 跑(缺陷: 缺陷) -> Report {
    check_sandbox(&受检对象(可控沙箱::new(缺陷))).await
}

/// 断言：恰好 `期望` 这些检查名不合格，其余全部通过。
fn 只有这些不合格(r: &Report, 期望: &[&str]) {
    let 实际: Vec<&str> = r.failures().iter().map(|c| c.name).collect();
    assert_eq!(实际, 期望, "捕获的违规项与预期不符\n{}", r.render());
    assert!(r.skipped().is_empty(), "本组适配器不应产生跳过项\n{}", r.render());
}

// ---- 基线 ----

#[tokio::test]
async fn 合格的适配器全部通过() {
    // 没有这条基线，下面每一条"能捕获"都可能只是因为套件恒报失败。
    let r = 跑(缺陷::default()).await;
    assert!(r.passed(), "合格实现被误判\n{}", r.render());
    assert!(r.skipped().is_empty(), "不应有跳过项\n{}", r.render());
    assert!(r.checks.len() >= 10, "检查项太少：{}", r.checks.len());
}

// ---- 逐条捕获 ----

#[tokio::test]
async fn 捕获_不复核输入指纹() {
    只有这些不合格(
        &跑(缺陷 {
            不复核指纹: true,
            ..Default::default()
        })
        .await,
        &["复核 bound_input_hash"],
    );
}

#[tokio::test]
async fn 捕获_不看_grant_有效期() {
    只有这些不合格(
        &跑(缺陷 {
            不看有效期: true,
            ..Default::default()
        })
        .await,
        &["复核 grant 有效期"],
    );
}

#[tokio::test]
async fn 捕获_接受未知_grant() {
    只有这些不合格(
        &跑(缺陷 {
            接受未知_grant: true,
            ..Default::default()
        })
        .await,
        &["拒绝未知 grant"],
    );
}

#[tokio::test]
async fn 捕获_grant_可被复用() {
    只有这些不合格(
        &跑(缺陷 {
            grant_可复用: true,
            ..Default::default()
        })
        .await,
        &["grant 一次性消费"],
    );
}

#[tokio::test]
async fn 捕获_静默降级隔离级别() {
    只有这些不合格(
        &跑(缺陷 {
            静默降级隔离: true,
            ..Default::default()
        })
        .await,
        &["达不到隔离级别时失败而非静默降级"],
    );
}

#[tokio::test]
async fn 捕获_reconcile_乱猜() {
    // "查不到就当它跑完了"——恢复时会跳过一次真正需要重做的操作。
    只有这些不合格(
        &跑(缺陷 {
            reconcile_乱猜: true,
            ..Default::default()
        })
        .await,
        &["reconcile 对未发生的执行报 NotStarted"],
    );
}

#[tokio::test]
async fn 捕获_reconcile_忘记已完成的执行() {
    // 反过来的错：真跑过却报 NotStarted，恢复时会重复一次副作用。
    只有这些不合格(
        &跑(缺陷 {
            reconcile_忘记已完成: true,
            ..Default::default()
        })
        .await,
        &["reconcile 对已完成的执行报 Finished 并带回结果"],
    );
}

#[tokio::test]
async fn 捕获_读不到自己刚写的内容() {
    // 连续两次修改会互相抹掉，而且不报错——模型只会看到自己的改动"没生效"，
    // 于是再改一遍，陷入循环。
    只有这些不合格(
        &跑(缺陷 {
            读不到自己刚写的: true,
            ..Default::default()
        })
        .await,
        &["同一 ChangeSet 内读得到刚写的内容"],
    );
}

#[tokio::test]
async fn 捕获_overlay_跨_change_set_泄露() {
    只有这些不合格(
        &跑(缺陷 {
            overlay_跨_change_set_泄露: true,
            ..Default::default()
        })
        .await,
        &["ChangeSet 之间互不可见"],
    );
}

// ---- 报告本身的性质 ----

#[tokio::test]
async fn 报告点名违规项与后果() {
    let r = 跑(缺陷 {
        不复核指纹: true,
        ..Default::default()
    })
    .await;
    let text = r.render();
    assert!(text.contains("H1"), "报告未标出义务编号：{text}");
    assert!(text.contains("内核不变量 2"), "报告未说明后果：{text}");
}

#[tokio::test]
async fn 多处违规被同时捕获而不是只报第一条() {
    // 只报第一条会让修复变成"改一处跑一次"的漫长循环。
    let r = 跑(缺陷 {
        不复核指纹: true,
        接受未知_grant: true,
        reconcile_乱猜: true,
        ..Default::default()
    })
    .await;
    assert_eq!(r.failures().len(), 3, "{}", r.render());
}

#[tokio::test]
async fn 跳过项不计入通过() {
    // 把"没法验"记成"验过了"是这类套件最容易犯的错。
    let mut r = Report::default();
    r.checks.push(agentrs_testkit::conformance::Check {
        obligation: "H2",
        name: "示例",
        consequence: "无",
        outcome: Outcome::Skipped {
            why: "没有更高的隔离级别".into(),
        },
    });
    assert!(r.passed(), "跳过不算不合格");
    assert_eq!(r.skipped().len(), 1);
    assert!(
        r.render().contains("跳过不等于通过"),
        "报告必须显式提醒：{}",
        r.render()
    );
}
