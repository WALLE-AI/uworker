use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::message::TokenUsage;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubAgentId(String);

impl SubAgentId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SubAgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct SubAgentSpec {
    pub name: String,
    pub agent_type: Option<String>,
    pub prompt: String,
    pub max_turns: Option<usize>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
    pub depth: usize,
    pub resume: Option<SubAgentId>,
    pub persistent: bool,
    pub isolation: SubAgentIsolation,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentIsolation {
    #[default]
    Shared,
    Worktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
    Pending,
    Running,
    Idle,
    Finished,
    Failed,
    Cancelled,
}

impl SubAgentStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Failed | Self::Cancelled)
    }

    pub fn is_error(self) -> bool {
        matches!(self, Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
pub struct SubAgentResult {
    pub id: SubAgentId,
    pub name: String,
    pub text: String,
    pub usage: TokenUsage,
    pub turns: usize,
    pub status: SubAgentStatus,
}

#[derive(Debug, Clone, Default)]
pub struct ForkOverrides {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSource {
    BuiltIn,
    User,
    Project,
}

#[derive(Debug, Clone)]
pub struct AgentDefinition {
    pub name: String,
    pub when_to_use: String,
    pub allowed_tools: Vec<String>,
    pub denied_tools: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub temperature: Option<f32>,
    pub max_turns: Option<usize>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
    pub omit_project_rules: bool,
    pub hidden: bool,
    pub source: AgentSource,
}

#[async_trait]
pub trait Spawner: Send + Sync {
    async fn spawn(&self, spec: SubAgentSpec, overrides: ForkOverrides, cancel: CancellationToken) -> SubAgentResult;
}

#[cfg(test)]
#[path = "subagent_test.rs"]
mod subagent_test;
