//! Token 账本与预算裁剪（架构 §9.1，任务 T11 起步）。
//!
//! ## 为什么有两套计数
//!
//! 架构 §1.1 裁定 1 把 token 计数拆成两条：
//!
//! | | 谁提供 | 用途 | 失败时 |
//! |---|---|---|---|
//! | **保守估算器** | 内核内置（本模块） | **硬上限保护** | 不会失败 |
//! | 精确计数 | Core 的 `TokenCounter` port | 账本与成本展示 | 降级到估算器 |
//!
//! 只做前者数字不准，只做后者内核就无法**独立**守住窗口——端口不可用时
//! 没有任何东西阻止超窗。因此二者并存。
//!
//! 估算器的唯一硬要求：**只允许高估，不允许低估**。低估会让请求超窗后被
//! provider 拒绝，而那时上下文已经装配完毕，只能整轮重来。

use agentrs_types::{ContentBlock, Message};

/// 保守 token 估算器。
///
/// 采用字节数启发式而非真实分词——真实分词需要 tokenizer 数据文件，那会让内核
/// 碰磁盘（违反边界判据）。启发式的代价是高估，而高估是安全的那一侧。
#[derive(Debug, Clone, Copy)]
pub struct ConservativeEstimator {
    /// 每 token 假定的最少字节数。取 2 覆盖 CJK（UTF-8 三字节/字，
    /// 常见分词约 1–1.5 字/token），拉丁文本会被显著高估——这是有意的。
    bytes_per_token: usize,
    /// 每条消息的固定开销（角色标记、分隔符等）。
    per_message_overhead: u64,
}

impl Default for ConservativeEstimator {
    fn default() -> Self {
        Self {
            bytes_per_token: 2,
            per_message_overhead: 8,
        }
    }
}

impl ConservativeEstimator {
    /// 估算一段文本。
    pub fn text(&self, s: &str) -> u64 {
        // 向上取整：宁可多算一个 token。
        s.len().div_ceil(self.bytes_per_token) as u64
    }

    /// 估算一个内容块。
    pub fn block(&self, b: &ContentBlock) -> u64 {
        match b {
            ContentBlock::Text { text } => self.text(text),
            ContentBlock::Thinking { thinking, .. } => self.text(thinking),
            ContentBlock::ToolResult { content, .. } => self.text(content),
            ContentBlock::ToolUse { name, input, .. } => self.text(name) + self.text(&input.to_string()),
            // 图像按目标模型的能力另行折算并占独立配额（架构 §9.1）。
            // 这里给一个保守的固定值，避免图像被当成零成本。
            ContentBlock::Image { .. } => 1_600,
            ContentBlock::ProviderItem { item, .. } => self.text(&item.to_string()),
        }
    }

    /// 估算一条消息。
    pub fn message(&self, m: &Message) -> u64 {
        self.per_message_overhead + m.content.iter().map(|b| self.block(b)).sum::<u64>()
    }

    /// 估算一组消息。
    pub fn messages(&self, ms: &[Message]) -> u64 {
        ms.iter().map(|m| self.message(m)).sum()
    }
}

/// 上下文段的优先级（架构 §9.1 的阶梯）。数值越小越先保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// 安全与能力约束。**永不裁剪。**
    SafetyRules,
    /// 用户当前输入。
    CurrentInput,
    /// 未决审批与当前任务。
    PendingWork,
    /// 最近对话。
    RecentHistory,
    /// 关键文件与产物引用。
    WorkingSet,
    /// 精选记忆。
    Memory,
    /// 技能正文。
    SkillBody,
    /// 工具 schema。
    ToolSchema,
    /// 低优先级历史。
    OldHistory,
}

impl Priority {
    /// 是否**不可裁剪**。
    ///
    /// 安全规则被裁掉就等于悄悄放宽了约束，比超窗危险得多——
    /// 因此宁可让请求失败也不裁它。
    pub fn is_pinned(&self) -> bool {
        matches!(self, Priority::SafetyRules)
    }
}

