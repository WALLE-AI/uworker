// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/keymap.ts, keybindings.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; chords are built from crossterm key
//            events; the action set is the subset this host can honour.
//! Actions, chords, and the table that maps one to the other.
//!
//! Every shortcut is one row in a table that the resolver and the shortcut sheet
//! both read, so rebinding a key changes what it does and what `?` says it does
//! in the same move. Nothing else in the crate is allowed to match on a raw key.
//!
//! Two chords are **reserved** and cannot be rebound or unbound:
//! [`Action::Cancel`] and [`Action::Escape`]. A terminal you cannot get out of
//! is not a terminal.
//!
//! Ported from `dsh-code-agent`'s `packages/dsh-tui/src/keymap.ts`.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the user is looking at, which decides what a key means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    /// Typing into the composer.
    Composer,
    /// Answering an approval.
    Approval,
    /// A full-screen list: the palette, the log browser, the shortcut sheet.
    Overlay,
    /// The searchable transcript screen.
    Transcript,
}

/// One thing a key can ask for.
///
/// The names are the `surface:verb` ids the shortcut sheet and the rebinding
/// file use, so the three never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    /// Two-step cancellation, then a bounded shutdown. Reserved.
    Cancel,
    /// Clear the draft, then arm and perform cancellation. Reserved.
    Escape,
    /// Send the draft.
    Submit,
    /// Insert a newline instead of sending.
    Newline,
    /// Move the caret one character left.
    CaretLeft,
    /// Move the caret one character right.
    CaretRight,
    /// Move the caret one word left.
    CaretWordLeft,
    /// Move the caret one word right.
    CaretWordRight,
    /// Jump to the start of the current line.
    CaretLineStart,
    /// Jump to the end of the current line.
    CaretLineEnd,
    /// Delete the character before the caret.
    DeleteBack,
    /// Delete the character after the caret.
    DeleteForward,
    /// Delete the word before the caret.
    DeleteWord,
    /// Delete to the start of the line.
    DeleteToLineStart,
    /// Delete to the end of the line.
    DeleteToLineEnd,
    /// Older draft, or the line above in a multi-line draft.
    HistoryPrevious,
    /// Newer draft, or the line below in a multi-line draft.
    HistoryNext,
    /// Scroll the transcript up one row.
    ScrollUp,
    /// Scroll the transcript down one row.
    ScrollDown,
    /// Scroll the transcript up one page.
    ScrollPageUp,
    /// Scroll the transcript down one page.
    ScrollPageDown,
    /// Fold or unfold the card in view.
    FoldToggle,
    /// Show the shortcut sheet.
    HelpOpen,
    /// Open the command palette.
    PaletteOpen,
    /// Open the durable-log browser.
    BrowseLogs,
    /// Open the searchable transcript.
    TranscriptOpen,
    /// Cycle the permission mode for the next run.
    PermissionCycle,
    /// Open the entry's first file location in `$EDITOR`.
    EditorOpen,
    /// Accept the open completion, or move focus.
    CompletionAccept,
    /// Move to the previous row of a list.
    ListPrevious,
    /// Move to the next row of a list.
    ListNext,
    /// Take the selected row.
    ListAccept,
    /// Close the surface.
    Close,
    /// Allow the approval once.
    ApprovalAllow,
    /// Reject the approval.
    ApprovalReject,
    /// Commit the staged ChangeSet.
    CommitChanges,
    /// Discard the staged ChangeSet.
    DiscardChanges,
    /// Leave.
    Quit,
    /// Start a search.
    SearchStart,
    /// Jump to the next match.
    SearchNext,
    /// Jump to the previous match.
    SearchPrevious,
    /// Put the selected user message back in the draft.
    RestoreDraft,
    /// Drop the conversation and start a fresh one.
    ClearSession,
    /// Send the last message again.
    Retry,
    /// Review what is staged, before committing it.
    ReviewChanges,
    /// Show what this session is: run, log, model, counters.
    ShowStatus,
    /// Write the transcript out as markdown.
    ExportTranscript,
    /// Hand the wheel back to the terminal, or take it again.
    ToggleMouse,
}

