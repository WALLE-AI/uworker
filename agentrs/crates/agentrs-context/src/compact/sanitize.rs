// Ported from aionrs (Apache-2.0), crates/aion-compact.
//   Source: crates/aion-compact/src/sanitize.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: ANSI 剥离改为手写状态机，去掉 `regex` 依赖（内核 crate 不为一条固定
//            形状引一份依赖，与本仓库手写 glob/markdown/HTML 同一判断）；
//            `merge_blank_lines` 三分支重写为等价但读得懂的两分支。

//! 无损清洗：把终端曾经怎么画，和它究竟说了什么，分开。
//!
//! 命令输出里有大量只对终端有意义的字节——颜色转义、用回车把整行重画一遍的
//! 进度条。它们进了模型上下文只是噪声，而且**很贵**：`cargo build` 的一条进度行
//! 反复重画几百次，原样送进去就是几百行几乎相同的内容。
//!
//! 这里的每一步都不改变文本要说的事，所以它是
//! [`CompactLevel::Safe`](super::CompactLevel::Safe) 的全部内容。

/// 剥掉 ANSI 转义序列。
///
/// 手写而不是引 `regex`：要认的形状是固定的一条（CSI），
/// 与本仓库手写 glob 匹配器、markdown 渲染器、HTML 抽取器同一判断。
pub fn strip_ansi(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        // CSI: ESC '[' 参数字节* 终止字母
        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ';') {
                j += 1;
            }
            if j < chars.len() && chars[j].is_ascii_alphabetic() {
                i = j + 1;
                continue;
            }
            // 不是一条完整的 CSI：原样留着。没写完的转义序列也是内容，
            // 吞掉它会让人以为输出被截断了。
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 折叠回车覆盖：一行里被 `\r` 重画多次时，只留最后一次的样子。
///
/// 这正是终端会显示的东西——进度条走完之后，屏幕上留下的是最后那一帧。
pub fn collapse_cr_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for line in text.split('\n') {
        if !result.is_empty() {
            result.push('\n');
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(last) = line.rsplit('\r').next() {
            result.push_str(last);
        }
    }
    result
}

/// 去掉每行行尾空白。
pub fn trim_trailing_whitespace(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 连续空行合并成一个。
pub fn merge_blank_lines(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut prev_blank = false;
    for line in text.split('\n') {
        let blank = line.trim().is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push(if blank { "" } else { line });
        prev_blank = blank;
    }
    out.join("\n")
}

/// 全套无损清洗。
pub fn sanitize(text: &str) -> String {
    let text = strip_ansi(text);
    let text = collapse_cr_lines(&text);
    let text = trim_trailing_whitespace(&text);
    merge_blank_lines(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 颜色转义被剥掉正文留下() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(strip_ansi("\u{1b}[1;32mok\u{1b}[m done"), "ok done");
        assert_eq!(strip_ansi("没有转义"), "没有转义");
    }

    #[test]
    fn 光标移动这类非颜色的_csi_也剥掉() {
        // 进度条不只用颜色，它还用 `2K`（清行）与 `1A`（上移一行）。
        assert_eq!(strip_ansi("\u{1b}[2K\u{1b}[1Aline"), "line");
    }

    #[test]
    fn 没写完的转义序列原样留着() {
        // 吞掉它会让人以为输出被截断了。截断的痕迹本身是信息——输出正好在一条
        // 转义序列中间断掉，说明命令是被杀掉的，而不是正常结束的。
        assert_eq!(strip_ansi("\u{1b}[31"), "\u{1b}[31");
        assert_eq!(strip_ansi("\u{1b}["), "\u{1b}[");
        assert_eq!(strip_ansi("\u{1b}"), "\u{1b}");
        // ESC 后面不是 `[` 的也不动：那是别的转义族（OSC 之类），不归这里管。
        assert_eq!(strip_ansi("a\u{1b}Xb"), "a\u{1b}Xb");
    }

    #[test]
    fn 无参数的_csi_是完整的() {
        // `ESC[z` 没有参数字节，但有终止字母，按 ANSI 它就是完整的一条。
        // 后面那个 z 是正文。
        assert_eq!(strip_ansi("a\u{1b}[zz"), "az");
    }

    #[test]
    fn 回车覆盖只留最后一帧() {
        // 这正是终端会显示的东西。`cargo build` 的进度行反复重画几百次，
        // 原样送进上下文就是几百行几乎相同的内容。
        assert_eq!(collapse_cr_lines("10%\r50%\r100%"), "100%");
        assert_eq!(collapse_cr_lines("a\r\nb"), "a\nb");
        assert_eq!(collapse_cr_lines("no cr"), "no cr");
    }

    #[test]
    fn 连续空行合并成一个() {
        assert_eq!(merge_blank_lines("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(merge_blank_lines("a\nb"), "a\nb");
        // 单个空行是段落边界，留着。
        assert_eq!(merge_blank_lines("a\n\nb"), "a\n\nb");
    }

    #[test]
    fn 行尾空白被去掉() {
        assert_eq!(trim_trailing_whitespace("a   \nb\t\n"), "a\nb");
    }

    #[test]
    fn 全套清洗按顺序生效() {
        let 脏 = "\u{1b}[32m构建中\u{1b}[0m   \r\u{1b}[32m完成\u{1b}[0m   \n\n\n下一步";
        assert_eq!(sanitize(脏), "完成\n\n下一步");
    }

    #[test]
    fn 清洗是无损的_不改变文本要说的事() {
        // 这一级的全部承诺就在这条里：把终端怎么画去掉，说了什么一字不动。
        let 正文 = "第一行\n第二行\n第三行";
        assert_eq!(sanitize(正文), 正文);
    }

    #[test]
    fn 空输入不会崩() {
        assert_eq!(sanitize(""), "");
        assert_eq!(strip_ansi(""), "");
        assert_eq!(collapse_cr_lines(""), "");
    }
}
