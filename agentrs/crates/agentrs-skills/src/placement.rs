//! 技能内容的缓存落位（架构 §10）。
//!
//! ## 落位不是排版问题，是钱的问题
//!
//! 技能正文若注入 S0（系统段），**每次启用/停用都会造成全量 cache miss**。
//! 一个会被频繁开关的技能放错位置，等于每轮对话都重新付一次全量输入费。
//!
//! | 技能贡献 | 落位 | 理由 |
//! |---|---|---|
//! | 长期生效、Run 内不变的指令 | S0 尾部，**启动时一次性确定** | 进稳定前缀，可被缓存 |
//! | 按需启用的技能正文与参考资料 | S3 | 每轮可变段，不污染前缀 |
//! | 技能带来的工具集缩减 | S1 目录投影 | 与 `PermissionMode` 同一机制，切换即断点 |
//!
//! > **文档矛盾一处**：§10 的表格把第二行写成 S4，而
//! > `CacheSegment::S3Selected` 的契约注释里明写了"skill fragment"。
//! > 两者都是可变段，缓存后果完全相同；这里按更具体的那一处（契约注释）走。
//! > 已记入架构文档 §18.11。
//!
//! ## 中途改 S0 必须推迟到压缩边界
//!
//! 若一个技能确实需要在 Run 中途修改 S0，该修改**必须推迟到 compaction 边界**，
//! 与压缩一次性重建前缀合并。不允许因技能启停单独触发全量 miss。
//!
//! 这条规则把两件事绑在了一起：压缩本来就要作废前缀（`Replace` 使
//! `range.start` 起失效），既然这一刀免不了，就让所有需要作废前缀的改动
//! **搭同一班车**。[`defer_to_boundary`] 是这条规则的执行者。

use agentrs_contracts::manifest::CacheSegment;

/// 技能的一项贡献。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contribution {
    /// 长期生效、Run 内不变的指令。
    StableInstruction {
        /// 正文。
        text: String,
    },
    /// 按需启用的技能正文与参考资料。
    OnDemandBody {
        /// 正文。
        text: String,
    },
    /// 工具集缩减。
    ToolNarrowing {
        /// 保留的工具名。
        keep: Vec<String>,
    },
}

impl Contribution {
    /// 该贡献应当落在哪一段。
    pub fn segment(&self) -> CacheSegment {
        match self {
            Self::StableInstruction { .. } => CacheSegment::S0SystemRules,
            // **每轮可变段**：开关技能不该动前缀。
            // S3 而非 S4——见模块文档里那条矛盾说明。
            Self::OnDemandBody { .. } => CacheSegment::S3Selected,
            Self::ToolNarrowing { .. } => CacheSegment::S1ToolCatalog,
        }
    }

    /// 该贡献是否会使缓存前缀失效。
    pub fn invalidates_prefix(&self) -> bool {
        self.segment().is_stable_prefix()
    }
}

/// Run 的生命周期位置，决定一项贡献能否立即生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moment {
    /// Run 启动，前缀尚未建立。此时改 S0 不花任何代价。
    Startup,
    /// Run 中途的普通 Step 边界。
    MidRun,
    /// 压缩边界——前缀**本来就要作废**。
    CompactionBoundary,
}

/// 一项贡献的施加时机裁定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// 立即生效。
    Immediate {
        /// 落在哪一段。
        segment: CacheSegment,
    },
    /// **推迟到下一个压缩边界**。
    ///
    /// 不是拒绝——是等一班顺路的车。
    Deferred {
        /// 落在哪一段。
        segment: CacheSegment,
        /// 为什么推迟。
        reason: &'static str,
    },
}

impl Placement {
    /// 是否被推迟。
    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred { .. })
    }
}

/// 裁定一项贡献该何时施加。
///
/// 规则只有一条：**会作废前缀的改动，只在"前缀本来就要重建"的时刻施加**。
pub fn defer_to_boundary(c: &Contribution, at: Moment) -> Placement {
    let segment = c.segment();

    if !c.invalidates_prefix() {
        // 可变段每轮都变，随时可加。
        return Placement::Immediate { segment };
    }

    match at {
        // 启动时前缀还没建立，改它不花代价。
        Moment::Startup => Placement::Immediate { segment },
        // 压缩本来就要作废前缀——搭同一班车。
        Moment::CompactionBoundary => Placement::Immediate { segment },
        // 中途单独改会造成一次全量 miss，不划算。
        Moment::MidRun => Placement::Deferred {
            segment,
            reason: "会作废缓存前缀；等下一个压缩边界一并生效",
        },
    }
}

