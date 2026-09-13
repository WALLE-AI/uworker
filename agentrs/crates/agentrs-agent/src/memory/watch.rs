use std::path::Path;

use agentrs_memory::paths::is_memory_path;
use agentrs_types::message::ContentBlock;

pub(crate) struct MemoryWriteObservation {
    pub(crate) first_write: bool,
}

pub(crate) fn observe_memory_writes(
    tool_calls: &[ContentBlock],
    memory_dir: Option<&Path>,
    already_upgraded: bool,
) -> Option<MemoryWriteObservation> {
    let memory_dir = memory_dir?;
    let wrote_memory = tool_calls.iter().any(|call| {
        let ContentBlock::ToolUse { name, input, .. } = call else {
            return false;
        };
        if name != "Write" && name != "Edit" {
            return false;
        }
        input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| is_memory_path(Path::new(path), memory_dir))
    });
    wrote_memory.then_some(MemoryWriteObservation {
        first_write: !already_upgraded,
    })
}

#[cfg(test)]
#[path = "watch_test.rs"]
mod watch_test;
