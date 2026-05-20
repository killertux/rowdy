//! `Action::ParamsPrompt(_)` dispatcher — the popup that collects
//! values for `$N` / `:name` placeholders before a query runs.

use std::collections::HashMap;

use super::{ParamsPromptAction, copy_from, cut_from, paste_into, send_to_worker};
use crate::app::App;
use crate::datasource::sql::placeholders;
use crate::state::overlay::Overlay;
use crate::state::status::QueryStatus;

pub fn apply(app: &mut App, action: ParamsPromptAction) {
    match action {
        ParamsPromptAction::Input(input) => {
            if let Some(state) = params_state_mut(app)
                && let Some(field) = state.current_input_mut()
            {
                let _ = field.input(input);
            }
        }
        ParamsPromptAction::Paste(text) => {
            let log = app.log.clone();
            if let Some(state) = params_state_mut(app)
                && let Some(field) = state.current_input_mut()
            {
                paste_into(field, &log, text);
            }
        }
        ParamsPromptAction::Copy => {
            let log = app.log.clone();
            if let Some(state) = params_state_mut(app)
                && let Some(field) = state.current_input_mut()
            {
                copy_from(field, &log);
            }
        }
        ParamsPromptAction::Cut => {
            let log = app.log.clone();
            if let Some(state) = params_state_mut(app)
                && let Some(field) = state.current_input_mut()
            {
                cut_from(field, &log);
            }
        }
        ParamsPromptAction::NextField => {
            if let Some(state) = params_state_mut(app) {
                state.cycle_focus(true);
            }
        }
        ParamsPromptAction::PrevField => {
            if let Some(state) = params_state_mut(app) {
                state.cycle_focus(false);
            }
        }
        ParamsPromptAction::ClearField => {
            if let Some(state) = params_state_mut(app)
                && let Some(field) = state.current_input_mut()
            {
                field.clear();
            }
        }
        ParamsPromptAction::Cancel => {
            app.overlay = None;
            app.status = QueryStatus::Cancelled;
        }
        ParamsPromptAction::Submit => submit(app),
    }
}

fn params_state_mut(
    app: &mut App,
) -> Option<&mut crate::state::params_prompt::ParamsPromptState> {
    match &mut app.overlay {
        Some(Overlay::ParamsPrompt(state)) => Some(state),
        _ => None,
    }
}

fn submit(app: &mut App) {
    let Some(Overlay::ParamsPrompt(state)) = app.overlay.take() else {
        return;
    };
    let collected = state.collected();
    let values: HashMap<_, _> = collected.iter().cloned().collect();
    let final_sql = placeholders::substitute(&state.statement, &state.placeholders, &values);

    if let Some(conn) = app.active_connection.clone() {
        crate::param_history::record(app, &conn, &state.statement, &collected);
    }

    send_to_worker(app, final_sql);
}
