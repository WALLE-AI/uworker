// Ported from aionrs (Apache-2.0), crates/aion-compact.
//   Source: crates/aion-compact/src/toon.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: let-chain 改写为嵌套 if（本仓库是 edition 2021）；
//            `unwrap()` 全部消除（原实现在第二遍遍历时对已校验过的值再 unwrap，
//            读的人无从确认那个不变量）；字段顺序显式取自首个对象并逐行按同一
//            顺序取值；说明文本改写。

//! TOON：把"一组同构对象"编码成表格。
//!
//! ```text
//! [2]{id,name,role}:
//!   1,Alice,admin
//!   2,Bob,user
//! ```
//!
//! 等价于 `[{"id":1,"name":"Alice",...},...]`，但字段名只出现一次而不是每行一遍。
//! 一份 50 行、8 个字段的结果集能省掉几百个 token——**信息一个字节没少**，
//! 少掉的全是重复的字段名。
//!
//! 只有在整组对象**字段完全一致且都是标量**时才编码；但凡有一条不合，
//! 就整段退回原样。宁可不省，也不能编出一份需要模型去猜结构的东西。
//!
//! 用它的话，系统提示里要带上 [`toon_format_instructions`]，否则模型不认得这个格式。

/// 把一个 JSON 数组编码成 TOON；不符合条件时返回 `None`。
pub fn toon_encode_array(value: &serde_json::Value) -> Option<String> {
    let arr = value.as_array()?;
    if arr.is_empty() {
        return None;
    }

    // 字段顺序取自第一个对象，后面每一行都按同一顺序取值——
    // serde_json 默认保序，但这里不依赖那件事，显式按名字取。
    let fields: Vec<String> = arr[0].as_object()?.keys().cloned().collect();
    if fields.is_empty() {
        return None;
    }

    // 先整体校验再编码：中途发现不合就整段退回，不留半份编好的东西。
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item.as_object()?;
        if obj.len() != fields.len() {
            return None;
        }
        let mut row = Vec::with_capacity(fields.len());
        for field in &fields {
            let val = obj.get(field)?;
            // 嵌套结构没法摊平成一格，整段退回。
            if val.is_object() || val.is_array() {
                return None;
            }
            row.push(format_toon_value(val));
        }
        rows.push(row);
    }

    let mut out = format!("[{}]{{{}}}:", arr.len(), fields.join(","));
    for row in rows {
        out.push_str("\n  ");
        out.push_str(&row.join(","));
    }
    Some(out)
}

fn format_toon_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            // 含分隔符的值必须加引号，否则一行的列数就对不上了。
            if s.contains(',') || s.contains('\n') || s.contains('"') {
                format!("\"{}\"", s.replace('"', "\\\""))
            } else {
                s.clone()
            }
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// 尝试把一段文本里的 JSON 数组编成 TOON；不合适就原样返回。
pub fn try_toon_encode(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let trimmed = text.trim();

    if trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(encoded) = toon_encode_array(&value) {
                return encoded;
            }
            return text.to_string();
        }
    }

    // 数组嵌在一句话里。找配对的 `]` 定出边界，前后缀都留着。
    if let Some(start) = trimmed.find('[') {
        let rest = &trimmed[start..];
        let mut depth = 0usize;
        let mut end = None;
        for (i, ch) in rest.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some(end_pos) = end {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&rest[..end_pos]) {
                if let Some(encoded) = toon_encode_array(&value) {
                    return format!("{}{}{}", &trimmed[..start], encoded, &rest[end_pos..]);
                }
            }
        }
    }

    text.to_string()
}

/// 要放进系统提示的一段说明。
///
/// **用了 TOON 就必须带上它。** 模型不认得这个格式，看到
/// `[2]{id,name}:` 会当成一串乱码或者猜错结构——那比多花几百个 token 糟得多。
pub fn toon_format_instructions() -> &'static str {
    "\
# TOON 格式

工具结果可能以 TOON（Token-Oriented Object Notation）表格形式返回，以节省 token。格式：

```
[N]{field1,field2,...}:
  value1,value2,...
  value1,value2,...
```

- `[N]` 是数组长度
- `{fields}` 是列名
- 其后每一行缩进两格，是一条记录，值以逗号分隔
- 含逗号的字符串值会加引号

