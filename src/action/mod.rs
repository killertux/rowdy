use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::Event as CtEvent;
use ratatui_textarea::{Input, TextArea};

mod auth;
mod chat;
mod completion;
mod conn_form;
mod conn_list;
mod llm_settings;

use crate::app::{App, MAX_SCHEMA_WIDTH, MIN_SCHEMA_WIDTH};
use crate::clipboard;
use crate::command::{
    self, ChatSubcommand, ConnSubcommand, FormatScope, ParsedTarget, ThemeChoice,
};
use crate::datasource::{Cell, Column, QueryResult};
use crate::export::{self, ExportFormat};
use crate::session;
use crate::state::command::CommandBuffer;
use crate::state::conn_form::{ConnFormPostSave, ConnFormState};
use crate::state::conn_list::ConnListState;
use crate::state::focus::{Focus, PendingChord};
use crate::state::layout::DragState;
use crate::state::overlay::Overlay;
use crate::state::results::{ResultBlock, ResultCursor, ResultId, ResultViewMode, SelectionRect};
use crate::state::schema::{ExpandOutcome, NodeId, SchemaPanel};
use crate::state::screen::Screen;
use crate::state::status::QueryStatus;
use crate::ui::theme::{Theme, ThemeKind};
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
    RunStatementUnderCursor,
    RunSelection,
    CancelQuery,
    ExpandLatestResult,
    CollapseResult,
    ResultNav(ResultNavAction),
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
    ToggleTheme,
    Worker(WorkerEvent),
    Auth(AuthAction),
    ConnForm(ConnFormAction),
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
    /// Mouse-driven action with a panel-specific target. See [`MouseTarget`].
    Mouse(MouseTarget),
    /// Per-keystroke or scroll input directed at the chat panel.
    Chat(ChatAction),
    /// Flip the right panel between schema and chat. Also moves focus into
    /// the new right pane so the user can immediately type / navigate.
    ToggleRightPanel,
    /// `:chat settings` modal interactions.
    LlmSettings(LlmSettingsAction),
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
    /// Click on a row in the schema tree.
    SchemaRow(NodeId),
    /// Toggle (or first-expand) the given schema node.
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
    Submit,
    Cancel,
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
    ScrollUp(u16),
    ScrollDown(u16),
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
    Submit,
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

/// Snap the schema selection to a specific node and toggle/expand it.
/// `select` only moves the selection; `toggle` does both. Helpers exist
/// in the apply layer so the mouse-click and chevron-click paths share a
/// canonical implementation.
fn schema_select(app: &mut App, id: NodeId) {
    app.schema.selected = Some(id);
}

fn schema_toggle_at(app: &mut App, id: NodeId) {
    app.schema.selected = Some(id);
    let outcome = app.schema.toggle_selected();
    maybe_dispatch(app, outcome);
}

fn schema_scroll(app: &mut App, delta: i32) {
    let total = app.schema.visible_rows().len();
    if total == 0 {
        return;
    }
    let max_offset = total.saturating_sub(1);
    let next = (app.schema.scroll_offset as i32).saturating_add(delta);
    let next = next.clamp(0, max_offset as i32) as usize;
    app.schema.scroll_offset = next;
}

pub fn apply(app: &mut App, action: Action) {
    match action {
        Action::Quit => app.should_quit = true,
        Action::FocusPanel(f) => app.focus = f,
        Action::ResizeSchema(delta) => resize_schema(app, delta),
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
        Action::Schema(s) => apply_schema(app, s),
        Action::PrepareConfirmRun => prepare_confirm_run(app),
        Action::ConfirmRunSubmit => confirm_run_submit(app),
        Action::ConfirmRunCancel => confirm_run_cancel(app),
        Action::RunStatementUnderCursor => run_statement_under_cursor(app),
        Action::RunSelection => run_selection(app),
        Action::CancelQuery => cancel_query(app),
        Action::ExpandLatestResult => expand_latest(app),
        Action::CollapseResult => app.screen = Screen::Normal,
        Action::ResultNav(nav) => apply_result_nav(app, nav),
        Action::ResultEnterVisual => result_enter_visual(app),
        Action::ResultExitVisual => result_exit_visual(app),
        Action::ResultYank => result_yank(app),
        Action::ResultYankFormat(fmt) => result_yank_format(app, fmt),
        Action::ResultCancelYankFormat => result_cancel_yank_format(app),
        Action::Export { fmt, target } => export_command(app, fmt, target),
        Action::ExportSql { table, target } => export_sql_command(app, table, target),
        Action::ToggleTheme => toggle_theme(app),
        Action::Worker(ev) => apply_worker_event(app, ev),
        Action::Auth(a) => auth::apply(app, a),
        Action::ConnForm(a) => conn_form::apply(app, a),
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
        Action::ReloadSchemaCache => reload_schema_cache(app),
        Action::Mouse(target) => apply_mouse(app, target),
        Action::Chat(a) => chat::apply(app, a),
        Action::ToggleRightPanel => chat::toggle_right_panel(app),
        Action::LlmSettings(a) => llm_settings::apply(app, a),
    }
}

