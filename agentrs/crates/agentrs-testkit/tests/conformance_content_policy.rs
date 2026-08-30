//! H4 / H5 conformance suite 自身的验证。
//!
//! 与前两组同一套路：每条检查都要有一个能让它失败的具体实现，
//! 并且**只让它自己失败**。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agentrs_contracts::content::{
    ByteRange, ContentError, ContentMeta, ContentRef, ContentScope, ContentStat, RetentionOwner,
};
use agentrs_contracts::ids::{ApprovalToken, Deadline, Digest, RunId, Timestamp};
use agentrs_contracts::policy::{
    ApprovalDecision, ApprovalOutcome, ApprovalRequest, DecisionSource, InputHash, PolicyDecision,
    PolicyError, SandboxGrant, ToolProposal,
};
use agentrs_contracts::ports::{ContentStore, PolicyEnforcer};
use agentrs_testkit::conformance::content::{check_content, ContentSubject};
use agentrs_testkit::conformance::policy::{check_policy, PolicySubject};
use agentrs_testkit::conformance::Report;
use bytes::Bytes;

fn 只有这些不合格(r: &Report, 期望: &[&str]) {
    let 实际: Vec<&str> = r.failures().iter().map(|c| c.name).collect();
    assert_eq!(实际, 期望, "捕获的违规项与预期不符\n{}", r.render());
    assert!(r.skipped().is_empty(), "本组不应产生跳过项\n{}", r.render());
}

// ===========================================================================
// H4：ContentStore
// ===========================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct 内容缺陷 {
    每次写入产生新引用: bool,
    回收无视_retain: bool,
    区间读越界: bool,
    stat_长度撒谎: bool,
    跨_scope_报_not_found: bool,
    跨_scope_直接放行: bool,
    release_后报后端错误: bool,
}

#[derive(Default)]
struct 内容状态 {
    blobs: HashMap<Digest, (Bytes, ContentScope)>,
    retained: Vec<Digest>,
    released: bool,
    序号: u64,
}

struct 可控内容库 {
    缺陷: 内容缺陷,
    st: Mutex<内容状态>,
}

impl 可控内容库 {
    fn new(缺陷: 内容缺陷) -> Arc<Self> {
        Arc::new(Self {
            缺陷,
            st: Mutex::new(内容状态::default()),
        })
    }

    fn 摘要(bytes: &Bytes) -> Digest {
        Digest::from_hex(format!("d{}", bytes.len()))
    }
}

#[async_trait::async_trait]
impl ContentStore for 可控内容库 {
    async fn put(
        &self,
        scope: ContentScope,
        bytes: Bytes,
        _meta: ContentMeta,
    ) -> Result<ContentRef, ContentError> {
        let mut st = self.st.lock().unwrap();
        let digest = if self.缺陷.每次写入产生新引用 {
            st.序号 += 1;
            Digest::from_hex(format!("d-{}", st.序号))
        } else {
            Self::摘要(&bytes)
        };
        let len = bytes.len() as u64;
        st.blobs.insert(digest.clone(), (bytes, scope.clone()));
        Ok(ContentRef {
            digest,
            len,
            media_type: "application/octet-stream".into(),
            scope,
        })
    }

    async fn get(&self, r: &ContentRef) -> Result<Bytes, ContentError> {
        let st = self.st.lock().unwrap();
        let Some((bytes, owner_scope)) = st.blobs.get(&r.digest) else {
            return Err(ContentError::NotFound {
                digest: r.digest.clone(),
            });
        };
        if *owner_scope != r.scope && !self.缺陷.跨_scope_直接放行 {
            return Err(if self.缺陷.跨_scope_报_not_found {
                ContentError::NotFound {
                    digest: r.digest.clone(),
                }
            } else {
                ContentError::Forbidden
            });
        }
        // 已释放且回收过：报 NotFound（或按缺陷报后端错误）。
        if st.released && !st.retained.contains(&r.digest) {
            return Err(if self.缺陷.release_后报后端错误 {
                ContentError::Backend {
                    message: "corrupted".into(),
                }
            } else {
                ContentError::NotFound {
                    digest: r.digest.clone(),
                }
            });
        }
        Ok(bytes.clone())
    }

