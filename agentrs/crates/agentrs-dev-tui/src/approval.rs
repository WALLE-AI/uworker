// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/approval-options.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; the wider row switches AgentRS's
//            PermissionMode for the next run, since the kernel has no
//            in-session transition path.
//! The answers an approval may be given.
//!
//! Two rows — `allow once` and `reject` — are the whole reason people turn
//! approvals off: the only way to stop being asked the same question is to stop
//! being asked any question. So the panel grows the two answers that were always
//! available and simply had nowhere to be typed.
//!
//! What it does *not* grow is a decision the kernel cannot honour.
//! [`ApprovalAnswer`](agentrs_dev_adapter::ApprovalAnswer) is a closed set of
//! two, and there is no rule-persisting grant to reach for — 内核不变量 15 puts
//! the decision with a human, every time. So the wider answers are composed out
//! of things that already exist and say exactly what they do:
//!
//! - **allow once, then <mode>** is a one-shot grant *plus* the permission-mode
//!   change `shift+tab` would make. The label names the mode, because the user
//!   is changing the rules of the session rather than just this call.
//! - **reject, and say why** is a rejection *plus* the message the user was
//!   going to type next anyway. Delivering it in the same step is what makes the
//!   model's next attempt informed rather than a retry of the same thing.
//!
//! The fail-closed default is not a constant: it is the first rejecting row,
//! wherever the list puts it.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/approval-options.ts`.

use agentrs_contracts::authority::PermissionMode;

/// What one row of the panel does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalOption {
    /// Stable identity, for tests and for key routing.
    pub id: &'static str,
    /// What the row says.
    pub label: String,
    /// The decision handed to the policy.
    pub allowed: bool,
    /// A permission mode to apply after the decision lands.
    pub then_mode: Option<PermissionMode>,
    /// Collect one line from the user before the run continues.
    pub accepts_feedback: bool,
}

/// The name of a permission mode as the status row and the panel say it.
pub fn mode_label(mode: &PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Plan => "plan",
        PermissionMode::Default => "default",
        PermissionMode::Accepted { .. } => "accepted",
    }
}

/// The next mode in the cycle, which is the one `shift+tab` would reach.
///
/// The cycle is ordered by strictness so the key always walks the same ring.
pub fn next_mode(mode: &PermissionMode) -> PermissionMode {
    match mode {
        PermissionMode::Plan => PermissionMode::Default,
        PermissionMode::Default => PermissionMode::Accepted {
            scopes: vec!["workspace-write".into()],
        },
        PermissionMode::Accepted { .. } => PermissionMode::Plan,
    }
}

/// The rows offered for one approval.
pub fn options(current_mode: &PermissionMode) -> Vec<ApprovalOption> {
    let next = next_mode(current_mode);
    vec![
        ApprovalOption {
            id: "allow-once",
            label: "allow once".into(),
            allowed: true,
            then_mode: None,
            accepts_feedback: false,
        },
        ApprovalOption {
            id: "allow-then-mode",
            label: format!("allow once, then {} for the next run", mode_label(&next)),
            allowed: true,
            then_mode: Some(next),
            accepts_feedback: false,
        },
        ApprovalOption {
            id: "reject",
            label: "reject".into(),
            allowed: false,
            then_mode: None,
            accepts_feedback: false,
        },
        ApprovalOption {
            id: "reject-with-reason",
            label: "reject, and say why".into(),
            allowed: false,
            then_mode: None,
            accepts_feedback: true,
        },
    ]
}

/// The row selected before the user touches anything.
///
/// The first rejecting row, wherever the list puts it: a panel that defaults to
/// allowing is a panel that grants on a stray keypress.
pub fn fail_closed_index(options: &[ApprovalOption]) -> usize {
    options
        .iter()
        .position(|option| !option.allowed)
        .unwrap_or(0)
}

/// The row a digit key selects, counting from one.
pub fn by_position(options: &[ApprovalOption], digit: u32) -> Option<usize> {
    let index = usize::try_from(digit).ok()?.checked_sub(1)?;
    (index < options.len()).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_row_is_the_first_rejecting_one() {
        let options = options(&PermissionMode::Default);
        let index = fail_closed_index(&options);
        assert!(!options[index].allowed);
        assert_eq!(options[index].id, "reject");
    }

    #[test]
    fn the_wider_row_names_the_mode_it_would_move_to() {
        let options = options(&PermissionMode::Plan);
        assert_eq!(options[1].label, "allow once, then default for the next run");
        assert_eq!(options[1].then_mode, Some(PermissionMode::Default));
    }

    #[test]
    fn the_mode_cycle_is_a_ring() {
        let mut mode = PermissionMode::Default;
        let mut seen = Vec::new();
        for _ in 0..3 {
            mode = next_mode(&mode);
            seen.push(mode_label(&mode));
        }
        assert_eq!(seen, ["accepted", "plan", "default"]);
    }

    #[test]
    fn only_one_row_asks_for_a_reason() {
        let options = options(&PermissionMode::Default);
        let asking: Vec<&str> = options
            .iter()
            .filter(|option| option.accepts_feedback)
            .map(|option| option.id)
            .collect();
        assert_eq!(asking, ["reject-with-reason"]);
    }

    #[test]
    fn digits_answer_by_position_and_stop_at_the_end() {
        let options = options(&PermissionMode::Default);
        assert_eq!(by_position(&options, 1), Some(0));
        assert_eq!(by_position(&options, 4), Some(3));
        assert_eq!(by_position(&options, 5), None);
        assert_eq!(by_position(&options, 0), None);
    }
}
