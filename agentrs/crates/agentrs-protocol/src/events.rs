use agentrs_types::message::ImageInputCapability;
use serde::Serialize;
use serde_json::Value;

/// Events emitted by the agent to the client (Agent -> Client)
#[derive(Debug, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ProtocolEvent {
    Ready {
        version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        capabilities: Capabilities,
    },
    StreamStart {
        msg_id: String,
    },
    TextDelta {
        text: String,
        msg_id: String,
    },
    Thinking {
        text: String,
        msg_id: String,
    },
    ToolRequest {
        msg_id: String,
        call_id: String,
        tool: ToolInfo,
    },
    ToolRunning {
        msg_id: String,
        call_id: String,
        tool_name: String,
    },
    ToolResult {
        msg_id: String,
        call_id: String,
        tool_name: String,
        status: ToolStatus,
        output: String,
        output_type: OutputType,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    ToolCancelled {
        msg_id: String,
        call_id: String,
        reason: String,
    },
    StreamEnd {
        msg_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        msg_id: Option<String>,
        error: ErrorInfo,
    },
    Info {
        msg_id: String,
        message: String,
    },
    ConfigChanged {
        capabilities: Capabilities,
    },
    McpReady {
        name: String,
        tools: Vec<String>,
    },
    /// The agent's task checklist changed. Carries the whole list, matching the
    /// tool's replace-only semantics: a host renders this snapshot and discards
    /// whatever it held before.
    TodoUpdated {
        todos: Vec<TodoSnapshot>,
    },
    SubAgentStarted {
        id: String,
        name: String,
        parent_msg_id: String,
        depth: usize,
    },
    SubAgentProgress {
        id: String,
        status: SubAgentEventStatus,
        turns: usize,
        usage: Usage,
    },
    SubAgentFinished {
        id: String,
        status: SubAgentEventStatus,
        usage: Usage,
        turns: usize,
    },
    Pong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentEventStatus {
    Pending,
    Running,
    Idle,
    Finished,
    Failed,
    Cancelled,
}

/// One checklist entry as it crosses the protocol boundary.
///
/// Deliberately a separate type from the tool's own item: this is the host
/// contract, and it must not move whenever the tool's internals do. `status` is
/// a plain string for the same reason: a host that meets an unfamiliar value
/// should be able to display it rather than fail to parse the frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TodoSnapshot {
    pub content: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    pub tool_approval: bool,
    pub image_input: ImageInputCapability,
    pub thinking: bool,
    pub effort: bool,
    pub effort_levels: Vec<String>,
    pub modes: Vec<String>,
    pub current_mode: String,
    pub mcp: bool,
}

#[derive(Debug, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub category: ToolCategory,
    pub args: Value,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Info,
    Edit,
    Exec,
    Mcp,
    /// Tools that reach the public network (WebFetch, WebSearch).
    Network,
}

impl std::fmt::Display for ToolCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Edit => write!(f, "edit"),
            Self::Exec => write!(f, "exec"),
            Self::Mcp => write!(f, "mcp"),
            Self::Network => write!(f, "network"),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Success,
    Error,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputType {
    Text,
    Diff,
    Image,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[cfg(test)]
#[path = "events_test.rs"]
mod events_test;
