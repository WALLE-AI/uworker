//! `ChildRun` 派生的测试。
//!
//! 总承诺一条：**派生结果恒不比父宽**（内核不变量 5）。

use agentrs_contracts::spec::{
    ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, SystemContext,
};
use agentrs_contracts::version::SpecVersion;

use super::*;

fn 信封(max_depth: u16) -> AuthorityEnvelope {
    AuthorityEnvelope {
        id: "e".into(),
        workspaces: vec!["ws".into()],
        tools: vec!["Read".into(), "Write".into(), "Bash".into()],
        providers: vec!["p".into()],
        models: vec!["big".into(), "small".into()],
        max_depth,
    }
}

fn 父(depth: u16, max_depth: u16) -> Parent {
    Parent {
        run_id: "r-parent".into(),
        epoch: RunEpoch(7),
        authority: 信封(max_depth),
        capabilities: CapabilityView {
            tools: vec!["Read".into(), "Write".into()],
            providers: vec!["p".into()],
            models: vec!["big".into(), "small".into()],
        },
        permission_mode: PermissionMode::Default,
        depth,
    }
}

fn 父规格() -> RunSpec {
    RunSpec {
        run_id: "r-parent".into(),
        parent_run_id: None,
        conversation: ConversationSnapshot::default(),
        system_context: SystemContext::default(),
        authority: 信封(3),
        initial_capabilities: CapabilityView {
            tools: vec!["Read".into(), "Write".into()],
            providers: vec!["p".into()],
            models: vec!["big".into(), "small".into()],
        },
        permission_mode: PermissionMode::Default,
        model_policy: ModelPolicy {
            tiers: Default::default(),
            fallback: vec![],
            providers: vec![],
            max_retries: 0,
            allow_attachments: false,
        },
        context_budget: ContextBudget {
            max_input_tokens: 1000,
            reserved_output_tokens: 100,
            compaction_threshold_pct: 80,
        },
        execution_budget: ExecutionBudget::default(),
        checkpoint: Some(agentrs_contracts::spec::RunCheckpoint {
            spec_version: SpecVersion(1),
            up_to_seq: agentrs_contracts::ids::EventSequence(9),
            pending_approval: Some("父的未决审批".into()),
        }),
        spec_version: SpecVersion(1),
        config: Default::default(),
    }
}

fn 请求() -> ChildRequest {
    ChildRequest::new("r-child", "op-1")
}

fn 已接受(scopes: &[&str]) -> PermissionMode {
    PermissionMode::Accepted {
        scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
    }
}

// ---------------------------------------------------------------------------
// 总承诺
// ---------------------------------------------------------------------------

#[test]
fn 任何请求都不能让子比父宽() {
    // **内核不变量 5 的可执行形式。** 下面每个请求都在尝试扩大某一项。
    let p = 父(0, 3);
    let 各种尝试 = [
        // 想要父没有的工具。
        ChildRequest {
            tools: Some(vec!["Read".into(), "Sudo".into()]),
            ..请求()
        },
        // 想要父没有的模型。
        ChildRequest {
            models: Some(vec!["big".into(), "huge".into()]),
            ..请求()
        },
        // 想把权限放宽。
        ChildRequest {
            permission_mode: Some(已接受(&["一切"])),
            ..请求()
        },
    ];

    for req in &各种尝试 {
        let c = derive(&p, &父规格(), req).expect("应当派生成功（收窄而非报错）");
        assert!(
            is_no_wider(&c, &p),
            "子比父宽了：{req:?} → {:?}",
            c.spec.initial_capabilities
        );
    }
}

// ---------------------------------------------------------------------------
// 工具与模型
// ---------------------------------------------------------------------------

#[test]
fn 不请求时照搬父的当下能力() {
    let c = derive(&父(0, 3), &父规格(), &请求()).unwrap();
    assert_eq!(c.spec.initial_capabilities.tools, ["Read", "Write"]);
}

#[test]
fn 上界取父的当下视图而不是父的信封() {
    // **父自己已经收窄过的东西，子不该拿回去。**
    // 信封里有 Bash，但父当下的视图里没有——子也不该有。
    let c = derive(&父(0, 3), &父规格(), &请求()).unwrap();
    assert!(
        !c.spec.initial_capabilities.tools.contains(&"Bash".to_string()),
        "子拿回了父已经收窄掉的工具"
    );
    assert!(!c.spec.authority.tools.contains(&"Bash".to_string()));
}

#[test]
fn 请求的工具与父取交集() {
    let req = ChildRequest {
        tools: Some(vec!["Read".into(), "Bash".into()]),
        ..请求()
    };
    let c = derive(&父(0, 3), &父规格(), &req).unwrap();
    // Bash 不在父的当下视图里 → 不会被加进来。
    assert_eq!(c.spec.initial_capabilities.tools, ["Read"]);
}

