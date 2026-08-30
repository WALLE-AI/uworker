//! Panic-safe terminal lifecycle ownership.

use std::io::{self, Stdout};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// Concrete terminal type used by the executable.
pub type DevTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Owns raw mode and the alternate screen until dropped.
pub struct TerminalGuard {
    terminal: DevTerminal,
}

impl TerminalGuard {
    /// Enters raw mode and the alternate screen.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }

    /// Mutable terminal handle used for drawing.
    pub fn terminal(&mut self) -> &mut DevTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}
