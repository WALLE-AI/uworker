//! Terminal-safe rendering helpers.

/// Removes terminal control bytes while preserving ordinary line layout.
///
/// ESC, C0 controls other than tab/newline, and DEL are removed. This prevents
/// model or tool output from injecting ANSI/OSC sequences into the host terminal.
pub fn terminal_text(input: &str) -> String {
    input
        .chars()
        .filter(|ch| matches!(ch, '\n' | '\t') || (!ch.is_control() && *ch != '\u{1b}'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_osc_controls() {
        let hostile = "ok\u{1b}[31mRED\u{1b}[0m\u{7}\nnext\u{1b}]0;owned\u{7}";
        let safe = terminal_text(hostile);
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\u{7}'));
        assert_eq!(safe, "ok[31mRED[0m\nnext]0;owned");
    }

    #[test]
    fn keeps_unicode_tabs_and_newlines() {
        assert_eq!(terminal_text("你好\tAgentRS\n"), "你好\tAgentRS\n");
    }
}
