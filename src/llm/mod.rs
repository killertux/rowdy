//! LLM chat integration.
//!
//! This module owns the runtime concerns: the [`LlmBackendKind`] tag (which
//! decides which provider the `llm` crate talks to), the [`keystore`]
//! responsible for AES-encrypting API keys at rest, and — once later phases
//! land — the streaming worker, the tool dispatch, and the system prompt.
//!
//! Phase 1 only adds the persisted shape and the keystore. Builder/streaming
//! code joins later — until then, several items are technically unused, so
//! the module silences `dead_code` rather than leaking phase-3 stubs.

#![allow(dead_code)]

pub mod keystore;
pub mod prompt;
pub mod provider;
pub mod tools;
pub mod worker;

use serde::{Deserialize, Serialize};

/// The provider behind a configured LLM. Persisted as the lowercase string
/// form (`"openai"`, `"anthropic"`, …) so future reorderings don't break
/// existing config files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmBackendKind {
    Openai,
    Anthropic,
    Ollama,
    Google,
    Deepseek,
    /// OpenRouter is plumbed through the OpenAI backend with a custom base
    /// URL. We keep it as a distinct on-disk tag so the settings UI can
    /// surface "OpenRouter" rather than "OpenAI w/ base URL".
    Openrouter,
}

impl LlmBackendKind {
    /// Canonical lowercase tag used both on disk and in user-facing labels.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Google => "google",
            Self::Deepseek => "deepseek",
            Self::Openrouter => "openrouter",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "openai" => Self::Openai,
            "anthropic" => Self::Anthropic,
            "ollama" => Self::Ollama,
            "google" => Self::Google,
            "deepseek" => Self::Deepseek,
            "openrouter" => Self::Openrouter,
            _ => return None,
        })
    }

    /// Every backend the build currently supports, in display order. The
    /// settings UI iterates this for its provider picker.
    pub fn all() -> &'static [Self] {
        &[
            Self::Openai,
            Self::Anthropic,
            Self::Ollama,
            Self::Google,
            Self::Deepseek,
            Self::Openrouter,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_canonical_tags() {
        for kind in LlmBackendKind::all() {
            assert_eq!(LlmBackendKind::parse(kind.as_str()), Some(*kind));
        }
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(LlmBackendKind::parse("xai").is_none());
        assert!(LlmBackendKind::parse("OPENAI").is_none());
        assert!(LlmBackendKind::parse("").is_none());
    }

    #[test]
    fn serde_uses_lowercase_form() {
        let json = serde_json::to_string(&LlmBackendKind::Openrouter).unwrap();
        assert_eq!(json, "\"openrouter\"");
        let back: LlmBackendKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, LlmBackendKind::Openrouter);
    }
}
