use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::Composer;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn edits_unicode_text_without_splitting_characters() {
    let mut composer = Composer::default();
    composer.input(key(KeyCode::Char('你')));
    composer.input(key(KeyCode::Char('好')));
    composer.input(key(KeyCode::Left));
    composer.input(key(KeyCode::Backspace));
    assert_eq!(composer.text(), "好");
}

#[test]
fn command_replacement_moves_cursor_to_end() {
    let mut composer = Composer::default();
    composer.replace_command("compact");
    composer.input(key(KeyCode::Char(' ')));
    assert_eq!(composer.text(), "/compact ");
}

#[test]
fn cjk_cursor_uses_display_width() {
    let mut composer = Composer::default();
    composer.input(key(KeyCode::Char('你')));
    composer.input(key(KeyCode::Char('a')));
    assert_eq!(composer.visual_cursor(20), (3, 0));
}

#[test]
fn shift_enter_inserts_a_newline() {
    let mut composer = Composer::default();
    composer.input(key(KeyCode::Char('a')));
    composer.input(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    composer.input(key(KeyCode::Char('b')));
    assert_eq!(composer.text(), "a\nb");
}

#[test]
fn control_j_is_a_newline_fallback() {
    let mut composer = Composer::default();
    composer.input(key(KeyCode::Char('a')));
    composer.input(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    composer.input(key(KeyCode::Char('b')));
    assert_eq!(composer.text(), "a\nb");
}

#[test]
fn ctrl_h_erases_like_backspace_for_terminals_that_send_0x08() {
    let mut composer = Composer::default();
    composer.insert_text("你好ab");

    let handled = composer.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));

    assert!(handled, "Ctrl+H must be consumed, not passed through as text");
    assert_eq!(composer.text(), "你好a");
}

#[test]
fn ctrl_h_on_an_empty_composer_is_not_consumed() {
    let mut composer = Composer::default();

    let handled = composer.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));

    // Nothing to erase: leave the key for whatever binding owns it.
    assert!(!handled);
    assert_eq!(composer.text(), "");
}

#[test]
fn ctrl_h_never_inserts_a_literal_h() {
    let mut composer = Composer::default();
    composer.insert_text("x");

    composer.input(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));

    assert_eq!(composer.text(), "");
}
