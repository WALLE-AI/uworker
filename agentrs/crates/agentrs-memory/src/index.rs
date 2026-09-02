// Ported from aionrs (Apache-2.0), crates/aion-memory.
//   Source: crates/aion-memory/src/{index,types}.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: **只取纯逻辑那半**——`read_index`/`append_index_entry`/
//            `remove_index_entry` 三个碰磁盘的函数不移植（索引的读写归 Core，
//            见 crate 文档）；`floor_char_boundary`（不稳定 API）换成本仓库
//            他处同样的 `is_char_boundary` 回退；警告文本改中文并说清怎么改。

//! 记忆索引的裁剪。
//!
//! `MEMORY.md` 是记忆系统的目录页，**每一轮都进系统提示**。它由模型自己写、
//! 只增不减，所以它一定会长——一个用了三个月的工作区，那份索引可能有上千行。
//!
//! 不设上限的话，那份索引会安静地吃掉上下文预算的一大块，而且没人会注意到：
//! 它不报错，只是让每一轮都更贵、可用的上下文更少。
//!
//! # 只裁剪，不读写
//!
//! 索引文件的读写归 Core（见 crate 文档：索引、权限、保留策略全归 Core）。
//! 这里收一段已经读出来的文本，返回裁剪过的版本。

/// 行数上限。
pub const MAX_INDEX_LINES: usize = 200;
/// 字节上限，约 25 KB。
pub const MAX_INDEX_BYTES: usize = 25_000;

/// 一次裁剪的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexTruncation {
    /// 裁剪后的内容。
    pub content: String,
    /// **原始**行数，不是裁剪后的。
    pub line_count: usize,
    /// **原始**字节数。
    pub byte_count: usize,
    /// 有没有裁过。
    pub was_truncated: bool,
}

/// 把索引裁进行数与字节两个上限之内。
///
/// 两个上限都要，因为失效模式有两种：条目**太多**（行数），
/// 以及条目**太长**（字节）。只设行数上限的话，200 行每行两千字的索引照样
/// 能吃掉一整个预算。
pub fn truncate_index(raw: &str) -> IndexTruncation {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return IndexTruncation {
            content: String::new(),
            line_count: 0,
            byte_count: 0,
            was_truncated: false,
        };
    }

    let lines: Vec<&str> = trimmed.split('\n').collect();
    let line_count = lines.len();
    let byte_count = trimmed.len();

    let 行超了 = line_count > MAX_INDEX_LINES;
    // 按**原始**字节数判，不按裁完行之后的：字节上限针对的正是"行少但每行很长"，
    // 而那种输入裁完行之后可能已经不超了，于是警告不会发出——
    // 用户就永远不知道该把条目写短些。
    let 字节超了 = byte_count > MAX_INDEX_BYTES;

    if !行超了 && !字节超了 {
        return IndexTruncation {
            content: trimmed.to_owned(),
            line_count,
            byte_count,
            was_truncated: false,
        };
    }

    let mut out = if 行超了 {
        lines[..MAX_INDEX_LINES].join("\n")
    } else {
        trimmed.to_owned()
    };

    if out.len() > MAX_INDEX_BYTES {
        // 上限可能落在一个多字节字符中间（中文索引尤其），
        // 那里切下去会 panic。先退到字符边界。
        let mut cap = MAX_INDEX_BYTES.min(out.len());
        while cap > 0 && !out.is_char_boundary(cap) {
            cap -= 1;
        }
        // 再退到最近的换行，别把一条索引切成半句。
        let cut = out[..cap].rfind('\n').filter(|&p| p > 0).unwrap_or(cap);
        out.truncate(cut);
    }

    // 说清是哪个上限触发的，以及**怎么改**。只说"被截断了"的话，
    // 用户下一步只能猜。
    let 原因 = match (行超了, 字节超了) {
        (true, false) => format!("有 {line_count} 行（上限 {MAX_INDEX_LINES} 行）"),
        (false, true) => format!(
            "有 {}（上限 {}）——条目写得太长了",
            人读字节(byte_count),
            人读字节(MAX_INDEX_BYTES)
        ),
        _ => format!("有 {line_count} 行、{}", 人读字节(byte_count)),
    };
    out.push_str(&format!(
        "\n\n> 注意：MEMORY.md {原因}，**只加载了一部分**。\
         请把每条索引压到一行、200 字以内，细节移进各自的主题文件。"
    ));

    IndexTruncation {
        content: out,
        line_count,
        byte_count,
        was_truncated: true,
    }
}