impl Action {
    /// The `surface:verb` id, as the shortcut sheet and the rebinding file use it.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Cancel => "app:cancel",
            Self::Escape => "app:escape",
            Self::Submit => "chat:submit",
            Self::Newline => "chat:newline",
            Self::CaretLeft => "caret:left",
            Self::CaretRight => "caret:right",
            Self::CaretWordLeft => "caret:word-left",
            Self::CaretWordRight => "caret:word-right",
            Self::CaretLineStart => "caret:line-start",
            Self::CaretLineEnd => "caret:line-end",
            Self::DeleteBack => "edit:delete-back",
            Self::DeleteForward => "edit:delete-forward",
            Self::DeleteWord => "edit:delete-word",
            Self::DeleteToLineStart => "edit:delete-to-line-start",
            Self::DeleteToLineEnd => "edit:delete-to-line-end",
            Self::HistoryPrevious => "history:previous",
            Self::HistoryNext => "history:next",
            Self::ScrollUp => "scroll:up",
            Self::ScrollDown => "scroll:down",
            Self::ScrollPageUp => "scroll:page-up",
            Self::ScrollPageDown => "scroll:page-down",
            Self::FoldToggle => "fold:toggle",
            Self::HelpOpen => "help:open",
            Self::PaletteOpen => "palette:open",
            Self::BrowseLogs => "session:browse",
            Self::TranscriptOpen => "transcript:open",
            Self::PermissionCycle => "permission:cycle",
            Self::EditorOpen => "editor:open",
            Self::CompletionAccept => "completion:accept",
            Self::ListPrevious => "list:previous",
            Self::ListNext => "list:next",
            Self::ListAccept => "list:accept",
            Self::Close => "surface:close",
            Self::ApprovalAllow => "approval:allow",
            Self::ApprovalReject => "approval:reject",
            Self::CommitChanges => "changeset:commit",
            Self::DiscardChanges => "changeset:discard",
            Self::Quit => "app:quit",
            Self::SearchStart => "transcript:search",
            Self::SearchNext => "transcript:next-match",
            Self::SearchPrevious => "transcript:previous-match",
            Self::RestoreDraft => "transcript:restore-draft",
            Self::ClearSession => "session:clear",
            Self::Retry => "chat:retry",
            Self::ReviewChanges => "changeset:review",
            Self::ShowStatus => "session:status",
            Self::ExportTranscript => "transcript:export",
            Self::ToggleMouse => "mouse:toggle",
        }
    }

    /// True for the two chords that may never be taken away.
    pub const fn reserved(self) -> bool {
        matches!(self, Self::Cancel | Self::Escape)
    }

    /// What the shortcut sheet says this action does.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Cancel => "cancel the run; again to force quit",
            Self::Escape => "clear the draft, then interrupt",
            Self::Submit => "send the draft",
            Self::Newline => "insert a newline",
            Self::CaretLeft => "caret one character left",
            Self::CaretRight => "caret one character right",
            Self::CaretWordLeft => "caret one word left",
            Self::CaretWordRight => "caret one word right",
            Self::CaretLineStart => "start of line",
            Self::CaretLineEnd => "end of line",
            Self::DeleteBack => "delete the character before the caret",
            Self::DeleteForward => "delete the character after the caret",
            Self::DeleteWord => "delete the word before the caret",
            Self::DeleteToLineStart => "delete to the start of the line",
            Self::DeleteToLineEnd => "delete to the end of the line",
            Self::HistoryPrevious => "previous line, then older drafts",
            Self::HistoryNext => "next line, then newer drafts",
            Self::ScrollUp => "scroll up",
            Self::ScrollDown => "scroll down",
            Self::ScrollPageUp => "scroll up one page",
            Self::ScrollPageDown => "scroll down one page",
            Self::FoldToggle => "fold or unfold the card in view",
            Self::HelpOpen => "show this sheet",
            Self::PaletteOpen => "command palette",
            Self::BrowseLogs => "browse durable logs",
            Self::TranscriptOpen => "searchable transcript",
            Self::PermissionCycle => "cycle the permission mode",
            Self::EditorOpen => "open the card's file in $EDITOR",
            Self::CompletionAccept => "accept the completion",
            Self::ListPrevious => "previous row",
            Self::ListNext => "next row",
            Self::ListAccept => "take the selected row",
            Self::Close => "close",
            Self::ApprovalAllow => "allow once",
            Self::ApprovalReject => "reject",
            Self::CommitChanges => "commit the staged ChangeSet",
            Self::DiscardChanges => "discard the staged ChangeSet",
            Self::Quit => "quit",
            Self::SearchStart => "search",
            Self::SearchNext => "next match",
            Self::SearchPrevious => "previous match",
            Self::RestoreDraft => "restore the selected message to the draft",
            Self::ClearSession => "drop the conversation and start a fresh one",
            Self::Retry => "send the last message again",
            Self::ReviewChanges => "review the staged ChangeSet",
            Self::ShowStatus => "what this session is",
            Self::ExportTranscript => "write the transcript out as markdown",
            Self::ToggleMouse => "give the wheel back to the terminal, or take it",
        }
    }
}

