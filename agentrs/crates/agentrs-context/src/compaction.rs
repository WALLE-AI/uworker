//! 压缩策略（架构 §9.3）。
//!
//! ## 四段，不是一次截断
//!
//! | 段 | 做什么 | 落在哪 |
//! |---|---|---|
//! | 1. 工具输出整理 | 去噪、大输出 ref 化 | 结果**首次**入上下文时，属 S4，**不破坏前缀** |
//! | 2. Microcompact | 剪掉早期已消费的工具全文，保留引用/错误/最近结果 | 一个 `Replace` 节点 |
//! | 3. Compact | 接近阈值时生成结构化摘要 | 一个 `Replace` 节点 |
//! | 4. ContextSummary | 恢复、跨 Run 延续、重大切换时的完整工作摘要 | 一个 `Replace` 节点 |
//!
//! 第 1 段与后三段的区别值得强调：它发生在内容**进来的时候**，所以不动已有前缀；
//! 后三段是事后遮蔽，必然使前缀从 `range.start` 起失效。
//!
//! ## 本模块只做计划，不做摘要
//!
//! 摘要要调模型（零工具子 Agent），那是 live 资源与 I/O。这里是**纯函数**：
//! 输入历史与预算，输出"该遮蔽哪一段、为什么"。摘要文本由调用方填进来。
//!
//! 这样切的好处是 §9.3 里真正难的那部分——**该压哪一段、压了到底有没有用**——
//! 可以在没有模型的情况下被穷尽测试。
//!
//! ## 防重试循环
//!
//! 溢出触发之后，只有当压缩**确实推进了** `SurfaceGeneration` 才允许重试。
//! 没有这条判据，"压缩没压下去 → 再请求 → 再溢出"会无限循环。
//! [`CompactionPlan::makes_progress`] 是这个判据。

use agentrs_contracts::ids::EventSequence;
use agentrs_contracts::surface::SurfaceRange;

use crate::budget::Priority;

/// 压缩的触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// **压力触发**：Step 开始、请求装配之前，预算接近阈值。属正常路径。
    Pressure,
    /// **溢出触发**：provider 明确返回"上下文过长"之后。
    ///
    /// 只对**规范的上下文溢出错误**生效——不是通用重试。把连接超时、
    /// 限流之类也塞进来，会让压缩变成掩盖问题的地毯。
    Overflow,
}

/// 压缩的层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// 剪掉早期已消费的工具全文。**先试它**——代价最小、信息损失最可控。
    Microcompact,
    /// 生成结构化摘要遮蔽一段历史。
    Compact,
    /// 完整工作摘要。恢复、跨 Run 延续、重大切换时使用。
    ContextSummary,
}

/// 一条可压缩的历史条目。
///
/// 调用方从 Surface 投影出来，附上优先级与估算成本。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// 在 Surface 上的位置。
    pub seq: EventSequence,
    /// 优先级，决定能不能动。
    pub priority: Priority,
    /// 估算 token 成本。
    pub tokens: u64,
    /// 是否为工具全文（microcompact 的目标）。
    pub is_tool_output: bool,
    /// 是否已被消费——**未消费的工具结果不能剪**，模型还没看过它。
    pub consumed: bool,
    /// 是否承载错误。**错误一律保留**：它是后续修复的依据，
    /// 剪掉会让模型反复犯同一个错。
    pub is_error: bool,
}

/// 一次压缩计划。
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPlan {
    /// 触发来源。
    pub trigger: Trigger,
    /// 采用的层级。
    pub tier: Tier,
    /// 待遮蔽的 Surface 区间。
    pub range: SurfaceRange,
    /// 预计回收的 token。
    pub reclaimed_tokens: u64,
    /// 遮蔽后的预计总量。
    pub projected_tokens: u64,
}

impl CompactionPlan {
    /// 这次压缩**是否确实推进了状态**。
    ///
    /// 溢出触发后的重试判据（架构 §9.3）。回收为 0 的"压缩"不算数——
    /// 允许它推进代际就等于允许无限重试。
    pub fn makes_progress(&self) -> bool {
        self.reclaimed_tokens > 0 && self.range.start < self.range.end
    }
}

/// 为什么没有产出计划。
///
/// **不是错误**——多数时候"不需要压"才是正常的。但它必须可解释，
/// 否则"为什么这次没压缩"只能靠猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoPlan {
    /// 尚未触及阈值。
    UnderThreshold,
    /// 没有任何可动的条目——全被钉住了。
    ///
    /// 这不是好消息：说明**光靠压缩已经救不了这个 Run**，
    /// 调用方应当停下问人而不是继续重试。
    NothingCompactible,
    /// 有可动条目，但回收量为 0。
    ///
    /// 与上一条一样是终止信号：再压一次也不会更好。
    NoProgress,
    /// 请求的区间已经被压过。**不重复压同一段**——那会造成信息漂移。
    AlreadyCompacted {
        /// 已压过的区间起点。
        start: EventSequence,
    },
}

/// 压缩策略参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// 触及此比例即压力触发。
    pub threshold_pct: u8,
    /// 输入预算上限。
    pub max_input_tokens: u64,
    /// **保留最近这么多条**，无论优先级。
    ///
    /// 没有它，压缩可能把刚发生的事也摘要掉，模型立刻失去当前任务的上下文。
    pub keep_recent: usize,
}