它等价于一个 JSON 对象数组。例如：
```
[2]{id,name,role}:
  1,Alice,admin
  2,Bob,user
```
等价于 `[{\"id\":1,\"name\":\"Alice\",\"role\":\"admin\"},{\"id\":2,\"name\":\"Bob\",\"role\":\"user\"}]`"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 同构对象数组编成表格() {
        let v = json!([
            {"id": 1, "name": "Alice", "role": "admin"},
            {"id": 2, "name": "Bob", "role": "user"}
        ]);
        assert_eq!(
            toon_encode_array(&v).unwrap(),
            "[2]{id,name,role}:\n  1,Alice,admin\n  2,Bob,user"
        );
    }

    #[test]
    fn 字段名只出现一次_那才是省下来的东西() {
        let v = json!([{"aaaa": 1}, {"aaaa": 2}, {"aaaa": 3}]);
        let out = toon_encode_array(&v).unwrap();
        assert_eq!(out.matches("aaaa").count(), 1, "{out}");
    }

    #[test]
    fn 字段不一致就整段退回() {
        // 宁可不省，也不能编出一份需要模型去猜结构的东西。
        let v = json!([{"a": 1}, {"b": 2}]);
        assert!(toon_encode_array(&v).is_none());
        let v = json!([{"a": 1}, {"a": 2, "b": 3}]);
        assert!(toon_encode_array(&v).is_none());
    }

    #[test]
    fn 有嵌套结构就整段退回() {
        assert!(toon_encode_array(&json!([{"a": {"b": 1}}])).is_none());
        assert!(toon_encode_array(&json!([{"a": [1]}])).is_none());
    }

    #[test]
    fn 空数组与非数组返回_none() {
        assert!(toon_encode_array(&json!([])).is_none());
        assert!(toon_encode_array(&json!({"a": 1})).is_none());
        assert!(toon_encode_array(&json!([{}])).is_none());
    }

    #[test]
    fn 含逗号的值会加引号() {
        // 不加的话一行的列数就对不上了，模型解出来的记录会错位。
        let v = json!([{"text": "a,b"}, {"text": "plain"}]);
        let out = toon_encode_array(&v).unwrap();
        assert!(out.contains("\"a,b\""), "{out}");
        assert!(out.contains("\n  plain"), "{out}");
    }

    #[test]
    fn 各种标量都编得出来() {
        // 列顺序按字段名排序，不按源文里的顺序——`serde_json` 的 Map 默认是
        // BTreeMap。这不影响正确性（表头与每一行用的是同一个顺序），
        // 但它是这个编码的可观察行为，钉在这里免得以后有人当成 bug。
        let v = json!([{"n": 1, "f": 1.5, "b": true, "z": null}]);
        let out = toon_encode_array(&v).unwrap();
        assert_eq!(out, "[1]{b,f,n,z}:\n  true,1.5,1,null");
    }

    #[test]
    fn 表头顺序与每行取值顺序一致() {
        // 这才是真正要紧的：表头说 `{b,f,n}`，行里第二个值就必须是 f 的值。
        // 两处顺序一旦分叉，模型解出来的每条记录都是错位的，而且看不出来。
        let v = json!([
            {"n": 1, "f": "一", "b": true},
            {"b": false, "n": 2, "f": "二"}
        ]);
        let out = toon_encode_array(&v).unwrap();
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), "[2]{b,f,n}:");
        assert_eq!(lines.next().unwrap(), "  true,一,1");
        // 第二条对象的字段是乱序写的，编出来仍按表头的顺序。
        assert_eq!(lines.next().unwrap(), "  false,二,2");
    }

    #[test]
    fn 整段是数组时直接编() {
        let text = r#"[{"id":1,"n":"a"},{"id":2,"n":"b"}]"#;
        assert_eq!(try_toon_encode(text), "[2]{id,n}:\n  1,a\n  2,b");
    }

    #[test]
    fn 嵌在话里的数组前后缀都留着() {
        let text = r#"共 2 条：[{"id":1},{"id":2}] 以上。"#;
        let out = try_toon_encode(text);
        assert!(out.starts_with("共 2 条："), "{out}");
        assert!(out.ends_with(" 以上。"), "尾部丢了：{out}");
        assert!(out.contains("[2]{id}:"), "{out}");
    }

    #[test]
    fn 编不了就原样返回() {
        assert_eq!(try_toon_encode("普通文本"), "普通文本");
        assert_eq!(try_toon_encode("[不是 json]"), "[不是 json]");
        assert_eq!(try_toon_encode(""), "");
        // 异构数组原样。
        let text = r#"[{"a":1},{"b":2}]"#;
        assert_eq!(try_toon_encode(text), text);
    }

    #[test]
    fn 说明文本把格式讲全了() {
        // 用了 TOON 却不带这段，模型会把 `[2]{id}:` 当乱码或者猜错结构——
        // 那比多花几百个 token 糟得多。
        let s = toon_format_instructions();
        for 要点 in ["[N]", "{field", "数组长度", "列名", "逗号"] {
            assert!(s.contains(要点), "说明里缺 {要点}");
        }
    }
}