/// One key press, normalized so it can be written down and compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    /// The key itself, lowercased for characters.
    pub key: Key,
    /// Whether control was held.
    pub ctrl: bool,
    /// Whether alt was held.
    pub alt: bool,
    /// Whether shift was held. Only meaningful for non-character keys.
    pub shift: bool,
}

/// The key part of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A printable character, lowercased.
    Char(char),
    /// Enter or return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Escape.
    Escape,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.alt {
            f.write_str("alt+")?;
        }
        if self.shift {
            f.write_str("shift+")?;
        }
        match self.key {
            Key::Char(' ') => f.write_str("space"),
            Key::Char(ch) => write!(f, "{ch}"),
            Key::Enter => f.write_str("enter"),
            Key::Tab => f.write_str("tab"),
            Key::Backspace => f.write_str("backspace"),
            Key::Delete => f.write_str("delete"),
            Key::Escape => f.write_str("escape"),
            Key::Left => f.write_str("left"),
            Key::Right => f.write_str("right"),
            Key::Up => f.write_str("up"),
            Key::Down => f.write_str("down"),
            Key::PageUp => f.write_str("pageup"),
            Key::PageDown => f.write_str("pagedown"),
            Key::Home => f.write_str("home"),
            Key::End => f.write_str("end"),
        }
    }
}

impl Chord {
    /// A plain key with no modifiers.
    pub const fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// A key with control held.
    pub const fn ctrl(key: Key) -> Self {
        Self {
            key,
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    /// A key with alt held.
    pub const fn alt(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: true,
            shift: false,
        }
    }

    /// A key with shift held.
    pub const fn shift(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: true,
        }
    }

    /// Normalizes one terminal key event into a chord.
    ///
    /// Shift is dropped for characters: the terminal has already applied it, and
    /// keeping it would make `A` and `shift+a` two different chords.
    ///
    /// **The character is lowercased**, so a chord is a lookup key and nothing
    /// more. Whatever inserts text must take the character from the key event
    /// instead — inserting `chord.key` makes capital letters impossible to type.
    pub fn from_event(event: &KeyEvent) -> Option<Self> {
        let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
        let alt = event.modifiers.contains(KeyModifiers::ALT);
        let shift = event.modifiers.contains(KeyModifiers::SHIFT);
        let key = match event.code {
            KeyCode::Char(ch) => Key::Char(ch.to_ascii_lowercase()),
            KeyCode::Enter => Key::Enter,
            KeyCode::Tab => Key::Tab,
            KeyCode::BackTab => Key::Tab,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            KeyCode::Esc => Key::Escape,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            _ => return None,
        };
        let shift = match event.code {
            KeyCode::Char(_) => false,
            // A terminal reports shift+tab as its own code and usually forgets
            // to set the modifier, so the code is the signal.
            KeyCode::BackTab => true,
            _ => shift,
        };
        Some(Self {
            key,
            ctrl,
            alt,
            shift,
        })
    }
}

/// One row of the table: a context, a chord, and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Where the chord applies.
    pub context: Context,
    /// The chord.
    pub chord: Chord,
    /// What it asks for.
    pub action: Action,
}

const fn binding(context: Context, chord: Chord, action: Action) -> Binding {
    Binding {
        context,
        chord,
        action,
    }
}

