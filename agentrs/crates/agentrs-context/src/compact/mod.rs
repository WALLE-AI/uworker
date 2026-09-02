// Ported from aionrs (Apache-2.0), crates/aion-compact.
//   Source: crates/aion-compact/src/{lib,api}.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 合并 lib.rs 与 api.rs 为一个模块；补上"哪一级会改变内容、
//            因此不能用在哪"的判据说明。

//! 工具输出压缩。
//!
//! 工具输出是上下文里**最容易失控**的一段：它由外部程序决定长度，不由模型也不由
//! 我们决定。一条 `cargo build` 能吐出几千行，其中大半是颜色转义、被回车反复重画的
//! 进度条、以及几百行同构的 `Compiling …`。
//!
//! 这与 [`crate::compaction`] 不是一回事：那个压的是**对话历史**（旧的轮次），
//! 这个压的是**单次工具输出**，发生在它进入上下文之前。
//!
//! # 分级，因为不同输出经得起的处理不一样
//!
//! - [`CompactLevel::Safe`]（默认）**无损**：去 ANSI、折叠回车覆盖、去行尾空白、
//!   合并连续空行。改变的只是终端曾经怎么把它画出来。
//! - [`CompactLevel::Full`] **有损**：再折叠相似行、重排 JSON。
//!
//! **`Full` 绝不能用在 `Read` 的输出上。** 折叠相似行会改变内容，而
//! "整份读回逐字节一致"是 `Read` 的硬承诺——基于一份被折叠过的内容做 Edit，
//! 会把文件改成谁也没要求的样子，而且全程没有任何一步报错。
//!
//! [`toon`] 单独一档，不进 [`compact_output`]：它换的是**编码方式**，
//! 用了就必须在系统提示里带上 [`toon::toon_format_instructions`]，
//! 否则模型不认得那个格式。这个决定归装配方，不归这里。

pub mod fold;
pub mod json;
pub mod level;
pub mod sanitize;
pub mod toon;

pub use level::CompactLevel;
pub use toon::toon_format_instructions;

/// 按给定强度压一段工具输出。
pub fn compact_output(text: &str, level: CompactLevel) -> String {
    match level {
        CompactLevel::Off => text.to_string(),
        CompactLevel::Safe => sanitize::sanitize(text),
        CompactLevel::Full => {
            let text = sanitize::sanitize(text);
            let text = fold::fold_repeated_lines(&text);
            json::compact_json(&text)
        }
    }
}

/// 把一段输出里的同构 JSON 数组编成 TOON。
///
/// 调用方**必须**同时把 [`toon_format_instructions`] 放进系统提示。
pub fn compact_output_toon(text: &str) -> String {
    toon::try_toon_encode(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_一个字节也不动() {
        let 脏 = "\u{1b}[31m红\u{1b}[0m   \n\n\n\n尾";
        assert_eq!(compact_output(脏, CompactLevel::Off), 脏);
    }

    #[test]
    fn safe_只去终端噪声不动内容() {
        let out = compact_output("\u{1b}[32m完成\u{1b}[0m  \n\n\n下一步", CompactLevel::Safe);
        assert_eq!(out, "完成\n\n下一步");
    }

    /// 一段 `Read` 会原样返回的文件内容，行与行足够像。
    const 配置文件: &str = "    \"name\": \"alpha\",\n    \"name\": \"beta\",\n    \"name\": \"gamma\",\n    \"name\": \"delta\",";

    #[test]
    fn safe_不折叠相似行() {
        // 这是 Safe 与 Full 的分界线，也是本模块最要紧的一条不变量：
        // Safe 用在任何输出上都不会改变它说的事。
        assert_eq!(compact_output(配置文件, CompactLevel::Safe), 配置文件);
    }

    #[test]
    fn full_会折叠_所以它不能用在_read_的输出上() {
        // 这条测试的作用是**把危险钉在明面上**：同一份文件内容，Full 之后不再是
        // 它自己。`Read` 承诺整份读回逐字节一致，基于折叠过的内容做 Edit 会把
        // 文件改成谁也没要求的样子，而且全程没有任何一步报错。
        let out = compact_output(配置文件, CompactLevel::Full);
        assert_ne!(out, 配置文件, "Full 没有折叠，这条测试就没在防任何事");
        assert!(out.contains("省略"), "{out}");
    }

    #[test]
    fn 折叠的门槛不低_普通代码不会被误折() {
        // 相似度要过半才折。`let a = 1;` 与 `let b = 2;` 只共享 `let `，
        // 四个字符占十个，不到一半——所以寻常代码即便走 Full 也留着原样。
        // 这不是可以放心把 Full 用在 Read 上的理由（见上一条），
        // 而是说明这个门槛调得还算克制。
        let 代码 = "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;";
        assert_eq!(compact_output(代码, CompactLevel::Full), 代码);
    }

    #[test]
    fn full_把命令输出压得动() {
        // 它该用在的地方：一条命令吐出几百行同构日志。
        let 日志: String = (1..=20)
            .map(|n| format!("\u{1b}[32mCompiling\u{1b}[0m crate{n} v1.0"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = compact_output(&日志, CompactLevel::Full);
        assert!(out.lines().count() < 5, "{out}");
        assert!(!out.contains('\u{1b}'), "转义没去干净：{out}");
        // 首末两行留着——模型据此判断这段是什么。
        assert!(out.starts_with("Compiling crate1 v1.0"), "{out}");
        assert!(out.ends_with("Compiling crate20 v1.0"), "{out}");
    }

    #[test]
    fn toon_不进_compact_output() {
        // 它换的是编码方式，用了就得在系统提示里带说明——那个决定归装配方。
        let text = r#"[{"a":1},{"a":2}]"#;
        assert_eq!(compact_output(text, CompactLevel::Safe), text);
        assert_eq!(compact_output_toon(text), "[2]{a}:\n  1\n  2");
    }

    #[test]
    fn 空输入在每一级都不崩() {
        for level in [CompactLevel::Off, CompactLevel::Safe, CompactLevel::Full] {
            assert_eq!(compact_output("", level), "");
        }
        assert_eq!(compact_output_toon(""), "");
    }
}
