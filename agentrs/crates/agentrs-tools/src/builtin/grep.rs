//! `Grep`：在工作区的文本文件里搜一个字面子串。

use agentrs_types::ToolDef;
use serde_json::{json, Value};

use agentrs_types::glob::{glob_match, leaf, under};
use super::text::{self, MATCH_DEFAULT_LIMIT};
use super::{arg, count, flag, ToolCtx, ToolOutput};

/// 模型侧声明。
pub fn def() -> ToolDef {
    ToolDef::read_only(
        "Grep",
        "在工作区的文本文件里搜索一个字面子串，返回 路径:行号:整行。不是正则。\
         搜得到本次会话尚未提交的新建内容，搜不到已删除的。",
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "要查找的字面子串，不是正则表达式",
                    "minLength": 1
                },
                "path": {
                    "type": "string",
                    "description": "限定在这个子目录下搜索；省略则整个工作区"
                },
                "glob": {
                    "type": "string",
                    "description": "只搜文件名匹配此通配符的文件，如 *.rs"
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "忽略大小写，默认 false"
                },
                "max_results": {
                    "type": "integer",
                    "description": "最多返回多少行，超出会明确告知还有多少未显示",
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
    let name_glob = arg(args, "glob");
    let fold = flag(args, "ignore_case");
    let limit = count(args, "max_results", MATCH_DEFAULT_LIMIT);
    let needle = if fold { pattern.to_lowercase() } else { pattern };

    let files = match cx.files.list().await {
        Ok(files) => files,
        Err(e) => return ToolOutput::from(e),
    };
    let mut hits = Vec::new();
    for rel in files {
        if !under(&rel, &scope) {
            continue;
        }
        if let Some(g) = &name_glob {
            // 整条路径与末段都试：模型写 `*.rs` 时想说的是文件名。
            if !glob_match(g, &rel) && !glob_match(g, leaf(&rel)) {
                continue;
            }
        }
        // 读不出来的（二进制、越界）跳过而不是中断——一个坏文件不该让
        // 整次搜索失败。
        let Ok(content) = cx.files.read(&rel).await else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            let hay = if fold { line.to_lowercase() } else { line.to_string() };
            if hay.contains(&needle) {
                hits.push(format!("{rel}:{}:{line}", i + 1));
            }
        }
    }
    ToolOutput::ok(text::cap(hits, limit, "处匹配", "无匹配"))
}

#[cfg(test)]
mod tests {
    use super::super::fake::FakeWorkspace;
    use super::*;

    fn 工作区() -> FakeWorkspace {
        FakeWorkspace::new([
            ("a.rs", "let todo = 1;\nfn main() {}\n"),
            ("b.md", "TODO: 写点说明\n"),
            ("src/c.rs", "// todo later\n"),
        ])
    }

    async fn 搜(args: Value) -> String {
        let files = 工作区();
        run(&ToolCtx::files_only(&files), &args).await.text.unwrap()
    }

    #[tokio::test]
    async fn 返回路径行号与整行() {
        // 行号与整行都要有：模型据此决定 Edit 的 old 该写什么。
        assert_eq!(搜(json!({"pattern": "fn main"})).await, "a.rs:2:fn main() {}");
    }

    #[tokio::test]
    async fn 是字面子串不是正则() {
        // 写成正则的话 `.` 会匹配任意字符，模型得到一堆它没要的东西。
        assert_eq!(搜(json!({"pattern": "fn m.in"})).await, "无匹配");
    }

    #[tokio::test]
    async fn 能限定目录与文件名并忽略大小写() {
        assert_eq!(搜(json!({"pattern": "todo", "path": "src"})).await, "src/c.rs:1:// todo later");
        assert_eq!(搜(json!({"pattern": "todo", "glob": "*.md", "ignore_case": true})).await,
                   "b.md:1:TODO: 写点说明");
        // 不忽略大小写时 TODO 搜不到。
        assert_eq!(搜(json!({"pattern": "todo", "glob": "*.md"})).await, "无匹配");
    }

    #[tokio::test]
    async fn 超过上限时说明总数而不是被整段吞掉() {
        let out = 搜(json!({"pattern": "o", "max_results": 1})).await;
        assert!(out.contains("处匹配，已显示前 1"), "{out}");
    }

    #[tokio::test]
    async fn 读不出来的文件被跳过而不是让整次搜索失败() {
        let files = FakeWorkspace::new([("好.txt", "命中"), ("坏.bin", "命中")])
            .with_outside(["坏.bin"]);
        let out = run(&ToolCtx::files_only(&files), &json!({"pattern": "命中"}))
            .await
            .text
            .unwrap();
        assert_eq!(out, "好.txt:1:命中");
    }

    #[tokio::test]
    async fn 缺参数是参数问题() {
        let files = FakeWorkspace::new([]);
        let out = run(&ToolCtx::files_only(&files), &json!({})).await;
        assert!(out.text.unwrap().contains("pattern"));
    }
}
