// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/draft-completion.ts, workspace-files.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; candidates are supplied by the caller rather
//            than read here, so the matcher stays a pure function and the one
//            directory read lives in host_io.
//! Completing a draft: `/` for a command, `@` for a workspace path.
//!
//! Two rules from the original are what make it feel like completion rather than
//! a popup that gets in the way:
//!
//! - **`/` only completes at the start of the draft.** A slash in the middle of
//!   a sentence is a slash.
//! - **Accepting a directory keeps the caret inside it.** `@src/` leaves the
//!   completion open on the directory's contents, so a path is typed in one go
//!   rather than one `Tab` per level.

/// What is being completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    /// A command name, after a leading `/`.
    Command,
    /// A workspace path, after an `@`.
    Path,
}

/// An open completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// What is being completed.
    pub kind: CompletionKind,
    /// Character index of the introducer (`/` or `@`) in the draft.
    pub start: usize,
    /// What has been typed after the introducer.
    pub query: String,
    /// Candidates, already filtered and in display order.
    pub items: Vec<String>,
    /// Which candidate is selected.
    pub selected: usize,
}

impl Completion {
    /// The candidate that `Tab` would take.
    pub fn current(&self) -> Option<&str> {
        self.items.get(self.selected).map(String::as_str)
    }

    /// Moves the selection, wrapping so the list is a ring.
    pub fn move_by(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        self.selected = (((self.selected as isize + delta) % len + len) % len) as usize;
    }
}

/// What the caret is sitting in, if anything completable.
///
/// Returns the introducer's character index and the query after it.
pub fn detect(draft: &str, cursor: usize) -> Option<(CompletionKind, usize, String)> {
    let chars: Vec<char> = draft.chars().collect();
    let cursor = cursor.min(chars.len());
    // A command only at the very start: a slash inside a sentence is a slash.
    if chars.first() == Some(&'/') && !chars[..cursor].contains(&' ') && cursor >= 1 {
        return Some((
            CompletionKind::Command,
            0,
            chars[1..cursor].iter().collect(),
        ));
    }
    // A path anywhere, back to the nearest `@` that is not inside a word.
    let mut index = cursor;
    while index > 0 {
        index -= 1;
        let ch = chars[index];
        if ch == '@' {
            let before = index.checked_sub(1).map(|at| chars[at]);
            if before.is_none_or(char::is_whitespace) {
                return Some((
                    CompletionKind::Path,
                    index,
                    chars[index + 1..cursor].iter().collect(),
                ));
            }
            return None;
        }
        if ch.is_whitespace() {
            return None;
        }
    }
    None
}

/// Splits a path query into the directory typed so far and the leaf being typed.
///
/// `src/ap` is "look in `src`, match `ap`"; `src/` is "look in `src`, match
/// everything".
pub fn split_path_query(query: &str) -> (&str, &str) {
    match query.rfind('/') {
        Some(at) => (&query[..=at], &query[at + 1..]),
        None => ("", query),
    }
}

/// Builds a path completion from one directory listing.
///
/// Directories come first and keep their trailing slash, which is both how they
/// are told apart and what keeps the caret inside them when one is taken.
pub fn path_items(query: &str, files: &[String], dirs: &[String]) -> Vec<String> {
    let (prefix, leaf) = split_path_query(query);
    let matching = |name: &String| name.to_lowercase().starts_with(&leaf.to_lowercase());
    dirs.iter()
        .filter(|name| matching(name))
        .map(|name| format!("{prefix}{name}/"))
        .chain(
            files
                .iter()
                .filter(|name| matching(name))
                .map(|name| format!("{prefix}{name}")),
        )
        .collect()
}

