// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/frontmatter.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 解析失败改为返回 `ParseOutcome` 而不是记 warning 后吞掉；
//            去掉发现相关的参数（source/loaded_from/skill_root，归 Core）；
//            let-chain 改嵌套 if（edition 2021）；花括号展开加上限，
//            防一份手写的 `{a,b}{c,d}{e,f}…` 把内存吃干净。

use super::types::{
    BoolOrString, EffortLevel, ExecutionContext, FrontmatterData, ParsedMarkdown, SkillMetadata,
    StringOrNumber, StringOrVec,
};

/// 花括号展开最多产出多少条 glob。
///
/// `{a,b}` 每多一组就翻一倍。一份手写的技能文件里连写十组就是 1024 条，
/// 二十组是一百万条——这不该由一个笔误决定。超了就退回原样，
/// 让它当一条普通 glob 去匹配（多半匹配不上，而那是看得见的失败）。
const MAX_BRACE_EXPANSION: usize = 256;

/// 一次解析的结果。
///
/// 三态而不是 `Result`：**"YAML 写坏了但正文还在"是一种真实且常见的中间态**。
/// 上游把它和成功并成一个，代价是技能"装上了"却什么也不做，而使用者只看到
/// 它不生效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// frontmatter 解析成功。
    Ok,
    /// 第一遍解析失败，自动补引号之后成功。
    ///
    /// 带上原因是为了让 Core 能提示用户改文件——补引号是权宜之计，
    /// 不是长久之计。
    Recovered(String),
    /// 两遍都失败，frontmatter 当空处理，正文仍然可用。
    Failed(String),
}

impl ParseOutcome {
    /// 有没有需要告诉用户的问题。
    pub fn needs_attention(&self) -> bool {
        !matches!(self, Self::Ok)
    }

    /// 给人看的一句话。
    pub fn describe(&self) -> Option<String> {
        match self {
            Self::Ok => None,
            Self::Recovered(why) => Some(format!(
                "frontmatter 里有 YAML 特殊字符，已自动补引号解析成功；建议手动加引号：{why}"
            )),
            Self::Failed(why) => Some(format!(
                "frontmatter 解析失败，本技能的全部声明字段都被忽略（正文仍可用）：{why}"
            )),
        }
    }
}

/// 从一份 Markdown 里解析 frontmatter 与正文。
///
/// 用字符串查找定位 `---` 围栏，不用正则。找不到围栏时整份内容都当正文——
/// 一个没写 frontmatter 的技能仍是一个合法的技能。
pub fn parse_pack(input: &str) -> (ParsedMarkdown, ParseOutcome) {
    match extract_frontmatter_bounds(input) {
        Some((yaml_text, content)) => {
            let (frontmatter, outcome) = parse_yaml_with_fallback(yaml_text);
            (
                ParsedMarkdown {
                    frontmatter,
                    content: content.to_owned(),
                },
                outcome,
            )
        }
        None => (
            ParsedMarkdown {
                frontmatter: FrontmatterData::default(),
                content: input.to_owned(),
            },
            ParseOutcome::Ok,
        ),
    }
}

/// 把原始 frontmatter 归一化成元数据。
///
/// `resolved_name` 由 Core 给定——它由文件位置决定，而位置归 Core。
pub fn parse_skill_fields(
    frontmatter: &FrontmatterData,
    content: &str,
    resolved_name: &str,
) -> SkillMetadata {
    let 用户写的描述 = coerce_description(&frontmatter.description);
    let has_user_specified_description = 用户写的描述.is_some();

    // frontmatter 没写描述就退回正文的第一个非标题行。半句话总比空强——
    // 模型至少能据此判断这个技能大概是干什么的。
    let description = 用户写的描述
        .or_else(|| extract_description_from_content(content))
        .unwrap_or_default();

    let execution_context = match frontmatter.context.as_deref() {
        Some("fork") => ExecutionContext::Fork,
        _ => ExecutionContext::Inline,
    };

    SkillMetadata {
        name: resolved_name.to_owned(),
        display_name: frontmatter.name.clone(),
        description,
        has_user_specified_description,
        allowed_tools: parse_string_or_vec(&frontmatter.allowed_tools),
        argument_hint: frontmatter.argument_hint.clone(),
        argument_names: parse_string_or_vec(&frontmatter.arguments),
        when_to_use: frontmatter.when_to_use.clone(),
        version: frontmatter.version.clone(),
        // `inherit` 的意思是"别覆盖调用方的选择"，归一化成 None 之后
        // 下游就不必到处判断那个字符串。
        model: frontmatter
            .model
            .as_deref()
            .filter(|m| *m != "inherit")
            .map(str::to_owned),
        disable_model_invocation: parse_bool(&frontmatter.hide_from_model_invocation, false),
        user_invocable: parse_bool(&frontmatter.user_invocable, true),
        execution_context,
        agent: frontmatter.agent.clone(),
        effort: parse_effort(&frontmatter.effort),
        shell: frontmatter.shell.clone(),
        paths: split_paths(&frontmatter.paths),
        hooks_raw: frontmatter.hooks.as_ref().and_then(yaml_value_to_json),
        content: content.to_owned(),
        content_length: content.chars().count(),
    }
}

