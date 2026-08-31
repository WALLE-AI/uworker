// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/notifications.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; the per-notice fold closure is
//            replaced by a built-in repeat count so the queue stays a plain
//            comparable value.
//! The notice queue.
//!
//! The frame has exactly one row for a notice. Held as a single string, whatever
//! was said last wins and nothing ever expires — which is how a startup warning
//! gets overwritten one statement later and a mode toggle stays on screen for
//! the rest of the session.
//!
//! So the row is fed by a queue instead. The rules are the smallest set that
//! fixes both:
//!
//! - **Priority, not recency.** A higher-priority notice is shown first; equal
//!   priorities keep their arrival order, so a burst still reads as a sequence.
//! - **Everything expires.** A notice holds the row for its own timeout and then
//!   yields it, so an idle frame settles back to nothing.
//! - **A key is an identity.** Pushing the same key again replaces the one
//!   already there and counts it, so a repeated event counts up rather than
//!   queueing up.
//! - **A notice may retract others.** [`Notice::invalidates`] drops the keys an
//!   event has made untrue, wherever they are.
//!
//! Time is a parameter, never read. Nothing here schedules anything: the caller
//! advances the queue with [`NoticeQueue::tick`], which is what lets an idle
//! session hold no timer at all.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/notifications.ts`. The
//! original lets a notice carry its own `fold` closure; here the one behaviour
//! that needed is built in as a repeat count, which keeps the queue a plain
//! value that can be compared in a test.

use crate::theme::RowTone;

/// How long a notice holds the row when it does not say.
pub const DEFAULT_NOTICE_MS: u64 = 8_000;

/// How badly the notice wants the row.
///
/// `Immediate` is not "very high" — it means *this answers the action in
/// progress*. It takes the row now, because a reply that waits behind an ambient
/// warning is not a reply. What it displaces is not lost: it goes back in line
/// at its own priority and returns once the answer has been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Ambient, and the first to wait.
    Low,
    /// The default for an ad-hoc message.
    Medium,
    /// A warning the reader should not miss.
    High,
    /// The answer to what the user just did.
    Immediate,
}

/// One thing to say on the notice row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Identity. Pushing the same key again replaces, never duplicates.
    pub key: String,
    /// What to say.
    pub text: String,
    /// How the row is painted.
    pub tone: RowTone,
    /// How badly it wants the row.
    pub priority: Priority,
    /// How long it holds the row.
    pub timeout_ms: u64,
    /// Keys this notice has made untrue, dropped wherever they are.
    pub invalidates: Vec<String>,
    /// How many times this key has arrived, counting the first.
    pub repeats: usize,
}

impl Notice {
    /// A medium-priority notice with a key derived from its text, so the same
    /// message twice does not queue twice.
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            key: format!("text:{text}"),
            text,
            tone: RowTone::System,
            priority: Priority::Medium,
            timeout_ms: DEFAULT_NOTICE_MS,
            invalidates: Vec::new(),
            repeats: 1,
        }
    }

    /// A keyed notice, so repeats of the same event replace rather than queue.
    pub fn keyed(key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ..Self::text(text)
        }
    }

    /// Sets the tone.
    pub fn with_tone(mut self, tone: RowTone) -> Self {
        self.tone = tone;
        self
    }

    /// Sets the priority.
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Marks the notice as the answer to what the user just did.
    pub fn immediate(self) -> Self {
        self.with_priority(Priority::Immediate)
    }

    /// Marks it as an error and raises its priority to match.
    pub fn error(self) -> Self {
        self.with_tone(RowTone::Error).with_priority(Priority::High)
    }

    /// Declares keys this notice has made untrue.
    pub fn invalidating<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.invalidates = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Sets how long the notice holds the row.
    pub const fn for_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }

    /// The row as shown, with the repeat count when there is one.
    pub fn display(&self) -> String {
        if self.repeats > 1 {
            format!("{} (×{})", self.text, self.repeats)
        } else {
            self.text.clone()
        }
    }
}

/// The row and everything waiting for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoticeQueue {
    current: Option<Notice>,
    expires_at_ms: Option<u64>,
    queue: Vec<Notice>,
}

impl NoticeQueue {
    /// The notice holding the row, if any.
    pub fn current(&self) -> Option<&Notice> {
        self.current.as_ref()
    }

    /// How many notices are waiting behind the row.
    pub fn waiting(&self) -> usize {
        self.queue.len()
    }

    /// The row, and the `+2` that says how many are still waiting.
    ///
    /// Nothing is silently overwritten, and nothing stays for ever, so the row
    /// has to be able to say that there is more.
    pub fn row(&self) -> Option<(String, RowTone)> {
        let current = self.current.as_ref()?;
        let text = if self.queue.is_empty() {
            current.display()
        } else {
            format!("{}  +{}", current.display(), self.queue.len())
        };
        Some((text, current.tone))
    }