    async fn get_range(&self, r: &ContentRef, range: ByteRange) -> Result<Bytes, ContentError> {
        let all = self.get(r).await?;
        let (s, e) = if self.缺陷.区间读越界 {
            // 差一错：把闭开区间当成闭区间。
            (range.start as usize, (range.end + 1) as usize)
        } else {
            (range.start as usize, range.end as usize)
        };
        Ok(all.slice(s.min(all.len())..e.min(all.len())))
    }

    async fn stat(&self, r: &ContentRef) -> Result<ContentStat, ContentError> {
        let bytes = self.get(r).await?;
        Ok(ContentStat {
            len: if self.缺陷.stat_长度撒谎 {
                bytes.len() as u64 + 1
            } else {
                bytes.len() as u64
            },
            media_type: r.media_type.clone(),
            created_at: Timestamp(0),
        })
    }

    async fn retain(&self, _o: RetentionOwner, refs: &[ContentRef]) -> Result<(), ContentError> {
        if self.缺陷.回收无视_retain {
            return Ok(());
        }
        let mut st = self.st.lock().unwrap();
        for r in refs {
            st.retained.push(r.digest.clone());
        }
        Ok(())
    }

    async fn release(&self, _o: RetentionOwner) -> Result<(), ContentError> {
        let mut st = self.st.lock().unwrap();
        st.retained.clear();
        Ok(())
    }
}

struct 受检内容库(Arc<可控内容库>);

impl ContentSubject for 受检内容库 {
    fn store(&self) -> Arc<dyn ContentStore> {
        self.0.clone()
    }

    fn try_collect_garbage(&self) -> bool {
        // 回收 = 把未被 retain 的内容标记为不可读。
        let mut st = self.0.st.lock().unwrap();
        st.released = true;
        true
    }

    fn foreign_scope(&self) -> Option<ContentScope> {
        Some(ContentScope::Run {
            run_id: RunId::new("另一个-run"),
        })
    }
}

async fn 跑内容(缺陷: 内容缺陷) -> Report {
    check_content(&受检内容库(可控内容库::new(缺陷))).await
}

#[tokio::test]
async fn 合格的内容库全部通过() {
    let r = 跑内容(内容缺陷::default()).await;
    assert!(r.passed(), "合格实现被误判\n{}", r.render());
    assert!(r.skipped().is_empty(), "{}", r.render());
}

#[tokio::test]
async fn 捕获_同一份字节产生不同引用() {
    // 去重、缓存前缀、"这两条历史引用的是不是同一份内容"全靠它。
    只有这些不合格(
        &跑内容(内容缺陷 {
            每次写入产生新引用: true,
            ..Default::default()
        })
        .await,
        &["同 scope 同字节产生同一引用"],
    );
}

#[tokio::test]
async fn 捕获_retain_期内内容被回收() {
    // **H4 的正题。** 表现极其滞后：Run 当时跑得好好的，
    // 几天后恢复才发现 manifest 引用的内容没了。
    只有这些不合格(
        &跑内容(内容缺陷 {
            回收无视_retain: true,
            ..Default::default()
        })
        .await,
        &["retain 期内内容不被回收"],
    );
}

#[tokio::test]
async fn 捕获_区间读差一() {
    只有这些不合格(
        &跑内容(内容缺陷 {
            区间读越界: true,
            ..Default::default()
        })
        .await,
        &["区间读返回准确的切片"],
    );
}

#[tokio::test]
async fn 捕获_stat_与引用不自洽() {
    只有这些不合格(
        &跑内容(内容缺陷 {
            stat_长度撒谎: true,
            ..Default::default()
        })
        .await,
        &["stat 与引用自洽"],
    );
}

