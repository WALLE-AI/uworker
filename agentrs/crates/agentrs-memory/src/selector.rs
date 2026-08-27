//! `memorySelector`：在候选里挑，不在候选外找（架构 §9.2）。
//!
//! ```text
//! MemoryRetriever.candidates  →  memorySelector  →  最多 N 条  →  MemoryRetriever.load
//!        （Core 的索引与权限）      （内核的选择）
//! ```
//!
//! ## 切点在哪
//!
//! AgentRS **不管 SQLite FTS、不管 Markdown 文件**。索引、权限、保留策略全归 Core；
//! 内核只做一件事：从 Core 交来的候选里挑几条。
//!
//! 这个切法的好处是可见范围由 Core 单方面决定——selector 再怎么写错，
//! 也变不出一条 Core 没给它的记忆。
//!
//! ## 两条约束
//!
//! 1. **只能降低注入数量，不能扩大可见范围。** 选出来的必须是候选的子集。
//! 2. **不确定时少选。** 记忆注错的代价（模型被一段不相关的旧事误导）
//!    比漏注（模型多问一句）高得多。
//!
//! ## 失败时软降级，而不是 fail-closed
//!
//! 这一点与 `compact` **刻意相反**：
//!
//! | | 失败后果 | 策略 |
//! |---|---|---|
//! | `compact` | 摘要没了还照样遮蔽 → **历史被换成空白** | fail-closed，不压 |
//! | `memorySelector` | 没挑出来 → 少几条记忆 | 软降级，用 Core 的确定性排名 |
//!
//! 判据是"失败会不会破坏已有的东西"。压缩会，记忆不会——
//! 它只是没帮上忙。为一个可选的增强而让 Run 停下来是不划算的。

use agentrs_contracts::ids::MemoryId;
use agentrs_contracts::ports::MemoryCandidate;

/// 默认注入上限。
pub const DEFAULT_LIMIT: usize = 5;

/// 一次选择的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// 选中的片段，按注入顺序。
    pub ids: Vec<MemoryId>,
    /// 这次选择是怎么来的。
    pub source: SelectionSource,
}

/// 选择的来源，进 manifest 供 trajectory 解释。
///
/// **"这一轮为什么注入了这几条记忆"必须能回答到具体机制**，
/// 而不是笼统的"selector 选的"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionSource {
    /// 由 selector 子 Agent 挑选。
    Selector,
    /// selector 不可用或产出非法，退回 Core 的确定性排名。
    ///
    /// 带上原因——**降级必须留痕**，否则"记忆怎么突然变差了"查不出来。
    RankFallback {
        /// 降级原因，稳定码。
        reason: &'static str,
    },
    /// 没有候选。
    Empty,
}

/// selector 的产出被拒绝的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// 选了候选之外的 id。**这是最要紧的一条**——它意味着 selector 在编造。
    OutOfCandidates {
        /// 越界的 id。
        unknown: Vec<String>,
    },
    /// 选了重复的 id。
    Duplicated,
    /// 超过上限。
    OverLimit {
        /// 实际条数。
        got: usize,
        /// 上限。
        limit: usize,
    },
}

impl Rejected {
    /// 稳定原因码，用于降级留痕。
    pub fn code(&self) -> &'static str {
        match self {
            Self::OutOfCandidates { .. } => "selector_out_of_candidates",
            Self::Duplicated => "selector_duplicated",
            Self::OverLimit { .. } => "selector_over_limit",
        }
    }
}

/// 按 Core 的确定性排名取前 `limit` 条。
///
/// **降级路径**：selector 不可用、产出非法，或干脆没启用时走这里。
/// 它是确定的——同一批候选永远得到同一个结果，因此 replay 可重现。
pub fn by_rank(candidates: &[MemoryCandidate], limit: usize) -> Vec<MemoryId> {
    let mut v: Vec<&MemoryCandidate> = candidates.iter().collect();
    // rank 小者优先；rank 相同时按 id 定序，**不能靠输入顺序**——
    // 检索实现返回的顺序未必稳定，那会让 replay 结果漂移。
    v.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.id.as_str().cmp(b.id.as_str())));
    v.into_iter().take(limit).map(|c| c.id.clone()).collect()
}

/// 校验 selector 的产出。
///
/// **只做校验，不做修补**：越界就整体退回排名，而不是"把越界的挑出来扔掉"。
/// 理由是 selector 一旦编造了 id，它对其余几条的判断也不再可信。
pub fn validate(picked: &[MemoryId], candidates: &[MemoryCandidate], limit: usize) -> Result<(), Rejected> {
    if picked.len() > limit {
        return Err(Rejected::OverLimit {
            got: picked.len(),
            limit,
        });
    }

    let 候选集: std::collections::BTreeSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
    let unknown: Vec<String> = picked
        .iter()
        .filter(|id| !候选集.contains(id.as_str()))
        .map(|id| id.to_string())
        .collect();
    if !unknown.is_empty() {
        return Err(Rejected::OutOfCandidates { unknown });
    }

    let mut seen = std::collections::BTreeSet::new();
    if picked.iter().any(|id| !seen.insert(id.as_str())) {
        return Err(Rejected::Duplicated);
    }

    Ok(())
}

/// 定下最终选择。
///
/// `picked` 为 `None` 表示 selector 未运行或调用失败。
pub fn decide(picked: Option<Vec<MemoryId>>, candidates: &[MemoryCandidate], limit: usize) -> Selection {
    if candidates.is_empty() {
        return Selection {
            ids: Vec::new(),
            source: SelectionSource::Empty,
        };
    }

    let Some(picked) = picked else {
        return Selection {
            ids: by_rank(candidates, limit),
            source: SelectionSource::RankFallback {
                reason: "selector_unavailable",
            },
        };
    };

    match validate(&picked, candidates, limit) {
        Ok(()) => Selection {
            ids: picked,
            source: SelectionSource::Selector,
        },
        Err(e) => Selection {
            ids: by_rank(candidates, limit),
            source: SelectionSource::RankFallback { reason: e.code() },
        },
    }
}

/// 解析 selector 的结构化输出。
///
/// 期望形如 `{"selected": ["mem-1", "mem-3"]}`。**解析失败返回 `None`**
/// 而不是空选择——两者的含义不同：`None` 会走降级，空选择是"我看了但一条都不要"。
pub fn parse_output(text: &str) -> Option<Vec<MemoryId>> {
    // 模型常在 JSON 前后加解释文字，取第一个平衡的 `{...}`。
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&text[start..=end]).ok()?;
    let arr = v.get("selected")?.as_array()?;
    arr.iter()
        .map(|x| x.as_str().map(MemoryId::new))
        .collect::<Option<Vec<_>>>()
}

/// selector 的系统提示。
///
/// 正文来自 [`agentrs_prompts::registry::MEMORY_SELECTOR`]。
/// **lite、零工具、结构化输出**——提示里明说"不确定时少选"，
/// 但落地靠 [`validate`]，不靠模型自觉。
pub fn system_prompt(limit: usize) -> String {
    agentrs_prompts::registry::MEMORY_SELECTOR
        .render(&[("limit", limit.to_string())].into_iter().collect())
        .expect("MEMORY_SELECTOR 的输入声明与模板必须一致（registry 测试保证）")
}

/// 本次使用的提示词引用，进 manifest。
pub fn prompt_ref() -> agentrs_prompts::PromptRef {
    agentrs_prompts::registry::MEMORY_SELECTOR.as_ref()
}

#[cfg(test)]
mod tests;
