// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/substitution.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 三遍正则改写为**一遍扫描**，因此去掉了 `regex` 依赖，
//            并顺带修掉一处缺陷：上游前一遍替换进去的值会被后一遍再扫一次，
//            于是一个内容里含 `$1` 的参数会被二次替换；
//            占位符前缀 `AIONRS_` 改为 `AGENTRS_`。

//! 技能正文里的参数替换。
//!
//! 一个技能是一份模板：`$1`、`$ARGUMENTS`、`$文件名` 这些占位符在调用时被换成
//! 实际参数。
//!
//! # 一遍扫描，不是三遍替换
//!
//! 上游按"具名 → 下标 → 简写 → 全量"跑四遍字符串替换。那样有一处不易发现的
//! 缺陷：**前一遍替换进去的值，会被后一遍当成模板再扫一次**。参数值里带一个
//! `$1` 就会被二次替换成另一个参数——而这在正常输入下几乎不会发生，
//! 一旦发生也很难看出是替换搞的。
//!
//! 一遍扫描没有这个问题：每个字符只被看一次，替换进去的内容直接进输出，
//! 不再参与匹配。

use super::types::SkillMetadata;

/// 技能目录占位符。
pub const SKILL_DIR: &str = "${AGENTRS_SKILL_DIR}";
/// 会话标识占位符。
pub const SESSION_ID: &str = "${AGENTRS_SESSION_ID}";

/// 把一串参数文本拆成一个个参数。
///
/// 认双引号与单引号，于是 `"hello world" foo` 拆成两个而不是三个——
/// 一个带空格的路径是最常见的参数，拆错了技能就拿不到它。
pub fn parse_arguments(args: &str) -> Vec<String> {
    if args.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let (mut in_double, mut in_single) = (false, false);

    for ch in args.chars() {
        match ch {
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            ' ' | '\t' if !in_double && !in_single => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// 替换技能正文里的全部占位符。
///
/// `args` 为 `None` 时只替换环境类占位符（`${AGENTRS_SKILL_DIR}` 等），
/// 参数类的原样留着——那时根本没有参数，把它们换成空串会让模板看起来像是
/// 少了几个词。
pub fn substitute(
    content: &str,
    args: Option<&str>,
    argument_names: &[String],
    skill_dir: Option<&str>,
    session_id: Option<&str>,
) -> String {
    let parsed = args.map(parse_arguments).unwrap_or_default();

    // 具名参数按**名字长度降序**匹配：`$file` 与 `$file_name` 同时存在时，
    // 先试短的会把 `$file_name` 匹配成 `$file` 再跟一个 `_name`。
    let mut named: Vec<(&str, &str)> = argument_names
        .iter()
        .enumerate()
        // 纯数字的名字与 `$0`/`$1` 简写冲突，跳过。
        .filter(|(_, n)| !n.is_empty() && !n.chars().all(|c| c.is_ascii_digit()))
        .map(|(i, n)| (n.as_str(), parsed.get(i).map(String::as_str).unwrap_or("")))
        .collect();
    named.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));

    let chars: Vec<char> = content.chars().collect();
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    let mut 替换过 = false;

    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let rest: String = chars[i..].iter().collect();

        // ---- ${AGENTRS_*} ----
        if let Some(dir) = skill_dir {
            if rest.starts_with(SKILL_DIR) {
                out.push_str(dir);
                i += SKILL_DIR.chars().count();
                替换过 = true;
                continue;
            }
        }
        if let Some(sid) = session_id {
            if rest.starts_with(SESSION_ID) {
                out.push_str(sid);
                i += SESSION_ID.chars().count();
                替换过 = true;
                continue;
            }
        }

        // 没有参数就不动参数类占位符。
        if args.is_none() {
            out.push('$');
            i += 1;
            continue;
        }

        // ---- $ARGUMENTS[n] ----
        if let Some((idx, len)) = 解下标(&chars, i) {
            out.push_str(parsed.get(idx).map(String::as_str).unwrap_or(""));
            i += len;
            替换过 = true;
            continue;
        }

        // ---- $ARGUMENTS ----
        if rest.starts_with("$ARGUMENTS") && !后接单词字符(&chars, i + "$ARGUMENTS".len()) {
            out.push_str(args.unwrap_or(""));
            i += "$ARGUMENTS".chars().count();
            替换过 = true;
            continue;
        }

        // ---- $具名 ----
        if let Some((name, value)) = named
            .iter()
            .find(|(n, _)| rest[1..].starts_with(*n) && !后接单词字符(&chars, i + 1 + n.chars().count()))
        {
            out.push_str(value);
            i += 1 + name.chars().count();
            替换过 = true;
            continue;
        }

        // ---- $n ----
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j > i + 1 && !后接单词字符(&chars, j) {
            let idx: usize = chars[i + 1..j].iter().collect::<String>().parse().unwrap_or(usize::MAX);
            out.push_str(parsed.get(idx).map(String::as_str).unwrap_or(""));
            i = j;
            替换过 = true;
            continue;
        }

        out.push('$');
        i += 1;
    }

    // 模板里一个占位符也没有，但调用方给了参数——把参数附在末尾，
    // 否则那些参数就凭空消失了，而使用者会以为技能收到了它们。
    if let Some(a) = args {
        if !替换过 && !a.is_empty() {
            out.push_str(&format!("\n\nARGUMENTS: {a}"));
        }
    }
    out
}

