use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::Event as CtEvent;
use ratatui_textarea::{Input, TextArea};

use crate::app::{App, MAX_SCHEMA_WIDTH, MIN_SCHEMA_WIDTH};
use crate::clipboard;
use crate::connections::{self, ConnectionStore};
use crate::datasource::{Cell, Column, QueryResult};
use crate::export::{self, ExportFormat};
use crate::session;
use crate::state::auth::AuthKind;
use crate::state::command::CommandBuffer;
use crate::state::conn_form::{ConnFormPostSave, ConnFormState};
use crate::state::conn_list::ConnListState;
use crate::state::focus::{Focus, Mode, PendingChord};
use crate::state::results::{
    ResultBlock, ResultCursor, ResultId, ResultPayload, ResultViewMode, SelectionRect,
};
use crate::state::schema::{ExpandOutcome, SchemaPanel};
use crate::state::status::QueryStatus;
use crate::ui::theme::{Theme, ThemeKind};
use crate::worker::{IntrospectTarget, WorkerCommand, WorkerEvent};

const PREVIEW_ROWS: usize = 100;

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
    ToggleTheme,
    Worker(WorkerEvent),
    Auth(AuthAction),
    ConnForm(ConnFormAction),
    ConnList(ConnListAction),
}

#[derive(Debug, Clone)]
pub enum ExportTarget {
    Clipboard,
    File(PathBuf),
}

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

pub enum SchemaAction {
    Down,
    Up,
    ExpandOrDescend,
    CollapseOrAscend,
    Toggle,
    Top,
    Bottom,
}

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

pub fn apply(app: &mut App, action: Action) {
    match action {
        Action::Quit => app.should_quit = true,
        Action::FocusPanel(f) => app.focus = f,
        Action::ResizeSchema(delta) => resize_schema(app, delta),
        Action::SetPendingChord(c) => app.pending = c,
        Action::EditorEvent(ev) => {
            app.editor.events.on_event(ev, &mut app.editor.state);
            schedule_session_save(app);
        }
        Action::OpenCommand => app.mode = Mode::Command(CommandBuffer::default()),
        Action::Command(cmd) => apply_command(app, cmd),
        Action::Schema(s) => apply_schema(app, s),
        Action::PrepareConfirmRun => prepare_confirm_run(app),
        Action::ConfirmRunSubmit => confirm_run_submit(app),
        Action::ConfirmRunCancel => confirm_run_cancel(app),
        Action::RunStatementUnderCursor => run_statement_under_cursor(app),
        Action::RunSelection => run_selection(app),
        Action::CancelQuery => cancel_query(app),
        Action::ExpandLatestResult => expand_latest(app),
        Action::CollapseResult => app.mode = Mode::Normal,
        Action::ResultNav(nav) => apply_result_nav(app, nav),
        Action::ResultEnterVisual => result_enter_visual(app),
        Action::ResultExitVisual => result_exit_visual(app),
        Action::ResultYank => result_yank(app),
        Action::ResultYankFormat(fmt) => result_yank_format(app, fmt),
        Action::ResultCancelYankFormat => result_cancel_yank_format(app),
        Action::Export { fmt, target } => export_command(app, fmt, target),
        Action::ToggleTheme => toggle_theme(app),
        Action::Worker(ev) => apply_worker_event(app, ev),
        Action::Auth(a) => apply_auth(app, a),
        Action::ConnForm(a) => apply_conn_form(app, a),
        Action::ConnList(a) => apply_conn_list(app, a),
    }
}

fn resize_schema(app: &mut App, delta: i16) {
    let next = (app.schema.width as i32 + delta as i32)
        .clamp(MIN_SCHEMA_WIDTH as i32, MAX_SCHEMA_WIDTH as i32);
    app.schema.width = next as u16;
    persist_schema_width(app);
}

fn apply_command(app: &mut App, action: CommandAction) {
    let Mode::Command(buf) = &mut app.mode else {
        return;
    };
    match action {
        CommandAction::Input(input) => {
            let _ = buf.input.input(input);
        }
        CommandAction::Paste(text) => paste_into(&mut buf.input, &app.log, text),
        CommandAction::Copy => copy_from(&mut buf.input, &app.log),
        CommandAction::Cut => cut_from(&mut buf.input, &app.log),
        CommandAction::Cancel => app.mode = Mode::Normal,
        CommandAction::Submit => submit_command(app),
    }
}

