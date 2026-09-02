//! 工具参数的 JSON Schema 校验。
//!
//! 固定管线的第一道关口（架构 §8.1）。它挡的是**模型说了句不合语法的话**——
//! 少了必填字段、把数字写成字符串、捏造了一个不存在的参数。此前这一步只存在于
//! 注释里，于是这类调用一路走到 Sandbox，换回一个与真实原因无关的拒绝码。
//!
//! ## 不认识的关键字一律放行
//!
//! 这是本模块最要紧的一条判据，也是它敢于只实现一个子集的前提。
//!
//! 这里**不是**一个完整的 JSON Schema 实现：没有 `$ref`、`oneOf`、`pattern`、
//! `allOf`。遇到不认识的关键字时它放行，只在能**确定地**判定"不合法"时才拒绝。
//!
//! 反过来做——看不懂就拒——会把第三方注册的工具整个挡在门外：MCP 目录里的 schema
//! 由别人写，用到什么关键字不由我们决定。放过一个畸形调用的代价是模型收到一条结构化
//! 错误再试一次；挡住一个合法工具的代价是它永远不能用。两者不对称。

use serde_json::Value;

/// 一处不符。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// 出问题的字段路径，如 `path` 或 `edits[0].old`；根对象为空串。
    pub pointer: String,
    /// 哪里不对，写成能直接指导改正的话。
    pub message: String,
}

impl Violation {
    fn at(pointer: &str, message: impl Into<String>) -> Self {
        Self {
            pointer: pointer.to_string(),
            message: message.into(),
        }
    }

    /// 一行人类可读形式：`path: 必填字段缺失`。
    pub fn describe(&self) -> String {
        if self.pointer.is_empty() {
            self.message.clone()
        } else {
            format!("{}: {}", self.pointer, self.message)
        }
    }
}

/// 把若干不符汇成一条可回灌给模型的消息。
///
/// 全部列出而不是只报第一条：模型一次改一处、每次都要再跑一整个 Step，
/// 三个字段错就是三轮，而这三轮里每一轮都可能再跨一次副作用边界。
pub fn describe_all(violations: &[Violation]) -> String {
    violations
        .iter()
        .map(Violation::describe)
        .collect::<Vec<_>>()
        .join("；")
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 值是否满足 `type` 关键字。整数按 JSON Schema 的规矩算作 number 的子集。
fn matches_type(expected: &str, value: &Value) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        // 不认识的类型名：放行。
        _ => true,
    }
}

fn child(pointer: &str, key: &str) -> String {
    if pointer.is_empty() {
        key.to_string()
    } else {
        format!("{pointer}.{key}")
    }
}

/// 按 `schema` 校验 `value`，返回全部不符之处。
///
/// 空 vec 表示通过。`schema` 不是对象时一律通过——那不是一份我们能读懂的约束。
pub fn validate(schema: &Value, value: &Value) -> Vec<Violation> {
    let mut out = Vec::new();
    validate_at(schema, value, "", &mut out);
    out
}

