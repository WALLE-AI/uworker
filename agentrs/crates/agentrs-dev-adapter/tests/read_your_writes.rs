//! ChangeSet overlay 的读己之写（M1 出口标准，架构 §8.2、宿主义务 H7）。
//!
//! > Edit 后 Read / Grep / 构建在同一 ChangeSet overlay 上读到新内容。
//!
//! ## 为什么这条是硬标准而不是"最好有"
//!
//! Agent 的典型动作序列是 **读 → 改 → 再读 → 再改**。若中间那次读绕过 overlay：
//!
//! - 第二次 `Edit` 基于盘上的旧内容，**把第一次的改动悄悄抹掉**；
//! - `Grep` 搜不到刚写的文件，模型据此断定"这个函数不存在"然后重复实现一遍；
//! - 删除只是看起来生效了，后续读仍命中旧内容。
//!
//! 三种都不报错，都表现为"Agent 干得莫名其妙"。
//!
//! ## overlay 不是缓存
//!
//! 它是**未提交事实的唯一所在**——磁盘上根本没有那份内容。
//! 因此"读不到就回落到磁盘"这种缓存式写法在删除那条路径上直接错：
//! 必须有墓碑，见 [`删除后读不到`]。

use agentrs_contracts::ids::{Digest, ExecutionId, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{ExecutionOutcome, ExecutionRequest, ExecutionResult, IsolationLevel};
use agentrs_dev_adapter::LocalFileSandbox;

const CS: &str = "cs-1";

struct 夹具 {
    sb: LocalFileSandbox,
    _dir: tempdir::TempDir,
    n: std::cell::Cell<u32>,
}

impl 夹具 {
    fn new() -> Self {
        let dir = tempdir::TempDir::new("agentrs-ryw").unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn old() {}\n").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.rs"), "fn keep() {}\n").unwrap();
        let sb = LocalFileSandbox::new(dir.path()).unwrap();
        Self {
            sb,
            _dir: dir,
            n: std::cell::Cell::new(0),
        }
    }

    /// 跑一个工具。每次用新的 grant——grant 是一次性的（H1）。
    async fn 跑(&self, cs: &str, tool: &str, args: serde_json::Value) -> ExecutionResult {
        let i = self.n.get();
        self.n.set(i + 1);
        let gid = format!("g{i}");
        let hash = InputHash(Digest::from_hex(format!("h{i}")));
        self.sb.issue_grant(&gid, hash.clone(), Timestamp(i64::MAX));

        self.sb
            .execute(
                SandboxGrant {
                    grant_id: gid,
                    payload: serde_json::json!({}),
                },
                ExecutionRequest {
                    execution_id: ExecutionId::new(format!("e{i}")),
                    tool_name: tool.into(),
                    arguments: args,
                    change_set_id: cs.into(),
                    input_hash: hash,
                    required_isolation: IsolationLevel::L0BasicContainment,
                },
            )
            .await
            .expect("执行失败")
    }

    async fn 读(&self, cs: &str, path: &str) -> ExecutionResult {
        self.跑(cs, "Read", serde_json::json!({ "path": path })).await
    }

    async fn 写(&self, cs: &str, path: &str, content: &str) -> ExecutionResult {
        self.跑(cs, "Write", serde_json::json!({"path": path, "content": content}))
            .await
    }

    async fn 改(&self, cs: &str, path: &str, old: &str, new: &str) -> ExecutionResult {
        self.跑(
            cs,
            "Edit",
            serde_json::json!({"path": path, "old": old, "new": new}),
        )
        .await
    }

    async fn 搜(&self, cs: &str, pattern: &str) -> String {
        self.跑(cs, "Grep", serde_json::json!({ "pattern": pattern }))
            .await
            .output
            .unwrap_or_default()
    }
}

fn 成功(r: &ExecutionResult) -> &str {
    assert_eq!(
        r.outcome,
        ExecutionOutcome::Completed { exit_code: 0 },
        "期望成功，实际 {:?}：{:?}",
        r.outcome,
        r.output
    );
    r.output.as_deref().unwrap_or_default()
}

// ---- Read ----

#[tokio::test]
async fn 写后立刻读到新内容() {
    let f = 夹具::new();
    f.写(CS, "a.rs", "fn brand_new() {}\n").await;
    assert_eq!(成功(&f.读(CS, "a.rs").await), "fn brand_new() {}\n");
}

