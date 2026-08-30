//! 可重建的模型请求装配（任务 T11）。
//!
//! 装配器只接收已经由 Surface 投影得到的消息。ContentRef 必须先经
//! [`resolve_text_refs`] 解引用并追加为 Surface 事件，不能从这里旁路注入模型历史。

use std::collections::HashSet;

use agentrs_contracts::content::{ContentError, ContentRef, RetentionOwner, UnresolvedReason};
use agentrs_contracts::ids::{Digest, EventRange, MemoryId, ModelId, RequestId, SkillId};
use agentrs_contracts::manifest::{
    CacheSegment, LegalizationOp, ModelRequestManifest, OperationView, TokenAccounting,
};
use agentrs_contracts::ports::{ContentStore, TokenCounter};
use agentrs_types::{Message, ToolDef};

use crate::budget::{trim_to_budget, ConservativeEstimator, Fragment, Priority};
use crate::cache::{CacheLayout, Segment};

const MANIFEST_LIMIT: usize = 64 * 1024;

/// 一条带预算优先级的 Surface 投影消息。
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedMessage {
    /// 稳定诊断标签，也用于把裁剪结果映射回消息。
    pub label: String,
    /// 裁剪优先级。
    pub priority: Priority,
    /// 由 Surface 投影得到的消息。
    pub message: Message,
}

/// 装配所需的全部可重建输入。
#[derive(Debug, Clone)]
pub struct ContextPlanInput {
    /// 请求标识。
    pub request_id: RequestId,
    /// 目标模型，供精确 TokenCounter 使用。
    pub model: ModelId,
    /// 冻结的 operation 依赖视图。
    pub operation_view: OperationView,
    /// 覆盖的 durable 事实区间。
    pub source_event_range: EventRange,
    /// 系统段正文。其来源是 RunSpec；对应 ref 记录在 manifest 中。
    pub system: String,
    /// 系统段引用。
    pub system_sections: Vec<ContentRef>,
    /// Surface 投影消息。
    pub messages: Vec<PlannedMessage>,
    /// 已经按 CapabilityView 投影的工具目录。
    pub tools: Vec<ToolDef>,
    /// 已选择的记忆标识。
    pub memory_fragments: Vec<MemoryId>,
    /// 已启用的技能标识。
    pub skill_fragments: Vec<SkillId>,
    /// 压缩产物引用。
    pub compaction_refs: Vec<ContentRef>,
    /// 实际成功解引用、需要 retain 的全部内容。
    pub resolved_content_refs: Vec<ContentRef>,
    /// Surface 当前摘要。
    pub surface_digest: Digest,
    /// Surface 最近一次改写的起点。
    pub surface_invalidation: Option<agentrs_contracts::ids::EventSequence>,
    /// 历史合法化留痕。
    pub legalization_ops: Vec<LegalizationOp>,
    /// ContentRef 解引用降级留痕。
    pub unresolved: Vec<UnresolvedReason>,
    /// 输入窗口上限。
    pub max_input_tokens: u64,
    /// 输出预留。
    pub reserved_output_tokens: u64,
}

/// 装配后的请求内容与一一对应的 manifest。
#[derive(Debug, Clone, PartialEq)]
pub struct ContextPlan {
    /// 系统段。
    pub system: String,
    /// 实际发送的消息。
    pub messages: Vec<Message>,
    /// 实际发送的工具目录。
    pub tools: Vec<ToolDef>,
    /// 请求构成说明。
    pub manifest: ModelRequestManifest,
    /// 因预算被裁掉的诊断标签。
    pub dropped: Vec<String>,
}

/// 装配失败。
#[derive(Debug, thiserror::Error)]
pub enum AssembleError {
    /// 安全固定段自身已经超过输入窗口。
    #[error("pinned context exceeds input budget")]
    PinnedOverBudget,
    /// manifest 超过契约规定的 64 KiB。
    #[error("model request manifest exceeds 64 KiB: {bytes}")]
    ManifestTooLarge {
        /// 实际序列化大小。
        bytes: usize,
    },
    /// ContentStore 暂时不可用；调用方应重试，不能伪装成内容不存在。
    #[error("content store unavailable")]
    ContentBackend,
}

/// ContentRef 解引用结果。正文必须先进入 Surface，之后才能交给 [`assemble`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// 按输入顺序保留的文本。Forbidden 项不占位置，避免暴露其存在。
    pub texts: Vec<String>,
    /// 成功解引用的引用。Forbidden 与 NotFound 均不进入这里。
    pub resolved_refs: Vec<ContentRef>,
    /// 可解释降级。
    pub unresolved: Vec<UnresolvedReason>,
}

