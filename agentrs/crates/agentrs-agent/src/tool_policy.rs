use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use agentrs_config::paths::is_within;
use serde_json::Value;

/// Tools whose write target is named by a `file_path` argument.
///
/// A confinement can only judge a call whose destination it can read. Tools
/// that write through some other channel — a shell command, say — are kept out
/// by the allow-list instead, which is why [`ToolPolicy::allow_only_within`]
/// takes the allowed names rather than assuming them.
const WRITE_TOOLS: [&str; 2] = ["Write", "Edit"];

/// Runtime authorization policy for tools registered with an agent engine.
///
/// The policy is enforced both when tool definitions are sent to the model and
/// immediately before a requested tool is executed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ToolPolicy {
    /// Every registered tool is available.
    #[default]
    Unrestricted,
    /// Only tools whose exact names are present in the set are available.
    AllowOnly(BTreeSet<String>),
    /// An allow-list plus a filesystem write confinement: `Write` and `Edit`
    /// calls landing outside `write_root` are denied before execution.
    ///
    /// Used for sub-agents that are trusted to read broadly but must only
    /// persist into one directory.
    AllowOnlyWithinRoot {
        tools: BTreeSet<String>,
        write_root: PathBuf,
    },
}

impl ToolPolicy {
    pub fn allow_only<I, S>(tool_names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::AllowOnly(tool_names.into_iter().map(Into::into).collect())
    }

    /// Allow `tool_names`, but confine writes to `write_root`.
    pub fn allow_only_within<I, S>(tool_names: I, write_root: PathBuf) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::AllowOnlyWithinRoot {
            tools: tool_names.into_iter().map(Into::into).collect(),
            write_root,
        }
    }

    pub fn allows(&self, tool_name: &str) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::AllowOnly(tool_names) => tool_names.contains(tool_name),
            Self::AllowOnlyWithinRoot { tools, .. } => tools.contains(tool_name),
        }
    }

    /// The directory writes are confined to, if any.
    pub fn write_root(&self) -> Option<&Path> {
        match self {
            Self::Unrestricted | Self::AllowOnly(_) => None,
            Self::AllowOnlyWithinRoot { write_root, .. } => Some(write_root),
        }
    }

    /// Whether this specific call escapes the write confinement.
    ///
    /// A call with no readable `file_path` is refused rather than waved
    /// through: under a confinement, an unreadable destination is
    /// indistinguishable from a destination outside it.
    pub fn escapes_write_root(&self, tool_name: &str, input: &Value) -> bool {
        let Some(write_root) = self.write_root() else {
            return false;
        };
        if !WRITE_TOOLS.contains(&tool_name) {
            return false;
        }
        match input.get("file_path").and_then(Value::as_str) {
            Some(path) => !is_within(Path::new(path), write_root),
            None => true,
        }
    }
}

#[cfg(test)]
#[path = "tool_policy_test.rs"]
mod tool_policy_test;