#[tokio::test]
async fn 写入的新文件磁盘上并不存在() {
    // overlay 不是缓存——它是未提交事实的唯一所在。
    let f = 夹具::new();
    f.写(CS, "新建.rs", "fn x() {}\n").await;
    assert_eq!(成功(&f.读(CS, "新建.rs").await), "fn x() {}\n");
    assert!(
        !f.sb.root().join("新建.rs").exists(),
        "内核无提交权，改动不得落盘"
    );
}

// ---- Edit ----

#[tokio::test]
async fn 连续两次_edit_不会互相抹掉() {
    // **这是整条标准最要紧的一个用例。**
    // 第二次 Edit 若基于盘上的旧内容，第一次的改动就没了——而且不报错。
    let f = 夹具::new();
    成功(&f.改(CS, "a.rs", "old", "first").await);
    成功(&f.改(CS, "a.rs", "fn ", "pub fn ").await);

    let text = 成功(&f.读(CS, "a.rs").await).to_string();
    assert!(text.contains("first"), "第一次 Edit 的改动被抹掉了：{text}");
    assert!(text.contains("pub fn"), "第二次 Edit 没生效：{text}");
    assert_eq!(text, "pub fn first() {}\n");
}

#[tokio::test]
async fn edit_能改到只存在于_overlay_里的文件() {
    let f = 夹具::new();
    f.写(CS, "仅内存.rs", "fn a() {}\n").await;
    成功(&f.改(CS, "仅内存.rs", "a()", "b()").await);
    assert_eq!(成功(&f.读(CS, "仅内存.rs").await), "fn b() {}\n");
}

#[tokio::test]
async fn edit_找不到目标文本时报失败而不是静默改坏() {
    let f = 夹具::new();
    let r = f.改(CS, "a.rs", "根本不存在的串", "x").await;
    assert_eq!(r.outcome, ExecutionOutcome::Completed { exit_code: 1 });
    // 原文件不受影响。
    assert_eq!(成功(&f.读(CS, "a.rs").await), "fn old() {}\n");
}

#[tokio::test]
async fn edit_失败信息不透传待替换正文() {
    // old/new 可能含用户内容或密钥。
    let f = 夹具::new();
    let r = f.改(CS, "a.rs", "sk-secret-token", "x").await;
    assert!(
        !r.output.unwrap_or_default().contains("sk-secret-token"),
        "错误信息不得回显待替换正文"
    );
}

// ---- Grep ----

#[tokio::test]
async fn grep_能搜到只存在于_overlay_里的文件() {
    // 搜不到刚写的文件，模型会断定"这个函数不存在"然后重复实现一遍。
    let f = 夹具::new();
    f.写(CS, "新模块.rs", "fn 独特标记() {}\n").await;
    let hits = f.搜(CS, "独特标记").await;
    assert!(hits.contains("新模块.rs"), "Grep 漏掉了 overlay 里的文件：{hits}");
}

#[tokio::test]
async fn grep_看到的是改后的内容() {
    let f = 夹具::new();
    成功(&f.改(CS, "a.rs", "old", "改过了").await);

    let 新 = f.搜(CS, "改过了").await;
    assert!(新.contains("a.rs"), "Grep 没看到改后的内容：{新}");

    let 旧 = f.搜(CS, "fn old").await;
    assert_eq!(旧, "无匹配", "Grep 仍能搜到改前的内容：{旧}");
}

#[tokio::test]
async fn grep_覆盖子目录() {
    let f = 夹具::new();
    assert!(f.搜(CS, "fn keep").await.contains("b.rs"));
}

#[tokio::test]
async fn grep_带出行号() {
    let f = 夹具::new();
    f.写(CS, "多行.rs", "一\n二\n目标\n").await;
    let hits = f.搜(CS, "目标").await;
    assert!(hits.contains(":3:"), "缺少行号：{hits}");
}

// ---- 删除 ----

#[tokio::test]
async fn 删除后读不到() {
    // **墓碑不能省。** 只记写入的话，overlay 未命中就回落磁盘，
    // 删除只是看起来生效了。
    let f = 夹具::new();
    成功(&f.跑(CS, "Delete", serde_json::json!({"path": "a.rs"})).await);

    let r = f.读(CS, "a.rs").await;
    assert_eq!(
        r.outcome,
        ExecutionOutcome::Completed { exit_code: 1 },
        "删除后仍读到内容：{:?}",
        r.output
    );
    // 磁盘上还在——没提交就不该动它。
    assert!(f.sb.root().join("a.rs").exists());
}