#[test]
fn 请求的工具全都拿不到时报错而不是给一个零工具子运行() {
    // 静默给零工具，子运行会在**第一次尝试调用时**才失败，
    // 而那时已经花了一次模型请求。早点说更省。
    let req = ChildRequest {
        tools: Some(vec!["Sudo".into(), "Rm".into()]),
        ..请求()
    };
    assert_eq!(
        derive(&父(0, 3), &父规格(), &req),
        Err(DeriveError::NotASubset { field: "tools" })
    );
}

#[test]
fn 模型同样取交集() {
    let req = ChildRequest {
        models: Some(vec!["small".into()]),
        ..请求()
    };
    let c = derive(&父(0, 3), &父规格(), &req).unwrap();
    assert_eq!(
        c.spec.initial_capabilities.models,
        vec![agentrs_contracts::ids::ModelId::new("small")]
    );
}

// ---------------------------------------------------------------------------
// 权限模式
// ---------------------------------------------------------------------------

#[test]
fn 模式收紧生效() {
    let req = ChildRequest {
        permission_mode: Some(PermissionMode::Plan),
        ..请求()
    };
    let c = derive(&父(0, 3), &父规格(), &req).unwrap();
    assert_eq!(c.spec.permission_mode, PermissionMode::Plan);
}

#[test]
fn 模式放宽时降级为父模式而不是报错() {
    // 与 `permission::inherit` 同一处理：子拿到的权限少于它要的，
    // 这是安全方向；报错会让一个写错的编排整体失败。
    let req = ChildRequest {
        permission_mode: Some(已接受(&["一切"])),
        ..请求()
    };
    let c = derive(&父(0, 3), &父规格(), &req).unwrap();
    assert_eq!(c.spec.permission_mode, PermissionMode::Default);
}

#[test]
fn 父在_plan_模式时子也在_plan() {
    let mut p = 父(0, 3);
    p.permission_mode = PermissionMode::Plan;
    let c = derive(&p, &父规格(), &请求()).unwrap();
    assert_eq!(c.spec.permission_mode, PermissionMode::Plan);
}

// ---------------------------------------------------------------------------
// 深度
// ---------------------------------------------------------------------------

#[test]
fn 深度是父加一() {
    assert_eq!(derive(&父(0, 3), &父规格(), &请求()).unwrap().depth, 1);
    assert_eq!(derive(&父(2, 3), &父规格(), &请求()).unwrap().depth, 3);
}

#[test]
fn 超过上限直接拒绝而不是截断() {
    // **静默截断会让一个写错的递归编排看起来在正常工作**，
    // 而它实际上少做了最里面那几层。
    assert_eq!(
        derive(&父(3, 3), &父规格(), &请求()),
        Err(DeriveError::TooDeep { depth: 4, max: 3 })
    );
}

#[test]
fn 上限为零时任何子运行都被拒绝() {
    assert!(matches!(
        derive(&父(0, 0), &父规格(), &请求()),
        Err(DeriveError::TooDeep { .. })
    ));
}

// ---------------------------------------------------------------------------
// 所有权与生命周期
// ---------------------------------------------------------------------------

#[test]
fn 共享父的_epoch() {
    // **这一条只对函数式子 Agent 成立。** 它活不过一个 operation，
    // 父被围栏时它跟着倒是对的。协作式成员跨父的多个 turn，
    // 让它跟着父的 epoch 倒就错了——那是 MemberRun，各有自己的 epoch。
    let c = derive(&父(0, 3), &父规格(), &请求()).unwrap();
    assert_eq!(c.epoch, RunEpoch(7));
}

#[test]
fn 记下发起它的_operation() {
    // 子运行的生命周期不得超过它。
    let c = derive(&父(0, 3), &父规格(), &请求()).unwrap();
    assert_eq!(c.operation_id, "op-1".into());
}

#[test]
fn 父子关系双向可查() {
    let c = derive(&父(0, 3), &父规格(), &请求()).unwrap();
    assert_eq!(c.parent_run_id, "r-parent".into());
    assert_eq!(c.spec.parent_run_id, Some("r-parent".into()));
    assert_eq!(c.spec.run_id, "r-child".into());
}

#[test]
fn 不继承父的_checkpoint() {
    // 父的挂起状态与子无关。继承它会让子一启动就带着一个
    // **不属于它的未决审批**。
    let c = derive(&父(0, 3), &父规格(), &请求()).unwrap();
    assert!(c.spec.checkpoint.is_none());
}

#[test]
fn 子运行_id_不能与父相同() {
    let req = ChildRequest {
        child_run_id: "r-parent".into(),
        ..请求()
    };
    assert_eq!(derive(&父(0, 3), &父规格(), &req), Err(DeriveError::SameRunId));
}

// ---------------------------------------------------------------------------
// 确定性
// ---------------------------------------------------------------------------

#[test]
fn 相同输入产生相同派生() {
    let (p, s, r) = (父(0, 3), 父规格(), 请求());
    assert_eq!(derive(&p, &s, &r), derive(&p, &s, &r));
}
