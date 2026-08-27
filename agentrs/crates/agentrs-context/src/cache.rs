//! 缓存前缀布局、摘要与断裂归因（架构 §9.1.1，任务 T05C）。
//!
//! ## 为什么这是一等约束
//!
//! Provider 的前缀缓存是**严格前缀匹配**：前缀任何一字节变化，从该点起全部 miss。
//! 它同时是最大的成本项和 TTFT 项。若不加协调，多项机制会各自砸掉缓存——
//! Microcompact 改写历史中段、deferred schema 加载改变工具目录、
//! ContextModifier 中途注入、模式切换、provider fallback。
//!
//! ## S2 的稳定性是 ModelSurface 的推论
//!
//! 初版为保住缓存设过一整套"S0–S3 只能追加、改写攒到边界"的规则。采用
//! Surface 模型后它们被取代了：log 是 append-only，Surface 的 append-origin
//! 前缀天然稳定；一次 `Replace` 从其 `range.start` 起使前缀失效，而**失效点是
//! 已知且可精确计算的**，不需要靠约定去近似。

use agentrs_contracts::ids::{Digest, EventSequence};
use agentrs_contracts::manifest::{CacheBreakCause, CacheSegment};

/// 一段参与缓存前缀计算的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// 所属分段。
    pub kind: CacheSegment,
    /// 该段的内容指纹。**不是正文**——摘要计算不需要也不应持有用户内容。
    pub digest: Digest,
}

/// 一次请求的缓存布局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    /// 按装配顺序排列的分段。
    pub segments: Vec<Segment>,
    /// Surface 上最近一次 `Replace` 的起点。`None` 表示前缀完全稳定。
    pub surface_invalidation: Option<EventSequence>,
}

impl CacheLayout {
    /// 稳定前缀的摘要（覆盖 S0–S2）。
    ///
    /// 只把**稳定段**纳入计算——S3/S4 每轮都变，纳入会让摘要失去比较意义。
    pub fn prefix_digest(&self) -> Digest {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for seg in self.stable_segments() {
            for b in seg.digest.as_str().as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x1000_0000_01b3);
            }
            // 分隔符：避免 ("ab","c") 与 ("a","bc") 撞出同一摘要。
            h ^= 0xff;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        Digest::from_hex(format!("{h:016x}"))
    }

    /// 稳定前缀部分，按装配顺序。
    pub fn stable_segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.iter().filter(|s| s.kind.is_stable_prefix())
    }

    /// 缓存断点位置：稳定前缀与可变段的交界。
    pub fn breakpoints(&self) -> Vec<CacheSegment> {
        let mut out = Vec::new();
        let mut prev_stable = true;
        for seg in &self.segments {
            let stable = seg.kind.is_stable_prefix();
            if prev_stable && !stable {
                out.push(seg.kind);
            }
            prev_stable = stable;
        }
        out
    }

    /// 校验分段顺序：稳定段必须全部排在可变段之前。
    ///
    /// 顺序错乱会让"前缀"这个概念本身失效——可变段夹在稳定段中间时，
    /// 它之后的一切都不可能命中缓存。
    pub fn is_well_ordered(&self) -> bool {
        let mut seen_volatile = false;
        for seg in &self.segments {
            if seg.kind.is_stable_prefix() {
                if seen_volatile {
                    return false;
                }
            } else {
                seen_volatile = true;
            }
        }
        true
    }
}

/// 归因所需的两次请求快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSnapshot {
    /// 稳定前缀摘要。
    pub prefix_digest: Digest,
    /// 各稳定段的摘要，用于定位**哪一段**变了。
    pub stable: Vec<Segment>,
    /// Surface 失效点。
    pub surface_invalidation: Option<EventSequence>,
    /// 本次使用的 provider。
    pub provider: String,
    /// 本次的权限模式判别串。
    pub permission_mode: String,
    /// 本次是否由 steering 注入触发。
    pub steering_injected: bool,
}

/// 归因一次缓存断裂。
///
/// `None` 表示前缀未变——此时若仍然 miss，原因在服务端（TTL 过期或未启用）。
///
/// **每次 miss 都必须能落到一个具体原因上**，否则"命中率低"就无从优化。
pub fn attribute(prev: Option<&RequestSnapshot>, curr: &RequestSnapshot) -> Option<CacheBreakCause> {
    let Some(prev) = prev else {
        return Some(CacheBreakCause::FirstRequest);
    };

    if prev.prefix_digest == curr.prefix_digest {
        return None;
    }

    // 按"根因优先"排序：provider 切换会连带改变一切，先判它。
    if prev.provider != curr.provider {
        return Some(CacheBreakCause::ProviderSwitched);
    }
    if prev.permission_mode != curr.permission_mode {
        // 模式切换同时改变系统段与工具目录投影，归因到模式本身更有解释力。
        return Some(CacheBreakCause::PermissionModeChanged);
    }
    if curr.surface_invalidation != prev.surface_invalidation && curr.surface_invalidation.is_some() {
        return Some(CacheBreakCause::HistoryRewritten);
    }

    // 逐段定位。
    let changed = |kind: CacheSegment| -> bool {
        let a = prev.stable.iter().find(|s| s.kind == kind).map(|s| &s.digest);
        let b = curr.stable.iter().find(|s| s.kind == kind).map(|s| &s.digest);
        a != b
    };
    if changed(CacheSegment::S0SystemRules) {
        return Some(CacheBreakCause::SystemPromptChanged);
    }
    if changed(CacheSegment::S1ToolCatalog) {
        return Some(CacheBreakCause::ToolCatalogChanged);
    }
    if changed(CacheSegment::S2Surface) {
        // Surface 段变了但没有 Replace —— 那就是正常追加。
        // 正常追加不该破坏前缀；走到这里说明装配把可变内容混进了稳定段。
        return Some(CacheBreakCause::HistoryRewritten);
    }
    if curr.steering_injected {
        return Some(CacheBreakCause::SteeringInjected);
    }
    None
}