fn apply_mouse(app: &mut App, target: MouseTarget) {
    match target {
        MouseTarget::Editor(ev) => {
            // Click on the editor: focus it, then forward the raw event so
            // edtui can place the cursor / start its own selection.
            app.focus = Focus::Editor;
            apply(app, Action::EditorEvent(ev));
        }
        MouseTarget::SchemaRow(id) => {
            app.focus = Focus::Schema;
            schema_select(app, id);
        }
        MouseTarget::SchemaToggle(id) => {
            app.focus = Focus::Schema;
            schema_toggle_at(app, id);
        }
        MouseTarget::SchemaScroll(delta) => {
            schema_scroll(app, delta);
        }
        MouseTarget::ResultDragStart { row, col } => result_drag_start(app, row, col),
        MouseTarget::ResultDragTo { row, col } => result_drag_to(app, row, col),
        MouseTarget::ResultDragEnd => result_drag_end(app),
        MouseTarget::ResultScroll(delta) => result_scroll(app, delta),
        MouseTarget::InlineResultJump { row, col } => inline_result_jump(app, row, col),
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

fn result_drag_start(app: &mut App, row: usize, col: usize) {
    let Screen::ResultExpanded { id, .. } = &app.screen else {
        return;
    };
    let id = *id;
    let Some(block) = app.results.iter().find(|b| b.id == id) else {
        return;
    };
    let max_rows = block.rows().len();
    let max_cols = block.columns.len();
    if max_rows == 0 || max_cols == 0 {
        return;
    }
    let Screen::ResultExpanded { cursor, view, .. } = &mut app.screen else {
        return;
    };
    if matches!(view, ResultViewMode::YankFormat { .. }) {
        return;
    }
    let r = row.min(max_rows - 1);
    let c = col.min(max_cols - 1);
    cursor.jump_to(r, c);
    // Anchor visual selection at the click cell; subsequent Drag events
    // extend `cursor` while `anchor` stays put.
    *view = ResultViewMode::Visual { anchor: *cursor };
    app.layout.drag = Some(DragState::ResultSelect);
}

fn result_drag_to(app: &mut App, row: usize, col: usize) {
    if !matches!(app.layout.drag, Some(DragState::ResultSelect)) {
        return;
    }
    let Screen::ResultExpanded { id, .. } = &app.screen else {
        return;
    };
    let id = *id;
    let Some(block) = app.results.iter().find(|b| b.id == id) else {
        return;
    };
    let max_rows = block.rows().len();
    let max_cols = block.columns.len();
    if max_rows == 0 || max_cols == 0 {
        return;
    }
    let Screen::ResultExpanded { cursor, view, .. } = &mut app.screen else {
        return;
    };
    if matches!(view, ResultViewMode::YankFormat { .. }) {
        return;
    }
    let r = row.min(max_rows - 1);
    let c = col.min(max_cols - 1);
    cursor.jump_to(r, c);
}

fn result_drag_end(app: &mut App) {
    if !matches!(app.layout.drag, Some(DragState::ResultSelect)) {
        return;
    }
    app.layout.drag = None;
    // If anchor == cursor (no actual drag), drop visual mode back to
    // Normal — the user just clicked a single cell.
    let Screen::ResultExpanded { cursor, view, .. } = &mut app.screen else {
        return;
    };
    if let ResultViewMode::Visual { anchor } = *view
        && anchor.row == cursor.row
        && anchor.col == cursor.col
    {
        *view = ResultViewMode::Normal;
    }
}

fn result_scroll(app: &mut App, delta: i32) {
    let Screen::ResultExpanded { id, row_offset, .. } = &mut app.screen else {
        return;
    };
    let id = *id;
    let Some(block) = app.results.iter().find(|b| b.id == id) else {
        return;
    };
    let total = block.rows().len();
    if total == 0 {
        return;
    }
    let max_offset = total.saturating_sub(1) as i32;
    let next = (*row_offset as i32)
        .saturating_add(delta)
        .clamp(0, max_offset);
    *row_offset = next as usize;
}

fn inline_result_jump(app: &mut App, row: usize, col: usize) {
    let Some(block) = app.results.last() else {
        return;
    };
    let max_rows = block.rows().len();
    let max_cols = block.columns.len();
    if max_rows == 0 || max_cols == 0 {
        return;
    }
    let id = block.id;
    let r = row.min(max_rows - 1);
    let c = col.min(max_cols - 1);
    let mut cursor = ResultCursor::default();
    cursor.jump_to(r, c);
    app.screen = Screen::ResultExpanded {
        id,
        cursor,
        col_offset: 0,
        row_offset: 0,
        view: ResultViewMode::Normal,
    };
}

fn reload_schema_cache(app: &mut App) {
    let Some(name) = app.active_connection.clone() else {
        app.status = QueryStatus::Failed {
            error: "no active connection".into(),
        };
        return;
    };
    let _ = app.cmd_tx.send(WorkerCommand::Reload { connection: name });
    app.status = QueryStatus::Notice {
        msg: "reloading schema cache…".into(),
    };
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

fn resize_schema(app: &mut App, delta: i16) {
    let next = (app.schema.width as i32 + delta as i32)
        .clamp(MIN_SCHEMA_WIDTH as i32, MAX_SCHEMA_WIDTH as i32);
    app.schema.width = next as u16;
    persist_schema_width(app);
}

fn apply_command(app: &mut App, action: CommandAction) {
    let Some(Overlay::Command(buf)) = &mut app.overlay else {
        return;
    };
    match action {
        CommandAction::Input(input) => {
            let _ = buf.input.input(input);
        }
        CommandAction::Paste(text) => paste_into(&mut buf.input, &app.log, text),
        CommandAction::Copy => copy_from(&mut buf.input, &app.log),
        CommandAction::Cut => cut_from(&mut buf.input, &app.log),
        CommandAction::Cancel => app.overlay = None,
        CommandAction::Submit => submit_command(app),
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

fn dispatch_command(app: &mut App, cmd: command::Command) {
    use command::Command as C;
    match cmd {
        C::Quit => app.should_quit = true,
        C::Help => apply(app, Action::OpenHelp),
        C::SetSchemaWidth(w) => set_schema_width(app, w),
        C::Run => apply(app, Action::RunStatementUnderCursor),
        C::Cancel => apply(app, Action::CancelQuery),
        C::Expand => apply(app, Action::ExpandLatestResult),
        C::Collapse => apply(app, Action::CollapseResult),
        C::Theme(ThemeChoice::Toggle) => apply(app, Action::ToggleTheme),
        C::Theme(ThemeChoice::Set(kind)) => apply_theme(app, kind),
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
        C::Conn(sub) => dispatch_conn(app, sub),
        C::Chat(sub) => dispatch_chat(app, sub),
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

fn set_schema_width(app: &mut App, value: u16) {
    app.schema.width = value.clamp(MIN_SCHEMA_WIDTH, MAX_SCHEMA_WIDTH);
    persist_schema_width(app);
}

fn toggle_theme(app: &mut App) {
    apply_theme(app, app.theme.kind.toggled());
}

fn apply_theme(app: &mut App, kind: ThemeKind) {
    app.theme = Theme::for_kind(kind);
    if let Err(err) = app.config.set_theme(kind) {
        app.log.warn("config", format!("save theme failed: {err}"));
    }
}

fn persist_schema_width(app: &mut App) {
    if let Err(err) = app.config.set_schema_width(app.schema.width) {
        app.log
            .warn("config", format!("save schema_width failed: {err}"));
    }
}

fn apply_schema(app: &mut App, action: SchemaAction) {
    match action {
        SchemaAction::Down => app.schema.move_selection(1),
        SchemaAction::Up => app.schema.move_selection(-1),
        SchemaAction::ExpandOrDescend => {
            let outcome = app.schema.expand_or_descend();
            maybe_dispatch(app, outcome);
        }
        SchemaAction::CollapseOrAscend => app.schema.collapse_or_ascend(),
        SchemaAction::Toggle => {
            let outcome = app.schema.toggle_selected();
            maybe_dispatch(app, outcome);
        }
        SchemaAction::Top => app.schema.select_first(),
        SchemaAction::Bottom => app.schema.select_last(),
    }
}

fn maybe_dispatch(app: &mut App, outcome: ExpandOutcome) {
    if let ExpandOutcome::Dispatch(targets) = outcome {
        for target in targets {
            dispatch_introspect(app, target);
        }
    }
}

fn dispatch_introspect(app: &mut App, target: IntrospectTarget) {
    let _ = app.cmd_tx.send(WorkerCommand::Introspect { target });
}

fn prepare_confirm_run(app: &mut App) {
    let Some(range) = crate::state::editor::statement_under_cursor(&app.editor.state) else {
        app.status = QueryStatus::Failed {
            error: "no statement under cursor".into(),
        };
        return;
    };
    let style = crate::state::editor::confirm_highlight_style(
        app.theme.selection_bg,
        app.theme.selection_fg,
    );
    crate::state::editor::highlight_range(&mut app.editor.state, &range, style);
    app.overlay = Some(Overlay::ConfirmRun {
        statement: range.text,
    });
}

fn confirm_run_submit(app: &mut App) {
    let Some(Overlay::ConfirmRun { statement }) = app.overlay.take() else {
        return;
    };
    crate::state::editor::clear_confirm_highlight(&mut app.editor.state);
    dispatch_query(app, statement);
}

fn confirm_run_cancel(app: &mut App) {
    if !matches!(app.overlay, Some(Overlay::ConfirmRun { .. })) {
        return;
    }
    app.overlay = None;
    crate::state::editor::clear_confirm_highlight(&mut app.editor.state);
}

fn run_statement_under_cursor(app: &mut App) {
    let Some(range) = crate::state::editor::statement_under_cursor(&app.editor.state) else {
        app.status = QueryStatus::Failed {
            error: "no statement under cursor".into(),
        };
        return;
    };
    dispatch_query(app, range.text);
}

fn run_selection(app: &mut App) {
    let Some(text) = crate::state::editor::selection_text(&app.editor.state) else {
        app.status = QueryStatus::Failed {
            error: "no selection to run".into(),
        };
        return;
    };
    dispatch_query(app, text);
}

fn cancel_query(app: &mut App) {
    if app.in_flight_query.is_none() {
        app.status = QueryStatus::Failed {
            error: "no query running".into(),
        };
        return;
    }
    app.in_flight_query = None;
    app.status = QueryStatus::Cancelled;
    let _ = app.cmd_tx.send(WorkerCommand::Cancel);
}

fn dispatch_query(app: &mut App, sql: String) {
    if app.in_flight_query.is_some() {
        app.status = QueryStatus::Failed {
            error: "query already in progress — :cancel first".into(),
        };
        return;
    }
    let trimmed = sql.trim().to_string();
    if trimmed.is_empty() {
        app.status = QueryStatus::Failed {
            error: "no query to run".into(),
        };
        return;
    }
    let req = app.requests.next();
    app.in_flight_query = Some(crate::app::InFlightQuery {
        req,
        sql: trimmed.clone(),
    });
    app.status = QueryStatus::Running {
        query: trimmed.clone(),
        started_at: Instant::now(),
    };
    let _ = app
        .cmd_tx
        .send(WorkerCommand::Execute { req, sql: trimmed });
}

fn expand_latest(app: &mut App) {
    let Some(block) = app.results.last() else {
        app.status = QueryStatus::Failed {
            error: "no results to expand".into(),
        };
        return;
    };
    app.screen = Screen::ResultExpanded {
        id: block.id,
        cursor: ResultCursor::default(),
        col_offset: 0,
        row_offset: 0,
        view: ResultViewMode::Normal,
    };
}

fn apply_result_nav(app: &mut App, nav: ResultNavAction) {
    let Screen::ResultExpanded {
        id, cursor, view, ..
    } = &mut app.screen
    else {
        return;
    };
    // Movement is locked while the format prompt is open — we don't want
    // navigation keys to silently extend the selection while we're waiting
    // for `c`/`t`/`j`.
    if matches!(view, ResultViewMode::YankFormat { .. }) {
        return;
    }
    let Some(block) = app.results.iter().find(|b| b.id == *id) else {
        return;
    };
    let max_rows = block.rows().len();
    let max_cols = block.columns.len();
    apply_nav_step(cursor, nav, max_rows, max_cols);
}

fn apply_nav_step(
    cursor: &mut ResultCursor,
    nav: ResultNavAction,
    max_rows: usize,
    max_cols: usize,
) {
    match nav {
        ResultNavAction::Left => cursor.move_in(0, -1, max_rows, max_cols),
        ResultNavAction::Right => cursor.move_in(0, 1, max_rows, max_cols),
        ResultNavAction::Up => cursor.move_in(-1, 0, max_rows, max_cols),
        ResultNavAction::Down => cursor.move_in(1, 0, max_rows, max_cols),
        ResultNavAction::LineStart => cursor.jump_to(cursor.row, 0),
        ResultNavAction::LineEnd => cursor.jump_to(cursor.row, max_cols.saturating_sub(1)),
        ResultNavAction::Top => cursor.jump_to(0, cursor.col),
        ResultNavAction::Bottom => cursor.jump_to(max_rows.saturating_sub(1), cursor.col),
    }
}

fn apply_worker_event(app: &mut App, event: WorkerEvent) {
    match event {
        WorkerEvent::QueryDone { req, result } => on_query_done(app, req, result),
        WorkerEvent::QueryFailed { req, error } => on_query_failed(app, req, error.to_string()),
        WorkerEvent::SchemaLoaded { target, payload } => on_schema_loaded(app, target, payload),
        WorkerEvent::SchemaFailed { target, error } => {
            on_schema_failed(app, target, error.to_string())
        }
        WorkerEvent::Connected { name } => on_connected(app, name),
        WorkerEvent::ConnectFailed { name, error } => {
            on_connect_failed(app, name, error.to_string())
        }
        WorkerEvent::CompletionCacheStage { stage } => on_cache_stage(app, stage),
        WorkerEvent::CompletionCacheFailed { stage, error } => {
            on_cache_failed(app, stage, error.to_string())
        }
        WorkerEvent::ChatDelta(delta) => chat::on_delta(app, delta),
    }
}

fn on_cache_stage(app: &mut App, stage: crate::worker::CacheStage) {
    use crate::worker::CacheStage;
    if matches!(stage, CacheStage::Reloaded) {
        app.status = QueryStatus::Notice {
            msg: "schema cache reloaded".into(),
        };
    }
    // Columns just landed — if the popover is currently waiting on
    // them (likely showing a "loading…" placeholder), recompute.
    if matches!(stage, CacheStage::Columns { .. }) && app.completion.is_some() {
        completion::refresh(app);
    }
}

fn on_cache_failed(app: &mut App, stage: crate::worker::CacheStage, error: String) {
    app.log.warn(
        "autocomplete",
        format!("cache load failed at {stage:?}: {error}"),
    );
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
    // Fresh tree — drop any nodes left over from the previous connection
    // and re-fire the catalog load.
    app.schema = SchemaPanel::new(app.schema.width);
    app.results.clear();
    load_session(app, &name);
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

fn on_schema_loaded(
    app: &mut App,
    target: IntrospectTarget,
    payload: crate::worker::SchemaPayload,
) {
    use crate::worker::SchemaPayload;
    match payload {
        SchemaPayload::Catalogs(catalogs) => app.schema.populate_catalogs(catalogs),
        other => app.schema.populate(&target, other),
    }
}

fn on_schema_failed(app: &mut App, target: IntrospectTarget, error: String) {
    if matches!(target, IntrospectTarget::Catalogs) {
        app.schema.fail_root_load(error);
        return;
    }
    app.schema.record_failure(&target, error);
}

fn on_query_done(app: &mut App, req: crate::worker::RequestId, result: QueryResult) {
    let Some(in_flight) = app.in_flight_query.as_ref() else {
        return;
    };
    if in_flight.req != req {
        return;
    }
    let in_flight = app.in_flight_query.take().expect("checked above");

    // DDL detection: if the just-executed SQL reshaped the schema,
    // re-prime the autocomplete cache so the next popover sees the
    // new state. Best-effort — failures are surfaced through the
    // normal cache-stage failure path.
    if crate::autocomplete::ddl::affects_schema_cache(&in_flight.sql)
        && let Some(name) = app.active_connection.clone()
    {
        let _ = app.cmd_tx.send(WorkerCommand::Reload { connection: name });
    }

    let took = result.elapsed;
    let total_rows = result.rows.len();
    let affected = result.affected;

    // Statements run via `execute()` (DML/DDL) report no columns — there's
    // nothing to render in a result block, so skip pushing one.
    if !result.columns.is_empty() {
        let id = ResultId(app.results.len());
        // `active_dialect` should always be Some here (we only run queries
        // through an active connection), but fall back to Sqlite rather than
        // panic if the invariant ever breaks.
        let dialect = app
            .active_dialect
            .unwrap_or(crate::datasource::DriverKind::Sqlite);
        app.results.push(ResultBlock {
            id,
            took,
            columns: result.columns,
            rows: result.rows,
            sql: in_flight.sql,
            dialect,
        });
    }

    app.status = QueryStatus::Succeeded {
        rows: total_rows,
        affected,
        took,
    };
}

fn on_query_failed(app: &mut App, req: crate::worker::RequestId, error: String) {
    let Some(in_flight) = app.in_flight_query.as_ref() else {
        return;
    };
    if in_flight.req != req {
        return;
    }
    app.in_flight_query = None;
    app.status = QueryStatus::Failed { error };
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

// ---------------------------------------------------------------------------
// Result view: visual selection, yank, export
// ---------------------------------------------------------------------------

fn result_enter_visual(app: &mut App) {
    let Screen::ResultExpanded { cursor, view, .. } = &mut app.screen else {
        return;
    };
    if matches!(view, ResultViewMode::Normal) {
        *view = ResultViewMode::Visual { anchor: *cursor };
    }
}

fn result_exit_visual(app: &mut App) {
    let Screen::ResultExpanded { view, .. } = &mut app.screen else {
        return;
    };
    *view = ResultViewMode::Normal;
}

fn result_yank(app: &mut App) {
    let Screen::ResultExpanded {
        id, cursor, view, ..
    } = &mut app.screen
    else {
        return;
    };
    match *view {
        ResultViewMode::Normal => {
            // Single cell — copy the rendered string straight to the clipboard.
            // No header, no quoting, no prompt.
            let cur = *cursor;
            let id = *id;
            let Some(block) = app.results.iter().find(|b| b.id == id) else {
                return;
            };
            let text = block
                .rows()
                .get(cur.row)
                .and_then(|row| row.get(cur.col))
                .map(|cell| cell.display())
                .unwrap_or_default();
            clipboard::write(&app.log, &text);
            app.status = QueryStatus::Notice {
                msg: format!("yanked cell ({}, {})", cur.row + 1, cur.col + 1),
            };
        }
        ResultViewMode::Visual { anchor } => {
            *view = ResultViewMode::YankFormat { anchor };
        }
        ResultViewMode::YankFormat { .. } => {}
    }
}

fn result_yank_format(app: &mut App, fmt: ExportFormat) {
    let (id, cursor, anchor) = {
        let Screen::ResultExpanded {
            id, cursor, view, ..
        } = &app.screen
        else {
            return;
        };
        let ResultViewMode::YankFormat { anchor } = view else {
            return;
        };
        (*id, *cursor, *anchor)
    };
    let rect = SelectionRect::new(anchor, cursor);
    let payload = match fmt {
        ExportFormat::Sql => match render_selection_sql(app, id, &rect) {
            Ok(p) => p,
            Err(e) => {
                // Stay in Visual on error — the user might want to copy the
                // selection in another format, or expand it.
                if let Screen::ResultExpanded { view, .. } = &mut app.screen {
                    *view = ResultViewMode::Visual { anchor };
                }
                app.status = QueryStatus::Failed { error: e };
                return;
            }
        },
        _ => match render_selection(app, id, &rect, fmt) {
            Some(p) => p,
            None => {
                // Block disappeared between expand and yank — drop back to
                // Normal and surface the error.
                if let Screen::ResultExpanded { view, .. } = &mut app.screen {
                    *view = ResultViewMode::Normal;
                }
                app.status = QueryStatus::Failed {
                    error: "result no longer available".into(),
                };
                return;
            }
        },
    };
    clipboard::write(&app.log, &payload);
    if let Screen::ResultExpanded { view, .. } = &mut app.screen {
        *view = ResultViewMode::Normal;
    }
    app.status = QueryStatus::Notice {
        msg: format!(
            "yanked {}×{} as {} ({} bytes)",
            rect.rows(),
            rect.cols(),
            fmt.label(),
            payload.len()
        ),
    };
}

fn result_cancel_yank_format(app: &mut App) {
    let Screen::ResultExpanded { view, .. } = &mut app.screen else {
        return;
    };
    if let ResultViewMode::YankFormat { anchor } = *view {
        *view = ResultViewMode::Visual { anchor };
    }
}

/// `:export sql` handler. Mirrors `export_command` (selection wins over
/// whole-block) but resolves the target table via inference when the
/// caller didn't provide one. Failure modes surface as a status error
/// so the user knows to retry with `:export sql <table>`.
fn export_sql_command(app: &mut App, table: Option<String>, target: ExportTarget) {
    // Same selection-vs-block dispatch shape as `export_command`. The
    // selection branch passes the column-index slice down to inference
    // so a Visual subset can succeed even when the full projection
    // wouldn't.
    if let Screen::ResultExpanded {
        id, cursor, view, ..
    } = &app.screen
        && let Some(anchor) = view.anchor()
    {
        let id = *id;
        let cursor = *cursor;
        let rect = SelectionRect::new(anchor, cursor);
        let Some(block) = app.results.iter().find(|b| b.id == id) else {
            app.status = QueryStatus::Failed {
                error: "result no longer available".into(),
            };
            return;
        };
        let col_end = (rect.col_end + 1).min(block.columns.len());
        let col_start = rect.col_start.min(col_end);
        let row_end = (rect.row_end + 1).min(block.rows().len());
        let row_start = rect.row_start.min(row_end);
        let column_indices: Vec<usize> = (col_start..col_end).collect();
        let resolved_table = match resolve_export_table(table, block, Some(&column_indices)) {
            Ok(t) => t,
            Err(e) => {
                app.status = QueryStatus::Failed { error: e };
                return;
            }
        };
        let columns: Vec<&Column> = block.columns[col_start..col_end].iter().collect();
        let rows: Vec<Vec<&Cell>> = block.rows()[row_start..row_end]
            .iter()
            .map(|row| {
                let end = col_end.min(row.len());
                let start = col_start.min(end);
                row[start..end].iter().collect()
            })
            .collect();
        let dialect = block.dialect;
        let payload = export::format_insert(dialect, &resolved_table, &columns, &rows);
        let drop_visual = matches!(target, ExportTarget::Clipboard);
        finish_export(
            app,
            ExportFormat::Sql,
            target,
            rect.rows(),
            rect.cols(),
            payload,
        );
        if drop_visual && let Screen::ResultExpanded { view, .. } = &mut app.screen {
            *view = ResultViewMode::Normal;
        }
        return;
    }
    let Some(block) = app.results.last() else {
        app.status = QueryStatus::Failed {
            error: "no result to export".into(),
        };
        return;
    };
    let resolved_table = match resolve_export_table(table, block, None) {
        Ok(t) => t,
        Err(e) => {
            app.status = QueryStatus::Failed { error: e };
            return;
        }
    };
    let columns: Vec<&Column> = block.columns.iter().collect();
    let rows: Vec<Vec<&Cell>> = block
        .rows()
        .iter()
        .map(|row| row.iter().collect())
        .collect();
    let dialect = block.dialect;
    let payload = export::format_insert(dialect, &resolved_table, &columns, &rows);
    let row_count = rows.len();
    let col_count = columns.len();
    finish_export(
        app,
        ExportFormat::Sql,
        target,
        row_count,
        col_count,
        payload,
    );
}

/// Returns the target table for `:export sql`. If the user passed one
/// explicitly, use it; otherwise run inference and surface the (always
/// human-readable) failure reason verbatim.
fn resolve_export_table(
    explicit: Option<String>,
    block: &ResultBlock,
    column_indices: Option<&[usize]>,
) -> Result<String, String> {
    if let Some(t) = explicit {
        return Ok(t);
    }
    crate::sql_infer::infer_source_table(&block.sql, block.dialect, column_indices)
        .map_err(|e| format!("can't infer source table — {e}"))
}

fn export_command(app: &mut App, fmt: ExportFormat, target: ExportTarget) {
    // Two routes:
    // - Inside an expanded result with an active selection → export the rect.
    // - Otherwise → export the latest result block in full.
    if let Screen::ResultExpanded {
        id, cursor, view, ..
    } = &app.screen
        && let Some(anchor) = view.anchor()
    {
        let id = *id;
        let cursor = *cursor;
        let rect = SelectionRect::new(anchor, cursor);
        let Some(payload) = render_selection(app, id, &rect, fmt) else {
            app.status = QueryStatus::Failed {
                error: "result no longer available".into(),
            };
            return;
        };
        let drop_visual = matches!(target, ExportTarget::Clipboard);
        finish_export(app, fmt, target, rect.rows(), rect.cols(), payload);
        if drop_visual && let Screen::ResultExpanded { view, .. } = &mut app.screen {
            *view = ResultViewMode::Normal;
        }
        return;
    }
    let Some(block) = app.results.last() else {
        app.status = QueryStatus::Failed {
            error: "no result to export".into(),
        };
        return;
    };
    let columns: Vec<&Column> = block.columns.iter().collect();
    let rows: Vec<Vec<&Cell>> = block
        .rows()
        .iter()
        .map(|row| row.iter().collect())
        .collect();
    let payload = export::format(fmt, &columns, &rows);
    let row_count = rows.len();
    let col_count = columns.len();
    finish_export(app, fmt, target, row_count, col_count, payload);
}

/// Deliver `payload` to `target` and set the status line. The clipboard path
/// is fire-and-forget (failures get logged inside `clipboard::write`); the
/// file path surfaces I/O errors to the user since they typed the path.
fn finish_export(
    app: &mut App,
    fmt: ExportFormat,
    target: ExportTarget,
    rows: usize,
    cols: usize,
    payload: String,
) {
    match target {
        ExportTarget::Clipboard => {
            clipboard::write(&app.log, &payload);
            app.status = QueryStatus::Notice {
                msg: format!(
                    "exported {}×{} as {} ({} bytes)",
                    rows,
                    cols,
                    fmt.label(),
                    payload.len()
                ),
            };
        }
        ExportTarget::File(path) => match std::fs::write(&path, &payload) {
            Ok(()) => {
                app.status = QueryStatus::Notice {
                    msg: format!(
                        "exported {}×{} as {} to {} ({} bytes)",
                        rows,
                        cols,
                        fmt.label(),
                        path.display(),
                        payload.len()
                    ),
                };
            }
            Err(err) => {
                app.status = QueryStatus::Failed {
                    error: format!("export failed: {err}"),
                };
            }
        },
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

/// Slice the selected rectangle out of `block` and run it through the
/// chosen formatter. Returns `None` only if the block has gone missing.
fn render_selection(
    app: &App,
    id: ResultId,
    rect: &SelectionRect,
    fmt: ExportFormat,
) -> Option<String> {
    let block = app.results.iter().find(|b| b.id == id)?;
    let col_end = (rect.col_end + 1).min(block.columns.len());
    let col_start = rect.col_start.min(col_end);
    let columns: Vec<&Column> = block.columns[col_start..col_end].iter().collect();
    let row_end = (rect.row_end + 1).min(block.rows().len());
    let row_start = rect.row_start.min(row_end);
    let rows: Vec<Vec<&Cell>> = block.rows()[row_start..row_end]
        .iter()
        .map(|row| {
            let end = col_end.min(row.len());
            let start = col_start.min(end);
            row[start..end].iter().collect()
        })
        .collect();
    Some(export::format(fmt, &columns, &rows))
}

/// SQL-flavoured render path for the Visual yank prompt. There's no
/// place to type a table from inside the prompt, so this always relies
/// on `infer_source_table`; on miss the caller surfaces the error and
/// keeps the user in Visual so they can retry via `:export sql <table>`.
fn render_selection_sql(app: &App, id: ResultId, rect: &SelectionRect) -> Result<String, String> {
    let block = app
        .results
        .iter()
        .find(|b| b.id == id)
        .ok_or_else(|| "result no longer available".to_string())?;
    let col_end = (rect.col_end + 1).min(block.columns.len());
    let col_start = rect.col_start.min(col_end);
    let row_end = (rect.row_end + 1).min(block.rows().len());
    let row_start = rect.row_start.min(row_end);
    let column_indices: Vec<usize> = (col_start..col_end).collect();
    let table =
        crate::sql_infer::infer_source_table(&block.sql, block.dialect, Some(&column_indices))
            .map_err(|e| format!("can't infer source table — {e}"))?;
    let columns: Vec<&Column> = block.columns[col_start..col_end].iter().collect();
    let rows: Vec<Vec<&Cell>> = block.rows()[row_start..row_end]
        .iter()
        .map(|row| {
            let end = col_end.min(row.len());
            let start = col_start.min(end);
            row[start..end].iter().collect()
        })
        .collect();
    Ok(export::format_insert(
        block.dialect,
        &table,
        &columns,
        &rows,
    ))
}

// ---------------------------------------------------------------------------
// Editor session persistence
// ---------------------------------------------------------------------------

const SESSION_DEBOUNCE: Duration = Duration::from_millis(800);

/// Push the next debounced save 800ms into the future. Skips when there's
/// no active connection — the editor isn't user-reachable in those modes,
/// but the early return keeps us honest if that ever changes.
pub(super) fn schedule_session_save(app: &mut App) {
    if app.active_connection.is_none() {
        return;
    }
    app.editor_dirty = true;
    app.pending_save_at = Some(tokio::time::Instant::now() + SESSION_DEBOUNCE);
}

/// Write the current editor buffer to the active connection's session
/// file. Best-effort: failures are logged and swallowed so a flaky disk
/// can't break the editor.
pub(crate) fn flush_session(app: &mut App) {
    let Some(name) = app.active_connection.clone() else {
        app.editor_dirty = false;
        app.pending_save_at = None;
        return;
    };
    let path = session::path_for(&app.data_dir, &name);
    let text = app.editor.text();
    match session::save(&path, &text) {
        Ok(()) => app.log.info("session", format!("saved {}", path.display())),
        Err(err) => app
            .log
            .warn("session", format!("save {} failed: {err}", path.display())),
    }
    app.editor_dirty = false;
    app.pending_save_at = None;
}

/// Load the session for `name` into the editor. Treats a missing file as
/// an empty buffer — first save will create it. Resets the dirty/timer
/// state so the load itself doesn't trigger another save.
fn load_session(app: &mut App, name: &str) {
    let path = session::path_for(&app.data_dir, name);
    match session::load(&path) {
        Ok(text) => {
            app.editor.replace_text(&text);
            app.log
                .info("session", format!("loaded {}", path.display()));
        }
        Err(err) => {
            app.log
                .warn("session", format!("load {} failed: {err}", path.display()));
            app.editor.replace_text("");
        }
    }
    app.editor_dirty = false;
    app.pending_save_at = None;
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