fn submit_command(app: &mut App) {
    let Mode::Command(buf) = &app.mode else {
        return;
    };
    let raw = buf.text().trim().to_string();
    app.mode = Mode::Normal;
    run_command_line(app, &raw);
}

fn run_command_line(app: &mut App, line: &str) {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return;
    };
    let args: Vec<&str> = parts.collect();
    match cmd {
        "q" | "quit" => app.should_quit = true,
        "width" => set_schema_width(app, &args),
        "run" | "r" => run_statement_under_cursor(app),
        "cancel" => cancel_query(app),
        "expand" | "e" => expand_latest(app),
        "collapse" | "c" => app.mode = Mode::Normal,
        "theme" => set_theme(app, &args),
        "export" => run_export_command(app, &args),
        "conn" | "conns" => run_conn_command(app, &args),
        _ => {
            app.status = QueryStatus::Failed {
                error: format!("unknown command: {cmd}"),
            };
        }
    }
}

fn run_conn_command(app: &mut App, args: &[&str]) {
    let sub = args.first().copied();
    let rest: Vec<&str> = args.iter().skip(1).copied().collect();
    match sub {
        None | Some("list") | Some("ls") => open_conn_list(app),
        Some("add") => open_conn_form_create(app, rest.first().copied()),
        Some("edit") => open_conn_form_edit_command(app, rest.first().copied()),
        Some("delete") | Some("rm") => delete_connection_command(app, rest.first().copied()),
        Some("use") => use_connection_command(app, rest.first().copied()),
        Some(other) => {
            app.status = QueryStatus::Failed {
                error: format!("unknown :conn subcommand: {other} (use list/add/edit/delete/use)"),
            };
        }
    }
}

