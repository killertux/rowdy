//! Editor-session lifecycle: switching, creating, deleting, persisting,
//! and the debounced background-save logic.

use std::time::Duration;

use crate::app::App;
use crate::command;
use crate::session;
use crate::state::status::QueryStatus;
use crate::worker::WorkerCommand;

use super::SessionAction;

const SESSION_DEBOUNCE: Duration = Duration::from_millis(800);

pub(super) fn reset_session(app: &mut App) {
    if app.active_connection.is_none() {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    }
    let _ = app.cmd_tx.send(WorkerCommand::ResetSession);
    app.status = QueryStatus::Notice {
        msg: "session reset — open transactions rolled back".into(),
    };
}

pub(super) fn clear_session(app: &mut App) {
    // Editor buffer + results — local state we own, so wipe synchronously.
    crate::state::editor::replace_buffer_text(&mut app.editor.state, "");
    app.results.clear();
    app.preview_hidden = false;
    app.editor_dirty = true;
    // And roll back the pinned connection so a fresh prompt doesn't
    // inherit a stale BEGIN. Best-effort: if there's no connection, the
    // local clear still stands.
    if app.active_connection.is_some() {
        let _ = app.cmd_tx.send(WorkerCommand::ResetSession);
    }
    app.status = QueryStatus::Notice {
        msg: "session cleared".into(),
    };
}

/// Push the next debounced save 800ms into the future. Skips when there's
/// no active connection — the editor isn't user-reachable in those modes,
/// but the early return keeps us honest if that ever changes.
pub(crate) fn schedule_session_save(app: &mut App) {
    if app.active_connection.is_none() {
        return;
    }
    app.editor_dirty = true;
    app.pending_save_at = Some(tokio::time::Instant::now() + SESSION_DEBOUNCE);
}

/// Write the current editor buffer to the active connection's
/// active session file (`session_<active_session_index>.sql`).
/// Best-effort: failures are logged and swallowed so a flaky disk
/// can't break the editor.
pub(crate) fn flush_session(app: &mut App) {
    let Some(name) = app.active_connection.clone() else {
        app.editor_dirty = false;
        app.pending_save_at = None;
        return;
    };
    let path = session::path_for(&app.data_dir, &name, app.active_session_index);
    let text = app.editor.text();
    match session::save(&path, &text) {
        Ok(()) => app.log.info("session", format!("saved {}", path.display())),
        Err(err) => app
            .log
            .warn("session", format!("save {} failed: {err}", path.display())),
    }
    app.editor_dirty = false;
    app.pending_save_at = None;
}

/// `:session …` ↔ `Action::Session(...)` translation. Keeps the
/// command parser independent of `SessionAction` (which lives next
/// to the dispatcher) so adding a new subcommand only touches
/// `command.rs` + this conversion + the dispatcher.
pub(super) fn session_subcommand_to_action(sub: command::SessionSubcommand) -> SessionAction {
    use command::SessionSubcommand as S;
    match sub {
        S::List => SessionAction::List,
        S::Next => SessionAction::Next,
        S::Prev => SessionAction::Prev,
        S::New => SessionAction::New,
        S::Switch(n) => SessionAction::Switch(n),
        S::Delete(n) => SessionAction::Delete(n),
    }
}

/// Route a `SessionAction` against the active connection. No-ops with a
/// status notice when there's no connection — the editor isn't
/// reachable in those modes, but the early return keeps an
/// accidentally-bound `<Space>n` from silently doing nothing.
pub(super) fn dispatch_session(app: &mut App, action: SessionAction) {
    let Some(name) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    match action {
        SessionAction::List => session_list_status(app),
        SessionAction::Next => session_switch_relative(app, &name, 1),
        SessionAction::Prev => session_switch_relative(app, &name, -1),
        SessionAction::New => session_create_and_switch(app, &name),
        SessionAction::Switch(n) => session_switch_to_index(app, &name, n),
        SessionAction::Delete(n) => session_delete(app, &name, n),
    }
}

fn session_list_status(app: &mut App) {
    let list: Vec<String> = app.session_indices.iter().map(usize::to_string).collect();
    app.status = QueryStatus::Notice {
        msg: format!(
            "sessions: {} (active {})",
            list.join(", "),
            app.active_session_index
        ),
    };
}

fn session_switch_relative(app: &mut App, name: &str, delta: i32) {
    if app.session_indices.len() < 2 {
        app.status = QueryStatus::Notice {
            msg: format!(
                "only one session ({}) — use `:session new` to create another",
                app.active_session_index
            ),
        };
        return;
    }
    let pos = app
        .session_indices
        .iter()
        .position(|&i| i == app.active_session_index)
        .unwrap_or(0) as i32;
    let len = app.session_indices.len() as i32;
    let next_pos = (pos + delta).rem_euclid(len) as usize;
    let target = app.session_indices[next_pos];
    session_switch_to_existing(app, name, target);
}

