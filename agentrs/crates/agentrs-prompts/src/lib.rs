//! AgentRS `prompts` —— 版本化提示词（架构 §10 末段）。
//!
//! > 系统提示、子 Agent 提示、压缩提示和 tool result 提示均放入
//! > `agentrs-prompts`，具备版本号、输入 schema、snapshot fixture 和安全测试。
//! > **禁止将产品文案、数据库路径、UI 控制逻辑嵌入 prompt。**
//!
//! ## 提示词是代码，不是配置
//!
//! 改一句提示词就是改一次行为，而且是**没有类型系统兜底**的那种改动。
//! 所以它需要与代码同等的对待：
//!
//! | 措施 | 防什么 |
//! |---|---|
//! | 版本号 + 内容摘要 | "这一轮用的哪版提示词"必须能从 trajectory 回答 |
//! | 输入 schema | 模板占位符与实际传参对不上时**当场失败**，而不是渲染出一句半截话 |
//! | snapshot fixture | 提示词改动在 review 里可见——否则它悄悄混在一次重构里 |
//! | 安全 lint | 产品文案、路径、密钥、UI 逻辑不该进提示词 |
//!
//! ## 为什么 lint 里这几条是硬禁
//!
//! - **绝对路径**：提示词会随 bundle 导出，路径就跟着漏出去；
//!   而且换台机器它就是错的。
//! - **数据库连接串 / 密钥**：同上，且后果更严重。
//! - **产品文案与 UI 控制逻辑**：它们属于 Core。混进提示词意味着
//!   改一句界面用语要动内核，而内核改动要重跑全部 eval。

#![forbid(unsafe_code)]

/// Plan 模式的提示词（自 aionrs 移植）。
pub mod plan_prompt;
pub mod lint;
pub mod registry;

pub use lint::{audit, Finding, Violation};
pub use registry::{all, by_id};

use std::collections::BTreeMap;

/// 一份提示词。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// 稳定标识。**改内容不改 id**，改 id 等于换了一份提示词。
    pub id: &'static str,
    /// 版本。**内容一变就必须 +1**——这是 trajectory 能回答
    /// "这一轮用的哪版"的前提。
    pub version: u32,
    /// 模板。占位符写作 `{名字}`。
    pub template: &'static str,
    /// 必填占位符。渲染时逐个检查。
    pub inputs: &'static [&'static str],
}

/// 渲染失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// 缺少必填输入。
    ///
    /// **当场失败而不是留个空洞**：一句半截的提示词比一句没有的提示词更糟——
    /// 模型会照着那半句去猜。
    #[error("prompt {id} missing input: {missing:?}")]
    MissingInput {
        /// 提示词标识。
        id: &'static str,
        /// 缺的占位符。
        missing: Vec<String>,
    },
    /// 传了模板里没有的输入。
    ///
    /// 多半是改模板时忘了改调用方，或反之。**静默忽略会让人以为参数生效了。**
    #[error("prompt {id} got unknown input: {unknown:?}")]
    UnknownInput {
        /// 提示词标识。
        id: &'static str,
        /// 多余的键。
        unknown: Vec<String>,
    },
}

impl Prompt {
    /// 渲染。
    pub fn render(&self, inputs: &BTreeMap<&str, String>) -> Result<String, RenderError> {
        let missing: Vec<String> = self
            .inputs
            .iter()
            .filter(|k| !inputs.contains_key(*k))
            .map(|k| (*k).to_owned())
            .collect();
        if !missing.is_empty() {
            return Err(RenderError::MissingInput { id: self.id, missing });
        }

        let unknown: Vec<String> = inputs
            .keys()
            .filter(|k| !self.inputs.contains(k))
            .map(|k| (*k).to_owned())
            .collect();
        if !unknown.is_empty() {
            return Err(RenderError::UnknownInput { id: self.id, unknown });
        }

        let mut out = self.template.to_string();
        for (k, v) in inputs {
            out = out.replace(&format!("{{{k}}}"), v);
        }
        Ok(out)
    }

    /// 无输入的提示词直接取正文。
    pub fn render_static(&self) -> Result<String, RenderError> {
        self.render(&BTreeMap::new())
    }

    /// 内容摘要，进 manifest。
    ///
    /// **与版本号一起用**：版本号靠人维护、会忘记加；摘要不会。
    /// 两者不一致时以摘要为准，并说明有人改了内容却没升版本。
    pub fn digest(&self) -> agentrs_contracts::ids::Digest {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.template.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        agentrs_contracts::ids::Digest::from_hex(format!("{h:016x}"))
    }

    /// 供 manifest 引用的形态。
    pub fn as_ref(&self) -> PromptRef {
        PromptRef {
            id: self.id,
            version: self.version,
            digest: self.digest(),
        }
    }

    /// 模板里实际出现的占位符。
    ///
    /// 与 [`inputs`](Self::inputs) 对照即可发现"声明了却没用"
    /// 与"用了却没声明"两类错误。
    pub fn placeholders(&self) -> Vec<String> {
        let mut out = Vec::new();
        let b = self.template.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'{' {
                // `{{` 是转义，跳过。
                if b.get(i + 1) == Some(&b'{') {
                    i += 2;
                    continue;
                }
                if let Some(end) = self.template[i..].find('}') {
                    let name = &self.template[i + 1..i + end];
                    if !name.is_empty() && !name.contains(char::is_whitespace) {
                        out.push(name.to_owned());
                    }
                    i += end + 1;
                    continue;
                }
            }
            i += 1;
        }
        out.sort();
        out.dedup();
        out
    }
}

/// 提示词在 manifest 里的引用。
///
/// **必须同时带 version 与 digest**：没有它们，"这一轮为什么模型行为变了"
/// 只能回答到"用了提示词 X"，而 X 的内容是会变的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRef {
    /// 标识。
    pub id: &'static str,
    /// 版本。
    pub version: u32,
    /// 内容摘要。
    pub digest: agentrs_contracts::ids::Digest,
}
