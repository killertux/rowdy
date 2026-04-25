use edtui::EditorMode;
use ratatui::crossterm::event::{Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::action::{Action, CommandAction, ResultNavAction, SchemaAction};
use crate::app::App;
use crate::state::focus::{Focus, Mode, PendingChord};

pub fn translate(app: &App, event: CtEvent) -> Option<Action> {
    match event {
        CtEvent::Key(key) if key.kind == KeyEventKind::Press => translate_key(app, key, event),
        CtEvent::Key(_) => None,
        _ if app.focus == Focus::Editor && app.mode.is_normal() => Some(Action::EditorEvent(event)),
        _ => None,
    }
}

fn translate_key(app: &App, key: KeyEvent, raw: CtEvent) -> Option<Action> {
    if let Some(action) = panic_quit(key) {
        return Some(action);
    }
    match &app.mode {
        Mode::Command(_) => translate_command_key(key),
        Mode::Normal => translate_normal_key(app, key, raw),
        Mode::ResultExpanded { .. } => translate_expanded_key(app, key),
        Mode::ConfirmRun { .. } => translate_confirm_key(key),
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
    let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
    ctrl_c.then_some(Action::Quit)
}

fn translate_command_key(key: KeyEvent) -> Option<Action> {
    use CommandAction::*;
    let action = match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Cancel,
        (KeyCode::Enter, _) => Submit,
        (KeyCode::Backspace, _) => Backspace,
        (KeyCode::Left, _) => MoveLeft,
        (KeyCode::Right, _) => MoveRight,
        (KeyCode::Char(ch), m) if !m.contains(KeyModifiers::CONTROL) => Insert(ch),
        _ => return None,
    };
    Some(Action::Command(action))
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

fn translate_expanded_key(_app: &App, key: KeyEvent) -> Option<Action> {
    use ResultNavAction::*;
    let action = match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => return Some(Action::CollapseResult),
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
