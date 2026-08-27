//! 事件日志的只读子命令：`validate` / `trajectory` / `cache-report` / `replay`。
//!
//! 四条都是**纯读**：加载 durable 事件、折叠投影、打印。它们不写事实流、
//! 不触达 provider、不执行任何工具——因此在一个坏掉的 Run 上也能跑，
//! 而那正是最需要它们的时候。
//!
//! ## 为什么这几条要分开
//!
//! | 子命令 | 回答的问题 |
//! |---|---|
//! | `validate` | 这份日志**本身**有没有问题（能不能信它） |
//! | `trajectory` | 这次 Run 做了什么 |
//! | `cache-report` | 钱花在哪、每次 miss 因为什么 |
//! | `replay` | 同一份前缀是否**确定性地**产生同一份快照 |
//! | `export` | 产出一份可对外分享的脱敏 bundle |
//!
//! 第一条必须独立于其余三条：如果日志本身自相矛盾，
//! 后三条给出的答案就是在一份不可信的输入上做的推断。

use agentrs_contracts::event::{EventPayload, RunEventEnvelope};
use agentrs_contracts::ids::EventSequence;
use agentrs_dev_adapter::JsonlPersistence;
use agentrs_observability::projection::{defs, page, Cursor, EventFilter, ProjectionKey, ProjectionRegistry};

type R = Result<(), Box<dyn std::error::Error>>;

fn 注册表() -> ProjectionRegistry {
    let mut r = ProjectionRegistry::new();
    defs::register_default(&mut r).expect("首批投影注册失败");
    r
}

fn 加载(path: &str) -> Result<(JsonlPersistence, Vec<RunEventEnvelope>), Box<dyn std::error::Error>> {
    let p = JsonlPersistence::open(path)?;
    let events = p.load_events()?;
    Ok((p, events))
}

// ---------------------------------------------------------------------------
// validate
// ---------------------------------------------------------------------------

/// 校验一份事件日志的自洽性。
///
/// 检查的都是**内核不变量在日志上的投影**——不是"内容对不对"，
/// 而是"这份日志能不能作为恢复的依据"。
pub fn validate(path: &str) -> R {
    let (p, events) = 加载(path)?;
    println!("事件日志：{path}");
    println!("durable 事件：{} 条\n", events.len());

    let mut 问题: Vec<String> = Vec::new();

    // 1. 写坏的行。
    let 坏行 = p.corrupt_line_count()?;
    if 坏行 > 0 {
        问题.push(format!(
            "{坏行} 行无法解析——日志有缺口，恢复只能到最后一条完好的记录"
        ));
    }

    // 2. 每条 durable 事件都必须有序号。
    let 无序号 = events.iter().filter(|e| e.seq.is_none()).count();
    if 无序号 > 0 {
        问题.push(format!("{无序号} 条 durable 事件没有序号——它们无法参与有序折叠"));
    }

    // 3. 序号严格递增。
    let seqs: Vec<EventSequence> = events.iter().filter_map(|e| e.seq).collect();
    if !seqs.windows(2).all(|w| w[0] < w[1]) {
        问题.push("序号非严格递增——投影与分页游标都不可信".into());
    }

    // 4. event_id 唯一。**去重键重复意味着两条记录声称是同一件事。**
    let mut ids: Vec<&str> = events.iter().map(|e| e.event_id.as_str()).collect();
    let 总数 = ids.len();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != 总数 {
        问题.push(format!(
            "{} 个重复的 event_id——消费端按 (run_id, event_id) 去重会得到矛盾的结果",
            总数 - ids.len()
        ));
    }

    // 5. epoch 不倒退。
    let mut 上一个 = 0;
    for e in &events {
        if e.epoch.0 < 上一个 {
            问题.push(format!(
                "epoch 倒退（{} → {}）——说明有两个 writer 写过这份日志",
                上一个, e.epoch.0
            ));
            break;
        }
        上一个 = e.epoch.0;
    }

    // 6. 悬挂意图：有 StepIntent 而无 StepResult。
    let reg = 注册表();
    let snap = reg.snapshot(ProjectionKey("tool_paths"), &events, None)?;
    let v: defs::ToolPathsView = serde_json::from_value(snap.view)?;
    if !v.dangling.is_empty() {
        // **这不一定是错误**——崩溃时正常会留下悬挂意图，恢复时向
        // Sandbox reconcile 即可。但它必须被点名，否则没人知道要去 reconcile。
        println!("待 reconcile 的意图（有意图无结果，恢复时须向 Sandbox 核对）：");
        for c in &v.dangling {
            println!("  - {c}");
        }
        println!();
    }

    // 7. checkpoint 不得超前。
    if let Some(cp) = p.load_checkpoint()? {
        let 最大 = seqs.last().copied().unwrap_or(EventSequence(0));
        if cp.up_to_seq > 最大 {
            问题.push(format!(
                "checkpoint 指向 {:?}，但日志只到 {:?}——恢复会读到一个不存在的位置",
                cp.up_to_seq, 最大
            ));
        }
    }

    if 问题.is_empty() {
        println!("✅ 日志自洽，可作为恢复依据");
        Ok(())
    } else {
        for q in &问题 {
            println!("❌ {q}");
        }
        Err(format!("{} 项不自洽", 问题.len()).into())
    }
}

