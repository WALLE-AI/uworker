//! 工具描述与 schema 的质量审查（架构 §1.1 裁决 3）。
//!
//! 描述文本归 Core——它随注册一起提供，内核不发明它。但它**直接决定 Agent 好不好
//! 用**：模型选错工具、捏造参数、把 `old`/`new` 用反，几乎总能追回到一句写坏的描述
//! 或一份漏了约束的 schema。`ToolDef::description` 的文档一直写着内核对它做质量
//! lint，而在此之前这句话没有任何代码兑现。
//!
//! 形制照抄 [`agentrs_prompts::lint`]：`Finding` + `Violation` + `audit`。那套已经
//! 证明好用，两处各写一套迟早会漂移。
//!
//! **这份规则必然不全。** 它拦的是能机械判定的那几种；真正的防线是 eval 集里的
//! "工具选择正确性"。所以每条规则都只在**确定**时才报，宁可漏报。

use agentrs_types::ToolDef;
use serde_json::Value;

/// 描述短于此即视为没写。
const 描述下限: usize = 20;

/// 一条 lint 发现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 哪个工具。
    pub tool: String,
    /// 违反了什么。
    pub violation: Violation,
    /// 命中的片段或字段名，**已截断**。
    pub sample: String,
}

impl Finding {
    /// 一行人类可读形式。
    pub fn describe(&self) -> String {
        format!("{}: {} ({})", self.tool, self.violation.why(), self.sample)
    }
}

/// 违规类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    /// 描述为空或过短。
    ThinDescription,
    /// 参数 schema 不是对象。
    SchemaNotObject,
    /// 没有 `additionalProperties: false`。
    OpenSchema,
    /// 某个参数没有 `description`。
    UndocumentedField,
    /// `required` 里的名字不在 `properties` 里。
    RequiredNotDeclared,
    /// 描述里含绝对路径。
    AbsolutePath,
    /// 描述里含疑似密钥或连接串。
    Secret,
    /// 目录里有重名工具。
    DuplicateName,
}

impl Violation {
    /// 不这样会怎样。写给读到 lint 输出的人。
    pub const fn why(self) -> &'static str {
        match self {
            Self::ThinDescription => "描述太短，模型只能靠名字猜用途",
            Self::SchemaNotObject => "参数 schema 不是 object，各厂商一律要求对象",
            Self::OpenSchema => "缺 additionalProperties:false，模型可以捏造参数而无人拦截",
            Self::UndocumentedField => "参数没有 description，字段名往往不自明",
            Self::RequiredNotDeclared => "required 里的名字不在 properties 里，这条永远过不了",
            Self::AbsolutePath => "描述里有绝对路径，换台机器就是错的",
            Self::Secret => "描述里有疑似密钥或连接串",
            Self::DuplicateName => "目录里有重名工具，模型的调用会落到哪一个不确定",
        }
    }
}

/// 密钥与连接串特征。与 `agentrs_prompts::lint` 同源。
const 密钥特征: &[&str] = &[
    "sk-",
    "ghp_",
    "AKIA",
    "xoxb-",
    "-----BEGIN",
    "postgres://",
    "mysql://",
    "mongodb://",
    "redis://",
    "password=",
    "api_key=",
    "apikey=",
];

fn 截断(text: &str) -> String {
    let cut: String = text.chars().take(40).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

fn finding(tool: &str, violation: Violation, sample: &str) -> Finding {
    Finding {
        tool: tool.to_string(),
        violation,
        sample: 截断(sample),
    }
}

/// 审查一个工具。
pub fn audit(tool: &ToolDef) -> Vec<Finding> {
    let mut out = Vec::new();

    if tool.description.trim().chars().count() < 描述下限 {
        out.push(finding(&tool.name, Violation::ThinDescription, &tool.description));
    }
    for token in tool
        .description
        .split(|ch: char| ch.is_whitespace() || ch == '"' || ch == '`')
    {
        // Windows 盘符也算：`C:\` 与 `/etc` 一样换台机器就错。
        let 绝对 = token.starts_with('/') && token.len() > 1
            || token.len() > 2 && token.as_bytes()[1] == b':' && token.contains('\\');
        if 绝对 {
            out.push(finding(&tool.name, Violation::AbsolutePath, token));
        }
    }
    for needle in 密钥特征 {
        if tool.description.contains(needle) {
            out.push(finding(&tool.name, Violation::Secret, needle));
        }
    }

    let Some(schema) = tool.parameters.as_object() else {
        out.push(finding(&tool.name, Violation::SchemaNotObject, "参数"));
        return out;
    };
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        out.push(finding(&tool.name, Violation::SchemaNotObject, "type"));
    }

    let properties = schema.get("properties").and_then(Value::as_object);

    // 无参数的工具没什么可捏造的，不强求这条。
    let 有参数 = properties.is_some_and(|p| !p.is_empty());
    if 有参数 && schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        out.push(finding(&tool.name, Violation::OpenSchema, "additionalProperties"));
    }

    if let Some(properties) = properties {
        for (name, spec) in properties {
            let 有说明 = spec
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty());
            if !有说明 {
                out.push(finding(&tool.name, Violation::UndocumentedField, name));
            }
        }
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !properties.contains_key(name) {
                    out.push(finding(&tool.name, Violation::RequiredNotDeclared, name));
                }
            }
        }
    } else if schema.get("required").is_some() {
        out.push(finding(&tool.name, Violation::RequiredNotDeclared, "required"));
    }

    out
}

