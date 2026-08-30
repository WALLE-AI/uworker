//! Durable 事实到 OpenTelemetry 日志信号的纯投影边界。
//!
//! AgentRS 不读取墙钟、不访问 collector，也不把用户正文放进 telemetry。
//! 宿主可以把这些稳定记录交给任意 OTel SDK/exporter，并补 observed timestamp。

use agentrs_contracts::event::{Durability, RunEventEnvelope, Visibility};
use serde::{Deserialize, Serialize};

use crate::projection::payload_kind;

/// 一个低基数、无用户正文的 OTel attribute。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtelAttribute {
    /// 语义化键。
    pub key: String,
    /// 字符串值。
    pub value: String,
}

/// 可交给 OpenTelemetry Logs SDK 的 signal-neutral 记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtelLogRecord {
    /// 稳定事件类型，作为 log body。
    pub body: String,
    /// OTel severity text。
    pub severity_text: String,
    /// 稳定属性；不包含 prompt、模型输出、工具参数或绝对路径。
    pub attributes: Vec<OtelAttribute>,
}

/// 只投影 durable 事实。live delta 可丢失，不能进入可 replay telemetry。
pub fn project_otel_logs(events: &[RunEventEnvelope]) -> Vec<OtelLogRecord> {
    events
        .iter()
        .filter(|event| event.is_durable())
        .map(|event| {
            let durability = match event.durability {
                Durability::DurableFact => "durable_fact",
                Durability::DurableContentRef => "durable_content_ref",
                Durability::LiveStream => unreachable!("live events were filtered"),
            };
            let visibility = match event.visibility {
                Visibility::User => "user",
                Visibility::Internal => "internal",
                Visibility::Diagnostic => "diagnostic",
            };
            let mut attributes = vec![
                attribute("agentrs.run.id", event.run_id.as_str()),
                attribute("agentrs.event.id", event.event_id.as_str()),
                attribute("agentrs.run.epoch", event.epoch.0.to_string()),
                attribute("agentrs.event.durability", durability),
                attribute("agentrs.event.visibility", visibility),
            ];
            if let Some(seq) = event.seq {
                attributes.push(attribute("agentrs.event.sequence", seq.0.to_string()));
            }
            if let agentrs_contracts::event::EventPayload::ModelRequestManifestRecorded { manifest } =
                &event.payload
            {
                let generations = manifest
                    .operation_view
                    .component_generations
                    .iter()
                    .map(|(component, generation)| format!("{}={}", component.as_str(), generation.0))
                    .collect::<Vec<_>>()
                    .join(",");
                if !generations.is_empty() {
                    attributes.push(attribute("agentrs.component.generations", generations));
                }
            }
            OtelLogRecord {
                body: payload_kind(&event.payload).to_owned(),
                severity_text: severity(payload_kind(&event.payload)).to_owned(),
                attributes,
            }
        })
        .collect()
}

fn attribute(key: impl Into<String>, value: impl Into<String>) -> OtelAttribute {
    OtelAttribute {
        key: key.into(),
        value: value.into(),
    }
}

fn severity(kind: &str) -> &'static str {
    if kind.contains("Failed") || kind.contains("Rejected") || kind.contains("Denied") {
        "ERROR"
    } else if kind.contains("Suspended") || kind.contains("Unknown") {
        "WARN"
    } else {
        "INFO"
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use agentrs_contracts::authority::{CapabilityViewDigest, PermissionMode};
    use agentrs_contracts::component::Generation;
    use agentrs_contracts::event::{Causality, EventPayload};
    use agentrs_contracts::ids::{Digest, EventRange, EventSequence, LiveSequence, RunEpoch, Timestamp};
    use agentrs_contracts::manifest::{ModelRequestManifest, OperationView, TokenAccounting};

    use super::*;
    use crate::projection::{page, Cursor, EventFilter};

    fn event(durability: Durability, payload: EventPayload) -> RunEventEnvelope {
        RunEventEnvelope {
            run_id: "r1".into(),
            epoch: RunEpoch(2),
            event_id: "e1".into(),
            seq: (durability != Durability::LiveStream).then_some(EventSequence(7)),
            live_seq: (durability == Durability::LiveStream).then_some(LiveSequence(1)),
            at: Timestamp(0),
            durability,
            visibility: Visibility::User,
            causality: Causality::default(),
            surface: None,
            payload,
        }
    }

    fn manifest_event() -> RunEventEnvelope {
        event(
            Durability::DurableFact,
            EventPayload::ModelRequestManifestRecorded {
                manifest: Box::new(ModelRequestManifest {
                    request_id: "q1".into(),
                    operation_view: OperationView {
                        authority_id: "a1".into(),
                        permission_mode: PermissionMode::Default,
                        capability_digest: CapabilityViewDigest(Digest::from_hex("cap")),
                        component_generations: BTreeMap::from([("provider.main".into(), Generation(9))]),
                    },
                    source_event_range: EventRange {
                        start: EventSequence(1),
                        end: EventSequence(1),
                    },
                    system_sections: vec![],
                    memory_fragments: vec![],
                    skill_fragments: vec![],
                    compaction_refs: vec![],
                    resolved_content_refs: vec![],
                    tool_catalog_digest: Digest::from_hex("tools"),
                    cache_prefix_digest: Digest::from_hex("cache"),
                    cache_breakpoints: vec![],
                    legalization_ops: vec![],
                    unresolved: vec![],
                    token_accounting: TokenAccounting::default(),
                }),
            },
        )
    }

    #[test]
    fn 只投影_durable_且相同事实结果确定() {
        let durable = event(Durability::DurableFact, EventPayload::RunStarted);
        let live = event(
            Durability::LiveStream,
            EventPayload::TextDelta { text: "x".into() },
        );
        let first = project_otel_logs(&[durable.clone(), live.clone()]);
        let second = project_otel_logs(&[durable, live]);
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].body, "RunStarted");
    }

    #[test]
    fn telemetry_不携带正文路径或工具参数() {
        let source = event(
            Durability::DurableFact,
            EventPayload::SurfaceMessageRecorded {
                message: serde_json::json!({"content":"C:\\\\private\\secret.txt", "api_key":"secret"}),
            },
        );
        let encoded = serde_json::to_string(&project_otel_logs(&[source])).unwrap();
        assert!(!encoded.contains("private"));
        assert!(!encoded.contains("api_key"));
        assert!(encoded.contains("SurfaceMessageRecorded"));
    }

    #[test]
    fn generation_进入_otel_并可被_trajectory_精确过滤() {
        let source = manifest_event();
        let records = project_otel_logs(std::slice::from_ref(&source));
        assert!(records[0]
            .attributes
            .iter()
            .any(|attribute| attribute.key == "agentrs.component.generations"
                && attribute.value == "provider.main=9"));

        let events = [source];
        let matched = page(
            &events,
            Cursor::start(),
            10,
            &EventFilter {
                component_id: Some("provider.main".into()),
                generation: Some(Generation(9)),
                ..Default::default()
            },
        );
        assert_eq!(matched.items.len(), 1);
        let missed = page(
            &events,
            Cursor::start(),
            10,
            &EventFilter {
                component_id: Some("provider.main".into()),
                generation: Some(Generation(8)),
                ..Default::default()
            },
        );
        assert!(missed.items.is_empty());
    }
}
