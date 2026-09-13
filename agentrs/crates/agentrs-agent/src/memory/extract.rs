// Background memory extraction.
//
// At the end of a completed query loop, fork a restricted sub-agent that reads
// what just happened and persists anything worth remembering. The main agent
// never has to be told to save; it only has to be worth saving from.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use agentrs_config::memory::ExtractConfig;
use agentrs_memory::store::{format_memory_manifest, scan_memory_files};
use agentrs_types::message::{ContentBlock, Message, Role};
use agentrs_types::subagent::{ForkOverrides, Spawner, SubAgentIsolation, SubAgentSpec};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::spawner::AgentSpawner;

/// Tools the extraction agent may use.
///
/// Read/Grep/Glob let it check a claim before recording it; Write/Edit let it
/// persist. Everything else — shell, network, spawning further agents — is
/// withheld, and the write pair is confined to the memory directory on top of
/// that.
const EXTRACTION_TOOLS: [&str; 5] = ["Read", "Grep", "Glob", "Write", "Edit"];

/// How much of the transcript to hand the extraction agent.
///
/// Long enough to see how an exchange resolved, short enough that the pass
/// stays cheap.
const MAX_TRANSCRIPT_CHARS: usize = 24_000;

/// Forks a restricted sub-agent to persist durable memories from a finished
/// turn.
///
/// Skipped when the main agent already wrote memory this turn: its prompt
/// carries the full save instructions, so a second pass would at best
/// duplicate and at worst contradict.
pub(crate) struct MemoryExtractor {
    spawner: Arc<AgentSpawner>,
    memory_dir: PathBuf,
    config: ExtractConfig,
    in_flight: Mutex<Option<JoinHandle<()>>>,
}

impl MemoryExtractor {
    pub(crate) fn new(spawner: Arc<AgentSpawner>, memory_dir: PathBuf, config: ExtractConfig) -> Self {
        Self {
            spawner,
            memory_dir,
            config,
            in_flight: Mutex::new(None),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Start an extraction over `transcript` and return immediately.
    ///
    /// Reports whether a pass actually started, so the caller can log the
    /// decision rather than guess at it. The caller must reach
    /// [`MemoryExtractor::drain`] before the process exits, or a half-written
    /// memory file can outlive the run.
    pub(crate) fn spawn_extraction(&self, transcript: &[Message]) -> bool {
        if !self.config.enabled {
            return false;
        }
        let Some(prompt) = self.build_prompt(transcript) else {
            return false;
        };

        let spawner = Arc::clone(&self.spawner);
        let spec = SubAgentSpec {
            name: "memory-extraction".to_string(),
            agent_type: None,
            prompt,
            max_turns: Some(self.config.max_turns),
            max_tokens: None,
            system_prompt: Some(SYSTEM_PROMPT.to_string()),
            depth: 0,
            resume: None,
            persistent: false,
            isolation: SubAgentIsolation::Shared,
        };
        let overrides = ForkOverrides {
            model: self.config.model.clone(),
            effort: None,
            allowed_tools: EXTRACTION_TOOLS.iter().map(|name| (*name).to_string()).collect(),
            write_root: Some(self.memory_dir.clone()),
        };

        // A previous extraction still running is left to finish; its handle is
        // replaced rather than awaited, and `drain` only ever waits on the
        // newest. Turns are far apart relative to one extraction, so this is a
        // corner case, and dropping the older handle detaches rather than
        // cancels it.
        let handle = tokio::spawn(async move {
            let result = spawner.spawn(spec, overrides, CancellationToken::new()).await;
            if result.status.is_error() {
                tracing::debug!(
                    target: "agentrs_memory",
                    status = ?result.status,
                    "memory extraction did not complete",
                );
            } else {
                tracing::debug!(target: "agentrs_memory", "memory extraction finished");
            }
        });
        *self.in_flight.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);
        true
    }

    /// Wait for an in-flight extraction, up to `timeout`.
    ///
    /// On timeout the task is aborted: a caller that waited its budget would
    /// rather lose the memory than hang the exit.
    pub(crate) async fn drain(&self, timeout: Duration) {
        let handle = self
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(handle) = handle else {
            return;
        };
        if tokio::time::timeout(timeout, &mut { handle }).await.is_err() {
            tracing::debug!(
                target: "agentrs_memory",
                timeout_ms = timeout.as_millis() as u64,
                "memory extraction abandoned at shutdown",
            );
        }
    }

    /// Assemble the extraction prompt, or `None` when there is nothing to read.
    fn build_prompt(&self, transcript: &[Message]) -> Option<String> {
        let conversation = render_transcript(transcript);
        if conversation.trim().is_empty() {
            return None;
        }
        // The existing memories are injected rather than left to be discovered,
        // so the agent does not spend a turn listing the directory before it
        // can tell a new observation from one already on file.
        let headers = scan_memory_files(&self.memory_dir).unwrap_or_default();
        let manifest = if headers.is_empty() {
            "(no memories stored yet)".to_string()
        } else {
            format_memory_manifest(&headers)
        };

        Some(format!(
            "Memory directory: {}\n\n\
             <existing-memories>\n{manifest}\n</existing-memories>\n\n\
             <conversation>\n{conversation}\n</conversation>\n\n\
             Extract anything durable from the conversation above, following your instructions. \
             If nothing in it is worth remembering, say so and stop.",
            self.memory_dir.display()
        ))
    }
}

/// Flatten the turn into something readable, dropping everything the extractor
/// cannot use.
///
/// Tool results are the bulk of a transcript and the least informative part for
/// this purpose — file contents and command output say what the work touched,
/// not what was learned about the user or the project.
fn render_transcript(transcript: &[Message]) -> String {
    let mut rendered = String::new();
    for message in transcript {
        let speaker = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            _ => continue,
        };
        for block in &message.content {
            let ContentBlock::Text { text } = block else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            rendered.push_str(speaker);
            rendered.push_str(": ");
            rendered.push_str(text);
            rendered.push('\n');
        }
    }

