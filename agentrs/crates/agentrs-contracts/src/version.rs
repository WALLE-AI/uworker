//! Schema 版本与恢复兼容窗口（架构 §4.1）。
//!
//! durable workflow 的核心场景就是"应用升级后恢复昨天挂起的 Run"。
//! 因此**禁止静默降级恢复**——不兼容时必须显式拒绝并交给用户决策。

use serde::{Deserialize, Serialize};

/// Schema 版本号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpecVersion(pub u32);

/// 内核声明的兼容窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatWindow {
    /// 支持恢复的最低版本。
    pub min_supported: SpecVersion,
    /// 当前版本。
    pub current: SpecVersion,
}

/// 兼容性判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatVerdict {
    /// 版本相符，直接恢复。
    Exact,
    /// 窗口内的旧版本，走显式 migration，并写一条 `CheckpointMigrated` 事件。
    NeedsMigration,
    /// 低于 `min_supported`：拒绝恢复。
    TooOld,
    /// 高于 `current`：拒绝恢复（降级会丢字段）。
    TooNew,
}

impl CompatWindow {
    /// 判定一个 checkpoint 的版本能否恢复。
    pub fn verdict(&self, v: SpecVersion) -> CompatVerdict {
        if v > self.current {
            CompatVerdict::TooNew
        } else if v < self.min_supported {
            CompatVerdict::TooOld
        } else if v == self.current {
            CompatVerdict::Exact
        } else {
            CompatVerdict::NeedsMigration
        }
    }

    /// 兼容窗口至少覆盖两个发行版；缩窗必须是显式的破坏性变更公告。
    pub fn spans_at_least_two_releases(&self) -> bool {
        self.current.0.saturating_sub(self.min_supported.0) >= 1
    }
}

/// 本内核当前的兼容窗口。
pub const COMPAT_WINDOW: CompatWindow = CompatWindow {
    min_supported: SpecVersion(1),
    current: SpecVersion(1),
};

#[cfg(test)]
mod tests {
    use super::*;

    const W: CompatWindow = CompatWindow {
        min_supported: SpecVersion(2),
        current: SpecVersion(4),
    };

    #[test]
    fn 版本判定四种情形() {
        assert_eq!(W.verdict(SpecVersion(4)), CompatVerdict::Exact);
        assert_eq!(W.verdict(SpecVersion(3)), CompatVerdict::NeedsMigration);
        assert_eq!(W.verdict(SpecVersion(1)), CompatVerdict::TooOld);
        assert_eq!(W.verdict(SpecVersion(5)), CompatVerdict::TooNew);
    }

    #[test]
    fn 过新版本必须拒绝而非降级() {
        // 高版本 checkpoint 可能含本版本不认识的字段，降级读取会静默丢数据。
        assert_eq!(W.verdict(SpecVersion(99)), CompatVerdict::TooNew);
    }

    #[test]
    fn 窗口跨度检查() {
        assert!(W.spans_at_least_two_releases());
        let narrow = CompatWindow {
            min_supported: SpecVersion(3),
            current: SpecVersion(3),
        };
        assert!(!narrow.spans_at_least_two_releases(), "单点窗口需要显式公告");
    }
}
