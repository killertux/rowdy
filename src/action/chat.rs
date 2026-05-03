//! `Action::Chat(_)` dispatcher — composer input, scrolling, and the
//! submit path.
//!
//! Phase 2 ships the panel and the input pipeline; the submit path is a
//! placeholder that synthesises a "(LLM not configured)" assistant reply
//! so the loop is exercisable end-to-end without a real provider. Phase 3
//! replaces `submit` with a tokio task that streams from the `llm` crate.

use std::sync::Arc;

use tokio::sync::oneshot;

use crate::action::{ChatAction, copy_from, cut_from, paste_into};
use crate::app::App;
use crate::chat_session;
use crate::llm::prompt::build_system_prompt;
use crate::llm::provider::build_client;
use crate::llm::tools;
use crate::llm::worker::{
    ChatDelta, ChatTurn, PendingApprovalTool, PendingChatTool, ToolReply, spawn_chat_turn,
};
use crate::state::chat::ChatMessage;
use crate::state::focus::Focus;
use crate::state::overlay::Overlay;
use crate::state::right_panel::RightPanelMode;
use crate::user_config::ReadToolsMode;
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
/// follows the panel so the user lands in the new pane. Chat lands in
/// *normal* mode (`Focus::Chat`) — the user picks `i` to start typing.
pub fn toggle_right_panel(app: &mut App) {
    set_right_panel(app, app.right_panel.toggle());
}

