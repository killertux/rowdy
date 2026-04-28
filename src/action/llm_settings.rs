//! `Action::LlmSettings(_)` dispatcher — provider/model/key form.
//!
//! Submit upserts via [`crate::config::ConfigStore::upsert_llm_provider`]
//! and [`crate::llm::keystore::LlmKeyStore::make_entry`] (which encrypts
//! the API key against the shared `DerivedKey`). Edit-mode submits with
//! an empty `api_key` field preserve the existing ciphertext rather than
//! wiping the user's saved key.

use crate::action::{LlmSettingsAction, copy_from, cut_from, paste_into};
use crate::app::App;
use crate::state::llm_settings::{LlmSettingsField, LlmSettingsState};
use crate::state::overlay::Overlay;

/// Open the settings overlay, prefilled from the current default provider
/// (if any). No-op when called twice — the first one wins.
pub fn open(app: &mut App) {
    if matches!(app.overlay, Some(Overlay::LlmSettings(_))) {
        return;
    }
    // Prefill from whatever provider is keyed by the *currently selected
    // backend's tag* — phase 3 stores at most one entry per backend, so
    // checking by backend tag matches the upsert key. If there's nothing
    // saved yet, open a fresh form.
    let prefill = app
        .config
        .llm_providers()
        .first()
        .map(LlmSettingsState::editing)
        .unwrap_or_else(LlmSettingsState::new_create);
    app.overlay = Some(Overlay::LlmSettings(prefill));
}

pub fn apply(app: &mut App, action: LlmSettingsAction) {
    let Some(Overlay::LlmSettings(state)) = &mut app.overlay else {
        return;
    };
    match action {
        LlmSettingsAction::Input(input) => {
            if !matches!(state.focus, LlmSettingsField::Backend) {
                let _ = state.current_input_mut().input(input);
            }
        }
        LlmSettingsAction::Paste(text) => {
            if !matches!(state.focus, LlmSettingsField::Backend) {
                paste_into(state.current_input_mut(), &app.log, text);
            }
        }
        LlmSettingsAction::Copy => {
            // Don't copy the API key field — risks leaking it onto the
            // clipboard via an accidental Ctrl+C while focused there.
            if matches!(
                state.focus,
                LlmSettingsField::Model | LlmSettingsField::BaseUrl
            ) {
                copy_from(state.current_input_mut(), &app.log);
            }
        }
        LlmSettingsAction::Cut => {
            if matches!(
                state.focus,
                LlmSettingsField::Model | LlmSettingsField::BaseUrl
            ) {
                cut_from(state.current_input_mut(), &app.log);
            }
        }
        LlmSettingsAction::CycleBackend(delta) => state.cycle_backend(delta),
        LlmSettingsAction::CycleField => state.focus = state.focus.next(),
        LlmSettingsAction::CycleFieldBack => state.focus = state.focus.prev(),
        LlmSettingsAction::Cancel => app.overlay = None,
        LlmSettingsAction::Submit => submit(app),
    }
}

fn submit(app: &mut App) {
    let Some(Overlay::LlmSettings(state)) = &mut app.overlay else {
        return;
    };
    state.error = None;

    let model = state.model_value();
    if model.is_empty() {
        state.error = Some("model is required".into());
        return;
    }
    let base_url = state.base_url_value();
    let api_key_input = state.api_key_value();
    let backend = state.backend;
    let entry_name = state.entry_name();
    let original_name = state.original_name.clone();

    // If editing without re-entering the key, reuse the previously stored
    // ciphertext verbatim. The user saw the field blank — that's the cue.
    let preserved_entry = if api_key_input.is_empty() {
        original_name
            .as_deref()
            .and_then(|n| app.config.llm_provider(n))
            .cloned()
    } else {
        None
    };

    let Some(keystore) = app.llm_keystore.as_ref() else {
        state.error = Some("internal: no keystore unlocked".into());
        return;
    };

    let entry = match (preserved_entry, api_key_input.as_str()) {
        (Some(prev), _) => {
            // Reuse old nonce/ciphertext; just refresh the metadata.
            crate::config::LlmProviderEntry {
                name: entry_name.clone(),
                backend,
                model: model.clone(),
                base_url: base_url.clone(),
                api_key: prev.api_key.clone(),
                nonce: prev.nonce.clone(),
                ciphertext: prev.ciphertext.clone(),
            }
        }
        (None, "") => {
            state.error = Some("api key is required".into());
            return;
        }
        (None, key) => match keystore.make_entry(
            entry_name.clone(),
            backend,
            model.clone(),
            base_url.clone(),
            key,
        ) {
            Ok(entry) => entry,
            Err(err) => {
                state.error = Some(format!("encrypt failed: {err}"));
                return;
            }
        },
    };

    if let Err(err) = app.config.upsert_llm_provider(entry) {
        state.error = Some(format!("save failed: {err}"));
        return;
    }
    app.log
        .info("llm", format!("saved provider {entry_name} ({backend:?})"));
    app.overlay = None;
}
