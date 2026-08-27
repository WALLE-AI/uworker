//! 工具定义。
//!
//! **`ToolDef` 不可执行**——它只描述"模型知道有这么个工具"。
//! 真正的执行体在 SandboxRS / Core adapter 里，内核只负责把 schema 告诉模型、
//! 把提议送进固定管线（架构 §8）。
//!
//! ## 两个正交的布尔，不要合并
//!
//! [`EffectProfile`]（会不会改东西）与 `concurrency_safe`（能不能并行）
//! 看起来相关，实际互不蕴含：
//!
//! - 只读但不可并发：读一份受锁保护的资源，并发读会争锁；
//! - 可并发但会改东西：向互不相同的路径各自追加，彼此无冲突。
//!
//! 用其中一个冒充另一个，早晚会在 Plan 模式下放过一个写类工具，
//! 或者把一批本可并行的读串起来。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 工具的副作用画像。
///
/// **Plan 模式据此过滤工具目录**（架构 §4.1.2 规则 1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectProfile {
    /// 只读：不改变工作区、不发起外部写入。
    ReadOnly,
    /// 会改变状态。
    ///
    /// **fail-closed 默认值**：未声明的工具一律按会改东西处理。
    /// 反过来默认会让第三方注册的工具在 Plan 模式下畅通无阻。
    #[default]
    Mutating,
}

impl EffectProfile {
    /// 是否只读。
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

/// 一个模型可见的工具。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    /// 工具名。
    pub name: String,
    /// 给模型看的描述。
    ///
    /// **描述文本归 Core**（随注册一起提供），但它直接决定 Agent 好不好用——
    /// 内核对它做质量 lint 并在 eval 中覆盖"工具选择正确性"（架构 §1.1 裁定 3）。
    pub description: String,
    /// 参数的 JSON Schema。
    pub parameters: Value,
    /// 副作用画像。
    ///
    /// **fail-closed**：缺字段落到 [`EffectProfile::Mutating`]，
    /// 于是它在 Plan 模式下被挡掉而不是放行。
    #[serde(default)]
    pub effect: EffectProfile,
    /// 并发判定。
    ///
    /// **fail-closed**：只有明确为 `true` 才允许并行；未知、未声明、
    /// 无法判定一律 exclusive，且 exclusive 形成 ordering barrier（架构 §8.3）。
    ///
    /// 与 [`effect`](Self::effect) 正交——见模块文档。
    #[serde(default)]
    pub concurrency_safe: bool,
}

impl ToolDef {
    /// 构造一个只读工具（默认可并发）。
    pub fn read_only(name: &str, description: &str, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            effect: EffectProfile::ReadOnly,
            concurrency_safe: true,
        }
    }

    /// 构造一个写类工具（默认独占）。
    pub fn mutating(name: &str, description: &str, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            effect: EffectProfile::Mutating,
            concurrency_safe: false,
        }
    }

    /// 覆盖并发判定。
    ///
    /// 存在的理由就是那两种"正交"的情形：只读但不可并发、
    /// 会改东西但可并发。见模块文档。
    pub fn with_concurrency_safe(mut self, safe: bool) -> Self {
        self.concurrency_safe = safe;
        self
    }

    /// 是否只读。
    pub fn is_read_only(&self) -> bool {
        self.effect.is_read_only()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 写类工具默认独占且非只读() {
        // fail-closed：拿不准就串行、就当它会改东西。
        let w = ToolDef::mutating("Write", "写文件", serde_json::json!({}));
        assert!(!w.concurrency_safe);
        assert!(!w.is_read_only());

        let r = ToolDef::read_only("Read", "读文件", serde_json::json!({}));
        assert!(r.concurrency_safe);
        assert!(r.is_read_only());
    }

    #[test]
    fn 缺省字段反序列化为不可并发且会改东西() {
        // 老版本或第三方注册没写这两个字段时，必须双双落到安全的那一侧。
        // 尤其 effect：默认成只读会让第三方工具在 Plan 模式下畅通无阻。
        let d: ToolDef = serde_json::from_str(r#"{"name":"X","description":"d","parameters":{}}"#).unwrap();
        assert!(!d.concurrency_safe);
        assert!(!d.is_read_only(), "未声明的工具不得被当成只读");
        assert_eq!(d.effect, EffectProfile::Mutating);
    }

    #[test]
    fn 两个布尔正交可独立设置() {
        // 只读但不可并发：读一份受锁保护的资源。
        let 独占读 =
            ToolDef::read_only("LockedRead", "读", serde_json::json!({})).with_concurrency_safe(false);
        assert!(独占读.is_read_only());
        assert!(!独占读.concurrency_safe);

        // 会改东西但可并发：向互不相同的路径各自追加。
        let 并发写 =
            ToolDef::mutating("AppendDistinct", "追加", serde_json::json!({})).with_concurrency_safe(true);
        assert!(!并发写.is_read_only());
        assert!(并发写.concurrency_safe);
    }

    #[test]
    fn effect_序列化为稳定字符串() {
        // 事件与 manifest 会带上它，字符串一变，历史就读不回来了。
        let j = serde_json::to_value(EffectProfile::ReadOnly).unwrap();
        assert_eq!(j, serde_json::json!("read_only"));
        assert_eq!(
            serde_json::to_value(EffectProfile::Mutating).unwrap(),
            serde_json::json!("mutating")
        );
    }
}