/// `$ARGUMENTS[12]` → `Some((12, 该跳过几个字符))`。
fn 解下标(chars: &[char], at: usize) -> Option<(usize, usize)> {
    const HEAD: &str = "$ARGUMENTS[";
    let head: Vec<char> = HEAD.chars().collect();
    if chars.len() < at + head.len() || chars[at..at + head.len()] != head[..] {
        return None;
    }
    let mut j = at + head.len();
    let start = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if j == start || chars.get(j) != Some(&']') {
        return None;
    }
    let idx = chars[start..j].iter().collect::<String>().parse().ok()?;
    Some((idx, j + 1 - at))
}

/// 紧接着的是不是单词字符（或 `[`）？
///
/// `$file` 不该匹配 `$filename` 的前半截，也不该匹配 `$file[0]`。
fn 后接单词字符(chars: &[char], at: usize) -> bool {
    matches!(chars.get(at), Some(c) if c.is_alphanumeric() || *c == '_' || *c == '[')
}

/// 对一个技能做替换，参数名取自它的声明。
pub fn substitute_for(
    skill: &SkillMetadata,
    args: Option<&str>,
    skill_dir: Option<&str>,
    session_id: Option<&str>,
) -> String {
    substitute(
        &skill.content,
        args,
        &skill.argument_names,
        skill_dir,
        session_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 名字(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn 替换(content: &str, args: &str, names: &[&str]) -> String {
        substitute(content, Some(args), &名字(names), None, None)
    }

    #[test]
    fn 按空白拆参数() {
        assert_eq!(parse_arguments("a b c"), ["a", "b", "c"]);
        assert_eq!(parse_arguments("   "), Vec::<String>::new());
        assert_eq!(parse_arguments(""), Vec::<String>::new());
    }

    #[test]
    fn 引号里的空格不拆() {
        // 一个带空格的路径是最常见的参数，拆错了技能就拿不到它。
        assert_eq!(parse_arguments(r#""hello world" foo"#), ["hello world", "foo"]);
        assert_eq!(parse_arguments("'a b' c"), ["a b", "c"]);
        // 另一种引号在引号里是普通字符。
        assert_eq!(parse_arguments(r#""it's here""#), ["it's here"]);
    }

    #[test]
    fn 简写下标() {
        assert_eq!(替换("跑 $0 与 $1", "甲 乙", &[]), "跑 甲 与 乙");
        // 越界的换成空串——报错会让一个可选参数变成硬错误。
        assert_eq!(替换("$0/$5", "甲", &[]), "甲/");
    }

    #[test]
    fn 全量与带下标的_arguments() {
        assert_eq!(替换("全部：$ARGUMENTS", "甲 乙", &[]), "全部：甲 乙");
        assert_eq!(替换("$ARGUMENTS[1]", "甲 乙", &[]), "乙");
        assert_eq!(替换("$ARGUMENTS[9]", "甲", &[]), "");
    }

    #[test]
    fn 具名参数() {
        assert_eq!(替换("改 $file", "a.rs", &["file"]), "改 a.rs");
        assert_eq!(
            替换("$from → $to", "旧 新", &["from", "to"]),
            "旧 → 新"
        );
    }

    #[test]
    fn 具名参数不吃掉更长的名字() {
        // 先试短的会把 `$file_name` 匹配成 `$file` 再跟一个 `_name`。
        let out = substitute(
            "$file 与 $file_name",
            Some("短 长"),
            &名字(&["file", "file_name"]),
            None,
            None,
        );
        assert_eq!(out, "短 与 长");
    }

    #[test]
    fn 名字后面紧跟单词字符时不替换() {
        // `$file` 不该匹配 `$filename` 的前半截。一个也没替换成，
        // 于是兜底把参数附在末尾——参数不能凭空消失。
        assert_eq!(替换("$filename", "x", &["file"]), "$filename\n\nARGUMENTS: x");
    }

    #[test]
    fn 替换进去的值不会被二次扫描() {
        // 这是三遍替换的那处缺陷：参数值里带 `$1` 会被后一遍再换一次。
        // 一遍扫描下，替换进去的内容直接进输出，不再参与匹配。
        let out = 替换("$0 然后 $1", "$1 乙", &[]);
        assert_eq!(out, "$1 然后 乙");
    }

    #[test]
    fn 环境占位符() {
        let out = substitute(
            "在 ${AGENTRS_SKILL_DIR} 里，会话 ${AGENTRS_SESSION_ID}",
            None,
            &[],
            Some("/skills/x"),
            Some("run-7"),
        );
        assert_eq!(out, "在 /skills/x 里，会话 run-7");
    }

    #[test]
    fn 没有参数时不动参数类占位符() {
        // 换成空串会让模板看起来像是少了几个词，而使用者不知道是没传参数。
        let out = substitute("跑 $1 与 $ARGUMENTS", None, &[], None, None);
        assert_eq!(out, "跑 $1 与 $ARGUMENTS");
    }

    #[test]
    fn 模板里没有占位符时参数附在末尾() {
        // 否则那些参数凭空消失，而使用者会以为技能收到了它们。
        let out = 替换("就是一段提示", "甲 乙", &[]);
        assert_eq!(out, "就是一段提示\n\nARGUMENTS: 甲 乙");
    }

    #[test]
    fn 有占位符时不再附加() {
        assert_eq!(替换("用 $0", "甲", &[]), "用 甲");
    }

    #[test]
    fn 参数为空串时不附加() {
        assert_eq!(替换("提示", "", &[]), "提示");
    }

    #[test]
    fn 纯数字的具名参数被跳过() {
        // 它与 $0/$1 简写冲突，两条规则会互相打架。
        assert_eq!(替换("$0", "甲 乙", &["0"]), "甲");
    }

    #[test]
    fn 落单的美元符号原样留着() {
        // `$` 与未声明的 `$x` 都不是占位符，原样留着；没替换成任何东西，
        // 所以参数走兜底附在末尾。
        assert_eq!(替换("价格 $ 与 $x", "甲", &[]), "价格 $ 与 $x\n\nARGUMENTS: 甲");
        // 没给参数时连兜底也不做。
        assert_eq!(substitute("价格 $ 与 $x", None, &[], None, None), "价格 $ 与 $x");
    }

    #[test]
    fn 中文与多字节内容不会切错() {
        let out = 替换("把 $0 改成 $1，完成", "旧内容 新内容", &[]);
        assert_eq!(out, "把 旧内容 改成 新内容，完成");
    }

    #[test]
    fn 对一个技能做替换() {
        let (p, _) = super::super::parse_pack("---\narguments: [file]\n---\n改 $file 这个文件");
        let s = super::super::parse_skill_fields(&p.frontmatter, &p.content, "edit");
        assert_eq!(substitute_for(&s, Some("a.rs"), None, None), "改 a.rs 这个文件");
    }
}