/// 审查一整份目录，另查重名。
pub fn audit_catalog(catalog: &[ToolDef]) -> Vec<Finding> {
    let mut out = Vec::new();
    for (index, tool) in catalog.iter().enumerate() {
        if catalog[..index].iter().any(|seen| seen.name == tool.name) {
            out.push(finding(&tool.name, Violation::DuplicateName, &tool.name));
        }
        out.extend(audit(tool));
    }
    out
}

/// 把发现汇成一段可读文本，给测试失败与 CLI 输出用。
pub fn describe_all(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(Finding::describe)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn 好工具() -> ToolDef {
        ToolDef::read_only(
            "Read",
            "读取工作区内一个 UTF-8 文本文件的内容，可按行翻页。",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "工作区相对路径" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    #[test]
    fn 写得好的工具没有任何发现() {
        assert!(audit(&好工具()).is_empty(), "{:?}", audit(&好工具()));
    }

    #[test]
    fn 描述太短会被指出来() {
        let mut tool = 好工具();
        tool.description = "读文件".into();
        let f = audit(&tool);
        assert_eq!(f[0].violation, Violation::ThinDescription);
        assert!(f[0].describe().contains("靠名字猜"));
    }

    #[test]
    fn 开放_schema_会被指出来() {
        // 模型会捏造字段，而 schema 校验此时无从拦截。
        let mut tool = 好工具();
        tool.parameters = json!({
            "type": "object",
            "properties": {"path": {"type": "string", "description": "路径"}}
        });
        assert_eq!(audit(&tool)[0].violation, Violation::OpenSchema);
    }

    #[test]
    fn 无参数的工具不强求那条() {
        let tool = ToolDef::read_only(
            "Status",
            "报告当前 Run 的状态摘要，不接受任何参数。",
            json!({"type": "object", "properties": {}}),
        );
        assert!(audit(&tool).is_empty(), "{:?}", audit(&tool));
    }

    #[test]
    fn 没有说明的参数会被指出来() {
        let mut tool = 好工具();
        tool.parameters = json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false
        });
        let f = audit(&tool);
        assert_eq!(f[0].violation, Violation::UndocumentedField);
        assert_eq!(f[0].sample, "path");
    }

    #[test]
    fn required_指向不存在的参数会被指出来() {
        let mut tool = 好工具();
        tool.parameters = json!({
            "type": "object",
            "properties": {"path": {"type": "string", "description": "路径"}},
            "required": ["path", "recursive"],
            "additionalProperties": false
        });
        let f = audit(&tool);
        assert_eq!(f[0].violation, Violation::RequiredNotDeclared);
        assert_eq!(f[0].sample, "recursive");
    }

    #[test]
    fn schema_不是对象会被指出来并停下() {
        let mut tool = 好工具();
        tool.parameters = json!("一个字符串");
        let f = audit(&tool);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].violation, Violation::SchemaNotObject);
    }

    #[test]
    fn 描述里的绝对路径与密钥会被指出来() {
        let mut tool = 好工具();
        tool.description = "读取 /home/alice/secrets 下的文件，用 sk-abc 认证。".into();
        let kinds: Vec<Violation> = audit(&tool).iter().map(|f| f.violation).collect();
        assert!(kinds.contains(&Violation::AbsolutePath));
        assert!(kinds.contains(&Violation::Secret));
    }

    #[test]
    fn 相对路径与斜杠本身不误报() {
        let mut tool = 好工具();
        tool.description = "读取 src/main.rs 之类的相对路径；分隔符是 / 。".into();
        assert!(audit(&tool).is_empty(), "{:?}", audit(&tool));
    }

    #[test]
    fn 重名工具会被指出来() {
        let catalog = vec![好工具(), 好工具()];
        let f = audit_catalog(&catalog);
        assert_eq!(f[0].violation, Violation::DuplicateName);
    }

    #[test]
    fn 样本被截断以免把用户正文整段带进日志() {
        let mut tool = 好工具();
        tool.description = "x".repeat(200);
        // 描述够长，不触发 ThinDescription；这里只看截断行为。
        let f = finding("T", Violation::ThinDescription, &tool.description);
        assert!(f.sample.chars().count() <= 41, "{}", f.sample);
        assert!(f.sample.ends_with('…'));
    }
}
