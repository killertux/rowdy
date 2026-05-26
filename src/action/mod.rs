use std::path::PathBuf;

use ratatui::crossterm::event::{Event as CtEvent, MouseEventKind};
use ratatui_textarea::{Input, TextArea};

mod auth;
pub mod chat;
mod completion;
mod conn_form;
mod conn_list;
mod llm_settings;
mod params_prompt;
mod query;
mod results;
mod saved_queries;
mod schema;
mod session;
mod update;

pub use saved_queries::SavedQueryAction;

pub(crate) use session::{flush_session, schedule_session_save};
pub use update::try_promote_pending_update;

use crate::app::App;
use crate::clipboard;
use crate::command::{
    self, ChatSubcommand, ConnSubcommand, FormatScope, ParsedTarget, ThemeChoice,
};
use crate::export::ExportFormat;
use crate::state::command::CommandBuffer;
use crate::state::conn_form::{ConnFormPostSave, ConnFormState};
use crate::state::conn_list::ConnListState;
use crate::state::focus::{Focus, PendingChord};
use crate::state::overlay::Overlay;
use crate::state::right_panel::RightPanelMode;
use crate::state::schema::{NodeId, SchemaPanel};
use crate::state::screen::Screen;
use crate::state::status::QueryStatus;
use crate::state::theme_picker::ThemePickerState;
use crate::ui::theme::Theme;
use crate::worker::{IntrospectTarget, WorkerCommand, WorkerEvent};

#[derive(Debug)]
pub enum Action {
    Quit,
    FocusPanel(Focus),
    ResizeSchema(i16),
    SetPendingChord(PendingChord),
    EditorEvent(CtEvent),
    OpenCommand,
    Command(CommandAction),
    Schema(SchemaAction),
    PrepareConfirmRun,
    ConfirmRunSubmit,
    ConfirmRunCancel,
    /// User pressed `y` on the auto-update prompt. Spawns the install
    /// script against the running binary's directory.
    UpdateAccept,
    /// User pressed `n`/`Esc` on the auto-update prompt. Persists the
    /// dismissed tag so we don't re-prompt for the same version.
    UpdateDismiss,
    /// `:update` — explicit user-initiated check. Bypasses the 24h
    /// throttle and the dismissal record so the user can re-trigger
    /// a prompt they previously dismissed, and surfaces an "already
    /// on the latest" notice when no new release is found.
    CheckForUpdate,
    RunStatementUnderCursor,
    RunSelection,
    CancelQuery,
    ExpandLatestResult,
    CollapseResult,
    /// Hide the inline result preview (`Q` in Normal mode, or `:close`).
    /// Doesn't drop history — the latest block is still reachable via
    /// `:expand`. Auto-cleared by the next `dispatch_query`.
    DismissResult,
    ResultNav(ResultNavAction),
    /// Reorder / hide / reset the focused column in the expanded view.
    /// Local to the current grid view — re-opens reset.
    ResultColumn(ResultColumnAction),
    ResultEnterVisual,
    ResultExitVisual,
    /// `y` in the expanded view. Yanks the current cell straight to the
    /// clipboard in Normal sub-mode; switches to YankFormat (prompt) in
    /// Visual sub-mode.
    ResultYank,
    ResultYankFormat(ExportFormat),
    ResultCancelYankFormat,
    Export {
        fmt: ExportFormat,
        target: ExportTarget,
    },
    /// `:export sql [table] [path]`. The table name is optional — when
    /// absent we run source-table inference against the originating
    /// query and only fall back to an error if inference can't pin a
    /// single table.
    ExportSql {
        table: Option<String>,
        target: ExportTarget,
    },
    /// `:theme` (no args) — open the modal theme picker.
    OpenThemePicker,
    /// Cursor / confirm / cancel inside the theme picker.
    ThemePicker(ThemePickerAction),
    Worker(WorkerEvent),
    Auth(AuthAction),
    ConnForm(ConnFormAction),
    ParamsPrompt(ParamsPromptAction),
    ConnList(ConnListAction),
    OpenHelp,
    CloseHelp,
    /// Move the help popover viewport along `axis` by `delta` (a relative
    /// step) or to a named anchor (top/bottom).
    HelpScroll(HelpAxis, HelpScrollDelta),
    /// Run a slice of the editor buffer through the SQL formatter and
    /// replace it in-place. `Cursor` formats the active selection (if
    /// any) or the statement under the cursor; `All` rewrites the
    /// whole buffer.
    FormatEditor(FormatScope),
    /// Autocomplete popover lifecycle and navigation. See
    /// `CompletionAction` for the sub-variants.
    Completion(CompletionAction),
    /// User-facing `:reload`. Drops the autocomplete schema cache and
    /// re-primes from the active connection.
    ReloadSchemaCache,
    /// User-facing `:reset`. Asks the worker to roll back any open
    /// transaction on the pinned session connection and drop it; the
    /// next `execute` re-acquires a fresh connection. Autocomplete
    /// cache, results, and editor buffer are untouched.
    ResetSession,
    /// User-facing `:clear`. Empties the editor buffer, drops result
    /// blocks, and resets the pinned session connection so the
    /// fresh-start state isn't haunted by an open transaction.
    ClearSession,
    /// Re-read user + project UI prefs, the user keybindings file,
    /// and LLM provider records. Connections, crypto, the worker
    /// pool, and any in-flight query are NOT touched. Bottom bar
    /// surfaces the result.
    Source,
    /// Mouse-driven action with a panel-specific target. See [`MouseTarget`].
    Mouse(MouseTarget),
    /// Per-keystroke or scroll input directed at the chat panel.
    Chat(ChatAction),
    /// Flip the right panel between schema and chat. Also moves focus into
    /// the new right pane so the user can immediately type / navigate.
    ToggleRightPanel,
    /// Set the right panel to a specific mode (and focus into it). Used by
    /// the leader-chord bindings (`<leader> S` / `<leader> C`) which want
    /// an unambiguous "go to schema" / "go to chat" gesture, not a toggle.
    SetRightPanel(RightPanelMode),
    /// `:chat settings` modal interactions.
    LlmSettings(LlmSettingsAction),
    /// Editor-session lifecycle: `<Space>n` cycles to the next
    /// session; `:session new`/`prev`/`next`/`<N>`/`delete <N>` route
    /// through this same enum.
    Session(SessionAction),
    /// User pressed `y`/`Y`/`Enter` on an `Overlay::ConfirmToolUse`
    /// prompt — run the paused fs read tool.
    ToolApproveAccept,
    /// User pressed `n`/`N`/`Esc` on the prompt — refuse the call;
    /// the action layer replies to the LLM with `{"error": "user
    /// denied access"}` so the turn keeps moving.
    ToolApproveDeny,
    /// Saved-query overlay / picker interaction. The translation layer
    /// keeps `:save` / `:load` / `:run-saved` outside this variant
    /// (they're dispatched directly through `dispatch_command`) so this
    /// only handles the overlay key flow.
    SavedQuery(SavedQueryAction),
}

