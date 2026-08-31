//! Test-oriented terminal host for AgentRS.
//!
//! This crate is deliberately outside the kernel. It projects runtime events,
//! owns terminal lifecycle, and asks a human to resolve development approvals.
//!
//! Its interaction form is ported from `dsh-code-agent`
//! (`packages/dsh-tui/src/`), module by module, so the two can be compared
//! directly; each module names its source file in its own documentation.
//! Everything except [`host_io`] is a pure function of injected values.

pub mod approval;
pub mod capabilities;
pub mod collapse;
pub mod completion;
pub mod composer;
pub mod diff;
pub mod event_sink;
pub mod glyphs;
pub mod host_io;
pub mod keybindings;
pub mod keymap;
pub mod markdown;
pub mod notices;
pub mod overlay;
pub mod spinner;
pub mod state;
pub mod status_line;
pub mod styling;
pub mod surfaces;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod tool_card;
pub mod transcript;
pub mod ui;
pub mod working_line;