impl Policy {
    /// 触发阈值对应的绝对 token 数。
    pub fn threshold_tokens(&self) -> u64 {
        self.max_input_tokens * self.threshold_pct as u64 / 100
    }
}

/// 一个条目是否可以被 microcompact 剪掉。
fn 可剪(e: &Entry) -> bool {
    // 三个条件缺一不可：是工具全文、已被模型消费过、不承载错误。
    e.is_tool_output && e.consumed && !e.is_error && !e.priority.is_pinned()
}

/// 一个条目是否可以进摘要。
fn 可摘要(e: &Entry) -> bool {
    // 钉住的段永不参与；错误同样保留——摘要会丢掉具体的报错文本。
    !e.priority.is_pinned() && !e.is_error
}

/// 规划一次压缩。
///
/// `already_compacted` 是历史上全部已遮蔽区间的起点，用于避免重复压缩同一段。
pub fn plan(
    entries: &[Entry],
    policy: &Policy,
    trigger: Trigger,
    already_compacted: &[EventSequence],
) -> Result<CompactionPlan, NoPlan> {
    let total: u64 = entries.iter().map(|e| e.tokens).sum();

    // 压力触发要看阈值；溢出触发是 provider 已经明确说了太长——**不再自问自答**。
    if trigger == Trigger::Pressure && total < policy.threshold_tokens() {
        return Err(NoPlan::UnderThreshold);
    }

    // 尾部永远保留。
    let 可动上界 = entries.len().saturating_sub(policy.keep_recent);
    if 可动上界 == 0 {
        return Err(NoPlan::NothingCompactible);
    }
    let 候选 = &entries[..可动上界];

    // 先试 Microcompact：代价最小、信息损失最可控。
    if let Some(p) = 规划一层(候选, trigger, already_compacted, Tier::Microcompact, 可剪, total) {
        return Ok(p);
    }
    // 剪不动就摘要。
    if let Some(p) = 规划一层(候选, trigger, already_compacted, Tier::Compact, 可摘要, total) {
        return Ok(p);
    }

    // 走到这里说明**光靠压缩救不了**。
    if 候选.iter().any(可摘要) {
        Err(NoPlan::NoProgress)
    } else {
        Err(NoPlan::NothingCompactible)
    }
}

fn 规划一层(
    候选: &[Entry],
    trigger: Trigger,
    already: &[EventSequence],
    tier: Tier,
    可动: fn(&Entry) -> bool,
    total: u64,
) -> Option<CompactionPlan> {
    // 取**最长的一段连续可动条目**。
    //
    // 为什么要连续：`Replace` 遮蔽的是一个区间。挑一堆不连续的条目需要多个
    // Replace 节点，而每个都是一次缓存失效点——压一次却断好几处前缀，
    // 得不偿失。
    let mut best: Option<(usize, usize, u64)> = None;
    let mut i = 0;
    while i < 候选.len() {
        if !可动(&候选[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut sum = 0;
        while i < 候选.len() && 可动(&候选[i]) {
            sum += 候选[i].tokens;
            i += 1;
        }
        if best.is_none_or(|(_, _, b)| sum > b) {
            best = Some((start, i, sum));
        }
    }

    let (s, e, reclaimed) = best?;
    if reclaimed == 0 {
        return None;
    }

    let range = SurfaceRange {
        start: 候选[s].seq,
        end: EventSequence(候选[e - 1].seq.0 + 1),
    };
    // **不重复压同一段**：反复摘要同一段历史会造成信息漂移。
    if already.contains(&range.start) {
        return None;
    }

    Some(CompactionPlan {
        trigger,
        tier,
        range,
        reclaimed_tokens: reclaimed,
        // 摘要本身也占 token，但它由调用方生成后才知道大小；
        // 这里给出的是遮蔽后的**下界**，调用方加上摘要成本即可。
        projected_tokens: total.saturating_sub(reclaimed),
    })
}

impl Default for CompactionPlan {
    fn default() -> Self {
        Self {
            trigger: Trigger::Pressure,
            tier: Tier::Microcompact,
            range: SurfaceRange {
                start: EventSequence(0),
                end: EventSequence(0),
            },
            reclaimed_tokens: 0,
            projected_tokens: 0,
        }
    }
}

/// 摘要必须覆盖的要点（架构 §9.3 末段）。
///
/// **不是建议清单，是必填项。** 少一项就意味着恢复后模型会丢掉一类信息，
/// 而那类信息往往正是它接着要用的。
pub const SUMMARY_REQUIRED_SECTIONS: &[&str] = &[
    "原始意图",
    "约束",
    "授权范围",
    "关键文件与产物",
    "已做任务",
    "待做任务",
    "错误与修复",
    "未决审批",
    "当前 ChangeSet",
    "下一步",
];

/// 校验一段摘要是否覆盖了全部必填要点。
///
/// 返回缺失的要点。摘要由模型生成，**模型会漏**——所以要检查，
/// 而不是相信提示词里写了就一定照做。
pub fn missing_sections(summary: &str) -> Vec<&'static str> {
    SUMMARY_REQUIRED_SECTIONS
        .iter()
        .filter(|s| !summary.contains(*s))
        .copied()
        .collect()
}

#[cfg(test)]
mod tests;
