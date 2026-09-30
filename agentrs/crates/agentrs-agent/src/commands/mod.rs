mod agents;
pub mod clear;
pub mod compact;
mod context;
pub mod help;
mod memory;
pub mod quit;
mod registry;
mod teams;
#[cfg(test)]
mod test_support;

pub use registry::{CommandContext, CommandRegistry, CommandResult, CommandSpec, SlashCommand, default_registry};
