use edtui::EditorMode;
use ratatui::crossterm::event::{Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_textarea::Input;

use crate::action::{
    Action, AuthAction, CommandAction, ConnFormAction, ConnListAction, ResultNavAction,
    SchemaAction,
};
use crate::app::App;
use crate::export::ExportFormat;
use crate::state::focus::{Focus, Mode, PendingChord};
use crate::state::results::ResultViewMode;

pub fn translate(app: &App, event: CtEvent) -> Option<Action> {
    match event {
        CtEvent::Key(key) if key.kind == KeyEventKind::Press => translate_key(app, key, event),
        CtEvent::Key(_) => None,
        CtEvent::Paste(_) => translate_paste(app, event),
        _ if app.focus == Focus::Editor && app.mode.is_normal() => Some(Action::EditorEvent(event)),
        _ => None,
    }
}

/// Bracketed-paste flow: many terminals (and macOS by default) handle
/// `Cmd+V` / `Ctrl+Shift+V` themselves and deliver the result as a single
/// `Event::Paste(text)` rather than a key event. Route it to the focused
/// input or, if the editor is the active surface, hand the original event
/// to edtui (which has its own paste support).
fn translate_paste(app: &App, event: CtEvent) -> Option<Action> {
    let CtEvent::Paste(text) = &event else {
        return None;
    };
    let text = text.clone();
    match &app.mode {
        Mode::Command(_) => Some(Action::Command(CommandAction::Paste(Some(text)))),
        Mode::Auth(_) => Some(Action::Auth(AuthAction::Paste(Some(text)))),
        Mode::EditConnection(_) => Some(Action::ConnForm(ConnFormAction::Paste(Some(text)))),
        Mode::Normal if app.focus == Focus::Editor => Some(Action::EditorEvent(event)),
        _ => None,
    }
}

fn translate_key(app: &App, key: KeyEvent, raw: CtEvent) -> Option<Action> {
    // Ctrl+C is the global escape hatch — except in TextArea-input modes,
    // where it's bound to "copy" instead.
    if !consumes_ctrl_c(&app.mode)
        && let Some(action) = panic_quit(key)
    {
        return Some(action);
    }
    match &app.mode {
        Mode::Command(_) => translate_command_key(key),
        Mode::Normal => translate_normal_key(app, key, raw),
        Mode::ResultExpanded { view, .. } => translate_expanded_key(app, key, view),
        Mode::ConfirmRun { .. } => translate_confirm_key(key),
        Mode::Auth(_) => translate_auth_key(key),
        Mode::EditConnection(_) => translate_conn_form_key(key),
        Mode::ConnectionList(state) => translate_conn_list_key(key, state.is_confirming()),
        Mode::Connecting { .. } => None, // keys are inert until the worker responds
    }
}

fn consumes_ctrl_c(mode: &Mode) -> bool {
    matches!(
        mode,
        Mode::Command(_) | Mode::Auth(_) | Mode::EditConnection(_)
    )
}

fn translate_conn_list_key(key: KeyEvent, confirming: bool) -> Option<Action> {
    use ConnListAction::*;
    if confirming {
        let action = match (key.code, key.modifiers) {
            (KeyCode::Char('y') | KeyCode::Char('Y'), _) | (KeyCode::Enter, _) => ConfirmDelete,
            (KeyCode::Char('n') | KeyCode::Char('N'), _) | (KeyCode::Esc, _) => CancelDelete,
            _ => return None,
        };
        return Some(Action::ConnList(action));
    }
    let action = match (key.code, key.modifiers) {
        (KeyCode::Char('j') | KeyCode::Down, _) => Down,
        (KeyCode::Char('k') | KeyCode::Up, _) => Up,
        (KeyCode::Char('g'), _) => Top,
        (KeyCode::Char('G'), _) => Bottom,
        (KeyCode::Enter | KeyCode::Char('u'), _) => UseSelected,
        (KeyCode::Char('a'), _) => AddNew,
        (KeyCode::Char('e'), _) => EditSelected,
        (KeyCode::Char('d'), _) => BeginDelete,
        (KeyCode::Esc | KeyCode::Char('q'), _) => Close,
        _ => return None,
    };
    Some(Action::ConnList(action))
}

fn translate_auth_key(key: KeyEvent) -> Option<Action> {
    if let Some(clip) = clipboard_action(key) {
        return Some(Action::Auth(match clip {
            ClipboardOp::Paste => AuthAction::Paste(None),
            ClipboardOp::Copy => AuthAction::Copy,
            ClipboardOp::Cut => AuthAction::Cut,
        }));
    }
    match key.code {
        KeyCode::Esc => Some(Action::Auth(AuthAction::Cancel)),
        KeyCode::Enter => Some(Action::Auth(AuthAction::Submit)),
        _ => Some(Action::Auth(AuthAction::Input(Input::from(key)))),
    }
}

fn translate_conn_form_key(key: KeyEvent) -> Option<Action> {
    if let Some(clip) = clipboard_action(key) {
        return Some(Action::ConnForm(match clip {
            ClipboardOp::Paste => ConnFormAction::Paste(None),
            ClipboardOp::Copy => ConnFormAction::Copy,
            ClipboardOp::Cut => ConnFormAction::Cut,
        }));
    }
    match key.code {
        KeyCode::Esc => Some(Action::ConnForm(ConnFormAction::Cancel)),
        KeyCode::Enter => Some(Action::ConnForm(ConnFormAction::Submit)),
        KeyCode::Tab | KeyCode::BackTab => Some(Action::ConnForm(ConnFormAction::ToggleFocus)),
        _ => Some(Action::ConnForm(ConnFormAction::Input(Input::from(key)))),
    }
}

#[derive(Debug, Clone, Copy)]
enum ClipboardOp {
    Paste,
    Copy,
    Cut,
}

/// Recognises the standard system-clipboard shortcuts:
/// - `Ctrl+C` / `Ctrl+V` / `Ctrl+X`
/// - `Ctrl+Shift+C` / `Ctrl+Shift+V` / `Ctrl+Shift+X` (terminal default)
/// - `Cmd+C` / `Cmd+V` / `Cmd+X` (macOS, via `KeyModifiers::SUPER`, when the
///   terminal forwards Cmd via the kitty keyboard protocol; otherwise the
///   terminal handles it itself and we receive an `Event::Paste` instead)
///
/// Returns `None` for any other key — caller falls through to the regular
/// per-mode handling.
fn clipboard_action(key: KeyEvent) -> Option<ClipboardOp> {
    let mods = key.modifiers;
    let triggered = mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::SUPER);
    if !triggered {
        return None;
    }
    match key.code {
        KeyCode::Char('v') | KeyCode::Char('V') => Some(ClipboardOp::Paste),
        KeyCode::Char('c') | KeyCode::Char('C') => Some(ClipboardOp::Copy),
        KeyCode::Char('x') | KeyCode::Char('X') => Some(ClipboardOp::Cut),
        _ => None,
    }
}

