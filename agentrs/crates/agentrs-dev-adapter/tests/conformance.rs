//! 把 conformance suite 跑在 dev-adapter 的真实实现上。
//!
//! `LocalFileSandbox` 是**我们自己写的参考实现**——正因如此才更要跑：
//! 自家实现最容易被默认"当然是对的"，而宿主义务恰恰是内核无法自证的那一半。

use std::sync::Arc;

use agentrs_contracts::ids::{ExecutionId, Timestamp};
use agentrs_contracts::policy::InputHash;
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{ExecutionRequest, IsolationLevel};
use agentrs_dev_adapter::LocalFileSandbox;
use agentrs_testkit::conformance::{check_sandbox, SandboxSubject};

struct 受检的DevAdapter {
    sb: Arc<LocalFileSandbox>,
    _dir: tempdir::TempDir,
}

impl SandboxSubject for 受检的DevAdapter {
    fn executor(&self) -> Arc<dyn SandboxExecutor> {
        self.sb.clone()
    }

    fn issue_grant(&self, grant_id: &str, bound: InputHash, expires_at: Timestamp) {
        self.sb.issue_grant(grant_id, bound, expires_at);
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
            // 每次写不同路径，避免不同检查之间通过 overlay 互相影响。
            arguments: serde_json::json!({
                "path": format!("{execution_id}.txt"),
                "content": "x"
            }),
            change_set_id: "cs-conf".into(),
            input_hash,
            required_isolation,
        }
    }

    fn side_effect_happened(&self, request: &ExecutionRequest) -> Option<bool> {
        // 副作用落在 overlay 里（内核无提交权），所以查 overlay 而不是磁盘。
        let path = request.arguments["path"].as_str()?;
        Some(self.sb.read_text("cs-conf", path).is_some())
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

    fn read_content(&self, result: &agentrs_contracts::sandbox::ExecutionResult) -> Option<String> {
        result.output.clone()
    }

    fn in_change_set(&self, req: &ExecutionRequest, change_set: &str) -> Option<ExecutionRequest> {
        let mut r = req.clone();
        r.change_set_id = change_set.into();
        Some(r)
    }

    fn max_isolation(&self) -> IsolationLevel {
        // dev-adapter 只到 L0——真实隔离归 SandboxRS。
        IsolationLevel::L0BasicContainment
    }
}

#[tokio::test]
async fn dev_adapter_满足全部宿主义务() {
    let dir = tempdir::TempDir::new("agentrs-conf").unwrap();
    let sb = Arc::new(LocalFileSandbox::new(dir.path()).unwrap());
    let subject = 受检的DevAdapter { sb, _dir: dir };

    let report = check_sandbox(&subject).await;
    assert!(report.passed(), "dev-adapter 不合格：\n{}", report.render());
    // 跳过项必须显式盘点——静默跳过会让"全绿"变成假象。
    assert!(
        report.skipped().is_empty(),
        "存在未验证的义务：\n{}",
        report.render()
    );
}

/// JsonlPersistence 的 H3 检查。
mod h3 {
    use agentrs_contracts::ids::RunEpoch;
    use agentrs_contracts::ports::RunPersistence;
    use agentrs_dev_adapter::JsonlPersistence;
    use agentrs_testkit::conformance::persistence::{check_persistence, PersistenceSubject};
    use std::sync::Arc;

    struct 受检的Jsonl {
        p: Arc<JsonlPersistence>,
        _dir: tempdir::TempDir,
    }

    impl PersistenceSubject for 受检的Jsonl {
        fn persistence(&self) -> Arc<dyn RunPersistence> {
            self.p.clone()
        }
        fn advance_epoch(&self, current: RunEpoch) -> Option<RunEpoch> {
            Some(RunEpoch(current.0 + 1))
        }
    }

    #[tokio::test]
    async fn jsonl_持久化满足_h3() {
        let dir = tempdir::TempDir::new("agentrs-conf-h3").unwrap();
        let path = dir.path().join("events.jsonl");
        let p = Arc::new(JsonlPersistence::open(&path).unwrap());
        let subject = 受检的Jsonl { p, _dir: dir };

        let report = check_persistence(&subject).await;
        assert!(report.passed(), "JsonlPersistence 不合格：\n{}", report.render());
        assert!(
            report.skipped().is_empty(),
            "存在未验证的义务：\n{}",
            report.render()
        );
    }
}

