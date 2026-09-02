//! `Edit`：把文件里的一段文本替换成另一段。
//!
//! **这是读己之写最要紧的那条路径**：它先读、再改、再写，中间那次读必须看到
//! 本 ChangeSet 的 overlay，否则连续两次 Edit 的第二次会基于盘上的旧内容，
//! 把第一次的改动悄悄抹掉。那件事由 [`super::WorkspaceIo`] 的实现方保证（H7）。

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use super::{arg, flag, IoError, ToolCtx, ToolOutput};

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::mutating(
        "Edit",
        "把文件里的一段文本替换成另一段，暂存到当前 ChangeSet，需人工批准。\
         old 必须在文件里恰好出现一次——多处命中会被拒绝，请补足上下文让它唯一，\
         确实要全改则显式传 replace_all。",
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "工作区相对路径",
                    "minLength": 1
                },
                "old": {
                    "type": "string",
                    "description": "要被替换掉的原文，必须与文件内容逐字符一致",
                    "minLength": 1
                },
                "new": {
                    "type": "string",
                    "description": "替换成的新文本，可以为空字符串表示删掉这一段"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "允许替换全部命中处，默认 false（要求唯一）"
                }
            },
            "required": ["path", "old", "new"],
            "additionalProperties": false
        }),
    )
}

/// 执行。
pub async fn run(cx: &ToolCtx<'_>, args: &Value) -> ToolOutput {
    let (Some(path), Some(old), Some(new)) = (arg(args, "path"), arg(args, "old"), arg(args, "new"))
    else {
        return ToolOutput::bad_args(match (args.get("path"), args.get("old")) {
            (None, _) => "path",
            (_, None) => "old",
            _ => "new",
        });
    };
    // 空转要说出来。报成功的话模型会以为改好了，接着往下走。
    if old == new {
        return ToolOutput::failed(format!("old 与 new 相同，这次编辑不会改变 {path}"));
    }

    let content = match cx.files.read(&path).await {
        Ok(content) => content,
        Err(IoError::NotFound) => return ToolOutput::failed(format!("文件不存在：{path}")),
        Err(e) => return ToolOutput::from(e),
    };

    let n = content.matches(&old).count();
    let all = flag(args, "replace_all");
    // 不透传 old/new 正文——可能含用户内容。
    match (n, all) {
        (0, _) => ToolOutput::failed("未找到待替换的文本"),
        // 唯一性是默认要求：模型想改一处、实际改了七处，事后只会看到一句
        // "已替换 7 处"，那时改动已经在 ChangeSet 里了。让它补上下文比让它
        // 事后发现便宜得多。
        (n, false) if n > 1 => ToolOutput::failed(format!(
            "old 在 {path} 里命中 {n} 处，必须唯一；\
             请补足上下文让它只匹配一处，或显式传 replace_all"
        )),
        (n, _) => match cx.files.write(&path, content.replace(&old, &new)).await {
            Err(e) => ToolOutput::from(e),
            Ok(()) => ToolOutput::ok(format!("已替换 {n} 处（未提交，位于 ChangeSet 内）")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;

    async fn 改(files: &FakeWorkspace, args: Value) -> String {
        run(&ToolCtx::files_only(files), &args).await.text.unwrap()
    }

    #[tokio::test]
    async fn 唯一命中时替换() {
        let files = FakeWorkspace::new([("a.md", "开头\n中间\n结尾\n")]);
        let out = 改(&files, json!({"path": "a.md", "old": "中间", "new": "改过"})).await;
        assert!(out.contains("已替换 1 处"), "{out}");
        assert_eq!(files.content("a.md").unwrap(), "开头\n改过\n结尾\n");
    }

    #[tokio::test]
    async fn 命中多处时拒绝并说清楚怎么办() {
        // 只说"失败"的话模型会原样重试。这句话要让它知道有两条出路。
        let files = FakeWorkspace::new([("a.md", "x\nx\n")]);
        let out = 改(&files, json!({"path": "a.md", "old": "x", "new": "y"})).await;
        assert!(out.contains("命中 2 处"), "{out}");
        assert!(out.contains("补足上下文"), "{out}");
        assert!(out.contains("replace_all"), "{out}");
        // 而且**没有改**——拒绝就是没发生。
        assert_eq!(files.content("a.md").unwrap(), "x\nx\n");
    }

    #[tokio::test]
    async fn 补足上下文之后就能唯一命中() {
        let files = FakeWorkspace::new([("a.md", "第一处 x\n第二处 x\n")]);
        let out = 改(&files, json!({"path": "a.md", "old": "第二处 x", "new": "第二处 y"})).await;
        assert!(out.contains("已替换 1 处"), "{out}");
        assert_eq!(files.content("a.md").unwrap(), "第一处 x\n第二处 y\n");
    }

    #[tokio::test]
    async fn 显式_replace_all_才全改() {
        let files = FakeWorkspace::new([("a.md", "x\nx\n")]);
        let out = 改(
            &files,
            json!({"path": "a.md", "old": "x", "new": "y", "replace_all": true}),
        )
        .await;
        assert!(out.contains("已替换 2 处"), "{out}");
        assert_eq!(files.content("a.md").unwrap(), "y\ny\n");
    }

    #[tokio::test]
    async fn 文件不存在与找不到文本是两句不同的话() {
        // 两者要让模型做不同的事：前者去 Glob，后者去 Read 确认原文。
        let files = FakeWorkspace::new([("a.md", "内容")]);
        assert!(改(&files, json!({"path": "无.md", "old": "x", "new": "y"}))
            .await
            .contains("文件不存在"));
        assert!(改(&files, json!({"path": "a.md", "old": "没有这段", "new": "y"}))
            .await
            .contains("未找到待替换的文本"));
    }

    #[tokio::test]
    async fn old_与_new_相同判为失败() {
        // 报成功的话模型会以为改好了，接着往下走。
        let files = FakeWorkspace::new([("a.md", "x")]);
        let out = 改(&files, json!({"path": "a.md", "old": "x", "new": "x"})).await;
        assert!(out.contains("不会改变"), "{out}");
    }

    #[tokio::test]
    async fn 连续两次编辑第二次看得到第一次() {
        // 读己之写。中间那次读读不到 overlay 的话，第二次会基于旧内容，
        // 把第一次的改动悄悄抹掉。
        let files = FakeWorkspace::new([("a.md", "一 二")]);
        改(&files, json!({"path": "a.md", "old": "一", "new": "壹"})).await;
        改(&files, json!({"path": "a.md", "old": "二", "new": "贰"})).await;
        assert_eq!(files.content("a.md").unwrap(), "壹 贰");
    }

    #[tokio::test]
    async fn 正文不进输出() {
        // old/new 可能含用户内容；它们会进事件日志。
        let files = FakeWorkspace::new([("a.md", "机密内容")]);
        let out = 改(&files, json!({"path": "a.md", "old": "机密内容", "new": "另一段机密"})).await;
        assert!(!out.contains("机密"), "{out}");
    }

    #[tokio::test]
    async fn 缺哪个参数就说哪个() {
        let files = FakeWorkspace::new([]);
        let cx = ToolCtx::files_only(&files);
        assert!(run(&cx, &json!({"old": "a", "new": "b"})).await.text.unwrap().contains("path"));
        assert!(run(&cx, &json!({"path": "a", "new": "b"})).await.text.unwrap().contains("old"));
        assert!(run(&cx, &json!({"path": "a", "old": "b"})).await.text.unwrap().contains("new"));
    }
}
