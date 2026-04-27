//! `Action::Auth(_)` dispatcher — first-setup vs. unlock flow.
//!
//! `auth_submit` is the entire decision tree for "what does pressing
//! Enter on the password prompt do" — branches on `AuthKind::{FirstSetup,
//! Unlock}` and decides whether to initialise crypto, unlock with an
//! existing block, or count down failed attempts.

use crate::action::{AuthAction, paste_into};
use crate::app::App;
use crate::connections::{self, ConnectionStore};
use crate::state::auth::AuthKind;
use crate::state::conn_form::ConnFormState;
use crate::state::conn_list::ConnListState;
use crate::state::screen::Screen;

pub fn apply(app: &mut App, action: AuthAction) {
    let Screen::Auth(state) = &mut app.screen else {
        return;
    };
    match action {
        AuthAction::Input(input) => {
            let _ = state.input.input(input);
        }
        AuthAction::Paste(text) => paste_into(&mut state.input, &app.log, text),
        // Copying a masked password buffer would defeat the masking;
        // ignore copy/cut here.
        AuthAction::Copy | AuthAction::Cut => {}
        AuthAction::Cancel => app.should_quit = true,
        AuthAction::Submit => submit(app),
    }
}

fn submit(app: &mut App) {
    let Screen::Auth(state) = &mut app.screen else {
        return;
    };
    state.error = None;
    let attempt = state.input.lines().first().cloned().unwrap_or_default();
    let kind = state.kind.clone();

    match kind {
        AuthKind::FirstSetup => {
            let store = if attempt.is_empty() {
                app.log.info("auth", "plaintext store chosen");
                ConnectionStore::plaintext()
            } else {
                match connections::initialise_crypto(&attempt) {
                    Ok((block, key)) => {
                        if let Err(err) = app.config.set_crypto(block) {
                            set_error(app, format!("save crypto block failed: {err}"));
                            return;
                        }
                        app.log.info("auth", "encrypted store initialised");
                        ConnectionStore::encrypted(key)
                    }
                    Err(err) => {
                        set_error(app, format!("crypto setup failed: {err}"));
                        return;
                    }
                }
            };
            app.connection_store = Some(store);
            transition_post_auth(app);
        }
        AuthKind::Unlock { block } => match connections::unlock(&attempt, &block) {
            Ok(key) => {
                app.connection_store = Some(ConnectionStore::encrypted(key));
                app.log.info("auth", "store unlocked");
                transition_post_auth(app);
            }
            Err(_) => {
                if let Screen::Auth(state) = &mut app.screen {
                    state.attempts = state.attempts.saturating_add(1);
                    state.clear_input();
                    let remaining = state.attempts_remaining();
                    if remaining == 0 {
                        app.log.error("auth", "too many failed attempts; exiting");
                        app.exit_code = 1;
                        app.should_quit = true;
                    } else {
                        state.error = Some(format!(
                            "wrong password ({} {} left)",
                            remaining,
                            if remaining == 1 {
                                "attempt"
                            } else {
                                "attempts"
                            }
                        ));
                    }
                }
            }
        },
    }
}

fn set_error(app: &mut App, msg: String) {
    if let Screen::Auth(state) = &mut app.screen {
        state.error = Some(msg);
    }
}

/// Decides what to render after the Auth screen resolves. Either jumps
/// straight into a connection form (no saved connections) or opens the
/// connection picker so the user can choose one.
fn transition_post_auth(app: &mut App) {
    let entries = app.config.connection_names();
    if entries.is_empty() {
        app.screen = Screen::EditConnection(ConnFormState::new_create());
        return;
    }
    app.screen = Screen::ConnectionList(ConnListState::new(entries));
}
