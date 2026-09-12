use std::sync::Arc;

use agentrs_protocol::events::{SubAgentEventStatus, TodoSnapshot, Usage};

use crate::output::OutputSink;

pub(crate) struct SubAgentSink {
    parent: Arc<dyn OutputSink>,
    id: String,
}

impl SubAgentSink {
    pub(crate) fn new(parent: Arc<dyn OutputSink>, id: String) -> Self {
        Self { parent, id }
    }
}

impl OutputSink for SubAgentSink {
    fn emit_text_delta(&self, _text: &str, _msg_id: &str) {}
    fn emit_thinking(&self, _text: &str, _msg_id: &str) {}
    fn emit_tool_call(&self, _tool_use_id: &str, _name: &str, _input: &str) {}
    fn emit_tool_result(&self, _tool_use_id: &str, _name: &str, _is_error: bool, _content: &str) {}
    fn emit_stream_start(&self, _msg_id: &str) {}
    fn emit_stream_end(
        &self,
        _msg_id: &str,
        turns: usize,
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
    ) {
        self.parent.emit_subagent_progress(
            &self.id,
            SubAgentEventStatus::Running,
            turns,
            Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens: (cache_read_tokens > 0).then_some(cache_read_tokens),
                cache_write_tokens: (cache_creation_tokens > 0).then_some(cache_creation_tokens),
            },
        );
    }
    fn emit_error(&self, _msg: &str) {}
    fn emit_info(&self, _msg: &str) {}
    fn emit_todo_update(&self, _todos: &[TodoSnapshot]) {}
}

#[cfg(test)]
#[path = "output_test.rs"]
mod output_test;
