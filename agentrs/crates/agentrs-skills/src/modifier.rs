//! `ContextModifier`：技能对上下文的贡献如何合并（架构 §10）。
//!
//! ## 一条规则：合并只能收窄
//!
//! > 集合取交集、预算取最小、风险策略取更严格、可见范围只能缩小，
//! > **不能使用普通"右侧覆盖"扩大父 Run 的 `AuthorityEnvelope`**。
//!
//! 这与单调 guard、Hook 的 `Proceed/Advise/Block` 是同一原则的第三次出现：
//! **任何可插拔的东西只能让结论更严**。
//!
//! 普通的配置合并语义（后者覆盖前者）在这里是**不安全的**：
//! 一个写错的技能清单能靠"覆盖"把 `tool_subset` 写成一个更大的集合，
//! 于是技能成了提权路径。所以这里的合并算子不是 `override` 而是 `meet`——
//! 数学上的下确界，天然满足"结果不比任一输入宽"。
//!
//! ## 技能不是权限插件
//!
//! 技能能做的是**在已有能力内挑一个子集**，不能引入新能力。
//! [`ContextModifier::apply`] 的签名就说明了这一点：它拿一个当前视图，
//! 返回一个不更宽的视图，除此之外没有别的出口。

use std::collections::BTreeSet;

use agentrs_contracts::authority::PermissionMode;

/// 一个技能对上下文的贡献。
///
/// 每个字段都是**收窄意图**，不是"设置成"。`None` 表示该技能对这一项没有意见。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextModifier {
    /// 要求的工具子集。**与当前视图取交集**。
    pub tool_subset: Option<BTreeSet<String>>,
    /// 要求的输入预算上限。**与当前取最小**。
    pub max_input_tokens: Option<u64>,
    /// 要求的权限模式。**只在更严格时生效**。
    pub permission_mode: Option<PermissionMode>,
    /// 要求的最大子运行深度。**与当前取最小**。
    pub max_depth: Option<u16>,
}

/// 合并的落点：一份可被收窄的能力视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextView {
    /// 当前可见工具。
    pub tools: BTreeSet<String>,
    /// 当前输入预算。
    pub max_input_tokens: u64,
    /// 当前权限模式。
    pub permission_mode: PermissionMode,
    /// 当前最大子运行深度。
    pub max_depth: u16,
}

/// 一次合并里实际发生的收窄，用于留痕。
///
/// **"这一轮为什么少了几个工具"必须可从 trajectory 解释**（§10 末段）。
/// 只记"应用了技能 X"不够——得说清它到底动了什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Narrowing {
    /// 工具被移除。
    ToolsRemoved {
        /// 被移除的工具名，已排序。
        removed: Vec<String>,
    },
    /// 预算被压低。
    BudgetLowered {
        /// 原值。
        from: u64,
        /// 新值。
        to: u64,
    },
    /// 权限模式收紧。
    ModeTightened {
        /// 原模式判别串。
        from: String,
        /// 新模式判别串。
        to: String,
    },
    /// 子运行深度上限被压低。
    DepthLowered {
        /// 原值。
        from: u16,
        /// 新值。
        to: u16,
    },
    /// 技能提出了一个**更宽**的要求，被忽略。
    ///
    /// **必须留痕而不是静默丢弃。** 它多半是技能清单写错了，
    /// 静默忽略会让作者一直以为自己的配置生效了。
    WideningIgnored {
        /// 哪一项。
        field: &'static str,
        /// 技能要的值。
        wanted: String,
    },
}

/// 合并结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    /// 收窄后的视图。**保证不比输入更宽。**
    pub view: ContextView,
    /// 实际发生的收窄，按发生顺序。
    pub narrowings: Vec<Narrowing>,
}

impl ContextModifier {
    /// 把本修饰器合并进一个视图。
    ///
    /// **返回值恒不比 `view` 更宽**——这是本模块的全部承诺。
    pub fn apply(&self, view: &ContextView) -> Merged {
        let mut out = view.clone();
        let mut narrowings = Vec::new();

        // 工具：取交集。技能要的工具里不在当前视图的部分**直接丢弃**，
        // 不是加进去——那正是"技能不是权限插件"的含义。
        if let Some(want) = &self.tool_subset {
            let 越界: Vec<String> = want.difference(&out.tools).cloned().collect();
            if !越界.is_empty() {
                narrowings.push(Narrowing::WideningIgnored {
                    field: "tool_subset",
                    wanted: 越界.join(","),
                });
            }
            let 新的: BTreeSet<String> = out.tools.intersection(want).cloned().collect();
            let 被移除: Vec<String> = out.tools.difference(&新的).cloned().collect();
            if !被移除.is_empty() {
                narrowings.push(Narrowing::ToolsRemoved { removed: 被移除 });
            }
            out.tools = 新的;
        }

        // 预算：取最小。
        if let Some(want) = self.max_input_tokens {
            if want < out.max_input_tokens {
                narrowings.push(Narrowing::BudgetLowered {
                    from: out.max_input_tokens,
                    to: want,
                });
                out.max_input_tokens = want;
            } else if want > out.max_input_tokens {
                narrowings.push(Narrowing::WideningIgnored {
                    field: "max_input_tokens",
                    wanted: want.to_string(),
                });
            }
        }

        // 权限模式：只在更严格时生效。
        if let Some(want) = &self.permission_mode {
            if want != &out.permission_mode {
                if want.is_at_least_as_strict_as(&out.permission_mode) {
                    narrowings.push(Narrowing::ModeTightened {
                        from: format!("{:?}", out.permission_mode),
                        to: format!("{want:?}"),
                    });
                    out.permission_mode = want.clone();
                } else {
                    narrowings.push(Narrowing::WideningIgnored {
                        field: "permission_mode",
                        wanted: format!("{want:?}"),
                    });
                }
            }
        }

        // 深度：取最小。
        if let Some(want) = self.max_depth {
            if want < out.max_depth {
                narrowings.push(Narrowing::DepthLowered {
                    from: out.max_depth,
                    to: want,
                });
                out.max_depth = want;
            } else if want > out.max_depth {
                narrowings.push(Narrowing::WideningIgnored {
                    field: "max_depth",
                    wanted: want.to_string(),
                });
            }
        }

        Merged {
            view: out,
            narrowings,
        }
    }
}

/// 依次合并多个修饰器。
///
/// **顺序无关**：合并算子是取下确界，满足交换律与结合律。
/// 这一点很重要——技能的启用顺序不该影响最终能力，
/// 否则"先启 A 再启 B"和"先启 B 再启 A"会得到两个不同的 Run。
pub fn merge_all(view: &ContextView, mods: &[ContextModifier]) -> Merged {
    let mut cur = view.clone();
    let mut all = Vec::new();
    for m in mods {
        let r = m.apply(&cur);
        cur = r.view;
        all.extend(r.narrowings);
    }
    Merged {
        view: cur,
        narrowings: all,
    }
}

/// 判断 `narrow` 是否确实不比 `wide` 宽。
///
/// 供测试与调试断言使用——**合并的全部承诺就是这一条**。
pub fn is_no_wider(narrow: &ContextView, wide: &ContextView) -> bool {
    narrow.tools.is_subset(&wide.tools)
        && narrow.max_input_tokens <= wide.max_input_tokens
        && narrow
            .permission_mode
            .is_at_least_as_strict_as(&wide.permission_mode)
        && narrow.max_depth <= wide.max_depth
}

#[cfg(test)]
mod tests;