/// 一个可计量、可裁剪的上下文片段。
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    /// 优先级。
    pub priority: Priority,
    /// 诊断标签。
    pub label: String,
    /// 估算成本。
    pub tokens: u64,
    /// 承载的消息（可为空，例如纯 schema 段）。
    pub messages: Vec<Message>,
}

/// 裁剪结果。
#[derive(Debug, Clone, PartialEq)]
pub struct TrimResult {
    /// 保留的片段，**保持输入顺序**。
    pub kept: Vec<Fragment>,
    /// 被裁掉的片段，供 manifest 留痕。
    pub dropped: Vec<Fragment>,
    /// 保留部分的估算总量。
    pub total_tokens: u64,
    /// 是否即便裁到只剩 pinned 也仍然超预算。
    pub over_budget: bool,
}

/// 按优先级阶梯裁剪到预算内。
///
/// 规则：
///
/// 1. `SafetyRules` **永不裁剪**——宁可 `over_budget` 也不放宽约束；
/// 2. 从**最低优先级**开始裁；
/// 3. 同优先级内从**靠后**的开始裁（更旧的先走）；
/// 4. 保留部分**维持原始顺序**——装配顺序即缓存前缀顺序，不能被裁剪打乱。
pub fn trim_to_budget(fragments: Vec<Fragment>, budget: u64) -> TrimResult {
    let total: u64 = fragments.iter().map(|f| f.tokens).sum();
    if total <= budget {
        return TrimResult {
            total_tokens: total,
            kept: fragments,
            dropped: Vec::new(),
            over_budget: false,
        };
    }

    // 候选裁剪顺序：优先级从低到高，同级内从后到前。
    let mut order: Vec<usize> = (0..fragments.len())
        .filter(|i| !fragments[*i].priority.is_pinned())
        .collect();
    order.sort_by(|a, b| fragments[*b].priority.cmp(&fragments[*a].priority).then(b.cmp(a)));

    let mut drop_flags = vec![false; fragments.len()];
    let mut current = total;
    for i in order {
        if current <= budget {
            break;
        }
        drop_flags[i] = true;
        current -= fragments[i].tokens;
    }

    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for (i, f) in fragments.into_iter().enumerate() {
        if drop_flags[i] {
            dropped.push(f);
        } else {
            kept.push(f);
        }
    }

    TrimResult {
        total_tokens: current,
        over_budget: current > budget,
        kept,
        dropped,
    }
}

#[cfg(test)]
mod tests {
    use agentrs_types::Role;

    use super::*;

    fn 片段(p: Priority, label: &str, tokens: u64) -> Fragment {
        Fragment {
            priority: p,
            label: label.into(),
            tokens,
            messages: vec![],
        }
    }

    fn 文本消息(t: &str) -> Message {
        Message::new(Role::User, vec![ContentBlock::text(t)])
    }

    #[test]
    fn 估算器只高估不低估_对_ascii() {
        let e = ConservativeEstimator::default();
        // "hello world" = 11 字节 → 6 token。真实分词约 2 个。
        assert!(e.text("hello world") >= 2);
    }

    #[test]
    fn 估算器对中文同样不低估() {
        let e = ConservativeEstimator::default();
        // 中文 UTF-8 三字节/字；10 个字 = 30 字节 → 15 token。
        // 真实分词约 7–10 个 token，仍是高估。
        let s = "这是一段中文文本内容";
        assert!(
            e.text(s) >= s.chars().count() as u64,
            "对 CJK 至少按每字一 token 估，不得更低"
        );
    }

    #[test]
    fn 图像不被当作零成本() {
        let e = ConservativeEstimator::default();
        let img = ContentBlock::Image {
            image_url: agentrs_types::ImageUrl {
                url: "data:image/png;base64,AA".into(),
            },
        };
        assert!(e.block(&img) > 100, "图像必须占用可观配额");
    }

