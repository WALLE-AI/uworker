use super::*;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use agentrs_memory::index::append_index_entry;
    use agentrs_memory::paths::ENTRYPOINT_NAME;
    use agentrs_providers::{LlmProvider, ProviderError};
    use agentrs_types::llm::{LlmEvent, LlmRequest};
    use agentrs_types::message::Message;

    use super::*;
    use crate::commands::{CommandContext, CommandRegistry};
    use crate::compact::state::CompactState;
    use crate::context_usage::{ContextState, PromptUsage};
    use crate::output::OutputSink;

    struct NullProvider;
    #[async_trait::async_trait]
    impl LlmProvider for NullProvider {
        async fn stream(&self, _: &LlmRequest) -> Result<tokio::sync::mpsc::Receiver<LlmEvent>, ProviderError> {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }
    }

    /// Captures what the command told the user, so assertions can read it.
    #[derive(Default)]
    struct RecordingSink {
        info: Mutex<Vec<String>>,
        errors: Mutex<Vec<String>>,
    }

    impl RecordingSink {
        fn info(&self) -> String {
            self.info.lock().unwrap().join("\n")
        }

        fn errors(&self) -> String {
            self.errors.lock().unwrap().join("\n")
        }
    }

    impl OutputSink for RecordingSink {
        fn emit_text_delta(&self, _: &str, _: &str) {}
        fn emit_thinking(&self, _: &str, _: &str) {}
        fn emit_tool_call(&self, _: &str, _: &str, _: &str) {}
        fn emit_tool_result(&self, _: &str, _: &str, _: bool, _: &str) {}
        fn emit_stream_start(&self, _: &str) {}
        fn emit_stream_end(&self, _: &str, _: usize, _: u64, _: u64, _: u64, _: u64) {}
        fn emit_error(&self, text: &str) {
            self.errors.lock().unwrap().push(text.to_owned());
        }
        fn emit_info(&self, text: &str) {
            self.info.lock().unwrap().push(text.to_owned());
        }
    }

    /// Owns everything `CommandContext` borrows, so tests can build one in a
    /// single line without a dozen locals apiece.
    struct Harness {
        messages: Vec<Message>,
        compact_state: CompactState,
        compact_config: agentrs_config::compact::CompactConfig,
        context_state: ContextState,
        prompt_usage: PromptUsage,
        context_tools: Vec<agentrs_types::tool::ToolDef>,
        registry: CommandRegistry,
        output: RecordingSink,
        memory_dir: Option<PathBuf>,
    }

    impl Harness {
        fn new(memory_dir: Option<PathBuf>) -> Self {
            Self {
                messages: Vec::new(),
                compact_state: CompactState::new(),
                compact_config: Default::default(),
                context_state: ContextState::default(),
                prompt_usage: PromptUsage::default(),
                context_tools: Vec::new(),
                registry: CommandRegistry::new(),
                output: RecordingSink::default(),
                memory_dir,
            }
        }

        async fn run(&mut self, args: &str) -> CommandResult {
            let mut ctx = CommandContext {
                messages: &mut self.messages,
                compact_state: &mut self.compact_state,
                compact_config: &self.compact_config,
                provider: Arc::new(NullProvider),
                model: "test",
                internal_summarizer: None,
                output: &self.output,
                registry: &self.registry,
                context_state: &mut self.context_state,
                prompt_usage: &self.prompt_usage,
                context_tools: &self.context_tools,
                memory_dir: self.memory_dir.as_deref(),
                subagent_registry: None,
                team_runtime: None,
                dynamic_system_tokens: 0,
            };
            MemoryCommand.execute(&mut ctx, args).await.unwrap()
        }
    }

    fn populated_dir() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("memory");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("user_role.md"),
            "---\nname: user_role\ndescription: senior Rust engineer\ntype: user\n---\n\nThe user writes Rust.\n",
        )
        .unwrap();
        append_index_entry(
            &dir.join(ENTRYPOINT_NAME),
            "User role",
            "user_role.md",
            "senior Rust engineer",
        )
        .unwrap();
        (temp, dir)
    }

    #[tokio::test]
    async fn listing_shows_type_age_and_description() {
        let (_temp, dir) = populated_dir();
        let mut harness = Harness::new(Some(dir));

        let result = harness.run("").await;

        assert_eq!(result, CommandResult::Continue);
        let listed = harness.output.info();
        assert!(listed.contains("[user] user_role.md (today)"));
        assert!(listed.contains("senior Rust engineer"));
    }

    #[tokio::test]
    async fn an_empty_directory_says_so_and_still_names_the_path() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("memory");
        std::fs::create_dir_all(&dir).unwrap();
        let mut harness = Harness::new(Some(dir.clone()));

        harness.run("").await;

        let listed = harness.output.info();
        assert!(listed.contains("No memories stored yet"));
        assert!(listed.contains(&dir.display().to_string()));
    }

    #[tokio::test]
    async fn show_prints_the_body() {
        let (_temp, dir) = populated_dir();
        let mut harness = Harness::new(Some(dir));

        harness.run("show user_role.md").await;

        assert!(harness.output.info().contains("The user writes Rust."));
        assert!(harness.output.errors().is_empty());
    }

    #[tokio::test]
    async fn path_prints_the_memory_directory() {
        let (_temp, dir) = populated_dir();
        let mut harness = Harness::new(Some(dir.clone()));

        harness.run("path").await;

        assert_eq!(harness.output.info().trim(), dir.display().to_string());
    }

    // Deleting must clear both the file and its index line, or the prompt keeps
    // advertising a memory that no longer exists.
    #[tokio::test]
    async fn rm_deletes_the_file_and_its_index_entry() {
        let (_temp, dir) = populated_dir();
        let mut harness = Harness::new(Some(dir.clone()));

        let result = harness.run("rm user_role.md").await;

        assert_eq!(
            result,
            CommandResult::ContextChanged,
            "the system prompt still describes the deleted memory"
        );
        assert!(!dir.join("user_role.md").exists());
        let index = std::fs::read_to_string(dir.join(ENTRYPOINT_NAME)).unwrap();
        assert!(!index.contains("user_role.md"));
    }

    #[tokio::test]
    async fn rm_refuses_to_reach_outside_the_memory_directory() {
        let (temp, dir) = populated_dir();
        let outside = temp.path().join("secrets.env");
        std::fs::write(&outside, "TOKEN=1").unwrap();
        let mut harness = Harness::new(Some(dir));

        let result = harness.run("rm ../secrets.env").await;

        assert_eq!(result, CommandResult::Continue);
        assert!(outside.exists(), "traversal must not delete a file outside memory");
        assert!(harness.output.errors().contains("not inside the memory directory"));
    }

    #[tokio::test]
    async fn a_missing_file_is_reported_rather_than_silently_ignored() {
        let (_temp, dir) = populated_dir();
        let mut harness = Harness::new(Some(dir));

        harness.run("show absent.md").await;

        assert!(harness.output.errors().contains("No memory named absent.md"));
    }

    #[tokio::test]
    async fn subcommands_that_need_a_name_say_so() {
        let (_temp, dir) = populated_dir();
        let mut harness = Harness::new(Some(dir));

        harness.run("rm").await;
        harness.run("show").await;

        assert_eq!(
            harness.output.errors().matches("Which memory?").count(),
            2,
            "both subcommands need a target"
        );
    }

    #[tokio::test]
    async fn an_unknown_subcommand_shows_the_usage_line() {
        let (_temp, dir) = populated_dir();
        let mut harness = Harness::new(Some(dir));

        let result = harness.run("purge everything").await;

        assert_eq!(result, CommandResult::Continue);
        assert!(harness.output.errors().contains("Unknown /memory subcommand: purge"));
    }

    #[tokio::test]
    async fn the_command_is_inert_when_memory_is_disabled() {
        let mut harness = Harness::new(None);

        let result = harness.run("").await;

        assert_eq!(result, CommandResult::Continue);
        assert!(harness.output.errors().contains("Memory is disabled"));
        assert!(harness.output.info().is_empty());
    }
}