/// What a click or scroll-wheel was aimed at. Translated from
/// `crossterm::MouseEvent` by `event::translate_mouse`; consumed by
/// `apply_mouse` which routes back into the existing per-panel state
/// mutations.
#[derive(Debug)]
pub enum MouseTarget {
    /// Click landed on the editor pane. The raw event is forwarded to edtui
    /// (which handles its own mouse selection / cursor placement).
    Editor(CtEvent),
    /// Click on a row in the schema tree — selects and toggles the node
    /// (toggle is a no-op for leaves, so clicking a column just selects).
    SchemaToggle(NodeId),
    /// Scroll-wheel over the schema panel; positive scrolls down.
    SchemaScroll(i32),
    /// Mouse-down began a drag at this cell — anchor for the visual
    /// rectangle. A click that doesn't move (DragEnd with anchor==cursor)
    /// is treated as plain "select this cell" by `apply_mouse`.
    ResultDragStart { row: usize, col: usize },
    /// Drag-extend the current selection to this cell.
    ResultDragTo { row: usize, col: usize },
    /// Mouse-up released the drag.
    ResultDragEnd,
    /// Scroll-wheel over the expanded result body; positive scrolls down.
    /// Moves the viewport, not the cursor.
    ResultScroll(i32),
    /// Click on a cell in the inline preview — opens the expanded view
    /// pre-positioned at that cell.
    InlineResultJump { row: usize, col: usize },
    /// Click outside the active overlay; dismiss it.
    OverlayDismiss,
}

/// Which axis of the help popover viewport to move.
#[derive(Debug, Clone, Copy)]
pub enum HelpAxis {
    Vertical,
    Horizontal,
}

/// What kind of help-popover move to perform: a relative step or a jump
/// to a named anchor.
#[derive(Debug, Clone, Copy)]
pub enum HelpScrollDelta {
    By(i32),
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy)]
pub enum CompletionAction {
    /// Open the popover (manual `Ctrl+Space`).
    Open,
    /// Close without inserting.
    Close,
    Up,
    Down,
    /// Insert the highlighted item and close the popover.
    Accept,
}

#[derive(Debug, Clone)]
pub enum ExportTarget {
    Clipboard,
    File(PathBuf),
}

#[derive(Debug)]
pub enum AuthAction {
    Input(Input),
    /// `None` reads the system clipboard; `Some(text)` is supplied directly
    /// (bracketed paste from the terminal).
    Paste(Option<String>),
    Copy,
    Cut,
    /// Wipe the password field (`Ctrl+U`).
    ClearField,
    Submit,
    Cancel,
}

#[derive(Debug)]
pub enum ParamsPromptAction {
    Input(Input),
    Paste(Option<String>),
    Copy,
    Cut,
    /// Tab / Shift+Tab move between fields. The popup wraps around so
    /// users with one field can still hit Tab harmlessly.
    NextField,
    PrevField,
    /// Wipe the focused field (`Ctrl+U`).
    ClearField,
    Submit,
    Cancel,
}

#[derive(Debug)]
pub enum ConnFormAction {
    Input(Input),
    /// `None` reads the system clipboard; `Some(text)` is supplied directly
    /// (bracketed paste from the terminal).
    Paste(Option<String>),
    Copy,
    Cut,
    ToggleFocus,
    /// Wipe the focused field (`Ctrl+U`).
    ClearField,
    Submit,
    Cancel,
    /// Fire a one-shot connect-and-disconnect to check the URL.
    TestConnection,
}

#[derive(Debug)]
pub enum ConnListAction {
    Down,
    Up,
    Top,
    Bottom,
    UseSelected,
    AddNew,
    EditSelected,
    BeginDelete,
    ConfirmDelete,
    CancelDelete,
    Close,
}

#[derive(Debug)]
#[allow(dead_code)] // `Cancel` lights up in phase 3 once streaming exists.
pub enum ChatAction {
    /// Composer keystroke. Routed straight into the `TextArea`.
    Input(Input),
    /// `None` reads the system clipboard; `Some(text)` carries the
    /// terminal's bracketed-paste payload.
    Paste(Option<String>),
    Copy,
    Cut,
    /// Enter (no modifiers) — submits the composer's contents as a user
    /// message. Phase 2 stub appends a placeholder assistant reply; phase
    /// 3 dispatches a real LLM turn.
    Submit,
    /// Cancel an in-flight stream (no-op in phase 2; meaningful from
    /// phase 3 onward).
    Cancel,
    /// Wipe the message log and reset the composer.
    Clear,
    /// Wipe just the composer (`Ctrl+U`), leaving the message log intact.
    ClearComposer,
    ScrollUp(u16),
    ScrollDown(u16),
    /// Jump the message log to the top.
    ScrollToTop,
    /// Jump to the bottom and re-engage auto-follow.
    ScrollToBottom,
}

#[derive(Debug)]
pub enum LlmSettingsAction {
    Input(Input),
    Paste(Option<String>),
    Copy,
    Cut,
    /// Move backend selection by `±1` (left/right arrows or `[`/`]`).
    CycleBackend(i32),
    /// Tab forward through the four fields.
    CycleField,
    /// Shift+Tab backward through the four fields.
    CycleFieldBack,
    /// Wipe the focused field (`Ctrl+U`). No-op when focus is on Backend.
    ClearField,
    Submit,
    Cancel,
}