/// Replaces the completed span with `item`, returning the new draft and caret.
///
/// The caret lands after the item, and a directory therefore leaves it after the
/// slash — with the completion still open on that directory's contents.
pub fn apply(draft: &str, cursor: usize, start: usize, item: &str) -> (String, usize) {
    let chars: Vec<char> = draft.chars().collect();
    let cursor = cursor.min(chars.len());
    let introducer = chars.get(start).copied().unwrap_or('@');
    let head: String = chars[..start].iter().collect();
    let tail: String = chars[cursor..].iter().collect();
    let inserted = format!("{introducer}{item}");
    let caret = start + inserted.chars().count();
    (format!("{head}{inserted}{tail}"), caret)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sorted, as `host_io::list_dir` returns them: `path_items` preserves the
    /// order it is given rather than sorting again.
    fn dirs() -> Vec<String> {
        vec!["docs".to_string(), "src".to_string()]
    }

    fn files() -> Vec<String> {
        vec!["README.md".to_string(), "run.jsonl".to_string()]
    }

    #[test]
    fn a_slash_completes_only_at_the_start_of_the_draft() {
        assert_eq!(
            detect("/he", 3),
            Some((CompletionKind::Command, 0, "he".into()))
        );
        // A slash inside a sentence is a slash.
        assert_eq!(detect("read a/b", 8), None);
        assert_eq!(detect("/help me", 8), None, "past the first word it is text");
    }

    #[test]
    fn an_at_completes_anywhere_it_starts_a_word() {
        assert_eq!(
            detect("look at @src/a", 14),
            Some((CompletionKind::Path, 8, "src/a".into()))
        );
        assert_eq!(detect("@", 1), Some((CompletionKind::Path, 0, String::new())));
        // An `@` inside a word is an email address, not a path.
        assert_eq!(detect("me@example", 10), None);
    }

    #[test]
    fn nothing_is_completed_when_the_caret_is_in_open_text() {
        assert_eq!(detect("just words", 10), None);
        assert_eq!(detect("", 0), None);
    }

    #[test]
    fn a_path_query_splits_into_a_directory_and_a_leaf() {
        assert_eq!(split_path_query("src/ap"), ("src/", "ap"));
        assert_eq!(split_path_query("src/"), ("src/", ""));
        assert_eq!(split_path_query("ap"), ("", "ap"));
    }

    #[test]
    fn directories_come_first_and_keep_their_slash() {
        let items = path_items("", &files(), &dirs());
        assert_eq!(items[0], "docs/");
        assert_eq!(items[1], "src/");
        assert_eq!(items[2], "README.md");
    }

    #[test]
    fn matching_is_case_insensitive_and_keeps_the_typed_directory() {
        let items = path_items("src/re", &files(), &[]);
        assert_eq!(items, ["src/README.md".to_string()]);
    }

    #[test]
    fn accepting_a_directory_leaves_the_caret_inside_it() {
        let (draft, caret) = apply("look at @sr", 11, 8, "src/");
        assert_eq!(draft, "look at @src/");
        assert_eq!(caret, 13);
        // And the completion reopens on that directory's contents.
        assert_eq!(
            detect(&draft, caret),
            Some((CompletionKind::Path, 8, "src/".into()))
        );
    }

    #[test]
    fn accepting_keeps_whatever_followed_the_caret() {
        let (draft, caret) = apply("see @sr and stop", 7, 4, "src/");
        assert_eq!(draft, "see @src/ and stop");
        assert_eq!(caret, 9);
    }

    #[test]
    fn a_command_is_applied_with_its_own_introducer() {
        let (draft, caret) = apply("/he", 3, 0, "help");
        assert_eq!(draft, "/help");
        assert_eq!(caret, 5);
    }

    #[test]
    fn the_selection_is_a_ring() {
        let mut completion = Completion {
            kind: CompletionKind::Path,
            start: 0,
            query: String::new(),
            items: vec!["a".into(), "b".into(), "c".into()],
            selected: 0,
        };
        completion.move_by(-1);
        assert_eq!(completion.current(), Some("c"));
        completion.move_by(1);
        assert_eq!(completion.current(), Some("a"));
    }

    #[test]
    fn an_empty_candidate_list_is_not_a_panic() {
        let mut completion = Completion {
            kind: CompletionKind::Command,
            start: 0,
            query: "zzz".into(),
            items: Vec::new(),
            selected: 0,
        };
        completion.move_by(1);
        assert_eq!(completion.current(), None);
    }

    #[test]
    fn completion_positions_are_characters_not_bytes() {
        let draft = "读 @sr";
        let cursor = draft.chars().count();
        assert_eq!(detect(draft, cursor), Some((CompletionKind::Path, 2, "sr".into())));
        let (applied, caret) = apply(draft, cursor, 2, "src/");
        assert_eq!(applied, "读 @src/");
        assert_eq!(caret, 7);
    }
}
