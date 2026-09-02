//! `Read`：读工作区里的一个文本文件。

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use super::text::{self, READ_DEFAULT_LIMIT};
use super::{arg, count, ToolCtx, ToolOutput};

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::read_only(
        "Read",
        "读取工作区内一个 UTF-8 文本文件。大文件按行翻页，返回内容里会说明还剩多少行。\
         读得到本次会话尚未提交的改动。",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "工作区相对路径",
                    "minLength": 1
                },
                "offset": {
                    "type": "integer",
                    "description": "从第几行开始，1 起算，默认 1",
                    "minimum": 1
                },
                "limit": {
                    "type": "integer",
                    "description": "最多读多少行，默认 2000",
                    "minimum": 1,
                    "maximum": 10000
                }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    )
}

/// 执行。
pub async fn run(cx: &ToolCtx<'_>, args: &Value) -> ToolOutput {
    let Some(path) = arg(args, "path") else {
        return ToolOutput::bad_args("path");
    };
    match cx.files.read(&path).await {
        Err(super::IoError::NotFound) => ToolOutput::failed(format!("文件不存在：{path}")),
        Err(e) => ToolOutput::from(e),
        Ok(content) => ToolOutput::ok(text::page(
            &content,
            count(args, "offset", 1),
            count(args, "limit", READ_DEFAULT_LIMIT),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;
    use agentrs_contracts::sandbox::{ExecutionOutcome, RejectReason};

    async fn 读(files: &FakeWorkspace, args: Value) -> ToolOutput {
        run(&ToolCtx::files_only(files), &args).await
    }

    #[tokio::test]
    async fn 整份读回逐字节一致() {
        // Edit 会基于这份内容做替换。少一个结尾换行，一次读改写就悄悄改了文件。
        let files = FakeWorkspace::new([("a.txt", "第一行\n第二行\n")]);
        let out = 读(&files, json!({"path": "a.txt"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 0 });
        assert_eq!(out.text.unwrap(), "第一行\n第二行\n");
    }

    #[tokio::test]
    async fn 能翻页并说明还剩多少() {
        let content = (1..=10).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let files = FakeWorkspace::new([("n.txt", content.as_str())]);
        let out = 读(&files, json!({"path": "n.txt", "offset": 1, "limit": 3})).await;
        let text = out.text.unwrap();
        assert!(text.starts_with("1\n2\n3"), "{text}");
        assert!(text.contains("还有 7 行未显示"), "{text}");
        assert!(text.contains("offset=4"), "要告诉它怎么接着读：{text}");
    }

    #[tokio::test]
    async fn 不存在的文件说得清楚() {
        // "读取失败：NotFound" 这种话模型看不懂该怎么办；带上路径它才能去 Glob。
        let files = FakeWorkspace::new([]);
        let out = 读(&files, json!({"path": "缺席.md"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 1 });
        assert!(out.text.unwrap().contains("缺席.md"));
    }

    #[tokio::test]
    async fn 路径越界有自己的拒绝码() {
        // 把路径逃逸报成"变更集不可用"，会让排查的人去查变更集，
        // 而那里什么问题也没有。
        let files = FakeWorkspace::new([]).with_outside(["../外面"]);
        let out = 读(&files, json!({"path": "../外面"})).await;
        assert_eq!(
            out.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::OutsideWorkspace
            }
        );
    }

    #[tokio::test]
    async fn 缺参数是参数问题而不是变更集问题() {
        let out = 读(&FakeWorkspace::new([]), json!({})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 2 });
        assert!(out.text.unwrap().contains("path"));
    }
}