fn open_conn_list(app: &mut App) {
    let entries = app.config.connection_names();
    if entries.is_empty() {
        // Nothing to list — bounce straight to the create form so the user
        // doesn't get an empty modal and have to type `:conn add` next.
        app.mode = Mode::EditConnection(
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
    app.mode = Mode::ConnectionList(state);
}

fn open_conn_form_create(app: &mut App, name: Option<&str>) {
    let mut form = ConnFormState::new_create().with_post_save(ConnFormPostSave::ReturnToList);
    if let Some(n) = name {
        form = form.with_prefilled_name(n);
    }
    app.mode = Mode::EditConnection(form);
}

fn open_conn_form_edit_command(app: &mut App, name: Option<&str>) {
    let Some(name) = name else {
        app.status = QueryStatus::Failed {
            error: "usage: :conn edit <name>".into(),
        };
        return;
    };
    open_conn_form_edit(app, name, ConnFormPostSave::ReturnToList);
}

fn open_conn_form_edit(app: &mut App, name: &str, post_save: ConnFormPostSave) {
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
    app.mode = Mode::EditConnection(
        ConnFormState::editing(name.to_string(), url).with_post_save(post_save),
    );
}

fn delete_connection_command(app: &mut App, name: Option<&str>) {
    let Some(name) = name else {
        app.status = QueryStatus::Failed {
            error: "usage: :conn delete <name>".into(),
        };
        return;
    };
    perform_delete(app, name);
}

fn perform_delete(app: &mut App, name: &str) {
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

fn use_connection_command(app: &mut App, name: Option<&str>) {
    let Some(name) = name else {
        app.status = QueryStatus::Failed {
            error: "usage: :conn use <name>".into(),
        };
        return;
    };
    use_connection(app, name);
}

fn use_connection(app: &mut App, name: &str) {
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

fn set_schema_width(app: &mut App, args: &[&str]) {
    let Some(value) = args.first().and_then(|v| v.parse::<u16>().ok()) else {
        app.status = QueryStatus::Failed {
            error: "usage: :width <cols>".into(),
        };
        return;
    };
    app.schema.width = value.clamp(MIN_SCHEMA_WIDTH, MAX_SCHEMA_WIDTH);
    persist_schema_width(app);
}

fn set_theme(app: &mut App, args: &[&str]) {
    match args.first().copied() {
        None | Some("toggle") => toggle_theme(app),
        Some(name) => match ThemeKind::parse(name) {
            Some(k) => apply_theme(app, k),
            None => {
                app.status = QueryStatus::Failed {
                    error: format!("unknown theme: {name} (use dark|light|toggle)"),
                };
            }
        },
    }
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
    app.mode = Mode::ConfirmRun {
        statement: range.text,
    };
}

fn confirm_run_submit(app: &mut App) {
    let Mode::ConfirmRun { statement } = std::mem::replace(&mut app.mode, Mode::Normal) else {
        return;
    };
    crate::state::editor::clear_confirm_highlight(&mut app.editor.state);
    dispatch_query(app, statement);
}

fn confirm_run_cancel(app: &mut App) {
    if !matches!(app.mode, Mode::ConfirmRun { .. }) {
        return;
    }
    app.mode = Mode::Normal;
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
    app.in_flight_query = Some(req);
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
    app.mode = Mode::ResultExpanded {
        id: block.id,
        cursor: ResultCursor::default(),
        col_offset: 0,
        row_offset: 0,
        view: ResultViewMode::Normal,
    };
}

fn apply_result_nav(app: &mut App, nav: ResultNavAction) {
    let Mode::ResultExpanded {
        id, cursor, view, ..
    } = &mut app.mode
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
        WorkerEvent::SchemaLoaded { target, payload } => {
            on_schema_loaded(app, target, payload)
        }
        WorkerEvent::SchemaFailed { target, error } => {
            on_schema_failed(app, target, error.to_string())
        }
        WorkerEvent::Connected { name } => on_connected(app, name),
        WorkerEvent::ConnectFailed { name, error } => on_connect_failed(app, name, error.to_string()),
    }
}

fn on_connected(app: &mut App, name: String) {
    // Only react if we're still expecting this connection. A late event from
    // an aborted swap would otherwise clobber the active connection.
    let expected = matches!(&app.mode, Mode::Connecting { name: pending } if pending == &name);
    if !expected {
        return;
    }
    app.active_connection = Some(name.clone());
    app.mode = Mode::Normal;
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
    app.log
        .info("app", format!("connected to {name}"));
}

fn on_connect_failed(app: &mut App, name: String, error: String) {
    let was_pending = matches!(&app.mode, Mode::Connecting { name: pending } if pending == &name);
    if !was_pending {
        return;
    }
    app.log
        .warn("app", format!("connect failed for {name}: {error}"));

    // Live switch (`:conn use`) — the previous datasource is still alive in
    // the worker, so just surface the error and stay in Normal.
    if app.active_connection.is_some() {
        app.mode = Mode::Normal;
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
            app.mode = Mode::EditConnection(form);
        }
        None => {
            app.mode = Mode::Normal;
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
    if app.in_flight_query != Some(req) {
        return;
    }
    app.in_flight_query = None;

    let took = result.elapsed;
    let total_rows = result.rows.len();
    let affected = result.affected;

    // Statements run via `execute()` (DML/DDL) report no columns — there's
    // nothing to render in a result block, so skip pushing one.
    if !result.columns.is_empty() {
        let payload = build_payload(result.rows, total_rows);
        let id = ResultId(app.results.len());
        app.results.push(ResultBlock {
            id,
            took,
            columns: result.columns,
            payload,
        });
    }

    app.status = QueryStatus::Succeeded {
        rows: total_rows,
        affected,
        took,
    };
}

fn build_payload(rows: Vec<crate::state::results::Row>, total_rows: usize) -> ResultPayload {
    if total_rows > PREVIEW_ROWS {
        let preview = rows.into_iter().take(PREVIEW_ROWS).collect();
        ResultPayload::Clipped {
            preview,
            total_rows,
        }
    } else {
        ResultPayload::Clipped {
            preview: rows,
            total_rows,
        }
    }
}

fn on_query_failed(app: &mut App, req: crate::worker::RequestId, error: String) {
    if app.in_flight_query != Some(req) {
        return;
    }
    app.in_flight_query = None;
    app.status = QueryStatus::Failed { error };
}

// ---------------------------------------------------------------------------
// Auth flow
// ---------------------------------------------------------------------------

fn apply_auth(app: &mut App, action: AuthAction) {
    let Mode::Auth(state) = &mut app.mode else {
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
        AuthAction::Submit => auth_submit(app),
    }
}

fn auth_submit(app: &mut App) {
    let Mode::Auth(state) = &mut app.mode else {
        return;
    };
    state.error = None;
    let attempt = state
        .input
        .lines()
        .first()
        .cloned()
        .unwrap_or_default();
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
                            set_auth_error(app, format!("save crypto block failed: {err}"));
                            return;
                        }
                        app.log.info("auth", "encrypted store initialised");
                        ConnectionStore::encrypted(key)
                    }
                    Err(err) => {
                        set_auth_error(app, format!("crypto setup failed: {err}"));
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
                if let Mode::Auth(state) = &mut app.mode {
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
                            if remaining == 1 { "attempt" } else { "attempts" }
                        ));
                    }
                }
            }
        },
    }
}

