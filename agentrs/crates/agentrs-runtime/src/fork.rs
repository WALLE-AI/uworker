//! 对话分叉（架构 §4.1.1，任务 T23）。
//!
//! 两个参考实现都有分叉而本方案初版没有——这是**遗漏而非有意省略**：
//! aionrs 的 `Session` 带 `forked_from`/`root_id` 谱系，Harness 提供
//! `ctx.sessions.fork(source, boundary?, childSessionId?)`。
//!
//! ## 六条规则
//!
//! 1. Fork 产生一个**新 Run**，不修改源 Run；
//! 2. 分叉点必须落在 **durable 事实**上，不能落在 live delta 上；
//! 3. 谱系只作记录，**加载时从不跟随**——新 Run 自带完整可重建前缀；
//! 4. 被引用的 `ContentRef` 必须由新 Run 的 checkpoint 重新 `retain`；
//! 5. 不继承任何 live 状态：inbox、未决审批、in-flight 工具、grant；
//! 6. 不能扩大权限：`AuthorityEnvelope` 由 Core 重新签发且不得更宽。
//!
//! 第 3 条是最容易做错的：若加载时跟随源 Run，源 Run 归档后新 Run 就废了。

use agentrs_contracts::authority::AuthorityEnvelope;
use agentrs_contracts::content::ContentRef;
use agentrs_contracts::event::{EventPayload, RunEventEnvelope};
use agentrs_contracts::ids::{EventSequence, RunId};
use agentrs_contracts::spec::{ConversationSnapshot, ForkSpec, RunSpec};

/// 分叉失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForkError {
    /// 分叉点超出源 Run 已有的 durable 事实。
    #[error("fork boundary {boundary} exceeds source log (latest {latest})")]
    BoundaryBeyondLog {
        /// 请求的分叉点。
        boundary: EventSequence,
        /// 源 Run 的最新序号。
        latest: EventSequence,
    },
    /// 新 Run 的授权宽于源 Run。**分叉不得成为提权路径。**
    #[error("fork must not widen authority")]
    AuthorityWidened,
    /// 新旧 `RunId` 相同——那不是分叉，是覆盖。
    #[error("fork target must be a new run id")]
    SameRunId,
}

/// 分叉派生的产物。
#[derive(Debug, Clone, PartialEq)]
pub struct Forked {
    /// 新 Run 的规格。
    pub spec: RunSpec,
    /// 谱系：直接来源。**只作记录，加载时不跟随。**
    pub forked_from: RunId,
    /// 分叉树的根。跨多层分叉保持稳定。
    pub root_id: RunId,
    /// 前缀引用的全部内容。**新 Run 必须重新 `retain` 它们**，
    /// 否则源 Run 归档后会出现悬空引用。
    pub refs_to_retain: Vec<ContentRef>,
}

/// 从源 Run 的 durable 前缀派生一个新 Run。
///
/// `source_events` 应为源 Run 按 `seq` 升序的 durable 事件；
/// `source_root` 是源 Run 所属分叉树的根（源 Run 自身即根时传它自己）。
pub fn fork(
    spec: &ForkSpec,
    source_events: &[RunEventEnvelope],
    source_spec: &RunSpec,
    source_root: Option<&RunId>,
    new_authority: AuthorityEnvelope,
) -> Result<Forked, ForkError> {
    if spec.new_run_id == spec.source_run_id {
        return Err(ForkError::SameRunId);
    }

    // **权限不得扩大**——分叉不是提权路径。
    if !source_spec.authority.contains(&new_authority) {
        return Err(ForkError::AuthorityWidened);
    }

    let durable: Vec<&RunEventEnvelope> = source_events.iter().filter(|e| e.is_durable()).collect();
    let latest = durable.last().and_then(|e| e.seq).unwrap_or(EventSequence(0));

    let boundary = match spec.boundary {
        None => latest,
        Some(b) if b > latest => return Err(ForkError::BoundaryBeyondLog { boundary: b, latest }),
        Some(b) => b,
    };

    // 收集前缀引用的全部内容。
    let refs_to_retain = collect_refs(&durable, boundary);

    let mut new_spec = source_spec.clone();
    new_spec.run_id = spec.new_run_id.clone();
    new_spec.authority = new_authority;
    new_spec.conversation = ConversationSnapshot {
        derived_from: Some(spec.source_run_id.clone()),
        up_to_seq: Some(boundary),
    };
    // **不继承任何 live 状态**：checkpoint 里可能挂着未决审批，必须清掉。
    new_spec.checkpoint = None;
    // ChangeSet 也不由分叉决定：它归宿主，宿主要么沿用（多轮对话里同一个会话）、
    // 要么开新的（另起一段工作）。分叉替它做主，两种都会做错一种。
    new_spec.change_set_id = None;

    Ok(Forked {
        spec: new_spec,
        forked_from: spec.source_run_id.clone(),
        root_id: source_root.cloned().unwrap_or_else(|| spec.source_run_id.clone()),
        refs_to_retain,
    })
}

