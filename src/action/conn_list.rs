//! `Action::ConnList(_)` dispatcher — modal connection picker.
//!
//! Lives in its own file so the per-mode coordination (delete-confirm
//! flow, swap-to-edit-form, swap-to-active) sits next to a single
//! readable match instead of getting buried in the main dispatcher.

use crate::action::{ConnListAction, open_conn_form_edit, perform_delete, use_connection};
use crate::app::App;
use crate::state::conn_form::{ConnFormPostSave, ConnFormState};
use crate::state::screen::Screen;

pub fn apply(app: &mut App, action: ConnListAction) {
    let Screen::ConnectionList(state) = &mut app.screen else {
        return;
    };
    // While confirming a delete, only y/Enter and n/Esc do anything (handled
    // via ConfirmDelete / CancelDelete).
    if state.is_confirming() {
        match action {
            ConnListAction::ConfirmDelete => {
                if let Some(name) = state.take_pending_delete() {
                    perform_delete(app, &name);
                    refresh_conn_list(app);
                }
            }
            ConnListAction::CancelDelete => state.cancel_delete(),
            _ => {}
        }
        return;
    }
    match action {
        ConnListAction::Down => state.move_selection(1),
        ConnListAction::Up => state.move_selection(-1),
        ConnListAction::Top => state.jump_top(),
        ConnListAction::Bottom => state.jump_bottom(),
        ConnListAction::AddNew => {
            app.screen = Screen::EditConnection(
                ConnFormState::new_create().with_post_save(ConnFormPostSave::ReturnToList),
            );
        }
        ConnListAction::EditSelected => {
            if let Some(name) = state.selected_name().map(str::to_string) {
                open_conn_form_edit(app, &name, ConnFormPostSave::ReturnToList);
            }
        }
        ConnListAction::UseSelected => {
            if let Some(name) = state.selected_name().map(str::to_string) {
                use_connection(app, &name);
            }
        }
        ConnListAction::BeginDelete => state.begin_delete(),
        ConnListAction::Close => app.screen = Screen::Normal,
        // Handled in the confirming branch above.
        ConnListAction::ConfirmDelete | ConnListAction::CancelDelete => {}
    }
}

/// Re-load the picker's entry list from `app.config` after a delete or
/// other config edit. If the list is now empty there's nothing to pick
/// from, so we drop straight back to the Normal screen.
pub fn refresh_conn_list(app: &mut App) {
    if let Screen::ConnectionList(state) = &mut app.screen {
        state.refresh(app.config.connection_names());
        if state.entries.is_empty() {
            app.screen = Screen::Normal;
        }
    }
}
