//! MCP 工具目录的受控映射边界。
//!
//! 本模块不启动 MCP 进程、不持有凭据、也不执行调用。Core/Sandbox 提供目录并执行；
//! AgentRS 只把远端名称映射为不可冲突的本地工具名，调用仍走 Policy -> SandboxGrant。

/// MCP 的线格式（JSON-RPC 报文与 MCP 载荷类型）。
pub mod wire;

pub use wire::{
    ClientCapabilities, ClientInfo, InitializeParams, InitializeResult, JsonRpcError,
    JsonRpcRequest, JsonRpcResponse, McpContent, McpResource, McpToolDef, McpToolResult,
    ResourceContent, ResourcesListResult, ResourcesReadResult, ToolsListResult, PROTOCOL_VERSION,
};

use std::collections::{BTreeMap, BTreeSet};

use agentrs_contracts::authority::CapabilityView;
use agentrs_contracts::component::{
    ComponentKind, ComponentManifest, ComponentScope, ComponentTrust, Generation,
};
use agentrs_contracts::ids::ComponentId;
use agentrs_types::ToolDef;
use serde::{Deserialize, Serialize};

use crate::{Registration, RegistryError, ToolRegistry};

/// MCP `tools/list` 中 AgentRS 需要的稳定子集。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTool {
    /// MCP server 内的工具名。
    pub name: String,
    /// 供模型发现的描述。
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema 输入定义。
    pub input_schema: serde_json::Value,
}

/// 交给 Sandbox/Core MCP adapter 的反向路由结果。
#[derive(Debug, Clone, PartialEq)]
pub struct McpDispatch {
    /// 组件命名空间。
    pub component: String,
    /// 调用绑定的目录代际。
    pub generation: Generation,
    /// MCP server 内的原始名称。
    pub remote_name: String,
    /// 未改写的结构化参数。
    pub arguments: serde_json::Value,
}

/// MCP 目录映射失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    /// 组件或工具名称无法安全组成稳定本地名。
    #[error("invalid MCP name: {0}")]
    InvalidName(String),
    /// 同一 MCP 目录重复声明名称。
    #[error("duplicate MCP tool: {0}")]
    Duplicate(String),
    /// 输入 schema 不是 JSON object。
    #[error("MCP input schema must be an object: {0}")]
    InvalidSchema(String),
    /// 本地调用名不属于该目录。
    #[error("unknown MCP tool: {0}")]
    UnknownTool(String),
    /// 注册到全局 ToolRegistry 失败。
    #[error(transparent)]
    Registry(#[from] RegistryError),
}

/// 一个已验证、带命名空间的 MCP 工具目录。
#[derive(Debug, Clone, PartialEq)]
pub struct McpCatalog {
    component: String,
    component_id: ComponentId,
    generation: Generation,
    registrations: Vec<Registration>,
    routes: BTreeMap<String, String>,
}

impl McpCatalog {
    /// 验证并映射一次 `tools/list` 响应。MCP 工具默认 deferred，避免目录撑爆上下文。
    pub fn new(component: impl Into<String>, tools: Vec<McpTool>) -> Result<Self, McpError> {
        Self::for_generation(component, Generation(1), tools)
    }

    /// 验证并映射指定 generation 的 `tools/list` 响应。
    pub fn for_generation(
        component: impl Into<String>,
        generation: Generation,
        tools: Vec<McpTool>,
    ) -> Result<Self, McpError> {
        let component = component.into();
        validate_name(&component)?;
        if generation.0 == 0 {
            return Err(McpError::InvalidName("generation:0".into()));
        }
        let component_id = ComponentId::new(format!("mcp.{component}"));
        let mut seen = BTreeSet::new();
        let mut registrations = Vec::with_capacity(tools.len());
        let mut routes = BTreeMap::new();
        for tool in tools {
            validate_name(&tool.name)?;
            if !seen.insert(tool.name.clone()) {
                return Err(McpError::Duplicate(tool.name));
            }
            if !tool.input_schema.is_object() {
                return Err(McpError::InvalidSchema(tool.name));
            }
            let local = format!("mcp::{component}::{}", tool.name);
            let description = tool
                .description
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| format!("MCP tool {}", tool.name));
            let keywords = tool
                .name
                .split(['_', '-'])
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            registrations.push(Registration::deferred(
                // 副作用分类由 Core 的 component manifest 决定；默认按 mutating 收紧。
                ToolDef::mutating(&local, &description, tool.input_schema),
                keywords,
            ));
            routes.insert(local, tool.name);
        }
        Ok(Self {
            component,
            component_id,
            generation,
            registrations,
            routes,
        })
    }

    /// 注册到全局目录；实际可见性仍需与 Run authority/capability 取交集。
    pub fn register_into(&self, registry: &mut ToolRegistry) -> Result<(), McpError> {
        for registration in &self.registrations {
            registry.register_owned(self.component_id.clone(), self.generation, registration.clone())?;
        }
        Ok(())
    }

    /// 把模型侧本地名反向映射给宿主 adapter，不执行网络或进程调用。
    pub fn dispatch(&self, local_name: &str, arguments: serde_json::Value) -> Result<McpDispatch, McpError> {
        let remote_name = self
            .routes
            .get(local_name)
            .ok_or_else(|| McpError::UnknownTool(local_name.to_owned()))?;
        Ok(McpDispatch {
            component: self.component.clone(),
            generation: self.generation,
            remote_name: remote_name.clone(),
            arguments,
        })
    }

    /// 映射后的本地工具名。
    pub fn local_names(&self) -> Vec<&str> {
        self.routes.keys().map(String::as_str).collect()
    }

    /// 生成可交给 Composition Kernel 的外部组件清单。
    pub fn manifest(&self) -> ComponentManifest {
        let names: Vec<String> = self.routes.keys().cloned().collect();
        ComponentManifest {
            id: self.component_id.clone(),
            source: format!("core:mcp/{}", self.component),
            version: format!("generation-{}", self.generation.0),
            api_version: 1,
            kind: ComponentKind::External,
            requires: vec![],
            provides: names.clone(),
            config_schema: serde_json::json!({"type":"object"}),
            scope: ComponentScope::Process,
            trust: ComponentTrust::ExternalSandboxed,
            requested_capabilities: CapabilityView {
                tools: names,
                providers: vec![],
                models: vec![],
            },
            redacted_config_fields: vec!["oauth_token".into(), "headers".into()],
        }
    }
}

