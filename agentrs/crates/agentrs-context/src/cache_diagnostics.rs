// Ported from aionrs (Apache-2.0).
//   Source: aionrs/crates/aion-agent/src/cache_diagnostics.rs @ f7111746015d8e6f960e1568a805ceef975022d3
//   Copied: 2026-08-25   Modified: yes
//   Changes:
//     - 归因不再由本模块用 DefaultHasher 自行算 system/tools 哈希：请求侧归因已由
//       cache.rs 的分段与 attribute() 承担（架构 §9.1.1）。本模块只做**响应侧**判定，
//       两侧在 diagnose() 里配对。原版把两件事揉在一起，导致新增一个稳定段就要改哈希函数。
//     - 新增 Unsupported 判定：原版把"端点根本不报缓存字段"和"命中率 0"都算作
//       Healthy{hit_rate: 0.0}。这两者必须区分——本项目实测遇到过 vLLM 侧
//       prefix caching 未生效却无任何报错的情况，混为一谈就永远查不出来。
//     - 移除 CacheBreakCause 的本地定义，改用 contracts::CacheBreakCause（8 个变体），
//       避免读模型与契约两处各有一份原因枚举。
//     - PartialMiss 的 5% 阈值保留，但改为具名常量并在文档中说明它是经验值。

//! 响应侧缓存诊断（架构 §9.1.1）。
//!
//! [`crate::cache`] 回答的是"**我这次发的前缀和上次一样吗**"——纯请求侧，
//! 不需要端点配合。本模块回答的是"**端点那边到底命中了没有**"，
//! 并把两者配对，从而分出三种只靠单侧无法区分的情况：
//!
//! | 现象 | 请求侧 | 响应侧 | 结论 |
//! |---|---|---|---|
//! | 前缀变了、没命中 | 有断点 | read=0 | 归因到具体段，**可优化** |
//! | 前缀没变、没命中 | 无断点 | read=0 | [`CacheBreakCause::TtlExpiry`]，等或加断点 |
//! | 前缀没变、无字段 | 无断点 | 全程无缓存字段 | [`CacheDiagnosis::Unsupported`]，**端点没开** |
//!
//! 第三行是本模块相对参考实现的主要增补。把它归进"命中率 0%"会让人一直去调
//! 前缀，而真正该做的是去查端点配置——这正是本项目实测踩过的坑。

use agentrs_contracts::manifest::CacheBreakCause;

use crate::cache::{attribute, RequestSnapshot};

/// 命中率相对上一轮的下跌超过此比例即判为部分失效。
///
/// **经验值**：低于此幅度的波动通常来自正常追加导致的分母增长，
/// 报出来只会淹没真正的断裂。
const PARTIAL_MISS_DROP: f64 = 0.05;

/// 单次响应报告的缓存用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ObservedUsage {
    /// 完整输入 token（含缓存部分）。
    pub input_tokens: u64,
    /// 命中缓存的部分。
    pub cache_read_tokens: u64,
    /// 写入缓存的部分。
    pub cache_creation_tokens: u64,
    /// 端点**是否报告了**缓存字段。
    ///
    /// 与"报告了但为 0"是两回事：前者说明端点没开缓存，
    /// 后者说明开了但这次没命中。混同会把配置问题误诊成前缀问题。
    pub reports_cache_fields: bool,
}

impl ObservedUsage {
    fn has_cache(&self) -> bool {
        self.cache_read_tokens > 0 || self.cache_creation_tokens > 0
    }

    fn hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        self.cache_read_tokens as f64 / self.input_tokens as f64
    }
}

/// 一次诊断结论。
#[derive(Debug, Clone, PartialEq)]
pub enum CacheDiagnosis {
    /// 端点不报告缓存字段。**这是配置问题，不是命中率问题。**
    Unsupported,
    /// 首次请求，无可比对象。
    FirstRequest,
    /// 健康。
    Healthy {
        /// 本次命中率。
        hit_rate: f64,
    },
    /// 部分失效。
    PartialMiss {
        /// 本次命中率。
        hit_rate: f64,
        /// 归因。
        cause: CacheBreakCause,
    },
    /// 完全失效：上一轮有缓存，这一轮读取归零。
    FullMiss {
        /// 归因。
        cause: CacheBreakCause,
    },
}

impl CacheDiagnosis {
    /// 是否构成一次需要计入 `CacheStats::breaks` 的断裂。
    ///
    /// `Unsupported` 与 `FirstRequest` **不算**——把它们计入会让命中率报表
    /// 在冷启动和不支持的端点上永远难看，从而失去告警价值。
    pub fn is_break(&self) -> bool {
        matches!(self, Self::PartialMiss { .. } | Self::FullMiss { .. })
    }