    // Keep the tail: how an exchange ended carries the correction or the
    // confirmation, which is exactly what is worth saving.
    if rendered.len() > MAX_TRANSCRIPT_CHARS {
        let target = rendered.len() - MAX_TRANSCRIPT_CHARS;
        // Cut on a line break at or after the target so a message is dropped
        // whole rather than beheaded. Walking char boundaries rather than bytes
        // keeps the cut off the middle of a multi-byte character.
        let boundary = rendered
            .char_indices()
            .find(|(index, character)| *index >= target && *character == '\n')
            .map(|(index, _)| index + 1)
            .or_else(|| {
                rendered
                    .char_indices()
                    .find(|(index, _)| *index >= target)
                    .map(|(index, _)| index)
            })
            .unwrap_or(0);
        rendered = rendered.split_off(boundary);
    }
    rendered
}

/// System prompt for the extraction pass.
const SYSTEM_PROMPT: &str = "\
You extract durable memories from a finished conversation between a user and a coding agent.

You are not continuing the conversation and you are not answering the user. Your only job is to \
decide what, if anything, from the exchange should be remembered for future sessions, and to write \
it down.

## What to save

<types>
<type name=\"user\">Who the user is: role, expertise, preferences, how they want to be worked with. \
Save these so future sessions can tailor their approach. Never record a negative judgement.</type>
<type name=\"feedback\">Guidance the user gave about how to approach work \u{2014} corrections AND \
confirmations. If you only record corrections, future sessions will avoid past mistakes but drift \
away from approaches the user already validated. Lead with the rule, then a **Why:** line and a \
**How to apply:** line.</type>
<type name=\"project\">Ongoing work, goals, constraints, or incidents not derivable from the code or \
git history. Convert relative dates to absolute ones \u{2014} \"Thursday\" is meaningless in three \
weeks.</type>
<type name=\"reference\">Pointers to external systems: dashboards, tickets, docs, channels.</type>
</types>

## What not to save

Do not save anything the repository already records: code structure, what a function does, how a \
bug was fixed, what is in the git history, or what CLAUDE.md/AGENTS.md already states. Do not save \
what mattered only inside this one conversation. Saving nothing is the correct and common outcome \
\u{2014} most turns teach nothing durable, and a directory full of restated code comments makes the \
real memories harder to find.

Check the existing memories you were given before writing. If an observation refines one that is \
already on file, update that file instead of adding a near-duplicate.

## How to save

Write one file per fact into the memory directory, named `<type>_<short-slug>.md`:

```markdown
---
name: <short-kebab-case-slug>
description: <one-line summary>
metadata:
  type: user | feedback | project | reference
---

<the fact>
```

Then append one line to `MEMORY.md` in the same directory: `- [Title](file.md) \u{2014} hook`. \
Never put memory content in `MEMORY.md` itself; it is an index.

You may only write inside the memory directory. Reading elsewhere is allowed when you need to \
verify a claim before recording it.";

#[cfg(test)]
#[path = "extract_test.rs"]
mod extract_test;