/// Force the right panel to a specific mode and follow with focus. Used
/// by the leader-chord bindings (`<leader> S` / `<leader> C`) and by
/// `toggle_right_panel`. Idempotent — calling with the current mode just
/// re-asserts focus.
pub fn set_right_panel(app: &mut App, mode: RightPanelMode) {
    app.right_panel = mode;
    app.focus = match mode {
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
    // Snapshot the tool list at submit time so a mid-turn settings
    // change doesn't shift the catalog the model is reasoning against
    // (the gate in `on_tool_request` reads the *current* mode at call
    // time, which is the right place to react to changes).
    let tools = tools::for_mode(read_tools_mode(app));
    // Dropping the JoinHandle here doesn't cancel the task — tokio
    // detaches it. We don't need to track it yet; cancellation
    // (`Action::Chat(Cancel)`) will plumb it through `app.chat` later.
    let _handle = spawn_chat_turn(ChatTurn {
        client,
        history,
        evt_tx,
        tools,
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

    if tools::is_fs_read_tool(&name) {
        match read_tools_mode(app) {
            ReadToolsMode::Off => {
                // Defense-in-depth — the tool list passed to the LLM
                // omits these in Off mode (see `submit`), so a model
                // shouldn't even know they exist. If one slips through
                // (legacy chat history, model hallucinated the name),
                // refuse with a reason it can use to redirect.
                let result = serde_json::json!({
                    "error": "filesystem read tools are disabled — ask the user to enable them in :chat settings"
                });
                let display = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
                app.chat.append_tool_result(
                    call_id,
                    name,
                    display.clone(),
                    Some("disabled".into()),
                );
                let _ = reply.send(ToolReply { result, display });
                return;
            }
            ReadToolsMode::Ask => {
                queue_tool_for_approval(app, call_id, name, args_json, reply);
                return;
            }
            ReadToolsMode::Auto => {} // fall through to finalize_tool
        }
    }

    finalize_tool(app, &call_id, &name, &args_json, reply);
}

/// Effective read-tools mode. `None` in user-config resolves to
/// [`ReadToolsMode::Ask`] so a fresh install gets a conservative gate.
pub(crate) fn read_tools_mode(app: &App) -> ReadToolsMode {
    app.user_config.state().read_tools.unwrap_or_default()
}

/// Park a fs-read tool call on the approval queue and pop the
/// confirmation overlay. Mirrors `queue_tool_for_introspect` shape, but
/// the wakeup signal is the user pressing y/n instead of a worker event.
///
/// If another overlay is already up (the user opened `:` mid-stream, or
/// `:help`, etc.) we queue the approval without stealing their input —
/// `try_promote_pending_tool_confirm` promotes the prompt once the
/// user-driven overlay closes.
fn queue_tool_for_approval(
    app: &mut App,
    call_id: String,
    name: String,
    args_json: String,
    reply: oneshot::Sender<ToolReply>,
) {
    app.pending_approval_tools.push(PendingApprovalTool {
        call_id: call_id.clone(),
        tool_name: name.clone(),
        args_json: args_json.clone(),
        reply,
    });
    if app.overlay.is_none() {
        present_approval_overlay(app, call_id, name, args_json);
    }
    // else: prompt sits in the queue until the user dismisses their
    // current overlay; `try_promote_pending_tool_confirm` (called once
    // per loop tick from main) does the handoff.
}

/// Move the next pending approval onto the live overlay if nothing else
/// is preempting input. Idempotent and cheap. Safe to call from the run
/// loop on every iteration (mirrors `try_promote_pending_update`).
pub fn try_promote_pending_tool_confirm(app: &mut App) {
    if app.pending_approval_tools.is_empty() {
        return;
    }
    if app.overlay.is_some() {
        return;
    }
    let pending = match app.pending_approval_tools.first() {
        Some(p) => p,
        None => return,
    };
    let call_id = pending.call_id.clone();
    let name = pending.tool_name.clone();
    let args_json = pending.args_json.clone();
    present_approval_overlay(app, call_id, name, args_json);
}

/// Pop the approval overlay AND auto-demote focus out of the chat
/// composer if that's where the user was. The composer's TextArea
/// otherwise still looks live (cursor blinking, mode indicator on),
/// which makes it tempting to type into instead of pressing y/n.
/// Snapshot the prior focus on first demote so back-to-back prompts
/// don't keep clobbering it; restore happens in `clear_approval_overlay`
/// once the queue empties.
fn present_approval_overlay(app: &mut App, call_id: String, name: String, args_json: String) {
    if matches!(app.focus, Focus::ChatComposer) && app.focus_before_approval.is_none() {
        app.focus_before_approval = Some(app.focus);
        app.focus = Focus::Chat;
    }
    app.overlay = Some(Overlay::ConfirmToolUse {
        call_id,
        name,
        args_json,
    });
}

/// Drop the approval overlay and, if we just resolved the last pending
/// approval, restore whatever focus we demoted out of when the first
/// prompt popped. If more approvals are queued (rare — the worker
/// awaits each oneshot — but possible if a future change parallelises
/// tool calls), the focus snapshot stays put for the next pop.
fn clear_approval_overlay(app: &mut App) {
    app.overlay = None;
    if app.pending_approval_tools.is_empty()
        && let Some(prev) = app.focus_before_approval.take()
    {
        app.focus = prev;
    }
}

/// Approve handler: drain the matching pending tool and run it through
/// the regular dispatch path. Idempotent against a stale overlay (e.g.
/// the user smashed y twice) — the second call finds an empty queue
/// and just clears the overlay.
pub fn on_tool_approve_accept(app: &mut App) {
    let call_id = match app.overlay.as_ref() {
        Some(Overlay::ConfirmToolUse { call_id, .. }) => call_id.clone(),
        _ => return,
    };
    let Some(idx) = app
        .pending_approval_tools
        .iter()
        .position(|p| p.call_id == call_id)
    else {
        // Overlay was up but the pending entry already drained — clear
        // the stale overlay and bail.
        clear_approval_overlay(app);
        return;
    };
    let pending = app.pending_approval_tools.remove(idx);
    clear_approval_overlay(app);
    finalize_tool(
        app,
        &pending.call_id,
        &pending.tool_name,
        &pending.args_json,
        pending.reply,
    );
}

/// Deny handler: refuse the tool with a JSON shape the LLM can read,
/// then keep the turn going. The model will either move on or ask the
/// user, neither of which leaks anything we didn't already authorize.
pub fn on_tool_approve_deny(app: &mut App) {
    let (call_id, name) = match app.overlay.as_ref() {
        Some(Overlay::ConfirmToolUse { call_id, name, .. }) => (call_id.clone(), name.clone()),
        _ => return,
    };
    let Some(idx) = app
        .pending_approval_tools
        .iter()
        .position(|p| p.call_id == call_id)
    else {
        clear_approval_overlay(app);
        return;
    };
    let pending = app.pending_approval_tools.remove(idx);
    clear_approval_overlay(app);
    let result = serde_json::json!({
        "error": format!("user denied access — do not retry {name}; ask the user what they want instead")
    });
    let display = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
    app.chat.append_tool_result(
        pending.call_id,
        name,
        display.clone(),
        Some("denied".into()),
    );
    let _ = pending.reply.send(ToolReply { result, display });
}

/// Run the tool and reply on the oneshot. Schema + buffer tools run
/// inline on the main loop because they touch in-memory state and
/// finish in microseconds; **filesystem read tools** are routed to
/// `tokio::task::spawn_blocking` so the regex/IO work doesn't freeze
/// the UI. Shared by the initial sync path and the post-introspection
/// retry.
fn finalize_tool(
    app: &mut App,
    call_id: &str,
    name: &str,
    args_json: &str,
    reply: oneshot::Sender<ToolReply>,
) {
    if tools::is_fs_read_tool(name) {
        spawn_fs_tool(app, call_id, name, args_json, reply);
        return;
    }

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

/// Off-thread fs-tool execution. Snapshots the project root and tool
/// args, hands them to `spawn_blocking`, and emits a
/// [`WorkerEvent::ChatFsToolDone`] so the main loop can paint the
/// tool-result block without holding state across a blocking call.
/// The spawned task replies on the LLM oneshot directly — that channel
/// is `Send`, so no main-loop bounce is needed for the worker side.
fn spawn_fs_tool(
    app: &App,
    call_id: &str,
    name: &str,
    args_json: &str,
    reply: oneshot::Sender<ToolReply>,
) {
    let project_root = app.project_root.clone();
    let evt_tx = app.evt_tx.clone();
    let agents_md = Arc::clone(&app.agents_md);
    let log = app.log.clone();
    let call_id = call_id.to_string();
    let name = name.to_string();
    let args_json = args_json.to_string();

    tokio::task::spawn_blocking(move || {
        let result = tools::dispatch_fs(&project_root, &name, &args_json);
        let error = result
            .as_object()
            .and_then(|m| m.get("error"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let display = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());

        // Lazy AGENTS.md discovery: walk the touched directory's
        // chain up to project_root and load any AGENTS.md we
        // haven't seen. Only runs when the resolved target was
        // valid (out-of-jail / missing-path resolves to None and
        // the cache stays untouched). The freshly-loaded paths ride
        // the ChatFsToolDone event back to the main loop, which
        // surfaces a per-load chat notice.
        let agents_md_loaded =
            if let Some(target_dir) = tools::fs_target_dir(&project_root, &name, &args_json) {
                match agents_md.write() {
                    Ok(mut cache) => cache.discover_for(&project_root, &target_dir, &log),
                    Err(_) => Vec::new(),
                }
            } else {
                Vec::new()
            };

        // Update the UI through the main loop — `app.chat` isn't safe
        // to touch from a blocking task. The event handler in
        // `on_fs_tool_done` mirrors the inline `append_tool_result`
        // call above.
        let _ = evt_tx.send(crate::worker::WorkerEvent::ChatFsToolDone {
            call_id: call_id.clone(),
            name: name.clone(),
            display: display.clone(),
            error,
            agents_md_loaded,
        });

        // Reply to the LLM worker. Receiver might be gone (turn was
        // aborted) — that's the same "log but ignore" case as the sync
        // path; we have no logger here, so we just drop the error.
        let _ = reply.send(ToolReply { result, display });
    });
}

/// Main-loop handler for [`WorkerEvent::ChatFsToolDone`]. Appends the
/// tool-result block onto the chat panel — same shape `finalize_tool`
/// emits for the synchronous tools — and surfaces a small system
/// notice for any AGENTS.md the fs tool's directory walk loaded.
/// Notices are pushed before the tool result so they land
/// chronologically just above the read that triggered them.
pub fn on_fs_tool_done(
    app: &mut App,
    call_id: String,
    name: String,
    display: String,
    error: Option<String>,
    agents_md_loaded: Vec<String>,
) {
    for path in &agents_md_loaded {
        app.chat.push_message(ChatMessage::system_text(format!(
            "Loaded AGENTS.md ({path})"
        )));
    }
    app.chat.append_tool_result(call_id, name, display, error);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autocomplete::SchemaCache;
    use crate::config::ConfigStore;
    use crate::keybindings::keymap::Keymap;
    use crate::log::Logger;
    use crate::state::overlay::Overlay;
    use crate::user_config::UserConfigStore;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc::unbounded_channel;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-chat-{}", uuid::Uuid::new_v4()));
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
            UserConfigStore::empty(&dir),
            Arc::new(Keymap::new()),
            None,
            Logger::discard(),
            dir.clone(),
            cache,
        );
        (app, dir)
    }

    #[test]
    fn queue_tool_for_approval_pops_overlay_when_no_other_overlay() {
        let (mut app, _dir) = build_app();
        let (tx, _rx) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"src/lib.rs"}"#.into(),
            tx,
        );
        assert!(matches!(app.overlay, Some(Overlay::ConfirmToolUse { .. })));
        assert_eq!(app.pending_approval_tools.len(), 1);
    }

    #[test]
    fn queue_tool_for_approval_does_not_steal_existing_overlay() {
        let (mut app, _dir) = build_app();
        app.overlay = Some(Overlay::Help {
            scroll: 0,
            h_scroll: 0,
        });
        let (tx, _rx) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"x"}"#.into(),
            tx,
        );
        assert!(matches!(app.overlay, Some(Overlay::Help { .. })));
        assert_eq!(app.pending_approval_tools.len(), 1);
    }

    #[test]
    fn try_promote_pending_tool_confirm_swaps_in_when_overlay_clears() {
        let (mut app, _dir) = build_app();
        app.overlay = Some(Overlay::Help {
            scroll: 0,
            h_scroll: 0,
        });
        let (tx, _rx) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::GREP_FILES.into(),
            r#"{"pattern":"x"}"#.into(),
            tx,
        );
        // User closes their help popover; the next tick should promote.
        app.overlay = None;
        try_promote_pending_tool_confirm(&mut app);
        assert!(matches!(
            app.overlay,
            Some(Overlay::ConfirmToolUse { ref name, .. }) if name == tools::GREP_FILES
        ));
    }

    #[test]
    fn read_tools_mode_default_is_ask() {
        let (app, _dir) = build_app();
        assert_eq!(read_tools_mode(&app), ReadToolsMode::Ask);
    }

    #[test]
    fn read_tools_mode_reads_from_user_config() {
        for mode in [ReadToolsMode::Off, ReadToolsMode::Ask, ReadToolsMode::Auto] {
            let (mut app, _dir) = build_app();
            app.user_config.set_read_tools_mode(mode).unwrap();
            assert_eq!(read_tools_mode(&app), mode);
        }
    }

    #[tokio::test]
    async fn off_mode_refuses_without_prompting() {
        let (mut app, dir) = build_app();
        std::fs::write(dir.join("hi.txt"), "x").unwrap();
        app.project_root = dir.clone();
        app.user_config
            .set_read_tools_mode(ReadToolsMode::Off)
            .unwrap();
        let (tx, rx) = oneshot::channel();
        on_tool_request(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"hi.txt"}"#.into(),
            tx,
        );
        // No overlay (no prompt), no pending entry — straight refusal.
        assert!(app.overlay.is_none());
        assert!(app.pending_approval_tools.is_empty());
        let reply = rx.await.unwrap();
        let err = reply
            .result
            .get("error")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        assert!(err.contains("disabled"), "got: {err}");
    }

    #[tokio::test]
    async fn auto_mode_runs_tool_without_prompting() {
        let (mut app, dir) = build_app();
        std::fs::write(dir.join("hi.txt"), "ok").unwrap();
        app.project_root = dir.clone();
        app.user_config
            .set_read_tools_mode(ReadToolsMode::Auto)
            .unwrap();
        let (tx, rx) = oneshot::channel();
        on_tool_request(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"hi.txt"}"#.into(),
            tx,
        );
        // Auto mode → spawn_blocking handles it; no overlay surfaces.
        assert!(app.overlay.is_none());
        assert!(app.pending_approval_tools.is_empty());
        let reply = rx.await.unwrap();
        assert_eq!(
            reply.result.get("text").and_then(|s| s.as_str()),
            Some("ok")
        );
    }

    #[tokio::test]
    async fn approve_runs_tool_and_clears_overlay() {
        let (mut app, dir) = build_app();
        std::fs::write(dir.join("hello.txt"), "hi\nthere").unwrap();
        app.project_root = dir.clone();
        let (tx, rx) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"hello.txt"}"#.into(),
            tx,
        );
        on_tool_approve_accept(&mut app);
        assert!(app.overlay.is_none());
        assert!(app.pending_approval_tools.is_empty());
        let reply = rx.await.unwrap();
        assert_eq!(
            reply.result.get("text").and_then(|s| s.as_str()),
            Some("hi\nthere")
        );
    }

    #[tokio::test]
    async fn deny_replies_with_refusal_and_clears_overlay() {
        let (mut app, _dir) = build_app();
        let (tx, rx) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":".env"}"#.into(),
            tx,
        );
        on_tool_approve_deny(&mut app);
        assert!(app.overlay.is_none());
        assert!(app.pending_approval_tools.is_empty());
        let reply = rx.await.unwrap();
        let err = reply
            .result
            .get("error")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        assert!(err.contains("denied"), "got: {err}");
    }

    #[tokio::test]
    async fn approval_demotes_chat_composer_focus_and_restores_after() {
        let (mut app, dir) = build_app();
        std::fs::write(dir.join("hello.txt"), "hi").unwrap();
        app.project_root = dir.clone();
        // Simulate the user being mid-typing in the chat composer.
        app.focus = Focus::ChatComposer;
        let (tx, rx) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"hello.txt"}"#.into(),
            tx,
        );
        // Prompt popped → focus demoted to Chat normal mode so y/n is
        // unambiguous instead of being eaten by the textarea visually.
        assert_eq!(app.focus, Focus::Chat);
        assert_eq!(app.focus_before_approval, Some(Focus::ChatComposer));

        on_tool_approve_accept(&mut app);
        let _ = rx.await;
        // Last pending approval cleared → focus restored to where the
        // user was when the prompt fired.
        assert_eq!(app.focus, Focus::ChatComposer);
        assert!(app.focus_before_approval.is_none());
    }

    #[tokio::test]
    async fn approval_does_not_clobber_non_composer_focus() {
        let (mut app, dir) = build_app();
        std::fs::write(dir.join("hi.txt"), "x").unwrap();
        app.project_root = dir.clone();
        // User is on the editor, not the composer — no demote needed.
        app.focus = Focus::Editor;
        let (tx, rx) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"hi.txt"}"#.into(),
            tx,
        );
        assert_eq!(app.focus, Focus::Editor);
        assert!(app.focus_before_approval.is_none());

        on_tool_approve_accept(&mut app);
        let _ = rx.await;
        assert_eq!(app.focus, Focus::Editor);
    }

    #[tokio::test]
    async fn back_to_back_approvals_dont_lose_original_focus() {
        // Simulate the model firing two tool calls in sequence while
        // the user was in the composer. The first should snapshot
        // ChatComposer; the second should NOT overwrite the snapshot;
        // resolving the second should restore ChatComposer.
        let (mut app, dir) = build_app();
        std::fs::write(dir.join("a.txt"), "1").unwrap();
        std::fs::write(dir.join("b.txt"), "2").unwrap();
        app.project_root = dir.clone();
        app.focus = Focus::ChatComposer;
        let (tx1, rx1) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c1".into(),
            tools::READ_FILE.into(),
            r#"{"path":"a.txt"}"#.into(),
            tx1,
        );
        on_tool_approve_accept(&mut app);
        let _ = rx1.await;
        // After the first approval the queue would be empty in the
        // real worker because the second tool call only fires after
        // the first reply travels. We simulate that order by enqueuing
        // the second now (post-first-resolve) — focus should already
        // be back to ChatComposer.
        assert_eq!(app.focus, Focus::ChatComposer);
        let (tx2, rx2) = oneshot::channel();
        queue_tool_for_approval(
            &mut app,
            "c2".into(),
            tools::READ_FILE.into(),
            r#"{"path":"b.txt"}"#.into(),
            tx2,
        );
        assert_eq!(app.focus, Focus::Chat);
        on_tool_approve_accept(&mut app);
        let _ = rx2.await;
        assert_eq!(app.focus, Focus::ChatComposer);
    }
}
