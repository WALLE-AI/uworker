// Ported from dsh-code-agent (MIT), packages/dsh-tui.
//   Source: packages/dsh-tui/src/keybindings.ts @ d7cd008
//   Copied: 2026-08-31   Modified: yes
//   Changes: TypeScript → Rust; multi-key chord sequences are not supported
//            (no action here needs one, and a one-second prefix timer inside a
//            16 ms frame loop is a real cost for a hypothetical); the file is
//            read by host_io and parsed here as a pure function.
//! Rebinding keys from `~/.agentrs/keybindings.json`.
//!
//! The file maps an action id to a chord, a list of chords, or `null` to hand
//! the key back to typing:
//!
//! ```json
//! {
//!   "palette:open": "ctrl+g",
//!   "session:browse": ["ctrl+r", "alt+r"],
//!   "help:open": null
//! }
//! ```
//!
//! **Nothing in that file can stop the TUI from starting**, because the TUI is
//! where the user would go to fix it. Anything wrong — a misspelt action, an
//! unreadable chord, a chord already held by a reserved action — is reported and
//! then ignored, and the default for that one action stands. The rest of the
//! file still applies: one bad row does not throw away a good one.

use crate::keymap::{Action, Chord, Key, Keymap};

/// What was applied, and what was not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// Actions whose binding was replaced.
    pub rebound: Vec<Action>,
    /// One message per row that could not be applied.
    pub problems: Vec<String>,
}

/// Parses one chord, as the file writes it: `ctrl+g`, `shift+tab`, `alt+left`.
pub fn parse_chord(text: &str) -> Result<Chord, String> {
    let lowered = text.trim().to_lowercase();
    if lowered.is_empty() {
        return Err("empty chord".into());
    }
    let mut chord = Chord {
        key: Key::Char(' '),
        ctrl: false,
        alt: false,
        shift: false,
    };
    // A trailing `+` is the plus key itself: `ctrl++` binds control-plus.
    let mut parts: Vec<&str> = lowered.split('+').collect();
    if lowered.ends_with('+') {
        parts.pop();
        if let Some(last) = parts.last_mut() {
            if last.is_empty() {
                *last = "+";
            }
        }
    }
    let (name, modifiers) = parts.split_last().ok_or("empty chord")?;
    for modifier in modifiers {
        match *modifier {
            "ctrl" | "control" => chord.ctrl = true,
            "alt" | "opt" | "option" | "meta" => chord.alt = true,
            "shift" => chord.shift = true,
            other => return Err(format!("unknown modifier: {other}")),
        }
    }
    chord.key = match *name {
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "escape" | "esc" => Key::Escape,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "space" => Key::Char(' '),
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => Key::Char(ch),
                _ => return Err(format!("unknown key: {other}")),
            }
        }
    };
    Ok(chord)
}

/// The action with this id, if there is one.
fn action_by_id(id: &str) -> Option<Action> {
    // The keymap's default table names every action at least once, so the id
    // list can never drift from the action list.
    Keymap::default()
        .bindings()
        .iter()
        .map(|binding| binding.action)
        .find(|action| action.id() == id)
}