/// 命中率统计。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 累计输入 token。
    pub input_tokens: u64,
    /// 累计命中 token。
    pub cache_read_tokens: u64,
    /// 观测到的断裂次数。
    pub breaks: u32,
}

impl CacheStats {
    /// 累加一次请求的用量。
    pub fn record(&mut self, input: u64, cache_read: u64, broke: bool) {
        self.input_tokens += input;
        self.cache_read_tokens += cache_read;
        if broke {
            self.breaks += 1;
        }
    }

    /// 命中率。无输入时返回 `None`。
    ///
    /// M1 收益验收要求：**在报告缓存字段的端点上**连续 10 轮 ≥ 70%。
    pub fn hit_rate(&self) -> Option<f64> {
        if self.input_tokens == 0 {
            return None;
        }
        Some(self.cache_read_tokens as f64 / self.input_tokens as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(kind: CacheSegment, d: &str) -> Segment {
        Segment {
            kind,
            digest: Digest::from_hex(d),
        }
    }

    fn 布局(segs: Vec<Segment>) -> CacheLayout {
        CacheLayout {
            segments: segs,
            surface_invalidation: None,
        }
    }

    fn 标准布局() -> CacheLayout {
        布局(vec![
            seg(CacheSegment::S0SystemRules, "sys"),
            seg(CacheSegment::S1ToolCatalog, "tools"),
            seg(CacheSegment::S2Surface, "hist"),
            seg(CacheSegment::S3Selected, "mem"),
            seg(CacheSegment::S4Recent, "recent"),
        ])
    }

    fn 快照(layout: &CacheLayout) -> RequestSnapshot {
        RequestSnapshot {
            prefix_digest: layout.prefix_digest(),
            stable: layout.stable_segments().cloned().collect(),
            surface_invalidation: layout.surface_invalidation,
            provider: "p1".into(),
            permission_mode: "default".into(),
            steering_injected: false,
        }
    }

    // ---------- 摘要 ----------

    #[test]
    fn 摘要只覆盖稳定段() {
        let a = 标准布局();
        let mut b = 标准布局();
        // 改可变段不应影响前缀摘要。
        b.segments[3] = seg(CacheSegment::S3Selected, "别的记忆");
        b.segments[4] = seg(CacheSegment::S4Recent, "别的结果");
        assert_eq!(a.prefix_digest(), b.prefix_digest());
    }

    #[test]
    fn 稳定段变化改变摘要() {
        let a = 标准布局();
        let mut b = 标准布局();
        b.segments[0] = seg(CacheSegment::S0SystemRules, "改过的系统段");
        assert_ne!(a.prefix_digest(), b.prefix_digest());
    }

    #[test]
    fn 摘要对分段边界敏感() {
        // ("ab","c") 与 ("a","bc") 不得撞出同一摘要，否则归因会误判。
        let x = 布局(vec![
            seg(CacheSegment::S0SystemRules, "ab"),
            seg(CacheSegment::S1ToolCatalog, "c"),
        ]);
        let y = 布局(vec![
            seg(CacheSegment::S0SystemRules, "a"),
            seg(CacheSegment::S1ToolCatalog, "bc"),
        ]);
        assert_ne!(x.prefix_digest(), y.prefix_digest());
    }

    #[test]
    fn 摘要是确定性的() {
        let a = 标准布局();
        let first = a.prefix_digest();
        for _ in 0..8 {
            assert_eq!(a.prefix_digest(), first);
        }
    }

    // ---------- 布局 ----------

    #[test]
    fn 断点在稳定与可变的交界处() {
        assert_eq!(标准布局().breakpoints(), vec![CacheSegment::S3Selected]);
    }

    #[test]
    fn 稳定段必须全部排在可变段之前() {
        assert!(标准布局().is_well_ordered());

        // 可变段夹在稳定段中间——它之后的一切都不可能命中缓存。
        let 错序 = 布局(vec![
            seg(CacheSegment::S0SystemRules, "sys"),
            seg(CacheSegment::S3Selected, "mem"),
            seg(CacheSegment::S1ToolCatalog, "tools"),
        ]);
        assert!(!错序.is_well_ordered());
    }

    // ---------- 归因 ----------

    #[test]
    fn 首次请求归因为_first_request() {
        assert_eq!(
            attribute(None, &快照(&标准布局())),
            Some(CacheBreakCause::FirstRequest)
        );
    }

    #[test]
    fn 前缀未变时不产生断裂归因() {
        // 此时若仍 miss，原因在服务端（TTL 或未启用），不是我们的问题。
        let s = 快照(&标准布局());
        assert_eq!(attribute(Some(&s), &s), None);
    }

    #[test]
    fn 系统段变化被正确归因() {
        let prev = 快照(&标准布局());
        let mut l = 标准布局();
        l.segments[0] = seg(CacheSegment::S0SystemRules, "新系统段");
        assert_eq!(
            attribute(Some(&prev), &快照(&l)),
            Some(CacheBreakCause::SystemPromptChanged)
        );
    }

    #[test]
    fn 工具目录变化被正确归因() {
        // deferred schema 加载是最常见的触发。
        let prev = 快照(&标准布局());
        let mut l = 标准布局();
        l.segments[1] = seg(CacheSegment::S1ToolCatalog, "多了一个工具");
        assert_eq!(
            attribute(Some(&prev), &快照(&l)),
            Some(CacheBreakCause::ToolCatalogChanged)
        );
    }

    #[test]
    fn surface_replace_归因为历史改写() {
        let prev = 快照(&标准布局());
        let mut l = 标准布局();
        l.segments[2] = seg(CacheSegment::S2Surface, "压缩后");
        l.surface_invalidation = Some(EventSequence(5));
        assert_eq!(
            attribute(Some(&prev), &快照(&l)),
            Some(CacheBreakCause::HistoryRewritten)
        );
    }

    #[test]
    fn provider_切换优先于其他归因() {
        // 它会连带改变一切，归因到最根本的原因才有解释力。
        let prev = 快照(&标准布局());
        let mut l = 标准布局();
        l.segments[0] = seg(CacheSegment::S0SystemRules, "也变了");
        let mut curr = 快照(&l);
        curr.provider = "p2".into();
        assert_eq!(
            attribute(Some(&prev), &curr),
            Some(CacheBreakCause::ProviderSwitched)
        );
    }

    #[test]
    fn 模式切换优先于逐段定位() {
        // 模式切换同时改系统段与目录投影，归因到模式本身更有解释力。
        let prev = 快照(&标准布局());
        let mut l = 标准布局();
        l.segments[0] = seg(CacheSegment::S0SystemRules, "plan 模式的系统段");
        l.segments[1] = seg(CacheSegment::S1ToolCatalog, "只读工具");
        let mut curr = 快照(&l);
        curr.permission_mode = "plan".into();
        assert_eq!(
            attribute(Some(&prev), &curr),
            Some(CacheBreakCause::PermissionModeChanged)
        );
    }

    #[test]
    fn 每次断裂都能归因_不存在未知原因() {
        // M1 功能验收：100% miss 可归因到具体 CacheBreakCause。
        let prev = 快照(&标准布局());
        let 变体: Vec<Box<dyn Fn() -> RequestSnapshot>> = vec![
            Box::new(|| {
                let mut l = 标准布局();
                l.segments[0] = seg(CacheSegment::S0SystemRules, "x");
                快照(&l)
            }),
            Box::new(|| {
                let mut l = 标准布局();
                l.segments[1] = seg(CacheSegment::S1ToolCatalog, "x");
                快照(&l)
            }),
            Box::new(|| {
                let mut l = 标准布局();
                l.segments[2] = seg(CacheSegment::S2Surface, "x");
                l.surface_invalidation = Some(EventSequence(1));
                快照(&l)
            }),
            Box::new(|| {
                let mut c = 快照(&标准布局());
                c.provider = "other".into();
                c.prefix_digest = Digest::from_hex("差异");
                c
            }),
            Box::new(|| {
                let mut c = 快照(&标准布局());
                c.permission_mode = "plan".into();
                c.prefix_digest = Digest::from_hex("差异");
                c
            }),
        ];
        for (i, mk) in 变体.iter().enumerate() {
            let curr = mk();
            assert!(
                attribute(Some(&prev), &curr).is_some(),
                "第 {i} 个变体无法归因——存在未覆盖的断裂原因"
            );
        }
    }

    // ---------- 统计 ----------

    #[test]
    fn 命中率按累计_token_计算() {
        let mut s = CacheStats::default();
        s.record(1000, 0, true); // 首次全 miss
        s.record(1000, 900, false);
        s.record(1000, 900, false);
        // 2700 / 3000 = 0.9
        assert_eq!(s.hit_rate(), Some(0.6));
        assert_eq!(s.breaks, 1);
    }

    #[test]
    fn 无输入时命中率为_none() {
        assert_eq!(CacheStats::default().hit_rate(), None);
    }
}