/// 装配请求，并生成与实际发送内容一致的 manifest。
pub async fn assemble(
    input: ContextPlanInput,
    counter: Option<&dyn TokenCounter>,
) -> Result<ContextPlan, AssembleError> {
    let estimator = ConservativeEstimator::default();
    let tool_json = serde_json::to_string(&input.tools).expect("ToolDef is serializable");
    let system_tokens = estimator.text(&input.system);
    let tool_tokens = estimator.text(&tool_json);

    let mut fragments = vec![Fragment {
        priority: Priority::SafetyRules,
        label: "system".into(),
        tokens: system_tokens,
        messages: Vec::new(),
    }];
    fragments.push(Fragment {
        priority: Priority::ToolSchema,
        label: "tool-catalog".into(),
        tokens: tool_tokens,
        messages: Vec::new(),
    });
    fragments.extend(input.messages.iter().map(|planned| Fragment {
        priority: planned.priority,
        label: planned.label.clone(),
        tokens: estimator.message(&planned.message),
        messages: vec![planned.message.clone()],
    }));

    let available = input
        .max_input_tokens
        .saturating_sub(input.reserved_output_tokens);
    let trimmed = trim_to_budget(fragments, available);
    if trimmed.over_budget {
        return Err(AssembleError::PinnedOverBudget);
    }

    let kept_labels: HashSet<&str> = trimmed.kept.iter().map(|f| f.label.as_str()).collect();
    let tools = if kept_labels.contains("tool-catalog") {
        input.tools
    } else {
        Vec::new()
    };
    let messages = input
        .messages
        .into_iter()
        .filter(|planned| kept_labels.contains(planned.label.as_str()))
        .map(|planned| planned.message)
        .collect::<Vec<_>>();
    let dropped = trimmed.dropped.into_iter().map(|f| f.label).collect::<Vec<_>>();

    let actual_tool_json = serde_json::to_string(&tools).expect("ToolDef is serializable");
    let tool_catalog_digest = digest(actual_tool_json.as_bytes());
    let system_digest = digest(input.system.as_bytes());
    let layout = CacheLayout {
        segments: vec![
            Segment {
                kind: CacheSegment::S0SystemRules,
                digest: system_digest,
            },
            Segment {
                kind: CacheSegment::S1ToolCatalog,
                digest: tool_catalog_digest.clone(),
            },
            Segment {
                kind: CacheSegment::S2Surface,
                digest: input.surface_digest,
            },
        ],
        surface_invalidation: input.surface_invalidation,
    };
    let cache_prefix_digest = layout.prefix_digest();

    let exact_input = if let Some(counter) = counter {
        let text = format!(
            "{}\n{}\n{}",
            input.system,
            serde_json::to_string(&messages).expect("Message is serializable"),
            actual_tool_json
        );
        counter.count(&input.model, &text).await.ok()
    } else {
        None
    };

    let manifest = ModelRequestManifest {
        request_id: input.request_id,
        operation_view: input.operation_view,
        source_event_range: input.source_event_range,
        system_sections: input.system_sections,
        memory_fragments: input.memory_fragments,
        skill_fragments: input.skill_fragments,
        compaction_refs: input.compaction_refs,
        resolved_content_refs: input.resolved_content_refs,
        tool_catalog_digest,
        cache_prefix_digest,
        cache_breakpoints: layout.breakpoints(),
        legalization_ops: input.legalization_ops,
        unresolved: input.unresolved,
        token_accounting: TokenAccounting {
            estimated_input: trimmed.total_tokens,
            exact_input,
            reserved_output: input.reserved_output_tokens,
            cache_read: None,
            cache_creation: None,
        },
    };
    let bytes = serde_json::to_vec(&manifest)
        .expect("manifest is serializable")
        .len();
    if bytes >= MANIFEST_LIMIT {
        return Err(AssembleError::ManifestTooLarge { bytes });
    }

    Ok(ContextPlan {
        system: input.system,
        messages,
        tools,
        manifest,
        dropped,
    })
}

/// 解引用文本内容。NotFound 产生不含摘要的占位；Forbidden 直接剔除。
pub async fn resolve_text_refs(
    store: &dyn ContentStore,
    refs: &[ContentRef],
) -> Result<Resolution, AssembleError> {
    let mut texts = Vec::new();
    let mut resolved_refs = Vec::new();
    let mut unresolved = Vec::new();
    for reference in refs {
        match store.get(reference).await {
            Ok(bytes) => {
                texts.push(String::from_utf8_lossy(&bytes).into_owned());
                resolved_refs.push(reference.clone());
            }
            Err(ContentError::NotFound { .. }) => {
                texts.push(format!("[内容已过期：原始长度 {} 字节]", reference.len));
                unresolved.push(UnresolvedReason::Expired {
                    original_len: reference.len,
                });
            }
            Err(ContentError::Forbidden) => unresolved.push(UnresolvedReason::Forbidden),
            Err(ContentError::Backend { .. }) => return Err(AssembleError::ContentBackend),
        }
    }
    Ok(Resolution {
        texts,
        resolved_refs,
        unresolved,
    })
}

