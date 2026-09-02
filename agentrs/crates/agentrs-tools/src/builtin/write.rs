//! `Write`：把完整内容写进工作区里的一个文件。

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use super::{arg, IoError, ToolCtx, ToolOutput};

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::mutating(
        "Write",
        "把完整内容写进工作区里的一个文件，暂存到当前 ChangeSet，需人工批准。\
         目标已存在时是整体覆盖——要改其中一部分请用 Edit。",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "工作区相对路径，父目录会在提交时自动创建",
                    "minLength": 1
                },
                "content": {
                    "type": "string",
                    "description": "文件的完整新内容"
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
    )
}

/// 执行。
pub async fn run(cx: &ToolCtx<'_>, args: &Value) -> ToolOutput {
    let (Some(path), Some(content)) = (arg(args, "path"), arg(args, "content")) else {
        return ToolOutput::bad_args(if arg(args, "path").is_none() { "path" } else { "content" });
    };

    // 覆盖与新建要分得清：审批面板上一眼能看出这次会盖掉什么。
    let 原有 = match cx.files.read(&path).await {
        Ok(text) => Some(text.len()),
        Err(IoError::NotFound) => None,
        Err(e) => return ToolOutput::from(e),
    };
    let n = content.len();
    if let Err(e) = cx.files.write(&path, content).await {
        return ToolOutput::from(e);
    }
    let 说明 = match 原有 {
        Some(before) => format!("已覆盖（原 {before} 字节 → {n} 字节）"),
        None => format!("已新建（{n} 字节）"),
    };
    ToolOutput::ok(format!("{说明}，未提交，位于 ChangeSet 内"))
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;
    use agentrs_contracts::sandbox::{ExecutionOutcome, RejectReason};

    #[tokio::test]
    async fn 区分新建与覆盖() {
        // 审批面板上人要一眼看出这次会不会盖掉什么。
        let files = FakeWorkspace::new([("旧.md", "12345")]);
        let cx = ToolCtx::files_only(&files);

        let out = run(&cx, &json!({"path": "新.md", "content": "abc"})).await;
        assert!(out.text.unwrap().contains("已新建（3 字节）"));

        let out = run(&cx, &json!({"path": "旧.md", "content": "ab"})).await;
        let text = out.text.unwrap();
        assert!(text.contains("已覆盖"), "{text}");
        assert!(text.contains("原 5 字节 → 2 字节"), "{text}");
    }

    #[tokio::test]
    async fn 写完立刻读得到() {
        // 读己之写。中间那次读读不到的话，连续两次 Edit 的第二次会基于旧内容。
        let files = FakeWorkspace::new([]);
        let cx = ToolCtx::files_only(&files);
        run(&cx, &json!({"path": "x.md", "content": "写入的内容"})).await;
        assert_eq!(files.content("x.md").as_deref(), Some("写入的内容"));
    }

    #[tokio::test]
    async fn 明说未提交() {
        // 模型据此知道还要请人确认，而不是以为已经落盘了。
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({"path": "a", "content": "b"})).await;
        assert!(out.text.unwrap().contains("未提交"));
    }

    #[tokio::test]
    async fn 路径越界被拒() {
        let files = FakeWorkspace::new([]).with_outside(["/etc/passwd"]);
        let out = run(
            &ToolCtx::files_only(&files),
            &json!({"path": "/etc/passwd", "content": "x"}),
        )
        .await;
        assert_eq!(
            out.outcome,
            ExecutionOutcome::Rejected {
                reason: RejectReason::OutsideWorkspace
            }
        );
    }

    #[tokio::test]
    async fn 缺哪个参数就说哪个() {
        let files = FakeWorkspace::new([]);
        let cx = ToolCtx::files_only(&files);
        assert!(run(&cx, &json!({"content": "x"})).await.text.unwrap().contains("path"));
        assert!(run(&cx, &json!({"path": "x"})).await.text.unwrap().contains("content"));
    }
}
