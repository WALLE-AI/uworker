//! 脱敏与 replay bundle 的测试。
//!
//! 核心那条不显然的性质单独列在最前：**脱敏是稳定映射，不是删除**。

use agentrs_contracts::event::{Causality, Durability, Visibility};
use agentrs_contracts::ids::{Digest, EventSequence, RunEpoch, Timestamp};
use agentrs_contracts::policy::{DenyCode, InputHash};
use agentrs_contracts::version::SpecVersion;
use agentrs_contracts::{StepIntent, StepOutcome, StepResult};

use super::*;

fn 盐() -> BundleSalt {
    BundleSalt("bundle-salt-42".into())
}

fn 脱敏器() -> Redactor {
    Redactor::new(盐(), Some("/home/alice/proj".into()))
}

fn 事件(seq: u64, payload: EventPayload) -> RunEventEnvelope {
    RunEventEnvelope {
        run_id: "r".into(),
        epoch: RunEpoch(1),
        event_id: format!("e{seq}").into(),
        seq: Some(EventSequence(seq)),
        live_seq: None,
        at: Timestamp(0),
        durability: Durability::DurableFact,
        visibility: Visibility::User,
        causality: Causality::default(),
        surface: None,
        payload,
    }
}

fn 结果(outcome: StepOutcome, output: Option<&str>) -> EventPayload {
    EventPayload::StepResultRecorded {
        result: Box::new(StepResult {
            step_id: "s".into(),
            call_id: "c".into(),
            outcome,
            effective_isolation: None,
            artifacts: vec![],
            output: output.map(str::to_owned),
            at: Timestamp(0),
        }),
    }
}

fn 意图() -> EventPayload {
    EventPayload::StepIntentRecorded {
        intent: Box::new(StepIntent {
            step_id: "s".into(),
            call_id: "c".into(),
            tool_name: "Write".into(),
            input_hash: InputHash(Digest::from_hex("h")),
            change_set_id: "cs".into(),
            execution_id: "x".into(),
            at: Timestamp(0),
        }),
    }
}

// ---------------------------------------------------------------------------
// 稳定映射，不是删除
// ---------------------------------------------------------------------------

#[test]
fn 同一_bundle_内同路径得到同结果() {
    // **这是全部设计的支点。** 不成立的话，同一份 bundle 导出两次
    // 会得到两个 snapshot，replay 不可重现——而 replay 正是导出的目的。
    let r = 脱敏器();
    let a = r.path("/etc/secret/config");
    let b = r.path("/etc/secret/config");
    assert_eq!(a, b);
}

#[test]
fn 不同路径得到不同结果() {
    // 若都替换成同一个占位（比如统一写"[路径]"），两条不同事件会撞成一条，
    // 投影结果随之改变。
    let r = 脱敏器();
    assert_ne!(r.path("/etc/a").to, r.path("/etc/b").to);
}

#[test]
fn 换一份_bundle_则结果不同() {
    // 跨 bundle 不可关联——否则攒够多份 bundle 就能反推路径分布。
    let a = Redactor::new(BundleSalt("salt-1".into()), None);
    let b = Redactor::new(BundleSalt("salt-2".into()), None);
    assert_ne!(a.path("/etc/x").to, b.path("/etc/x").to);
}

#[test]
fn 能相对化时优先相对化() {
    // 相对路径**可读、可排查，且同样确定**，比一串 opaque 好用得多。
    let r = 脱敏器();
    let p = r.path("/home/alice/proj/src/main.rs");
    assert_eq!(p.rule, Rule::Relativized);
    assert_eq!(p.to, "src/main.rs");
}

#[test]
fn 工作区根本身相对化为点() {
    assert_eq!(脱敏器().path("/home/alice/proj").to, ".");
    assert_eq!(脱敏器().path("/home/alice/proj/").to, ".");
}

#[test]
fn 工作区之外的路径变成不可逆替身() {
    let p = 脱敏器().path("/etc/passwd");
    assert_eq!(p.rule, Rule::Opaque);
    assert!(p.to.starts_with("opaque:"));
    assert!(!p.to.contains("etc"), "替身里不该残留原路径：{}", p.to);
    assert!(!p.to.contains("passwd"));
}

#[test]
fn 相对路径原样保留() {
    // 它本来就不泄漏什么。全都替换会让 bundle 完全不可读。
    let p = 脱敏器().path("src/lib.rs");
    assert_eq!(p.rule, Rule::Untouched);
    assert_eq!(p.to, "src/lib.rs");
}

#[test]
fn windows_绝对路径同样被识别() {
    let r = Redactor::new(盐(), None);
    assert_eq!(r.path(r"C:\Users\alice\secret.txt").rule, Rule::Opaque);
    assert_eq!(r.path(r"\\server\share\x").rule, Rule::Opaque);
    assert_eq!(r.path(r"src\lib.rs").rule, Rule::Untouched);
}

