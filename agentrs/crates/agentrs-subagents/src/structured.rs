//! 功能型子 Agent 的严格结构化输出。

use std::collections::BTreeSet;

use agentrs_contracts::{content::ContentRef, spec::SubagentSummary};
use serde::{Deserialize, Serialize};

use crate::{SubagentError, SubagentOutput};

/// 使用共同结论 schema 的功能型子 Agent。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionalKind {
    /// 只读调查；必须返回证据。
    Explore,
    /// 执行规划；必须返回后续步骤。
    Plan,
    /// 上下文交接摘要。
    ContextSummary,
}

/// Explore、Plan 与 ContextSummary 的输出合约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredConclusion {
    /// 可直接回灌给父运行的结论。
    pub conclusion: String,
    /// 支撑结论的短证据；持久化前应写入 ContentStore。
    #[serde(default)]
    pub evidence: Vec<String>,
    /// 已识别的风险。
    #[serde(default)]
    pub risks: Vec<String>,
    /// 置信度，0 到 100。
    pub confidence: u8,
    /// 建议的可执行下一步。
    #[serde(default)]
    pub next_steps: Vec<String>,
}

/// ToolSearch 的输出合约。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSearchDecision {
    /// 从候选集合中选出的工具名。
    pub selected: Vec<String>,
    /// 选择理由。
    pub rationale: String,
}

/// 解析并按功能种类校验结构化结论。
pub fn parse_structured(
    kind: FunctionalKind,
    output: &SubagentOutput,
) -> Result<StructuredConclusion, SubagentError> {
    let parsed: StructuredConclusion =
        serde_json::from_str(output.conclusion.trim()).map_err(|_| SubagentError::SchemaViolation {
            missing: vec!["valid_json".into()],
        })?;
    let mut invalid = Vec::new();
    if parsed.conclusion.trim().is_empty() {
        invalid.push("conclusion".into());
    }
    if parsed.confidence > 100 {
        invalid.push("confidence_0_to_100".into());
    }
    if kind == FunctionalKind::Explore && parsed.evidence.is_empty() {
        invalid.push("evidence".into());
    }
    if kind == FunctionalKind::Plan && parsed.next_steps.is_empty() {
        invalid.push("next_steps".into());
    }
    if invalid.is_empty() {
        Ok(parsed)
    } else {
        Err(SubagentError::SchemaViolation { missing: invalid })
    }
}

/// 解析 ToolSearch，并强制选择结果是候选集合的有界子集。
pub fn parse_tool_search(
    output: &SubagentOutput,
    candidates: &[String],
    limit: usize,
) -> Result<ToolSearchDecision, SubagentError> {
    let parsed: ToolSearchDecision =
        serde_json::from_str(output.conclusion.trim()).map_err(|_| SubagentError::SchemaViolation {
            missing: vec!["valid_json".into()],
        })?;
    let allowed: BTreeSet<&str> = candidates.iter().map(String::as_str).collect();
    let selected: BTreeSet<&str> = parsed.selected.iter().map(String::as_str).collect();
    let valid = parsed.selected.len() <= limit
        && selected.len() == parsed.selected.len()
        && selected.iter().all(|name| allowed.contains(name))
        && !parsed.rationale.trim().is_empty();
    if valid {
        Ok(parsed)
    } else {
        Err(SubagentError::SchemaViolation {
            missing: vec!["candidate_subset_within_limit".into()],
        })
    }
}

/// 把已校验结论和已入 ContentStore 的证据引用归集给父 Run。
pub fn to_summary(
    task: impl Into<String>,
    result: StructuredConclusion,
    evidence: Vec<ContentRef>,
    output: &SubagentOutput,
) -> SubagentSummary {
    SubagentSummary {
        task: task.into(),
        conclusion: result.conclusion,
        evidence,
        risks: result.risks,
        confidence: result.confidence,
        next_steps: result.next_steps,
        usage: agentrs_contracts::spec::TokenUsage {
            input_tokens: output.usage.input_tokens,
            output_tokens: output.usage.output_tokens,
        },
    }
}

#[cfg(test)]
mod tests {
    use agentrs_types::TokenUsage;

    use super::*;

    fn output(json: &str) -> SubagentOutput {
        SubagentOutput {
            conclusion: json.into(),
            usage: TokenUsage {
                input_tokens: 17,
                output_tokens: 9,
                cache_creation_tokens: 3,
                cache_read_tokens: 5,
            },
        }
    }

    #[test]
    fn explore_必须带证据且拒绝额外字段() {
        let no_evidence =
            output(r#"{"conclusion":"结论","evidence":[],"risks":[],"confidence":80,"next_steps":[]}"#);
        assert!(parse_structured(FunctionalKind::Explore, &no_evidence).is_err());
        let extra = output(
            r#"{"conclusion":"结论","evidence":["e"],"risks":[],"confidence":80,"next_steps":[],"draft":"泄漏"}"#,
        );
        assert!(parse_structured(FunctionalKind::Explore, &extra).is_err());
    }

    #[test]
    fn plan_必须有步骤且置信度有界() {
        let missing_steps =
            output(r#"{"conclusion":"计划","evidence":[],"risks":[],"confidence":80,"next_steps":[]}"#);
        assert!(parse_structured(FunctionalKind::Plan, &missing_steps).is_err());
        let bad_confidence =
            output(r#"{"conclusion":"计划","evidence":[],"risks":[],"confidence":101,"next_steps":["做"]}"#);
        assert!(parse_structured(FunctionalKind::Plan, &bad_confidence).is_err());
    }

    #[test]
    fn tool_search_只能返回候选的不重复有界子集() {
        let candidates = vec!["Read".into(), "Search".into()];
        let good = output(r#"{"selected":["Read"],"rationale":"读取文件"}"#);
        assert_eq!(
            parse_tool_search(&good, &candidates, 1).unwrap().selected,
            ["Read"]
        );
        let invented = output(r#"{"selected":["Shell"],"rationale":"方便"}"#);
        assert!(parse_tool_search(&invented, &candidates, 1).is_err());
        let duplicate = output(r#"{"selected":["Read","Read"],"rationale":"重复"}"#);
        assert!(parse_tool_search(&duplicate, &candidates, 2).is_err());
    }

    #[test]
    fn summary_只归集总输入输出_token() {
        let parsed = parse_structured(
            FunctionalKind::ContextSummary,
            &output(r#"{"conclusion":"状态","confidence":90}"#),
        )
        .unwrap();
        let source = output("unused");
        let summary = to_summary("交接", parsed, Vec::new(), &source);
        assert_eq!(summary.usage.input_tokens, 17);
        assert_eq!(summary.usage.output_tokens, 9);
    }
}
