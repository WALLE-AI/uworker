//! 提示词安全 lint（架构 §10 末段）。
//!
//! > 禁止将产品文案、数据库路径、UI 控制逻辑嵌入 prompt。
//!
//! ## 这不是风格检查
//!
//! 每一条禁令背后都有一个具体后果：
//!
//! | 禁的东西 | 后果 |
//! |---|---|
//! | 绝对路径 | 随 bundle 导出就漏出去；换台机器它还是错的 |
//! | 密钥 / 连接串 | 同上，且直接是安全事故 |
//! | 产品文案 / UI 控制 | 改一句界面用语要动内核，内核改动要重跑全部 eval |
//! | 未声明的占位符 | 渲染出一句字面量 `{foo}` 给模型看 |
//! | 声明了却没用的输入 | 调用方在准备一个没人要的参数，多半是改模板时漏了 |
//!
//! **宁可误报**：漏报的代价是一条坏提示词进了生产，误报的代价是多看一眼。

use crate::Prompt;

/// 一条 lint 发现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 哪份提示词。
    pub prompt_id: &'static str,
    /// 违反了什么。
    pub violation: Violation,
    /// 命中的片段，**已截断**。
    pub sample: String,
}

/// 违规类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    /// 含绝对路径。
    AbsolutePath,
    /// 含疑似密钥或连接串。
    Secret,
    /// 含产品文案或 UI 控制逻辑。
    ProductCopy,
    /// 模板里有占位符却没在 `inputs` 里声明。
    UndeclaredPlaceholder,
    /// 声明了输入却没在模板里用。
    UnusedInput,
}

/// 产品文案与 UI 控制逻辑的特征词。
///
/// 这份名单**必然不全**——它拦的是常见的那几种，
/// 真正的防线是 review 时看得见提示词改动（snapshot fixture）。
const 产品文案特征: &[&str] = &[
    "点击",
    "按钮",
    "菜单",
    "对话框",
    "弹窗",
    "侧边栏",
    "升级到专业版",
    "订阅",
    "试用期",
    "客服",
    "className",
    "onClick",
    "<div",
    "</div>",
];

/// 密钥与连接串特征。
const 密钥特征: &[&str] = &[
    "sk-",
    "ghp_",
    "AKIA",
    "xoxb-",
    "-----BEGIN",
    "postgres://",
    "mysql://",
    "mongodb://",
    "redis://",
    "password=",
    "api_key=",
    "apikey=",
];

/// 审查一份提示词。
pub fn audit(p: &Prompt) -> Vec<Finding> {
    let mut out = Vec::new();
    let t = p.template;

    // 绝对路径。
    for tok in t.split(|c: char| c.is_whitespace() || c == '"' || c == '`') {
        if tok.len() > 3 && is_absolute(tok) {
            out.push(Finding {
                prompt_id: p.id,
                violation: Violation::AbsolutePath,
                sample: truncate(tok),
            });
            break;
        }
    }

    // 密钥与连接串。
    for k in 密钥特征 {
        if let Some(i) = t.find(k) {
            out.push(Finding {
                prompt_id: p.id,
                violation: Violation::Secret,
                sample: truncate(&t[i..]),
            });
            break;
        }
    }

    // 产品文案与 UI。
    for k in 产品文案特征 {
        if let Some(i) = t.find(k) {
            out.push(Finding {
                prompt_id: p.id,
                violation: Violation::ProductCopy,
                sample: truncate(&t[i..]),
            });
            break;
        }
    }

    // 占位符与声明对不上——两个方向都查。
    let 实际 = p.placeholders();
    for name in &实际 {
        if !p.inputs.contains(&name.as_str()) {
            out.push(Finding {
                prompt_id: p.id,
                violation: Violation::UndeclaredPlaceholder,
                sample: format!("{{{name}}}"),
            });
        }
    }
    for name in p.inputs {
        if !实际.iter().any(|x| x == name) {
            out.push(Finding {
                prompt_id: p.id,
                violation: Violation::UnusedInput,
                sample: (*name).to_owned(),
            });
        }
    }

    out
}

/// 审查全部已注册提示词。
pub fn audit_all() -> Vec<Finding> {
    crate::registry::all().iter().flat_map(audit).collect()
}

fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    b.first() == Some(&b'/')
        || p.starts_with("\\\\")
        // Windows 盘符 `C:\`。**按字节比较，不能切字符串**——
        // `&p[1..3]` 在 "a条件" 这类以 ASCII 开头的多字节串上会切在
        // 字符中间直接 panic。这个 bug 在中文提示词上一碰就炸。
        || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\')
}

fn truncate(s: &str) -> String {
    s.chars().take(16).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 提示(template: &'static str, inputs: &'static [&'static str]) -> Prompt {
        Prompt {
            id: "test",
            version: 1,
            template,
            inputs,
        }
    }

    #[test]
    fn 干净的提示词无发现() {
        assert!(audit(&提示("你是一个简洁的助手。", &[])).is_empty());
    }

    #[test]
    fn 抓得住绝对路径() {
        // 随 bundle 导出就漏出去；换台机器它还是错的。
        let f = audit(&提示("配置在 /etc/agentrs/config.toml 里。", &[]));
        assert_eq!(f[0].violation, Violation::AbsolutePath);
    }

    #[test]
    fn 抓得住连接串() {
        let f = audit(&提示("连 postgres://user:pw@db/x 查一下。", &[]));
        assert_eq!(f[0].violation, Violation::Secret);
    }

    #[test]
    fn 抓得住_ui_控制逻辑() {
        // 混进提示词意味着改一句界面用语要动内核，
        // 而内核改动要重跑全部 eval。
        let f = audit(&提示("请用户点击右上角的按钮继续。", &[]));
        assert_eq!(f[0].violation, Violation::ProductCopy);
    }

    #[test]
    fn 抓得住未声明的占位符() {
        // 否则渲染出一句字面量 `{limit}` 给模型看。
        let f = audit(&提示("最多选 {limit} 条。", &[]));
        assert_eq!(f[0].violation, Violation::UndeclaredPlaceholder);
        assert_eq!(f[0].sample, "{limit}");
    }

    #[test]
    fn 抓得住声明了却没用的输入() {
        // 调用方在准备一个没人要的参数，多半是改模板时漏了。
        let f = audit(&提示("固定文本。", &["limit"]));
        assert_eq!(f[0].violation, Violation::UnusedInput);
    }

    #[test]
    fn 声明与使用一致时通过() {
        assert!(audit(&提示("最多选 {limit} 条。", &["limit"])).is_empty());
    }

    #[test]
    fn 转义的花括号不算占位符() {
        // JSON 示例里 `{{"selected": []}}` 很常见。
        assert!(audit(&提示(r#"输出 {{"ok": true}}"#, &[])).is_empty());
    }

    #[test]
    fn 样本本身不是完整泄漏() {
        let f = audit(&提示(
            "密钥是 sk-this-is-a-very-long-secret-value 请用它。",
            &[],
        ));
        assert!(f[0].sample.chars().count() <= 17, "{}", f[0].sample);
        assert!(!f[0].sample.contains("very-long"), "{}", f[0].sample);
    }

    #[test]
    fn 以_ascii_开头的多字节串不会让_lint_崩溃() {
        // **这是真实撞到的一个 panic**：`&p[1..3]` 在 "a条件" 这类
        // 以 ASCII 字母开头的多字节串上会切在字符中间。
        // 中文提示词里这种 token 到处都是，一碰就炸。
        for t in ["a条件", "C条件", "x：值", "选 5 条；换行"] {
            let _ = audit(&提示(Box::leak(t.to_string().into_boxed_str()), &[]));
        }
    }

    #[test]
    fn windows_盘符仍被识别() {
        let f = audit(&提示(r"配置在 C:\Users\x\a.toml 里。", &[]));
        assert_eq!(f[0].violation, Violation::AbsolutePath);
    }

    #[test]
    fn 全部已注册提示词通过安全审查() {
        // **这条是本 crate 的守门人。** 新增一份提示词若带了路径、
        // 密钥或产品文案，在这里就会被拦下。
        let f = audit_all();
        assert!(f.is_empty(), "已注册提示词存在违规：{f:#?}");
    }
}
