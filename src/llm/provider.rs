//! Build an `llm::LLMProvider` from a stored [`LlmProviderEntry`].
//!
//! This is the seam between rowdy's persisted shape and the
//! `graniet/llm` crate's runtime. It decrypts the API key via
//! [`LlmKeyStore`], maps our [`LlmBackendKind`] tag to `llm::LLMBackend`,
//! and routes optional `base_url` overrides through the builder.
//!
//! The returned `Box<dyn LLMProvider>` is what the streaming worker calls
//! `chat_stream` / `chat_stream_with_tools` against. A fresh client is
//! built per chat turn so a settings change takes effect immediately
//! without a separate "reconnect" step.

use anyhow::{Context as _, Result};
use llm::LLMProvider;
use llm::builder::{LLMBackend, LLMBuilder};

use crate::config::LlmProviderEntry;
use crate::llm::LlmBackendKind;
use crate::llm::keystore::LlmKeyStore;

/// Map our backend tag to the upstream crate's enum. Total — every
/// variant of `LlmBackendKind` is supported by `llm` 1.3.8.
fn map_backend(kind: LlmBackendKind) -> LLMBackend {
    match kind {
        LlmBackendKind::Openai => LLMBackend::OpenAI,
        LlmBackendKind::Anthropic => LLMBackend::Anthropic,
        LlmBackendKind::Ollama => LLMBackend::Ollama,
        LlmBackendKind::Google => LLMBackend::Google,
        LlmBackendKind::Deepseek => LLMBackend::DeepSeek,
        LlmBackendKind::Openrouter => LLMBackend::OpenRouter,
    }
}

/// Construct an `LLMProvider` ready to call `chat_stream` against.
///
/// `system_prompt` is folded in via `LLMBuilder::system` so every turn
/// inherits the same role/guardrail text without us having to prepend
/// a system message manually each time.
pub fn build_client(
    entry: &LlmProviderEntry,
    keystore: &LlmKeyStore,
    system_prompt: &str,
) -> Result<Box<dyn LLMProvider>> {
    let api_key = keystore
        .lookup(entry)
        .with_context(|| format!("decrypt API key for {:?}", entry.name))?;

    let mut builder = LLMBuilder::new()
        .backend(map_backend(entry.backend))
        .api_key(api_key.as_str())
        .model(entry.model.as_str())
        .system(system_prompt);

    if let Some(base) = &entry.base_url {
        builder = builder.base_url(base.as_str());
    }

    builder
        .build()
        .with_context(|| format!("build llm client for {:?}", entry.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_backend_is_total() {
        for kind in LlmBackendKind::all() {
            // Just makes sure no variant of LlmBackendKind panics or hits an
            // unreachable arm. The actual mapping is enforced by the match
            // exhaustiveness above.
            let _ = map_backend(*kind);
        }
    }
}
