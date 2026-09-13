use std::fmt::Write;
use std::path::{Path, PathBuf};

use agentrs_memory::age::memory_age;
use agentrs_memory::index::remove_index_entry;
use agentrs_memory::paths::{is_memory_path, memory_entrypoint};
use agentrs_memory::store::{delete_memory, read_memory, scan_memory_files};
use agentrs_memory::types::MemoryHeader;
use async_trait::async_trait;
use chrono::Utc;

use super::{CommandContext, CommandResult, SlashCommand};

const USAGE: &str = "Usage: /memory | /memory show <file> | /memory rm <file> | /memory path";

pub(super) struct MemoryCommand;

#[async_trait]
impl SlashCommand for MemoryCommand {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "List, inspect, or delete stored memories"
    }

    async fn execute(&self, ctx: &mut CommandContext<'_>, args: &str) -> anyhow::Result<CommandResult> {
        let Some(memory_dir) = ctx.memory_dir else {
            ctx.output
                .emit_error("Memory is disabled for this session; there is nothing to show.");
            return Ok(CommandResult::Continue);
        };

        let (subcommand, argument) = split_subcommand(args);
        match subcommand {
            "" => {
                ctx.output.emit_info(&list_memories(memory_dir));
                Ok(CommandResult::Continue)
            }
            "path" => {
                ctx.output.emit_info(&memory_dir.display().to_string());
                Ok(CommandResult::Continue)
            }
            "show" => {
                match resolve_memory_file(memory_dir, argument) {
                    Ok(path) => match read_memory(&path) {
                        Ok(entry) => ctx.output.emit_info(&format_entry(&path, &entry.content)),
                        Err(error) => ctx.output.emit_error(&format!("Could not read {argument}: {error}")),
                    },
                    Err(message) => ctx.output.emit_error(&message),
                }
                Ok(CommandResult::Continue)
            }
            "rm" => match resolve_memory_file(memory_dir, argument) {
                Ok(path) => Ok(remove_memory(ctx, memory_dir, &path, argument)),
                Err(message) => {
                    ctx.output.emit_error(&message);
                    Ok(CommandResult::Continue)
                }
            },
            other => {
                ctx.output
                    .emit_error(&format!("Unknown /memory subcommand: {other}. {USAGE}"));
                Ok(CommandResult::Continue)
            }
        }
    }
}

/// Split `args` into a subcommand and the remainder.
fn split_subcommand(args: &str) -> (&str, &str) {
    let trimmed = args.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (trimmed, ""),
    }
}

/// Resolve a user-supplied name to a file inside the memory directory.
///
/// The name is joined and then re-checked against the directory rather than
/// merely rejected for looking suspicious: `..` segments and absolute paths
/// both collapse into a membership question, and that is the one the answer
/// depends on.
fn resolve_memory_file(memory_dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() {
        return Err(format!("Which memory? {USAGE}"));
    }
    let candidate = memory_dir.join(name);
    if !is_memory_path(&candidate, memory_dir) {
        return Err(format!("{name} is not inside the memory directory."));
    }
    if !candidate.is_file() {
        return Err(format!("No memory named {name}. Run /memory to list them."));
    }
    Ok(candidate)
}

fn list_memories(memory_dir: &Path) -> String {
    let headers = scan_memory_files(memory_dir).unwrap_or_default();
    if headers.is_empty() {
        return format!("No memories stored yet.\nMemory directory: {}", memory_dir.display());
    }

    let now = Utc::now();
    let mut output = format!("{} memories in {}\n", headers.len(), memory_dir.display());
    for header in &headers {
        writeln!(&mut output, "  {}", format_header(header, now)).expect("writing to String cannot fail");
    }
    output.push_str("\nRun /memory show <file> to read one, /memory rm <file> to delete it.");
    output
}

fn format_header(header: &MemoryHeader, now: chrono::DateTime<Utc>) -> String {
    let type_tag = header
        .memory_type
        .map(|memory_type| format!("[{memory_type}] "))
        .unwrap_or_default();
    let age = memory_age(header.mtime, now);
    let description = header
        .description
        .as_deref()
        .map(|text| format!(" \u{2014} {text}"))
        .unwrap_or_default();
    format!("{type_tag}{} ({age}){description}", header.filename)
}

fn format_entry(path: &Path, content: &str) -> String {
    format!("{}\n\n{}", path.display(), content.trim())
}

/// Delete the file and its index line, then report what changed.
///
/// A failure to clean the index is reported but not fatal: the file is already
/// gone, and leaving the user believing it still exists would be worse than a
/// stale index line they can see named in the message.
fn remove_memory(ctx: &mut CommandContext<'_>, memory_dir: &Path, path: &Path, name: &str) -> CommandResult {
    if let Err(error) = delete_memory(path) {
        ctx.output.emit_error(&format!("Could not delete {name}: {error}"));
        return CommandResult::Continue;
    }
    let filename = path
        .file_name()
        .map(|filename| filename.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_owned());
    if let Err(error) = remove_index_entry(&memory_entrypoint(memory_dir), &filename) {
        ctx.output.emit_error(&format!(
            "Deleted {filename}, but its MEMORY.md entry could not be removed: {error}"
        ));
    } else {
        ctx.output.emit_info(&format!("Deleted {filename}"));
    }
    // The memory section of the system prompt now describes a file that is
    // gone, so the caller must rebuild it.
    CommandResult::ContextChanged
}

#[cfg(test)]
#[path = "memory_test.rs"]
mod memory_test;
