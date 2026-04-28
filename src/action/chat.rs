//! `Action::Chat(_)` dispatcher — composer input, scrolling, and the
//! submit path.
//!
//! Phase 2 ships the panel and the input pipeline; the submit path is a
//! placeholder that synthesises a "(LLM not configured)" assistant reply
//! so the loop is exercisable end-to-end without a real provider. Phase 3
//! replaces `submit` with a tokio task that streams from the `llm` crate.

use tokio::sync::oneshot;

use crate::action::{ChatAction, copy_from, cut_from, paste_into};
use crate::app::App;
use crate::chat_session;
use crate::llm::prompt::build_system_prompt;
use crate::llm::provider::build_client;
use crate::llm::tools;
use crate::llm::worker::{ChatDelta, ChatTurn, PendingChatTool, ToolReply, spawn_chat_turn};
use crate::state::chat::ChatMessage;
use crate::state::focus::Focus;
use crate::state::right_panel::RightPanelMode;
use crate::worker::{IntrospectTarget, WorkerCommand};

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
            if let Some(name) = app.active_connection.clone() {
                let path = chat_session::path_for(&app.data_dir, &name);
                if let Err(err) = chat_session::clear(&path) {
                    app.log
                        .warn("chat", format!("clear {} failed: {err}", path.display()));
                }
            }
            app.log.info("chat", "cleared session");
        }
        ChatAction::ClearComposer => {
            app.chat.composer.clear();
        }
        ChatAction::ScrollUp(n) => app.chat.scroll_up(n),
        ChatAction::ScrollDown(n) => app.chat.scroll_down(n),
        ChatAction::ScrollToTop => app.chat.scroll_to_top(),
        ChatAction::ScrollToBottom => app.chat.scroll_to_bottom(),
    }
}

/// Persist `msg` to the active connection's chat-session file. No-op
/// when no connection is active (the LLM is still callable in that mode
/// for ad-hoc questions, but we don't have anywhere reasonable to store
/// the transcript). Failures are logged and swallowed — a flaky disk
/// shouldn't break the conversation.
fn persist_message(app: &mut App, msg: &ChatMessage) {
    let Some(name) = app.active_connection.as_deref() else {
        return;
    };
    let path = chat_session::path_for(&app.data_dir, name);
    if let Err(err) = chat_session::append(&path, msg) {
        app.log
            .warn("chat", format!("append {} failed: {err}", path.display()));
    }
}

/// Persist the chat panel's most recent message — i.e. the assistant
/// turn we just streamed to completion. Pulled out because `on_delta`'s
/// `Done` and `Error` branches both need it.
fn persist_last_message(app: &mut App) {
    let last = match app.chat.messages.last() {
        Some(m) => m.clone(),
        None => return,
    };
    persist_message(app, &last);
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
    let user_msg = ChatMessage::user_text(text);
    persist_message(app, &user_msg);
    app.chat.push_message(user_msg);
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
    // detaches it. We don't need to track it yet; cancellation
    // (`Action::Chat(Cancel)`) will plumb it through `app.chat` later.
    let _handle = spawn_chat_turn(ChatTurn {
        client,
        history,
        evt_tx,
    });
    app.chat.streaming = true;
}

/// Handle a tool-execution request from the worker. Paints a tool-call
/// block into the live chat panel, then either:
/// - replies synchronously after running the tool against `app` (buffer
///   tools, schema tools whose target is already cached), or
/// - dispatches a `WorkerCommand::Introspect` and queues the tool in
///   `app.pending_chat_tools` for completion when the corresponding
///   `WorkerEvent::SchemaLoaded`/`SchemaFailed` arrives. This is what
///   makes "describe a table that hasn't been expanded" Just Work — the
///   model never has to ask the user to click around.
pub fn on_tool_request(
    app: &mut App,
    call_id: String,
    name: String,
    args_json: String,
    reply: oneshot::Sender<ToolReply>,
) {
    app.chat
        .append_tool_call(call_id.clone(), name.clone(), args_json.clone());

    if tools::is_schema_tool(&name)
        && let Some(target) = tools::target_for(&name, &args_json)
    {
        let cached = match app.schema_cache.read() {
            Ok(guard) => tools::is_cached(&guard, &target),
            Err(_) => false,
        };
        if !cached && app.active_connection.is_some() {
            queue_tool_for_introspect(
                app,
                PendingChatTool {
                    target,
                    call_id,
                    tool_name: name,
                    args_json,
                    reply,
                },
            );
            return;
        }
    }

    finalize_tool(app, &call_id, &name, &args_json, reply);
}