    /// Inserts by priority, keeping arrival order within one priority.
    fn insert(&mut self, notice: Notice) {
        let at = self
            .queue
            .iter()
            .position(|held| held.priority < notice.priority)
            .unwrap_or(self.queue.len());
        self.queue.insert(at, notice);
    }

    /// Promotes the head of the queue when the row is free.
    fn promote(&mut self, now_ms: u64) {
        if self.current.is_some() || self.queue.is_empty() {
            return;
        }
        let next = self.queue.remove(0);
        self.expires_at_ms = Some(now_ms + next.timeout_ms);
        self.current = Some(next);
    }

    /// Adds a notice.
    ///
    /// The row is claimed straight away when it is free, or when the arrival is
    /// [`Priority::Immediate`] — in which case whatever was showing goes back to
    /// the queue at its own priority rather than being lost.
    pub fn push(&mut self, notice: Notice, now_ms: u64) {
        let retracted = notice.invalidates.clone();
        if self
            .current
            .as_ref()
            .is_some_and(|held| retracted.contains(&held.key))
        {
            self.current = None;
            self.expires_at_ms = None;
        }
        self.queue.retain(|held| !retracted.contains(&held.key));

        // Same key: replace whichever copy is holding it, and stay where it is.
        if let Some(held) = &self.current {
            if held.key == notice.key {
                let repeats = held.repeats + 1;
                let mut next = notice;
                next.repeats = repeats;
                // A repeat refreshes the timeout: it has just said something new.
                self.expires_at_ms = Some(now_ms + next.timeout_ms);
                self.current = Some(next);
                return;
            }
        }
        // Same key, but waiting rather than showing: absorb its count and carry
        // on. It must **not** return here — an immediate arrival still has to
        // claim the row, and returning early is how `/permission nope` followed
        // by `/permission plan` left the second answer stuck behind an ambient
        // notice that had another seven seconds to run.
        let mut notice = notice;
        notice.repeats = notice.repeats.max(1);
        if let Some(at) = self.queue.iter().position(|held| held.key == notice.key) {
            notice.repeats = self.queue.remove(at).repeats + 1;
        }
        // An immediate arrival is *the answer to what just happened*, so it takes
        // the row directly rather than being inserted and promoted. Going
        // through the queue looks equivalent and is not: displaced immediates
        // pile up at its head, and `insert` places an equal priority after them
        // — so the newest answer would queue behind a growing stack of stale
        // ones, and a run of commands would show its answers in reverse.
        if notice.priority == Priority::Immediate {
            let displaced = self.current.take();
            self.expires_at_ms = Some(now_ms + notice.timeout_ms);
            self.current = Some(notice);
            // What it displaced is not lost; it goes back in line at its own
            // priority and returns once the answer has been read.
            if let Some(displaced) = displaced {
                self.insert(displaced);
            }
            return;
        }
        self.insert(notice);
        self.promote(now_ms);
    }

    /// Retires the showing notice once its time is up, and promotes the next.
    pub fn tick(&mut self, now_ms: u64) {
        match (&self.current, self.expires_at_ms) {
            (Some(_), Some(expires)) if now_ms < expires => {}
            (Some(_), _) => {
                self.current = None;
                self.expires_at_ms = None;
                self.promote(now_ms);
            }
            (None, _) => self.promote(now_ms),
        }
    }

    /// Drops one notice by key, wherever it is.
    pub fn drop_key(&mut self, key: &str, now_ms: u64) {
        self.queue.retain(|held| held.key != key);
        if self.current.as_ref().is_some_and(|held| held.key == key) {
            self.current = None;
            self.expires_at_ms = None;
            self.promote(now_ms);
        }
    }