/// DevPolicy 的 H5 检查。
mod h5 {
    use std::sync::Arc;

    use agentrs_contracts::ids::{Digest, Timestamp};
    use agentrs_contracts::policy::{InputHash, ToolProposal};
    use agentrs_contracts::ports::PolicyEnforcer;
    use agentrs_dev_adapter::{DevPolicy, LocalFileSandbox};
    use agentrs_testkit::conformance::policy::{check_policy, PolicySubject};

    struct 受检的DevPolicy {
        p: Arc<DevPolicy>,
        _dir: tempdir::TempDir,
    }

    impl PolicySubject for 受检的DevPolicy {
        fn policy(&self) -> Arc<dyn PolicyEnforcer> {
            self.p.clone()
        }

        fn allowed_proposal(&self, tag: &str) -> ToolProposal {
            ToolProposal {
                call_id: tag.into(),
                tool_name: "Write".into(),
                arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
                workspace_id: "ws".into(),
                change_set_id: "cs".into(),
                input_hash: InputHash(Digest::from_hex(tag)),
            }
        }

        fn now(&self) -> Timestamp {
            Timestamp(0)
        }
    }

    #[tokio::test]
    async fn dev_policy_满足_h5() {
        let dir = tempdir::TempDir::new("agentrs-conf-h5").unwrap();
        let sb = Arc::new(LocalFileSandbox::new(dir.path()).unwrap());
        let p = Arc::new(DevPolicy::new(sb, ["Write"]));
        let subject = 受检的DevPolicy { p, _dir: dir };

        let report = check_policy(&subject).await;
        assert!(report.passed(), "DevPolicy 不合格：\n{}", report.render());
    }
}

/// 有效期必须与逻辑时刻比较，而不是"是不是 ≤ 0"。
///
/// 这条是 conformance suite 逼出来的连锁修正：`DevPolicy` 改为签发有限期
/// grant 之后，`LocalFileSandbox` 原来那句 `expires_at <= 0` 就露馅了——
/// 一个"5 分钟后到期"的 grant 在第 10 分钟仍会被放行。
mod 有效期 {
    use std::sync::Arc;

    use agentrs_contracts::ids::{Digest, ExecutionId, Timestamp};
    use agentrs_contracts::policy::{InputHash, SandboxGrant};
    use agentrs_contracts::ports::SandboxExecutor;
    use agentrs_contracts::sandbox::{ExecutionOutcome, ExecutionRequest, IsolationLevel, RejectReason};
    use agentrs_dev_adapter::LocalFileSandbox;

    #[tokio::test]
    async fn 逻辑时刻走过到期点后_grant_失效() {
        let dir = tempdir::TempDir::new("agentrs-ttl").unwrap();
        let sb = Arc::new(LocalFileSandbox::new(dir.path()).unwrap());
        let hash = InputHash(Digest::from_hex("h"));

        // 一个在 t=100 到期的 grant。
        sb.issue_grant("g", hash.clone(), Timestamp(100));

        let req = |id: &str| ExecutionRequest {
            execution_id: ExecutionId::new(id),
            tool_name: "Write".into(),
            arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
            change_set_id: "cs".into(),
            input_hash: hash.clone(),
            required_isolation: IsolationLevel::L0BasicContainment,
        };
        let grant = || SandboxGrant {
            grant_id: "g".into(),
            payload: serde_json::json!({}),
        };

        // t=50：还没到期，放行。
        sb.set_now(Timestamp(50));
        let r = sb.execute(grant(), req("e1")).await.unwrap();
        assert_eq!(r.outcome, ExecutionOutcome::Completed { exit_code: 0 });

        // 重新签发同一个 grant，把时刻推过到期点。
        sb.issue_grant("g2", hash.clone(), Timestamp(100));
        sb.set_now(Timestamp(200));
        let r = sb
            .execute(
                SandboxGrant {
                    grant_id: "g2".into(),
                    payload: serde_json::json!({}),
                },
                req("e2"),
            )
            .await
            .unwrap();
        assert_eq!(
            r.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::GrantExpired
            },
            "过了到期点仍被放行——有效期形同虚设"
        );
    }
}
