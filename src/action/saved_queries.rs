//! `:save`, `:load`, `:run-saved` — per-connection named query store.

use crate::app::App;
use crate::saved_queries;
use crate::state::overlay::Overlay;
use crate::state::saved_query_picker::SavedQueryPickerState;
use crate::state::status::QueryStatus;

#[derive(Debug, Clone)]
pub enum SavedQueryAction {
    /// Picker cursor — up/down step.
    PickerMove(i32),
    PickerTop,
    PickerBottom,
    /// Picker Enter — load / run the selected entry.
    PickerConfirm,
    /// Picker Esc.
    PickerCancel,
    /// `:save` overwrite prompt — Enter.
    ConfirmOverwrite,
    /// `:save` overwrite prompt — Esc / n.
    CancelOverwrite,
}

pub fn apply_save(app: &mut App, name: String) {
    let Some(conn) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    if let Err(err) = saved_queries::validate_name(&name) {
        app.status = QueryStatus::Failed { error: err };
        return;
    }
    let Some(sql) = resolve_sql_to_save(app) else {
        app.status = QueryStatus::Failed {
            error: "no selection or statement under cursor to save".into(),
        };
        return;
    };
    if saved_queries::exists(&app.data_dir, &conn, &name) {
        app.overlay = Some(Overlay::ConfirmSaveOverwrite { name, sql });
        return;
    }
    write_and_notice(app, &conn, &name, &sql);
}

pub fn apply_load(app: &mut App, name: String) {
    let Some(conn) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    if let Err(err) = saved_queries::validate_name(&name) {
        app.status = QueryStatus::Failed { error: err };
        return;
    }
    let sql = match saved_queries::load(&app.data_dir, &conn, &name) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            app.status = QueryStatus::Failed {
                error: format!("no saved query named {name:?}"),
            };
            return;
        }
        Err(err) => {
            app.status = QueryStatus::Failed {
                error: format!("load {name:?} failed: {err}"),
            };
            return;
        }
    };
    crate::state::editor::insert_text_at_cursor(&mut app.editor.state, &sql);
    app.editor_dirty = true;
    super::schedule_session_save(app);
    app.status = QueryStatus::Notice {
        msg: format!("loaded saved query {name:?}"),
    };
}

pub fn apply_run_saved(app: &mut App, name: String) {
    let Some(conn) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    if let Err(err) = saved_queries::validate_name(&name) {
        app.status = QueryStatus::Failed { error: err };
        return;
    }
    let sql = match saved_queries::load(&app.data_dir, &conn, &name) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            app.status = QueryStatus::Failed {
                error: format!("no saved query named {name:?}"),
            };
            return;
        }
        Err(err) => {
            app.status = QueryStatus::Failed {
                error: format!("load {name:?} failed: {err}"),
            };
            return;
        }
    };
    super::query::dispatch_query(app, sql);
}

pub fn open_run_picker(app: &mut App) {
    let Some(conn) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    let entries = match saved_queries::list(&app.data_dir, &conn) {
        Ok(v) => v,
        Err(err) => {
            app.status = QueryStatus::Failed {
                error: format!("list saved queries failed: {err}"),
            };
            return;
        }
    };
    app.overlay = Some(Overlay::SavedQueryPicker(SavedQueryPickerState::new(
        entries,
    )));
}

pub fn apply(app: &mut App, action: SavedQueryAction) {
    match action {
        SavedQueryAction::PickerMove(delta) => {
            if let Some(Overlay::SavedQueryPicker(state)) = app.overlay.as_mut() {
                state.move_selection(delta);
            }
        }
        SavedQueryAction::PickerTop => {
            if let Some(Overlay::SavedQueryPicker(state)) = app.overlay.as_mut() {
                state.jump_top();
            }
        }
        SavedQueryAction::PickerBottom => {
            if let Some(Overlay::SavedQueryPicker(state)) = app.overlay.as_mut() {
                state.jump_bottom();
            }
        }
        SavedQueryAction::PickerCancel => {
            if matches!(app.overlay, Some(Overlay::SavedQueryPicker(_))) {
                app.overlay = None;
            }
        }
        SavedQueryAction::PickerConfirm => picker_confirm(app),
        SavedQueryAction::ConfirmOverwrite => confirm_overwrite(app),
        SavedQueryAction::CancelOverwrite => {
            if matches!(app.overlay, Some(Overlay::ConfirmSaveOverwrite { .. })) {
                app.overlay = None;
                app.status = QueryStatus::Notice {
                    msg: "save cancelled".into(),
                };
            }
        }
    }
}

fn picker_confirm(app: &mut App) {
    let Some(Overlay::SavedQueryPicker(state)) = app.overlay.as_ref() else {
        return;
    };
    let Some(name) = state.selected_name().map(str::to_string) else {
        // Empty list — just close the overlay.
        app.overlay = None;
        return;
    };
    app.overlay = None;
    apply_run_saved(app, name);
}

fn confirm_overwrite(app: &mut App) {
    let Some(Overlay::ConfirmSaveOverwrite { name, sql }) = app.overlay.take() else {
        return;
    };
    let Some(conn) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    write_and_notice(app, &conn, &name, &sql);
}

fn resolve_sql_to_save(app: &App) -> Option<String> {
    if let Some(text) = crate::state::editor::selection_text(&app.editor.state) {
        return Some(text);
    }
    let range = crate::state::editor::statement_under_cursor(&app.editor.state)?;
    let trimmed = range.text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn write_and_notice(app: &mut App, conn: &str, name: &str, sql: &str) {
    match saved_queries::save(&app.data_dir, conn, name, sql) {
        Ok(()) => {
            app.status = QueryStatus::Notice {
                msg: format!("saved query {name:?}"),
            };
        }
        Err(err) => {
            app.status = QueryStatus::Failed {
                error: format!("save {name:?} failed: {err}"),
            };
        }
    }
}
