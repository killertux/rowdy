//! `Action::Chat(_)` dispatcher — composer input, scrolling, and the
//! submit path.
//!
//! Phase 2 ships the panel and the input pipeline; the submit path is a
//! placeholder that synthesises a "(LLM not configured)" assistant reply
//! so the loop is exercisable end-to-end without a real provider. Phase 3
//! replaces `submit` with a tokio task that streams from the `llm` crate.

use crate::action::{ChatAction, copy_from, cut_from, paste_into};
use crate::app::App;
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
    app.chat.error = None;
    app.chat.push_message(ChatMessage::user_text(text));
    app.chat.reset_composer();
    // Phase 2 stub — remove once the LLM worker is wired in phase 3.
    app.chat.push_message(ChatMessage::assistant_text(
        "(LLM not configured yet — `:chat settings` lands in phase 3)",
    ));
}