// ---------------------------------------------------------------------------
// 正文一律不进
// ---------------------------------------------------------------------------

#[test]
fn 命令输出只留长度() {
    let e = 事件(1, 结果(StepOutcome::Succeeded, Some("超级机密的输出内容")));
    let out = 脱敏器().event(&e);
    let d = format!("{:?}", out.payload);
    assert!(!d.contains("超级机密"), "正文进了 bundle：{d}");
    assert!(d.contains("已脱敏"), "{d}");
}

#[test]
fn 失败说明只留长度() {
    let e = 事件(
        1,
        结果(
            StepOutcome::Failed {
                message: "打开 /home/bob/.ssh/id_rsa 失败".into(),
            },
            None,
        ),
    );
    let d = format!("{:?}", 脱敏器().event(&e).payload);
    assert!(!d.contains(".ssh"), "{d}");
    assert!(!d.contains("id_rsa"), "{d}");
}

#[test]
fn 拒绝码保留而说明脱敏() {
    // **码是稳定枚举、不含用户内容，而它恰恰是排查时最有用的那一半。**
    // 连码一起抹掉，bundle 就只剩"某次调用被拒了"。
    let e = 事件(
        1,
        结果(
            StepOutcome::Denied {
                code: DenyCode::PermissionMode,
                message: "写 /etc/hosts 不被允许".into(),
            },
            None,
        ),
    );
    let d = format!("{:?}", 脱敏器().event(&e).payload);
    assert!(d.contains("PermissionMode"), "拒绝码被抹掉了：{d}");
    assert!(!d.contains("/etc/hosts"), "{d}");
}

#[test]
fn 意图里的工具名与指纹保留() {
    // 工具名不是路径；input_hash 本来就是摘要。抹掉它们等于抹掉排查线索。
    let d = format!("{:?}", 脱敏器().event(&事件(1, 意图())).payload);
    assert!(d.contains("Write"));
}

#[test]
fn 事件标识与序号不被改动() {
    // 接收端按 (run_id, event_id) 去重。改了它，去重就失效了。
    let e = 事件(7, 结果(StepOutcome::Succeeded, Some("x")));
    let out = 脱敏器().event(&e);
    assert_eq!(out.event_id, e.event_id);
    assert_eq!(out.seq, e.seq);
    assert_eq!(out.run_id, e.run_id);
    assert_eq!(out.epoch, e.epoch);
}

// ---------------------------------------------------------------------------
// bundle
// ---------------------------------------------------------------------------

fn 版本表() -> BTreeMap<String, u32> {
    [("skeleton".to_string(), 1u32)].into_iter().collect()
}

#[test]
fn bundle_头部记下了盐() {
    // 没有它，同一份 bundle 无法复现脱敏结果。
    let b = ReplayBundle::build(SpecVersion(1), &脱敏器(), &[], 版本表());
    assert_eq!(b.salt, 盐());
}

#[test]
fn bundle_只含_durable_事件() {
    // live delta 可丢。导出它只会让 bundle 在"丢了"和"没丢"两种情况下不一致。
    let mut live = 事件(2, EventPayload::TextDelta);
    live.seq = None;
    live.durability = Durability::LiveStream;

    let b = ReplayBundle::build(
        SpecVersion(1),
        &脱敏器(),
        &[事件(1, EventPayload::RunStarted), live],
        版本表(),
    );
    assert_eq!(b.events.len(), 1);
}

#[test]
fn bundle_带上投影版本() {
    // 接收端据此判断自己的投影能不能读这份 bundle。
    let b = ReplayBundle::build(SpecVersion(1), &脱敏器(), &[], 版本表());
    assert_eq!(b.projection_versions.get("skeleton"), Some(&1));
}