// ---------------------------------------------------------------------------
// trajectory
// ---------------------------------------------------------------------------

/// 打印 Run 的轨迹。
pub fn trajectory(path: &str, limit: usize) -> R {
    let (_, events) = 加载(path)?;
    let reg = 注册表();

    let sk: defs::SkeletonView =
        serde_json::from_value(reg.snapshot(ProjectionKey("skeleton"), &events, None)?.view)?;
    println!("骨架");
    println!(
        "  已开始 {} ｜ 终态 {} ｜ Turn {}（其中 0-Step {}）",
        sk.started,
        sk.terminal.as_deref().unwrap_or("（未终结）"),
        sk.turns.len(),
        sk.zero_step_turns
    );
    for t in &sk.turns {
        println!(
            "  Turn {} ｜ Step {} ｜ {}",
            if t.turn_id.is_empty() {
                "(无 id)"
            } else {
                &t.turn_id
            },
            t.steps.len(),
            if t.ended { "已闭合" } else { "未闭合" }
        );
    }

    let rq: defs::RequestsView =
        serde_json::from_value(reg.snapshot(ProjectionKey("requests"), &events, None)?.view)?;
    println!(
        "\n模型请求：{} 次 ｜ 助手消息 {} 条 ｜ 已产出可见输出 {} 次",
        rq.request_ids.len(),
        rq.assistant_messages,
        rq.partial_outputs
    );

    let tp: defs::ToolPathsView =
        serde_json::from_value(reg.snapshot(ProjectionKey("tool_paths"), &events, None)?.view)?;
    if !tp.calls.is_empty() {
        println!("\n工具路径");
        for c in &tp.calls {
            println!(
                "  {} {} ｜ 提议{} Hook{} 意图{} 启动{} → {}",
                c.call_id,
                c.tool_name.as_deref().unwrap_or("?"),
                勾(c.proposed),
                勾(c.hooked),
                勾(c.intent_recorded),
                勾(c.started),
                c.outcome.as_deref().unwrap_or("（无结果·待 reconcile）"),
            );
        }
    }

    let ap: defs::ApprovalsView =
        serde_json::from_value(reg.snapshot(ProjectionKey("approvals"), &events, None)?.view)?;
    if !ap.spans.is_empty() {
        println!("\n审批");
        for s in &ap.spans {
            match s.waited_ms() {
                Some(ms) => println!("  {} ｜ 等待 {ms}ms 后超时挂起", s.call_id),
                None => println!("  {} ｜ 仍在等待", s.call_id),
            }
        }
    }

    println!("\n最近 {limit} 条事件");
    let all = page(&events, Cursor::start(), usize::MAX, &EventFilter::default());
    let 起点 = all.items.len().saturating_sub(limit);
    for e in &all.items[起点..] {
        println!(
            "  #{:<4} {}",
            e.seq.map(|s| s.0).unwrap_or(0),
            agentrs_observability::projection::payload_kind(&e.payload)
        );
    }
    Ok(())
}

