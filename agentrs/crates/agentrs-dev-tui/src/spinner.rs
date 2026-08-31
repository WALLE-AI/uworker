// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/spinner.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust, unchanged in behaviour.
//! Frame selection and duration formatting for running work.
//!
//! Both are pure functions of elapsed time, so a frame is reproducible from a
//! timestamp and the view needs no animation state of its own — the clock only
//! has to make the frame redraw.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/spinner.ts`.

/// Frame period. Slow enough to stay cheap, fast enough to read as motion.
pub const SPINNER_INTERVAL_MS: u64 = 80;

const UNICODE_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// A terminal without braille support still gets a turning bar.
const ASCII_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

/// The frame showing at `elapsed_ms` into a run.
pub fn spinner_frame(elapsed_ms: u64, unicode: bool) -> &'static str {
    let step = (elapsed_ms / SPINNER_INTERVAL_MS) as usize;
    if unicode {
        UNICODE_FRAMES[step % UNICODE_FRAMES.len()]
    } else {
        ASCII_FRAMES[step % ASCII_FRAMES.len()]
    }
}

/// A compact duration: `4s`, `1m12s`, `2h05m`.
///
/// Seconds are dropped past an hour because at that scale they are noise, and
/// the field must not grow — it sits on a row whose other fields are dropped by
/// width, and a field that widens as it runs would evict them.
pub fn format_elapsed(elapsed_ms: u64) -> String {
    let total = elapsed_ms / 1_000;
    if total < 60 {
        return format!("{total}s");
    }
    let minutes = total / 60;
    if minutes < 60 {
        return format!("{minutes}m{:02}s", total % 60);
    }
    format!("{}h{:02}m", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::display_width;

    #[test]
    fn frames_advance_once_per_interval_and_wrap() {
        assert_eq!(spinner_frame(0, true), "⠋");
        assert_eq!(spinner_frame(SPINNER_INTERVAL_MS - 1, true), "⠋");
        assert_eq!(spinner_frame(SPINNER_INTERVAL_MS, true), "⠙");
        assert_eq!(spinner_frame(SPINNER_INTERVAL_MS * 10, true), "⠋");
    }

    #[test]
    fn every_frame_is_one_cell_wide() {
        // The frame sits at the head of a width-budgeted row; a two-cell frame
        // would silently cost the row its last field.
        for step in 0..12 {
            for unicode in [true, false] {
                let frame = spinner_frame(step * SPINNER_INTERVAL_MS, unicode);
                assert_eq!(display_width(frame), 1, "{frame:?}");
            }
        }
    }

    #[test]
    fn elapsed_is_compact_and_stays_bounded() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(4_400), "4s");
        assert_eq!(format_elapsed(59_999), "59s");
        assert_eq!(format_elapsed(60_000), "1m00s");
        assert_eq!(format_elapsed(72_000), "1m12s");
        assert_eq!(format_elapsed(3_599_000), "59m59s");
        assert_eq!(format_elapsed(7_500_000), "2h05m");
        for ms in [0, 1_000, 61_000, 3_600_000, 86_400_000] {
            assert!(display_width(&format_elapsed(ms)) <= 6, "{ms}");
        }
    }
}
