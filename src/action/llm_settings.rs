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
use crate::user_config::ReadToolsMode;

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
    let mut prefill = app
        .config
        .llm_providers()
        .first()
        .map(LlmSettingsState::editing)
        .unwrap_or_else(LlmSettingsState::new_create);
    // Seed the read-tools mode from user-config so the modal reflects
    // the live value the chat gate is using.
    prefill.read_tools_mode = app.user_config.state().read_tools.unwrap_or_default();
    app.overlay = Some(Overlay::LlmSettings(prefill));
}

pub fn apply(app: &mut App, action: LlmSettingsAction) {
    // The radio row persists the moment it changes — it's a global
    // preference (lives on user-config), not part of the provider
    // entry, so making it wait for `Submit` was a trap: the user could
    // change it, hit Esc, and lose the change. We track the new mode
    // here so we can persist after dropping the &mut borrow on
    // `app.overlay`.
    let mut changed_read_tools_to: Option<ReadToolsMode> = None;
    let mut do_submit = false;
    {
        let Some(Overlay::LlmSettings(state)) = &mut app.overlay else {
            return;
        };
        match action {
            LlmSettingsAction::Input(input) => {
                // Space on the radio row advances; explicit shortcuts
                // (o/a/A) jump to a specific state. Anything else
                // routed to Backend / ReadToolsMode is dropped — no
                // TextArea to hand it to.
                if matches!(state.focus, LlmSettingsField::ReadToolsMode) {
                    if let Some(target) = read_tools_jump(&input) {
                        if state.read_tools_mode != target {
                            state.read_tools_mode = target;
                            changed_read_tools_to = Some(target);
                        }
                    } else if is_advance_input(&input) {
                        let mode = state.cycle_read_tools_mode(1);
                        changed_read_tools_to = Some(mode);
                    }
                } else if !matches!(state.focus, LlmSettingsField::Backend) {
                    let _ = state.current_input_mut().input(input);
                }
            }
            LlmSettingsAction::Paste(text) => {
                if !matches!(
                    state.focus,
                    LlmSettingsField::Backend | LlmSettingsField::ReadToolsMode
                ) {
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
            LlmSettingsAction::CycleBackend(delta) => match state.focus {
                // ←/→ on the radio row advances/reverses the mode;
                // matches how Backend treats ←/→ as "cycle this field".
                LlmSettingsField::ReadToolsMode => {
                    let mode = state.cycle_read_tools_mode(delta);
                    changed_read_tools_to = Some(mode);
                }
                _ => state.cycle_backend(delta),
            },
            LlmSettingsAction::CycleField => state.focus = state.focus.next(),
            LlmSettingsAction::CycleFieldBack => state.focus = state.focus.prev(),
            LlmSettingsAction::ClearField => {
                if !matches!(
                    state.focus,
                    LlmSettingsField::Backend | LlmSettingsField::ReadToolsMode
                ) {
                    state.current_input_mut().clear();
                }
            }
            LlmSettingsAction::Cancel => app.overlay = None,
            LlmSettingsAction::Submit => {
                if matches!(state.focus, LlmSettingsField::ReadToolsMode) {
                    let mode = state.cycle_read_tools_mode(1);
                    changed_read_tools_to = Some(mode);
                } else {
                    // Defer the actual save until after we drop the
                    // &mut borrow on `app.overlay`.
                    do_submit = true;
                }
            }
        }
    }
    if let Some(mode) = changed_read_tools_to {
        if let Err(err) = app.user_config.set_read_tools_mode(mode) {
            app.log
                .warn("llm", format!("read-tools mode persist failed: {err}"));
        } else {
            app.log
                .info("llm", format!("filesystem read tools → {}", mode.label()));
        }
    }
    if do_submit {
        submit(app);
    }
}

/// Recognise direct-jump shortcuts: `o` → Off, `a` → Ask, `A` → Auto.
/// The lowercase/uppercase asymmetry on `a` keeps the chord short
/// without colliding with normal text-field characters.
fn read_tools_jump(input: &ratatui_textarea::Input) -> Option<ReadToolsMode> {
    use ratatui_textarea::{Input, Key};
    match input {
        Input {
            key: Key::Char('o' | 'O'),
            ..
        } => Some(ReadToolsMode::Off),
        Input {
            key: Key::Char('a'),
            shift: false,
            ..
        } => Some(ReadToolsMode::Ask),
        Input {
            key: Key::Char('A'),
            ..
        }
        | Input {
            key: Key::Char('a'),
            shift: true,
            ..
        } => Some(ReadToolsMode::Auto),
        _ => None,
    }
}

/// True if `input` should advance the radio one step forward (cyclic).
/// Space is the conventional "flip a checkbox" key; we honour Enter
/// elsewhere via `Submit`, so this is just the space-bar gate.
fn is_advance_input(input: &ratatui_textarea::Input) -> bool {
    use ratatui_textarea::{Input, Key};
    matches!(
        input,
        Input {
            key: Key::Char(' '),
            ..
        }
    )
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
    // Belt-and-braces: the radio row already persists on every
    // change, but flushing again on Submit means a failed
    // intermediate write (rare, but possible) doesn't leave a
    // stale value.
    let read_tools_mode = state.read_tools_mode;
    if let Err(err) = app.user_config.set_read_tools_mode(read_tools_mode) {
        app.log
            .warn("llm", format!("read-tools mode persist failed: {err}"));
    }
    app.log
        .info("llm", format!("saved provider {entry_name} ({backend:?})"));
    app.overlay = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::SchemaCache;
    use crate::config::ConfigStore;
    use crate::keybindings::keymap::Keymap;
    use crate::log::Logger;
    use crate::user_config::UserConfigStore;
    use ratatui_textarea::{Input, Key};
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc::unbounded_channel;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-llmset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    fn build_app() -> (App, PathBuf) {
        let dir = tempdir();
        let (cmd_tx, _c) = unbounded_channel();
        let (evt_tx, _e) = unbounded_channel();
        let cache = Arc::new(RwLock::new(SchemaCache::new()));
        let app = App::new(
            cmd_tx,
            evt_tx,
            ConfigStore::load(&dir).unwrap(),
            UserConfigStore::load(&dir).unwrap(),
            Arc::new(Keymap::new()),
            None,
            Logger::discard(),
            dir.clone(),
            cache,
        );
        (app, dir)
    }

    fn focus_read_tools(state: &mut LlmSettingsState) {
        while state.focus != LlmSettingsField::ReadToolsMode {
            state.focus = state.focus.next();
        }
    }

    #[test]
    fn arrow_advances_mode_and_persists_immediately() {
        let (mut app, _dir) = build_app();
        open(&mut app);
        if let Some(Overlay::LlmSettings(state)) = &mut app.overlay {
            focus_read_tools(state);
        }
        // Default starts at Ask (the seeded state when nothing is
        // persisted). Right arrow advances to Auto.
        assert_eq!(app.user_config.state().read_tools, None);
        apply(&mut app, LlmSettingsAction::CycleBackend(1));
        assert_eq!(
            app.user_config.state().read_tools,
            Some(ReadToolsMode::Auto)
        );
        // Right arrow again wraps to Off.
        apply(&mut app, LlmSettingsAction::CycleBackend(1));
        assert_eq!(app.user_config.state().read_tools, Some(ReadToolsMode::Off));
        // Right arrow again brings us back to Ask.
        apply(&mut app, LlmSettingsAction::CycleBackend(1));
        assert_eq!(app.user_config.state().read_tools, Some(ReadToolsMode::Ask));
    }

    #[test]
    fn space_advances_mode_and_persists_immediately() {
        let (mut app, _dir) = build_app();
        open(&mut app);
        if let Some(Overlay::LlmSettings(state)) = &mut app.overlay {
            focus_read_tools(state);
        }
        apply(
            &mut app,
            LlmSettingsAction::Input(Input {
                key: Key::Char(' '),
                ctrl: false,
                alt: false,
                shift: false,
            }),
        );
        // Ask → Auto.
        assert_eq!(
            app.user_config.state().read_tools,
            Some(ReadToolsMode::Auto)
        );
    }

    #[test]
    fn jump_keys_set_specific_mode() {
        let (mut app, _dir) = build_app();
        open(&mut app);
        if let Some(Overlay::LlmSettings(state)) = &mut app.overlay {
            focus_read_tools(state);
        }
        // Jump to Off.
        apply(
            &mut app,
            LlmSettingsAction::Input(Input {
                key: Key::Char('o'),
                ctrl: false,
                alt: false,
                shift: false,
            }),
        );
        assert_eq!(app.user_config.state().read_tools, Some(ReadToolsMode::Off));
        // Jump to Auto via Shift+a / 'A'.
        apply(
            &mut app,
            LlmSettingsAction::Input(Input {
                key: Key::Char('A'),
                ctrl: false,
                alt: false,
                shift: true,
            }),
        );
        assert_eq!(
            app.user_config.state().read_tools,
            Some(ReadToolsMode::Auto)
        );
        // Lowercase 'a' jumps to Ask.
        apply(
            &mut app,
            LlmSettingsAction::Input(Input {
                key: Key::Char('a'),
                ctrl: false,
                alt: false,
                shift: false,
            }),
        );
        assert_eq!(app.user_config.state().read_tools, Some(ReadToolsMode::Ask));
    }

    #[test]
    fn enter_on_radio_row_advances_and_persists_does_not_close() {
        let (mut app, _dir) = build_app();
        open(&mut app);
        if let Some(Overlay::LlmSettings(state)) = &mut app.overlay {
            focus_read_tools(state);
        }
        apply(&mut app, LlmSettingsAction::Submit);
        assert_eq!(
            app.user_config.state().read_tools,
            Some(ReadToolsMode::Auto)
        );
        // Modal still open — Enter on the radio row advances, doesn't save+close.
        assert!(matches!(app.overlay, Some(Overlay::LlmSettings(_))));
    }

    #[test]
    fn cancel_after_change_keeps_persisted_value() {
        let (mut app, _dir) = build_app();
        open(&mut app);
        if let Some(Overlay::LlmSettings(state)) = &mut app.overlay {
            focus_read_tools(state);
        }
        apply(&mut app, LlmSettingsAction::CycleBackend(1));
        apply(&mut app, LlmSettingsAction::Cancel);
        assert!(app.overlay.is_none());
        assert_eq!(
            app.user_config.state().read_tools,
            Some(ReadToolsMode::Auto)
        );
    }

    #[test]
    fn open_seeds_state_from_persisted_value() {
        let (mut app, _dir) = build_app();
        app.user_config
            .set_read_tools_mode(ReadToolsMode::Off)
            .unwrap();
        open(&mut app);
        match &app.overlay {
            Some(Overlay::LlmSettings(state)) => {
                assert_eq!(state.read_tools_mode, ReadToolsMode::Off);
            }
            _ => panic!("expected settings overlay"),
        }
    }
}