fn 勾(b: bool) -> &'static str {
    if b {
        "✓"
    } else {
        "·"
    }
}

// ---------------------------------------------------------------------------
// cache-report
// ---------------------------------------------------------------------------

/// 打印缓存命中与断裂归因。
pub fn cache_report(path: &str) -> R {
    let (_, events) = 加载(path)?;
    let reg = 注册表();
    let v: defs::CacheView =
        serde_json::from_value(reg.snapshot(ProjectionKey("cache"), &events, None)?.view)?;

    println!("请求 {} 次 ｜ 观测到缓存断裂 {} 次", v.requests, v.cache_breaks);
    match v.unbroken_rate() {
        // **没有请求时不报 0%**——那会被读成"全 miss"。
        None => println!("前缀未断裂比例：（尚无请求）"),
        Some(r) => println!("前缀未断裂比例：{:.1}%", r * 100.0),
    }
    if !v.observed_any_break() && v.requests > 0 {
        // **这不是"缓存 100% 命中"。**
        println!(
            "\n注意：全程未观测到断裂。这只说明我们发出去的前缀是稳定的，\n\
             不代表端点那边真的命中了——后者要看端点报告的 cache_read_tokens。\n\
             一个根本没开缓存的端点，这一行照样是 100%。"
        );
    }
    println!(
        "历史合法化 {} 次 ｜ 内容引用解析失败 {} 次",
        v.legalizations, v.unresolved_refs
    );

    if !v.compacted_ranges.is_empty() {
        println!("\n已压缩区间（摘要可复用，不重复压同一段）");
        for (a, b) in &v.compacted_ranges {
            println!("  [{}, {})", a.0, b.0);
        }
    }

    let 断裂 = page(
        &events,
        Cursor::start(),
        usize::MAX,
        &EventFilter {
            kinds: Some(vec!["CacheBreakObserved"]),
            ..Default::default()
        },
    );
    if !断裂.items.is_empty() {
        println!("\n断裂发生在");
        for e in &断裂.items {
            println!("  #{}", e.seq.map(|s| s.0).unwrap_or(0));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------

/// 校验投影的确定性：同一份前缀必须产生同一份快照。
///
/// **这不是"重跑一遍 Run"**——重跑会再次调用模型与工具，那是 `resume` 的事。
/// replay 只重放**折叠**，验证读模型这一层是纯的。
pub fn replay(path: &str) -> R {
    let (_, events) = 加载(path)?;
    let reg = 注册表();
    let keys = reg.keys();

    println!("事件 {} 条 ｜ 投影 {} 个\n", events.len(), keys.len());

    let mut 不一致 = Vec::new();

    for k in &keys {
        // 1. 同一份输入折两次，必须完全相同。
        let a = reg.snapshot(*k, &events, None)?;
        let b = reg.snapshot(*k, &events, None)?;
        if a != b {
            不一致.push(format!("{k}：同一份输入折两次结果不同"));
            continue;
        }

        // 2. 打乱到达顺序，结果必须不变——重投递与并发写入会造成乱序。
        let mut 乱序 = events.clone();
        乱序.reverse();
        let c = reg.snapshot(*k, &乱序, None)?;
        if a.view != c.view {
            不一致.push(format!("{k}：到达顺序影响了快照——折叠前未按 seq 定序"));
            continue;
        }

        // 3. 逐段前缀折叠，与一次性折到底必须一致。
        //    这条抓的是"投影里藏了跨调用的状态"。
        if let Some(mid) = events.get(events.len() / 2).and_then(|e| e.seq) {
            let 半程 = reg.snapshot(*k, &events, Some(mid))?;
            let 半程再来 = reg.snapshot(*k, &events, Some(mid))?;
            if 半程 != 半程再来 {
                不一致.push(format!("{k}：as_of 截断不确定"));
                continue;
            }
        }

        println!("✅ {k} ｜ state_version {}", a.state_version);
    }

    if 不一致.is_empty() {
        println!("\n全部投影确定性成立");
        Ok(())
    } else {
        println!();
        for x in &不一致 {
            println!("❌ {x}");
        }
        Err(format!("{} 个投影不确定", 不一致.len()).into())
    }
}

// ---------------------------------------------------------------------------
// resume 的只读部分
// ---------------------------------------------------------------------------

/// 打印恢复计划：从日志能读出的"崩溃时正在做什么"。
///
/// **只读**。真正的续跑要向 Sandbox `reconcile` 并重新装配 live 资源，
/// 那需要完整的 adapter；这里先把"该做什么"说清楚。
pub fn resume_plan(path: &str) -> R {
    let (_, events) = 加载(path)?;
    let plan = agentrs_runtime::recovery::plan(&events);

    println!("事件 {} 条\n", events.len());
    println!("恢复计划");
    println!("  {plan:#?}");

    // 悬挂意图单独点名——它决定了要不要向 Sandbox 核对。
    let reg = 注册表();
    let v: defs::ToolPathsView =
        serde_json::from_value(reg.snapshot(ProjectionKey("tool_paths"), &events, None)?.view)?;
    if v.dangling.is_empty() {
        println!("\n无待 reconcile 的意图，可直接续跑。");
    } else {
        println!("\n**必须先 reconcile 的调用**（有意图无结果）：");
        for c in &v.dangling {
            println!("  - {c}");
        }
        println!("\n内核不猜：Sandbox 报 NotStarted 才可重试，报 Unknown 必须停下问人。");
    }

    let 终态 = events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::RunCompleted => Some("已正常完成"),
        EventPayload::RunFailed => Some("已失败"),
        EventPayload::RunCanceled => Some("已取消"),
        EventPayload::RunNeedsUserAction => Some("等待用户介入"),
        _ => None,
    });
    if let Some(t) = 终态 {
        println!("\n该 Run {t}。");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// 导出一份脱敏 replay bundle。
///
/// **默认无敏感正文与绝对路径**（M2 出口标准）。导出前会跑一遍自检，
/// 有泄漏就拒绝导出——脱敏规则会漏，而漏了没人会发现，除非有这道自检。
pub fn export(path: &str, workspace: Option<String>, salt: &str) -> R {
    use agentrs_observability::redact::{BundleSalt, Redactor, ReplayBundle};

    let (_, events) = 加载(path)?;
    let reg = 注册表();
    let versions: std::collections::BTreeMap<String, u32> = reg
        .keys()
        .iter()
        .filter_map(|k| reg.state_version(*k).map(|v| (k.0.to_string(), v)))
        .collect();

    let redactor = Redactor::new(BundleSalt(salt.to_string()), workspace);
    let bundle = ReplayBundle::build(
        agentrs_contracts::version::SpecVersion(1),
        &redactor,
        &events,
        versions,
    );

    let leaks = bundle.audit();
    println!(
        "事件 {} 条 ｜ 投影版本 {} 项",
        bundle.events.len(),
        bundle.projection_versions.len()
    );
    println!("脱敏盐：{}（已记入 bundle 头部）", bundle.salt.0);

    if !leaks.is_empty() {
        println!("\n❌ 自检发现 {} 处疑似泄漏：", leaks.len());
        for l in &leaks {
            println!("  {} ｜ {:?} ｜ {}", l.event_id, l.kind, l.sample);
        }
        return Err("自检未通过，拒绝导出".into());
    }

    println!("\n✅ 自检通过：无绝对路径、无疑似密钥");
    println!("\n提示：命令输出与文件正文只留长度，敏感正文需由 Core 按可见性另行导出。");
    Ok(())
}