fn collect_refs(durable: &[&RunEventEnvelope], boundary: EventSequence) -> Vec<ContentRef> {
    let mut out = Vec::new();
    for e in durable {
        if e.seq.map(|s| s > boundary).unwrap_or(false) {
            break;
        }
        if let EventPayload::StepResultRecorded { result } = &e.payload {
            out.extend(result.artifacts.iter().cloned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::authority::{CapabilityView, PermissionMode};
    use agentrs_contracts::content::{ContentRef, ContentScope};
    use agentrs_contracts::event::{Causality, Durability, Visibility};
    use agentrs_contracts::ids::{Digest, RunEpoch, Timestamp};
    use agentrs_contracts::spec::{
        ContextBudget, ExecutionBudget, ModelPolicy, RunCheckpoint, SystemContext,
    };
    use agentrs_contracts::version::SpecVersion;
    use agentrs_contracts::{StepOutcome, StepResult};

    use super::*;

    fn 信封(tools: &[&str]) -> AuthorityEnvelope {
        AuthorityEnvelope {
            id: "e".into(),
            workspaces: vec!["ws".into()],
            tools: tools.iter().map(|s| (*s).to_owned()).collect(),
            providers: vec!["p".into()],
            models: vec!["m".into()],
            max_depth: 2,
        }
    }

    fn 源规格() -> RunSpec {
        RunSpec {
            run_id: "r-src".into(),
            parent_run_id: None,
            conversation: ConversationSnapshot::default(),
            system_context: SystemContext::default(),
            authority: 信封(&["Read", "Write"]),
            initial_capabilities: CapabilityView {
                tools: vec!["Read".into()],
                providers: vec![],
                models: vec![],
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
            // 源 Run 写着某个 ChangeSet；分叉不该替宿主决定新 Run 用哪个。
            change_set_id: Some("cs-src".into()),
            checkpoint: Some(RunCheckpoint {
                spec_version: SpecVersion(1),
                up_to_seq: EventSequence(3),
                // 源 Run 挂着一个未决审批。
                pending_approval: Some("tok".into()),
            }),
            spec_version: SpecVersion(1),
            config: Default::default(),
        }
    }

    fn 内容(d: &str) -> ContentRef {
        ContentRef {
            digest: Digest::from_hex(d),
            len: 1,
            media_type: "text/plain".into(),
            scope: ContentScope::Run {
                run_id: "r-src".into(),
            },
        }
    }

    fn 事件(n: u64, payload: EventPayload, durable: bool) -> RunEventEnvelope {
        RunEventEnvelope {
            run_id: "r-src".into(),
            epoch: RunEpoch(1),
            event_id: format!("e{n}").into(),
            seq: if durable { Some(EventSequence(n)) } else { None },
            live_seq: if durable {
                None
            } else {
                Some(agentrs_contracts::ids::LiveSequence(n))
            },
            at: Timestamp(0),
            durability: if durable {
                Durability::DurableFact
            } else {
                Durability::LiveStream
            },
            visibility: Visibility::User,
            causality: Causality::default(),
            surface: None,
            payload,
        }
    }

    fn 带产物的结果(n: u64, d: &str) -> RunEventEnvelope {
        事件(
            n,
            EventPayload::StepResultRecorded {
                result: Box::new(StepResult {
                    step_id: "s".into(),
                    call_id: "c".into(),
                    outcome: StepOutcome::Succeeded,
                    effective_isolation: None,
                    artifacts: vec![内容(d)],
                    output: None,
                    at: Timestamp(0),
                }),
            },
            true,
        )
    }

    fn 规格(boundary: Option<u64>) -> ForkSpec {
        ForkSpec {
            source_run_id: "r-src".into(),
            boundary: boundary.map(EventSequence),
            new_run_id: "r-fork".into(),
        }
    }

    #[test]
    fn 分叉不替宿主决定新_run_写哪个_change_set() {
        // 沿用（同一段会话的下一轮）还是另开（另起一段工作），只有宿主知道。
        // 分叉替它做主，两种都会做错一种。
        let src = 源规格();
        assert!(src.change_set_id.is_some(), "源 Run 本来写着一个");
        let events = vec![事件(1, EventPayload::RunStarted, true)];
        let f = fork(&规格(None), &events, &src, None, 信封(&["Read"])).unwrap();
        assert_eq!(f.spec.change_set_id, None);
    }

    #[test]
    fn 分叉产生新_run_不修改源() {
        let src = 源规格();
        let events = vec![事件(1, EventPayload::RunStarted, true)];
        let f = fork(&规格(None), &events, &src, None, 信封(&["Read"])).unwrap();

        assert_eq!(f.spec.run_id.as_str(), "r-fork");
        assert_eq!(src.run_id.as_str(), "r-src", "源 Run 未被修改");
    }

    #[test]
    fn 谱系被记录但加载时不跟随() {
        // 若加载时跟随源 Run，源归档后新 Run 就废了。
        // 这里的检验方式：新 spec 的 conversation 自带完整前缀游标，
        // 不需要在运行期回头查源 Run。
        let events = vec![事件(1, EventPayload::RunStarted, true)];
        let f = fork(&规格(None), &events, &源规格(), None, 信封(&["Read"])).unwrap();

        assert_eq!(f.forked_from.as_str(), "r-src");
        assert_eq!(f.spec.conversation.up_to_seq, Some(EventSequence(1)));
        assert_eq!(
            f.spec.conversation.derived_from.as_ref().unwrap().as_str(),
            "r-src"
        );
    }

    #[test]
    fn 根标识跨多层分叉保持稳定() {
        let events = vec![事件(1, EventPayload::RunStarted, true)];
        let root: RunId = "r-root".into();
        let f = fork(&规格(None), &events, &源规格(), Some(&root), 信封(&["Read"])).unwrap();
        assert_eq!(f.root_id, root, "fork of a fork 仍指向同一个根");
    }

    #[test]
    fn 不继承任何_live_状态() {
        // 源 Run 挂着未决审批；新 Run 不得带着它启动。
        let src = 源规格();
        assert!(src.checkpoint.as_ref().unwrap().pending_approval.is_some());

        let events = vec![事件(1, EventPayload::RunStarted, true)];
        let f = fork(&规格(None), &events, &src, None, 信封(&["Read"])).unwrap();
        assert!(f.spec.checkpoint.is_none(), "未决审批不得被继承");
    }

    #[test]
    fn 不能扩大权限() {
        // 分叉不是提权路径。
        let events = vec![事件(1, EventPayload::RunStarted, true)];
        let 更宽 = 信封(&["Read", "Write", "Exec"]);
        assert_eq!(
            fork(&规格(None), &events, &源规格(), None, 更宽),
            Err(ForkError::AuthorityWidened)
        );
    }

    #[test]
    fn 分叉点必须落在_durable_事实上() {
        // boundary 类型是 EventSequence 而非 LiveSequence——
        // 类型上就不可能指向一条 live delta。
        let events = vec![
            事件(1, EventPayload::RunStarted, true),
            事件(2, EventPayload::TextDelta { text: "x".into() }, false), // live，不计入
        ];
        let f = fork(&规格(None), &events, &源规格(), None, 信封(&["Read"])).unwrap();
        assert_eq!(
            f.spec.conversation.up_to_seq,
            Some(EventSequence(1)),
            "live 事件不参与分叉点计算"
        );
    }

    #[test]
    fn 超出日志的分叉点被拒绝() {
        let events = vec![事件(1, EventPayload::RunStarted, true)];
        assert_eq!(
            fork(&规格(Some(99)), &events, &源规格(), None, 信封(&["Read"])),
            Err(ForkError::BoundaryBeyondLog {
                boundary: EventSequence(99),
                latest: EventSequence(1)
            })
        );
    }

    #[test]
    fn 同_id_不是分叉而是覆盖() {
        let mut spec = 规格(None);
        spec.new_run_id = "r-src".into();
        let events = vec![事件(1, EventPayload::RunStarted, true)];
        assert_eq!(
            fork(&spec, &events, &源规格(), None, 信封(&["Read"])),
            Err(ForkError::SameRunId)
        );
    }

    #[test]
    fn 前缀引用的内容被收集以便重新_retain() {
        // 源 Run 归档后新 Run 仍要能解引用——因此必须由新 Run 自己 retain。
        let events = vec![
            事件(1, EventPayload::RunStarted, true),
            带产物的结果(2, "aaa"),
            带产物的结果(3, "bbb"),
        ];
        let f = fork(&规格(None), &events, &源规格(), None, 信封(&["Read"])).unwrap();
        assert_eq!(f.refs_to_retain.len(), 2);
    }

    #[test]
    fn 分叉点之后的引用不被收集() {
        let events = vec![
            带产物的结果(1, "before"),
            带产物的结果(2, "before2"),
            带产物的结果(3, "after"),
        ];
        let f = fork(&规格(Some(2)), &events, &源规格(), None, 信封(&["Read"])).unwrap();
        assert_eq!(f.refs_to_retain.len(), 2, "只收集分叉点及之前的引用");
    }

    #[test]
    fn 从中途分叉截断前缀() {
        let events = vec![
            事件(1, EventPayload::RunStarted, true),
            事件(2, EventPayload::TurnStarted, true),
            事件(3, EventPayload::AssistantMessage, true),
        ];
        let f = fork(&规格(Some(2)), &events, &源规格(), None, 信封(&["Read"])).unwrap();
        assert_eq!(f.spec.conversation.up_to_seq, Some(EventSequence(2)));
    }
}