#[derive(Debug)]
pub enum CommandAction {
    Input(Input),
    /// `None` reads the system clipboard. `Some(text)` carries text supplied
    /// by the terminal's bracketed-paste mode.
    Paste(Option<String>),
    Copy,
    Cut,
    /// Wipe the command-bar input (`Ctrl+U`).
    ClearField,
    Submit,
    Cancel,
    /// Move the autocomplete popover selection by `±1`. No-op when no
    /// popover is open.
    CompletionMove(i32),
    /// Replace the in-progress command name with the highlighted
    /// candidate. When `submit` is true, the completed command is
    /// immediately executed — Tab (`false`) vs Enter (`true`).
    CompletionAccept {
        submit: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum SessionAction {
    /// Print "sessions: 0, 1 (active 0)" to the bottom bar.
    List,
    /// Cycle to the next index in `app.session_indices`, wrapping.
    Next,
    /// Cycle to the previous index, wrapping.
    Prev,
    /// Pick the lowest unused index, create the file, switch to it.
    New,
    /// Switch to a specific index. Refuses on out-of-range.
    Switch(usize),
    /// Delete the file at `index`. Refuses on the only remaining
    /// session; if the active one is deleted, falls back to the
    /// previous index in the list.
    Delete(usize),
}

#[derive(Debug, Clone, Copy)]
pub enum ThemePickerAction {
    /// Move cursor to the next theme.
    Down,
    /// Move cursor to the previous theme.
    Up,
    /// Jump to the first theme.
    Top,
    /// Jump to the last theme.
    Bottom,
    /// Apply the hovered theme and persist it to project config.
    Confirm,
    /// Discard hover-preview and restore the original theme.
    Cancel,
}

#[derive(Debug)]
pub enum SchemaAction {
    Down,
    Up,
    ExpandOrDescend,
    CollapseOrAscend,
    Toggle,
    Top,
    Bottom,
}

#[derive(Debug)]
pub enum ResultNavAction {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy)]
pub enum ResultColumnAction {
    /// Swap the focused column with the visible column to its left.
    MoveLeft,
    /// Swap the focused column with the visible column to its right.
    MoveRight,
    /// Hide the focused column. No-op when only one column is visible.
    Hide,
    /// Restore identity column order with every column visible.
    Reset,
}

pub fn apply(app: &mut App, action: Action) {
    match action {
        Action::Quit => app.should_quit = true,
        Action::FocusPanel(f) => focus_panel(app, f),
        Action::ResizeSchema(delta) => schema::resize_schema(app, delta),
        Action::SetPendingChord(c) => app.pending = c,
        Action::EditorEvent(ev) => {
            app.editor.events.on_event(ev, &mut app.editor.state);
            if app.completion.is_some() {
                completion::refresh(app);
            } else {
                completion::maybe_auto_trigger(app);
            }
            schedule_session_save(app);
        }
        Action::OpenCommand => app.overlay = Some(Overlay::Command(CommandBuffer::default())),
        Action::Command(cmd) => apply_command(app, cmd),
        Action::Schema(s) => schema::apply_schema(app, s),
        Action::PrepareConfirmRun => query::prepare_confirm_run(app),
        Action::ConfirmRunSubmit => query::confirm_run_submit(app),
        Action::ConfirmRunCancel => query::confirm_run_cancel(app),
        Action::UpdateAccept => update::apply_update_accept(app),
        Action::UpdateDismiss => update::apply_update_dismiss(app),
        Action::CheckForUpdate => update::apply_check_for_update(app),
        Action::RunStatementUnderCursor => query::run_statement_under_cursor(app),
        Action::RunSelection => query::run_selection(app),
        Action::CancelQuery => query::cancel_query(app),
        Action::ExpandLatestResult => results::expand_latest(app),
        Action::CollapseResult => app.screen = Screen::Normal,
        Action::DismissResult => results::dismiss_result(app),
        Action::ResultNav(nav) => results::apply_result_nav(app, nav),
        Action::ResultColumn(op) => results::apply_result_column(app, op),
        Action::ResultEnterVisual => results::result_enter_visual(app),
        Action::ResultExitVisual => results::result_exit_visual(app),
        Action::ResultYank => results::result_yank(app),
        Action::ResultYankFormat(fmt) => results::result_yank_format(app, fmt),
        Action::ResultCancelYankFormat => results::result_cancel_yank_format(app),
        Action::Export { fmt, target } => results::export_command(app, fmt, target),
        Action::ExportSql { table, target } => results::export_sql_command(app, table, target),
        Action::OpenThemePicker => open_theme_picker(app),
        Action::ThemePicker(a) => apply_theme_picker(app, a),
        Action::Worker(ev) => apply_worker_event(app, ev),
        Action::Auth(a) => auth::apply(app, a),
        Action::ConnForm(a) => conn_form::apply(app, a),
        Action::ParamsPrompt(a) => params_prompt::apply(app, a),
        Action::ConnList(a) => conn_list::apply(app, a),
        Action::OpenHelp => {
            app.overlay = Some(Overlay::Help {
                scroll: 0,
                h_scroll: 0,
            })
        }
        Action::CloseHelp => app.overlay = None,
        Action::HelpScroll(axis, delta) => apply_help_scroll(app, axis, delta),
        Action::FormatEditor(scope) => format_editor(app, scope),
        Action::Completion(c) => completion::apply(app, c),
        Action::ReloadSchemaCache => schema::reload_schema_cache(app),
        Action::ResetSession => session::reset_session(app),
        Action::ClearSession => session::clear_session(app),
        Action::Source => apply_source(app),
        Action::Mouse(target) => apply_mouse(app, target),
        Action::Chat(a) => chat::apply(app, a),
        Action::ToggleRightPanel => chat::toggle_right_panel(app),
        Action::SetRightPanel(mode) => chat::set_right_panel(app, mode),
        Action::LlmSettings(a) => llm_settings::apply(app, a),
        Action::Session(s) => session::dispatch_session(app, s),
        Action::ToolApproveAccept => chat::on_tool_approve_accept(app),
        Action::ToolApproveDeny => chat::on_tool_approve_deny(app),
        Action::SavedQuery(a) => saved_queries::apply(app, a),
    }
}

/// Set focus, keeping `app.right_panel` in sync. Schema/Chat/ChatComposer
/// imply a particular right-panel painting; Editor is left orthogonal so
/// `Ctrl+W h` from chat doesn't accidentally re-paint the right pane.
fn focus_panel(app: &mut App, target: Focus) {
    app.focus = target;
    match target {
        Focus::Schema => app.right_panel = RightPanelMode::Schema,
        Focus::Chat | Focus::ChatComposer => app.right_panel = RightPanelMode::Chat,
        Focus::Editor => {}
    }
}

fn apply_mouse(app: &mut App, target: MouseTarget) {
    match target {
        MouseTarget::Editor(ev) => {
            // Click/drag focuses the editor; wheel scroll just forwards
            // (mirrors how the schema/result panels scroll without stealing
            // focus). edtui handles cursor placement / selection / viewport
            // scroll from the raw event.
            if let CtEvent::Mouse(ref mev) = ev
                && matches!(mev.kind, MouseEventKind::Down(_) | MouseEventKind::Drag(_))
            {
                app.focus = Focus::Editor;
            }
            apply(app, Action::EditorEvent(ev));
        }
        MouseTarget::SchemaToggle(id) => {
            app.focus = Focus::Schema;
            schema::schema_toggle_at(app, id);
        }
        MouseTarget::SchemaScroll(delta) => {
            schema::schema_scroll(app, delta);
        }
        MouseTarget::ResultDragStart { row, col } => results::result_drag_start(app, row, col),
        MouseTarget::ResultDragTo { row, col } => results::result_drag_to(app, row, col),
        MouseTarget::ResultDragEnd => results::result_drag_end(app),
        MouseTarget::ResultScroll(delta) => results::result_scroll(app, delta),
        MouseTarget::InlineResultJump { row, col } => results::inline_result_jump(app, row, col),
        MouseTarget::OverlayDismiss => overlay_dismiss(app),
    }
}

fn overlay_dismiss(app: &mut App) {
    match &app.overlay {
        Some(Overlay::Help { .. }) => app.overlay = None,
        Some(Overlay::Command(_)) => app.overlay = None,
        // Other overlays (ConfirmRun, Connecting) intentionally don't dismiss
        // on outside-click — ConfirmRun needs an explicit yes/no to avoid
        // accidental "yes I meant to run that" via stray clicks; Connecting
        // is in-flight and dismissing it wouldn't actually cancel the work.
        _ => {}
    }
    if app.overlay.is_some() {
        return;
    }
    // Modal screens (ConnList, EditConnection, Auth) are full-screen;
    // outside-click closes them only when there's a sane place to return to.
    if matches!(app.screen, Screen::ConnectionList(_)) {
        app.screen = Screen::Normal;
    }
}

fn apply_help_scroll(app: &mut App, axis: HelpAxis, delta: HelpScrollDelta) {
    let Some(Overlay::Help { scroll, h_scroll }) = &mut app.overlay else {
        return;
    };
    let target: &mut u16 = match axis {
        HelpAxis::Vertical => scroll,
        HelpAxis::Horizontal => h_scroll,
    };
    match delta {
        HelpScrollDelta::By(n) => {
            let next = (*target as i32).saturating_add(n).max(0);
            *target = u16::try_from(next).unwrap_or(u16::MAX);
        }
        // Render-time clamping pulls these back to the actual content
        // bounds, so `u16::MAX` is the cheapest way to say "as far as
        // it'll go" without re-deriving the content size here.
        HelpScrollDelta::Top => *target = 0,
        HelpScrollDelta::Bottom => *target = u16::MAX,
    }
}

fn apply_command(app: &mut App, action: CommandAction) {
    let Some(Overlay::Command(buf)) = &mut app.overlay else {
        return;
    };
    match action {
        CommandAction::Input(input) => {
            let _ = buf.input.input(input);
            buf.recompute_completion();
        }
        CommandAction::Paste(text) => {
            paste_into(&mut buf.input, &app.log, text);
            buf.recompute_completion();
        }
        CommandAction::Copy => copy_from(&mut buf.input, &app.log),
        CommandAction::Cut => {
            cut_from(&mut buf.input, &app.log);
            buf.recompute_completion();
        }
        CommandAction::ClearField => {
            buf.input.clear();
            buf.recompute_completion();
        }
        CommandAction::Cancel => app.overlay = None,
        CommandAction::Submit => submit_command(app),
        CommandAction::CompletionMove(delta) => {
            if let Some(c) = &mut buf.completion {
                c.move_selection(delta);
            }
        }
        CommandAction::CompletionAccept { submit } => {
            if let Some(c) = &buf.completion
                && let Some(name) = c.current()
            {
                buf.accept_completion(name);
                if submit {
                    submit_command(app);
                }
            }
        }
    }
}

fn submit_command(app: &mut App) {
    let Some(Overlay::Command(buf)) = &app.overlay else {
        return;
    };
    let raw = buf.text().trim().to_string();
    app.overlay = None;
    // NOTE: any command parsed in `crate::command` MUST also be listed in
    // the `:help` popover. See `HELP_SECTIONS` in `src/ui/help_view.rs`.
    match command::parse(&raw) {
        Ok(None) => {}
        Ok(Some(cmd)) => dispatch_command(app, cmd),
        Err(error) => app.status = QueryStatus::Failed { error },
    }
}

/// Re-read the user + project configs and the user keybindings file.
/// Connections, crypto, the active session, and the worker pool are
/// untouched. On any parse error, the previously-active state is
/// preserved (whole-load rollback per plan B.4).
fn apply_source(app: &mut App) {
    use crate::config::ConfigStore;
    use crate::keybindings;
    use crate::keybindings::keymap::Keymap;
    use crate::state::focus::PendingChord;
    use crate::user_config::{UserConfigStore, user_data_dir};

    // Reset any partially-armed chord BEFORE swapping the keymap so a
    // mid-chord `:source` does not interpret the next keystroke
    // against the new keymap (R.7 in the plan).
    app.pending = PendingChord::None;

    let user_dir_opt = user_data_dir();
    let new_user = match &user_dir_opt {
        Some(dir) => match UserConfigStore::load(dir) {
            Ok(s) => s,
            Err(e) => {
                app.status = QueryStatus::Failed {
                    error: format!(":source user config: {e}"),
                };
                return;
            }
        },
        None => UserConfigStore::empty(std::path::Path::new(".")),
    };

    let new_project = match ConfigStore::load(&app.data_dir) {
        Ok(s) => s,
        Err(e) => {
            app.status = QueryStatus::Failed {
                error: format!(":source project config: {e}"),
            };
            return;
        }
    };

    // Keymap reload — soft failure (keep current keymap on parse error
    // so the user does not lose their working overrides mid-session).
    let (new_keymap, keymap_err) = match user_dir_opt.as_deref() {
        Some(dir) => match keybindings::load(dir) {
            Ok(file) => {
                let mut m = Keymap::defaults();
                match m.merge_overrides(&file) {
                    Ok(()) => (std::sync::Arc::new(m), None),
                    Err(e) => (app.keymap.clone(), Some(format!("keybindings.toml: {e}"))),
                }
            }
            Err(e) => (app.keymap.clone(), Some(format!("keybindings.toml: {e}"))),
        },
        None => (std::sync::Arc::new(Keymap::defaults()), None),
    };

    let theme_name = crate::user_config::effective_theme(
        new_project.state().theme.as_deref(),
        new_user.state().theme.as_deref(),
    );
    let width = crate::user_config::effective_schema_width(
        new_project.state().schema_width,
        new_user.state().schema_width,
        crate::app::DEFAULT_SCHEMA_WIDTH,
    );
    app.theme = crate::ui::theme::Theme::by_name(&theme_name)
        .unwrap_or_else(|| crate::ui::theme::Theme::for_kind(crate::ui::theme::ThemeKind::Dark));
    app.schema.width = width;

    app.config = new_project;
    app.user_config = new_user;
    app.keymap = new_keymap;

    // Reset the AGENTS.md cache and re-seed against `project_root`.
    // Subdirectory AGENTS.md files discovered earlier this session
    // are dropped — they'll be re-loaded the next time the agent
    // reads from those subdirs. Only the root file is checked here
    // since `:source` is a "reset point" and we don't walk anywhere
    // else proactively. Each freshly-loaded file gets a system-role
    // notice in the chat history so the user sees confirmation
    // alongside the existing status-line token.
    let newly_loaded = {
        let mut cache = app.agents_md.write().unwrap();
        cache.clear();
        cache.seed_root(&app.project_root, &app.log)
    };
    for path in &newly_loaded {
        app.chat
            .push_message(crate::state::chat::ChatMessage::system_text(format!(
                "Loaded AGENTS.md ({path})"
            )));
    }
    let agents_md_loaded = !newly_loaded.is_empty();

    app.status = match keymap_err {
        Some(err) => QueryStatus::Failed { error: err },
        None => {
            let mut parts: Vec<&str> = vec!["user config", "project config", "keybindings"];
            if agents_md_loaded {
                parts.push("AGENTS.md");
            }
            QueryStatus::Notice {
                msg: format!("sourced {}", parts.join(" + ")),
            }
        }
    };
}

fn dispatch_command(app: &mut App, cmd: command::Command) {
    use command::Command as C;
    match cmd {
        C::Quit => app.should_quit = true,
        C::Help => apply(app, Action::OpenHelp),
        C::SetSchemaWidth(w) => schema::set_schema_width(app, w),
        C::Run => apply(app, Action::RunStatementUnderCursor),
        C::Cancel => apply(app, Action::CancelQuery),
        C::Expand => apply(app, Action::ExpandLatestResult),
        C::Collapse => apply(app, Action::CollapseResult),
        C::CloseResult => apply(app, Action::DismissResult),
        C::Theme(ThemeChoice::OpenPicker) => apply(app, Action::OpenThemePicker),
        C::Theme(ThemeChoice::Set(name)) => apply_theme_named(app, &name),
        C::Export { fmt, target } => apply(
            app,
            Action::Export {
                fmt,
                target: resolve_target(target),
            },
        ),
        C::ExportSql { table, target } => apply(
            app,
            Action::ExportSql {
                table,
                target: resolve_target(target),
            },
        ),
        C::Format(scope) => apply(app, Action::FormatEditor(scope)),
        C::Reload => apply(app, Action::ReloadSchemaCache),
        C::Reset => apply(app, Action::ResetSession),
        C::Clear => apply(app, Action::ClearSession),
        C::Source => apply(app, Action::Source),
        C::Conn(sub) => dispatch_conn(app, sub),
        C::Chat(sub) => dispatch_chat(app, sub),
        C::Session(sub) => apply(
            app,
            Action::Session(session::session_subcommand_to_action(sub)),
        ),
        C::Update => apply(app, Action::CheckForUpdate),
        C::Save(name) => saved_queries::apply_save(app, name),
        C::Load(name) => saved_queries::apply_load(app, name),
        C::RunSaved(Some(name)) => saved_queries::apply_run_saved(app, name),
        C::RunSaved(None) => saved_queries::open_run_picker(app),
    }
}

fn dispatch_chat(app: &mut App, sub: ChatSubcommand) {
    match sub {
        ChatSubcommand::Toggle => apply(app, Action::ToggleRightPanel),
        ChatSubcommand::Clear => apply(app, Action::Chat(ChatAction::Clear)),
        ChatSubcommand::Settings => llm_settings::open(app),
    }
}

fn dispatch_conn(app: &mut App, sub: ConnSubcommand) {
    match sub {
        ConnSubcommand::List => open_conn_list(app),
        ConnSubcommand::Add(name) => open_conn_form_create(app, name.as_deref()),
        ConnSubcommand::Edit(name) => {
            open_conn_form_edit(app, &name, ConnFormPostSave::ReturnToList)
        }
        ConnSubcommand::Delete(name) => perform_delete(app, &name),
        ConnSubcommand::Use(name) => use_connection(app, &name),
    }
}

fn resolve_target(t: ParsedTarget) -> ExportTarget {
    match t {
        ParsedTarget::Clipboard => ExportTarget::Clipboard,
        ParsedTarget::File(path) => ExportTarget::File(expand_tilde(&path)),
    }
}

fn open_conn_list(app: &mut App) {
    let entries = app.config.connection_names();
    if entries.is_empty() {
        // Nothing to list — bounce straight to the create form so the user
        // doesn't get an empty modal and have to type `:conn add` next.
        app.screen = Screen::EditConnection(
            ConnFormState::new_create().with_post_save(ConnFormPostSave::ReturnToList),
        );
        return;
    }
    let mut state = ConnListState::new(entries);
    if let Some(active) = &app.active_connection
        && let Some(idx) = state.entries.iter().position(|n| n == active)
    {
        state.selected = idx;
    }
    app.screen = Screen::ConnectionList(state);
}

fn open_conn_form_create(app: &mut App, name: Option<&str>) {
    let mut form = ConnFormState::new_create().with_post_save(ConnFormPostSave::ReturnToList);
    if let Some(n) = name {
        form = form.with_prefilled_name(n);
    }
    app.screen = Screen::EditConnection(form);
}

pub(super) fn open_conn_form_edit(app: &mut App, name: &str, post_save: ConnFormPostSave) {
    let entry = match app.config.connection(name).cloned() {
        Some(e) => e,
        None => {
            app.status = QueryStatus::Failed {
                error: format!("no connection named {name:?}"),
            };
            return;
        }
    };
    let store = match app.connection_store.as_ref() {
        Some(s) => s,
        None => {
            app.status = QueryStatus::Failed {
                error: "internal: no connection store available".into(),
            };
            return;
        }
    };
    let url = match store.lookup(&entry) {
        Ok(s) => s.to_string(),
        Err(err) => {
            app.status = QueryStatus::Failed {
                error: format!("decrypt {name:?} failed: {err}"),
            };
            return;
        }
    };
    app.screen = Screen::EditConnection(
        ConnFormState::editing(name.to_string(), url).with_post_save(post_save),
    );
}

pub(super) fn perform_delete(app: &mut App, name: &str) {
    if Some(name) == app.active_connection.as_deref() {
        app.status = QueryStatus::Failed {
            error: format!("{name:?} is the active connection — :conn use another first"),
        };
        return;
    }
    match app.config.delete_connection(name) {
        Ok(true) => {
            app.log.info("conn", format!("deleted connection {name}"));
            app.status = QueryStatus::Idle;
        }
        Ok(false) => {
            app.status = QueryStatus::Failed {
                error: format!("no connection named {name:?}"),
            };
        }
        Err(err) => {
            app.status = QueryStatus::Failed {
                error: format!("delete failed: {err}"),
            };
        }
    }
}

pub(super) fn use_connection(app: &mut App, name: &str) {
    if Some(name) == app.active_connection.as_deref() {
        app.status = QueryStatus::Failed {
            error: format!("{name:?} is already active"),
        };
        return;
    }
    let entry = match app.config.connection(name).cloned() {
        Some(e) => e,
        None => {
            app.status = QueryStatus::Failed {
                error: format!("no connection named {name:?}"),
            };
            return;
        }
    };
    let store = match app.connection_store.as_ref() {
        Some(s) => s,
        None => {
            app.status = QueryStatus::Failed {
                error: "internal: no connection store available".into(),
            };
            return;
        }
    };
    let url = match store.lookup(&entry) {
        Ok(s) => s.to_string(),
        Err(err) => {
            app.status = QueryStatus::Failed {
                error: format!("decrypt {name:?} failed: {err}"),
            };
            return;
        }
    };
    dispatch_connect(app, name.to_string(), url);
}

/// Resolve `:theme <name>` against the bundled registry. Unknown names
/// surface as a status-bar error rather than aborting the command loop.
fn apply_theme_named(app: &mut App, name: &str) {
    let Some(theme) = Theme::by_name(name) else {
        app.status = QueryStatus::Failed {
            error: format!("unknown theme: {name}"),
        };
        return;
    };
    app.theme = theme;
    if let Err(err) = app.config.set_theme(name) {
        app.log.warn("config", format!("save theme failed: {err}"));
    }
}

/// Open the modal theme picker (bare `:theme`). Pre-selects the
/// currently persisted theme so the user lands on the row they expect.
fn open_theme_picker(app: &mut App) {
    let current = app
        .config
        .state()
        .theme
        .clone()
        .unwrap_or_else(|| crate::user_config::DEFAULT_THEME_NAME.to_string());
    let state = ThemePickerState::new(&current);
    app.screen = Screen::ThemePicker(state);
}

/// Dispatch picker key actions. Hover (Up/Down/Top/Bottom) live-previews
/// the theme so the whole window changes; Confirm persists; Cancel
/// restores the original theme.
fn apply_theme_picker(app: &mut App, action: ThemePickerAction) {
    let Screen::ThemePicker(state) = &mut app.screen else {
        return;
    };
    match action {
        ThemePickerAction::Down => state.move_down(),
        ThemePickerAction::Up => state.move_up(),
        ThemePickerAction::Top => state.top(),
        ThemePickerAction::Bottom => state.bottom(),
        ThemePickerAction::Confirm => {
            let Some(name) = state.selected().map(|i| i.name.clone()) else {
                app.screen = Screen::Normal;
                return;
            };
            apply_theme_named(app, &name);
            app.screen = Screen::Normal;
            return;
        }
        ThemePickerAction::Cancel => {
            let original = state.original_theme_name.clone();
            if let Some(theme) = Theme::by_name(&original) {
                app.theme = theme;
            }
            app.screen = Screen::Normal;
            return;
        }
    }
    // Hover: live-preview the hovered theme.
    if let Some(theme) = state.hovered_theme() {
        app.theme = theme;
    }
}

fn apply_worker_event(app: &mut App, event: WorkerEvent) {
    match event {
        WorkerEvent::QueryDone { req, result } => query::on_query_done(app, req, result),
        WorkerEvent::QueryFailed { req, error } => {
            query::on_query_failed(app, req, error.to_string())
        }
        WorkerEvent::SchemaLoaded { target, payload } => {
            schema::on_schema_loaded(app, target, payload)
        }
        WorkerEvent::SchemaFailed { target, error } => {
            schema::on_schema_failed(app, target, error.to_string())
        }
        WorkerEvent::Connected { name } => on_connected(app, name),
        WorkerEvent::ConnectFailed { name, error } => {
            on_connect_failed(app, name, error.to_string())
        }
        WorkerEvent::TestConnectionResult { success, error, .. } => {
            on_test_result(app, success, error)
        }
        WorkerEvent::CompletionCacheStage { stage } => schema::on_cache_stage(app, stage),
        WorkerEvent::CompletionCacheFailed { stage, error } => {
            schema::on_cache_failed(app, stage, error.to_string())
        }
        WorkerEvent::ChatDelta(delta) => chat::on_delta(app, delta),
        WorkerEvent::ChatToolRequest {
            call_id,
            name,
            args_json,
            reply,
        } => chat::on_tool_request(app, call_id, name, args_json, reply),
        WorkerEvent::ChatFsToolDone {
            call_id,
            name,
            display,
            error,
            agents_md_loaded,
        } => chat::on_fs_tool_done(app, call_id, name, display, error, agents_md_loaded),
        WorkerEvent::UpdateAvailable { current, latest } => {
            update::on_update_available(app, current, latest)
        }
        WorkerEvent::UpdateInstalled { tag } => update::on_update_installed(app, tag),
        WorkerEvent::UpdateInstallFailed { error } => update::on_update_install_failed(app, error),
        WorkerEvent::UpdateUpToDate { current } => update::on_update_up_to_date(app, current),
        WorkerEvent::UpdateCheckFailed { error } => update::on_update_check_failed(app, error),
    }
}

fn on_connected(app: &mut App, name: String) {
    // Only react if we're still expecting this connection. A late event from
    // an aborted swap would otherwise clobber the active connection.
    let expected =
        matches!(&app.overlay, Some(Overlay::Connecting { name: pending }) if pending == &name);
    if !expected {
        return;
    }
    app.active_connection = Some(name.clone());
    app.overlay = None;
    app.screen = Screen::Normal;
    app.status = QueryStatus::Idle;
    // The previous connection's reload (if any) won't deliver
    // `Reloaded` against this new pool — drop the flag so the first
    // DDL here can reprime.
    app.schema_reload_in_flight = false;
    // Fresh tree — drop any nodes left over from the previous connection
    // and re-fire the catalog load.
    app.schema = SchemaPanel::new(app.schema.width);
    app.results.clear();
    app.session_indices = crate::session::list_indices(&app.data_dir, &name);
    // `list_indices` always returns at least `[0]`, so the unwrap-by-index
    // is safe; default to session 0 on connect.
    app.active_session_index = app.session_indices[0];
    session::load_session(app, &name, app.active_session_index);
    session::load_chat_session(app, &name);
    app.schema.begin_root_load();
    let _ = app.cmd_tx.send(WorkerCommand::Introspect {
        target: IntrospectTarget::Catalogs,
    });
    // Kick off the autocomplete cache prime — runs in the background;
    // popover opens before it finishes will see whatever's already
    // landed (keywords always work).
    let _ = app.cmd_tx.send(WorkerCommand::PrimeCompletionCache {
        connection: name.clone(),
    });
    app.log.info("app", format!("connected to {name}"));
}

fn on_connect_failed(app: &mut App, name: String, error: String) {
    let was_pending =
        matches!(&app.overlay, Some(Overlay::Connecting { name: pending }) if pending == &name);
    if !was_pending {
        return;
    }
    app.log
        .warn("app", format!("connect failed for {name}: {error}"));
    // Either way, the in-flight connect is over — the spinner clears.
    app.overlay = None;

    // Live switch (`:conn use`) — the previous datasource is still alive in
    // the worker, so just surface the error and leave the underlying screen
    // alone (typically Normal).
    if app.active_connection.is_some() {
        app.screen = Screen::Normal;
        app.status = QueryStatus::Failed {
            error: format!("connect to {name} failed: {error}"),
        };
        return;
    }

    // Initial connect — re-open the form pre-filled so the user can fix
    // the URL and retry without losing what they typed.
    let entry = app.config.connection(&name).cloned();
    let store = app.connection_store.as_ref();
    let prefill_url = match (entry, store) {
        (Some(entry), Some(store)) => store.lookup(&entry).ok().map(|s| s.to_string()),
        _ => None,
    };
    match prefill_url {
        Some(url) => {
            let mut form = ConnFormState::editing(name.clone(), url);
            form.error = Some(format!("connect failed: {error}"));
            app.screen = Screen::EditConnection(form);
        }
        None => {
            app.screen = Screen::Normal;
            app.status = QueryStatus::Failed {
                error: format!("connect to {name} failed: {error}"),
            };
        }
    }
}

/// Update the connection form with the test-connect result.
fn on_test_result(app: &mut App, success: bool, error: Option<String>) {
    let Screen::EditConnection(state) = &mut app.screen else {
        return;
    };
    state.testing = false;
    state.test_result = Some(if success {
        crate::state::conn_form::TestResult::Success
    } else {
        crate::state::conn_form::TestResult::Failure(
            error.unwrap_or_else(|| "unknown error".into()),
        )
    });
    if success {
        app.log.info("conn", "test connection succeeded");
    }
}

// ---------------------------------------------------------------------------
// Auth flow
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Connection-form flow
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Connection list
// ---------------------------------------------------------------------------

pub(crate) fn dispatch_connect(app: &mut App, name: String, url: String) {
    // If we're swapping connections, persist the current session before the
    // editor's contents get replaced by the next `Connected` event.
    flush_session(app);
    // Snapshot the dialect off the URL up front. Result blocks created by
    // this connection will pin to it; if the connect fails before any rows
    // come back, the stale value is harmless (no result block uses it).
    app.active_dialect = crate::datasource::DriverKind::from_url(&url);
    // Drop the previous connection's autocomplete cache so a popover that
    // opens before the new prime lands can't show stale tables.
    if let Ok(mut cache) = app.schema_cache.write() {
        cache.clear();
    }
    app.completion = None;
    app.overlay = Some(Overlay::Connecting { name: name.clone() });
    app.status = QueryStatus::Idle;
    let _ = app.cmd_tx.send(WorkerCommand::Connect { name, url });
}

// ---------------------------------------------------------------------------
// Clipboard helpers (shared across every TextArea-backed input)
// ---------------------------------------------------------------------------

pub(super) fn paste_into(
    input: &mut TextArea<'static>,
    log: &crate::log::Logger,
    supplied: Option<String>,
) {
    let text = match supplied {
        Some(t) => t,
        None => match clipboard::read(log) {
            Some(t) => t,
            None => return,
        },
    };
    let _ = input.insert_str(text);
}

pub(super) fn copy_from(input: &mut TextArea<'static>, log: &crate::log::Logger) {
    // No-op when nothing is selected — TextArea's `copy()` would just no-op
    // anyway, but we don't want to clobber the system clipboard with
    // whatever's left in the yank buffer.
    if input.selection_range().is_none() {
        return;
    }
    input.copy();
    let text = input.yank_text();
    clipboard::write(log, &text);
}

pub(super) fn cut_from(input: &mut TextArea<'static>, log: &crate::log::Logger) {
    if input.selection_range().is_none() {
        return;
    }
    let did_cut = input.cut();
    if did_cut {
        clipboard::write(log, &input.yank_text());
    }
}

/// Run the SQL formatter against a slice of the editor buffer and
/// rewrite it in-place. Sets a status notice on success so the user sees
/// the command landed even when the visible diff is just whitespace.
///
/// `Cursor` mirrors how `r` picks what to run: a Visual selection wins,
/// otherwise we format the statement containing the cursor.
/// `All` rewrites the whole buffer, used by `:format all`.
fn format_editor(app: &mut App, scope: FormatScope) {
    match scope {
        FormatScope::Cursor => format_at_cursor(app),
        FormatScope::All => format_buffer(app),
    }
}

fn format_at_cursor(app: &mut App) {
    if let Some(sel) = crate::state::editor::selection_text(&app.editor.state) {
        let formatted = format_sql(&sel);
        if crate::state::editor::replace_selection_text(&mut app.editor.state, &formatted) {
            app.status = QueryStatus::Notice {
                msg: "formatted selection".into(),
            };
            schedule_session_save(app);
            return;
        }
    }
    let Some(range) = crate::state::editor::statement_under_cursor(&app.editor.state) else {
        app.status = QueryStatus::Failed {
            error: "no statement under cursor".into(),
        };
        return;
    };
    // Trim so we don't smuggle a trailing newline back in front of the `;`.
    let formatted = format_sql(&range.text).trim().to_string();
    if crate::state::editor::replace_statement_under_cursor(&mut app.editor.state, &formatted) {
        app.status = QueryStatus::Notice {
            msg: "formatted statement".into(),
        };
        schedule_session_save(app);
    }
}

fn format_buffer(app: &mut App) {
    let buffer = app.editor.text();
    if buffer.trim().is_empty() {
        app.status = QueryStatus::Failed {
            error: "buffer is empty".into(),
        };
        return;
    }
    let formatted = format_sql(&buffer);
    crate::state::editor::replace_buffer_text(&mut app.editor.state, &formatted);
    app.status = QueryStatus::Notice {
        msg: "formatted buffer".into(),
    };
    schedule_session_save(app);
}

fn format_sql(sql: &str) -> String {
    sqlformat::format(
        sql,
        &sqlformat::QueryParams::None,
        &sqlformat::FormatOptions::default(),
    )
}

/// Expand a leading `~` / `~/` to `$HOME`. Anything else (including the
/// `~user` form, which would need /etc/passwd) is passed through untouched.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::results::apply_nav_step;
    use super::*;
    use crate::state::results::ResultCursor;

