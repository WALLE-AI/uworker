//! The one module that touches the operating system.
//!
//! AgentRS itself may not read the clock, the filesystem, or the environment —
//! that is the boundary rule of 架构方案 §1.1, enforced by `clippy.toml` and by
//! `scripts/check-no-env.sh`. `agentrs-dev-tui` is a **host**, not the kernel, so
//! it legitimately needs all three: a spinner needs a real clock, rebindable keys
//! need a config file, `@` completion needs a directory listing.
//!
//! Rather than sprinkle those calls through the UI, every one of them lives
//! here, and this file is the only path the gate script exempts. Everything else
//! in the crate stays a pure function of injected values, which is also what
//! keeps the renderer snapshot-testable.

#![allow(
    clippy::disallowed_methods,
    reason = "host boundary: a spinner and an elapsed field need a real clock; \
              the kernel's Clock port exists for the kernel, which this is not"
)]
#![allow(
    clippy::disallowed_types,
    reason = "host boundary: ctrl+x hands the terminal to $EDITOR, which is a \
              process. The kernel's ban stands — it has no execution authority \
              and reaches the world through SandboxExecutor; this file is the \
              host, and the one place the gate script exempts"
)]

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Monotonic-enough wall clock in milliseconds.
///
/// Every duration the UI shows is a difference between two of these, so what
/// matters is that the two come from the same source, not that the value is a
/// true epoch. A clock that steps backwards produces a saturating zero rather
/// than an underflow.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Elapsed milliseconds between two [`now_ms`] readings, floored at zero.
pub const fn elapsed_ms(from: u64, to: u64) -> u64 {
    to.saturating_sub(from)
}

/// Reads one environment variable, or the empty string.
pub fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Reads several environment variables and joins their values.
///
/// Used for the locale and SSH probes, where any one of a group being set is the
/// signal and which one it was does not matter.
pub fn env_joined(names: &[&str]) -> String {
    names.iter().map(|name| env(name)).collect::<Vec<_>>().join("")
}

/// True when both stdin and stdout are terminals.
///
/// A redirected run cannot be driven, and saying so is what turns a blank frame
/// into a sentence the user can act on.
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Builds the capability probe from the process environment.
pub fn probe_environment(interactive: bool) -> crate::capabilities::EnvProbe {
    crate::capabilities::EnvProbe {
        term: env("TERM"),
        color_term: env("COLORTERM"),
        no_color: env("NO_COLOR"),
        force_color: env("FORCE_COLOR"),
        wt_session: env("WT_SESSION"),
        tmux: env("TMUX"),
        ssh: env_joined(&["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]),
        locale: env_joined(&["LC_ALL", "LC_CTYPE", "LANG"]),
        interactive,
        windows: cfg!(windows),
    }
}

/// The directory rebindable keys and other host config live in.
///
/// `$AGENTRS_HOME`, or `~/.agentrs`. A home directory that cannot be read costs
/// the config, never the session.
pub fn config_dir() -> Option<PathBuf> {
    let explicit = env("AGENTRS_HOME");
    if !explicit.is_empty() {
        return Some(PathBuf::from(explicit));
    }
    let home = env_joined(&["HOME", "USERPROFILE"]);
    (!home.is_empty()).then(|| PathBuf::from(home).join(".agentrs"))
}

/// Reads a UTF-8 file, or `None` when it is absent or unreadable.
///
/// The distinction between "absent" and "unreadable" is deliberately dropped:
/// nothing here may stop the TUI from starting, because the TUI is where the
/// user would go to fix whatever is wrong.
pub fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Lists the entries of a directory, files and directories separately.
///
/// Both lists are sorted, so completion and the browser are deterministic.
/// Hidden entries are dropped: `@` completion over a repository is otherwise
/// mostly `.git`.
pub fn list_dir(path: &Path) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    let Ok(entries) = std::fs::read_dir(path) else {
        return (files, dirs);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    files.sort();
    dirs.sort();
    (files, dirs)
}

/// One durable log the browser can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// Full path.
    pub path: PathBuf,
    /// File name, which carries the run's UUIDv7 and therefore its time order.
    pub name: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// Lists the durable JSONL logs beside `log`, newest name first.
///
/// The name carries a UUIDv7, so sorting by name descending is sorting by time
/// descending — no `mtime` call, and no dependence on a clock the logs were not
/// written with.
pub fn list_logs(log: &Path) -> Vec<LogEntry> {
    let dir = log.parent().unwrap_or(Path::new("."));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<LogEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "jsonl" {
                return None;
            }
            Some(LogEntry {
                name: path.file_name()?.to_string_lossy().to_string(),
                bytes: entry.metadata().map(|meta| meta.len()).unwrap_or(0),
                path,
            })
        })
        .collect();
    out.sort_by(|a, b| b.name.cmp(&a.name));
    out
}

/// Writes a UTF-8 file, creating its directory.
pub fn write_text(path: &Path, body: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
    }
    std::fs::write(path, body).map_err(|error| error.to_string())
}

/// Whether a path exists.
pub fn exists(path: &Path) -> bool {
    path.exists()
}

/// Opens `path` in `$EDITOR`, handing it the terminal for the duration.
///
/// The caller must have left raw mode and the alternate screen first: an editor
/// that inherits a terminal in raw mode paints over the frame and leaves the
/// user with neither.
pub fn open_editor(path: &Path, line: Option<usize>) -> Result<(), String> {
    let editor = env_joined(&["VISUAL"]);
    let editor = if editor.is_empty() { env("EDITOR") } else { editor };
    if editor.is_empty() {
        return Err("neither $VISUAL nor $EDITOR is set".into());
    }
    // The variable may carry arguments (`code -w`), which is the common case for
    // a GUI editor that must be told to wait.
    let mut parts = editor.split_whitespace();
    let program = parts.next().ok_or("editor command is empty")?;
    let mut command = std::process::Command::new(program);
    command.args(parts);
    // `+N` is the one line-number form every terminal editor understands.
    if let Some(line) = line {
        command.arg(format!("+{line}"));
    }
    command.arg(path);
    let status = command
        .status()
        .map_err(|error| format!("could not start {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_never_underflows_on_a_backwards_clock() {
        assert_eq!(elapsed_ms(100, 40), 0);
        assert_eq!(elapsed_ms(40, 100), 60);
    }

    #[test]
    fn a_missing_variable_reads_as_empty_not_as_an_error() {
        assert_eq!(env("AGENTRS_TUI_DEFINITELY_UNSET_VARIABLE"), "");
        assert_eq!(env_joined(&["AGENTRS_TUI_UNSET_A", "AGENTRS_TUI_UNSET_B"]), "");
    }

    #[test]
    fn an_unreadable_file_reads_as_absent_rather_than_stopping_anything() {
        assert_eq!(read_text(Path::new("/definitely/not/here.json")), None);
        assert_eq!(list_dir(Path::new("/definitely/not/here")), (vec![], vec![]));
        assert!(list_logs(Path::new("/definitely/not/here/x.jsonl")).is_empty());
    }

    #[test]
    fn an_editor_that_is_not_configured_says_so_rather_than_guessing() {
        // Guessing at `vi` would be a surprise on a machine that has no terminal
        // editor at all, and the message is the fix.
        if env("EDITOR").is_empty() && env("VISUAL").is_empty() {
            let error = open_editor(Path::new("/tmp/x"), None).unwrap_err();
            assert!(error.contains("EDITOR"), "{error}");
        }
    }
}