/// 定位 `---` 围栏，返回 (yaml 文本, 正文)。
///
/// 开围栏必须是第一行。闭围栏是**整行恰好等于 `---`** 的那一行——
/// 按"包含 `---`"判会被正文里的水平分割线骗到。
fn extract_frontmatter_bounds(input: &str) -> Option<(&str, &str)> {
    let after_open = input
        .strip_prefix("---\n")
        .or_else(|| input.strip_prefix("---\r\n"))?;

    let mut pos = 0;
    for line in after_open.lines() {
        let 本行长度 = {
            let raw = &after_open[pos..];
            let n = line.len();
            if raw[n..].starts_with("\r\n") {
                n + 2
            } else if raw[n..].starts_with('\n') {
                n + 1
            } else {
                n // 最后一行没有换行
            }
        };

        if line == "---" {
            let yaml_text = &after_open[..pos];
            let yaml_text = yaml_text.strip_suffix('\n').unwrap_or(yaml_text);
            let body_start = pos + 本行长度;
            let body = after_open.get(body_start..).unwrap_or("");
            return Some((yaml_text, body));
        }
        pos += 本行长度;
    }
    None
}

/// 两遍解析：原样一遍，自动补引号一遍。
fn parse_yaml_with_fallback(yaml_text: &str) -> (FrontmatterData, ParseOutcome) {
    let 第一遍 = match serde_yaml::from_str::<FrontmatterData>(yaml_text) {
        Ok(data) => return (data, ParseOutcome::Ok),
        Err(e) => e.to_string(),
    };
    match serde_yaml::from_str::<FrontmatterData>(&quote_problematic_values(yaml_text)) {
        Ok(data) => (data, ParseOutcome::Recovered(第一遍)),
        // 两遍都失败：frontmatter 当空，但**正文照样交出去**，
        // 而且失败原因要能传到用户面前。
        Err(e) => (FrontmatterData::default(), ParseOutcome::Failed(e.to_string())),
    }
}

/// 给含 YAML 特殊字符的顶层标量值补上引号。
///
/// 只动顶层的 `key: value`：缩进过的行属于嵌套结构（比如 hooks 块），
/// 给它们补引号会把结构本身改坏。
fn quote_problematic_values(yaml_text: &str) -> String {
    const SPECIAL: &[char] = &['{', '}', '[', ']', '*', '&', '#', '!', '|', '>', '%', '@', '`'];

    let mut result = String::with_capacity(yaml_text.len() + 64);
    for line in yaml_text.lines() {
        let mut 照抄 = true;
        if !line.starts_with(' ') && !line.starts_with('\t') {
            if let Some(colon) = line.find(": ") {
                let key = &line[..colon + 1];
                let value = &line[colon + 2..];
                let 已引 = value.starts_with('"') || value.starts_with('\'');
                if !value.is_empty() && !已引 && value.contains(SPECIAL) {
                    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
                    result.push_str(key);
                    result.push_str(" \"");
                    result.push_str(&escaped);
                    result.push('"');
                    result.push('\n');
                    照抄 = false;
                }
            }
        }
        if 照抄 {
            result.push_str(line);
            result.push('\n');
        }
    }
    if result.ends_with('\n') && !yaml_text.ends_with('\n') {
        result.pop();
    }
    result
}

