//! H5：`PolicyEnforcer` 的宿主义务检查。
//!
//! > 最终裁决者，必须记录裁决事实并对 `Allow` 承担审计责任。
//! > 违反后果：**审批链路不可审计**。
//!
//! ## 内核在这条上的处境
//!
//! 内核**不裁决**，它只消费裁决结果。这意味着一个错误的 Policy 实现可以让
//! 整套授权体系形同虚设，而内核完全看不出来——它拿到 `Allow` 就执行，
//! 那正是设计要求它做的事。
//!
//! 所以检查落在**裁决结果的自洽性**上：
//!
//! | 检查 | 违反后会怎样 |
//! |---|---|
//! | `Allow` 绑定到本次提议的 `input_hash` | 签出一张可挪用到任意输入上的通行证 |
//! | `Allow` 带有限期 | 凭证永不失效，一次批准等于永久批准 |
//! | 未知令牌 `redeem` 失败 | 编一个令牌就能取回一个"已批准" |
//! | 令牌一次性 | 一次审批被无限次兑现 |
//!
//! ## `DecisionSource` 里没有"模型"
//!
//! 这一条**不需要 conformance 检查**——`DecisionSource` 只有 `Human` 与
//! `Policy` 两个变体，第三方实现连表达"模型自己批准的"都做不到。
//! 内核不变量 15 在这里是由类型保证的，不是靠约定。

use std::sync::Arc;

use agentrs_contracts::ids::{ApprovalToken, Deadline, Timestamp};
use agentrs_contracts::policy::{ApprovalOutcome, ApprovalRequest, PolicyDecision, ToolProposal};
use agentrs_contracts::ports::PolicyEnforcer;

use super::{Check, Outcome, Report};

/// 被检查的策略实现。
pub trait PolicySubject: Send + Sync {
    /// 待检查的实现。
    fn policy(&self) -> Arc<dyn PolicyEnforcer>;

    /// 构造一份**该实现会放行**的提议。
    ///
    /// 没有它就没有合法起点——一个恒拒的 Policy 会让所有检查假通过。
    fn allowed_proposal(&self, tag: &str) -> ToolProposal;

    /// 构造一份**该实现会要求审批**的提议。返回 `None` 则跳过令牌相关检查。
    fn approval_proposal(&self, _tag: &str) -> Option<ToolProposal> {
        None
    }

    /// 当前逻辑时刻，用于判定 grant 是否真的有期限。
    fn now(&self) -> Timestamp {
        Timestamp(0)
    }
}

