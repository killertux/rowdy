use edtui::EditorMode;
use ratatui::crossterm::event::{
    Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui_textarea::Input;

use crate::action::{
    Action, AuthAction, ChatAction, CommandAction, CompletionAction, ConnFormAction,
    ConnListAction, HelpAxis, HelpScrollDelta, LlmSettingsAction, MouseTarget, ResultNavAction,
    SchemaAction,
};
use crate::app::App;
use crate::command::FormatScope;
use crate::export::ExportFormat;
use crate::state::focus::{Focus, PendingChord};
use crate::state::layout::{DragState, rect_contains};
use crate::state::llm_settings::{LlmSettingsField, LlmSettingsState};
use crate::state::overlay::Overlay;
use crate::state::results::ResultViewMode;
use crate::state::right_panel::RightPanelMode;
use crate::state::schema::NodeId;
use crate::state::screen::Screen;

pub fn translate(app: &App, event: CtEvent) -> Option<Action> {
    match event {
        CtEvent::Key(key) if key.kind == KeyEventKind::Press => translate_key(app, key, event),
        CtEvent::Key(_) => None,
        CtEvent::Paste(_) => translate_paste(app, event),
        CtEvent::Mouse(mev) => translate_mouse(app, mev),
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
    // Overlays preempt paste routing — `:` command bar takes the
    // bracketed-paste payload before the underlying screen sees it.
    if let Some(overlay) = &app.overlay {
        return match overlay {
            Overlay::Command(_) => Some(Action::Command(CommandAction::Paste(Some(text)))),
            Overlay::LlmSettings(_) => {
                Some(Action::LlmSettings(LlmSettingsAction::Paste(Some(text))))
            }
            // Help / ConfirmRun / Connecting don't take text input.
            _ => None,
        };
    }
    match &app.screen {
        Screen::Auth(_) => Some(Action::Auth(AuthAction::Paste(Some(text)))),
        Screen::EditConnection(_) => Some(Action::ConnForm(ConnFormAction::Paste(Some(text)))),
        Screen::Normal if app.focus == Focus::Editor => Some(Action::EditorEvent(event)),
        // Bracketed paste only goes into the chat composer when it's the
        // active sink — not in chat *normal* mode (where the composer is
        // dormant and a stray paste would silently disappear).
        Screen::Normal if app.focus == Focus::ChatComposer => {
            Some(Action::Chat(ChatAction::Paste(Some(text))))
        }
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
            Overlay::LlmSettings(state) => translate_llm_settings_key(state, key),
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
    if is_ctrl_u(key) {
        return Some(Action::Auth(AuthAction::ClearField));
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
    if is_ctrl_u(key) {
        return Some(Action::ConnForm(ConnFormAction::ClearField));
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
    if is_ctrl_u(key) {
        return Some(Action::Command(CommandAction::ClearField));
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
    // Bare `Q` (Shift+q) dismisses the inline result preview — same as
    // `:close`. Gated on a visible preview so Q is a no-op when nothing
    // is showing (avoids stealing Ex-mode-style chords from edtui in
    // contexts where it might mean something else later).
    if can_intercept_globally(app)
        && matches!(app.screen, Screen::Normal)
        && app.results.last().is_some()
        && !app.preview_hidden
        && key.code == KeyCode::Char('Q')
        && key.modifiers == KeyModifiers::SHIFT
    {
        return Some(Action::DismissResult);
    }
    match app.focus {
        Focus::Editor => Some(Action::EditorEvent(raw)),
        Focus::Schema => translate_schema_key(key),
        Focus::Chat => translate_chat_normal_key(key),
        Focus::ChatComposer => translate_chat_composer_key(key),
    }
}

fn is_ctrl_space(key: KeyEvent) -> bool {
    key.code == KeyCode::Char(' ')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
}

/// Recognise the "clear current field" shortcut. `Ctrl+U` is the
/// universal *kill-to-start-of-line* convention; in our single-line
/// form fields it functions as "wipe everything I typed".
fn is_ctrl_u(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('u') | KeyCode::Char('U'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
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
        PendingChord::Window => translate_window_chord(app.right_panel, key),
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
        // Chat *normal* mode is the modal counterpart to editor Normal —
        // globals like `:`, leader, and `Ctrl+W` flow through naturally.
        Focus::Chat => true,
        // The composer is the chat's "insert mode": keystrokes belong to
        // the TextArea. Globals are routed explicitly inside
        // `translate_chat_composer_key`.
        Focus::ChatComposer => false,
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
        return Some(Action::FormatEditor(FormatScope::Cursor));
    }
    // Panel resize is global so the user doesn't have to chase focus
    // into the schema pane (or remember the Ctrl+W chord) just to widen
    // the right column. Bare `<` grows, `>` shrinks; the Ctrl+W variants
    // remain in `translate_window_chord` for muscle memory.
    if key.code == KeyCode::Char('<') && key.modifiers.is_empty() {
        return Some(Action::ResizeSchema(2));
    }
    if key.code == KeyCode::Char('>') && key.modifiers.is_empty() {
        return Some(Action::ResizeSchema(-2));
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
        // Right-panel switchers. Uppercase to leave room for future
        // lowercase bindings; mnemonic: Schema / Chat.
        KeyCode::Char('S') => Some(Action::SetRightPanel(RightPanelMode::Schema)),
        KeyCode::Char('C') => Some(Action::SetRightPanel(RightPanelMode::Chat)),
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

fn translate_window_chord(right: RightPanelMode, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('h') => Some(Action::FocusPanel(Focus::Editor)),
        // Ctrl+W l targets whichever pane is currently painted on the
        // right — Schema by default, Chat once the user has toggled.
        KeyCode::Char('l') => Some(Action::FocusPanel(if right.is_chat() {
            Focus::Chat
        } else {
            Focus::Schema
        })),
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
        // Esc returns focus to the editor without flipping the right
        // panel back to chat — same gesture as `Ctrl+W h`, just shorter.
        (KeyCode::Esc, m) if m.is_empty() => return Some(Action::FocusPanel(Focus::Editor)),
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
        (Screen::Normal, Focus::Chat) => Some(Action::Chat(ChatAction::ScrollToTop)),
        _ => None,
    }
}

fn translate_llm_settings_key(state: &LlmSettingsState, key: KeyEvent) -> Option<Action> {
    if let Some(act) = clipboard_arm(
        key,
        LlmSettingsAction::Paste(None),
        LlmSettingsAction::Copy,
        LlmSettingsAction::Cut,
    ) {
        return Some(Action::LlmSettings(act));
    }
    let mods = key.modifiers;
    let bare = mods.is_empty();
    let on_backend = state.focus == LlmSettingsField::Backend;

    // Ctrl+U clears the focused TextArea (no-op when focus is Backend).
    if key.code == KeyCode::Char('u') && mods.contains(KeyModifiers::CONTROL) {
        return Some(Action::LlmSettings(LlmSettingsAction::ClearField));
    }
    match (key.code, bare) {
        (KeyCode::Esc, _) => Some(Action::LlmSettings(LlmSettingsAction::Cancel)),
        (KeyCode::Enter, true) => Some(Action::LlmSettings(LlmSettingsAction::Submit)),
        (KeyCode::Tab, _) => Some(Action::LlmSettings(LlmSettingsAction::CycleField)),
        (KeyCode::BackTab, _) => Some(Action::LlmSettings(LlmSettingsAction::CycleFieldBack)),
        // Backend cycling — only fires when the Backend field is active so
        // the user can use ← / → / `[` / `]` / h / l normally inside text
        // fields.
        (KeyCode::Left, true) | (KeyCode::Char('['), true) | (KeyCode::Char('h'), true)
            if on_backend =>
        {
            Some(Action::LlmSettings(LlmSettingsAction::CycleBackend(-1)))
        }
        (KeyCode::Right, true) | (KeyCode::Char(']'), true) | (KeyCode::Char('l'), true)
            if on_backend =>
        {
            Some(Action::LlmSettings(LlmSettingsAction::CycleBackend(1)))
        }
        _ => Some(Action::LlmSettings(LlmSettingsAction::Input(Input::from(
            key,
        )))),
    }
}

/// Chat *normal* mode keymap. Mirrors edtui's Normal mode in spirit: the
/// composer is dormant, keystrokes navigate the log, and `i` switches into
/// insert mode (composer focused). Globals (`:`, leader, `Ctrl+W`, etc.)
/// flow through `translate_global` because [`can_intercept_globally`]
/// returns `true` for [`Focus::Chat`].
fn translate_chat_normal_key(key: KeyEvent) -> Option<Action> {
    let mods = key.modifiers;
    let bare = mods.is_empty();
    let shift_only = mods == KeyModifiers::SHIFT;
    let _ = shift_only;

    // `i` enters insert mode (focuses the composer). `I` is a vim-ish
    // alias that lands in the same place — we don't model "start of line"
    // separately because the composer is a single text buffer.
    if matches!(key.code, KeyCode::Char('i') | KeyCode::Char('I')) && bare {
        return Some(Action::FocusPanel(Focus::ChatComposer));
    }
    // Esc returns focus to the editor without changing what the right
    // panel paints — symmetric with `Esc` from the schema panel. The
    // chat panel keeps its messages and the composer keeps its contents.
    if key.code == KeyCode::Esc && bare {
        return Some(Action::FocusPanel(Focus::Editor));
    }

    // Scrolling. The user-facing convention: ↑/h/k scroll up, ↓/l/j
    // scroll down. h/l are unconventional (vim uses them for left/right)
    // but the chat log is a vertical-only viewport, so the horizontal
    // bindings are reused for vertical scroll.
    let action = match key.code {
        KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') if bare => {
            Some(ChatAction::ScrollUp(1))
        }
        KeyCode::Down | KeyCode::Char('l') | KeyCode::Char('j') if bare => {
            Some(ChatAction::ScrollDown(1))
        }
        KeyCode::PageUp if bare => Some(ChatAction::ScrollUp(8)),
        KeyCode::PageDown if bare => Some(ChatAction::ScrollDown(8)),
        KeyCode::Home if bare => Some(ChatAction::ScrollToTop),
        KeyCode::End if bare => Some(ChatAction::ScrollToBottom),
        // `G` jumps to bottom; `gg` is handled via the GG pending chord.
        KeyCode::Char('G') => Some(ChatAction::ScrollToBottom),
        _ => None,
    };
    if let Some(a) = action {
        return Some(Action::Chat(a));
    }

    if key.code == KeyCode::Char('g') && bare {
        return Some(Action::SetPendingChord(PendingChord::GG));
    }
    None
}

/// Chat composer (insert mode) keymap. Mirrors the original chat keymap;
/// the only behavioral change is `Esc` — instead of toggling the right
/// panel, it drops back to chat *normal* mode so the user can keep
/// scrolling without losing the composer's contents.
fn translate_chat_composer_key(key: KeyEvent) -> Option<Action> {
    if let Some(act) = clipboard_arm(
        key,
        ChatAction::Paste(None),
        ChatAction::Copy,
        ChatAction::Cut,
    ) {
        return Some(Action::Chat(act));
    }
    if is_ctrl_u(key) {
        return Some(Action::Chat(ChatAction::ClearComposer));
    }
    let mods = key.modifiers;
    let bare = mods.is_empty();
    let shift_only = mods == KeyModifiers::SHIFT;
    let ctrl = mods.contains(KeyModifiers::CONTROL);

    // Ctrl+W routes back through the window-chord layer so the user can
    // hop to the editor without leaving chat focus first.
    if key.code == KeyCode::Char('w') && ctrl {
        return Some(Action::SetPendingChord(PendingChord::Window));
    }
    // Esc returns to chat normal mode — composer keeps its contents,
    // user can scroll then re-enter with `i`.
    if key.code == KeyCode::Esc && bare {
        return Some(Action::FocusPanel(Focus::Chat));
    }
    // Plain Enter submits; Shift+Enter falls through to the TextArea
    // (insert newline). The composer is multi-line by design.
    if key.code == KeyCode::Enter && bare {
        return Some(Action::Chat(ChatAction::Submit));
    }
    if (key.code == KeyCode::PageUp) && bare {
        return Some(Action::Chat(ChatAction::ScrollUp(8)));
    }
    if (key.code == KeyCode::PageDown) && bare {
        return Some(Action::Chat(ChatAction::ScrollDown(8)));
    }
    // Ctrl+Up/Down — line-by-line scroll. Plain Up/Down is reserved for
    // the composer's textarea (multi-line cursor movement).
    if key.code == KeyCode::Up && ctrl {
        return Some(Action::Chat(ChatAction::ScrollUp(1)));
    }
    if key.code == KeyCode::Down && ctrl {
        return Some(Action::Chat(ChatAction::ScrollDown(1)));
    }
    // Ctrl+Home / Ctrl+End — jump to top/bottom of the log. Plain
    // Home/End remain delegated to the composer for line-start /
    // line-end movement, matching the rest of the app's text inputs.
    if key.code == KeyCode::Home && ctrl {
        return Some(Action::Chat(ChatAction::ScrollToTop));
    }
    if key.code == KeyCode::End && ctrl {
        return Some(Action::Chat(ChatAction::ScrollToBottom));
    }
    let _ = shift_only;
    Some(Action::Chat(ChatAction::Input(Input::from(key))))
}

// ============================================================================
// Mouse translation
// ============================================================================

/// Translate a `MouseEvent` into an `Action` based on which panel the click
/// landed in. Reads `app.layout` (populated by the previous frame's render)
/// for hit-testing. Returns `None` for clicks that miss every interactive
/// surface — most quietly, hover/move events that we don't care about.
fn translate_mouse(app: &App, ev: MouseEvent) -> Option<Action> {
    // Always ignore Moved — they fire constantly and we have no use for them.
    if matches!(ev.kind, MouseEventKind::Moved) {
        return None;
    }
    // Help overlay preempts everything — its own translator handles the
    // narrow set of mouse interactions (wheel, click-outside-to-close).
    if let Some(Overlay::Help { .. }) = &app.overlay {
        return translate_mouse_help(app, ev);
    }
    // Other text-input overlays/screens (Auth, EditConnection, ConnList)
    // capture clicks on their dialog area but otherwise let outside clicks
    // dismiss them where it's safe.
    if let Some(target) = translate_mouse_modal(app, ev) {
        return Some(target);
    }
    // Drag-extend doesn't need a fresh hit-test — once a drag is in flight,
    // drags clamp to the result table and we keep extending the rectangle.
    if let MouseEventKind::Drag(MouseButton::Left) = ev.kind
        && matches!(app.layout.drag, Some(DragState::ResultSelect))
    {
        let table = app.layout.expanded_result.as_ref()?;
        let (row, col) = clamp_to_table(table, ev.column, ev.row);
        return Some(Action::Mouse(MouseTarget::ResultDragTo { row, col }));
    }
    if let MouseEventKind::Up(MouseButton::Left) = ev.kind
        && matches!(app.layout.drag, Some(DragState::ResultSelect))
    {
        return Some(Action::Mouse(MouseTarget::ResultDragEnd));
    }
    // Expanded result has its own dispatch (the editor isn't on screen).
    if matches!(app.screen, Screen::ResultExpanded { .. }) {
        return translate_mouse_expanded(app, ev);
    }
    translate_mouse_workspace(app, ev)
}

/// Hit-test against the help overlay: wheel scrolls, click outside closes.
fn translate_mouse_help(app: &App, ev: MouseEvent) -> Option<Action> {
    let area = app.layout.overlay.as_ref()?.area();
    let inside = rect_contains(area, ev.column, ev.row);
    match ev.kind {
        MouseEventKind::ScrollDown if inside => Some(Action::HelpScroll(
            HelpAxis::Vertical,
            HelpScrollDelta::By(3),
        )),
        MouseEventKind::ScrollUp if inside => Some(Action::HelpScroll(
            HelpAxis::Vertical,
            HelpScrollDelta::By(-3),
        )),
        MouseEventKind::Down(MouseButton::Left) if !inside => {
            Some(Action::Mouse(MouseTarget::OverlayDismiss))
        }
        _ => None,
    }
}

/// Click-handling for modal screens (Auth, ConnList, EditConnection) and
/// the Command overlay. For now: outside-click dismisses where safe; clicks
/// inside the modal area are inert (per-row hit-testing comes in a later
/// pass).
fn translate_mouse_modal(app: &App, ev: MouseEvent) -> Option<Action> {
    let dismissible_overlay = matches!(&app.overlay, Some(Overlay::Command(_)));
    let dismissible_screen = matches!(app.screen, Screen::ConnectionList(_));
    if !dismissible_overlay && !dismissible_screen {
        return None;
    }
    let MouseEventKind::Down(MouseButton::Left) = ev.kind else {
        return None;
    };
    if let Some(layout) = &app.layout.overlay {
        if rect_contains(layout.area(), ev.column, ev.row) {
            return None;
        }
    } else if let Some(bottom) = app.layout.bottom_bar
        && rect_contains(bottom, ev.column, ev.row)
    {
        // The command bar lives on the bottom row; clicks there shouldn't
        // dismiss the overlay (the user is just typing).
        return None;
    }
    Some(Action::Mouse(MouseTarget::OverlayDismiss))
}

fn translate_mouse_expanded(app: &App, ev: MouseEvent) -> Option<Action> {
    let table = app.layout.expanded_result.as_ref()?;
    if !rect_contains(table.area, ev.column, ev.row) {
        return None;
    }
    match ev.kind {
        MouseEventKind::ScrollDown => Some(Action::Mouse(MouseTarget::ResultScroll(3))),
        MouseEventKind::ScrollUp => Some(Action::Mouse(MouseTarget::ResultScroll(-3))),
        MouseEventKind::Down(MouseButton::Left) => {
            let (row, col) = table.cell_at(ev.column, ev.row)?;
            Some(Action::Mouse(MouseTarget::ResultDragStart { row, col }))
        }
        _ => None,
    }
}

fn translate_mouse_workspace(app: &App, ev: MouseEvent) -> Option<Action> {
    // Order: schema → chat → editor → inline-result. Each is a thin wrapper
    // that hit-tests against `app.layout` and emits an action if the click
    // lands. Schema and chat are mutually exclusive (only one populates its
    // layout slot per frame) so order between them doesn't matter.
    if let Some(action) = translate_mouse_schema(app, ev) {
        return Some(action);
    }
    if let Some(action) = translate_mouse_chat(app, ev) {
        return Some(action);
    }
    if let Some(action) = translate_mouse_inline(app, ev) {
        return Some(action);
    }
    if let Some(action) = translate_mouse_editor(app, ev) {
        return Some(action);
    }
    None
}

fn translate_mouse_chat(app: &App, ev: MouseEvent) -> Option<Action> {
    let layout = app.layout.chat.as_ref()?;
    if !rect_contains(layout.area, ev.column, ev.row) {
        return None;
    }
    match ev.kind {
        MouseEventKind::ScrollDown => Some(Action::Chat(ChatAction::ScrollDown(3))),
        MouseEventKind::ScrollUp => Some(Action::Chat(ChatAction::ScrollUp(3))),
        MouseEventKind::Down(MouseButton::Left) => {
            // A click on the composer drops straight into insert mode
            // (the user clearly wants to type). Anywhere else focuses the
            // chat panel in *normal* mode for scrolling. TextArea doesn't
            // expose a "click → cursor" hook, so we don't try to place
            // the cursor — the focus change alone is what the user is
            // usually after.
            let target = if rect_contains(layout.composer_area, ev.column, ev.row) {
                Focus::ChatComposer
            } else {
                Focus::Chat
            };
            Some(Action::FocusPanel(target))
        }
        _ => None,
    }
}

fn translate_mouse_schema(app: &App, ev: MouseEvent) -> Option<Action> {
    let layout = app.layout.schema.as_ref()?;
    if !rect_contains(layout.area, ev.column, ev.row) {
        return None;
    }
    match ev.kind {
        MouseEventKind::ScrollDown => Some(Action::Mouse(MouseTarget::SchemaScroll(3))),
        MouseEventKind::ScrollUp => Some(Action::Mouse(MouseTarget::SchemaScroll(-3))),
        MouseEventKind::Down(MouseButton::Left) => {
            let row_idx = (ev.row.checked_sub(layout.rows_area.y))? as usize;
            let id: NodeId = *layout.rows.get(row_idx)?;
            // Clicks on the leading indent + chevron column toggle expand/collapse;
            // anywhere else just selects. We don't currently track depth in
            // `SchemaLayout`, so use the leftmost few columns as a pragmatic
            // chevron hit-zone — far enough that the user landing on the
            // glyph counts, narrow enough that clicking on a label still
            // means "select this row".
            let col_local = ev.column.saturating_sub(layout.rows_area.x);
            if col_local <= 1 {
                Some(Action::Mouse(MouseTarget::SchemaToggle(id)))
            } else {
                Some(Action::Mouse(MouseTarget::SchemaRow(id)))
            }
        }
        _ => None,
    }
}

fn translate_mouse_inline(app: &App, ev: MouseEvent) -> Option<Action> {
    let layout = app.layout.inline_result.as_ref()?;
    if !rect_contains(layout.area, ev.column, ev.row) {
        return None;
    }
    if let MouseEventKind::Down(MouseButton::Left) = ev.kind
        && let Some((row, col)) = layout.cell_at(ev.column, ev.row)
    {
        return Some(Action::Mouse(MouseTarget::InlineResultJump { row, col }));
    }
    None
}

fn translate_mouse_editor(app: &App, ev: MouseEvent) -> Option<Action> {
    let area = app.layout.editor?;
    if !rect_contains(area, ev.column, ev.row) {
        return None;
    }
    // Forward only the gestures edtui understands; ignore wheel etc. for now
    // (edtui doesn't scroll on its own anyway).
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left)
        | MouseEventKind::Drag(MouseButton::Left)
        | MouseEventKind::Up(MouseButton::Left) => {
            Some(Action::Mouse(MouseTarget::Editor(CtEvent::Mouse(ev))))
        }
        _ => None,
    }
}

/// Clamp `(x, y)` to the body of `table`, returning the (row, col) under
/// the clamped position. Used while drag-extending — the user can drag
/// beyond the table without losing the selection.
fn clamp_to_table(table: &crate::state::layout::TableLayout, x: u16, y: u16) -> (usize, usize) {
    let body_bottom = table
        .body_top_y
        .saturating_add(table.body_rows.saturating_sub(1));
    let cy = y.clamp(table.body_top_y, body_bottom);
    let cx = if let (Some(&first), Some(&last)) = (table.col_x.first(), table.col_x.last()) {
        x.clamp(first, last.saturating_sub(1))
    } else {
        x
    };
    table
        .cell_at(cx, cy)
        .unwrap_or((table.row_offset, table.col_offset))
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
        let m = RightPanelMode::Schema;
        assert!(matches!(
            translate_window_chord(m, key(KeyCode::Char('h'))),
            Some(Action::FocusPanel(Focus::Editor))
        ));
        assert!(matches!(
            translate_window_chord(m, key(KeyCode::Char('l'))),
            Some(Action::FocusPanel(Focus::Schema))
        ));
        // `<` grows the schema panel, `>` shrinks it.
        assert!(matches!(
            translate_window_chord(m, key(KeyCode::Char('<'))),
            Some(Action::ResizeSchema(2))
        ));
        assert!(matches!(
            translate_window_chord(m, key(KeyCode::Char('>'))),
            Some(Action::ResizeSchema(-2))
        ));
        assert!(translate_window_chord(m, key(KeyCode::Char('z'))).is_none());
    }

    #[test]
    fn window_chord_l_targets_chat_when_right_panel_is_chat() {
        // Same chord, different right-panel mode → focus follows the painted pane.
        assert!(matches!(
            translate_window_chord(RightPanelMode::Chat, key(KeyCode::Char('l'))),
            Some(Action::FocusPanel(Focus::Chat))
        ));
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
            Some(Action::FormatEditor(FormatScope::Cursor))
        ));
        // Bare `<` / `>` resize the schema panel from any global-
        // intercept context (Editor Normal/Visual, Schema, Chat normal).
        assert!(matches!(
            translate_global(key(KeyCode::Char('<'))),
            Some(Action::ResizeSchema(2))
        ));
        assert!(matches!(
            translate_global(key(KeyCode::Char('>'))),
            Some(Action::ResizeSchema(-2))
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
        // `<` / `>` no longer live here — they're global (see
        // `global_keys_recognised`).
        assert!(translate_schema_key(key(KeyCode::Char('<'))).is_none());
        assert!(translate_schema_key(key(KeyCode::Char('>'))).is_none());
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

    // ----- translate_llm_settings_key ----------------------------------

    #[test]
    fn llm_settings_arrows_cycle_backend_only_on_backend_field() {
        let mut state = LlmSettingsState::new_create();
        // Default focus is Backend — arrows cycle.
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Left)),
            "CycleBackend(-1)"
        ));
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Right)),
            "CycleBackend(1)"
        ));
        // Move focus to Model — arrows now fall through to TextArea Input.
        state.focus = LlmSettingsField::Model;
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Left)),
            "Input"
        ));
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Right)),
            "Input"
        ));
        // `[` and `]` follow the same gating.
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Char('['))),
            "Input"
        ));
        // h/l also passthrough on non-Backend fields so users can type
        // them into the Model / API key textareas.
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Char('h'))),
            "Input"
        ));
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Char('l'))),
            "Input"
        ));
        state.focus = LlmSettingsField::Backend;
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Char('['))),
            "CycleBackend(-1)"
        ));
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Char('h'))),
            "CycleBackend(-1)"
        ));
        assert!(matches_action(
            &translate_llm_settings_key(&state, key(KeyCode::Char('l'))),
            "CycleBackend(1)"
        ));
    }

    #[test]
    fn llm_settings_ctrl_u_clears_field() {
        let state = LlmSettingsState::new_create();
        assert!(matches_action(
            &translate_llm_settings_key(&state, ctrl(KeyCode::Char('u'))),
            "ClearField"
        ));
    }

    #[test]
    fn ctrl_u_recognised_in_form_keymaps() {
        // Ctrl+U → ClearField across every modal that has a TextArea.
        assert!(matches_action(
            &translate_auth_key(ctrl(KeyCode::Char('u'))),
            "ClearField"
        ));
        assert!(matches_action(
            &translate_conn_form_key(ctrl(KeyCode::Char('u'))),
            "ClearField"
        ));
        assert!(matches_action(
            &translate_command_key(ctrl(KeyCode::Char('u'))),
            "ClearField"
        ));
        // Chat composer uses ClearComposer to disambiguate from `:chat clear`.
        assert!(matches_action(
            &translate_chat_composer_key(ctrl(KeyCode::Char('u'))),
            "ClearComposer"
        ));
    }

    // ----- translate_chat_normal_key -----------------------------------

    #[test]
    fn chat_normal_i_enters_insert_mode() {
        assert!(matches!(
            translate_chat_normal_key(key(KeyCode::Char('i'))),
            Some(Action::FocusPanel(Focus::ChatComposer))
        ));
        assert!(matches!(
            translate_chat_normal_key(key(KeyCode::Char('I'))),
            Some(Action::FocusPanel(Focus::ChatComposer))
        ));
    }

    #[test]
    fn chat_normal_esc_focuses_editor_without_flipping_panel() {
        // Esc means "go back to the editor" — the right panel keeps
        // painting chat; only focus changes. focus_panel() in the action
        // layer leaves right_panel untouched when target is Editor.
        assert!(matches!(
            translate_chat_normal_key(key(KeyCode::Esc)),
            Some(Action::FocusPanel(Focus::Editor))
        ));
    }

    #[test]
    fn schema_esc_focuses_editor() {
        assert!(matches!(
            translate_schema_key(key(KeyCode::Esc)),
            Some(Action::FocusPanel(Focus::Editor))
        ));
    }

    #[test]
    fn chat_normal_scroll_keys() {
        // Up alternatives.
        for k in [KeyCode::Up, KeyCode::Char('h'), KeyCode::Char('k')] {
            let action = translate_chat_normal_key(key(k));
            assert!(matches_action(&action, "ScrollUp(1)"), "{k:?}");
        }
        // Down alternatives.
        for k in [KeyCode::Down, KeyCode::Char('l'), KeyCode::Char('j')] {
            let action = translate_chat_normal_key(key(k));
            assert!(matches_action(&action, "ScrollDown(1)"), "{k:?}");
        }
        // Page scrolling.
        assert!(matches_action(
            &translate_chat_normal_key(key(KeyCode::PageUp)),
            "ScrollUp(8)"
        ));
        assert!(matches_action(
            &translate_chat_normal_key(key(KeyCode::PageDown)),
            "ScrollDown(8)"
        ));
        // Jumps.
        assert!(matches_action(
            &translate_chat_normal_key(key(KeyCode::Home)),
            "ScrollToTop"
        ));
        assert!(matches_action(
            &translate_chat_normal_key(key(KeyCode::End)),
            "ScrollToBottom"
        ));
        assert!(matches_action(
            &translate_chat_normal_key(key(KeyCode::Char('G'))),
            "ScrollToBottom"
        ));
    }

    #[test]
    fn chat_normal_g_starts_gg_chord() {
        assert!(matches!(
            translate_chat_normal_key(key(KeyCode::Char('g'))),
            Some(Action::SetPendingChord(PendingChord::GG))
        ));
    }

    #[test]
    fn chat_composer_esc_returns_to_normal_mode() {
        assert!(matches!(
            translate_chat_composer_key(key(KeyCode::Esc)),
            Some(Action::FocusPanel(Focus::Chat))
        ));
    }
}