/// Run the tool synchronously and reply on the oneshot. Shared by the
/// initial sync path and the post-introspection retry.
fn finalize_tool(
    app: &mut App,
    call_id: &str,
    name: &str,
    args_json: &str,
    reply: oneshot::Sender<ToolReply>,
) {
    let result = tools::dispatch(app, name, args_json);
    let error = result
        .as_object()
        .and_then(|m| m.get("error"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let display = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());

    app.chat.append_tool_result(
        call_id.to_string(),
        name.to_string(),
        display.clone(),
        error,
    );

    // Receiver might have been dropped (worker aborted) — log but ignore.
    let _ = reply.send(ToolReply { result, display });
}

fn queue_tool_for_introspect(app: &mut App, pending: PendingChatTool) {
    let target = pending.target.clone();
    app.pending_chat_tools.push(pending);
    if let Err(err) = app.cmd_tx.send(WorkerCommand::Introspect {
        target: target.clone(),
    }) {
        // Worker channel is gone — drain immediately so we don't hang
        // the LLM stream waiting for a reply that will never come.
        app.log
            .warn("chat", format!("introspect dispatch failed: {err}"));
        complete_pending_for_target(
            app,
            &target,
            Some("worker unavailable for schema lookup".into()),
        );
    }
}

/// Drain pending tools whose introspection target just resolved. Each
/// match runs the tool against the (now-populated on success, or
/// untouched on failure) cache and replies on its oneshot. Called by
/// `action::on_schema_loaded` and `action::on_schema_failed`. On
/// failure the tool's existing empty-with-`note` shape is what the LLM
/// sees — preserving the v1 behaviour for tools that can't be auto-loaded.
pub fn complete_pending_for_target(
    app: &mut App,
    target: &IntrospectTarget,
    _error: Option<String>,
) {
    let mut still_waiting: Vec<PendingChatTool> = Vec::new();
    let pending = std::mem::take(&mut app.pending_chat_tools);
    for item in pending {
        if &item.target != target {
            still_waiting.push(item);
            continue;
        }

        let call_id = item.call_id.clone();
        let tool_name = item.tool_name.clone();
        let args_json = item.args_json.clone();
        finalize_tool(app, &call_id, &tool_name, &args_json, item.reply);
    }
    app.pending_chat_tools = still_waiting;
}

/// Fold a streaming delta into the chat panel. Called from
/// `action::apply_worker_event` when a `WorkerEvent::ChatDelta` lands.
pub fn on_delta(app: &mut App, delta: ChatDelta) {
    match delta {
        ChatDelta::Text(text) => {
            app.chat.append_assistant_text(&text);
        }
        ChatDelta::Done { full_text } => {
            // The Text deltas already covered the live UI; `full_text`
            // is the same content but in one piece — handy if a future
            // phase wants to splice it elsewhere. The persisted form
            // comes from `app.chat.messages.last()` so tool-call /
            // tool-result blocks are kept too.
            let _ = full_text;
            app.chat.streaming = false;
            persist_last_message(app);
        }
        ChatDelta::Error(msg) => {
            app.chat.streaming = false;
            app.chat.error = Some(msg.clone());
            let err_msg = ChatMessage::assistant_text(format!("error: {msg}"));
            persist_message(app, &err_msg);
            app.chat.push_message(err_msg);
        }
    }
}
