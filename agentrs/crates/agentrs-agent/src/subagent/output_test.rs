use std::sync::{Arc, Mutex};

use agentrs_protocol::events::{SubAgentEventStatus, TodoSnapshot, Usage};

use super::SubAgentSink;
use crate::output::OutputSink;

#[derive(Default)]
struct RecordingSink {
    text: Mutex<Vec<String>>,
    progress: Mutex<Vec<(String, usize, u64)>>,
}

impl OutputSink for RecordingSink {
    fn emit_text_delta(&self, text: &str, _msg_id: &str) {
        self.text.lock().unwrap().push(text.to_string());
    }
    fn emit_thinking(&self, _text: &str, _msg_id: &str) {}
    fn emit_tool_call(&self, _tool_use_id: &str, _name: &str, _input: &str) {}
    fn emit_tool_result(&self, _tool_use_id: &str, _name: &str, _is_error: bool, _content: &str) {}
    fn emit_stream_start(&self, _msg_id: &str) {}
    fn emit_stream_end(&self, _msg_id: &str, _turns: usize, _input: u64, _output: u64, _created: u64, _read: u64) {}
    fn emit_error(&self, _msg: &str) {}
    fn emit_info(&self, _msg: &str) {}
    fn emit_todo_update(&self, _todos: &[TodoSnapshot]) {}
    fn emit_subagent_progress(&self, id: &str, _status: SubAgentEventStatus, turns: usize, usage: Usage) {
        self.progress
            .lock()
            .unwrap()
            .push((id.to_string(), turns, usage.output_tokens));
    }
}

#[test]
fn child_text_is_filtered_but_usage_progress_is_forwarded() {
    let parent = Arc::new(RecordingSink::default());
    let child = SubAgentSink::new(parent.clone(), "child-1".to_string());
    child.emit_text_delta("secret child draft", "message");
    child.emit_stream_end("message", 2, 3, 5, 0, 0);

    assert!(parent.text.lock().unwrap().is_empty());
    assert_eq!(
        parent.progress.lock().unwrap().as_slice(),
        &[("child-1".to_string(), 2, 5)]
    );
}
