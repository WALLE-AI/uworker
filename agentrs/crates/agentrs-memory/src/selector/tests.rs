//! `memorySelector` 的选择语义测试。
//!
//! 两条约束贯穿全部用例：**只能从候选里选**、**不确定时少选**。

use super::*;

fn 候选(items: &[(&str, u32)]) -> Vec<MemoryCandidate> {
    items
        .iter()
        .map(|(id, rank)| MemoryCandidate {
            id: MemoryId::new(*id),
            summary: format!("摘要 {id}"),
            rank: *rank,
        })
        .collect()
}

fn 标准候选() -> Vec<MemoryCandidate> {
    候选(&[("m1", 3), ("m2", 1), ("m3", 2), ("m4", 5), ("m5", 4), ("m6", 6)])
}

fn ids(xs: &[&str]) -> Vec<MemoryId> {
    xs.iter().map(|s| MemoryId::new(*s)).collect()
}

// ---------------------------------------------------------------------------
// 只能从候选里选
// ---------------------------------------------------------------------------

#[test]
fn 选了候选之外的_id_被整体退回() {
    // **最要紧的一条**：它意味着 selector 在编造。
    // 一旦编造了一个 id，它对其余几条的判断也不再可信——
    // 所以整体退回排名，而不是"把越界的挑出来扔掉"。
    let c = 标准候选();
    let s = decide(Some(ids(&["m2", "凭空捏造的"])), &c, 5);

    assert_eq!(
        s.source,
        SelectionSource::RankFallback {
            reason: "selector_out_of_candidates"
        }
    );
    assert_eq!(s.ids, by_rank(&c, 5), "应当整体退回排名");
}

#[test]
fn 校验直接点名越界的_id() {
    match validate(&ids(&["m2", "幽灵", "鬼魂"]), &标准候选(), 5) {
        Err(Rejected::OutOfCandidates { unknown }) => {
            assert_eq!(unknown, ["幽灵", "鬼魂"]);
        }
        other => panic!("期望 OutOfCandidates，得到 {other:?}"),
    }
}

#[test]
fn 合法选择原样通过() {
    let c = 标准候选();
    let s = decide(Some(ids(&["m2", "m3"])), &c, 5);
    assert_eq!(s.source, SelectionSource::Selector);
    assert_eq!(s.ids, ids(&["m2", "m3"]));
}

// ---------------------------------------------------------------------------
// 只能降低数量
// ---------------------------------------------------------------------------

#[test]
fn 超过上限被退回() {
    let c = 标准候选();
    let s = decide(Some(ids(&["m1", "m2", "m3", "m4", "m5", "m6"])), &c, 5);
    assert_eq!(
        s.source,
        SelectionSource::RankFallback {
            reason: "selector_over_limit"
        }
    );
    assert_eq!(s.ids.len(), 5);
}

#[test]
fn 重复的_id_被退回() {
    // 同一条注两遍既浪费预算，也会让模型以为这件事更重要。
    let c = 标准候选();
    let s = decide(Some(ids(&["m2", "m2"])), &c, 5);
    assert_eq!(
        s.source,
        SelectionSource::RankFallback {
            reason: "selector_duplicated"
        }
    );
}

#[test]
fn 一条都不选是合法的() {
    // **不确定就不选。** 空选择与"没跑 selector"含义不同，
    // 前者是它看了之后决定都不要——这是被鼓励的行为，不该走降级。
    let c = 标准候选();
    let s = decide(Some(vec![]), &c, 5);
    assert_eq!(s.source, SelectionSource::Selector);
    assert!(s.ids.is_empty());
}

// ---------------------------------------------------------------------------
// 降级
// ---------------------------------------------------------------------------

#[test]
fn selector_不可用时退回排名() {
    let c = 标准候选();
    let s = decide(None, &c, 5);
    assert_eq!(
        s.source,
        SelectionSource::RankFallback {
            reason: "selector_unavailable"
        }
    );
    assert_eq!(s.ids, by_rank(&c, 5));
}

