//! 压缩规划的语义测试。
//!
//! 重点在**该压哪一段**与**压了到底有没有用**这两件事——它们是 §9.3 里
//! 真正难的部分，也是唯一能脱离模型穷尽测试的部分。

use super::*;

fn 条目(seq: u64, tokens: u64) -> Entry {
    Entry {
        seq: EventSequence(seq),
        priority: Priority::OldHistory,
        tokens,
        is_tool_output: false,
        consumed: true,
        is_error: false,
    }
}

fn 工具输出(seq: u64, tokens: u64) -> Entry {
    Entry {
        is_tool_output: true,
        ..条目(seq, tokens)
    }
}

fn 策略() -> Policy {
    Policy {
        threshold_pct: 80,
        max_input_tokens: 1000,
        keep_recent: 2,
    }
}

/// 造一段总量超过阈值的历史。
fn 超阈值(n: usize) -> Vec<Entry> {
    (0..n as u64).map(|i| 工具输出(i, 200)).collect()
}

// ---------------------------------------------------------------------------
// 触发
// ---------------------------------------------------------------------------

#[test]
fn 未触及阈值时不压缩() {
    let e = vec![工具输出(0, 10), 工具输出(1, 10), 工具输出(2, 10)];
    assert_eq!(
        plan(&e, &策略(), Trigger::Pressure, &[]),
        Err(NoPlan::UnderThreshold)
    );
}

#[test]
fn 溢出触发不看阈值() {
    // provider 已经明确说了"太长"。此时再去问自己的估算器"够不够长"
    // 是本末倒置——估算器只保证不低估，它完全可能说"还没到 80%"。
    let e = vec![工具输出(0, 10), 工具输出(1, 10), 工具输出(2, 10)];
    let p = plan(&e, &策略(), Trigger::Overflow, &[]).expect("溢出触发应当产出计划");
    assert_eq!(p.trigger, Trigger::Overflow);
}

// ---------------------------------------------------------------------------
// 保留最近
// ---------------------------------------------------------------------------

#[test]
fn 最近若干条永远保留() {
    // 没有这条，压缩会把刚发生的事也摘要掉，模型立刻失去当前任务的上下文。
    let e = 超阈值(5);
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    // keep_recent = 2 → 只有 seq 0..3 可动。
    assert!(p.range.end.0 <= 3, "动到了要保留的尾部：{:?}", p.range);
}

#[test]
fn 全部条目都在保留窗口内时无计划() {
    let e = vec![工具输出(0, 500), 工具输出(1, 500)];
    assert_eq!(
        plan(&e, &策略(), Trigger::Overflow, &[]),
        Err(NoPlan::NothingCompactible)
    );
}

// ---------------------------------------------------------------------------
// 谁不能动
// ---------------------------------------------------------------------------

#[test]
fn 钉住的段永不参与压缩() {
    let mut e = 超阈值(5);
    e[0].priority = Priority::SafetyRules;
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    assert!(p.range.start.0 > 0, "动到了 SafetyRules：{:?}", p.range);
}

#[test]
fn 承载错误的条目被保留() {
    // 错误是后续修复的依据。剪掉会让模型反复犯同一个错——
    // 它看不到自己上次为什么失败。
    let mut e = 超阈值(6);
    e[1].is_error = true;
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    let 覆盖 = p.range.start.0..p.range.end.0;
    assert!(!覆盖.contains(&1), "错误条目被压掉了：{:?}", p.range);
}

#[test]
fn 未消费的工具结果不被剪掉() {
    // 模型还没看过它。剪了等于这次调用白做。
    let mut e = 超阈值(6);
    for x in e.iter_mut() {
        x.consumed = false;
    }
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    // microcompact 剪不动 → 降级到 Compact（摘要不要求已消费）。
    assert_eq!(p.tier, Tier::Compact);
}

// ---------------------------------------------------------------------------
// 分层
// ---------------------------------------------------------------------------

#[test]
fn 优先尝试_microcompact() {
    // 代价最小、信息损失最可控。
    let e = 超阈值(6);
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    assert_eq!(p.tier, Tier::Microcompact);
}

#[test]
fn 没有工具输出可剪时降级为摘要() {
    let e: Vec<Entry> = (0..6).map(|i| 条目(i, 200)).collect();
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    assert_eq!(p.tier, Tier::Compact);
}

// ---------------------------------------------------------------------------
// 区间选择
// ---------------------------------------------------------------------------

