use async_trait::async_trait;

/// Applies a natural-language instruction to a body of text using a small,
/// fast model.
///
/// Defined here — rather than in `agentrs-tools` — so that tools which need a
/// secondary model call (currently only `WebFetch`) can depend on the
/// capability without depending on `agentrs-providers`, which sits in the same
/// layer. The agent crate supplies the concrete implementation, mirroring how
/// [`crate::spawner::Spawner`] is wired.
#[async_trait]
pub trait TextSummarizer: Send + Sync {
    /// Run `instruction` against `content` and return the model's answer.
    ///
    /// Implementations are expected to be self-contained: callers pass content
    /// that already fits the model's context window.
    async fn summarize(&self, instruction: &str, content: &str) -> Result<String, String>;
}
