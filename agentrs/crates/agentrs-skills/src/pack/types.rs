// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/types.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 去掉 `SkillSource` / `LoadedFrom`（"这个技能是从哪个目录发现的"归
//            Core，内核不做发现）；去掉 `skill_root`（同上）；`skill_type`/`skills`
//            两个未被读取的字段删掉；`EffortLevel` 落到本地而不是从 types crate
//            re-export（agentrs-types 没有 skill_types 模块）。

//! 技能内容包的字段。
//!
//! **只有解析，没有发现。** 技能文件在哪、哪个目录优先、装没装上，全归 Core
//! （见 crate 文档）。这里收到的是一段已经读出来的文本，产出的是一份结构化元数据。
//!
//! 字段名用连字符（`allowed-tools`）是因为技能文件是给人写的，YAML 里连字符
//! 比下划线常见。这层映射写在 serde 属性上，Rust 侧仍用蛇形。

use serde::{Deserialize, Serialize};

/// 推理强度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffortLevel {
    /// 最低。
    Low,
    /// 中等。
    Medium,
    /// 高。
    High,
    /// 最高。
    Max,
}

/// 技能在哪里执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionContext {
    /// 在当前 Run 内联执行。
    #[default]
    Inline,
    /// fork 一个新 Run 执行。
    Fork,
}

/// 一个字符串，或一列字符串。
///
/// 技能文件是人写的，`allowed-tools: Read` 与 `allowed-tools: [Read, Write]`
/// 都会出现。强制只收一种，用户写错时得到的是一句 YAML 报错而不是一个技能。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    /// 单个（可能是逗号分隔的）字符串。
    Single(String),
    /// 已经是一列。
    Multiple(Vec<String>),
}

/// 一个字符串，或一个整数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrNumber {
    /// `effort: high`
    Str(String),
    /// `effort: 2`
    Num(i64),
}

/// 一个布尔，或 `"true"` / `"false"` 字符串。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BoolOrString {
    /// `user-invocable: true`
    Bool(bool),
    /// `user-invocable: "true"`
    Str(String),
}

/// frontmatter 里的原始字段，YAML 反序列化的目标。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FrontmatterData {
    /// 展示名。
    pub name: Option<String>,
    /// 描述。
    pub description: Option<String>,
    /// 这个技能允许用哪些工具。**只能收窄，不能引入新能力。**
    #[serde(rename = "allowed-tools")]
    pub allowed_tools: Option<StringOrVec>,
    /// 参数提示，给人看的。
    #[serde(rename = "argument-hint")]
    pub argument_hint: Option<String>,
    /// 具名参数表。
    pub arguments: Option<StringOrVec>,
    /// 什么时候该用它。
    #[serde(rename = "when-to-use")]
    pub when_to_use: Option<String>,
    /// 版本。
    pub version: Option<String>,
    /// 指定模型；`inherit` 归一化为"不覆盖"。
    pub model: Option<String>,
    /// 推理强度。
    pub effort: Option<StringOrNumber>,
    /// `inline` 或 `fork`。
    pub context: Option<String>,
    /// 指定子 Agent。
    pub agent: Option<String>,
    /// 触发路径的 glob。
    pub paths: Option<StringOrVec>,
    /// 只支持 `bash`。
    pub shell: Option<String>,
    /// 人能不能直接调它。
    #[serde(rename = "user-invocable")]
    pub user_invocable: Option<BoolOrString>,
    /// 对模型隐藏。
    #[serde(rename = "hide-from-slash-command-tool")]
    pub hide_from_model_invocation: Option<BoolOrString>,
    /// hooks 原样保留，解析归 Core。
    pub hooks: Option<serde_yaml::Value>,
    // 上游此处有一句注释：不要用 `serde(flatten)` + HashMap 收未知字段，
    // serde_yaml 在那个组合上有已知缺陷。这里同样不收未知字段。
}

/// 一次解析的产物：frontmatter 与正文。
#[derive(Debug, Clone)]
pub struct ParsedMarkdown {
    /// 解析出来的 frontmatter。
    pub frontmatter: FrontmatterData,
    /// `---` 之后的正文。
    pub content: String,
}

/// 归一化之后的技能元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    /// 调用名，由 Core 解析文件位置后给定。
    pub name: String,
    /// 展示名。
    pub display_name: Option<String>,
    /// 描述。frontmatter 没写时退回正文首个非标题行。
    pub description: String,
    /// 描述是用户显式写的，还是从正文推出来的。
    ///
    /// 这个区分不是装饰：推出来的描述常常是半句话，Core 据此决定要不要
    /// 在列表里提示"这个技能没写描述"。
    pub has_user_specified_description: bool,
    /// 允许的工具。
    pub allowed_tools: Vec<String>,
    /// 参数提示。
    pub argument_hint: Option<String>,
    /// 具名参数。
    pub argument_names: Vec<String>,
    /// 什么时候该用它。
    pub when_to_use: Option<String>,
    /// 版本。
    pub version: Option<String>,
    /// `None` 表示不覆盖调用方的模型选择。
    pub model: Option<String>,
    /// 对模型隐藏。
    pub disable_model_invocation: bool,
    /// 人能不能直接调。
    pub user_invocable: bool,
    /// 内联还是 fork。
    pub execution_context: ExecutionContext,
    /// 指定子 Agent。
    pub agent: Option<String>,
    /// 推理强度。
    pub effort: Option<EffortLevel>,
    /// 只支持 `bash`。
    pub shell: Option<String>,
    /// 花括号展开之后的 glob。
    pub paths: Vec<String>,
    /// hooks 原样，解析归 Core。
    pub hooks_raw: Option<serde_json::Value>,
    /// 正文。
    pub content: String,
    /// 正文字符数。
    ///
    /// **不是 token 数**，只是个数量级参考。真正的计费口径在
    /// `agentrs-context::budget`。
    pub content_length: usize,
}
