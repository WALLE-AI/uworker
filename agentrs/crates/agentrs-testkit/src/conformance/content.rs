//! H4：`ContentStore` 的宿主义务检查。
//!
//! > 不得在 `retain` 有效期内回收内容。
//! > 违反后果：**模型请求不可重建**。
//!
//! ## 这条为什么难自证
//!
//! 内核不实现 GC——回收策略、冷热分层、配额全归 Core。因此"这份内容还在不在"
//! 这个问题，内核只能问、不能保证。它唯一能做的是**在 checkpoint 前 `retain`**，
//! 然后依赖宿主守约。
//!
//! 守约失败的表现极其滞后：Run 当时跑得好好的，几天后恢复才发现
//! `ModelRequestManifest` 引用的内容没了，请求重建不出来——
//! 而那时已经没人记得是谁回收的。
//!
//! ## 检查的落点
//!
//! 套件没法真的触发一次 GC（那是宿主内部行为），所以它验的是**可观察的契约**：
//!
//! - 写进去的读得回来，字节完全一致；
//! - 内容寻址：**同 scope 同字节必然同 ref**，否则去重与缓存全失效；
//! - `retain` 之后即使宿主认为该回收，`get` 仍必须成功；
//! - `release` 之后**不再承诺**可读——但也不得报"内容损坏"这类误导性错误；
//! - scope 越界必须是 `Forbidden` 而不是 `NotFound`，两者的排查方向完全不同。

use std::sync::Arc;

use agentrs_contracts::content::{ByteRange, ContentMeta, ContentScope, RetentionOwner};
use agentrs_contracts::ids::RunId;
use agentrs_contracts::ports::ContentStore;
use bytes::Bytes;

use super::{Check, Outcome, Report};

/// 被检查的内容存储实现。
pub trait ContentSubject: Send + Sync {
    /// 待检查的实现。
    fn store(&self) -> Arc<dyn ContentStore>;

    /// 本次检查使用的 scope。
    fn scope(&self) -> ContentScope {
        ContentScope::Run {
            run_id: RunId::new("conf-run"),
        }
    }

    /// 触发一次宿主侧回收。
    ///
    /// 返回 `false` 表示实现无法按需触发——相关检查记为 `Skipped`。
    /// **不猜**：没触发过回收就断言"retain 有效"，等于什么都没验。
    fn try_collect_garbage(&self) -> bool {
        false
    }

    /// 一个**别的** scope，用于越界检查。返回 `None` 则跳过该项。
    fn foreign_scope(&self) -> Option<ContentScope> {
        None
    }
}

fn owner() -> RetentionOwner {
    RetentionOwner::Checkpoint {
        run_id: RunId::new("conf-run"),
        up_to_seq: agentrs_contracts::ids::EventSequence(1),
    }
}