    /// 归因（若有）。
    pub fn cause(&self) -> Option<CacheBreakCause> {
        match self {
            Self::PartialMiss { cause, .. } | Self::FullMiss { cause } => Some(*cause),
            _ => None,
        }
    }
}

/// 跨轮配对请求侧与响应侧的检测器。
///
/// 持有**上一轮**的快照与用量。每轮先 [`record_request`](Self::record_request)
/// 再 [`diagnose`](Self::diagnose)；顺序颠倒会把本轮快照当成上一轮来归因。
#[derive(Debug, Default)]
pub struct CacheBreakDetector {
    prev_snapshot: Option<RequestSnapshot>,
    curr_snapshot: Option<RequestSnapshot>,
    prev_usage: Option<ObservedUsage>,
    /// 至今为止是否见过任何缓存字段。
    seen_cache_fields: bool,
}

impl CacheBreakDetector {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录本次请求的前缀快照。**必须在发请求前调用。**
    pub fn record_request(&mut self, snapshot: RequestSnapshot) {
        self.prev_snapshot = self.curr_snapshot.take();
        self.curr_snapshot = Some(snapshot);
    }

    /// 用本次响应的用量给出诊断。
    ///
    /// 未先 `record_request` 时返回 `None`——**不猜**，缺快照就没有归因依据。
    pub fn diagnose(&mut self, usage: ObservedUsage) -> Option<CacheDiagnosis> {
        self.curr_snapshot.as_ref()?;
        if usage.reports_cache_fields {
            self.seen_cache_fields = true;
        }
        let d = self.compute(&usage);
        self.prev_usage = Some(usage);
        Some(d)
    }

    fn compute(&self, usage: &ObservedUsage) -> CacheDiagnosis {
        // 端点从没报过缓存字段 → 配置问题，先别谈命中率。
        if !self.seen_cache_fields {
            return CacheDiagnosis::Unsupported;
        }

        let Some(prev) = &self.prev_usage else {
            return CacheDiagnosis::FirstRequest;
        };

        // 报了字段但两轮都全 0：端点声称支持却始终不缓存，
        // 同样归为不支持而不是"连续 miss"。
        if !prev.has_cache() && !usage.has_cache() {
            return CacheDiagnosis::Unsupported;
        }

        // 上一轮有缓存、这一轮读取归零 → 完全失效。
        if prev.has_cache() && usage.cache_read_tokens == 0 {
            return CacheDiagnosis::FullMiss {
                cause: self.attribute_cause(),
            };
        }

        let hit_rate = usage.hit_rate();

        if prev.cache_read_tokens > 0 {
            let drop = 1.0 - (usage.cache_read_tokens as f64 / prev.cache_read_tokens as f64);
            if drop > PARTIAL_MISS_DROP {
                return CacheDiagnosis::PartialMiss {
                    hit_rate,
                    cause: self.attribute_cause(),
                };
            }
        }

        CacheDiagnosis::Healthy { hit_rate }
    }

