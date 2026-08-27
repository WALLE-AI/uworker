//! 工具并发调度（架构 §8.3，任务 T09A）。
//!
//! ## 一条判定，一条纪律
//!
//! **判定**：只有 `ToolDef::concurrency_safe == true` 才允许并行。
//! 未知工具、未声明、拿不准，一律独占——`ToolDef` 的 `#[serde(default)]`
//! 让"缺字段"落到 `false`，这里的兜底与它同向。
//!
//! **纪律**：独占工具形成 **ordering barrier**。它之前的全部调用必须先
//! settlement，它自己单独跑，跑完之后才轮到后面的。
//!
//! ## 为什么必须是 barrier 而不是"独占地跑但可以和读并行"
//!
//! 模型在同一个 Step 里发出 `[Read a, Write a, Read a]` 是常见的。
//! 若只保证 Write 自身独占而不设 barrier，两个 Read 谁先谁后取决于调度，
//! 第二个 Read 可能读到写前的内容——**同一份历史在重放时会得到不同结果**，
//! 恢复的确定性就没了。barrier 让"第二个 Read 一定看到 Write 的结果"
//! 成为结构保证，而不是时序巧合。
//!
//! ## 结果顺序与执行顺序是两件事
//!
//! 批次内并行执行，但 [`Plan::reorder`] 保证结果按**原始 proposal order**
//! 回灌模型。乱序回灌会让 `tool_use` 与 `tool_result` 的配对在部分厂商上直接 400。

use agentrs_types::ToolDef;

use crate::toolround::ProposedCall;

/// 一个可并行执行的批次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// 本批次内调用在原始序列中的下标。
    pub indices: Vec<usize>,
    /// 是否为独占批次（恒含且仅含一个调用）。
    pub exclusive: bool,
}

/// 调度计划。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// 按依赖顺序排列的批次。**批次之间是 barrier，批次内可并行。**
    pub batches: Vec<Batch>,
}

impl Plan {
    /// 计划中的调用总数。
    pub fn len(&self) -> usize {
        self.batches.iter().map(|b| b.indices.len()).sum()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// 最大并行度。
    pub fn max_parallelism(&self) -> usize {
        self.batches.iter().map(|b| b.indices.len()).max().unwrap_or(0)
    }

    /// 把按批次收集的结果还原成**原始 proposal order**。
    ///
    /// `results` 里每一项是 `(原始下标, 结果)`。缺项返回 `None`——
    /// **宁可报缺失也不要用默认值补位**，那会让模型收到一条它没请求过的结果。
    pub fn reorder<T>(&self, mut results: Vec<(usize, T)>) -> Option<Vec<T>> {
        let n = self.len();
        if results.len() != n {
            return None;
        }
        results.sort_by_key(|(i, _)| *i);
        // 下标必须恰好是 0..n 的一个排列。
        if results.iter().enumerate().any(|(k, (i, _))| k != *i) {
            return None;
        }
        Some(results.into_iter().map(|(_, r)| r).collect())
    }
}

/// 判断一个工具是否允许并行。**目录里找不到就是独占。**
fn is_concurrent(name: &str, catalog: &[ToolDef]) -> bool {
    catalog
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.concurrency_safe)
        // fail-closed：不在目录里的工具不该被调度，但即便走到这里也必须串行。
        .unwrap_or(false)
}