fn session_create_and_switch(app: &mut App, name: &str) {
    let new_index = session::next_free_index(&app.session_indices);
    // Touch the new file so subsequent `list_indices` calls (and any
    // external `ls`) see it. An empty session file round-trips
    // through `load` as an empty buffer.
    let path = session::path_for(&app.data_dir, name, new_index);
    if let Err(err) = session::save(&path, "") {
        app.log.warn(
            "session",
            format!("create {} failed: {err}", path.display()),
        );
        app.status = QueryStatus::Failed {
            error: format!("create session {new_index} failed: {err}"),
        };
        return;
    }
    flush_session(app);
    app.session_indices.push(new_index);
    app.session_indices.sort_unstable();
    app.active_session_index = new_index;
    load_session(app, name, new_index);
    app.status = QueryStatus::Notice {
        msg: format!("created session {new_index}"),
    };
}

fn session_switch_to_index(app: &mut App, name: &str, target: usize) {
    if !app.session_indices.contains(&target) {
        app.status = QueryStatus::Failed {
            error: format!(
                "no session {target} (existing: {})",
                app.session_indices
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        return;
    }
    if target == app.active_session_index {
        // No-op switches still refresh the indicator so the user
        // gets a confirmation of where they are.
        app.status = QueryStatus::Notice {
            msg: format!("session {target} (already active)"),
        };
        return;
    }
    session_switch_to_existing(app, name, target);
}

fn session_switch_to_existing(app: &mut App, name: &str, target: usize) {
    flush_session(app);
    app.active_session_index = target;
    load_session(app, name, target);
    app.status = QueryStatus::Notice {
        msg: format!("switched to session {target}"),
    };
}

fn session_delete(app: &mut App, name: &str, target: usize) {
    if !app.session_indices.contains(&target) {
        app.status = QueryStatus::Failed {
            error: format!("no session {target} to delete"),
        };
        return;
    }
    if app.session_indices.len() == 1 {
        app.status = QueryStatus::Failed {
            error: "can't delete the only remaining session".into(),
        };
        return;
    }
    let active_being_deleted = app.active_session_index == target;
    if let Err(err) = session::delete(&app.data_dir, name, target) {
        app.log
            .warn("session", format!("delete {target} failed: {err}"));
        app.status = QueryStatus::Failed {
            error: format!("delete session {target} failed: {err}"),
        };
        return;
    }
    app.session_indices.retain(|&i| i != target);
    if active_being_deleted {
        // The buffer the user was editing belonged to the deleted
        // file — discard it (deliberately *not* flushing) and load
        // the previous index in the list. `session_indices` is
        // guaranteed non-empty here because we refused on len==1.
        let fallback = app
            .session_indices
            .iter()
            .copied()
            .rev()
            .find(|&i| i < target)
            .unwrap_or(app.session_indices[0]);
        // Suppress the pending debounced save for the just-killed
        // index — without this clear the next tick would re-write
        // the file we just deleted.
        app.editor_dirty = false;
        app.pending_save_at = None;
        app.active_session_index = fallback;
        load_session(app, name, fallback);
    }
    app.status = QueryStatus::Notice {
        msg: format!("deleted session {target}"),
    };
}

/// Load the session at `index` for `name` into the editor. Treats a
/// missing file as an empty buffer — first save will create it.
/// Resets the dirty/timer state so the load itself doesn't trigger
/// another save.
pub(super) fn load_session(app: &mut App, name: &str, index: usize) {
    let path = session::path_for(&app.data_dir, name, index);
    match session::load(&path) {
        Ok(text) => {
            app.editor.replace_text(&text);
            app.log
                .info("session", format!("loaded {}", path.display()));
        }
        Err(err) => {
            app.log
                .warn("session", format!("load {} failed: {err}", path.display()));
            app.editor.replace_text("");
        }
    }
    app.editor_dirty = false;
    app.pending_save_at = None;
}

/// Load the persisted chat-session messages for `name` into
/// `app.chat.messages`. Missing file → empty history. Failures are
/// surfaced as a warning + empty history rather than a hard error;
/// chat is non-essential to the rest of the UI.
pub(super) fn load_chat_session(app: &mut App, name: &str) {
    let path = crate::chat_session::path_for(&app.data_dir, name);
    match crate::chat_session::load(&path) {
        Ok(messages) => {
            let count = messages.len();
            app.chat.messages = messages;
            // Land at the bottom of the loaded history — that's where
            // the conversation left off, and what the user expects when
            // resuming a session.
            app.chat.scroll_to_bottom();
            app.chat.streaming = false;
            app.chat.error = None;
            app.log.info(
                "chat",
                format!("loaded {count} message(s) from {}", path.display()),
            );
        }
        Err(err) => {
            app.log
                .warn("chat", format!("load {} failed: {err}", path.display()));
            app.chat.messages.clear();
        }
    }
}