#[tokio::test]
async fn 捕获_跨_scope_报了_not_found() {
    // NotFound 会让人去查"是不是被 GC 了"，Forbidden 才指向可见性收窄。
    只有这些不合格(
        &跑内容(内容缺陷 {
            跨_scope_报_not_found: true,
            ..Default::default()
        })
        .await,
        &["跨 scope 访问报 Forbidden"],
    );
}

#[tokio::test]
async fn 捕获_跨_scope_直接放行() {
    只有这些不合格(
        &跑内容(内容缺陷 {
            跨_scope_直接放行: true,
            ..Default::default()
        })
        .await,
        &["跨 scope 访问报 Forbidden"],
    );
}

#[tokio::test]
async fn 捕获_release_后报了误导性错误() {
    只有这些不合格(
        &跑内容(内容缺陷 {
            release_后报后端错误: true,
            ..Default::default()
        })
        .await,
        &["release 后若已回收则报 NotFound"],
    );
}

// ===========================================================================
// H5：PolicyEnforcer
// ===========================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct 策略缺陷 {
    grant_绑到别处: bool,
    grant_永不过期: bool,
    未知令牌也兑现: bool,
    令牌可重复兑现: bool,
    过期_deadline_报错: bool,
}

struct 可控策略 {
    缺陷: 策略缺陷,
    已兑现: Mutex<Vec<String>>,
}

impl 可控策略 {
    fn new(缺陷: 策略缺陷) -> Arc<Self> {
        Arc::new(Self {
            缺陷,
            已兑现: Mutex::new(Vec::new()),
        })
    }
}

const 现在: i64 = 1_000;

#[async_trait::async_trait]
impl PolicyEnforcer for 可控策略 {
    async fn evaluate(&self, proposal: ToolProposal) -> Result<PolicyDecision, PolicyError> {
        if proposal.tool_name == "NeedsApproval" {
            return Ok(PolicyDecision::RequireApproval(ApprovalRequest {
                step_id: "s".into(),
                proposal,
                risk_summary: "".into(),
                originating_member: None,
                team_id: None,
            }));
        }
        Ok(PolicyDecision::Allow {
            grant: SandboxGrant {
                grant_id: "g".into(),
                payload: serde_json::json!({}),
            },
            bound_input_hash: if self.缺陷.grant_绑到别处 {
                InputHash(Digest::from_hex("别的输入"))
            } else {
                proposal.input_hash
            },
            expires_at: if self.缺陷.grant_永不过期 {
                Timestamp(i64::MAX)
            } else {
                Timestamp(现在 + 60_000)
            },
        })
    }

    async fn await_approval(
        &self,
        _req: ApprovalRequest,
        deadline: Deadline,
    ) -> Result<ApprovalOutcome, PolicyError> {
        if deadline.0 .0 <= 现在 && self.缺陷.过期_deadline_报错 {
            return Err(PolicyError::Unavailable);
        }
        Ok(ApprovalOutcome::Pending {
            resume_token: ApprovalToken::new("conf-token"),
        })
    }

    async fn redeem(&self, token: ApprovalToken) -> Result<ApprovalOutcome, PolicyError> {
        let id = token.as_str().to_string();
        if id != "conf-token" && !self.缺陷.未知令牌也兑现 {
            return Err(PolicyError::Unavailable);
        }
        let mut 已 = self.已兑现.lock().unwrap();
        if 已.contains(&id) && !self.缺陷.令牌可重复兑现 {
            return Err(PolicyError::Unavailable);
        }
        已.push(id);
        Ok(ApprovalOutcome::Decided(ApprovalDecision {
            allowed: true,
            source: DecisionSource::Human { user_id: "u1".into() },
            grant: None,
            decided_at: Timestamp(现在),
        }))
    }
}

struct 受检策略(Arc<可控策略>);

impl PolicySubject for 受检策略 {
    fn policy(&self) -> Arc<dyn PolicyEnforcer> {
        self.0.clone()
    }

