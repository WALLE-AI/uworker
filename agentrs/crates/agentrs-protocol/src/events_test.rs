use super::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_ready_event_serialization() {
        let event = ProtocolEvent::Ready {
            version: "0.1.0".to_string(),
            session_id: Some("abc123".to_string()),
            capabilities: Capabilities {
                tool_approval: true,
                image_input: ImageInputCapability::Supported,
                thinking: true,
                effort: false,
                effort_levels: vec![],
                modes: vec!["default".into(), "auto_edit".into(), "yolo".into()],
                current_mode: "default".into(),
                mcp: false,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "ready");
        assert_eq!(json["version"], "0.1.0");
        assert_eq!(json["session_id"], "abc123");
        assert_eq!(json["capabilities"]["tool_approval"], true);

        // session_id omitted when None
        let event_no_sid = ProtocolEvent::Ready {
            version: "0.1.0".to_string(),
            session_id: None,
            capabilities: Capabilities {
                tool_approval: true,
                image_input: ImageInputCapability::Unknown,
                thinking: true,
                effort: false,
                effort_levels: vec![],
                modes: vec!["default".into(), "auto_edit".into(), "yolo".into()],
                current_mode: "default".into(),
                mcp: false,
            },
        };
        let json2 = serde_json::to_value(&event_no_sid).unwrap();
        assert!(json2.get("session_id").is_none());
    }

    #[test]
    fn test_text_delta_event_serialization() {
        let event = ProtocolEvent::TextDelta {
            text: "hello".to_string(),
            msg_id: "m1".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "text_delta");
        assert_eq!(json["text"], "hello");
        assert_eq!(json["msg_id"], "m1");
    }

    #[test]
    fn test_tool_request_event_serialization() {
        let event = ProtocolEvent::ToolRequest {
            msg_id: "m1".to_string(),
            call_id: "c1".to_string(),
            tool: ToolInfo {
                name: "ExecCommand".to_string(),
                category: ToolCategory::Exec,
                args: json!({"cmd": "ls"}),
                description: "Execute: ls".to_string(),
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_request");
        assert_eq!(json["tool"]["category"], "exec");
    }

    #[test]
    fn test_tool_result_event_serialization() {
        let event = ProtocolEvent::ToolResult {
            msg_id: "m1".to_string(),
            call_id: "c1".to_string(),
            tool_name: "Read".to_string(),
            status: ToolStatus::Success,
            output: "file content".to_string(),
            output_type: OutputType::Text,
            metadata: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["status"], "success");
        assert!(json.get("metadata").is_none());
    }

    #[test]
    fn test_error_event_serialization() {
        let event = ProtocolEvent::Error {
            msg_id: None,
            error: ErrorInfo {
                code: "rate_limit".to_string(),
                message: "Too many requests".to_string(),
                retryable: true,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "error");
        assert!(json.get("msg_id").is_none());
        assert_eq!(json["error"]["retryable"], true);
    }

    #[test]
    fn test_stream_end_with_usage() {
        let event = ProtocolEvent::StreamEnd {
            msg_id: "m1".to_string(),
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: Some(20),
                cache_write_tokens: None,
            }),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "stream_end");
        assert_eq!(json["usage"]["input_tokens"], 100);
        assert!(json["usage"].get("cache_write_tokens").is_none());
    }

    // --- TC-0.1-01 / TC-0.1-02: category wire names ---
    #[test]
    fn test_tool_category_display() {
        assert_eq!(ToolCategory::Info.to_string(), "info");
        assert_eq!(ToolCategory::Edit.to_string(), "edit");
        assert_eq!(ToolCategory::Exec.to_string(), "exec");
        assert_eq!(ToolCategory::Mcp.to_string(), "mcp");
        assert_eq!(ToolCategory::Network.to_string(), "network");
        assert_eq!(ToolCategory::Team.to_string(), "team");
    }

    #[test]
    fn test_tool_category_serializes_as_snake_case() {
        for (category, expected) in [
            (ToolCategory::Info, "info"),
            (ToolCategory::Edit, "edit"),
            (ToolCategory::Exec, "exec"),
            (ToolCategory::Mcp, "mcp"),
            (ToolCategory::Network, "network"),
            (ToolCategory::Team, "team"),
        ] {
            let json = serde_json::to_value(category).unwrap();
            assert_eq!(json, serde_json::Value::String(expected.to_string()));
            assert_eq!(
                category.to_string(),
                expected,
                "Display and serde must agree so protocol consumers see one name"
            );
        }
    }

    #[test]
    fn test_ready_event_with_expanded_capabilities() {
        let event = ProtocolEvent::Ready {
            version: "0.2.0".to_string(),
            session_id: Some("abc".to_string()),
            capabilities: Capabilities {
                tool_approval: true,
                image_input: ImageInputCapability::Supported,
                thinking: true,
                effort: true,
                effort_levels: vec!["low".into(), "medium".into(), "high".into()],
                modes: vec!["default".into(), "auto_edit".into(), "yolo".into()],
                current_mode: "default".into(),
                mcp: false,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["capabilities"]["thinking"], true);
        assert_eq!(json["capabilities"]["image_input"], "supported");
        assert_eq!(json["capabilities"]["effort"], true);
        assert_eq!(json["capabilities"]["effort_levels"][0], "low");
        assert_eq!(json["capabilities"]["modes"][2], "yolo");
    }

    #[test]
    fn test_mcp_ready_event_serialization() {
        let event = ProtocolEvent::McpReady {
            name: "team-tools".to_string(),
            tools: vec!["team_send_message".into(), "team_task_create".into()],
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "mcp_ready");
        assert_eq!(json["name"], "team-tools");
        assert_eq!(json["tools"][0], "team_send_message");
        assert_eq!(json["tools"][1], "team_task_create");
        assert_eq!(json["tools"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_pong_event_serialization() {
        let event = ProtocolEvent::Pong;
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "pong");
        assert_eq!(json.as_object().unwrap().len(), 1);
    }

    #[test]
    fn todo_updated_event_serializes_the_whole_list() {
        let event = ProtocolEvent::TodoUpdated {
            todos: vec![
                TodoSnapshot {
                    content: "Wire the store".into(),
                    status: "in_progress".into(),
                    active_form: Some("Wiring the store".into()),
                },
                TodoSnapshot {
                    content: "Add tests".into(),
                    status: "pending".into(),
                    active_form: None,
                },
            ],
        };
        let json = serde_json::to_value(&event).unwrap();

        assert_eq!(json["type"], "todo_updated");
        assert_eq!(json["todos"][0]["content"], "Wire the store");
        assert_eq!(json["todos"][0]["status"], "in_progress");
        assert_eq!(json["todos"][0]["active_form"], "Wiring the store");
        // An absent activeForm is omitted rather than sent as null, so hosts can
        // treat presence as meaning.
        assert!(json["todos"][1].get("active_form").is_none());
    }

    #[test]
    fn an_empty_todo_update_still_carries_the_key() {
        let event = ProtocolEvent::TodoUpdated { todos: Vec::new() };
        let json = serde_json::to_value(&event).unwrap();

        assert_eq!(json["type"], "todo_updated");
        assert_eq!(
            json["todos"],
            json!([]),
            "clearing the list must be distinguishable from no update at all"
        );
    }

    #[test]
    fn test_config_changed_event_serialization() {
        let event = ProtocolEvent::ConfigChanged {
            capabilities: Capabilities {
                tool_approval: true,
                image_input: ImageInputCapability::Unsupported,
                thinking: false,
                effort: true,
                effort_levels: vec!["low".into(), "medium".into(), "high".into()],
                modes: vec!["default".into(), "auto_edit".into(), "yolo".into()],
                current_mode: "default".into(),
                mcp: true,
            },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "config_changed");
        assert_eq!(json["capabilities"]["thinking"], false);
        assert_eq!(json["capabilities"]["effort"], true);
        assert_eq!(json["capabilities"]["image_input"], "unsupported");
    }

    #[test]
    fn subagent_lifecycle_events_have_a_stable_non_sensitive_shape() {
        let started = serde_json::to_value(ProtocolEvent::SubAgentStarted {
            id: "child-1".into(),
            name: "research".into(),
            parent_msg_id: "msg-1".into(),
            depth: 2,
        })
        .unwrap();
        assert_eq!(started["type"], "sub_agent_started");
        assert_eq!(started["depth"], 2);

        let progress = serde_json::to_value(ProtocolEvent::SubAgentProgress {
            id: "child-1".into(),
            status: SubAgentEventStatus::Running,
            turns: 3,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 4,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        })
        .unwrap();
        assert_eq!(progress["status"], "running");

        let finished = serde_json::to_value(ProtocolEvent::SubAgentFinished {
            id: "child-1".into(),
            status: SubAgentEventStatus::Finished,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 4,
                cache_read_tokens: Some(2),
                cache_write_tokens: None,
            },
            turns: 3,
        })
        .unwrap();
        assert_eq!(finished["type"], "sub_agent_finished");
        for event in [&started, &progress, &finished] {
            assert!(event.get("prompt").is_none());
            assert!(event.get("result").is_none());
        }
    }

    #[test]
    fn team_lifecycle_events_have_stable_non_sensitive_shapes() {
        let joined = serde_json::to_value(ProtocolEvent::TeamEvent {
            event: TeamEvent::MemberJoined {
                team_name: "backend".into(),
                member_name: "alice".into(),
                agent_id: "alice@backend".into(),
            },
        })
        .unwrap();
        assert_eq!(joined["type"], "team_event");
        assert_eq!(joined["event"]["kind"], "member_joined");
        assert_eq!(joined["event"]["agent_id"], "alice@backend");

        let exited = serde_json::to_value(ProtocolEvent::TeamEvent {
            event: TeamEvent::MemberExited {
                team_name: "backend".into(),
                member_name: "alice".into(),
                agent_id: "alice@backend".into(),
            },
        })
        .unwrap();
        assert_eq!(exited["event"]["kind"], "member_exited");

        let sent = serde_json::to_value(ProtocolEvent::TeamEvent {
            event: TeamEvent::MessageSent {
                team_name: "backend".into(),
                from: "team-lead".into(),
                to: "alice".into(),
            },
        })
        .unwrap();
        assert_eq!(sent["event"]["kind"], "message_sent");
        assert_eq!(sent["event"]["from"], "team-lead");
        assert_eq!(sent["event"]["to"], "alice");
        assert!(sent["event"].get("message").is_none());
        assert!(sent["event"].get("content").is_none());
    }
}