fn translate_confirm_key(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Enter => Some(Action::ConfirmRunSubmit),
        KeyCode::Esc => Some(Action::ConfirmRunCancel),
        _ => None,
    }
}

fn panic_quit(key: KeyEvent) -> Option<Action> {
    // Plain Ctrl+C only — `Ctrl+Shift+C` and `Cmd+C` are clipboard shortcuts
    // and must never accidentally quit.
    let mods = key.modifiers;
    let bare_ctrl_c = key.code == KeyCode::Char('c')
        && mods.contains(KeyModifiers::CONTROL)
        && !mods.contains(KeyModifiers::SHIFT)
        && !mods.contains(KeyModifiers::SUPER);
    bare_ctrl_c.then_some(Action::Quit)
}

fn translate_command_key(key: KeyEvent) -> Option<Action> {
    if let Some(clip) = clipboard_action(key) {
        return Some(Action::Command(match clip {
            ClipboardOp::Paste => CommandAction::Paste(None),
            ClipboardOp::Copy => CommandAction::Copy,
            ClipboardOp::Cut => CommandAction::Cut,
        }));
    }
    match key.code {
        KeyCode::Esc => Some(Action::Command(CommandAction::Cancel)),
        KeyCode::Enter => Some(Action::Command(CommandAction::Submit)),
        _ => Some(Action::Command(CommandAction::Input(Input::from(key)))),
    }
}

fn translate_normal_key(app: &App, key: KeyEvent, raw: CtEvent) -> Option<Action> {
    if let Some(action) = translate_pending_chord(app, key) {
        return Some(action);
    }
    if can_intercept_globally(app)
        && let Some(action) = translate_global(key)
    {
        return Some(action);
    }
    match app.focus {
        Focus::Editor => Some(Action::EditorEvent(raw)),
        Focus::Schema => translate_schema_key(key),
    }
}

fn translate_pending_chord(app: &App, key: KeyEvent) -> Option<Action> {
    match app.pending {
        PendingChord::Window => translate_window_chord(key),
        PendingChord::GG => translate_gg_chord(app, key),
        PendingChord::Leader => translate_leader_chord(app, key),
        PendingChord::None => None,
    }
}

fn can_intercept_globally(app: &App) -> bool {
    match app.focus {
        Focus::Editor => matches!(
            app.editor.editor_mode(),
            EditorMode::Normal | EditorMode::Visual
        ),
        Focus::Schema => true,
    }
}

fn translate_global(key: KeyEvent) -> Option<Action> {
    let ctrl_w = key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl_w {
        return Some(Action::SetPendingChord(PendingChord::Window));
    }
    if key.code == KeyCode::Char(':') && key.modifiers.is_empty() {
        return Some(Action::OpenCommand);
    }
    if key.code == KeyCode::Char(' ') && key.modifiers.is_empty() {
        return Some(Action::SetPendingChord(PendingChord::Leader));
    }
    None
}