/// The default table.
///
/// Order matters only for the shortcut sheet; the resolver takes the first row
/// whose context and chord match, and a rebinding is applied by replacing rows
/// rather than by appending to them.
pub fn default_bindings() -> Vec<Binding> {
    use Action as A;
    use Context::{Approval, Composer, Overlay, Transcript};
    use Key::{
        Backspace, Char, Delete, Down, End, Enter, Escape, Home, Left, PageDown, PageUp, Right,
        Tab, Up,
    };
    let mut rows = Vec::new();
    for context in [Composer, Approval, Overlay, Transcript] {
        rows.push(binding(context, Chord::ctrl(Char('c')), A::Cancel));
        rows.push(binding(context, Chord::plain(Escape), A::Escape));
    }
    rows.extend([
        // Composing.
        binding(Composer, Chord::plain(Enter), A::Submit),
        binding(Composer, Chord::alt(Enter), A::Newline),
        binding(Composer, Chord::ctrl(Char('j')), A::Newline),
        binding(Composer, Chord::plain(Left), A::CaretLeft),
        binding(Composer, Chord::plain(Right), A::CaretRight),
        binding(Composer, Chord::ctrl(Left), A::CaretWordLeft),
        binding(Composer, Chord::ctrl(Right), A::CaretWordRight),
        binding(Composer, Chord::alt(Char('b')), A::CaretWordLeft),
        binding(Composer, Chord::alt(Char('f')), A::CaretWordRight),
        binding(Composer, Chord::ctrl(Char('a')), A::CaretLineStart),
        binding(Composer, Chord::ctrl(Char('e')), A::CaretLineEnd),
        binding(Composer, Chord::plain(Home), A::CaretLineStart),
        binding(Composer, Chord::plain(End), A::CaretLineEnd),
        binding(Composer, Chord::plain(Backspace), A::DeleteBack),
        // A terminal with `stty erase ^H` sends 0x08, and crossterm decodes the
        // C0 range as letters: 0x08 arrives as **ctrl+h**, not as a backspace.
        // Unbound, such a terminal has no working delete key at all — which is
        // also readline's `backward-delete-char`, so the binding is what a shell
        // user would expect anyway. Some terminals do report a real
        // ctrl+backspace through CSI-u; that is bound to the same thing.
        //
        // Both map to a plain delete rather than to delete-word: on a modern
        // terminal that costs nothing (alt+backspace and ctrl+w both delete a
        // word), while a legacy terminal eating a whole word per keystroke is
        // unusable.
        binding(Composer, Chord::ctrl(Char('h')), A::DeleteBack),
        binding(Composer, Chord::ctrl(Backspace), A::DeleteBack),
        binding(Composer, Chord::plain(Delete), A::DeleteForward),
        binding(Composer, Chord::ctrl(Char('w')), A::DeleteWord),
        binding(Composer, Chord::alt(Backspace), A::DeleteWord),
        binding(Composer, Chord::ctrl(Char('u')), A::DeleteToLineStart),
        binding(Composer, Chord::ctrl(Char('k')), A::DeleteToLineEnd),
        binding(Composer, Chord::plain(Up), A::HistoryPrevious),
        binding(Composer, Chord::plain(Down), A::HistoryNext),
        binding(Composer, Chord::plain(Tab), A::CompletionAccept),
        // Reading.
        binding(Composer, Chord::plain(PageUp), A::ScrollPageUp),
        binding(Composer, Chord::plain(PageDown), A::ScrollPageDown),
        binding(Composer, Chord::alt(Up), A::ScrollUp),
        binding(Composer, Chord::alt(Down), A::ScrollDown),
        binding(Composer, Chord::ctrl(Char('o')), A::FoldToggle),
        binding(Composer, Chord::ctrl(Char('t')), A::TranscriptOpen),
        binding(Composer, Chord::ctrl(Char('p')), A::PaletteOpen),
        binding(Composer, Chord::ctrl(Char('r')), A::BrowseLogs),
        binding(Composer, Chord::ctrl(Char('x')), A::EditorOpen),
        binding(Composer, Chord::shift(Tab), A::PermissionCycle),
        binding(Composer, Chord::plain(Char('?')), A::HelpOpen),
        // The ChangeSet.
        //
        // These were bare `c` / `d` / `q` once, which quietly made three letters
        // of the alphabet unusable as the first character of a message: typing
        // `commit this` committed and then left `ommit this` in the draft, and
        // `quick` quit the program outright. A composer that is always live
        // cannot afford bare letters — the approval panel can, because there is
        // no draft to type into while it is open.
        //
        // `ctrl+s` is muscle memory for "save", and raw mode has already turned
        // off the flow control that would otherwise swallow it. Discard and quit
        // are `/discard` and `/quit`, plus ctrl+c twice.
        binding(Composer, Chord::ctrl(Char('s')), A::CommitChanges),
        // Reviewing before committing is the one of these worth a key: it is
        // what you do *just before* ctrl+s, every time.
        binding(Composer, Chord::ctrl(Char('g')), A::ReviewChanges),
        // Answering an approval.
        binding(Approval, Chord::plain(Char('y')), A::ApprovalAllow),
        binding(Approval, Chord::plain(Char('n')), A::ApprovalReject),
        binding(Approval, Chord::plain(Up), A::ListPrevious),
        binding(Approval, Chord::plain(Down), A::ListNext),
        binding(Approval, Chord::plain(Enter), A::ListAccept),
        // Lists.
        binding(Overlay, Chord::plain(Up), A::ListPrevious),
        binding(Overlay, Chord::plain(Down), A::ListNext),
        binding(Overlay, Chord::plain(PageUp), A::ScrollPageUp),
        binding(Overlay, Chord::plain(PageDown), A::ScrollPageDown),
        binding(Overlay, Chord::plain(Enter), A::ListAccept),
        binding(Overlay, Chord::plain(Char('q')), A::Close),
        // The transcript screen.
        // Enter commits the search and hands `n`, `N` and `q` back. Without it
        // the box swallows every key it was opened to make reachable.
        binding(Transcript, Chord::plain(Enter), A::ListAccept),
        binding(Transcript, Chord::plain(Char('/')), A::SearchStart),
        binding(Transcript, Chord::plain(Char('n')), A::SearchNext),
        binding(Transcript, Chord::shift(Char('n')), A::SearchPrevious),
        binding(Transcript, Chord::plain(Char('r')), A::RestoreDraft),
        binding(Transcript, Chord::plain(Char('q')), A::Close),
        binding(Transcript, Chord::plain(Up), A::ScrollUp),
        binding(Transcript, Chord::plain(Down), A::ScrollDown),
        binding(Transcript, Chord::plain(Char('k')), A::ScrollUp),
        binding(Transcript, Chord::plain(Char('j')), A::ScrollDown),
        binding(Transcript, Chord::plain(PageUp), A::ScrollPageUp),
        binding(Transcript, Chord::plain(PageDown), A::ScrollPageDown),
    ]);
    rows
}