fn yaml_value_to_json(v: &serde_yaml::Value) -> Option<serde_json::Value> {
    serde_json::to_string(v)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// 单个字符串按逗号拆开；已经是列表就原样。
fn parse_string_or_vec(value: &Option<StringOrVec>) -> Vec<String> {
    match value {
        None => vec![],
        Some(StringOrVec::Multiple(v)) => v.clone(),
        Some(StringOrVec::Single(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}

/// `paths` 先按顶层逗号拆，再逐条做花括号展开。
fn split_paths(value: &Option<StringOrVec>) -> Vec<String> {
    match value {
        None => vec![],
        Some(StringOrVec::Multiple(v)) => v.iter().flat_map(|p| expand_braces(p)).collect(),
        Some(StringOrVec::Single(s)) => split_respecting_braces(s)
            .into_iter()
            .flat_map(|p| expand_braces(&p))
            .collect(),
    }
}

/// 按**不在花括号内**的逗号拆分。
///
/// `*.{ts,tsx},*.rs` 要拆成两条而不是三条——花括号里那个逗号是花括号的。
fn split_respecting_braces(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth: usize = 0;

    for ch in s.chars() {
        match ch {
            '{' => {
                depth += 1;
                current.push(ch);
            }
            '}' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ',' if depth == 0 => {
                let t = current.trim().to_owned();
                if !t.is_empty() {
                    parts.push(t);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let t = current.trim().to_owned();
    if !t.is_empty() {
        parts.push(t);
    }
    parts
}

/// 花括号展开：`*.{ts,tsx}` → `["*.ts", "*.tsx"]`。
///
/// 超过 [`MAX_BRACE_EXPANSION`] 条就退回原样。每多一组花括号，条数翻倍——
/// 连写二十组是一百万条，那不该由一个笔误决定。
fn expand_braces(pattern: &str) -> Vec<String> {
    let mut out = Vec::new();
    expand_into(pattern, &mut out);
    if out.len() > MAX_BRACE_EXPANSION {
        return vec![pattern.to_owned()];
    }
    out
}

fn expand_into(pattern: &str, out: &mut Vec<String>) {
    if out.len() > MAX_BRACE_EXPANSION {
        return;
    }
    if let Some(open) = pattern.find('{') {
        if let Some(close_rel) = pattern[open..].find('}') {
            let close = open + close_rel;
            let prefix = &pattern[..open];
            let suffix = &pattern[close + 1..];
            for alt in pattern[open + 1..close].split(',') {
                expand_into(&format!("{prefix}{alt}{suffix}"), out);
            }
            return;
        }
    }
    out.push(pattern.to_owned());
}

fn parse_bool(value: &Option<BoolOrString>, default: bool) -> bool {
    match value {
        None => default,
        Some(BoolOrString::Bool(b)) => *b,
        Some(BoolOrString::Str(s)) => s.eq_ignore_ascii_case("true"),
    }
}

fn parse_effort(value: &Option<StringOrNumber>) -> Option<EffortLevel> {
    match value {
        None => None,
        Some(StringOrNumber::Num(n)) => Some(match n {
            0 => EffortLevel::Low,
            1 => EffortLevel::Medium,
            2 => EffortLevel::High,
            _ => EffortLevel::Max,
        }),
        Some(StringOrNumber::Str(s)) => match s.to_lowercase().as_str() {
            "low" => Some(EffortLevel::Low),
            "medium" | "normal" => Some(EffortLevel::Medium),
            "high" => Some(EffortLevel::High),
            "max" | "maximum" => Some(EffortLevel::Max),
            // 认不出来就当没写。猜一个会让技能悄悄以另一个强度跑。
            _ => None,
        },
    }
}

/// 正文里第一个非空、非标题的行。
fn extract_description_from_content(content: &str) -> Option<String> {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
}

fn coerce_description(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 解析(input: &str) -> (SkillMetadata, ParseOutcome) {
        let (parsed, outcome) = parse_pack(input);
        (
            parse_skill_fields(&parsed.frontmatter, &parsed.content, "test-skill"),
            outcome,
        )
    }

    #[test]
    fn 解析出_frontmatter_与正文() {
        let (m, o) = 解析("---\nname: 我的技能\ndescription: 做一件事\n---\n正文第一行\n");
        assert_eq!(o, ParseOutcome::Ok);
        assert_eq!(m.display_name.as_deref(), Some("我的技能"));
        assert_eq!(m.description, "做一件事");
        assert!(m.has_user_specified_description);
        assert_eq!(m.content, "正文第一行\n");
    }

    #[test]
    fn 没有_frontmatter_时整份都是正文() {
        // 一个没写 frontmatter 的技能仍是一个合法的技能。
        let (m, o) = 解析("# 标题\n就是一段正文\n");
        assert_eq!(o, ParseOutcome::Ok);
        assert_eq!(m.content, "# 标题\n就是一段正文\n");
        // 描述退回正文首个非标题行。
        assert_eq!(m.description, "就是一段正文");
        assert!(!m.has_user_specified_description, "这是推出来的，不是用户写的");
    }

    #[test]
    fn 正文里的水平分割线不会被当成闭围栏() {
        // 按"包含 ---"判会在这里截断，正文丢掉一半。
        let input = "---\nname: x\n---\n第一段\n\n---\n\n第二段\n";
        let (m, _) = 解析(input);
        assert!(m.content.contains("第二段"), "正文被截断了：{:?}", m.content);
    }

    #[test]
    fn crlf_换行也认() {
        let (m, o) = 解析("---\r\nname: x\r\n---\r\n正文\r\n");
        assert_eq!(o, ParseOutcome::Ok);
        assert_eq!(m.display_name.as_deref(), Some("x"));
        assert!(m.content.contains("正文"));
    }

    #[test]
    fn 空_frontmatter_不会崩() {
        let (m, o) = 解析("---\n---\n正文\n");
        assert_eq!(o, ParseOutcome::Ok);
        assert_eq!(m.content, "正文\n");
    }

    #[test]
    fn 单值与列表两种写法都收() {
        // 技能文件是人写的，两种写法都会出现。只收一种，用户写错时
        // 得到的是一句 YAML 报错而不是一个技能。
        let (a, _) = 解析("---\nallowed-tools: Read\n---\n");
        assert_eq!(a.allowed_tools, ["Read"]);
        let (b, _) = 解析("---\nallowed-tools: Read, Write\n---\n");
        assert_eq!(b.allowed_tools, ["Read", "Write"]);
        let (c, _) = 解析("---\nallowed-tools: [Read, Write]\n---\n");
        assert_eq!(c.allowed_tools, ["Read", "Write"]);
    }

    #[test]
    fn inherit_归一化成不覆盖() {
        // 归一化成 None，下游就不必到处判断那个字符串。
        let (a, _) = 解析("---\nmodel: inherit\n---\n");
        assert_eq!(a.model, None);
        let (b, _) = 解析("---\nmodel: claude-opus-4\n---\n");
        assert_eq!(b.model.as_deref(), Some("claude-opus-4"));
    }

    #[test]
    fn 强度数字与文字都认_认不出的当没写() {
        assert_eq!(解析("---\neffort: high\n---\n").0.effort, Some(EffortLevel::High));
        assert_eq!(解析("---\neffort: 0\n---\n").0.effort, Some(EffortLevel::Low));
        assert_eq!(解析("---\neffort: normal\n---\n").0.effort, Some(EffortLevel::Medium));
        // 猜一个会让技能悄悄以另一个强度跑。
        assert_eq!(解析("---\neffort: 飞快\n---\n").0.effort, None);
    }

    #[test]
    fn 布尔的两种写法都认且默认值正确() {
        // fail-open 的方向在这里是对的：没写 user-invocable 的技能应当可以被人调。
        assert!(解析("---\nname: x\n---\n").0.user_invocable);
        assert!(!解析("---\nuser-invocable: false\n---\n").0.user_invocable);
        assert!(!解析("---\nuser-invocable: \"false\"\n---\n").0.user_invocable);
        // 对模型隐藏则默认不隐藏。
        assert!(!解析("---\nname: x\n---\n").0.disable_model_invocation);
    }

    #[test]
    fn 花括号展开() {
        let (m, _) = 解析("---\npaths: \"*.{ts,tsx}\"\n---\n");
        assert_eq!(m.paths, ["*.ts", "*.tsx"]);
        let (m, _) = 解析("---\npaths: \"{a,b}/{c,d}\"\n---\n");
        assert_eq!(m.paths, ["a/c", "a/d", "b/c", "b/d"]);
    }

    #[test]
    fn 花括号里的逗号不算分隔符() {
        // 拆成三条的话，`*.{ts` 与 `tsx}` 两条谁也匹配不上。
        let (m, _) = 解析("---\npaths: \"*.{ts,tsx},*.rs\"\n---\n");
        assert_eq!(m.paths, ["*.ts", "*.tsx", "*.rs"]);
    }

    #[test]
    fn 花括号展开有上限() {
        // 每多一组就翻倍，连写二十组是一百万条。这不该由一个笔误决定。
        let 炸弹 = "{a,b}".repeat(20);
        let (m, _) = 解析(&format!("---\npaths: \"{炸弹}\"\n---\n"));
        assert_eq!(m.paths.len(), 1, "应当退回原样");
        assert_eq!(m.paths[0], 炸弹);
    }

    #[test]
    fn 特殊字符在句中不影响解析() {
        // YAML 只在特殊字符**开头**时才把值当成结构。句中的花括号是普通字符，
        // 第一遍就过得去——不该为它触发补引号那条退路。
        let (m, o) = 解析("---\ndescription: 用 {占位} 包起来\n---\n正文\n");
        assert_eq!(o, ParseOutcome::Ok);
        assert_eq!(m.description, "用 {占位} 包起来");
    }

    #[test]
    fn 特殊字符开头靠补引号救回来_并且告诉调用方() {
        // `description: {占位} 开头` 是合法的人类写法、非法的 YAML——
        // 开头的 `{` 会被当成 flow mapping。
        let (m, o) = 解析("---\ndescription: {占位} 开头\n---\n正文\n");
        assert_eq!(m.description, "{占位} 开头");
        // 救回来了，但要说出来——补引号是权宜之计，不是长久之计。
        assert!(matches!(o, ParseOutcome::Recovered(_)), "{o:?}");
        assert!(o.needs_attention());
        assert!(o.describe().unwrap().contains("建议手动加引号"));
    }

    #[test]
    fn 两遍都失败时正文仍然交出去且失败可见() {
        // 上游在这里记一条 warning 然后返回空 frontmatter——技能"装上了"
        // 却什么也不做，而使用者只看到它不生效。
        let (m, o) = 解析("---\n  乱\n 缩\n进: [\n---\n正文还在\n");
        assert!(matches!(o, ParseOutcome::Failed(_)), "{o:?}");
        assert!(o.describe().unwrap().contains("全部声明字段都被忽略"));
        assert_eq!(m.content, "正文还在\n");
    }

    #[test]
    fn 缩进过的行不会被补引号弄坏() {
        // 给嵌套结构补引号会把结构本身改坏。
        let yaml = "name: x\nhooks:\n  pre: [a, b]\n";
        assert_eq!(quote_problematic_values(yaml), yaml);
    }

    #[test]
    fn 已经加过引号的值不会被二次加引号() {
        let yaml = "description: \"已经引过 {了}\"\n";
        assert_eq!(quote_problematic_values(yaml), yaml);
    }

    #[test]
    fn fork_与_inline() {
        assert_eq!(
            解析("---\ncontext: fork\n---\n").0.execution_context,
            ExecutionContext::Fork
        );
        // 认不出来的一律 inline——fork 会开一个新 Run，那是更大的动作，
        // 不该因为拼错一个词就发生。
        assert_eq!(
            解析("---\ncontext: 随便\n---\n").0.execution_context,
            ExecutionContext::Inline
        );
        assert_eq!(
            解析("---\nname: x\n---\n").0.execution_context,
            ExecutionContext::Inline
        );
    }

    #[test]
    fn hooks_原样保留() {
        // 解析归 Core，这里只负责不把它丢掉。
        let (m, _) = 解析("---\nhooks:\n  pre-run: [check]\n---\n");
        let hooks = m.hooks_raw.expect("hooks 丢了");
        assert_eq!(hooks["pre-run"][0], "check");
    }

    #[test]
    fn 正文长度按字符数不按字节数() {
        // 按字节数算，一份中文技能会被高估三倍。它只是个数量级参考，
        // 但高估三倍的参考不如没有。
        let (m, _) = 解析("---\nname: x\n---\n中文四个字");
        assert_eq!(m.content_length, 5);
    }
}
