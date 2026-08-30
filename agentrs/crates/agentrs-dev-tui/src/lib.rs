//! Test-oriented terminal host for AgentRS.
//!
//! This crate is deliberately outside the kernel. It projects runtime events,
//! owns terminal lifecycle, and asks a human to resolve development approvals.

pub mod event_sink;
pub mod sanitize;
pub mod state;
pub mod terminal;
pub mod ui;