#[tokio::test]
async fn 删除后搜不到() {
    let f = 夹具::new();
    成功(&f.跑(CS, "Delete", serde_json::json!({"path": "a.rs"})).await);
    assert_eq!(f.搜(CS, "fn old").await, "无匹配");
}

#[tokio::test]
async fn 删除后可以重新写回() {
    let f = 夹具::new();
    f.跑(CS, "Delete", serde_json::json!({"path": "a.rs"})).await;
    f.写(CS, "a.rs", "fn 复活() {}\n").await;
    assert_eq!(成功(&f.读(CS, "a.rs").await), "fn 复活() {}\n");
}

// ---- ChangeSet 隔离 ----

#[tokio::test]
async fn 一个_change_set_的改动不影响另一个() {
    // 同一命令在不同 ChangeSet 上是不同的意图（§8.2）——
    // 视图也必须跟着分开，否则并行的两条支线会互相污染。
    let f = 夹具::new();
    f.写(CS, "a.rs", "来自 cs1\n").await;

    assert_eq!(成功(&f.读("cs-2", "a.rs").await), "fn old() {}\n");
    assert_eq!(f.搜("cs-2", "来自 cs1").await, "无匹配");
}

#[tokio::test]
async fn 一个_change_set_的删除不影响另一个() {
    let f = 夹具::new();
    f.跑(CS, "Delete", serde_json::json!({"path": "a.rs"})).await;
    assert_eq!(成功(&f.读("cs-2", "a.rs").await), "fn old() {}\n");
}

// ---- 提交 ----

#[tokio::test]
async fn 提交后改动才落盘() {
    let f = 夹具::new();
    f.写(CS, "新建.rs", "x\n").await;
    成功(&f.改(CS, "a.rs", "old", "new").await);

    assert_eq!(f.sb.pending_count(CS), 2);
    assert_eq!(f.sb.commit(CS).unwrap(), 2);

    assert_eq!(
        std::fs::read_to_string(f.sb.root().join("a.rs")).unwrap(),
        "fn new() {}\n"
    );
    assert!(f.sb.root().join("新建.rs").exists());
    assert_eq!(f.sb.pending_count(CS), 0, "提交后 overlay 应清空");
}

#[tokio::test]
async fn 提交墓碑会真的删掉文件() {
    let f = 夹具::new();
    f.跑(CS, "Delete", serde_json::json!({"path": "a.rs"})).await;
    assert!(f.sb.root().join("a.rs").exists(), "提交前不动磁盘");

    f.sb.commit(CS).unwrap();
    assert!(!f.sb.root().join("a.rs").exists());
}

#[tokio::test]
async fn 提交后读回落到磁盘且内容一致() {
    // 提交前后模型看到的内容必须一样，否则"提交"这个动作会改变语义。
    let f = 夹具::new();
    成功(&f.改(CS, "a.rs", "old", "same").await);
    let 提交前 = 成功(&f.读(CS, "a.rs").await).to_string();

    f.sb.commit(CS).unwrap();
    let 提交后 = 成功(&f.读(CS, "a.rs").await).to_string();

    assert_eq!(提交前, 提交后);
}

// ---- cwd jail 仍然生效 ----

#[tokio::test]
async fn overlay_不会成为逃逸工作区的通道() {
    // 新增的 Edit/Delete/Grep 三条路径都必须过同一道 resolve()。
    let f = 夹具::new();
    for (tool, args) in [
        (
            "Edit",
            serde_json::json!({"path": "../外面.rs", "old": "a", "new": "b"}),
        ),
        ("Delete", serde_json::json!({"path": "../外面.rs"})),
        (
            "Write",
            serde_json::json!({"path": "/etc/passwd", "content": "x"}),
        ),
    ] {
        let r = f.跑(CS, tool, args).await;
        assert!(
            matches!(r.outcome, ExecutionOutcome::Rejected { .. }),
            "{tool} 允许了越界路径"
        );
    }
}

#[tokio::test]
async fn grep_不会泄露工作区之外的内容() {
    let f = 夹具::new();
    let hits = f.搜(CS, "root:").await;
    assert!(!hits.contains("/etc"), "Grep 越出了工作区：{hits}");
}