    /// 请求侧归因优先；请求侧说"前缀没变"而缓存仍然丢了，那就是服务端 TTL。
    fn attribute_cause(&self) -> CacheBreakCause {
        let Some(curr) = &self.curr_snapshot else {
            return CacheBreakCause::TtlExpiry;
        };
        attribute(self.prev_snapshot.as_ref(), curr).unwrap_or(CacheBreakCause::TtlExpiry)
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::ids::Digest;
    use agentrs_contracts::manifest::CacheSegment;

    use super::*;
    use crate::cache::Segment;

    fn 快照(system: &str, tools: &str, provider: &str) -> RequestSnapshot {
        RequestSnapshot {
            prefix_digest: Digest::from_hex(format!("{system}-{tools}-{provider}")),
            stable: vec![
                Segment {
                    kind: CacheSegment::S0SystemRules,
                    digest: Digest::from_hex(system),
                },
                Segment {
                    kind: CacheSegment::S1ToolCatalog,
                    digest: Digest::from_hex(tools),
                },
            ],
            surface_invalidation: None,
            provider: provider.into(),
            component_generations: Default::default(),
            permission_mode: "default".into(),
            steering_injected: false,
        }
    }

    fn 有缓存(input: u64, read: u64) -> ObservedUsage {
        ObservedUsage {
            input_tokens: input,
            cache_read_tokens: read,
            cache_creation_tokens: 0,
            reports_cache_fields: true,
        }
    }

    fn 无缓存字段(input: u64) -> ObservedUsage {
        ObservedUsage {
            input_tokens: input,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            reports_cache_fields: false,
        }
    }

    #[test]
    fn 端点不报缓存字段时判为不支持而不是零命中() {
        // 这正是本项目实测踩过的坑：vLLM 侧 prefix caching 未生效但无任何报错，
        // 若归成"命中率 0%"，排查方向会一直错在前缀上。
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        assert_eq!(d.diagnose(无缓存字段(1000)), Some(CacheDiagnosis::Unsupported));
        d.record_request(快照("a", "t", "p"));
        assert_eq!(d.diagnose(无缓存字段(2000)), Some(CacheDiagnosis::Unsupported));
    }

    #[test]
    fn 报了字段但始终全零也判为不支持() {
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        d.diagnose(有缓存(1000, 0));
        d.record_request(快照("a", "t", "p"));
        assert_eq!(d.diagnose(有缓存(2000, 0)), Some(CacheDiagnosis::Unsupported));
    }

    #[test]
    fn 不支持与首次请求都不计入断裂() {
        // 计入会让冷启动和不支持的端点上报表永远难看，告警随之失效。
        assert!(!CacheDiagnosis::Unsupported.is_break());
        assert!(!CacheDiagnosis::FirstRequest.is_break());
        assert!(!CacheDiagnosis::Healthy { hit_rate: 0.9 }.is_break());
        assert!(CacheDiagnosis::FullMiss {
            cause: CacheBreakCause::TtlExpiry
        }
        .is_break());
    }

    #[test]
    fn 首轮有缓存时报首次请求() {
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        assert_eq!(d.diagnose(有缓存(1000, 800)), Some(CacheDiagnosis::FirstRequest));
    }

    #[test]
    fn 稳定命中判为健康() {
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        d.diagnose(有缓存(1000, 900));
        d.record_request(快照("a", "t", "p"));
        match d.diagnose(有缓存(1100, 900)).unwrap() {
            CacheDiagnosis::Healthy { hit_rate } => assert!((hit_rate - 0.818).abs() < 0.01),
            other => panic!("期望 Healthy，得到 {other:?}"),
        }
    }

    #[test]
    fn 系统提示变化被归因到具体段() {
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        d.diagnose(有缓存(1000, 900));
        // 系统段变了 → 前缀全毁。
        d.record_request(快照("b", "t", "p"));
        assert_eq!(
            d.diagnose(有缓存(1000, 0)).unwrap().cause(),
            Some(CacheBreakCause::SystemPromptChanged)
        );
    }

    #[test]
    fn 工具目录变化被归因到具体段() {
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t1", "p"));
        d.diagnose(有缓存(1000, 900));
        d.record_request(快照("a", "t2", "p"));
        assert_eq!(
            d.diagnose(有缓存(1000, 0)).unwrap().cause(),
            Some(CacheBreakCause::ToolCatalogChanged)
        );
    }

    #[test]
    fn 前缀未变却丢缓存时归因为_ttl_过期() {
        // **这是单靠请求侧永远看不见的那一类**——前缀完全一致，服务端自己丢了。
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        d.diagnose(有缓存(1000, 900));
        d.record_request(快照("a", "t", "p"));
        assert_eq!(
            d.diagnose(有缓存(1000, 0)).unwrap().cause(),
            Some(CacheBreakCause::TtlExpiry)
        );
    }

    #[test]
    fn provider_切换的归因优先于段变化() {
        // 换 provider 会连带改变一切；归到"系统提示变了"没有解释力。
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p1"));
        d.diagnose(有缓存(1000, 900));
        d.record_request(快照("b", "t2", "p2"));
        assert_eq!(
            d.diagnose(有缓存(1000, 0)).unwrap().cause(),
            Some(CacheBreakCause::ProviderSwitched)
        );
    }

    #[test]
    fn 小幅下跌不报部分失效() {
        // 正常追加会让分母增长、命中 token 略降，报出来只会淹没真断裂。
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        d.diagnose(有缓存(1000, 1000));
        d.record_request(快照("a", "t", "p"));
        assert!(matches!(
            d.diagnose(有缓存(1100, 980)).unwrap(),
            CacheDiagnosis::Healthy { .. }
        ));
    }

    #[test]
    fn 大幅下跌报部分失效() {
        let mut d = CacheBreakDetector::new();
        d.record_request(快照("a", "t", "p"));
        d.diagnose(有缓存(1000, 1000));
        d.record_request(快照("b", "t", "p"));
        match d.diagnose(有缓存(1000, 400)).unwrap() {
            CacheDiagnosis::PartialMiss { cause, hit_rate } => {
                assert_eq!(cause, CacheBreakCause::SystemPromptChanged);
                assert!((hit_rate - 0.4).abs() < 1e-9);
            }
            other => panic!("期望 PartialMiss，得到 {other:?}"),
        }
    }

    #[test]
    fn 缺请求快照时不猜() {
        // 没有快照就没有归因依据。给个"看起来合理"的结论比不给更糟。
        let mut d = CacheBreakDetector::new();
        assert_eq!(d.diagnose(有缓存(1000, 900)), None);
    }
}
