//! `ContextModifier` 的合并语义测试。
//!
//! 核心只有一条：**合并结果恒不比输入更宽**。其余测试都是它的分解。

use super::*;

fn 集合(xs: &[&str]) -> BTreeSet<String> {
    xs.iter().map(|s| (*s).to_owned()).collect()
}

fn 视图() -> ContextView {
    ContextView {
        tools: 集合(&["Read", "Write", "Bash"]),
        max_input_tokens: 100_000,
        permission_mode: PermissionMode::Default,
        max_depth: 3,
    }
}

fn 已接受(scopes: &[&str]) -> PermissionMode {
    PermissionMode::Accepted {
        scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
    }
}

// ---------------------------------------------------------------------------
// 总承诺
// ---------------------------------------------------------------------------

#[test]
fn 任何修饰器都不能让视图变宽() {
    // **本模块的全部承诺。** 下面每个修饰器都在尝试扩大某一项。
    let base = 视图();
    let 各种尝试 = [
        // 想加一个不存在的工具。
        ContextModifier {
            tool_subset: Some(集合(&["Read", "Write", "Bash", "Sudo"])),
            ..Default::default()
        },
        // 想把预算调大。
        ContextModifier {
            max_input_tokens: Some(999_999),
            ..Default::default()
        },
        // 想把权限放宽。
        ContextModifier {
            permission_mode: Some(已接受(&["一切"])),
            ..Default::default()
        },
        // 想把深度上限调大。
        ContextModifier {
            max_depth: Some(99),
            ..Default::default()
        },
    ];

    for m in &各种尝试 {
        let r = m.apply(&base);
        assert!(
            is_no_wider(&r.view, &base),
            "修饰器让视图变宽了：{m:?} → {:?}",
            r.view
        );
    }
}

// ---------------------------------------------------------------------------
// 工具：取交集
// ---------------------------------------------------------------------------

#[test]
fn 工具取交集而不是覆盖() {
    let m = ContextModifier {
        tool_subset: Some(集合(&["Read", "Grep"])),
        ..Default::default()
    };
    let r = m.apply(&视图());
    // Grep 不在当前视图里 → 不会被加进来。
    assert_eq!(r.view.tools, 集合(&["Read"]));
}

#[test]
fn 技能要一个不存在的工具时留痕而不是静默丢弃() {
    // 它多半是技能清单写错了。静默忽略会让作者一直以为自己的配置生效了。
    let m = ContextModifier {
        tool_subset: Some(集合(&["Read", "Sudo"])),
        ..Default::default()
    };
    let r = m.apply(&视图());
    assert!(
        r.narrowings.iter().any(|n| matches!(
            n,
            Narrowing::WideningIgnored {
                field: "tool_subset",
                wanted
            } if wanted.contains("Sudo")
        )),
        "{:?}",
        r.narrowings
    );
}

#[test]
fn 被移除的工具逐个点名() {
    // "这一轮为什么少了几个工具"必须可从 trajectory 解释。
    // 只记"应用了技能 X"不够——得说清它到底动了什么。
    let m = ContextModifier {
        tool_subset: Some(集合(&["Read"])),
        ..Default::default()
    };
    let r = m.apply(&视图());
    match r
        .narrowings
        .iter()
        .find(|n| matches!(n, Narrowing::ToolsRemoved { .. }))
    {
        Some(Narrowing::ToolsRemoved { removed }) => {
            assert_eq!(removed, &["Bash".to_string(), "Write".to_string()]);
        }
        other => panic!("缺少 ToolsRemoved：{other:?}"),
    }
}

#[test]
fn 不表态的字段保持原样() {
    let r = ContextModifier::default().apply(&视图());
    assert_eq!(r.view, 视图());
    assert!(r.narrowings.is_empty());
}

// ---------------------------------------------------------------------------
// 预算 / 深度：取最小
// ---------------------------------------------------------------------------

#[test]
fn 预算取最小() {
    let m = ContextModifier {
        max_input_tokens: Some(50_000),
        ..Default::default()
    };
    let r = m.apply(&视图());
    assert_eq!(r.view.max_input_tokens, 50_000);
    assert_eq!(
        r.narrowings,
        [Narrowing::BudgetLowered {
            from: 100_000,
            to: 50_000
        }]
    );
}

#[test]
fn 想调大预算被忽略并留痕() {
    let m = ContextModifier {
        max_input_tokens: Some(999_999),
        ..Default::default()
    };
    let r = m.apply(&视图());
    assert_eq!(r.view.max_input_tokens, 100_000);
    assert!(matches!(
        r.narrowings.as_slice(),
        [Narrowing::WideningIgnored {
            field: "max_input_tokens",
            ..
        }]
    ));
}

