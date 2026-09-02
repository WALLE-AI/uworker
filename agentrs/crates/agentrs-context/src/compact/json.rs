// Ported from aionrs (Apache-2.0), crates/aion-compact.
//   Source: crates/aion-compact/src/json.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: let-chain 改写为嵌套 if（本仓库是 edition 2021，let-chain 要 2024）；
//            嵌入式 JSON 的尾部原样保留（上游会把 JSON 之后的文本整段丢掉）。

//! 重排 JSON：短的对象与数组压成一行，长的才展开。
//!
//! 工具返回的 JSON 常常是 `serde_json::to_string_pretty` 的产物——每个字段一行，
//! 一个三字段的小对象占五行。压成一行不损失任何信息，却能省掉大半的行数。
//!
//! **只在压得更短时才采用**：重排之后反而更长的话就原样退回，
//! 那说明这份 JSON 本来的排版就是合适的。

/// 短于此就压成一行。
const INLINE_THRESHOLD: usize = 80;

fn format_value(value: &serde_json::Value, depth: usize) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let oneliner = serde_json::to_string(value).unwrap_or_default();
            if oneliner.len() <= INLINE_THRESHOLD && !oneliner.contains('\n') {
                return oneliner;
            }
            let indent = "  ".repeat(depth + 1);
            let close_indent = "  ".repeat(depth);
            let entries: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{indent}\"{k}\": {}", format_value(v, depth + 1)))
                .collect();
            format!("{{\n{}\n{close_indent}}}", entries.join(",\n"))
        }
        serde_json::Value::Array(arr) => {
            let oneliner = serde_json::to_string(value).unwrap_or_default();
            if oneliner.len() <= INLINE_THRESHOLD {
                return oneliner;
            }
            let indent = "  ".repeat(depth + 1);
            let close_indent = "  ".repeat(depth);
            let items: Vec<String> = arr
                .iter()
                .map(|v| format!("{indent}{}", format_value(v, depth + 1)))
                .collect();
            format!("[\n{}\n{close_indent}]", items.join(",\n"))
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// 重排一段文本里的 JSON。不是 JSON、或重排之后更长，就原样返回。
pub fn compact_json(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let trimmed = text.trim();

    // 整段就是一份 JSON。
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let compacted = format_value(&value, 0);
            return if compacted.len() < trimmed.len() {
                compacted
            } else {
                text.to_string()
            };
        }
    }

    // JSON 嵌在一句话后面（"结果如下：{...}"）。
    if let Some(start) = trimmed.find(['{', '[']) {
        let candidate = &trimmed[start..];
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) {
            let compacted = format_value(&value, 0);
            if compacted.len() < candidate.len() {
                return format!("{}{}", &trimmed[..start], compacted);
            }
        }
    }

    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 短对象压成一行() {
        let pretty = "{\n  \"a\": 1,\n  \"b\": 2\n}";
        assert_eq!(compact_json(pretty), r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn 长对象展开而其中短的子对象仍压成一行() {
        // 输入必须是 pretty 的：本函数只在**压得更短**时才动手，
        // 喂一份已经最紧的 JSON 进去，它理应原样退回（见下一条）。
        let pretty = serde_json::to_string_pretty(&serde_json::json!({
            "outer": {"a": 1, "b": 2},
            "long": (0..12).map(|n| format!("value{n}")).collect::<Vec<_>>(),
        }))
        .unwrap();
        let out = compact_json(&pretty);
        assert!(out.len() < pretty.len(), "没压短：{out}");
        // 外层展开着，内层那个短对象压成了一行。
        assert!(out.contains(r#"{"a":1,"b":2}"#), "{out}");
    }

    #[test]
    fn 已经最紧的_json_原样退回() {
        // 重排只会更长（缩进），此时"压"是负收益。
        let tight = serde_json::to_string(&serde_json::json!({
            "outer": {"a": 1}, "list": [1, 2, 3]
        }))
        .unwrap();
        assert_eq!(compact_json(&tight), tight);
    }

    #[test]
    fn 压不短就原样退回() {
        // 已经是最紧的写法了，重排只会更长（缩进）。原样退回比"压"更诚实。
        let already = r#"{"a":1}"#;
        assert_eq!(compact_json(already), already);
    }

    #[test]
    fn 不是_json_的原样返回() {
        assert_eq!(compact_json("就是一段普通的话"), "就是一段普通的话");
        assert_eq!(compact_json("{ 不闭合"), "{ 不闭合");
        assert_eq!(compact_json(""), "");
    }

    #[test]
    fn 嵌在话里的_json_也压且前缀留着() {
        let text = "结果如下：{\n  \"ok\": true,\n  \"count\": 3\n}";
        let out = compact_json(text);
        assert!(out.starts_with("结果如下："), "前缀丢了：{out}");
        // 字段按名字排序，不按源文里的顺序——`serde_json` 的 Map 默认是
        // BTreeMap。对本模块无所谓（重排本就允许改排版），但断言得按它来。
        assert!(out.contains(r#"{"count":3,"ok":true}"#), "{out}");
    }

    #[test]
    fn 数组同样处理() {
        let pretty = "[\n  1,\n  2,\n  3\n]";
        assert_eq!(compact_json(pretty), "[1,2,3]");
    }

    #[test]
    fn 重排不改变数据本身() {
        // 这一步允许改排版，不允许改内容。
        let pretty = "{\n  \"a\": [1, 2],\n  \"b\": {\"c\": \"x\"}\n}";
        let out = compact_json(pretty);
        let 原: serde_json::Value = serde_json::from_str(pretty).unwrap();
        let 新: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(原, 新);
    }
}