    /// Clears the row and everything waiting for it.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> NoticeQueue {
        NoticeQueue::default()
    }

    #[test]
    fn the_first_notice_takes_the_free_row() {
        let mut notices = queue();
        notices.push(Notice::text("hello"), 0);
        assert_eq!(notices.current().map(Notice::display), Some("hello".into()));
    }

    #[test]
    fn priority_beats_arrival_order() {
        let mut notices = queue();
        notices.push(Notice::keyed("a", "ambient").with_priority(Priority::Low), 0);
        notices.push(Notice::keyed("b", "warning").with_priority(Priority::High), 0);
        notices.push(Notice::keyed("c", "also ambient").with_priority(Priority::Low), 0);
        // `a` took the free row; `b` outranks `c` behind it.
        assert_eq!(notices.current().unwrap().key, "a");
        notices.tick(DEFAULT_NOTICE_MS + 1);
        assert_eq!(notices.current().unwrap().key, "b");
        notices.tick(DEFAULT_NOTICE_MS * 2 + 2);
        assert_eq!(notices.current().unwrap().key, "c");
    }

    #[test]
    fn an_immediate_answer_takes_the_row_and_gives_it_back() {
        let mut notices = queue();
        notices.push(Notice::keyed("warn", "capability warning").with_priority(Priority::High), 0);
        notices.push(Notice::keyed("answer", "mouse on").immediate(), 100);
        assert_eq!(notices.current().unwrap().key, "answer");
        // What it displaced is not lost.
        notices.tick(100 + DEFAULT_NOTICE_MS + 1);
        assert_eq!(notices.current().unwrap().key, "warn");
    }

    #[test]
    fn a_second_answer_does_not_let_the_first_take_the_row_back() {
        // Pressing `y` then `c` must show the result of `c`. The displaced
        // answer is older by definition, however loudly it asked for the row.
        let mut notices = queue();
        notices.push(Notice::keyed("approval", "allow once").immediate(), 0);
        notices.push(Notice::keyed("changeset", "committed 1 entries").immediate(), 10);
        assert_eq!(notices.current().unwrap().key, "changeset");
        notices.tick(10 + DEFAULT_NOTICE_MS + 1);
        assert_eq!(notices.current().unwrap().key, "approval");
    }

    #[test]
    fn the_newest_answer_beats_every_older_one() {
        // Each answer displaces the last, and the displaced ones pile up in the
        // queue. Inserting the newest by priority would put it *behind* that
        // pile, so a run of commands would show their answers in reverse.
        let mut notices = queue();
        for (index, key) in ["a", "b", "c", "d"].iter().enumerate() {
            notices.push(Notice::keyed(*key, format!("answer {key}")).immediate(), index as u64);
            assert_eq!(
                notices.current().unwrap().key,
                *key,
                "answer {key} must be the one on the row"
            );
        }
    }

    #[test]
    fn everything_expires_and_the_row_settles_to_nothing() {
        let mut notices = queue();
        notices.push(Notice::text("temporary").for_ms(1_000), 0);
        notices.tick(500);
        assert!(notices.current().is_some());
        notices.tick(1_001);
        assert!(notices.current().is_none());
        assert!(notices.row().is_none());
    }

    #[test]
    fn a_repeated_key_counts_up_rather_than_queueing_up() {
        let mut notices = queue();
        for _ in 0..3 {
            notices.push(Notice::keyed("drop", "live events dropped"), 0);
        }
        assert_eq!(notices.waiting(), 0);
        assert_eq!(notices.current().unwrap().display(), "live events dropped (×3)");
    }

    #[test]
    fn an_immediate_answer_claims_the_row_even_when_its_key_is_already_waiting() {
        // `/permission nope` queues an error; `/permission plan` is the answer to
        // what the user just did and must be seen now, not after the ambient
        // notice ahead of it has run its eight seconds.
        let mut notices = queue();
        notices.push(Notice::keyed("ambient", "capability warning"), 0);
        notices.push(Notice::keyed("permission", "unknown preset").error(), 0);
        assert_eq!(notices.current().unwrap().key, "ambient");
        notices.push(Notice::keyed("permission", "permission plan").immediate(), 10);
        assert_eq!(notices.current().unwrap().display(), "permission plan (×2)");
        // What it displaced is still waiting.
        notices.tick(10 + DEFAULT_NOTICE_MS + 1);
        assert_eq!(notices.current().unwrap().key, "ambient");
    }

    #[test]
    fn a_repeat_refreshes_the_timeout() {
        let mut notices = queue();
        notices.push(Notice::keyed("k", "first").for_ms(1_000), 0);
        notices.push(Notice::keyed("k", "second").for_ms(1_000), 900);
        notices.tick(1_500);
        assert!(notices.current().is_some(), "the repeat bought it another second");
        notices.tick(1_901);
        assert!(notices.current().is_none());
    }

    #[test]
    fn a_notice_can_retract_what_it_has_made_untrue() {
        let mut notices = queue();
        notices.push(Notice::keyed("provider", "provider config incomplete"), 0);
        notices.push(
            Notice::keyed("started", "run started").invalidating(["provider"]),
            10,
        );
        assert_eq!(notices.current().unwrap().key, "started");
        assert_eq!(notices.waiting(), 0);
    }

    #[test]
    fn the_row_says_how_many_are_still_waiting() {
        let mut notices = queue();
        notices.push(Notice::keyed("a", "one"), 0);
        notices.push(Notice::keyed("b", "two"), 0);
        notices.push(Notice::keyed("c", "three"), 0);
        assert_eq!(notices.row().unwrap().0, "one  +2");
    }

    #[test]
    fn dropping_a_key_reaches_it_wherever_it_is() {
        let mut notices = queue();
        notices.push(Notice::keyed("a", "one"), 0);
        notices.push(Notice::keyed("b", "two"), 0);
        notices.drop_key("b", 0);
        assert_eq!(notices.waiting(), 0);
        notices.drop_key("a", 0);
        assert!(notices.current().is_none());
    }
}