    #[test]
    fn 消息估算包含固定开销() {
        let e = ConservativeEstimator::default();
        let empty = Message::new(Role::User, vec![]);
        assert!(e.message(&empty) > 0, "角色标记与分隔符也有成本");
        assert!(e.message(&文本消息("abc")) > e.message(&empty));
    }

    #[test]
    fn 预算充足时不裁剪() {
        let f = vec![
            片段(Priority::SafetyRules, "rules", 10),
            片段(Priority::RecentHistory, "recent", 20),
        ];
        let r = trim_to_budget(f.clone(), 100);
        assert_eq!(r.kept, f);
        assert!(r.dropped.is_empty());
        assert!(!r.over_budget);
    }

    #[test]
    fn 从最低优先级开始裁() {
        let f = vec![
            片段(Priority::SafetyRules, "rules", 10),
            片段(Priority::CurrentInput, "input", 10),
            片段(Priority::SkillBody, "skill", 30),
            片段(Priority::OldHistory, "old", 40),
        ];
        let r = trim_to_budget(f, 50);
        let kept: Vec<_> = r.kept.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(kept, ["rules", "input", "skill"]);
        assert_eq!(r.dropped[0].label, "old", "最低优先级先走");
        assert!(!r.over_budget);
    }

    #[test]
    fn 安全规则永不被裁即使因此超预算() {
        // 裁掉安全规则等于悄悄放宽约束，比超窗危险得多。
        let f = vec![
            片段(Priority::SafetyRules, "rules", 100),
            片段(Priority::OldHistory, "old", 10),
        ];
        let r = trim_to_budget(f, 50);
        assert_eq!(r.kept.len(), 1);
        assert_eq!(r.kept[0].label, "rules");
        assert!(r.over_budget, "必须如实报告超预算，由上层决定压缩或失败");
    }

    #[test]
    fn 保留部分维持原始顺序() {
        // 装配顺序即缓存前缀顺序——裁剪不得打乱它，否则前缀缓存全废。
        let f = vec![
            片段(Priority::SafetyRules, "a", 5),
            片段(Priority::OldHistory, "b", 50),
            片段(Priority::CurrentInput, "c", 5),
            片段(Priority::SkillBody, "d", 5),
        ];
        let r = trim_to_budget(f, 20);
        let kept: Vec<_> = r.kept.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(kept, ["a", "c", "d"], "顺序不得因裁剪而改变");
    }

    #[test]
    fn 同优先级内靠后的先裁() {
        let f = vec![
            片段(Priority::OldHistory, "old-1", 30),
            片段(Priority::OldHistory, "old-2", 30),
            片段(Priority::OldHistory, "old-3", 30),
        ];
        let r = trim_to_budget(f, 70);
        let kept: Vec<_> = r.kept.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(kept, ["old-1", "old-2"], "更旧的（靠后的）先走");
    }

    #[test]
    fn 被裁片段可留痕供_manifest_解释() {
        let f = vec![
            片段(Priority::CurrentInput, "keep", 10),
            片段(Priority::Memory, "mem", 50),
        ];
        let r = trim_to_budget(f, 20);
        assert_eq!(r.dropped.len(), 1);
        assert_eq!(r.dropped[0].label, "mem");
        // manifest 记录它，"这次为什么少了这段记忆"就可解释。
    }

    #[test]
    fn 优先级阶梯顺序与架构一致() {
        assert!(Priority::SafetyRules < Priority::CurrentInput);
        assert!(Priority::CurrentInput < Priority::RecentHistory);
        assert!(Priority::Memory < Priority::SkillBody);
        assert!(Priority::ToolSchema < Priority::OldHistory);
        assert!(Priority::SafetyRules.is_pinned());
        assert!(!Priority::CurrentInput.is_pinned());
    }
}
