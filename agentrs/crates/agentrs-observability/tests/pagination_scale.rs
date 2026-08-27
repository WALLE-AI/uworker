//! 百万事件下的分页查询性能（**M2 出口标准**）。
//!
//! > 百万事件下分页查询 P95 < 100 ms
//!
//! ## 为什么这条要单独测
//!
//! 分页的正确性（键稳定、不重不漏）在单元测试里已经覆盖，但那些用例只有
//! 十条事件。一个**每次翻页都把全量事件重排一遍**的实现在十条上完全正确、
//! 在一百万条上完全不可用——而长 Run 恰恰是 trajectory 最需要用的时候。
//!
//! 这条标准的实际含义是：**翻页的代价必须与页大小相关，不与日志总长相关。**
//!
//! 默认跑 20 万条（几秒内完成）。要跑满一百万：
//!
//! ```bash
//! AGENTRS_SCALE_EVENTS=1000000 cargo test -p agentrs-observability \
//!   --release --test pagination_scale -- --nocapture
//! ```
//!
//! **必须 `--release`**，本文件在 debug 下会自行跳过。两个理由：
//!
//! 1. debug 的常数因子淹没算法差异，测出来的数字不代表任何东西；
//! 2. `page()` 的有序性断言是 `debug_assert!`——它本身就是 O(n)，
//!    在 debug 下恰好把我们要测的那个代价又加了回来。
//!
//! ## 关于 `Instant::now`
//!
//! 边界门禁禁止内核读真实时钟（§1.1）。**本文件是性能基准**，
//! 测的就是墙钟耗时，没有别的办法。这是一处**有意的、局部的**豁免：
//! 只在测试里、只用于计时、不产生任何进入事实流的值。

// 见模块文档：性能基准必须读墙钟，这是一处有意的局部豁免。
#![allow(clippy::disallowed_methods)]

use std::time::Instant;

use agentrs_contracts::event::{Causality, Durability, EventPayload, RunEventEnvelope, Visibility};
use agentrs_contracts::ids::{EventSequence, RunEpoch, StepId, Timestamp, TurnId};
use agentrs_observability::projection::{page, Cursor, EventFilter};

/// debug 构建下跳过并说明原因。
///
/// 返回 `true` 表示应当跳过。**不静默跳过**——静默会让"全绿"变成假象。
fn 应当跳过() -> bool {
    if cfg!(debug_assertions) {
        eprintln!(
            "跳过：性能基准必须在 --release 下跑。\n\
             debug 的常数因子会淹没算法差异，且 page() 的有序性 debug_assert\n\
             本身是 O(n)，恰好把要测的代价又加了回来。"
        );
        return true;
    }
    false
}