fn translate_leader_chord(app: &App, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('r') => Some(if app.editor.editor_mode() == EditorMode::Visual {
            Action::RunSelection
        } else {
            Action::PrepareConfirmRun
        }),
        KeyCode::Char('R') => Some(Action::RunStatementUnderCursor),
        KeyCode::Char('c') => Some(Action::CancelQuery),
        KeyCode::Char('e') => Some(Action::ExpandLatestResult),
        KeyCode::Char('t') => Some(Action::ToggleTheme),
        _ => None,
    }
}

fn translate_expanded_key(_app: &App, key: KeyEvent, view: &ResultViewMode) -> Option<Action> {
    // YankFormat sub-mode: only the format keys + cancel work; navigation
    // and other shortcuts are inert until the user picks one.
    if matches!(view, ResultViewMode::YankFormat { .. }) {
        return match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') => {
                Some(Action::ResultYankFormat(ExportFormat::Csv))
            }
            KeyCode::Char('t') | KeyCode::Char('T') => {
                Some(Action::ResultYankFormat(ExportFormat::Tsv))
            }
            KeyCode::Char('j') | KeyCode::Char('J') => {
                Some(Action::ResultYankFormat(ExportFormat::Json))
            }
            KeyCode::Esc => Some(Action::ResultCancelYankFormat),
            _ => None,
        };
    }

    use ResultNavAction::*;
    let in_visual = matches!(view, ResultViewMode::Visual { .. });
    let action = match (key.code, key.modifiers) {
        // Esc/q: in Visual, drop back to Normal; in Normal, close the view.
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
            return Some(if in_visual {
                Action::ResultExitVisual
            } else {
                Action::CollapseResult
            });
        }
        (KeyCode::Char('v'), m) if m.is_empty() => {
            return Some(if in_visual {
                Action::ResultExitVisual
            } else {
                Action::ResultEnterVisual
            });
        }
        (KeyCode::Char('y'), m) if m.is_empty() => return Some(Action::ResultYank),
        (KeyCode::Char('h'), _) | (KeyCode::Left, _) => Left,
        (KeyCode::Char('l'), _) | (KeyCode::Right, _) => Right,
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => Down,
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => Up,
        (KeyCode::Char('0'), _) | (KeyCode::Home, _) => LineStart,
        (KeyCode::Char('$'), _) | (KeyCode::End, _) => LineEnd,
        (KeyCode::Char('G'), _) => Bottom,
        (KeyCode::Char('g'), m) if m.is_empty() => {
            return Some(Action::SetPendingChord(PendingChord::GG));
        }
        _ => return None,
    };
    Some(Action::ResultNav(action))
}

fn translate_window_chord(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('h') => Some(Action::FocusPanel(Focus::Editor)),
        KeyCode::Char('l') => Some(Action::FocusPanel(Focus::Schema)),
        KeyCode::Char('<') => Some(Action::ResizeSchema(2)),
        KeyCode::Char('>') => Some(Action::ResizeSchema(-2)),
        // No vertical neighbours yet; chord is consumed by the loop regardless.
        _ => None,
    }
}

fn translate_schema_key(key: KeyEvent) -> Option<Action> {
    let action = match (key.code, key.modifiers) {
        (KeyCode::Char('j'), m) if m.is_empty() => SchemaAction::Down,
        (KeyCode::Char('k'), m) if m.is_empty() => SchemaAction::Up,
        (KeyCode::Char('h'), m) if m.is_empty() => SchemaAction::CollapseOrAscend,
        (KeyCode::Char('l'), m) if m.is_empty() => SchemaAction::ExpandOrDescend,
        (KeyCode::Enter, _) | (KeyCode::Char('o'), _) => SchemaAction::Toggle,
        (KeyCode::Char('G'), _) => SchemaAction::Bottom,
        (KeyCode::Char('g'), m) if m.is_empty() => {
            return Some(Action::SetPendingChord(PendingChord::GG));
        }
        (KeyCode::Char('<'), _) => return Some(Action::ResizeSchema(2)),
        (KeyCode::Char('>'), _) => return Some(Action::ResizeSchema(-2)),
        _ => return None,
    };
    Some(Action::Schema(action))
}

fn translate_gg_chord(app: &App, key: KeyEvent) -> Option<Action> {
    if key.code != KeyCode::Char('g') {
        return None;
    }
    match (&app.mode, app.focus) {
        (Mode::ResultExpanded { .. }, _) => Some(Action::ResultNav(ResultNavAction::Top)),
        (Mode::Normal, Focus::Schema) => Some(Action::Schema(SchemaAction::Top)),
        _ => None,
    }
}