/// checkpoint 可见前 retain manifest 引用的全部内容。
pub async fn retain_manifest_refs(
    store: &dyn ContentStore,
    owner: RetentionOwner,
    manifest: &ModelRequestManifest,
) -> Result<(), ContentError> {
    let mut refs = manifest.resolved_content_refs.clone();
    let mut seen = HashSet::new();
    refs.retain(|reference| seen.insert(reference.clone()));
    store.retain(owner, &refs).await
}

fn digest(bytes: &[u8]) -> Digest {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    Digest::from_hex(format!("{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::authority::{CapabilityViewDigest, PermissionMode};
    use agentrs_contracts::content::{ContentMeta, ContentScope};
    use agentrs_contracts::ids::{EventSequence, RunId};
    use agentrs_testkit::{ContentFault, FakeContentStore};
    use agentrs_types::{ContentBlock, Role};
    use bytes::Bytes;

    use super::*;

    fn input(messages: Vec<PlannedMessage>) -> ContextPlanInput {
        ContextPlanInput {
            request_id: "q1".into(),
            model: "m".into(),
            operation_view: OperationView {
                authority_id: "a1".into(),
                permission_mode: PermissionMode::Default,
                capability_digest: CapabilityViewDigest(Digest::from_hex("cap")),
                component_generations: Default::default(),
            },
            source_event_range: EventRange {
                start: EventSequence(1),
                end: EventSequence(9),
            },
            system: "安全规则".into(),
            system_sections: vec![],
            messages,
            tools: vec![ToolDef::read_only("Read", "read", serde_json::json!({}))],
            memory_fragments: vec![],
            skill_fragments: vec![],
            compaction_refs: vec![],
            resolved_content_refs: vec![],
            surface_digest: Digest::from_hex("surface"),
            surface_invalidation: None,
            legalization_ops: vec![],
            unresolved: vec![],
            max_input_tokens: 100,
            reserved_output_tokens: 10,
        }
    }

    fn message(label: &str, priority: Priority, text: &str) -> PlannedMessage {
        PlannedMessage {
            label: label.into(),
            priority,
            message: Message::new(Role::User, vec![ContentBlock::text(text)]),
        }
    }

    #[tokio::test]
    async fn 低优先级历史先于当前输入被裁() {
        let mut i = input(vec![
            message("current", Priority::CurrentInput, "now"),
            message("old", Priority::OldHistory, &"x".repeat(200)),
        ]);
        i.max_input_tokens = 80;
        let plan = assemble(i, None).await.unwrap();
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(plan.dropped, vec!["old"]);
        assert!(plan.manifest.token_accounting.estimated_input <= 70);
        assert_eq!(
            plan.manifest.cache_prefix_digest,
            plan.manifest.cache_prefix_digest.clone()
        );
    }

    #[tokio::test]
    async fn 安全段自身超窗时失败而不是裁掉() {
        let mut i = input(vec![]);
        i.system = "x".repeat(200);
        i.max_input_tokens = 20;
        assert!(matches!(
            assemble(i, None).await,
            Err(AssembleError::PinnedOverBudget)
        ));
    }

    #[tokio::test]
    async fn content_ref_缺失与禁止有不同降级语义() {
        let store = FakeContentStore::new();
        let a = store
            .put(
                ContentScope::Global,
                Bytes::from_static(b"alpha"),
                ContentMeta::default(),
            )
            .await
            .unwrap();
        let b = store
            .put(
                ContentScope::Global,
                Bytes::from_static(b"secret"),
                ContentMeta::default(),
            )
            .await
            .unwrap();
        store.inject(ContentFault::NotFound(a.digest.clone()));
        store.inject(ContentFault::Forbidden(b.digest.clone()));

        let out = resolve_text_refs(&store, &[a, b]).await.unwrap();
        assert_eq!(out.texts.len(), 1, "Forbidden 不得产生占位暴露存在性");
        assert!(out.resolved_refs.is_empty());
        assert!(out.texts[0].contains("5 字节"));
        assert_eq!(
            out.unresolved,
            vec![
                UnresolvedReason::Expired { original_len: 5 },
                UnresolvedReason::Forbidden
            ]
        );
    }

    #[tokio::test]
    async fn manifest_引用在_checkpoint_前被_retain() {
        let store = FakeContentStore::new();
        let reference = store
            .put(
                ContentScope::Global,
                Bytes::from_static(b"system"),
                ContentMeta::default(),
            )
            .await
            .unwrap();
        let mut i = input(vec![]);
        i.system_sections = vec![reference.clone()];
        i.resolved_content_refs = vec![reference.clone()];
        let plan = assemble(i, None).await.unwrap();
        retain_manifest_refs(
            &store,
            RetentionOwner::Run {
                run_id: RunId::new("r1"),
            },
            &plan.manifest,
        )
        .await
        .unwrap();

        assert_eq!(store.collect_unretained(), 0);
        assert!(!store.was_collected(&reference.digest));
    }
}