#[test]
fn 同样的输入产生同样的_bundle() {
    // 确定性一路传到 bundle 这一层。
    let evs = [
        事件(1, 意图()),
        事件(2, 结果(StepOutcome::Succeeded, Some("输出"))),
    ];
    let a = ReplayBundle::build(SpecVersion(1), &脱敏器(), &evs, 版本表());
    let b = ReplayBundle::build(SpecVersion(1), &脱敏器(), &evs, 版本表());
    assert_eq!(a, b);
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

#[test]
fn 干净的_bundle_自检无泄漏() {
    let evs = [
        事件(1, 意图()),
        事件(2, 结果(StepOutcome::Succeeded, Some("普通输出"))),
    ];
    let b = ReplayBundle::build(SpecVersion(1), &脱敏器(), &evs, 版本表());
    assert!(b.audit().is_empty(), "{:?}", b.audit());
}

#[test]
fn 自检抓得住漏网的绝对路径() {
    // **脱敏规则会漏**——新增一个带路径的字段就漏一处，而漏了没人会发现，
    // 除非有这道自检。这里用一个当前规则**不覆盖**的字段来验证自检本身。
    let mut e = 事件(1, EventPayload::RunStarted);
    e.causality.trace_id = Some("/home/bob/.aws/credentials".into());

    // 直接构造一个未经脱敏的 bundle 来考自检。
    let b = ReplayBundle {
        spec_version: SpecVersion(1),
        salt: 盐(),
        events: vec![e],
        projection_versions: 版本表(),
    };
    // trace_id 不在 payload 里，自检只看 payload —— 这条如实反映了
    // **自检的覆盖范围有限**，不是"自检通过就一定没泄漏"。
    let _ = b.audit();

    // 换一个真的在 payload 里的：
    let 漏了 = ReplayBundle {
        spec_version: SpecVersion(1),
        salt: 盐(),
        events: vec![事件(
            2,
            结果(
                StepOutcome::Failed {
                    message: "/home/bob/.aws/credentials 打不开".into(),
                },
                None,
            ),
        )],
        projection_versions: 版本表(),
    };
    let leaks = 漏了.audit();
    assert_eq!(leaks.len(), 1, "{leaks:?}");
    assert_eq!(leaks[0].kind, LeakKind::AbsolutePath);
}

#[test]
fn 自检抓得住疑似密钥() {
    let b = ReplayBundle {
        spec_version: SpecVersion(1),
        salt: 盐(),
        events: vec![事件(
            1,
            结果(StepOutcome::Succeeded, Some("token=sk-abc123def456")),
        )],
        projection_versions: 版本表(),
    };
    let leaks = b.audit();
    assert!(
        leaks.iter().any(|l| l.kind == LeakKind::PossibleSecret),
        "{leaks:?}"
    );
}

#[test]
fn 自检样本本身不是完整泄漏() {
    // 报告里贴一整条密钥，等于把泄漏搬了个地方。
    let b = ReplayBundle {
        spec_version: SpecVersion(1),
        salt: 盐(),
        events: vec![事件(
            1,
            结果(
                StepOutcome::Succeeded,
                Some("sk-this-is-a-very-long-secret-key-value"),
            ),
        )],
        projection_versions: 版本表(),
    };
    for l in b.audit() {
        assert!(l.sample.chars().count() <= 13, "样本太长：{}", l.sample);
        assert!(!l.sample.contains("very-long-secret"), "{}", l.sample);
    }
}

#[test]
fn 短的斜杠片段不被误报为路径() {
    // "/" 之类的分隔符到处都是，全报会让自检结果被噪声淹没。
    let b = ReplayBundle {
        spec_version: SpecVersion(1),
        salt: 盐(),
        events: vec![事件(1, 结果(StepOutcome::Succeeded, Some("a/b")))],
        projection_versions: 版本表(),
    };
    assert!(b.audit().is_empty(), "{:?}", b.audit());
}

#[test]
fn 经过脱敏的路径不会被自检误报() {
    // opaque 替身与相对路径都不是绝对路径。
    let r = 脱敏器();
    for p in ["/home/alice/proj/src/x.rs", "/etc/passwd"] {
        let t = r.path(p).to;
        assert!(find_absolute_path(&t).is_none(), "{t} 被误报");
    }
}

// ---------------------------------------------------------------------------
// base32
// ---------------------------------------------------------------------------

#[test]
fn 以_ascii_开头的多字节串不会让自检崩溃() {
    // 与 `prompts::lint` 里同一个 bug：`&p[1..3]` 切在多字节字符中间。
    // 两处各有一份实现，所以要各有一条回归测试。
    let b = ReplayBundle {
        spec_version: SpecVersion(1),
        salt: 盐(),
        events: vec![事件(
            1,
            结果(StepOutcome::Succeeded, Some("a条件 C条件 x：值 结果 ok")),
        )],
        projection_versions: 版本表(),
    };
    let _ = b.audit();
}

#[test]
fn 替身只含安全字符() {
    // 进 JSON、进文件名、进 URL 都不该需要转义。
    let t = 脱敏器().path("/etc/x").to;
    let body = t.strip_prefix("opaque:").unwrap();
    assert!(
        body.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
        "{body}"
    );
}

#[test]
fn 替身长度固定() {
    let r = Redactor::new(盐(), None);
    let a = r.path("/a").to.len();
    let b = r.path("/some/much/longer/path/indeed").to.len();
    assert_eq!(a, b, "长度泄漏了原路径的信息");
}
