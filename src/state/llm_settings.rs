//! State for the `:chat settings` modal — pick provider + model + key.
//!
//! Pure data; the action layer does all the I/O. Patterned after
//! `state/conn_form.rs` (two `TextArea` fields with Tab-cycle focus),
//! extended with a discrete "backend" choice the user cycles through
//! left/right arrows or `[`/`]`.

use ratatui::style::Style;
use ratatui_textarea::TextArea;

use crate::config::LlmProviderEntry;
use crate::llm::LlmBackendKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmSettingsField {
    Backend,
    Model,
    BaseUrl,
    ApiKey,
}

impl LlmSettingsField {
    pub fn next(self) -> Self {
        match self {
            Self::Backend => Self::Model,
            Self::Model => Self::BaseUrl,
            Self::BaseUrl => Self::ApiKey,
            Self::ApiKey => Self::Backend,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            Self::Backend => Self::ApiKey,
            Self::Model => Self::Backend,
            Self::BaseUrl => Self::Model,
            Self::ApiKey => Self::BaseUrl,
        }
    }
}

#[derive(Debug)]
pub struct LlmSettingsState {
    pub backend: LlmBackendKind,
    pub model: TextArea<'static>,
    pub base_url: TextArea<'static>,
    pub api_key: TextArea<'static>,
    pub focus: LlmSettingsField,
    pub error: Option<String>,
    /// Original entry name when editing, so submit can detect rename and
    /// preserve the AAD-bound ciphertext semantics.
    pub original_name: Option<String>,
}

impl LlmSettingsState {
    /// Fresh form, defaulting to OpenAI / gpt-4.1-mini. Suitable when no
    /// provider has been configured yet.
    pub fn new_create() -> Self {
        Self {
            backend: LlmBackendKind::Openai,
            model: build_field("e.g. gpt-4.1-mini, claude-sonnet-4-5, llama3"),
            base_url: build_field("optional · used by OpenRouter or self-hosted endpoints"),
            api_key: masked_field("paste your API key"),
            focus: LlmSettingsField::Backend,
            error: None,
            original_name: None,
        }
    }

    /// Pre-fill from an existing entry. The cleartext key isn't carried
    /// here — the user re-pastes if they want to update it; an empty
    /// `api_key` field on submit means "leave the stored value as-is".
    pub fn editing(entry: &LlmProviderEntry) -> Self {
        let mut state = Self::new_create();
        state.backend = entry.backend;
        state.model = seeded(&entry.model, "e.g. gpt-4.1-mini, claude-sonnet-4-5");
        state.base_url = seeded(
            entry.base_url.as_deref().unwrap_or(""),
            "optional · used by OpenRouter or self-hosted endpoints",
        );
        // Leave api_key blank by design — keying twice is the price of
        // not having to store the cleartext in form state.
        state.original_name = Some(entry.name.clone());
        state
    }

    /// Mutable handle on whichever TextArea is currently focused. Backend
    /// and the form-wide focus aren't TextAreas, so that arm panics — the
    /// caller is responsible for filtering `Backend` before calling this.
    pub fn current_input_mut(&mut self) -> &mut TextArea<'static> {
        match self.focus {
            LlmSettingsField::Model => &mut self.model,
            LlmSettingsField::BaseUrl => &mut self.base_url,
            LlmSettingsField::ApiKey => &mut self.api_key,
            LlmSettingsField::Backend => {
                panic!("current_input_mut called while focus == Backend")
            }
        }
    }

    pub fn cycle_backend(&mut self, delta: i32) {
        let kinds = LlmBackendKind::all();
        let idx = kinds.iter().position(|k| *k == self.backend).unwrap_or(0);
        let len = kinds.len() as i32;
        let next = ((idx as i32 + delta).rem_euclid(len)) as usize;
        self.backend = kinds[next];
    }

    pub fn model_value(&self) -> String {
        self.model
            .lines()
            .first()
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    pub fn base_url_value(&self) -> Option<String> {
        let s = self
            .base_url
            .lines()
            .first()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if s.is_empty() { None } else { Some(s) }
    }

    pub fn api_key_value(&self) -> String {
        self.api_key
            .lines()
            .first()
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Entry name we'll use for upserting. We key by backend tag so the
    /// user has at most one record per provider; switching providers is
    /// a separate save, not an overwrite.
    pub fn entry_name(&self) -> String {
        self.backend.as_str().to_string()
    }
}

fn build_field(placeholder: &'static str) -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text(placeholder);
    input.set_cursor_line_style(Style::default());
    input
}

fn masked_field(placeholder: &'static str) -> TextArea<'static> {
    let mut input = build_field(placeholder);
    input.set_mask_char('•');
    input
}

fn seeded(value: &str, placeholder: &'static str) -> TextArea<'static> {
    let mut input = if value.is_empty() {
        TextArea::default()
    } else {
        TextArea::new(vec![value.to_string()])
    };
    input.set_placeholder_text(placeholder);
    input.set_cursor_line_style(Style::default());
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_backend_wraps() {
        let mut state = LlmSettingsState::new_create();
        let initial = state.backend;
        for _ in 0..LlmBackendKind::all().len() {
            state.cycle_backend(1);
        }
        assert_eq!(state.backend, initial);
    }

    #[test]
    fn cycle_backend_negative_wraps() {
        let mut state = LlmSettingsState::new_create();
        state.cycle_backend(-1);
        assert_eq!(state.backend, *LlmBackendKind::all().last().unwrap());
    }

    #[test]
    fn focus_next_prev_round_trip() {
        let f = LlmSettingsField::Backend;
        assert_eq!(f.next().prev(), f);
        assert_eq!(f.prev().next(), f);
    }

    #[test]
    fn editing_seeds_existing_values_but_blanks_key() {
        let entry = LlmProviderEntry {
            name: "openai".into(),
            backend: LlmBackendKind::Openai,
            model: "gpt-4.1".into(),
            base_url: Some("https://example.com".into()),
            api_key: Some("sk-LIVE".into()),
            nonce: None,
            ciphertext: None,
        };
        let state = LlmSettingsState::editing(&entry);
        assert_eq!(state.backend, LlmBackendKind::Openai);
        assert_eq!(state.model_value(), "gpt-4.1");
        assert_eq!(
            state.base_url_value().as_deref(),
            Some("https://example.com")
        );
        assert!(state.api_key_value().is_empty());
        assert_eq!(state.original_name.as_deref(), Some("openai"));
    }
}