/// 为一组调用生成调度计划。
pub fn plan(calls: &[ProposedCall], catalog: &[ToolDef]) -> Plan {
    let mut batches: Vec<Batch> = Vec::new();
    let mut current: Vec<usize> = Vec::new();

    for (i, call) in calls.iter().enumerate() {
        if is_concurrent(&call.tool_name, catalog) {
            current.push(i);
            continue;
        }
        // 遇到独占工具：先关掉在攒的并行批次（barrier 的前半），
        // 再让它自己单独成批（barrier 的后半）。
        if !current.is_empty() {
            batches.push(Batch {
                indices: std::mem::take(&mut current),
                exclusive: false,
            });
        }
        batches.push(Batch {
            indices: vec![i],
            exclusive: true,
        });
    }

    if !current.is_empty() {
        batches.push(Batch {
            indices: current,
            exclusive: false,
        });
    }

    Plan { batches }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn 目录() -> Vec<ToolDef> {
        vec![
            ToolDef::read_only("Read", "读", json!({})),
            ToolDef::read_only("Grep", "搜", json!({})),
            ToolDef::mutating("Write", "写", json!({})),
        ]
    }

    fn 调用(names: &[&str]) -> Vec<ProposedCall> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| ProposedCall {
                call_id: format!("c{i}").into(),
                tool_name: (*n).to_owned(),
                arguments: json!({}),
            })
            .collect()
    }

    /// 把计划压成便于断言的形状：每个批次的下标 + 是否独占。
    fn 形状(p: &Plan) -> Vec<(Vec<usize>, bool)> {
        p.batches
            .iter()
            .map(|b| (b.indices.clone(), b.exclusive))
            .collect()
    }

    #[test]
    fn 全只读合成一个并行批次() {
        let p = plan(&调用(&["Read", "Grep", "Read"]), &目录());
        assert_eq!(形状(&p), [(vec![0, 1, 2], false)]);
        assert_eq!(p.max_parallelism(), 3);
    }

    #[test]
    fn 写类工具形成前后双向屏障() {
        // 这是本模块的核心：Write 之前的必须先完成，之后的必须等它。
        let p = plan(&调用(&["Read", "Write", "Read"]), &目录());
        assert_eq!(
            形状(&p),
            [(vec![0], false), (vec![1], true), (vec![2], false)],
            "Write 必须把序列切成三段"
        );
    }

    #[test]
    fn 读写读的第二个读一定看到写的结果() {
        // 若只让 Write 独占而不设 barrier，两个 Read 的先后取决于调度，
        // 同一份历史重放会得到不同结果。这里断言的是结构，不是时序。
        let p = plan(&调用(&["Read", "Write", "Read"]), &目录());
        let write_batch = p.batches.iter().position(|b| b.exclusive).unwrap();
        let read2_batch = p.batches.iter().position(|b| b.indices.contains(&2)).unwrap();
        assert!(read2_batch > write_batch, "第二个 Read 必须排在 Write 之后");
    }

    #[test]
    fn 连续两个写各自独占() {
        let p = plan(&调用(&["Write", "Write"]), &目录());
        assert_eq!(形状(&p), [(vec![0], true), (vec![1], true)]);
        assert_eq!(p.max_parallelism(), 1);
    }

    #[test]
    fn 写在首位时不产生空批次() {
        let p = plan(&调用(&["Write", "Read", "Read"]), &目录());
        assert_eq!(形状(&p), [(vec![0], true), (vec![1, 2], false)]);
        assert!(p.batches.iter().all(|b| !b.indices.is_empty()));
    }

    #[test]
    fn 写在末位时并行批次先收口() {
        let p = plan(&调用(&["Read", "Read", "Write"]), &目录());
        assert_eq!(形状(&p), [(vec![0, 1], false), (vec![2], true)]);
    }

    #[test]
    fn 不在目录里的工具按独占处理() {
        // fail-closed。它随后会被 Policy 拒掉，但**调度阶段就不能让它并行**——
        // 拿不准的东西并行跑是最难复现的一类问题。
        let p = plan(&调用(&["Read", "Mystery", "Read"]), &目录());
        assert_eq!(形状(&p), [(vec![0], false), (vec![1], true), (vec![2], false)]);
    }

    #[test]
    fn 空调用产生空计划() {
        let p = plan(&[], &目录());
        assert!(p.is_empty());
        assert_eq!(p.max_parallelism(), 0);
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn 计划覆盖全部调用且不重不漏() {
        let calls = 调用(&["Read", "Write", "Grep", "Read", "Write"]);
        let p = plan(&calls, &目录());
        let mut all: Vec<usize> = p.batches.iter().flat_map(|b| b.indices.clone()).collect();
        assert_eq!(p.len(), calls.len());
        all.sort_unstable();
        assert_eq!(all, (0..calls.len()).collect::<Vec<_>>());
    }

    #[test]
    fn 独占批次恒为单元素() {
        let p = plan(&调用(&["Write", "Write", "Read", "Write"]), &目录());
        for b in &p.batches {
            if b.exclusive {
                assert_eq!(b.indices.len(), 1, "独占批次不能装两个调用");
            }
        }
    }

    #[test]
    fn 结果按原始顺序还原() {
        let p = plan(&调用(&["Read", "Write", "Read"]), &目录());
        // 模拟并发完成：顺序被打乱。
        let 乱序 = vec![(2, "第三"), (0, "第一"), (1, "第二")];
        assert_eq!(p.reorder(乱序), Some(vec!["第一", "第二", "第三"]));
    }

    #[test]
    fn 结果缺项时报缺失而不是补位() {
        // 用默认值补位会让模型收到一条它没请求过的结果，
        // 而这条假结果会进入历史、被后续 Step 当成事实。
        let p = plan(&调用(&["Read", "Write"]), &目录());
        assert_eq!(p.reorder(vec![(0, "只有一个")]), None);
    }

    #[test]
    fn 结果下标重复时报错() {
        let p = plan(&调用(&["Read", "Write"]), &目录());
        assert_eq!(p.reorder(vec![(0, "a"), (0, "b")]), None);
    }

    #[test]
    fn 声明为并发的工具缺字段时按独占处理() {
        // ToolDef 的 concurrency_safe 有 #[serde(default)]，
        // 老版本或第三方注册没写这个字段时落到 false。这里与它同向。
        let 目录 = vec![
            serde_json::from_str::<ToolDef>(r#"{"name":"X","description":"d","parameters":{}}"#).unwrap(),
        ];
        let p = plan(&调用(&["X", "X"]), &目录);
        assert!(p.batches.iter().all(|b| b.exclusive));
    }
}
