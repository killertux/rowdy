use edtui::EditorMode;
use ratatui::crossterm::event::{Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_textarea::Input;

use crate::action::{
    Action, AuthAction, CommandAction, CompletionAction, ConnFormAction, ConnListAction, HelpAxis,
    HelpScrollDelta, ResultNavAction, SchemaAction,
};
use crate::app::App;
use crate::export::ExportFormat;
use crate::state::focus::{Focus, PendingChord};
use crate::state::overlay::Overlay;
use crate::state::results::ResultViewMode;
use crate::state::screen::Screen;

pub fn translate(app: &App, event: CtEvent) -> Option<Action> {
    match event {
        CtEvent::Key(key) if key.kind == KeyEventKind::Press => translate_key(app, key, event),
        CtEvent::Key(_) => None,
        CtEvent::Paste(_) => translate_paste(app, event),
        _ if app.focus == Focus::Editor && is_plain_normal(app) => Some(Action::EditorEvent(event)),
        _ => None,
    }
}

/// `true` when there's no overlay AND the screen is `Normal` — the
/// "user is just sitting in the editor" state where raw events are
/// safe to forward to edtui.
fn is_plain_normal(app: &App) -> bool {
    app.overlay.is_none() && matches!(app.screen, Screen::Normal)
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
    // Overlays preempt paste routing — `:` command bar takes the
    // bracketed-paste payload before the underlying screen sees it.
    if let Some(Overlay::Command(_)) = &app.overlay {
        return Some(Action::Command(CommandAction::Paste(Some(text))));
    }
    match &app.screen {
        Screen::Auth(_) => Some(Action::Auth(AuthAction::Paste(Some(text)))),
        Screen::EditConnection(_) => Some(Action::ConnForm(ConnFormAction::Paste(Some(text)))),
        Screen::Normal if app.focus == Focus::Editor => Some(Action::EditorEvent(event)),
        _ => None,
    }
}

fn translate_key(app: &App, key: KeyEvent, raw: CtEvent) -> Option<Action> {
    // Ctrl+C is the global escape hatch — except in TextArea-input modes,
    // where it's bound to "copy" instead.
    if !consumes_ctrl_c(app.overlay.as_ref(), &app.screen)
        && let Some(action) = panic_quit(key)
    {
        return Some(action);
    }
    // Overlays preempt the screen keymap — Help is open over the result
    // grid, key goes to the help popover and not the grid.
    if let Some(overlay) = &app.overlay {
        return match overlay {
            Overlay::Command(_) => translate_command_key(key),
            Overlay::ConfirmRun { .. } => translate_confirm_key(key),
            // Keys are inert until the worker responds.
            Overlay::Connecting { .. } => None,
            Overlay::Help { .. } => translate_help_key(key),
        };
    }
    match &app.screen {
        Screen::Normal => translate_normal_key(app, key, raw),
        Screen::ResultExpanded { view, .. } => translate_expanded_key(key, view),
        Screen::Auth(_) => translate_auth_key(key),
        Screen::EditConnection(_) => translate_conn_form_key(key),
        Screen::ConnectionList(state) => translate_conn_list_key(key, state.is_confirming()),
    }
}

/// Help popover keys: vim-style scrolling plus q/Esc to close. Any change
/// here should be reflected in the "Help (this screen)" section of
/// `HELP_SECTIONS` in `src/ui/help_view.rs`.
fn translate_help_key(key: KeyEvent) -> Option<Action> {
    use HelpAxis::{Horizontal, Vertical};
    use HelpScrollDelta::{Bottom, By, Top};
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, ctrl) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), false) => Some(Action::CloseHelp),
        (KeyCode::Char('j') | KeyCode::Down, false) => Some(Action::HelpScroll(Vertical, By(1))),
        (KeyCode::Char('k') | KeyCode::Up, false) => Some(Action::HelpScroll(Vertical, By(-1))),
        (KeyCode::Char('h') | KeyCode::Left, false) => Some(Action::HelpScroll(Horizontal, By(-2))),
        (KeyCode::Char('l') | KeyCode::Right, false) => Some(Action::HelpScroll(Horizontal, By(2))),
        (KeyCode::Char('d'), true) => Some(Action::HelpScroll(Vertical, By(8))),
        (KeyCode::Char('u'), true) => Some(Action::HelpScroll(Vertical, By(-8))),
        (KeyCode::Char('g'), false) => Some(Action::HelpScroll(Vertical, Top)),
        (KeyCode::Char('G'), false) => Some(Action::HelpScroll(Vertical, Bottom)),
        (KeyCode::Char('0') | KeyCode::Home, _) => Some(Action::HelpScroll(Horizontal, Top)),
        (KeyCode::Char('$') | KeyCode::End, _) => Some(Action::HelpScroll(Horizontal, Bottom)),
        _ => None,
    }
}

