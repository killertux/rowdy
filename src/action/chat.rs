//! `Action::Chat(_)` dispatcher — composer input, scrolling, and the
//! submit path.
//!
//! Phase 2 ships the panel and the input pipeline; the submit path is a
//! placeholder that synthesises a "(LLM not configured)" assistant reply
//! so the loop is exercisable end-to-end without a real provider. Phase 3
//! replaces `submit` with a tokio task that streams from the `llm` crate.

use crate::action::{ChatAction, copy_from, cut_from, paste_into};
use crate::app::App;
use crate::llm::prompt::build_system_prompt;
use crate::llm::provider::build_client;
use crate::llm::worker::{ChatDelta, spawn_chat_turn};
use crate::state::chat::ChatMessage;
use crate::state::focus::Focus;
use crate::state::right_panel::RightPanelMode;

pub fn apply(app: &mut App, action: ChatAction) {
    match action {
        ChatAction::Input(input) => {
            let _ = app.chat.composer.input(input);
        }
        ChatAction::Paste(text) => paste_into(&mut app.chat.composer, &app.log, text),
        ChatAction::Copy => copy_from(&mut app.chat.composer, &app.log),
        ChatAction::Cut => cut_from(&mut app.chat.composer, &app.log),
        ChatAction::Submit => submit(app),
        ChatAction::Cancel => {
            // Phase 3+: signals the worker task to abort. Until then, the
            // streaming flag should never be true outside of a turn so this
            // is effectively a no-op.
            app.chat.streaming = false;
        }
        ChatAction::Clear => {
            app.chat.clear();
            app.log.info("chat", "cleared session");
        }
        ChatAction::ClearComposer => {
            app.chat.composer.clear();
        }
        ChatAction::ScrollUp(n) => app.chat.scroll_up(n),
        ChatAction::ScrollDown(n) => app.chat.scroll_down(n),
    }
}

/// Toggle the right panel between schema and chat. Side-effect: focus
/// follows the panel so the user lands in the new pane.
pub fn toggle_right_panel(app: &mut App) {
    app.right_panel = app.right_panel.toggle();
    app.focus = match app.right_panel {
        RightPanelMode::Schema => Focus::Schema,
        RightPanelMode::Chat => Focus::Chat,
    };
    app.log
        .info("chat", format!("right panel -> {:?}", app.right_panel));
}

fn submit(app: &mut App) {
    let text = app.chat.composer_text();
    if text.is_empty() {
        return;
    }
    if app.chat.streaming {
        // A turn is already in flight — queue is not implemented yet, so
        // tell the user instead of silently dropping the new message.
        app.chat.error = Some("a response is still streaming — wait or :chat clear".into());
        return;
    }
    app.chat.error = None;
    app.chat.push_message(ChatMessage::user_text(text));
    app.chat.reset_composer();

    // Resolve provider + key. Errors land as a user-visible assistant
    // bubble rather than a status-bar message — feels more natural for
    // a chat-style UI.
    let Some(entry) = app.config.llm_providers().first().cloned() else {
        app.chat.push_message(ChatMessage::assistant_text(
            "No LLM provider configured. Run `:chat settings` to add one.",
        ));
        return;
    };
    let Some(keystore) = app.llm_keystore.as_ref() else {
        app.chat.error = Some("internal: no keystore unlocked".into());
        return;
    };

    let system_prompt = build_system_prompt(app);
    let client = match build_client(&entry, keystore, &system_prompt) {
        Ok(c) => c,
        Err(err) => {
            app.chat
                .push_message(ChatMessage::assistant_text(format!("Build error: {err}")));
            return;
        }
    };

    let history = app.chat.messages.clone();
    let evt_tx = app.evt_tx.clone();
    // Dropping the JoinHandle here doesn't cancel the task — tokio
    // detaches it. We don't need to track it yet; phase-3 cancellation
    // (`Action::Chat(Cancel)`) will plumb it through `app.chat` later.
    let _handle = spawn_chat_turn(client, history, evt_tx);
    app.chat.streaming = true;
}

/// Fold a streaming delta into the chat panel. Called from
/// `action::apply_worker_event` when a `WorkerEvent::ChatDelta` lands.
pub fn on_delta(app: &mut App, delta: ChatDelta) {
    match delta {
        ChatDelta::Text(text) => {
            app.chat.append_assistant_text(&text);
        }
        ChatDelta::Done { full_text } => {
            // The Text deltas already covered the live UI; `full_text` is
            // available for phase-5 session persistence. Drop it here for
            // now so we don't leak the API response into a log.
            let _ = full_text;
            app.chat.streaming = false;
        }
        ChatDelta::Error(msg) => {
            app.chat.streaming = false;
            app.chat.error = Some(msg.clone());
            app.chat
                .push_message(ChatMessage::assistant_text(format!("error: {msg}")));
        }
    }
}