fn set_auth_error(app: &mut App, msg: String) {
    if let Mode::Auth(state) = &mut app.mode {
        state.error = Some(msg);
    }
}

/// Decides what to render after the auth Mode resolves. Either jumps
/// straight into a connection form (no saved connections) or opens the
/// connection picker so the user can choose one.
fn transition_post_auth(app: &mut App) {
    let entries = app.config.connection_names();
    if entries.is_empty() {
        app.mode = Mode::EditConnection(ConnFormState::new_create());
        return;
    }
    app.mode = Mode::ConnectionList(ConnListState::new(entries));
}

// ---------------------------------------------------------------------------
// Connection-form flow
// ---------------------------------------------------------------------------

fn apply_conn_form(app: &mut App, action: ConnFormAction) {
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
        ConnFormAction::Submit => conn_form_submit(app),
    }
}

fn conn_form_submit(app: &mut App) {
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

// ---------------------------------------------------------------------------
// Connection list
// ---------------------------------------------------------------------------

fn apply_conn_list(app: &mut App, action: ConnListAction) {
    let Mode::ConnectionList(state) = &mut app.mode else {
        return;
    };
    // While confirming a delete, only y/Enter and n/Esc do anything (handled
    // via ConfirmDelete / CancelDelete).
    if state.is_confirming() {
        match action {
            ConnListAction::ConfirmDelete => {
                if let Some(name) = state.take_pending_delete() {
                    perform_delete(app, &name);
                    refresh_conn_list(app);
                }
            }
            ConnListAction::CancelDelete => state.cancel_delete(),
            _ => {}
        }
        return;
    }
    match action {
        ConnListAction::Down => state.move_selection(1),
        ConnListAction::Up => state.move_selection(-1),
        ConnListAction::Top => state.jump_top(),
        ConnListAction::Bottom => state.jump_bottom(),
        ConnListAction::AddNew => {
            app.mode = Mode::EditConnection(
                ConnFormState::new_create().with_post_save(ConnFormPostSave::ReturnToList),
            );
        }
        ConnListAction::EditSelected => {
            if let Some(name) = state.selected_name().map(str::to_string) {
                open_conn_form_edit(app, &name, ConnFormPostSave::ReturnToList);
            }
        }
        ConnListAction::UseSelected => {
            if let Some(name) = state.selected_name().map(str::to_string) {
                use_connection(app, &name);
            }
        }
        ConnListAction::BeginDelete => state.begin_delete(),
        ConnListAction::Close => app.mode = Mode::Normal,
        // Handled in the confirming branch above.
        ConnListAction::ConfirmDelete | ConnListAction::CancelDelete => {}
    }
}

fn refresh_conn_list(app: &mut App) {
    if let Mode::ConnectionList(state) = &mut app.mode {
        state.refresh(app.config.connection_names());
        if state.entries.is_empty() {
            app.mode = Mode::Normal;
        }
    }
}

pub(crate) fn dispatch_connect(app: &mut App, name: String, url: String) {
    // If we're swapping connections, persist the current session before the
    // editor's contents get replaced by the next `Connected` event.
    flush_session(app);
    app.mode = Mode::Connecting { name: name.clone() };
    app.status = QueryStatus::Idle;
    let _ = app
        .cmd_tx
        .send(WorkerCommand::Connect { name, url });
}

// ---------------------------------------------------------------------------
// Clipboard helpers (shared across every TextArea-backed input)
// ---------------------------------------------------------------------------

fn paste_into(
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

fn copy_from(input: &mut TextArea<'static>, log: &crate::log::Logger) {
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

fn cut_from(input: &mut TextArea<'static>, log: &crate::log::Logger) {
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
    let Mode::ResultExpanded { cursor, view, .. } = &mut app.mode else {
        return;
    };
    if matches!(view, ResultViewMode::Normal) {
        *view = ResultViewMode::Visual { anchor: *cursor };
    }
}

fn result_exit_visual(app: &mut App) {
    let Mode::ResultExpanded { view, .. } = &mut app.mode else {
        return;
    };
    *view = ResultViewMode::Normal;
}

fn result_yank(app: &mut App) {
    let Mode::ResultExpanded { id, cursor, view, .. } = &mut app.mode else {
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
        let Mode::ResultExpanded { id, cursor, view, .. } = &app.mode else {
            return;
        };
        let ResultViewMode::YankFormat { anchor } = view else {
            return;
        };
        (*id, *cursor, *anchor)
    };
    let rect = SelectionRect::new(anchor, cursor);
    let payload = match render_selection(app, id, &rect, fmt) {
        Some(p) => p,
        None => {
            // Block disappeared between expand and yank — drop back to Normal
            // and surface the error.
            if let Mode::ResultExpanded { view, .. } = &mut app.mode {
                *view = ResultViewMode::Normal;
            }
            app.status = QueryStatus::Failed {
                error: "result no longer available".into(),
            };
            return;
        }
    };
    clipboard::write(&app.log, &payload);
    if let Mode::ResultExpanded { view, .. } = &mut app.mode {
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
    let Mode::ResultExpanded { view, .. } = &mut app.mode else {
        return;
    };
    if let ResultViewMode::YankFormat { anchor } = *view {
        *view = ResultViewMode::Visual { anchor };
    }
}

fn run_export_command(app: &mut App, args: &[&str]) {
    let Some(fmt_str) = args.first() else {
        app.status = QueryStatus::Failed {
            error: "usage: :export <csv|tsv|json> [path]".into(),
        };
        return;
    };
    let Some(fmt) = ExportFormat::parse(fmt_str) else {
        app.status = QueryStatus::Failed {
            error: format!("unknown export format: {fmt_str} (use csv|tsv|json)"),
        };
        return;
    };
    // After the format token: nothing → clipboard. Otherwise the rest is a
    // path; an optional leading `>` is allowed so users can write the
    // shell-style `:export csv > out.csv`. Tokens are joined by single spaces
    // (good enough for sane paths; the command line is already whitespace-
    // tokenised by the time we get here).
    let rest = &args[1..];
    let target = match rest.first().copied() {
        None => ExportTarget::Clipboard,
        Some(">") if rest.len() == 1 => {
            app.status = QueryStatus::Failed {
                error: "missing path after `>`".into(),
            };
            return;
        }
        Some(">") => ExportTarget::File(expand_tilde(&rest[1..].join(" "))),
        Some(_) => ExportTarget::File(expand_tilde(&rest.join(" "))),
    };
    apply(app, Action::Export { fmt, target });
}

fn export_command(app: &mut App, fmt: ExportFormat, target: ExportTarget) {
    // Two routes:
    // - Inside an expanded result with an active selection → export the rect.
    // - Otherwise → export the latest result block in full.
    if let Mode::ResultExpanded { id, cursor, view, .. } = &app.mode
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
        if drop_visual
            && let Mode::ResultExpanded { view, .. } = &mut app.mode
        {
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

// ---------------------------------------------------------------------------
// Editor session persistence
// ---------------------------------------------------------------------------

const SESSION_DEBOUNCE: Duration = Duration::from_millis(800);

/// Push the next debounced save 800ms into the future. Skips when there's
/// no active connection — the editor isn't user-reachable in those modes,
/// but the early return keeps us honest if that ever changes.
fn schedule_session_save(app: &mut App) {
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
        Ok(()) => app
            .log
            .info("session", format!("saved {}", path.display())),
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
