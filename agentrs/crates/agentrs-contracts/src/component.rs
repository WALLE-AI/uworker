//! Phase D 组件清单与 generation 契约。

use serde::{Deserialize, Serialize};

use crate::authority::CapabilityView;
use crate::ids::ComponentId;

/// 已提交的组件代际。0 保留为“尚未提交”。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Generation(pub u64);

impl Generation {
    /// 下一代；溢出时拒绝继续换代。
    pub fn checked_next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// 组件在组合树中的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// 模型 provider。
    Provider,
    /// 工具目录或 MCP proxy。
    ToolCatalog,
    /// 不可短路安全阶段的 typed middleware。
    Middleware,
    /// 版本化提示词包。
    Prompt,
    /// 进程外受限组件。
    External,
}

/// 组件实例的最大生命周期范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentScope {
    /// 跨 Run 共享，由宿主管理。
    Process,
    /// 只属于一个 Run。
    Run,
}

/// 执行与信任边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentTrust {
    /// 随 AgentRS 编译并由宿主信任的内置实现。
    TrustedBuiltin,
    /// 必须由 Core/Sandbox 在进程外限制的组件。
    ExternalSandboxed,
}

/// Core 提供、AgentRS 校验的声明式组件清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentManifest {
    /// 稳定组件 id。
    pub id: ComponentId,
    /// Core 提供的来源标识（内置、包摘要或外部连接 id），不含凭据。
    pub source: String,
    /// 实现版本。
    pub version: String,
    /// AgentRS component API 版本。
    pub api_version: u32,
    /// 组件角色。
    pub kind: ComponentKind,
    /// 必须同时存在的组件。
    #[serde(default)]
    pub requires: Vec<ComponentId>,
    /// 注册的能力名，用于冲突检查。
    #[serde(default)]
    pub provides: Vec<String>,
    /// 配置 JSON Schema。实际 schema 求值由宿主 adapter 完成。
    pub config_schema: serde_json::Value,
    /// 生命周期范围。
    pub scope: ComponentScope,
    /// 信任边界。
    pub trust: ComponentTrust,
    /// 组件请求的能力；组合时只能与当前 CapabilityView 取交集。
    pub requested_capabilities: CapabilityView,
    /// 导出与诊断中必须删除的配置字段。
    #[serde(default)]
    pub redacted_config_fields: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_溢出时不回绕() {
        assert_eq!(Generation(1).checked_next(), Some(Generation(2)));
        assert_eq!(Generation(u64::MAX).checked_next(), None);
    }

    #[test]
    fn 外部组件信任边界可稳定序列化() {
        let manifest = ComponentManifest {
            id: "mcp.github".into(),
            source: "core:mcp/github".into(),
            version: "1.0.0".into(),
            api_version: 1,
            kind: ComponentKind::External,
            requires: vec![],
            provides: vec!["mcp::github::search".into()],
            config_schema: serde_json::json!({"type":"object"}),
            scope: ComponentScope::Process,
            trust: ComponentTrust::ExternalSandboxed,
            requested_capabilities: CapabilityView {
                tools: vec!["mcp::github::search".into()],
                providers: vec![],
                models: vec![],
            },
            redacted_config_fields: vec!["token".into()],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("external_sandboxed"));
        assert_eq!(
            serde_json::from_str::<ComponentManifest>(&json).unwrap(),
            manifest
        );
    }
}