#[test]
fn 降级必须留痕() {
    // "记忆怎么突然变差了"要能查出来。
    for picked in [
        None,
        Some(ids(&["不存在"])),
        Some(ids(&["m1", "m1"])),
        Some(ids(&["m1", "m2", "m3", "m4", "m5", "m6"])),
    ] {
        let s = decide(picked, &标准候选(), 5);
        assert!(
            matches!(s.source, SelectionSource::RankFallback { .. }),
            "降级没留痕：{:?}",
            s.source
        );
    }
}

#[test]
fn 没有候选时既不选也不降级() {
    // 降级是"selector 没用上"，无候选是"本来就没东西可选"——两回事。
    let s = decide(None, &[], 5);
    assert_eq!(s.source, SelectionSource::Empty);
    assert!(s.ids.is_empty());
}

// ---------------------------------------------------------------------------
// 排名降级路径本身
// ---------------------------------------------------------------------------

#[test]
fn 排名按_rank_升序() {
    assert_eq!(by_rank(&标准候选(), 3), ids(&["m2", "m3", "m1"]));
}

#[test]
fn 排名相同时按_id_定序而不是输入顺序() {
    // 检索实现返回的顺序未必稳定。靠它定序会让 replay 结果漂移。
    let a = 候选(&[("b", 1), ("a", 1), ("c", 1)]);
    let b = 候选(&[("c", 1), ("b", 1), ("a", 1)]);
    assert_eq!(by_rank(&a, 3), by_rank(&b, 3));
    assert_eq!(by_rank(&a, 3), ids(&["a", "b", "c"]));
}

#[test]
fn 排名不超过上限() {
    assert_eq!(by_rank(&标准候选(), 2).len(), 2);
    // 候选比上限少时取全部。
    assert_eq!(by_rank(&候选(&[("x", 1)]), 5).len(), 1);
}

#[test]
fn 默认上限是五条() {
    assert_eq!(DEFAULT_LIMIT, 5);
}

// ---------------------------------------------------------------------------
// 输出解析
// ---------------------------------------------------------------------------

#[test]
fn 解析结构化输出() {
    assert_eq!(
        parse_output(r#"{"selected": ["m1", "m3"]}"#),
        Some(ids(&["m1", "m3"]))
    );
}

#[test]
fn 容忍_json_前后的解释文字() {
    // 模型常在 JSON 前后加一段说明。为这个把整次选择判为失败太苛刻。
    let t = "我认为这两条相关：\n{\"selected\": [\"m1\"]}\n希望有帮助。";
    assert_eq!(parse_output(t), Some(ids(&["m1"])));
}

#[test]
fn 解析失败返回_none_而不是空选择() {
    // **两者含义不同**：None 走降级（selector 坏了），
    // 空选择是"我看了但一条都不要"（selector 正常工作）。
    assert_eq!(parse_output("完全不是 JSON"), None);
    assert_eq!(parse_output(r#"{"wrong_key": []}"#), None);
    assert_eq!(parse_output(r#"{"selected": "不是数组"}"#), None);
    assert_eq!(parse_output(r#"{"selected": [123]}"#), None, "id 必须是字符串");
}

#[test]
fn 空数组解析为空选择() {
    assert_eq!(parse_output(r#"{"selected": []}"#), Some(vec![]));
}

// ---------------------------------------------------------------------------
// 提示词
// ---------------------------------------------------------------------------

#[test]
fn 提示词写明三条约束() {
    let p = system_prompt(5);
    assert!(p.contains("最多选 5 条"));
    assert!(p.contains("只能从候选里选"));
    assert!(p.contains("不确定就不选"));
    assert!(p.contains("没有任何工具"));
}

#[test]
fn 提示词解释了为什么宁可少选() {
    // 光说"少选"模型不一定照做；给出理由更可能生效。
    assert!(system_prompt(5).contains("把模型带偏"));
}

// ---------------------------------------------------------------------------
// 确定性
// ---------------------------------------------------------------------------

#[test]
fn 相同输入产生相同选择() {
    // 记忆注入会进请求、影响缓存前缀；不确定就没法 replay。
    let c = 标准候选();
    assert_eq!(decide(None, &c, 5), decide(None, &c, 5));
    assert_eq!(
        decide(Some(ids(&["m2"])), &c, 5),
        decide(Some(ids(&["m2"])), &c, 5)
    );
}
