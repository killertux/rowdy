//! `Action::ConnForm(_)` dispatcher — name+url two-field connection
//! editor. Handles the create/edit common save path, including the
//! post-save branch (auto-connect vs. return to picker).

use crate::action::{
    ConnFormAction, copy_from, cut_from, dispatch_connect, paste_into,
};
use crate::app::App;
use crate::state::conn_form::ConnFormPostSave;
use crate::state::conn_list::ConnListState;
use crate::state::focus::Mode;

pub fn apply(app: &mut App, action: ConnFormAction) {
    let Mode::EditConnection(state) = &mut app.mode else {
        return;
    };
    match action {
        ConnFormAction::Input(input) => {
            let _ = state.current_input_mut().input(input);
        }
        ConnFormAction::Paste(text) => paste_into(state.current_input_mut(), &app.log, text),
        ConnFormAction::Copy => copy_from(state.current_input_mut(), &app.log),
        ConnFormAction::Cut => cut_from(state.current_input_mut(), &app.log),
        ConnFormAction::ToggleFocus => state.toggle_focus(),
        ConnFormAction::Cancel => app.should_quit = true,
        ConnFormAction::Submit => submit(app),
    }
}

fn submit(app: &mut App) {
    let Mode::EditConnection(state) = &mut app.mode else {
        return;
    };
    state.error = None;
    let name = state.name_value();
    let url = state.url_value();
    let post_save = state.post_save;

    if name.is_empty() {
        state.error = Some("name is required".into());
        return;
    }
    if url.is_empty() {
        state.error = Some("url is required".into());
        return;
    }

    let store = match app.connection_store.as_ref() {
        Some(s) => s,
        None => {
            state.error = Some("internal: no connection store available".into());
            return;
        }
    };

    let entry = match store.make_entry(name.clone(), &url) {
        Ok(e) => e,
        Err(err) => {
            state.error = Some(format!("encrypt failed: {err}"));
            return;
        }
    };
    if let Err(err) = app.config.upsert_connection(entry) {
        state.error = Some(format!("save failed: {err}"));
        return;
    }

    app.log.info("conn", format!("saved connection {name}"));
    match post_save {
        ConnFormPostSave::AutoConnect => dispatch_connect(app, name, url),
        ConnFormPostSave::ReturnToList => {
            let entries = app.config.connection_names();
            let mut list = ConnListState::new(entries);
            if let Some(idx) = list.entries.iter().position(|n| n == &name) {
                list.selected = idx;
            }
            app.mode = Mode::ConnectionList(list);
        }
    }
}