/// 一个技能的引用，进 manifest 供 trajectory 解释。
///
/// **必须带 version 与 content hash**：没有它们，"这一轮为什么多了这段指令"
/// 就只能回答到"启用了技能 X"，而技能 X 的内容是会变的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRef {
    /// 技能标识。
    pub id: agentrs_contracts::ids::SkillId,
    /// 版本。
    pub version: String,
    /// 正文的内容摘要。
    pub content_digest: agentrs_contracts::ids::Digest,
    /// 落位。
    pub segment: CacheSegment,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 稳定() -> Contribution {
        Contribution::StableInstruction {
            text: "始终用中文回复".into(),
        }
    }
    fn 按需() -> Contribution {
        Contribution::OnDemandBody {
            text: "这是一份 SQL 优化手册……".into(),
        }
    }
    fn 缩减() -> Contribution {
        Contribution::ToolNarrowing {
            keep: vec!["Read".into()],
        }
    }

    #[test]
    fn 三类贡献各归各段() {
        assert_eq!(稳定().segment(), CacheSegment::S0SystemRules);
        assert_eq!(按需().segment(), CacheSegment::S3Selected);
        assert_eq!(缩减().segment(), CacheSegment::S1ToolCatalog);
    }

    #[test]
    fn 按需正文不进稳定前缀() {
        // **落错这一项的代价最直接**：一个会被频繁开关的技能放进 S0，
        // 等于每轮对话都重新付一次全量输入费。
        assert!(!按需().invalidates_prefix());
    }

    #[test]
    fn 稳定指令与工具缩减都会作废前缀() {
        assert!(稳定().invalidates_prefix());
        assert!(缩减().invalidates_prefix());
    }

    #[test]
    fn 按需正文任何时刻都可立即生效() {
        for at in [Moment::Startup, Moment::MidRun, Moment::CompactionBoundary] {
            assert!(
                !defer_to_boundary(&按需(), at).is_deferred(),
                "可变段内容不该被推迟：{at:?}"
            );
        }
    }

    #[test]
    fn 启动时改_s0_不花代价() {
        // 前缀还没建立。
        assert!(!defer_to_boundary(&稳定(), Moment::Startup).is_deferred());
    }

    #[test]
    fn 中途改_s0_被推迟() {
        // 不是拒绝——是等一班顺路的车。
        let p = defer_to_boundary(&稳定(), Moment::MidRun);
        assert!(p.is_deferred());
        match p {
            Placement::Deferred { reason, segment } => {
                assert_eq!(segment, CacheSegment::S0SystemRules);
                assert!(reason.contains("压缩边界"), "{reason}");
            }
            other => panic!("期望 Deferred，得到 {other:?}"),
        }
    }

    #[test]
    fn 压缩边界上_s0_改动可以搭车() {
        // 压缩本来就要作废前缀（`Replace` 使 range.start 起失效），
        // 既然这一刀免不了，就让所有要作废前缀的改动搭同一班车。
        assert!(!defer_to_boundary(&稳定(), Moment::CompactionBoundary).is_deferred());
        assert!(!defer_to_boundary(&缩减(), Moment::CompactionBoundary).is_deferred());
    }

    #[test]
    fn 中途的工具缩减同样被推迟() {
        // 与 PermissionMode 同一机制：切换即断点。
        assert!(defer_to_boundary(&缩减(), Moment::MidRun).is_deferred());
    }

    #[test]
    fn 会作废前缀的改动只在两个时刻立即生效() {
        // 把规则本身写成一条断言：**恰好** Startup 与 CompactionBoundary。
        for c in [稳定(), 缩减()] {
            let 立即: Vec<Moment> = [Moment::Startup, Moment::MidRun, Moment::CompactionBoundary]
                .into_iter()
                .filter(|m| !defer_to_boundary(&c, *m).is_deferred())
                .collect();
            assert_eq!(
                立即,
                [Moment::Startup, Moment::CompactionBoundary],
                "{c:?} 的立即生效时刻不对"
            );
        }
    }
}
