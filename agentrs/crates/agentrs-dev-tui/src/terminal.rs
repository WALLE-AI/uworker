//! Panic-safe terminal lifecycle ownership.

use std::io::{self, Stdout};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// Concrete terminal type used by the executable.
pub type DevTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Owns raw mode, the alternate screen, and mouse capture until dropped.
///
/// The wheel is claimed for the in-app viewport. On the alternate screen the
/// terminal's own scroll buffer is not reachable anyway, so leaving the wheel
/// to the terminal would leave it doing nothing at all.
pub struct TerminalGuard {
    terminal: DevTerminal,
}

impl TerminalGuard {
    /// Enters raw mode, the alternate screen, and mouse capture.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }

    /// Claims the wheel for the app, or hands it back to the terminal.
    ///
    /// With capture on, the wheel scrolls the transcript but drag-select and the
    /// terminal's own copy stop working — which is the trade `/mouse` exists to
    /// let the reader make, rather than being made for them once at startup.
    pub fn set_mouse_capture(&mut self, capture: bool) -> io::Result<()> {
        if capture {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)
        }
    }

    /// Hands the terminal back to the OS for the duration of `body`.
    ///
    /// An editor that inherits a terminal in raw mode on the alternate screen
    /// paints over the frame and leaves the user with neither, so the guard is
    /// unwound and rebuilt around the call. The screen is redrawn from scratch
    /// afterwards because whatever ran had the whole window.
    pub fn suspended<T>(&mut self, body: impl FnOnce() -> T) -> io::Result<T> {
        disable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        )?;
        let out = body();
        enable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            Hide
        )?;
        self.terminal.clear()?;
        Ok(out)
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
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}
