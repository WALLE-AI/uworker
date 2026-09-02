//! `Delete`：删掉工作区里的一个文件。

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use super::{arg, IoError, ToolCtx, ToolOutput};

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::mutating(
        "Delete",
        "删除工作区里的一个文件，暂存到当前 ChangeSet，需人工批准。文件不存在会被拒绝。",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "工作区相对路径",
                    "minLength": 1
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
    // 删一个不存在的文件从前报成功，于是"删掉了吗"这个问题得不到真实答案。
    match cx.files.read(&path).await {
        Err(IoError::NotFound) => return ToolOutput::failed(format!("文件不存在：{path}")),
        Err(e) => return ToolOutput::from(e),
        Ok(_) => {}
    }
    match cx.files.delete(&path).await {
        Err(e) => ToolOutput::from(e),
        Ok(()) => ToolOutput::ok("已删除（未提交，位于 ChangeSet 内）"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;
    use agentrs_contracts::sandbox::ExecutionOutcome;

    #[tokio::test]
    async fn 删掉之后就读不到了() {
        // 墓碑要表现得和"文件不存在"完全一样，否则删除只是看起来生效了。
        let files = FakeWorkspace::new([("走.md", "内容")]);
        let cx = ToolCtx::files_only(&files);
        let out = run(&cx, &json!({"path": "走.md"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 0 });
        assert!(files.content("走.md").is_none());
        assert!(files.paths().is_empty());
    }

    #[tokio::test]
    async fn 不存在的文件报失败而不是成功() {
        // 报成功的话，"删掉了吗"这个问题得不到真实答案。
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({"path": "没有.md"})).await;
        assert_eq!(out.outcome, ExecutionOutcome::Completed { exit_code: 1 });
        assert!(out.text.unwrap().contains("没有.md"));
    }

    #[tokio::test]
    async fn 删两次第二次会失败() {
        let files = FakeWorkspace::new([("x.md", "内容")]);
        let cx = ToolCtx::files_only(&files);
        assert_eq!(
            run(&cx, &json!({"path": "x.md"})).await.outcome,
            ExecutionOutcome::Completed { exit_code: 0 }
        );
        assert_eq!(
            run(&cx, &json!({"path": "x.md"})).await.outcome,
            ExecutionOutcome::Completed { exit_code: 1 }
        );
    }

    #[tokio::test]
    async fn 明说未提交() {
        let files = FakeWorkspace::new([("x.md", "c")]);
        let out = run(&ToolCtx::files_only(&files), &json!({"path": "x.md"})).await;
        assert!(out.text.unwrap().contains("未提交"));
    }
}
