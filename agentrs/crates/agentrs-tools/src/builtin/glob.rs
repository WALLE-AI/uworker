//! `Glob`：按通配符列出工作区里的文件。

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use agentrs_types::glob::{glob_match, under};
use super::text::{self, MATCH_DEFAULT_LIMIT};
use super::{arg, count, ToolCtx, ToolOutput};

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::read_only(
        "Glob",
        "按通配符列出工作区里的文件。用它来发现有哪些文件，而不是凭猜测去 Read \
         一个可能不存在的路径。看得到本次会话尚未提交的新建文件，看不到已删除的。",
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "通配符，如 **/*.md 或 src/*.rs。* 不跨目录分隔符，** 跨，? 匹配单个字符",
                    "minLength": 1
                },
                "path": {
                    "type": "string",
                    "description": "限定在这个子目录下查找；省略则整个工作区"
                },
                "max_results": {
                    "type": "integer",
                    "description": "最多返回多少条，超出会明确告知还有多少未显示",
                    "minimum": 1,
                    "maximum": 1000
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        }),
    )
}

/// 执行。
pub async fn run(cx: &ToolCtx<'_>, args: &Value) -> ToolOutput {
    let Some(pattern) = arg(args, "pattern") else {
        return ToolOutput::bad_args("pattern");
    };
    let scope = arg(args, "path").unwrap_or_default();
    let limit = count(args, "max_results", MATCH_DEFAULT_LIMIT);

    let files = match cx.files.list().await {
        Ok(files) => files,
        Err(e) => return ToolOutput::from(e),
    };
    let hits: Vec<String> = files
        .into_iter()
        .filter(|rel| under(rel, &scope))
        .filter(|rel| glob_match(&pattern, rel))
        .collect();
    ToolOutput::ok(text::cap(hits, limit, "个文件", "无匹配的文件"))
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;

    fn 工作区() -> FakeWorkspace {
        FakeWorkspace::new([
            ("a.md", "x"),
            ("src/main.rs", "x"),
            ("src/lib.rs", "x"),
            ("src/deep/mod.rs", "x"),
            ("docs/b.md", "x"),
        ])
    }

    async fn 列(args: Value) -> String {
        let files = 工作区();
        run(&ToolCtx::files_only(&files), &args)
            .await
            .text
            .unwrap()
    }

    #[tokio::test]
    async fn 按通配符列出文件并排序() {
        // 顺序稳定，模型两次问同一个问题该得到同一个答案。
        assert_eq!(列(json!({"pattern": "**/*.rs"})).await, "src/deep/mod.rs\nsrc/lib.rs\nsrc/main.rs");
    }

    #[tokio::test]
    async fn 单星不跨目录() {
        assert_eq!(列(json!({"pattern": "*.md"})).await, "a.md");
        assert_eq!(列(json!({"pattern": "src/*.rs"})).await, "src/lib.rs\nsrc/main.rs");
    }

    #[tokio::test]
    async fn 能限定子目录() {
        let out = 列(json!({"pattern": "**/*.rs", "path": "src/deep"})).await;
        assert_eq!(out, "src/deep/mod.rs");
    }

    #[tokio::test]
    async fn 无匹配时说无匹配而不是空字符串() {
        // 空字符串会被模型读成"工具坏了"或"文件是空的"。
        assert_eq!(列(json!({"pattern": "**/*.toml"})).await, "无匹配的文件");
    }

    #[tokio::test]
    async fn 超过上限时说明总数() {
        let out = 列(json!({"pattern": "**/*", "max_results": 2})).await;
        assert!(out.contains("共 5 个文件，已显示前 2"), "{out}");
    }

    #[tokio::test]
    async fn 缺参数是参数问题() {
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({})).await;
        assert!(out.text.unwrap().contains("pattern"));
    }
}