fn 人读字节(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} 字节")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 没超上限时原样返回() {
        let r = truncate_index("- [甲](a.md) — 一句话\n- [乙](b.md) — 另一句");
        assert!(!r.was_truncated);
        assert_eq!(r.line_count, 2);
        assert!(r.content.contains("乙"));
        assert!(!r.content.contains("注意"));
    }

    #[test]
    fn 首尾空白被去掉() {
        let r = truncate_index("\n\n  内容  \n\n");
        assert_eq!(r.content, "内容");
    }

    #[test]
    fn 空索引不是错误() {
        // 一份还没写过任何记忆的工作区，索引就是空的。
        let r = truncate_index("   \n\n ");
        assert!(!r.was_truncated);
        assert_eq!(r.line_count, 0);
        assert_eq!(r.content, "");
    }

    #[test]
    fn 行太多时裁到上限并说明() {
        let raw: String = (0..300).map(|n| format!("- 第 {n} 条\n")).collect();
        let r = truncate_index(&raw);
        assert!(r.was_truncated);
        assert_eq!(r.line_count, 300, "报的是原始行数");
        assert!(r.content.contains("有 300 行"), "{}", &r.content[r.content.len() - 120..]);
        // 裁到上限，加上末尾那段警告（空行 + 一行）。
        assert!(r.content.lines().count() <= MAX_INDEX_LINES + 3);
    }

    #[test]
    fn 行少但每行很长时按字节裁() {
        // 只设行数上限的话，200 行每行两千字的索引照样能吃掉一整个预算。
        let raw: String = (0..30).map(|n| format!("- {n} {}\n", "长".repeat(400))).collect();
        let r = truncate_index(&raw);
        assert!(r.was_truncated);
        assert!(r.content.contains("条目写得太长了"), "{}", &r.content[r.content.len() - 150..]);
    }

    #[test]
    fn 中文索引裁剪时不会崩() {
        // 字节上限几乎必然落在某个汉字中间，那里切下去会 panic。
        let raw: String = (0..500).map(|n| format!("- 第{n}条中文记忆条目内容\n")).collect();
        let r = truncate_index(&raw);
        assert!(r.was_truncated);
        // 内容仍是合法 UTF-8（能走到这就是）。
        assert!(r.content.contains("第0条"));
    }

    #[test]
    fn 不把一条索引切成半句() {
        // 半条索引比没有更糟：模型会以为那个记忆就叫那半个名字。
        let 一条 = format!("- [{}](x.md) — 说明\n", "名".repeat(300));
        let raw = 一条.repeat(40);
        let r = truncate_index(&raw);
        let 正文 = r.content.split("\n\n> 注意").next().unwrap();
        // 每一行要么完整，要么不在。
        for line in 正文.lines() {
            assert!(line.is_empty() || line.ends_with("说明"), "半条：{line}");
        }
    }

    #[test]
    fn 警告说清了怎么改() {
        // 只说"被截断了"的话，用户下一步只能猜。
        let raw: String = (0..300).map(|n| format!("- 第 {n} 条\n")).collect();
        let w = truncate_index(&raw).content;
        assert!(w.contains("压到一行"), "{w}");
        assert!(w.contains("移进各自的主题文件"), "{w}");
    }

    #[test]
    fn 两个上限都超时都报() {
        let raw: String = (0..400).map(|n| format!("- {n} {}\n", "长".repeat(100))).collect();
        let r = truncate_index(&raw);
        let w = &r.content;
        assert!(w.contains("行、"), "两个都该提：{}", &w[w.len() - 140..]);
    }

    #[test]
    fn 字节数按人读的方式呈现() {
        assert_eq!(人读字节(500), "500 字节");
        assert_eq!(人读字节(25_000), "24.4 KB");
    }
}
