//! 翻页与截断。
//!
//! 一条判据贯穿本模块：**宁可截断也不撒谎**。从前 `Read` 超限时把**整份内容**
//! 换成一句"输出过大：N 字节"，模型会把那句话当成文件内容读下去；`Bash` 超长时
//! 同理。保留真实的前几行、末尾补一句还剩多少，是同样便宜而且不骗人的做法。
//!
//! 三个工具（Read/Glob/Grep）加两个（Bash/WebFetch）共用这里，所以它独立成模块——
//! 从前 `exec.rs` 与 `web.rs` 各有一份"按字节截断且不切开多字节字符"，
//! 两份都对，但两份就是两处会各自漂移的地方。

/// `Read` 一次最多返回多少行。
pub const READ_DEFAULT_LIMIT: u64 = 2_000;
/// `Grep` 与 `Glob` 一次最多返回多少条。
pub const MATCH_DEFAULT_LIMIT: u64 = 200;
/// `Read` 单次输出的字节上限。
const READ_MAX_BYTES: usize = 16 * 1024;

/// 按字节截断，snap 到字符边界，并**说明截了多少**。
///
/// `note` 是给模型的下一步提示，因为不同工具的出路不同：命令该自己收窄输出，
/// 网页则只能接受截断。
pub fn truncate_bytes(text: &str, max_bytes: usize, note: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    // 不能在多字节字符中间切开。
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n… 共 {} 字节，已显示前 {cut}{}{note}",
        &text[..cut],
        text.len(),
        if note.is_empty() { "" } else { "；" }
    )
}

/// 按行翻页，并**如实说明还剩多少**。
pub fn page(text: &str, offset: usize, limit: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.saturating_sub(1).min(total);
    // 起点越过文件末尾：说清楚，而不是返回一段空内容让模型以为文件是空的。
    if start >= total && total > 0 {
        return format!("offset {offset} 超过文件总行数 {total}");
    }
    let mut taken = Vec::new();
    let mut bytes = 0;
    let mut 因体积截断 = false;
    for line in lines.iter().skip(start).take(limit) {
        // 字节与行数双重上限，先到先算：一行两万字符的 JSON 同样能撑爆上下文。
        if bytes + line.len() > READ_MAX_BYTES && !taken.is_empty() {
            因体积截断 = true;
            break;
        }
        bytes += line.len() + 1;
        taken.push(*line);
    }
    let end = start + taken.len();
    let mut out = taken.join("\n");
    // 读到文件末尾时把结尾换行还回去。`lines()` 会吃掉它，而基于这份内容做的
    // Edit 会把文件重写成没有结尾换行的样子——一次读改写就悄悄改了一个字节。
    if end == total && text.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }
    if end < total {
        if !out.is_empty() {
            out.push('\n');
        }
        let 原因 = if 因体积截断 { "已达单次输出上限" } else { "已达 limit" };
        out.push_str(&format!(
            "… 还有 {} 行未显示（共 {total} 行，{原因}；用 offset={} 继续读）",
            total - end,
            end + 1
        ));
    } else if start > 0 && total > 0 {
        out.push_str(&format!("\n（第 {}–{end} 行，共 {total} 行）", start + 1));
    }
    out
}

/// 截断一份命中列表，并说明还剩多少。
pub fn cap(hits: Vec<String>, limit: usize, unit: &str, empty: &str) -> String {
    if hits.is_empty() {
        return empty.to_string();
    }
    let total = hits.len();
    if total <= limit {
        return hits.join("\n");
    }
    let mut out = hits[..limit].join("\n");
    out.push_str(&format!(
        "\n… 共 {total} {unit}，已显示前 {limit}；请缩小范围或调大 max_results"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 翻页保留真实内容并说明还剩多少() {
        let text = (1..=10).map(|n| n.to_string()).collect::<Vec<_>>().join("\n");
        let first = page(&text, 1, 3);
        assert!(first.starts_with("1\n2\n3"), "{first}");
        assert!(first.contains("还有 7 行未显示"), "{first}");
        assert!(first.contains("offset=4"), "要告诉它怎么接着读：{first}");
        // 中间一页会说明自己是哪一段。
        let middle = page(&text, 4, 3);
        assert!(middle.starts_with("4\n5\n6"), "{middle}");
        assert!(middle.contains("还有 4 行"), "{middle}");
    }

    #[test]
    fn 读到末尾时不追加任何说明() {
        let text = "a\nb\nc\n";
        assert_eq!(page(text, 1, 100), text, "整份读回必须逐字节一致");
    }

    #[test]
    fn 结尾换行不会被吃掉() {
        // `lines()` 会吃掉它，而基于这份内容做的 Edit 会把文件重写成没有结尾
        // 换行的样子——一次读改写就悄悄改了一个字节。
        assert_eq!(page("a\nb\n", 1, 10), "a\nb\n");
        assert_eq!(page("a\nb", 1, 10), "a\nb");
    }

    #[test]
    fn 体积上限先于行数上限生效() {
        let 巨行 = "x".repeat(20 * 1024);
        let text = format!("{巨行}\n{巨行}\n{巨行}");
        let out = page(&text, 1, 1000);
        assert!(out.contains("已达单次输出上限"), "{}", &out[out.len() - 80..]);
        assert!(out.len() < text.len());
    }

    #[test]
    fn 空文件与越界起点不会崩() {
        assert_eq!(page("", 1, 10), "");
        // 越界不能返回空内容——那会被读成"文件是空的"。
        assert_eq!(page("a", 99, 10), "offset 99 超过文件总行数 1");
    }

    #[test]
    fn 命中列表超限时说明总数() {
        let hits: Vec<String> = (1..=10).map(|n| format!("h{n}")).collect();
        let out = cap(hits.clone(), 3, "处匹配", "无匹配");
        assert!(out.starts_with("h1\nh2\nh3"), "{out}");
        assert!(out.contains("共 10 处匹配"), "{out}");
        assert_eq!(cap(hits, 100, "处匹配", "无匹配").lines().count(), 10);
        assert_eq!(cap(Vec::new(), 3, "处匹配", "无匹配"), "无匹配");
    }

    #[test]
    fn 超长文本保留真实前缀并说明截了多少() {
        let 长 = "x".repeat(100);
        let out = truncate_bytes(&长, 40, "让命令自己收窄输出");
        assert!(out.starts_with("xxxx"));
        assert!(out.contains("共 100 字节，已显示前 40"), "{out}");
        assert!(out.contains("让命令自己收窄输出"), "{out}");
    }

    #[test]
    fn 截断不会切开多字节字符() {
        // 只要不 panic 就说明边界找对了；顺带确认前缀仍是合法 UTF-8。
        let 长 = "汉".repeat(100);
        assert!(truncate_bytes(&长, 40, "").starts_with('汉'));
    }

    #[test]
    fn 没超限时原样返回() {
        assert_eq!(truncate_bytes("短", 100, "提示"), "短");
    }
}
