use std::path::Path;

use agentrs_memory::paths::is_memory_path;
use serde_json::Value;

/// Tools whose target file is named by a `file_path` argument. Only these can
/// be resolved against the memory directory, so only these are eligible for
/// the carve-out.
const FILE_PATH_TOOLS: [&str; 3] = ["Read", "Write", "Edit"];

/// Whether a tool call reads or writes inside the memory directory and may
/// therefore skip approval.
///
/// The default memory directory lives under the platform config directory,
/// which the permission layer otherwise treats as sensitive. Without this
/// carve-out every memory save would interrupt the user for a decision they
/// already made by enabling memory at all.
///
/// `memory_dir` is `None` whenever the directory came from an explicit
/// override: that is a caller-chosen location that merely happens to hold
/// memory files, so it keeps the normal permission flow.
pub(crate) fn is_memory_carve_out(tool_name: &str, input: &Value, memory_dir: Option<&Path>) -> bool {
    let Some(memory_dir) = memory_dir else {
        return false;
    };
    if !FILE_PATH_TOOLS.contains(&tool_name) {
        return false;
    }
    let Some(path) = input.get("file_path").and_then(Value::as_str) else {
        return false;
    };
    let allowed = is_memory_path(Path::new(path), memory_dir);
    if allowed {
        // Shape only — the targeted path and the file body stay out of logs.
        tracing::debug!(target: "agentrs_memory", tool = tool_name, "memory directory call auto-approved");
    }
    allowed
}

#[cfg(test)]
#[path = "permission_test.rs"]
mod permission_test;