/// 对一个内容存储实现跑 H4 的全部检查。
pub async fn check_content(subject: &dyn ContentSubject) -> Report {
    let s = subject.store();
    let scope = subject.scope();
    let mut checks = Vec::new();
    let mut record = |name: &'static str, consequence: &'static str, outcome: Outcome| {
        checks.push(Check {
            obligation: "H4",
            name,
            consequence,
            outcome,
        });
    };

    let 原文 = Bytes::from_static(b"conformance-content-h4-payload");

    // ---- 基线：写得进、读得回 ----
    let put = s.put(scope.clone(), 原文.clone(), ContentMeta::default()).await;
    record(
        "可以写入并取回",
        "后续所有检查都会假通过",
        match &put {
            Ok(_) => Outcome::Pass,
            Err(e) => Outcome::Fail {
                detail: format!("写入失败：{e}"),
            },
        },
    );
    let Ok(r1) = put else {
        return Report { checks };
    };

    {
        let outcome = match s.get(&r1).await {
            Ok(b) if b == 原文 => Outcome::Pass,
            Ok(b) => Outcome::Fail {
                detail: format!("取回的字节与写入不一致（{} vs {} 字节）", b.len(), 原文.len()),
            },
            Err(e) => Outcome::Fail {
                detail: format!("取回失败：{e}"),
            },
        };
        record("取回的字节与写入完全一致", "模型请求重建出错误内容", outcome);
    }

    // ---- 内容寻址 ----
    {
        // 同 scope 同字节必须得到同一个 ref。不然去重、缓存前缀、
        // 以及"这两条历史引用的是不是同一份内容"全都无从判断。
        let outcome = match s.put(scope.clone(), 原文.clone(), ContentMeta::default()).await {
            Ok(r2) if r2.digest == r1.digest && r2.len == r1.len => Outcome::Pass,
            Ok(r2) => Outcome::Fail {
                detail: format!("同一份字节产生了不同引用：{:?} vs {:?}", r1.digest, r2.digest),
            },
            Err(e) => Outcome::Fail {
                detail: format!("重复写入失败：{e}"),
            },
        };
        record(
            "同 scope 同字节产生同一引用",
            "去重与缓存失效，无法判断两条历史是否引用同一份内容",
            outcome,
        );
    }

    // ---- 区间读 ----
    {
        let outcome = match s.get_range(&r1, ByteRange { start: 2, end: 7 }).await {
            Ok(b) if b == 原文.slice(2..7) => Outcome::Pass,
            Ok(b) => Outcome::Fail {
                detail: format!("区间读结果不符：{:?}", String::from_utf8_lossy(&b)),
            },
            Err(e) => Outcome::Fail {
                detail: format!("区间读失败：{e}"),
            },
        };
        record("区间读返回准确的切片", "大输出的分段回灌会取到错误内容", outcome);
    }

    // ---- stat 与 ref 自洽 ----
    {
        let outcome = match s.stat(&r1).await {
            Ok(st) if st.len == r1.len => Outcome::Pass,
            Ok(st) => Outcome::Fail {
                detail: format!("stat 的长度 {} 与引用中的 {} 不符", st.len, r1.len),
            },
            Err(e) => Outcome::Fail {
                detail: format!("stat 失败：{e}"),
            },
        };
        record("stat 与引用自洽", "预算估算与分段读都会算错", outcome);
    }

    // ---- retain 期内不得回收 ----
    {
        let outcome = if s.retain(owner(), std::slice::from_ref(&r1)).await.is_err() {
            Outcome::Fail {
                detail: "retain 调用本身失败".into(),
            }
        } else if !subject.try_collect_garbage() {
            Outcome::Skipped {
                why: "实现无法按需触发回收，无法验证 retain 的有效性".into(),
            }
        } else {
            match s.get(&r1).await {
                Ok(b) if b == 原文 => Outcome::Pass,
                Ok(_) => Outcome::Fail {
                    detail: "回收后内容被改变".into(),
                },
                Err(e) => Outcome::Fail {
                    detail: format!("retain 期内内容被回收了：{e}"),
                },
            }
        };
        record(
            "retain 期内内容不被回收",
            "恢复时 ModelRequestManifest 引用的内容已消失，请求重建不出来",
            outcome,
        );
    }

    // ---- release 之后 ----
    {
        // release 之后**不承诺**内容还在——但错误必须是"没有"，
        // 不能是"损坏"或"无权限"，那会把排查引到完全错误的方向。
        let outcome = if s.release(owner()).await.is_err() {
            Outcome::Fail {
                detail: "release 调用本身失败".into(),
            }
        } else if !subject.try_collect_garbage() {
            Outcome::Skipped {
                why: "实现无法按需触发回收".into(),
            }
        } else {
            use agentrs_contracts::content::ContentError;
            match s.get(&r1).await {
                // 还在也合格：release 只是解除保留，不是要求立刻删。
                Ok(_) => Outcome::Pass,
                Err(ContentError::NotFound { .. }) => Outcome::Pass,
                Err(e) => Outcome::Fail {
                    detail: format!("release 后的错误应为 NotFound，实际为 {e}"),
                },
            }
        };
        record(
            "release 后若已回收则报 NotFound",
            "把'已回收'报成'损坏'会把排查引向存储故障",
            outcome,
        );
    }

    // ---- scope 越界 ----
    {
        use agentrs_contracts::content::ContentError;
        let outcome = match subject.foreign_scope() {
            None => Outcome::Skipped {
                why: "实现未提供另一个 scope".into(),
            },
            Some(other) => {
                let mut 越界的 = r1.clone();
                越界的.scope = other;
                match s.get(&越界的).await {
                    Err(ContentError::Forbidden) => Outcome::Pass,
                    Err(ContentError::NotFound { .. }) => Outcome::Fail {
                        detail: "跨 scope 访问报 NotFound——应为 Forbidden，两者排查方向不同".into(),
                    },
                    Err(e) => Outcome::Fail {
                        detail: format!("跨 scope 访问的错误应为 Forbidden，实际为 {e}"),
                    },
                    Ok(_) => Outcome::Fail {
                        detail: "跨 scope 读到了不属于本 scope 的内容".into(),
                    },
                }
            }
        };
        record("跨 scope 访问报 Forbidden", "Run 之间的内容隔离失效", outcome);
    }

    Report { checks }
}