    #[test]
    fn expand_tilde_substitutes_home() {
        // SAFETY: tests run single-threaded by default in this crate; no
        // other thread is racing on `HOME` here.
        unsafe {
            std::env::set_var("HOME", "/home/test-user");
        }
        assert_eq!(expand_tilde("~"), PathBuf::from("/home/test-user"));
        assert_eq!(
            expand_tilde("~/exports/foo.csv"),
            PathBuf::from("/home/test-user/exports/foo.csv")
        );
        assert_eq!(
            expand_tilde("/abs/path.csv"),
            PathBuf::from("/abs/path.csv")
        );
        assert_eq!(
            expand_tilde("relative/path.csv"),
            PathBuf::from("relative/path.csv")
        );
        // A literal `~` inside a name (no slash) is left alone.
        assert_eq!(expand_tilde("~foo/bar"), PathBuf::from("~foo/bar"));
    }

    // ---- :source tests (US-010) ----

    /// Returned receivers are held by the caller for the duration of
    /// the test so the channels do not appear closed under `is_closed`.
    fn fixture_app(
        data_dir: PathBuf,
    ) -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<crate::worker::WorkerCommand>,
        tokio::sync::mpsc::UnboundedReceiver<crate::worker::WorkerEvent>,
    ) {
        use crate::app::App;
        use crate::autocomplete::SchemaCache;
        use crate::config::ConfigStore;
        use crate::keybindings::keymap::Keymap;
        use crate::log::Logger;
        use crate::user_config::UserConfigStore;
        use std::sync::{Arc, RwLock};
        use tokio::sync::mpsc;

        std::fs::create_dir_all(&data_dir).unwrap();
        let logger = Logger::open(&data_dir.join("test.log")).unwrap();
        let config = ConfigStore::load(&data_dir).unwrap();
        let user_config = UserConfigStore::empty(&data_dir);
        let keymap = Arc::new(Keymap::defaults());
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel();
        let schema_cache = Arc::new(RwLock::new(SchemaCache::new()));
        let app = App::new(
            cmd_tx,
            evt_tx,
            config,
            user_config,
            keymap,
            None,
            logger,
            data_dir,
            schema_cache,
        );
        (app, cmd_rx, evt_rx)
    }

    fn temp_dir(label: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rowdy-source-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn source_preserves_connection_store_handle() {
        let dir = temp_dir("conn");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir.clone());
        // Seed with a plaintext connection store so the Arc has a
        // discoverable identity. (App.connection_store is `Option`, not
        // Arc, so we compare by mem::discriminant + a marker field.)
        app.connection_store = Some(crate::connections::ConnectionStore::plaintext());
        app.llm_keystore = Some(crate::llm::keystore::LlmKeyStore::plaintext());
        let conn_was_some = app.connection_store.is_some();
        let llm_was_some = app.llm_keystore.is_some();
        let active = app.active_connection.clone();

        super::apply_source(&mut app);

        assert_eq!(app.connection_store.is_some(), conn_was_some);
        assert_eq!(app.llm_keystore.is_some(), llm_was_some);
        assert_eq!(app.active_connection, active);
        // PendingChord reset (R.7).
        assert_eq!(app.pending, crate::state::focus::PendingChord::None);
    }

    #[test]
    fn source_keeps_keymap_arc_when_keybindings_file_malformed() {
        let dir = temp_dir("keymap-rollback");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir.clone());
        // Override HOME so the user-config path lives in a tempdir.
        let user_home = temp_dir("home");
        std::fs::create_dir_all(user_home.join(".rowdy")).unwrap();
        std::fs::write(
            user_home.join(".rowdy").join("keybindings.toml"),
            // Valid TOML, invalid action ID.
            "[leader]\nr = \"no-such-action\"\n",
        )
        .unwrap();
        let before = std::sync::Arc::clone(&app.keymap);
        // SAFETY: tests run single-threaded by default; matches existing
        // precedent at src/action/mod.rs `expand_tilde_substitutes_home`.
        unsafe {
            std::env::set_var("HOME", &user_home);
        }

        super::apply_source(&mut app);

        // Whole-load rollback: same Arc instance.
        assert!(
            std::sync::Arc::ptr_eq(&before, &app.keymap),
            "apply_source must keep the previous keymap on parse error"
        );
        // Bottom bar shows the error.
        match &app.status {
            crate::state::status::QueryStatus::Failed { error } => {
                assert!(error.contains("keybindings.toml"), "got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn nav_up_down_preserves_column() {
        // Regression: Up/Down used to call `move_in(±1, 0, max_rows, 1)`,
        // which clamps `col` to `0..1` and snapped the cursor to col 0
        // on every vertical move regardless of the visible permutation.
        let mut cursor = ResultCursor { row: 5, col: 3 };
        let visible = vec![0, 1, 2, 3, 4];

        apply_nav_step(&mut cursor, ResultNavAction::Up, 10, &visible);
        assert_eq!((cursor.row, cursor.col), (4, 3));

        apply_nav_step(&mut cursor, ResultNavAction::Down, 10, &visible);
        assert_eq!((cursor.row, cursor.col), (5, 3));

        // At the top: Up is a no-op, col still preserved.
        cursor = ResultCursor { row: 0, col: 2 };
        apply_nav_step(&mut cursor, ResultNavAction::Up, 10, &visible);
        assert_eq!((cursor.row, cursor.col), (0, 2));

        // At the bottom: Down is a no-op, col still preserved.
        cursor = ResultCursor { row: 9, col: 2 };
        apply_nav_step(&mut cursor, ResultNavAction::Down, 10, &visible);
        assert_eq!((cursor.row, cursor.col), (9, 2));

        // Reordered visible permutation: physical col 4 stays put on Up.
        cursor = ResultCursor { row: 3, col: 4 };
        let reordered = vec![2, 4, 0, 1, 3];
        apply_nav_step(&mut cursor, ResultNavAction::Up, 10, &reordered);
        assert_eq!((cursor.row, cursor.col), (2, 4));
    }

    #[test]
    fn pending_update_prompt_is_deferred_during_startup_overlay() {
        // Regression: the auto-update WorkerEvent landing while
        // Overlay::Connecting is up used to be silently dropped, and
        // landing while Screen::Auth was up used to steal keystrokes
        // from the password prompt. Both cases must defer.
        use crate::state::overlay::Overlay;
        use crate::state::screen::Screen;
        let dir = temp_dir("update-defer");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir);

        // Simulate startup: Connecting overlay is up.
        app.overlay = Some(Overlay::Connecting {
            name: "main".into(),
        });
        super::update::on_update_available(&mut app, "0.7.0".into(), "0.7.1".into());
        assert!(
            matches!(app.overlay, Some(Overlay::Connecting { .. })),
            "Connecting overlay must not be replaced",
        );
        assert_eq!(
            app.pending_update_prompt.as_ref(),
            Some(&("0.7.0".to_string(), "0.7.1".to_string())),
            "update event must be stashed, not dropped",
        );

        // Promotion is a no-op while the overlay is still active.
        super::try_promote_pending_update(&mut app);
        assert!(matches!(app.overlay, Some(Overlay::Connecting { .. })));
        assert!(app.pending_update_prompt.is_some());

        // Once the user reaches Normal with no overlay, promotion fires.
        app.overlay = None;
        app.screen = Screen::Normal;
        super::try_promote_pending_update(&mut app);
        assert!(
            matches!(app.overlay, Some(Overlay::UpdateAvailable { .. })),
            "expected promotion to UpdateAvailable, got {:?}",
            app.overlay,
        );
        assert!(app.pending_update_prompt.is_none(), "pending must clear");
    }

    #[test]
    fn pending_update_prompt_does_not_capture_password_screen() {
        use crate::state::auth::AuthState;
        use crate::state::screen::Screen;
        let dir = temp_dir("update-auth");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir);

        app.screen = Screen::Auth(AuthState::new(crate::state::auth::AuthKind::FirstSetup));
        super::update::on_update_available(&mut app, "0.7.0".into(), "0.7.1".into());
        super::try_promote_pending_update(&mut app);

        // Auth screen must keep its keyboard input — overlay must NOT
        // be set to UpdateAvailable while screen != Normal.
        assert!(app.overlay.is_none(), "auth screen must not be preempted");
        assert!(app.pending_update_prompt.is_some(), "still pending");
    }

    #[test]
    fn source_does_not_disturb_running_query_status() {
        let dir = temp_dir("running");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir.clone());
        app.status = crate::state::status::QueryStatus::Running {
            query: "SELECT 1".into(),
            started_at: std::time::Instant::now(),
        };

        super::apply_source(&mut app);

        // After a successful :source the status becomes a Notice; the
        // running query itself (in_flight_query / worker pool) is
        // untouched. The AC requires us to assert that the in-flight
        // query state and worker pool are not stomped — those live on
        // `in_flight_query` / `cmd_tx`, not on `status`. The bottom-bar
        // `status` field IS allowed to flip to Notice/Failed because
        // that's how :source surfaces its own outcome.
        assert!(app.in_flight_query.is_none()); // fixture didn't seed one
        // Worker channel still alive (no Close was sent).
        assert!(!app.cmd_tx.is_closed());
    }

    #[test]
    fn open_theme_picker_sets_screen_to_picker() {
        let dir = temp_dir("theme-open");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir);
        super::open_theme_picker(&mut app);
        assert!(matches!(app.screen, Screen::ThemePicker(_)));
    }

    #[test]
    fn theme_picker_confirm_persists_to_project_config() {
        let dir = temp_dir("theme-confirm");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir.clone());
        super::open_theme_picker(&mut app);
        // Move cursor to "light" deterministically.
        if let Screen::ThemePicker(state) = &mut app.screen {
            let idx = state
                .items
                .iter()
                .position(|i| i.name == "light")
                .expect("light theme bundled");
            state.cursor = idx;
        } else {
            panic!("picker not open");
        }
        super::apply_theme_picker(&mut app, ThemePickerAction::Confirm);
        assert!(matches!(app.screen, Screen::Normal));
        assert_eq!(app.config.state().theme.as_deref(), Some("light"));
    }

    #[test]
    fn theme_picker_cancel_restores_original_theme() {
        let dir = temp_dir("theme-cancel");
        let (mut app, _cmd_rx, _evt_rx) = fixture_app(dir);
        // Pin a known starting theme so the original-restore is observable.
        super::apply_theme_named(&mut app, "dark");
        let original = app.theme.bg;
        super::open_theme_picker(&mut app);
        // Hover something else — preview mutates app.theme.
        super::apply_theme_picker(&mut app, ThemePickerAction::Down);
        super::apply_theme_picker(&mut app, ThemePickerAction::Down);
        super::apply_theme_picker(&mut app, ThemePickerAction::Cancel);
        assert!(matches!(app.screen, Screen::Normal));
        assert_eq!(app.theme.bg, original, "cancel must restore original theme");
    }
}