#[test]
fn 选择最长的一段连续可动条目() {
    // 为什么要连续：Replace 遮蔽的是一个区间。挑一堆不连续的条目需要
    // 多个 Replace，而每个都是一次缓存失效点——压一次断好几处前缀。
    let mut e = 超阈值(9);
    // 在中间插一个不可动的，把可动区切成 [0,1] 与 [3..7]。
    e[2].is_error = true;
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    assert_eq!(p.range.start.0, 3, "应当选更长的那一段：{:?}", p.range);
    assert_eq!(p.range.end.0, 7);
}

#[test]
fn 回收量等于被遮蔽条目的总和() {
    let e = 超阈值(6);
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    let 实际: u64 = e
        .iter()
        .filter(|x| (p.range.start.0..p.range.end.0).contains(&x.seq.0))
        .map(|x| x.tokens)
        .sum();
    assert_eq!(p.reclaimed_tokens, 实际);
}

#[test]
fn 遮蔽后的预计总量扣掉了回收部分() {
    let e = 超阈值(6);
    let total: u64 = e.iter().map(|x| x.tokens).sum();
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    assert_eq!(p.projected_tokens, total - p.reclaimed_tokens);
}

// ---------------------------------------------------------------------------
// 不重复压同一段
// ---------------------------------------------------------------------------

#[test]
fn 已压过的区间不再压() {
    // 反复摘要同一段历史会造成信息漂移：每压一次就离原文远一点。
    let e = 超阈值(6);
    let 第一次 = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();

    let 第二次 = plan(&e, &策略(), Trigger::Pressure, &[第一次.range.start]);
    // microcompact 那段被排除后，Compact 层会挑同一段——同样被排除。
    assert!(
        第二次.is_err(),
        "同一段历史被压了第二次：{:?}",
        第二次.map(|p| p.range)
    );
}

// ---------------------------------------------------------------------------
// 防重试循环
// ---------------------------------------------------------------------------

#[test]
fn 有回收才算推进() {
    let e = 超阈值(6);
    let p = plan(&e, &策略(), Trigger::Pressure, &[]).unwrap();
    assert!(p.makes_progress());
}

#[test]
fn 空区间不算推进() {
    // 允许它推进代际就等于允许无限重试。
    let 空 = CompactionPlan::default();
    assert!(!空.makes_progress());
}

#[test]
fn 全部条目不可动时给出终止信号而不是空计划() {
    // **这不是好消息**：说明光靠压缩已经救不了这个 Run，
    // 调用方应当停下问人而不是继续重试。
    let mut e = 超阈值(6);
    for x in e.iter_mut() {
        x.priority = Priority::SafetyRules;
    }
    assert_eq!(
        plan(&e, &策略(), Trigger::Overflow, &[]),
        Err(NoPlan::NothingCompactible)
    );
}

#[test]
fn 零成本条目不产生假推进() {
    // 一段 token 数全为 0 的历史压了也白压。若报成功，溢出重试会无限转。
    let e: Vec<Entry> = (0..6).map(|i| 工具输出(i, 0)).collect();
    let r = plan(&e, &策略(), Trigger::Overflow, &[]);
    assert!(
        matches!(r, Err(NoPlan::NoProgress) | Err(NoPlan::NothingCompactible)),
        "{r:?}"
    );
}

// ---------------------------------------------------------------------------
// 摘要必填项
// ---------------------------------------------------------------------------

#[test]
fn 完整摘要没有缺失项() {
    let s = SUMMARY_REQUIRED_SECTIONS.join("；");
    assert!(missing_sections(&s).is_empty());
}

#[test]
fn 漏项被逐条点名() {
    // 摘要由模型生成，**模型会漏**——所以要检查，
    // 而不是相信提示词里写了就一定照做。
    let s = "原始意图：修 bug。已做任务：改了两个文件。";
    let 缺 = missing_sections(s);
    assert!(缺.contains(&"未决审批"), "{缺:?}");
    assert!(缺.contains(&"当前 ChangeSet"), "{缺:?}");
    assert!(!缺.contains(&"原始意图"));
}

#[test]
fn 空摘要缺全部要点() {
    assert_eq!(missing_sections("").len(), SUMMARY_REQUIRED_SECTIONS.len());
}

// ---------------------------------------------------------------------------
// 确定性
// ---------------------------------------------------------------------------

#[test]
fn 相同输入产生相同计划() {
    // 压缩会写进事实流并影响缓存前缀；不确定就没法 replay。
    let e = 超阈值(7);
    let a = plan(&e, &策略(), Trigger::Pressure, &[]);
    let b = plan(&e, &策略(), Trigger::Pressure, &[]);
    assert_eq!(a, b);
}