fn 事件数() -> usize {
    std::env::var("AGENTRS_SCALE_EVENTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000)
}

/// 造一段长日志。形状贴近真实：Turn / Step 交替，工具调用穿插其间。
fn 长日志(n: usize) -> Vec<RunEventEnvelope> {
    let kinds = [
        EventPayload::StepStarted,
        EventPayload::ModelRequestPrepared {
            request_id: "q".into(),
        },
        EventPayload::AssistantMessage,
        EventPayload::ToolProposed { call_id: "c".into() },
        EventPayload::ToolStarted { call_id: "c".into() },
        EventPayload::UsageUpdated,
        EventPayload::StepEnded,
    ];
    (0..n)
        .map(|i| RunEventEnvelope {
            run_id: "r".into(),
            epoch: RunEpoch(1),
            event_id: format!("e{i}").into(),
            seq: Some(EventSequence(i as u64 + 1)),
            live_seq: None,
            at: Timestamp(i as i64),
            durability: Durability::DurableFact,
            visibility: Visibility::User,
            causality: Causality {
                turn_id: Some(TurnId::new(format!("t{}", i / 64))),
                step_id: Some(StepId::new(format!("s{}", i / 8))),
                ..Default::default()
            },
            surface: None,
            payload: kinds[i % kinds.len()].clone(),
        })
        .collect()
}

fn p95(mut xs: Vec<u128>) -> u128 {
    xs.sort_unstable();
    xs[(xs.len() * 95 / 100).min(xs.len() - 1)]
}

#[test]
fn 百万事件下翻页的_p95_低于_100ms() {
    if 应当跳过() {
        return;
    }
    let n = 事件数();
    let evs = 长日志(n);
    let f = EventFilter::default();

    // 从头连续翻 60 页，逐页计时。
    let mut cursor = Cursor::start();
    let mut 样本 = Vec::new();
    for _ in 0..60 {
        let t = Instant::now();
        let p = page(&evs, cursor, 100, &f);
        样本.push(t.elapsed().as_micros());
        match p.next {
            Some(c) => cursor = c,
            None => break,
        }
    }

    let p95_us = p95(样本.clone());
    let 最大 = 样本.iter().max().copied().unwrap_or(0);
    println!(
        "事件 {n} 条 ｜ 翻 {} 页 ｜ P95 {:.2} ms ｜ 最大 {:.2} ms",
        样本.len(),
        p95_us as f64 / 1000.0,
        最大 as f64 / 1000.0
    );

    assert!(
        p95_us < 100_000,
        "P95 {:.2} ms 超过 100 ms 上限。\n\
         这多半意味着翻页的代价与**日志总长**相关而不是与**页大小**相关——\
         检查是不是每次翻页都把全量事件重排了一遍。",
        p95_us as f64 / 1000.0
    );
}

#[test]
fn 翻页代价不随日志变长而增长() {
    if 应当跳过() {
        return;
    }
    // **比绝对数字更能说明问题的一条。** 100ms 的阈值随机器变，
    // 但"翻一页的代价与日志总长无关"是实现性质，不随机器变。
    let 小 = 长日志(20_000);
    let 大 = 长日志(200_000);
    let f = EventFilter::default();

    let 测 = |evs: &[RunEventEnvelope]| -> u128 {
        let mut 样本 = Vec::new();
        // 预热一次，避免首次分配影响。
        let _ = page(evs, Cursor::start(), 100, &f);
        for _ in 0..20 {
            let t = Instant::now();
            let _ = page(evs, Cursor::start(), 100, &f);
            样本.push(t.elapsed().as_micros());
        }
        p95(样本)
    };

    let a = 测(&小);
    let b = 测(&大);
    println!(
        "2 万条 P95 {:.2} ms ｜ 20 万条 P95 {:.2} ms ｜ 比值 {:.1}×",
        a as f64 / 1000.0,
        b as f64 / 1000.0,
        b as f64 / a.max(1) as f64
    );

    // 十倍长度换来的耗时增长不该超过四倍。线性实现约 10×，
    // 排序实现更差；常数时间实现约 1×。留出余量给测量噪声。
    assert!(
        b < a.max(50) * 4,
        "日志长十倍，翻页慢了 {:.1} 倍——代价跟着总长走了。\n\
         2 万条 {:.2} ms → 20 万条 {:.2} ms",
        b as f64 / a.max(1) as f64,
        a as f64 / 1000.0,
        b as f64 / 1000.0
    );
}

#[test]
fn 翻到日志末尾同样快() {
    if 应当跳过() {
        return;
    }
    // 末尾是**最坏情况**：一个"从头扫到游标"的实现在这里最慢，
    // 而用户查 trajectory 时最常看的恰恰是末尾。
    let n = 事件数();
    let evs = 长日志(n);
    let f = EventFilter::default();
    let 末尾 = Cursor {
        after_seq: Some(EventSequence(n as u64 - 200)),
    };

    let mut 样本 = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        let _ = page(&evs, 末尾, 100, &f);
        样本.push(t.elapsed().as_micros());
    }
    let v = p95(样本);
    println!("末尾翻页 P95 {:.2} ms", v as f64 / 1000.0);
    assert!(v < 100_000, "末尾翻页 P95 {:.2} ms 超限", v as f64 / 1000.0);
}

#[test]
fn 带过滤的翻页同样在预算内() {
    if 应当跳过() {
        return;
    }
    // 过滤会让"每页 100 条"需要扫更多原始事件——这是真实用法
    // （"只看工具调用"），不该因此爆掉预算。
    let n = 事件数();
    let evs = 长日志(n);
    let f = EventFilter {
        kinds: Some(vec!["ToolProposed"]),
        ..Default::default()
    };

    let mut 样本 = Vec::new();
    let mut cursor = Cursor::start();
    for _ in 0..20 {
        let t = Instant::now();
        let p = page(&evs, cursor, 100, &f);
        样本.push(t.elapsed().as_micros());
        match p.next {
            Some(c) => cursor = c,
            None => break,
        }
    }
    let v = p95(样本);
    println!("过滤翻页 P95 {:.2} ms", v as f64 / 1000.0);
    assert!(v < 100_000, "过滤翻页 P95 {:.2} ms 超限", v as f64 / 1000.0);
}
