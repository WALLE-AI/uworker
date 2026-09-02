// Ported from aionrs (Apache-2.0), crates/aion-compact.
//   Source: crates/aion-compact/src/fold.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 相似度按**字符数**而不是字节长度算（原实现拿 `chars().count()` 的
//            前缀长度除以 `len()` 的字节长度，中文行上会算出远大于 1 的比值，
//            于是任意两行中文都判为"相似"）。

//! 折叠相似行。
//!
//! **这一步会改变内容**，所以只在 [`CompactLevel::Full`](super::CompactLevel::Full)
//! 生效。一份源代码被这样折叠之后，模型读到的就不再是文件里的东西了——
//! 所以它绝不能用在 `Read` 的输出上。
//!
//! 它该用在哪：一条命令吐出几百行同构日志（`Compiling foo v1.0`、
//! `Downloading bar`），中间那几百行对模型没有任何增量信息。

/// 少于这么多行不折叠。
const MIN_FOLD_COUNT: usize = 3;
/// 公共前缀占比达到这个比例才算"相似"。
const MIN_PREFIX_RATIO: f64 = 0.5;

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn lines_are_similar(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let prefix = common_prefix_len(a, b);
    // 分子分母必须同一把尺子。原实现分子数字符、分母数字节，
    // 中文行上比值会大于 1，于是任意两行中文都判成"相似"，
    // 一段中文输出会被折得只剩首尾两行。
    let min_len = a.chars().count().min(b.chars().count());
    prefix as f64 / min_len as f64 >= MIN_PREFIX_RATIO
}

/// 把连续的相似行折成"首行 + 省略说明 + 末行"。
///
/// 保留首末两行而不是只留一句说明：那两行是模型判断"这段是什么"的依据，
/// 而中间几百行只是同一件事重复。
pub fn fold_repeated_lines(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let lines: Vec<&str> = text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && lines_are_similar(lines[i], lines[j]) {
            j += 1;
        }

        let group_len = j - i;
        if group_len >= MIN_FOLD_COUNT {
            let folded = group_len - 2;
            result.push(lines[i].to_string());
            // 完全相同与只是相似要分开说：前者省掉的是重复，后者省掉的是
            // 模型看不到的内容，它据此决定要不要让人重跑一次拿全量。
            let identical = (i + 1..j).all(|k| lines[k] == lines[i]);
            let 什么 = if identical { "相同" } else { "相似" };
            result.push(format!("[… 省略 {folded} 行{什么}]"));
            result.push(lines[j - 1].to_string());
        } else {
            for line in &lines[i..j] {
                result.push(line.to_string());
            }
        }

        i = j;
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 完全相同的一串折成首尾加说明() {
        let text = "x\nx\nx\nx\nx";
        assert_eq!(fold_repeated_lines(text), "x\n[… 省略 3 行相同]\nx");
    }

    #[test]
    fn 相似但不相同的一串说的是相似() {
        // 两者要分开：省掉的是重复，还是省掉了模型看不到的内容？
        // 后者它可能需要让人重跑一次拿全量。
        let text = "Compiling aaa v1\nCompiling bbb v2\nCompiling ccc v3\nCompiling ddd v4";
        let out = fold_repeated_lines(text);
        assert!(out.contains("相似"), "{out}");
        assert!(out.starts_with("Compiling aaa v1"), "{out}");
        assert!(out.ends_with("Compiling ddd v4"), "{out}");
    }

    #[test]
    fn 不到三行不折() {
        // 折两行省不下什么，反而多一行说明。
        assert_eq!(fold_repeated_lines("x\nx"), "x\nx");
    }

    #[test]
    fn 不相似的行原样留着() {
        let text = "alpha\nbeta\ngamma";
        assert_eq!(fold_repeated_lines(text), text);
    }

    #[test]
    fn 中文行不会被无差别折叠() {
        // 上游按字符数取前缀、按字节数取长度，中文行上比值恒大于 1，
        // 于是任意两行中文都判成"相似"——一段中文输出会被折得只剩首尾两行。
        let text = "第一件事完成了\n完全不同的另一件事\n第三件毫不相干的事";
        assert_eq!(fold_repeated_lines(text), text, "中文行被误折了");
    }

    #[test]
    fn 中文行确实相似时仍然会折() {
        // 修的是尺子，不是把中文整个排除在外。
        let text = "正在编译 模块一\n正在编译 模块二\n正在编译 模块三\n正在编译 模块四";
        let out = fold_repeated_lines(text);
        assert!(out.contains("省略 2 行相似"), "{out}");
    }

    #[test]
    fn 空输入不会崩() {
        assert_eq!(fold_repeated_lines(""), "");
    }
}
