//! 确定性时钟（任务 T02）。
//!
//! 内核不读真实时钟（边界判据，见 `clippy.toml`）。测试用它精确控制时刻，
//! 使审批超时、预算耗尽等时间相关路径可复现。

use std::sync::atomic::{AtomicI64, Ordering};

use agentrs_contracts::ids::Timestamp;
use agentrs_contracts::ports::Clock;

/// 手动推进的时钟。
#[derive(Debug)]
pub struct FakeClock {
    now: AtomicI64,
}

impl FakeClock {
    /// 从指定时刻起步。
    pub fn new(start: i64) -> Self {
        Self {
            now: AtomicI64::new(start),
        }
    }

    /// 推进若干毫秒。
    pub fn advance(&self, millis: i64) {
        self.now.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.now.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 时钟只在被推进时前进() {
        let c = FakeClock::new(100);
        assert_eq!(c.now(), Timestamp(100));
        assert_eq!(c.now(), Timestamp(100), "不推进则不变，时间相关路径可复现");
        c.advance(60_000);
        assert_eq!(c.now(), Timestamp(60_100));
    }
}