fn validate_name(value: &str) -> Result<(), McpError> {
    let valid = !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(McpError::InvalidName(value.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> McpTool {
        McpTool {
            name: name.into(),
            description: Some("Search issues".into()),
            input_schema: serde_json::json!({"type":"object"}),
        }
    }

    #[test]
    fn mcp_目录带命名空间且默认延迟暴露() {
        let catalog = McpCatalog::new("github", vec![tool("search_issues")]).unwrap();
        assert_eq!(catalog.local_names(), ["mcp::github::search_issues"]);
        let mut registry = ToolRegistry::new();
        catalog.register_into(&mut registry).unwrap();
        let allowed = vec!["mcp::github::search_issues".into()];
        let scope = registry.scope(&allowed, &allowed);
        assert_eq!(scope.catalog()[0].name, "ToolSearch");
        assert_eq!(scope.search("search issues", 1)[0].name, allowed[0]);
    }

    #[test]
    fn dispatch_只反向映射不改变参数() {
        let catalog = McpCatalog::new("github", vec![tool("search_issues")]).unwrap();
        let args = serde_json::json!({"query":"bug"});
        let dispatch = catalog
            .dispatch("mcp::github::search_issues", args.clone())
            .unwrap();
        assert_eq!(dispatch.component, "github");
        assert_eq!(dispatch.generation, Generation(1));
        assert_eq!(dispatch.remote_name, "search_issues");
        assert_eq!(dispatch.arguments, args);
    }

    #[test]
    fn 撤回只移除自己的旧代注册() {
        let old = McpCatalog::for_generation("github", Generation(7), vec![tool("search")]).unwrap();
        let mut registry = ToolRegistry::new();
        old.register_into(&mut registry).unwrap();
        registry
            .register(Registration::eager(ToolDef::read_only(
                "Read",
                "read",
                serde_json::json!({}),
            )))
            .unwrap();
        assert!(registry
            .withdraw(&ComponentId::new("mcp.github"), Generation(6))
            .is_empty());
        assert_eq!(
            registry.withdraw(&ComponentId::new("mcp.github"), Generation(7)),
            ["mcp::github::search"]
        );
        let allowed = vec!["Read".into()];
        assert_eq!(registry.scope(&allowed, &allowed).catalog()[0].name, "Read");
    }

    #[test]
    fn manifest_声明外部沙箱边界和敏感配置字段() {
        let catalog = McpCatalog::for_generation("github", Generation(3), vec![tool("search")]).unwrap();
        let manifest = catalog.manifest();
        assert_eq!(manifest.id, ComponentId::new("mcp.github"));
        assert_eq!(manifest.trust, ComponentTrust::ExternalSandboxed);
        assert!(manifest.redacted_config_fields.contains(&"oauth_token".into()));
    }

    #[test]
    fn 名称_schema_重复与越权路由均拒绝() {
        assert!(McpCatalog::new("bad:name", vec![]).is_err());
        assert!(McpCatalog::new("x", vec![tool("a"), tool("a")]).is_err());
        let mut bad = tool("a");
        bad.input_schema = serde_json::json!([]);
        assert!(McpCatalog::new("x", vec![bad]).is_err());
        let catalog = McpCatalog::new("x", vec![tool("a")]).unwrap();
        assert!(catalog
            .dispatch("mcp::x::unknown", serde_json::json!({}))
            .is_err());
    }
}