/// The resolved table.
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            bindings: default_bindings(),
        }
    }
}

impl Keymap {
    /// The action for one chord in one context, if any.
    pub fn resolve(&self, context: Context, chord: Chord) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| binding.context == context && binding.chord == chord)
            .map(|binding| binding.action)
    }

    /// Every row, for the shortcut sheet.
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// The chords bound to one action, in table order.
    pub fn chords_for(&self, action: Action) -> Vec<Chord> {
        self.bindings
            .iter()
            .filter(|binding| binding.action == action)
            .map(|binding| binding.chord)
            .collect()
    }

    /// The chords bound to one action in one context, falling back to any.
    ///
    /// The shortcut sheet uses this rather than [`Self::chords_for`]: `up` is
    /// history in the composer and scrolling in the transcript screen, and a
    /// sheet that lists both against one description teaches the wrong thing.
    pub fn chords_in(&self, context: Context, action: Action) -> Vec<Chord> {
        let scoped: Vec<Chord> = self
            .bindings
            .iter()
            .filter(|binding| binding.action == action && binding.context == context)
            .map(|binding| binding.chord)
            .collect();
        if scoped.is_empty() {
            self.chords_for(action)
        } else {
            scoped
        }
    }

    /// Rebinds one action, dropping whatever it had before.
    ///
    /// Returns an error rather than applying anything when the action is
    /// reserved: a terminal you cannot get out of is not a terminal.
    pub fn rebind(&mut self, action: Action, chords: &[Chord]) -> Result<(), String> {
        if action.reserved() {
            return Err(format!("{} is reserved and cannot be rebound", action.id()));
        }
        let contexts: Vec<Context> = self
            .bindings
            .iter()
            .filter(|binding| binding.action == action)
            .map(|binding| binding.context)
            .collect();
        let contexts = if contexts.is_empty() {
            vec![Context::Composer]
        } else {
            contexts
        };
        // A chord that a reserved action already owns cannot be taken.
        for chord in chords {
            if let Some(held) = self
                .bindings
                .iter()
                .find(|binding| binding.chord == *chord && binding.action.reserved())
            {
                return Err(format!(
                    "{chord} is held by {} and cannot be reused",
                    held.action.id()
                ));
            }
        }
        self.bindings.retain(|binding| binding.action != action);
        // A chord can only mean one thing per context. Taking it from whoever
        // held it is what the user asked for; leaving both in the table would
        // make the winner depend on row order, and the loser would look bound in
        // the shortcut sheet while doing nothing at all.
        for context in &contexts {
            self.bindings
                .retain(|binding| binding.context != *context || !chords.contains(&binding.chord));
        }
        for context in contexts {
            for chord in chords {
                self.bindings.push(binding(context, *chord, action));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn a_chord_round_trips_through_its_written_form() {
        assert_eq!(Chord::ctrl(Key::Char('o')).to_string(), "ctrl+o");
        assert_eq!(Chord::shift(Key::Tab).to_string(), "shift+tab");
        assert_eq!(Chord::plain(Key::Escape).to_string(), "escape");
        assert_eq!(Chord::plain(Key::Char(' ')).to_string(), "space");
    }

    #[test]
    fn shift_is_dropped_for_characters_the_terminal_already_applied() {
        let upper = Chord::from_event(&key(KeyCode::Char('A'), KeyModifiers::SHIFT)).unwrap();
        let lower = Chord::from_event(&key(KeyCode::Char('a'), KeyModifiers::NONE)).unwrap();
        assert_eq!(upper, lower);
    }

    #[test]
    fn back_tab_is_shift_tab_even_when_the_modifier_is_missing() {
        let chord = Chord::from_event(&key(KeyCode::BackTab, KeyModifiers::NONE)).unwrap();
        assert_eq!(chord, Chord::shift(Key::Tab));
        assert_eq!(
            Keymap::default().resolve(Context::Composer, chord),
            Some(Action::PermissionCycle)
        );
    }

    #[test]
    fn the_same_chord_means_different_things_in_different_contexts() {
        let map = Keymap::default();
        let enter = Chord::plain(Key::Enter);
        assert_eq!(map.resolve(Context::Composer, enter), Some(Action::Submit));
        assert_eq!(map.resolve(Context::Approval, enter), Some(Action::ListAccept));
    }

    #[test]
    fn the_reserved_chords_exist_in_every_context() {
        let map = Keymap::default();
        for context in [
            Context::Composer,
            Context::Approval,
            Context::Overlay,
            Context::Transcript,
        ] {
            assert_eq!(
                map.resolve(context, Chord::ctrl(Key::Char('c'))),
                Some(Action::Cancel),
                "{context:?}"
            );
            assert_eq!(
                map.resolve(context, Chord::plain(Key::Escape)),
                Some(Action::Escape),
                "{context:?}"
            );
        }
    }

    #[test]
    fn a_reserved_action_cannot_be_rebound() {
        let mut map = Keymap::default();
        assert!(map.rebind(Action::Cancel, &[Chord::ctrl(Key::Char('g'))]).is_err());
        assert_eq!(
            map.resolve(Context::Composer, Chord::ctrl(Key::Char('c'))),
            Some(Action::Cancel)
        );
    }

    #[test]
    fn a_reserved_chord_cannot_be_taken_by_something_else() {
        let mut map = Keymap::default();
        let error = map
            .rebind(Action::PaletteOpen, &[Chord::plain(Key::Escape)])
            .unwrap_err();
        assert!(error.contains("app:escape"), "{error}");
        // And nothing was applied: the old binding still works.
        assert_eq!(
            map.resolve(Context::Composer, Chord::ctrl(Key::Char('p'))),
            Some(Action::PaletteOpen)
        );
    }

    #[test]
    fn rebinding_replaces_rather_than_appends() {
        let mut map = Keymap::default();
        map.rebind(Action::PaletteOpen, &[Chord::ctrl(Key::Char('g'))])
            .unwrap();
        assert_eq!(
            map.resolve(Context::Composer, Chord::ctrl(Key::Char('g'))),
            Some(Action::PaletteOpen)
        );
        assert_eq!(map.resolve(Context::Composer, Chord::ctrl(Key::Char('p'))), None);
    }

    #[test]
    fn rebinding_onto_a_taken_chord_takes_it() {
        // Otherwise the winner depends on row order, and the loser looks bound
        // in the sheet while doing nothing.
        let mut map = Keymap::default();
        let chord = Chord::ctrl(Key::Char('o'));
        assert_eq!(map.resolve(Context::Composer, chord), Some(Action::FoldToggle));
        map.rebind(Action::PaletteOpen, &[chord]).unwrap();
        assert_eq!(map.resolve(Context::Composer, chord), Some(Action::PaletteOpen));
        assert!(map.chords_for(Action::FoldToggle).is_empty());
    }

    #[test]
    fn unbinding_hands_the_key_back_to_typing() {
        let mut map = Keymap::default();
        map.rebind(Action::HelpOpen, &[]).unwrap();
        assert_eq!(map.resolve(Context::Composer, Chord::plain(Key::Char('?'))), None);
    }

    #[test]
    fn the_transcript_screen_can_leave_its_search_box() {
        // `/` opens the box; without Enter bound, `n`, `N` and `q` are typed
        // into it and the screen has no way back out but Esc.
        let map = Keymap::default();
        assert_eq!(
            map.resolve(Context::Transcript, Chord::plain(Key::Enter)),
            Some(Action::ListAccept)
        );
        for (chord, action) in [
            (Chord::plain(Key::Char('n')), Action::SearchNext),
            (Chord::shift(Key::Char('n')), Action::SearchPrevious),
            (Chord::plain(Key::Char('q')), Action::Close),
        ] {
            assert_eq!(map.resolve(Context::Transcript, chord), Some(action), "{chord}");
        }
    }

    #[test]
    fn the_sheet_shows_the_binding_that_applies_where_it_is_read() {
        let map = Keymap::default();
        // `up` means two different things; the composer's is the one the sheet
        // shows against each, rather than every context's at once.
        assert_eq!(
            map.chords_in(Context::Composer, Action::ScrollUp),
            vec![Chord::alt(Key::Up)]
        );
        assert_eq!(
            map.chords_in(Context::Composer, Action::HistoryPrevious),
            vec![Chord::plain(Key::Up)]
        );
        // An action the composer has no binding for still gets its chords.
        assert!(!map.chords_in(Context::Composer, Action::SearchNext).is_empty());
    }

    #[test]
    fn every_action_the_sheet_can_show_has_an_id_and_a_description() {
        let map = Keymap::default();
        for binding in map.bindings() {
            assert!(binding.action.id().contains(':'));
            assert!(!binding.action.describe().is_empty());
        }
    }

    #[test]
    fn no_bare_letter_is_stolen_from_the_composer() {
        // A bare `c` for commit made `commit this` commit and leave `ommit this`
        // in the draft; a bare `q` for quit made `quick` close the program.
        let map = Keymap::default();
        for letter in 'a'..='z' {
            assert_eq!(
                map.resolve(Context::Composer, Chord::plain(Key::Char(letter))),
                None,
                "{letter} is not the composer's to take"
            );
        }
        // The ChangeSet keeps a chord that cannot be typed, plus its commands.
        assert_eq!(
            map.resolve(Context::Composer, Chord::ctrl(Key::Char('s'))),
            Some(Action::CommitChanges)
        );
    }

    #[test]
    fn a_terminal_whose_erase_key_is_ctrl_h_can_still_delete() {
        // `stty erase ^H` sends 0x08. crossterm decodes the C0 range as letters,
        // so it arrives as ctrl+h — binding ctrl+backspace alone leaves that
        // terminal with no working delete key.
        let map = Keymap::default();
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        let chord = Chord::from_event(&key).unwrap();
        assert_eq!(
            map.resolve(Context::Composer, chord),
            Some(Action::DeleteBack),
            "0x08 arrives as {chord}"
        );
        assert_eq!(
            map.resolve(Context::Composer, Chord::ctrl(Key::Backspace)),
            Some(Action::DeleteBack)
        );
    }

    #[test]
    fn the_readline_pair_reaches_the_same_actions_as_the_arrows() {
        let map = Keymap::default();
        for (chord, action) in [
            (Chord::alt(Key::Char('b')), Action::CaretWordLeft),
            (Chord::alt(Key::Char('f')), Action::CaretWordRight),
            (Chord::ctrl(Key::Char('a')), Action::CaretLineStart),
            (Chord::ctrl(Key::Char('e')), Action::CaretLineEnd),
            (Chord::ctrl(Key::Char('w')), Action::DeleteWord),
            (Chord::ctrl(Key::Char('u')), Action::DeleteToLineStart),
            (Chord::ctrl(Key::Char('k')), Action::DeleteToLineEnd),
        ] {
            assert_eq!(map.resolve(Context::Composer, chord), Some(action), "{chord}");
        }
    }
}
