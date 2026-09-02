// Ported from aionrs (Apache-2.0), crates/aion-compact.
//   Source: crates/aion-compact/src/level.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 文档改写为"每一级各自不安全在哪"；`Default` 从 derive 改为手写以便
//            把"为什么默认是 Safe"写在它旁边。

//! 压缩强度。
//!
//! 分级的理由是**不同输出经得起的处理不一样**。命令输出里的 ANSI 与回车覆盖是
//! 纯噪声，去掉它一个字节的信息也不损失；而折叠相似行会**改变内容**——对一份
//! 源代码那样做，模型读到的就不再是文件里的东西了。
//!
//! 所以默认停在 [`Safe`](CompactLevel::Safe)：只做无损的那半。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// 压缩强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactLevel {
    /// 原样。
    Off,
    /// **无损**：去 ANSI、折叠回车覆盖、去行尾空白、合并连续空行。
    ///
    /// 这几样都不改变文本要说的事——它们改变的只是终端曾经怎么把它画出来。
    Safe,
    /// 有损：在 `Safe` 之上再折叠相似行、重排 JSON。
    ///
    /// **会改变内容**，所以只该用在确定不需要逐字节保真的地方。
    Full,
}

impl Default for CompactLevel {
    /// 默认 `Safe`。
    ///
    /// fail-closed 的方向在这里是"少改"：一个把文件内容折叠掉的默认值，
    /// 症状是模型读到一份它以为完整、实际被改写过的文本——而那种错误
    /// 不会在任何一步报错。
    fn default() -> Self {
        Self::Safe
    }
}

impl fmt::Display for CompactLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => write!(f, "off"),
            Self::Safe => write!(f, "safe"),
            Self::Full => write!(f, "full"),
        }
    }
}

impl FromStr for CompactLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "safe" => Ok(Self::Safe),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown compaction level: '{other}' (expected: off, safe, full)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认是安全的那一级() {
        // 默认折叠内容的话，症状是模型读到一份它以为完整、实际被改写过的文本，
        // 而那种错误不会在任何一步报错。
        assert_eq!(CompactLevel::default(), CompactLevel::Safe);
    }

    #[test]
    fn 三级都能来回转换() {
        for level in [CompactLevel::Off, CompactLevel::Safe, CompactLevel::Full] {
            assert_eq!(level.to_string().parse::<CompactLevel>().unwrap(), level);
            let json = serde_json::to_string(&level).unwrap();
            assert_eq!(serde_json::from_str::<CompactLevel>(&json).unwrap(), level);
        }
    }

    #[test]
    fn 大小写不敏感() {
        assert_eq!("FULL".parse::<CompactLevel>().unwrap(), CompactLevel::Full);
        assert_eq!("Safe".parse::<CompactLevel>().unwrap(), CompactLevel::Safe);
    }

    #[test]
    fn 认不出来的取值报错并列出可选项() {
        // 配置写错时，"unknown level: fast" 让人不知道该写什么。
        let e = "fast".parse::<CompactLevel>().unwrap_err();
        assert!(e.contains("fast"), "{e}");
        assert!(e.contains("off, safe, full"), "{e}");
    }
}
