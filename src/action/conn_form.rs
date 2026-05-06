//! `Action::ConnForm(_)` dispatcher — name+url two-field connection
//! editor. Handles the create/edit common save path, including the
//! post-save branch (auto-connect vs. return to picker) and the
//! test-connection flow.

use crate::action::{ConnFormAction, copy_from, cut_from, dispatch_connect, paste_into};
use crate::app::App;
use crate::state::conn_form::{ConnFormPostSave, TestResult};
use crate::state::conn_list::ConnListState;
use crate::state::screen::Screen;
use crate::worker::WorkerEvent;

pub fn apply(app: &mut App, action: ConnFormAction) {
    let Screen::EditConnection(state) = &mut app.screen else {
        return;
    };
    match action {
        ConnFormAction::Input(input) => {
            let _ = state.current_input_mut().input(input);
            state.test_result = None;
        }
        ConnFormAction::Paste(text) => {
            paste_into(state.current_input_mut(), &app.log, text);
            state.test_result = None;
        }
        ConnFormAction::Copy => copy_from(state.current_input_mut(), &app.log),
        ConnFormAction::Cut => {
            cut_from(state.current_input_mut(), &app.log);
            state.test_result = None;
        }
        ConnFormAction::ToggleFocus => state.toggle_focus(),
        ConnFormAction::ClearField => {
            state.current_input_mut().clear();
            state.test_result = None;
        }
        ConnFormAction::Cancel => app.should_quit = true,
        ConnFormAction::Submit => submit(app),
        ConnFormAction::TestConnection => test_connection(app),
    }
}

fn submit(app: &mut App) {
    let Screen::EditConnection(state) = &mut app.screen else {
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
            app.screen = Screen::ConnectionList(list);
        }
    }
}

fn test_connection(app: &mut App) {
    let Screen::EditConnection(state) = &mut app.screen else {
        return;
    };
    state.test_result = None;
    let url = state.url_value();
    if url.is_empty() {
        state.test_result = Some(TestResult::Failure("url is empty".into()));
        return;
    }
    state.testing = true;
    let evt_tx = app.evt_tx.clone();
    let log = app.log.clone();
    tokio::spawn(async move {
        let result = crate::datasource::connect(&url, log).await;
        let (success, error) = match result {
            Ok(_) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };
        let _ = evt_tx.send(WorkerEvent::TestConnectionResult {
            success,
            error,
        });
    });
}