fn validate_at(schema: &Value, value: &Value, pointer: &str, out: &mut Vec<Violation>) {
    let Some(schema) = schema.as_object() else {
        return;
    };

    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        if !matches_type(expected, value) {
            out.push(Violation::at(
                pointer,
                format!("期望 {expected}，收到 {}", type_name(value)),
            ));
            // 类型都不对，再按它的形状往下查只会产生一串派生错误。
            return;
        }
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            let names: Vec<String> = allowed.iter().map(ToString::to_string).collect();
            out.push(Violation::at(
                pointer,
                format!("只能取 {}", names.join(" / ")),
            ));
        }
    }

    if let Some(text) = value.as_str() {
        if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
            if (text.chars().count() as u64) < min {
                out.push(Violation::at(pointer, format!("至少 {min} 个字符")));
            }
        }
    }

    if let Some(number) = value.as_f64() {
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
            if number < min {
                out.push(Violation::at(pointer, format!("不得小于 {min}")));
            }
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
            if number > max {
                out.push(Violation::at(pointer, format!("不得大于 {max}")));
            }
        }
    }

    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    out.push(Violation::at(&child(pointer, name), "必填字段缺失"));
                }
            }
        }

        let properties = schema.get("properties").and_then(Value::as_object);

        // `additionalProperties: false` 才拦；缺省是允许，与 JSON Schema 一致。
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            if let Some(properties) = properties {
                let mut unknown: Vec<&str> = object
                    .keys()
                    .filter(|key| !properties.contains_key(*key))
                    .map(String::as_str)
                    .collect();
                unknown.sort_unstable();
                for name in unknown {
                    out.push(Violation::at(
                        &child(pointer, name),
                        "该工具没有这个参数",
                    ));
                }
            }
        }

        if let Some(properties) = properties {
            // 按 schema 的字段顺序查，而不是按收到的对象——同一个错误在两次调用里
            // 必须给出同样顺序的消息，否则日志比对会因为参数顺序而抖。
            for (name, sub) in properties {
                if let Some(field) = object.get(name) {
                    validate_at(sub, field, &child(pointer, name), out);
                }
            }
        }
    }

    if let (Some(items), Some(array)) = (schema.get("items"), value.as_array()) {
        for (index, item) in array.iter().enumerate() {
            validate_at(items, item, &format!("{pointer}[{index}]"), out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                "mode": { "type": "string", "enum": ["read", "write"] },
                "deep": { "type": "boolean" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    #[test]
    fn 合法参数没有任何不符() {
        let ok = json!({"path": "a.md", "limit": 10, "mode": "read", "deep": true});
        assert!(validate(&schema(), &ok).is_empty());
        // 只给必填字段也合法。
        assert!(validate(&schema(), &json!({"path": "a.md"})).is_empty());
    }

    #[test]
    fn 必填字段缺失被指名道姓() {
        let v = validate(&schema(), &json!({}));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pointer, "path");
        assert!(v[0].message.contains("必填"));
    }

    #[test]
    fn 类型不符报出期望与实际() {
        let v = validate(&schema(), &json!({"path": 7}));
        assert_eq!(v[0].describe(), "path: 期望 string，收到 number");
    }

    #[test]
    fn 类型不符之后不再产生派生错误() {
        // path 已经不是字符串了，再报一遍"至少 1 个字符"只是噪音。
        let v = validate(&schema(), &json!({"path": 7}));
        assert_eq!(v.len(), 1, "{v:?}");
    }

    #[test]
    fn 整数是_number_的子集而_number_不是整数() {
        let s = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
        assert!(validate(&s, &json!({"n": 3})).is_empty());
        assert!(!validate(&s, &json!({"n": 3.5})).is_empty());
        let s = json!({"type": "object", "properties": {"n": {"type": "number"}}});
        assert!(validate(&s, &json!({"n": 3})).is_empty());
        assert!(validate(&s, &json!({"n": 3.5})).is_empty());
    }

    #[test]
    fn 捏造的参数被拦下() {
        let v = validate(&schema(), &json!({"path": "a", "recursive": true}));
        assert_eq!(v[0].describe(), "recursive: 该工具没有这个参数");
    }

    #[test]
    fn 缺省允许额外参数() {
        // 与 JSON Schema 一致：没写 additionalProperties 就是允许。
        let s = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        assert!(validate(&s, &json!({"a": "x", "b": 1})).is_empty());
    }

    #[test]
    fn 范围与枚举与长度各自生效() {
        assert!(!validate(&schema(), &json!({"path": "a", "limit": 0})).is_empty());
        assert!(!validate(&schema(), &json!({"path": "a", "limit": 101})).is_empty());
        assert!(!validate(&schema(), &json!({"path": "a", "mode": "delete"})).is_empty());
        assert!(!validate(&schema(), &json!({"path": ""})).is_empty());
    }

    #[test]
    fn 不认识的关键字一律放行() {
        // 这是本模块敢于只实现一个子集的前提：看不懂就拒，会把第三方（含 MCP）
        // 注册的工具整个挡在门外。
        let s = json!({
            "type": "object",
            "properties": {
                "a": { "type": "string", "pattern": "^x", "format": "uri" },
                "b": { "oneOf": [{"type": "string"}, {"type": "integer"}] }
            },
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "additionalProperties": false
        });
        assert!(validate(&s, &json!({"a": "not-matching", "b": 7})).is_empty());
    }

    #[test]
    fn 看不懂的_schema_整体放行() {
        assert!(validate(&json!("nonsense"), &json!({"a": 1})).is_empty());
        assert!(validate(&json!(true), &json!({})).is_empty());
    }

    #[test]
    fn 数组逐项校验并带下标() {
        let s = json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"old": {"type": "string"}},
                        "required": ["old"]
                    }
                }
            }
        });
        let v = validate(&s, &json!({"edits": [{"old": "x"}, {}]}));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pointer, "edits[1].old");
    }

    #[test]
    fn 多处不符一次全报() {
        // 一次只报一条会让模型跑三轮才改对，而每一轮都可能再跨一次副作用边界。
        let v = validate(&schema(), &json!({"limit": 0, "mode": "x"}));
        assert_eq!(v.len(), 3, "{v:?}");
        let text = describe_all(&v);
        assert!(text.contains("path"));
        assert!(text.contains("limit"));
        assert!(text.contains("mode"));
    }

    #[test]
    fn 消息顺序只随_schema_不随参数顺序() {
        // 同一个错误在两次调用里必须给出同样顺序的消息，否则日志比对会因为参数
        // 顺序而抖。
        let a = validate(&schema(), &json!({"limit": 0, "mode": "x", "path": ""}));
        let b = validate(&schema(), &json!({"mode": "x", "path": "", "limit": 0}));
        assert_eq!(describe_all(&a), describe_all(&b));
    }
}