/// Applies a rebinding file to `keymap`, reporting what it could not do.
///
/// The file's own text is never echoed back in a problem message: it is user
/// content, and a message is a row on a shared notice line.
pub fn apply(keymap: &mut Keymap, source: &str) -> Applied {
    let mut applied = Applied::default();
    let parsed: serde_json::Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(error) => {
            applied
                .problems
                .push(format!("keybindings.json is not valid JSON: {error}"));
            return applied;
        }
    };
    let Some(map) = parsed.as_object() else {
        applied
            .problems
            .push("keybindings.json must be an object of action to chord".into());
        return applied;
    };

    for (id, value) in map {
        let Some(action) = action_by_id(id) else {
            applied.problems.push(format!("unknown action: {id}"));
            continue;
        };
        let texts: Vec<String> = match value {
            serde_json::Value::Null => Vec::new(),
            serde_json::Value::String(text) => vec![text.clone()],
            serde_json::Value::Array(items) => {
                let strings: Option<Vec<String>> = items
                    .iter()
                    .map(|item| item.as_str().map(str::to_string))
                    .collect();
                match strings {
                    Some(strings) => strings,
                    None => {
                        applied
                            .problems
                            .push(format!("{id}: every chord in the list must be a string"));
                        continue;
                    }
                }
            }
            _ => {
                applied
                    .problems
                    .push(format!("{id}: expected a chord, a list of chords, or null"));
                continue;
            }
        };

        let mut chords = Vec::with_capacity(texts.len());
        let mut broken = false;
        for text in &texts {
            match parse_chord(text) {
                Ok(chord) => chords.push(chord),
                Err(error) => {
                    applied.problems.push(format!("{id}: {error}"));
                    broken = true;
                    break;
                }
            }
        }
        if broken {
            continue;
        }
        // A chord given two meanings is a mistake, not a preference: applying
        // either one would leave the other silently dead.
        if let Some(duplicate) = chords
            .iter()
            .enumerate()
            .find(|(index, chord)| chords[..*index].contains(chord))
        {
            applied
                .problems
                .push(format!("{id}: {} is listed twice", duplicate.1));
            continue;
        }
        match keymap.rebind(action, &chords) {
            Ok(()) => applied.rebound.push(action),
            Err(error) => applied.problems.push(error),
        }
    }
    applied.rebound.sort_unstable();
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Context;

    fn keymap_from(source: &str) -> (Keymap, Applied) {
        let mut keymap = Keymap::default();
        let applied = apply(&mut keymap, source);
        (keymap, applied)
    }

    #[test]
    fn chords_parse_in_every_form_the_sheet_prints() {
        for text in ["ctrl+g", "shift+tab", "alt+left", "pageup", "escape", "space", "?"] {
            let chord = parse_chord(text).unwrap();
            assert_eq!(chord.to_string(), text.replace("pgup", "pageup"), "{text}");
        }
    }

    #[test]
    fn a_plus_key_is_not_a_modifier_separator() {
        let chord = parse_chord("ctrl++").unwrap();
        assert!(chord.ctrl);
        assert_eq!(chord.key, Key::Char('+'));
    }

    #[test]
    fn one_chord_or_a_list_of_them_both_work() {
        let (keymap, applied) = keymap_from(
            r#"{"palette:open": "alt+g", "session:browse": ["ctrl+r", "alt+r"]}"#,
        );
        assert!(applied.problems.is_empty(), "{:?}", applied.problems);
        assert_eq!(
            keymap.resolve(Context::Composer, parse_chord("alt+g").unwrap()),
            Some(Action::PaletteOpen)
        );
        for text in ["ctrl+r", "alt+r"] {
            assert_eq!(
                keymap.resolve(Context::Composer, parse_chord(text).unwrap()),
                Some(Action::BrowseLogs),
                "{text}"
            );
        }
    }

    #[test]
    fn null_hands_the_key_back_to_typing() {
        let (keymap, applied) = keymap_from(r#"{"help:open": null}"#);
        assert!(applied.problems.is_empty());
        assert_eq!(keymap.resolve(Context::Composer, parse_chord("?").unwrap()), None);
    }

    #[test]
    fn a_bad_row_is_reported_and_the_good_ones_still_apply() {
        let (keymap, applied) = keymap_from(
            r#"{"palette:open": "alt+g", "nonsense:verb": "alt+h", "fold:toggle": "ctrl+&&"}"#,
        );
        assert_eq!(applied.rebound, vec![Action::PaletteOpen]);
        assert_eq!(applied.problems.len(), 2);
        assert!(applied.problems.iter().any(|note| note.contains("unknown action")));
        // The default for the broken row stands.
        assert_eq!(
            keymap.resolve(Context::Composer, parse_chord("ctrl+o").unwrap()),
            Some(Action::FoldToggle)
        );
    }

    #[test]
    fn a_reserved_chord_cannot_be_taken_and_the_file_still_loads() {
        let (keymap, applied) = keymap_from(r#"{"palette:open": "escape"}"#);
        assert!(applied.rebound.is_empty());
        assert!(applied.problems[0].contains("app:escape"), "{:?}", applied.problems);
        assert_eq!(
            keymap.resolve(Context::Composer, parse_chord("escape").unwrap()),
            Some(Action::Escape)
        );
    }

    #[test]
    fn a_reserved_action_cannot_be_rebound_or_unbound() {
        let (keymap, applied) = keymap_from(r#"{"app:cancel": "alt+g", "app:escape": null}"#);
        assert!(applied.rebound.is_empty());
        assert_eq!(applied.problems.len(), 2);
        assert_eq!(
            keymap.resolve(Context::Composer, parse_chord("ctrl+c").unwrap()),
            Some(Action::Cancel)
        );
    }

    #[test]
    fn one_chord_given_two_meanings_in_a_row_is_refused() {
        let (_, applied) = keymap_from(r#"{"palette:open": ["alt+g", "alt+g"]}"#);
        assert!(applied.problems[0].contains("twice"), "{:?}", applied.problems);
    }

    #[test]
    fn a_broken_file_never_stops_the_tui() {
        for source in ["", "not json", "[1,2]", r#"{"palette:open": 7}"#] {
            let (keymap, applied) = keymap_from(source);
            assert!(!applied.problems.is_empty(), "{source:?}");
            // The whole default table is intact.
            assert_eq!(
                keymap.resolve(Context::Composer, parse_chord("ctrl+p").unwrap()),
                Some(Action::PaletteOpen)
            );
        }
    }
}