fn consumes_ctrl_c(overlay: Option<&Overlay>, screen: &Screen) -> bool {
    if matches!(overlay, Some(Overlay::Command(_))) {
        return true;
    }
    matches!(screen, Screen::Auth(_) | Screen::EditConnection(_))
}

// NOTE: any new connection-list binding MUST also be listed in the `:help`
// popover. See `HELP_SECTIONS` in `src/ui/help_view.rs`.
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
    if let Some(act) = clipboard_arm(
        key,
        AuthAction::Paste(None),
        AuthAction::Copy,
        AuthAction::Cut,
    ) {
        return Some(Action::Auth(act));
    }
    match key.code {
        KeyCode::Esc => Some(Action::Auth(AuthAction::Cancel)),
        KeyCode::Enter => Some(Action::Auth(AuthAction::Submit)),
        _ => Some(Action::Auth(AuthAction::Input(Input::from(key)))),
    }
}

fn translate_conn_form_key(key: KeyEvent) -> Option<Action> {
    if let Some(act) = clipboard_arm(
        key,
        ConnFormAction::Paste(None),
        ConnFormAction::Copy,
        ConnFormAction::Cut,
    ) {
        return Some(Action::ConnForm(act));
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

/// Pick the per-mode action variant matching the clipboard shortcut on
/// `key`, if any. Each input modal has its own `Paste`/`Copy`/`Cut`
/// variants; the caller passes them in and we project the recognised
/// op onto the right one.
fn clipboard_arm<A>(key: KeyEvent, paste: A, copy: A, cut: A) -> Option<A> {
    Some(match clipboard_action(key)? {
        ClipboardOp::Paste => paste,
        ClipboardOp::Copy => copy,
        ClipboardOp::Cut => cut,
    })
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
    if let Some(act) = clipboard_arm(
        key,
        CommandAction::Paste(None),
        CommandAction::Copy,
        CommandAction::Cut,
    ) {
        return Some(Action::Command(act));
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
    // Completion popover preempts edtui — without this, Esc would drop
    // the user out of Insert mode and Tab would insert a tab.
    if app.completion.is_some()
        && app.focus == Focus::Editor
        && let Some(action) = translate_completion_popover_key(key)
    {
        return Some(action);
    }
    // Manual `Ctrl+Space` opens the popover; works in any editor mode
    // (Normal/Insert/Visual). Intercepted before the global/edtui
    // branches so the chord never reaches edtui.
    if app.focus == Focus::Editor && is_ctrl_space(key) {
        return Some(Action::Completion(CompletionAction::Open));
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

fn is_ctrl_space(key: KeyEvent) -> bool {
    key.code == KeyCode::Char(' ')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// Keys consumed by the popover when it's open. `None` means "fall
/// through to whatever would normally handle this key" — the user can
/// keep typing while the popover stays open and refreshes itself.
fn translate_completion_popover_key(key: KeyEvent) -> Option<Action> {
    use CompletionAction::*;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, ctrl) {
        (KeyCode::Esc, _) => Some(Action::Completion(Close)),
        (KeyCode::Tab, _) | (KeyCode::Enter, _) => Some(Action::Completion(Accept)),
        (KeyCode::Up, _) => Some(Action::Completion(Up)),
        (KeyCode::Down, _) => Some(Action::Completion(Down)),
        (KeyCode::Char('n'), true) => Some(Action::Completion(Down)),
        (KeyCode::Char('p'), true) => Some(Action::Completion(Up)),
        _ => None,
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

// NOTE: any new global binding MUST also be listed in the `:help` popover.
// See `HELP_SECTIONS` in `src/ui/help_view.rs`.
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
    if key.code == KeyCode::Char('=') && key.modifiers.is_empty() {
        return Some(Action::FormatEditor);
    }
    None
}

// NOTE: any new leader-chord binding MUST also be listed in the `:help`
// popover. See `HELP_SECTIONS` in `src/ui/help_view.rs`.
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

// NOTE: any new expanded-result binding MUST also be listed in the `:help`
// popover. See `HELP_SECTIONS` in `src/ui/help_view.rs`.
fn translate_expanded_key(key: KeyEvent, view: &ResultViewMode) -> Option<Action> {
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
            KeyCode::Char('s') | KeyCode::Char('S') => {
                Some(Action::ResultYankFormat(ExportFormat::Sql))
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

// NOTE: any new schema-panel binding MUST also be listed in the `:help`
// popover. See `HELP_SECTIONS` in `src/ui/help_view.rs`.
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
    match (&app.screen, app.focus) {
        (Screen::ResultExpanded { .. }, _) => Some(Action::ResultNav(ResultNavAction::Top)),
        (Screen::Normal, Focus::Schema) => Some(Action::Schema(SchemaAction::Top)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the pure key→action translators. Functions that
    //! reach into `App` (the entry-point dispatcher and the few
    //! mode-driven helpers like `translate_normal_key`) are covered
    //! by the integration tests in `action.rs`; here we focus on
    //! everything that's a pure `KeyEvent` consumer.
    use super::*;
    use crate::action::{ConnListAction, SchemaAction};
    use crate::state::results::{ResultCursor, ResultViewMode};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        key_mod(code, KeyModifiers::CONTROL)
    }

    fn matches_action(actual: &Option<Action>, want: &str) -> bool {
        actual
            .as_ref()
            .map(|a| format!("{a:?}").contains(want))
            .unwrap_or(false)
    }

    // ----- panic_quit / clipboard_action / consumes_ctrl_c ---------------

    #[test]
    fn panic_quit_only_fires_for_bare_ctrl_c() {
        assert!(matches!(
            panic_quit(ctrl(KeyCode::Char('c'))),
            Some(Action::Quit)
        ));
        // Ctrl+Shift+C is a clipboard shortcut, must not quit.
        assert!(
            panic_quit(key_mod(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
            .is_none()
        );
        // Cmd+C similarly.
        assert!(
            panic_quit(key_mod(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SUPER
            ))
            .is_none()
        );
        // Plain `c` does nothing.
        assert!(panic_quit(key(KeyCode::Char('c'))).is_none());
    }

    #[test]
    fn ctrl_c_consumed_by_text_input_modes() {
        use crate::state::auth::{AuthKind, AuthState};
        use crate::state::command::CommandBuffer;
        use crate::state::conn_form::ConnFormState;
        // Command overlay over Normal screen.
        let cmd = Overlay::Command(CommandBuffer::default());
        assert!(consumes_ctrl_c(Some(&cmd), &Screen::Normal));
        // Auth and EditConnection screens (no overlay).
        assert!(consumes_ctrl_c(
            None,
            &Screen::Auth(AuthState::new(AuthKind::FirstSetup))
        ));
        assert!(consumes_ctrl_c(
            None,
            &Screen::EditConnection(ConnFormState::new_create())
        ));
        // Plain Normal screen with no overlay does NOT consume Ctrl+C
        // (the panic-quit branch fires).
        assert!(!consumes_ctrl_c(None, &Screen::Normal));
        // Help overlay does not consume Ctrl+C either — only Command does.
        let help = Overlay::Help {
            scroll: 0,
            h_scroll: 0,
        };
        assert!(!consumes_ctrl_c(Some(&help), &Screen::Normal));
    }

    #[test]
    fn clipboard_action_recognises_all_modifier_variants() {
        // Ctrl-only.
        assert!(matches!(
            clipboard_action(ctrl(KeyCode::Char('v'))),
            Some(ClipboardOp::Paste)
        ));
        assert!(matches!(
            clipboard_action(ctrl(KeyCode::Char('c'))),
            Some(ClipboardOp::Copy)
        ));
        assert!(matches!(
            clipboard_action(ctrl(KeyCode::Char('x'))),
            Some(ClipboardOp::Cut)
        ));
        // Ctrl+Shift.
        let cs = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(matches!(
            clipboard_action(key_mod(KeyCode::Char('V'), cs)),
            Some(ClipboardOp::Paste)
        ));
        // Cmd (Super).
        assert!(matches!(
            clipboard_action(key_mod(KeyCode::Char('v'), KeyModifiers::SUPER)),
            Some(ClipboardOp::Paste)
        ));
        // Bare key — not a shortcut.
        assert!(clipboard_action(key(KeyCode::Char('v'))).is_none());
        // Random ctrl key — not a shortcut.
        assert!(clipboard_action(ctrl(KeyCode::Char('a'))).is_none());
    }

    #[test]
    fn is_ctrl_space_is_strict() {
        assert!(is_ctrl_space(ctrl(KeyCode::Char(' '))));
        assert!(!is_ctrl_space(key(KeyCode::Char(' '))));
        // Ctrl+Shift+Space should NOT trigger — only Ctrl+Space exactly.
        assert!(!is_ctrl_space(key_mod(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
    }

    // ----- translate_help_key -------------------------------------------

    #[test]
    fn help_key_close_aliases() {
        assert!(matches!(
            translate_help_key(key(KeyCode::Esc)),
            Some(Action::CloseHelp)
        ));
        assert!(matches!(
            translate_help_key(key(KeyCode::Char('q'))),
            Some(Action::CloseHelp)
        ));
    }

    #[test]
    fn help_key_vertical_scroll() {
        let down = translate_help_key(key(KeyCode::Char('j')));
        assert!(matches_action(&down, "Vertical"));
        assert!(matches_action(&down, "By(1)"));
        let up = translate_help_key(key(KeyCode::Char('k')));
        assert!(matches_action(&up, "By(-1)"));
        let half_page_down = translate_help_key(ctrl(KeyCode::Char('d')));
        assert!(matches_action(&half_page_down, "By(8)"));
        let top = translate_help_key(key(KeyCode::Char('g')));
        assert!(matches_action(&top, "Top"));
        let bottom = translate_help_key(key(KeyCode::Char('G')));
        assert!(matches_action(&bottom, "Bottom"));
    }

    #[test]
    fn help_key_horizontal_scroll() {
        let left = translate_help_key(key(KeyCode::Char('h')));
        assert!(matches_action(&left, "Horizontal"));
        assert!(matches_action(&left, "By(-2)"));
        let right = translate_help_key(key(KeyCode::Char('l')));
        assert!(matches_action(&right, "By(2)"));
        let line_start = translate_help_key(key(KeyCode::Char('0')));
        assert!(matches_action(&line_start, "Horizontal"));
        assert!(matches_action(&line_start, "Top"));
        let line_end = translate_help_key(key(KeyCode::Char('$')));
        assert!(matches_action(&line_end, "Bottom"));
    }

    #[test]
    fn help_key_unknown_returns_none() {
        assert!(translate_help_key(key(KeyCode::Char('z'))).is_none());
        assert!(translate_help_key(key(KeyCode::F(1))).is_none());
    }

    // ----- translate_command_key ---------------------------------------

    #[test]
    fn command_key_clipboard_arms_route_through() {
        assert!(matches_action(
            &translate_command_key(ctrl(KeyCode::Char('v'))),
            "Paste"
        ));
        assert!(matches_action(
            &translate_command_key(ctrl(KeyCode::Char('c'))),
            "Copy"
        ));
        assert!(matches_action(
            &translate_command_key(ctrl(KeyCode::Char('x'))),
            "Cut"
        ));
    }

    #[test]
    fn command_key_submit_and_cancel() {
        assert!(matches_action(
            &translate_command_key(key(KeyCode::Enter)),
            "Submit"
        ));
        assert!(matches_action(
            &translate_command_key(key(KeyCode::Esc)),
            "Cancel"
        ));
    }

    #[test]
    fn command_key_input_passthrough() {
        // Any other key falls through as an Input. Matching just by tag.
        assert!(matches_action(
            &translate_command_key(key(KeyCode::Char('a'))),
            "Input"
        ));
    }

    // ----- translate_auth_key ------------------------------------------

    #[test]
    fn auth_key_clipboard_then_modal_keys() {
        assert!(matches_action(
            &translate_auth_key(ctrl(KeyCode::Char('v'))),
            "Auth(Paste"
        ));
        assert!(matches_action(
            &translate_auth_key(key(KeyCode::Esc)),
            "Cancel"
        ));
        assert!(matches_action(
            &translate_auth_key(key(KeyCode::Enter)),
            "Submit"
        ));
        // Random typing falls into Input(...).
        assert!(matches_action(
            &translate_auth_key(key(KeyCode::Char('p'))),
            "Input"
        ));
    }

    // ----- translate_conn_form_key -------------------------------------

    #[test]
    fn conn_form_key_routes() {
        assert!(matches_action(
            &translate_conn_form_key(ctrl(KeyCode::Char('v'))),
            "ConnForm(Paste"
        ));
        assert!(matches_action(
            &translate_conn_form_key(key(KeyCode::Tab)),
            "ToggleFocus"
        ));
        assert!(matches_action(
            &translate_conn_form_key(key(KeyCode::BackTab)),
            "ToggleFocus"
        ));
        assert!(matches_action(
            &translate_conn_form_key(key(KeyCode::Esc)),
            "Cancel"
        ));
        assert!(matches_action(
            &translate_conn_form_key(key(KeyCode::Enter)),
            "Submit"
        ));
    }

    // ----- translate_conn_list_key -------------------------------------

    #[test]
    fn conn_list_navigation() {
        let case = |k, want| {
            let action = translate_conn_list_key(key(k), false);
            assert!(
                matches!(action, Some(Action::ConnList(ref a)) if std::mem::discriminant(a) == std::mem::discriminant(&want)),
                "{k:?} → {want:?}"
            );
        };
        case(KeyCode::Char('j'), ConnListAction::Down);
        case(KeyCode::Char('k'), ConnListAction::Up);
        case(KeyCode::Char('g'), ConnListAction::Top);
        case(KeyCode::Char('G'), ConnListAction::Bottom);
        case(KeyCode::Enter, ConnListAction::UseSelected);
        case(KeyCode::Char('a'), ConnListAction::AddNew);
        case(KeyCode::Char('e'), ConnListAction::EditSelected);
        case(KeyCode::Char('d'), ConnListAction::BeginDelete);
        case(KeyCode::Esc, ConnListAction::Close);
        case(KeyCode::Char('q'), ConnListAction::Close);
    }

    #[test]
    fn conn_list_confirming_only_accepts_yes_no() {
        // y / Y / Enter → ConfirmDelete.
        for k in [KeyCode::Char('y'), KeyCode::Char('Y'), KeyCode::Enter] {
            assert!(matches!(
                translate_conn_list_key(key(k), true),
                Some(Action::ConnList(ConnListAction::ConfirmDelete))
            ));
        }
        // n / N / Esc → CancelDelete.
        for k in [KeyCode::Char('n'), KeyCode::Char('N'), KeyCode::Esc] {
            assert!(matches!(
                translate_conn_list_key(key(k), true),
                Some(Action::ConnList(ConnListAction::CancelDelete))
            ));
        }
        // Other keys are inert.
        assert!(translate_conn_list_key(key(KeyCode::Char('j')), true).is_none());
    }

    // ----- translate_confirm_key ---------------------------------------

    #[test]
    fn confirm_run_only_accepts_enter_or_esc() {
        assert!(matches!(
            translate_confirm_key(key(KeyCode::Enter)),
            Some(Action::ConfirmRunSubmit)
        ));
        assert!(matches!(
            translate_confirm_key(key(KeyCode::Esc)),
            Some(Action::ConfirmRunCancel)
        ));
        // Any other key is intentionally inert (no accidental edits).
        assert!(translate_confirm_key(key(KeyCode::Char('y'))).is_none());
        assert!(translate_confirm_key(key(KeyCode::Char(' '))).is_none());
    }

    // ----- translate_window_chord / translate_global / translate_schema_key -

    #[test]
    fn window_chord_focus_and_resize() {
        assert!(matches!(
            translate_window_chord(key(KeyCode::Char('h'))),
            Some(Action::FocusPanel(Focus::Editor))
        ));
        assert!(matches!(
            translate_window_chord(key(KeyCode::Char('l'))),
            Some(Action::FocusPanel(Focus::Schema))
        ));
        // `<` grows the schema panel, `>` shrinks it.
        assert!(matches!(
            translate_window_chord(key(KeyCode::Char('<'))),
            Some(Action::ResizeSchema(2))
        ));
        assert!(matches!(
            translate_window_chord(key(KeyCode::Char('>'))),
            Some(Action::ResizeSchema(-2))
        ));
        assert!(translate_window_chord(key(KeyCode::Char('z'))).is_none());
    }

    #[test]
    fn global_keys_recognised() {
        assert!(matches!(
            translate_global(ctrl(KeyCode::Char('w'))),
            Some(Action::SetPendingChord(PendingChord::Window))
        ));
        assert!(matches!(
            translate_global(key(KeyCode::Char(':'))),
            Some(Action::OpenCommand)
        ));
        assert!(matches!(
            translate_global(key(KeyCode::Char(' '))),
            Some(Action::SetPendingChord(PendingChord::Leader))
        ));
        assert!(matches!(
            translate_global(key(KeyCode::Char('='))),
            Some(Action::FormatEditor)
        ));
        assert!(translate_global(key(KeyCode::Char('a'))).is_none());
    }

    #[test]
    fn schema_keys() {
        let case = |k, want| {
            let actual = translate_schema_key(key(k));
            assert!(
                matches!(actual, Some(Action::Schema(ref a)) if std::mem::discriminant(a) == std::mem::discriminant(&want)),
                "{k:?}"
            );
        };
        case(KeyCode::Char('j'), SchemaAction::Down);
        case(KeyCode::Char('k'), SchemaAction::Up);
        case(KeyCode::Char('h'), SchemaAction::CollapseOrAscend);
        case(KeyCode::Char('l'), SchemaAction::ExpandOrDescend);
        case(KeyCode::Enter, SchemaAction::Toggle);
        case(KeyCode::Char('o'), SchemaAction::Toggle);
        case(KeyCode::Char('G'), SchemaAction::Bottom);
        // `g` is a chord trigger, not a Schema action.
        assert!(matches!(
            translate_schema_key(key(KeyCode::Char('g'))),
            Some(Action::SetPendingChord(PendingChord::GG))
        ));
        // `<` / `>` resize the panel.
        assert!(matches!(
            translate_schema_key(key(KeyCode::Char('<'))),
            Some(Action::ResizeSchema(2))
        ));
        assert!(translate_schema_key(key(KeyCode::Char('z'))).is_none());
    }

    // ----- translate_completion_popover_key ----------------------------

    #[test]
    fn completion_popover_keys() {
        let case = |key_event, want| {
            assert!(
                matches!(translate_completion_popover_key(key_event), Some(Action::Completion(a)) if std::mem::discriminant(&a) == std::mem::discriminant(&want))
            );
        };
        case(key(KeyCode::Esc), CompletionAction::Close);
        case(key(KeyCode::Tab), CompletionAction::Accept);
        case(key(KeyCode::Enter), CompletionAction::Accept);
        case(key(KeyCode::Up), CompletionAction::Up);
        case(key(KeyCode::Down), CompletionAction::Down);
        case(ctrl(KeyCode::Char('n')), CompletionAction::Down);
        case(ctrl(KeyCode::Char('p')), CompletionAction::Up);
        // Plain typing falls through (the popover keeps refining its filter).
        assert!(translate_completion_popover_key(key(KeyCode::Char('x'))).is_none());
    }

    // ----- translate_expanded_key --------------------------------------

    #[test]
    fn expanded_key_yank_format_submode_only_takes_format_picks() {
        let view = ResultViewMode::YankFormat {
            anchor: ResultCursor::default(),
        };
        let case = |k, want: ExportFormat| {
            assert!(matches!(
                translate_expanded_key(key(k), &view),
                Some(Action::ResultYankFormat(f)) if f == want
            ));
        };
        case(KeyCode::Char('c'), ExportFormat::Csv);
        case(KeyCode::Char('C'), ExportFormat::Csv);
        case(KeyCode::Char('t'), ExportFormat::Tsv);
        case(KeyCode::Char('j'), ExportFormat::Json);
        case(KeyCode::Char('s'), ExportFormat::Sql);
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Esc), &view),
            Some(Action::ResultCancelYankFormat)
        ));
        // Non-format, non-cancel keys are inert in YankFormat mode.
        assert!(translate_expanded_key(key(KeyCode::Char('h')), &view).is_none());
    }

    #[test]
    fn expanded_key_normal_navigation() {
        let view = ResultViewMode::Normal;
        // Esc/q in Normal closes the view (no Visual to drop back to).
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Esc), &view),
            Some(Action::CollapseResult)
        ));
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Char('q')), &view),
            Some(Action::CollapseResult)
        ));
        // `v` enters Visual.
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Char('v')), &view),
            Some(Action::ResultEnterVisual)
        ));
        // `y` yanks the current cell.
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Char('y')), &view),
            Some(Action::ResultYank)
        ));
        // `g` triggers the gg chord.
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Char('g')), &view),
            Some(Action::SetPendingChord(PendingChord::GG))
        ));
        // Navigation keys.
        let nav_cases = [
            (KeyCode::Char('h'), "Left"),
            (KeyCode::Char('l'), "Right"),
            (KeyCode::Char('j'), "Down"),
            (KeyCode::Char('k'), "Up"),
            (KeyCode::Char('0'), "LineStart"),
            (KeyCode::Char('$'), "LineEnd"),
            (KeyCode::Char('G'), "Bottom"),
        ];
        for (k, want) in nav_cases {
            let action = translate_expanded_key(key(k), &view);
            assert!(matches_action(&action, want), "{k:?} → {want}");
        }
    }

    #[test]
    fn expanded_key_visual_mode_esc_drops_to_normal() {
        let view = ResultViewMode::Visual {
            anchor: ResultCursor::default(),
        };
        // In Visual, Esc/q drops back to Normal sub-mode (not closing).
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Esc), &view),
            Some(Action::ResultExitVisual)
        ));
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Char('q')), &view),
            Some(Action::ResultExitVisual)
        ));
        // `v` toggles Visual off.
        assert!(matches!(
            translate_expanded_key(key(KeyCode::Char('v')), &view),
            Some(Action::ResultExitVisual)
        ));
    }
}