#[test]
fn 深度取最小() {
    let m = ContextModifier {
        max_depth: Some(1),
        ..Default::default()
    };
    assert_eq!(m.apply(&视图()).view.max_depth, 1);
}

// ---------------------------------------------------------------------------
// 权限模式：只在更严格时生效
// ---------------------------------------------------------------------------

#[test]
fn 模式收紧生效() {
    let m = ContextModifier {
        permission_mode: Some(PermissionMode::Plan),
        ..Default::default()
    };
    let r = m.apply(&视图());
    assert_eq!(r.view.permission_mode, PermissionMode::Plan);
    assert!(matches!(
        r.narrowings.as_slice(),
        [Narrowing::ModeTightened { .. }]
    ));
}

#[test]
fn 模式放宽被忽略() {
    // **技能不是权限插件。** 它能在已有能力内挑子集，不能引入新能力。
    let m = ContextModifier {
        permission_mode: Some(已接受(&["一切"])),
        ..Default::default()
    };
    let r = m.apply(&视图());
    assert_eq!(r.view.permission_mode, PermissionMode::Default);
    assert!(matches!(
        r.narrowings.as_slice(),
        [Narrowing::WideningIgnored {
            field: "permission_mode",
            ..
        }]
    ));
}

// ---------------------------------------------------------------------------
// 多技能合并
// ---------------------------------------------------------------------------

#[test]
fn 多个技能依次收窄() {
    let a = ContextModifier {
        tool_subset: Some(集合(&["Read", "Write"])),
        max_input_tokens: Some(80_000),
        ..Default::default()
    };
    let b = ContextModifier {
        tool_subset: Some(集合(&["Read", "Bash"])),
        max_input_tokens: Some(60_000),
        ..Default::default()
    };
    let r = merge_all(&视图(), &[a, b]);
    assert_eq!(r.view.tools, 集合(&["Read"]));
    assert_eq!(r.view.max_input_tokens, 60_000);
}

#[test]
fn 启用顺序不影响最终能力() {
    // **合并算子是取下确界，满足交换律。**
    // 若顺序有影响，"先启 A 再启 B"和"先启 B 再启 A"会得到两个不同的 Run。
    let a = ContextModifier {
        tool_subset: Some(集合(&["Read", "Write"])),
        max_input_tokens: Some(80_000),
        permission_mode: Some(PermissionMode::Plan),
        ..Default::default()
    };
    let b = ContextModifier {
        tool_subset: Some(集合(&["Read", "Bash"])),
        max_depth: Some(1),
        ..Default::default()
    };

    let ab = merge_all(&视图(), &[a.clone(), b.clone()]);
    let ba = merge_all(&视图(), &[b, a]);
    assert_eq!(ab.view, ba.view, "启用顺序改变了最终能力");
}

#[test]
fn 空修饰器列表不改变视图() {
    let r = merge_all(&视图(), &[]);
    assert_eq!(r.view, 视图());
    assert!(r.narrowings.is_empty());
}

#[test]
fn 合并是幂等的() {
    // 同一个技能被启用两次不该产生额外收窄——那意味着"重复启用"
    // 会一点点把能力磨没。
    let m = ContextModifier {
        tool_subset: Some(集合(&["Read", "Write"])),
        max_input_tokens: Some(80_000),
        ..Default::default()
    };
    let 一次 = merge_all(&视图(), std::slice::from_ref(&m));
    let 两次 = merge_all(&视图(), &[m.clone(), m]);
    assert_eq!(一次.view, 两次.view);
}

#[test]
fn 多技能合并同样不会变宽() {
    let base = 视图();
    let mods = [
        ContextModifier {
            tool_subset: Some(集合(&["Read", "Sudo"])),
            max_input_tokens: Some(999_999),
            ..Default::default()
        },
        ContextModifier {
            permission_mode: Some(已接受(&["一切"])),
            max_depth: Some(99),
            ..Default::default()
        },
    ];
    let r = merge_all(&base, &mods);
    assert!(is_no_wider(&r.view, &base));
    // 四项越界要求全部留痕。
    assert_eq!(
        r.narrowings
            .iter()
            .filter(|n| matches!(n, Narrowing::WideningIgnored { .. }))
            .count(),
        4,
        "{:?}",
        r.narrowings
    );
}