    fn allowed_proposal(&self, tag: &str) -> ToolProposal {
        ToolProposal {
            step_id: "s-conformance".into(),
            call_id: tag.into(),
            tool_name: "Write".into(),
            arguments: serde_json::json!({}),
            workspace_id: "ws".into(),
            change_set_id: "cs".into(),
            input_hash: InputHash(Digest::from_hex(tag)),
        }
    }

    fn approval_proposal(&self, tag: &str) -> Option<ToolProposal> {
        let mut p = self.allowed_proposal(tag);
        p.tool_name = "NeedsApproval".into();
        Some(p)
    }

    fn now(&self) -> Timestamp {
        Timestamp(现在)
    }
}

async fn 跑策略(缺陷: 策略缺陷) -> Report {
    check_policy(&受检策略(可控策略::new(缺陷))).await
}

#[tokio::test]
async fn 合格的策略实现全部通过() {
    let r = 跑策略(策略缺陷::default()).await;
    assert!(r.passed(), "合格实现被误判\n{}", r.render());
    assert!(r.skipped().is_empty(), "{}", r.render());
}

#[tokio::test]
async fn 捕获_grant_绑到了别的输入上() {
    只有这些不合格(
        &跑策略(策略缺陷 {
            grant_绑到别处: true,
            ..Default::default()
        })
        .await,
        &["Allow 绑定到本次提议的 input_hash"],
    );
}

#[tokio::test]
async fn 捕获_grant_永不过期() {
    // 测试替身里常见，产品实现里是真问题：一次批准等于永久批准。
    只有这些不合格(
        &跑策略(策略缺陷 {
            grant_永不过期: true,
            ..Default::default()
        })
        .await,
        &["Allow 带有限的有效期"],
    );
}

#[tokio::test]
async fn 捕获_未知令牌也能兑现() {
    只有这些不合格(
        &跑策略(策略缺陷 {
            未知令牌也兑现: true,
            ..Default::default()
        })
        .await,
        &["未知令牌 redeem 失败"],
    );
}

#[tokio::test]
async fn 捕获_令牌可重复兑现() {
    只有这些不合格(
        &跑策略(策略缺陷 {
            令牌可重复兑现: true,
            ..Default::default()
        })
        .await,
        &["令牌一次性兑现"],
    );
}

#[tokio::test]
async fn 捕获_过期_deadline_报错而非降级为挂起() {
    // 报错会让内核把"人没来得及批"当成"策略坏了"，
    // 于是 Run 失败而不是挂起等人——用户的工作就丢了。
    let r = 跑策略(策略缺陷 {
        过期_deadline_报错: true,
        ..Default::default()
    })
    .await;

    let 不合格: Vec<&str> = r.failures().iter().map(|c| c.name).collect();
    assert_eq!(不合格, ["已过期的 deadline 立刻返回"], "{}", r.render());

    // 令牌检查会因为拿不到令牌而**跳过**，不是跟着报错。
    // 一个缺陷只该产生一条不合格，否则修的人不知道该先看哪条。
    let 跳过: Vec<&str> = r.skipped().iter().map(|c| c.name).collect();
    assert_eq!(跳过, ["令牌一次性兑现"], "{}", r.render());
}

#[tokio::test]
async fn 不提供审批提议时令牌检查记为跳过() {
    struct 只放行(Arc<可控策略>);
    impl PolicySubject for 只放行 {
        fn policy(&self) -> Arc<dyn PolicyEnforcer> {
            self.0.clone()
        }
        fn allowed_proposal(&self, tag: &str) -> ToolProposal {
            受检策略(self.0.clone()).allowed_proposal(tag)
        }
        fn now(&self) -> Timestamp {
            Timestamp(现在)
        }
        // 不覆盖 approval_proposal，默认 None。
    }

    let r = check_policy(&只放行(可控策略::new(策略缺陷::default()))).await;
    let 跳过: Vec<&str> = r.skipped().iter().map(|c| c.name).collect();
    assert_eq!(跳过, ["令牌一次性兑现", "已过期的 deadline 立刻返回"]);
    assert!(r.render().contains("跳过不等于通过"), "{}", r.render());
}
