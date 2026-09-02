//! 通配符匹配与子目录限定。
//!
//! 手写而不是引一个匹配引擎，判断与 TUI 的 markdown 渲染器手写那七个固定形状相同：
//! 要支持的形状是固定且少的，为它引一份依赖不划算。
//!
//! **放在 types 里是为了只有一份。** `Glob`/`Grep` 两个工具、以及技能的
//! `paths:` 条件激活都要匹配路径；两份实现迟早会在 `*` 跨不跨 `/` 这类地方
//! 分叉，而那时用户会问"为什么 `paths:` 和 `Glob` 匹配得不一样"，
//! 答案会是"因为它们是两个东西"——那不该是一个答案。

/// 一个路径的最后一段。
pub fn leaf(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// 相对路径是否落在 `scope` 子目录下。空 scope 表示整个工作区。
pub fn under(rel: &str, scope: &str) -> bool {
    let scope = scope.trim_matches('/');
    scope.is_empty() || rel == scope || rel.starts_with(&format!("{scope}/"))
}

/// 通配符匹配：`*` 不跨 `/`，`**` 跨，`?` 匹配单个字符。
///
/// 回溯实现。模式与路径都短（一个路径段级别的量），指数最坏情形在这里够不着。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    matches_from(&p, 0, &t, 0)
}

fn matches_from(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    match p[pi] {
        '*' => {
            let 双星 = p.get(pi + 1) == Some(&'*');
            if 双星 {
                // `**/` 也匹配零个目录层级：`**/*.md` 要能命中根下的 a.md。
                let next = if p.get(pi + 2) == Some(&'/') { pi + 3 } else { pi + 2 };
                if matches_from(p, next, t, ti) {
                    return true;
                }
                for step in ti..t.len() {
                    if matches_from(p, next, t, step + 1) {
                        return true;
                    }
                }
                false
            } else {
                // 单星不跨目录分隔符。
                for step in ti..=t.len() {
                    if t[ti..step].contains(&'/') {
                        break;
                    }
                    if matches_from(p, pi + 1, t, step) {
                        return true;
                    }
                }
                false
            }
        }
        '?' => ti < t.len() && t[ti] != '/' && matches_from(p, pi + 1, t, ti + 1),
        ch => ti < t.len() && t[ti] == ch && matches_from(p, pi + 1, t, ti + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 单星不跨目录分隔符() {
        assert!(glob_match("*.md", "a.md"));
        assert!(!glob_match("*.md", "src/a.md"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/deep/main.rs"));
    }

    #[test]
    fn 双星跨目录且能匹配零层() {
        // `**/*.md` 必须命中根下的 a.md，否则"列出所有 md"这个最常见的用法
        // 恰好漏掉最外层。
        assert!(glob_match("**/*.md", "a.md"));
        assert!(glob_match("**/*.md", "src/a.md"));
        assert!(glob_match("**/*.md", "a/b/c.md"));
        assert!(!glob_match("**/*.md", "a.rs"));
    }

    #[test]
    fn 问号匹配单个非分隔符字符() {
        assert!(glob_match("a?.md", "ab.md"));
        assert!(!glob_match("a?.md", "abc.md"));
        assert!(!glob_match("a?b", "a/b"));
    }

    #[test]
    fn 没有通配符时是精确匹配() {
        assert!(glob_match("src/main.rs", "src/main.rs"));
        assert!(!glob_match("src/main.rs", "src/main.rs.bak"));
    }

    #[test]
    fn 子目录限定按路径段而不是前缀() {
        assert!(under("src/a.rs", "src"));
        assert!(under("src/a.rs", "/src/"));
        assert!(under("a.rs", ""));
        // `src2/a.rs` 不在 `src` 下——按字符串前缀判会误收。
        assert!(!under("src2/a.rs", "src"));
    }

    #[test]
    fn 取路径末段() {
        assert_eq!(leaf("src/a.rs"), "a.rs");
        assert_eq!(leaf("a.rs"), "a.rs");
    }
}