/// 对一个策略实现跑 H5 的全部检查。
pub async fn check_policy(subject: &dyn PolicySubject) -> Report {
    let p = subject.policy();
    let mut checks = Vec::new();
    let mut record = |name: &'static str, consequence: &'static str, outcome: Outcome| {
        checks.push(Check {
            obligation: "H5",
            name,
            consequence,
            outcome,
        });
    };

    // ---- 基线 ----
    let proposal = subject.allowed_proposal("conf-base");
    let decision = p.evaluate(proposal.clone()).await;
    let allow = match &decision {
        Ok(PolicyDecision::Allow {
            grant,
            bound_input_hash,
            expires_at,
        }) => {
            record("可以裁出 Allow", "后续所有检查都会假通过", Outcome::Pass);
            Some((grant.clone(), bound_input_hash.clone(), *expires_at))
        }
        other => {
            record(
                "可以裁出 Allow",
                "后续所有检查都会假通过",
                Outcome::Fail {
                    detail: format!("提供的提议未获放行：{other:?}"),
                },
            );
            None
        }
    };

    // ---- Allow 必须绑定到本次提议 ----
    if let Some((_, bound, expires)) = &allow {
        let outcome = if *bound == proposal.input_hash {
            Outcome::Pass
        } else {
            Outcome::Fail {
                detail: "签发的 grant 绑定到了别的输入指纹上".into(),
            }
        };
        record(
            "Allow 绑定到本次提议的 input_hash",
            "签出一张可挪用到任意输入上的通行证",
            outcome,
        );

        // ---- Allow 必须有期限 ----
        let now = subject.now();
        let outcome = if expires.0 <= now.0 {
            Outcome::Fail {
                detail: format!("签发即过期（expires_at={} ≤ now={}）", expires.0, now.0),
            }
        } else if expires.0 == i64::MAX {
            // **永不过期 = 一次批准等于永久批准。** 这在测试替身里常见，
            // 但产品实现里是个真问题，必须点名而不是放行。
            Outcome::Fail {
                detail: "grant 永不过期——一次批准等于永久批准".into(),
            }
        } else {
            Outcome::Pass
        };
        record(
            "Allow 带有限的有效期",
            "凭证永不失效，审批的时效性无从谈起",
            outcome,
        );
    }

    // ---- 令牌语义 ----
    {
        // 编一个从未签发过的令牌：必须失败，不能凭空取回一个"已批准"。
        let outcome = match p.redeem(ApprovalToken::new("conf-从未签发过的令牌")).await {
            Err(_) => Outcome::Pass,
            Ok(ApprovalOutcome::Pending { .. }) => Outcome::Fail {
                detail: "未知令牌被报告为 Pending——内核会一直挂着等一个不存在的裁决".into(),
            },
            Ok(ApprovalOutcome::Decided(d)) => Outcome::Fail {
                detail: format!("未知令牌兑出了一个裁决（allowed={}）", d.allowed),
            },
        };
        record("未知令牌 redeem 失败", "编一个令牌就能取回一个'已批准'", outcome);
    }

    {
        let outcome = match subject.approval_proposal("conf-tok") {
            None => Outcome::Skipped {
                why: "实现未提供会触发审批的提议".into(),
            },
            Some(prop) => {
                let req = ApprovalRequest {
                    step_id: "conf-step".into(),
                    proposal: prop,
                    risk_summary: "conformance".into(),
                    originating_member: None,
                    team_id: None,
                };
                // deadline 取当前时刻——立刻超时，逼出 Pending 与令牌。
                match p.await_approval(req, Deadline(subject.now())).await {
                    Ok(ApprovalOutcome::Pending { resume_token }) => {
                        let 第一次 = p.redeem(resume_token.clone()).await;
                        let 第二次 = p.redeem(resume_token).await;
                        match (第一次, 第二次) {
                            // 仍在等人裁决——两次都 Pending 是合理的，不算复用。
                            (Ok(ApprovalOutcome::Pending { .. }), _) => Outcome::Skipped {
                                why: "令牌仍处于未裁决状态，无法检查一次性".into(),
                            },
                            (Ok(ApprovalOutcome::Decided(_)), Ok(ApprovalOutcome::Decided(_))) => {
                                Outcome::Fail {
                                    detail: "同一令牌被兑现了两次".into(),
                                }
                            }
                            (Ok(ApprovalOutcome::Decided(_)), _) => Outcome::Pass,
                            (Err(e), _) => Outcome::Fail {
                                detail: format!("首次 redeem 失败：{e}"),
                            },
                        }
                    }
                    Ok(ApprovalOutcome::Decided(_)) => Outcome::Skipped {
                        why: "deadline 已到却直接裁决了，无令牌可检查".into(),
                    },
                    // **不在这里报 Fail。** `await_approval` 本身的行为由下一项
                    // 专门检查；两处都报会让"一个缺陷 → 一条不合格"的性质失效，
                    // 报告随之失去定位价值——修的人不知道该先看哪条。
                    Err(_) => Outcome::Skipped {
                        why: "await_approval 未能产出令牌，另见 deadline 检查".into(),
                    },
                }
            }
        };
        record("令牌一次性兑现", "一次审批被无限次兑现", outcome);
    }

    {
        // **绝不允许无限期阻塞**：deadline 已过时必须立刻回来。
        let outcome = match subject.approval_proposal("conf-deadline") {
            None => Outcome::Skipped {
                why: "实现未提供会触发审批的提议".into(),
            },
            Some(prop) => {
                let req = ApprovalRequest {
                    step_id: "conf-step2".into(),
                    proposal: prop,
                    risk_summary: "conformance".into(),
                    originating_member: None,
                    team_id: None,
                };
                // 给一个**已经过去**的 deadline。
                let past = Deadline(Timestamp(subject.now().0 - 1));
                match p.await_approval(req, past).await {
                    Ok(ApprovalOutcome::Pending { .. }) => Outcome::Pass,
                    Ok(ApprovalOutcome::Decided(_)) => Outcome::Pass,
                    Err(e) => Outcome::Fail {
                        detail: format!("已过期的 deadline 导致报错而非降级为挂起：{e}"),
                    },
                }
            }
        };
        record(
            "已过期的 deadline 立刻返回",
            "Run 的全部 live 资源随阻塞驻留内存，等价于句柄泄漏",
            outcome,
        );
    }

    Report { checks }
}
