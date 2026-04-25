use std::time::Instant;

use ratatui::crossterm::event::Event as CtEvent;

use crate::app::{App, MAX_SCHEMA_WIDTH, MIN_SCHEMA_WIDTH};
use crate::datasource::QueryResult;
use crate::state::command::CommandBuffer;
use crate::state::focus::{Focus, Mode, PendingChord};
use crate::state::results::{ResultBlock, ResultCursor, ResultId, ResultPayload};
use crate::state::schema::ExpandOutcome;
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
    ToggleTheme,
    Worker(WorkerEvent),
}

pub enum CommandAction {
    Insert(char),
    Backspace,
    MoveLeft,
    MoveRight,
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
        Action::ToggleTheme => toggle_theme(app),
        Action::Worker(ev) => apply_worker_event(app, ev),
    }
}

fn resize_schema(app: &mut App, delta: i16) {
    let next = (app.schema.width as i32 + delta as i32)
        .clamp(MIN_SCHEMA_WIDTH as i32, MAX_SCHEMA_WIDTH as i32);
    app.schema.width = next as u16;
}

fn apply_command(app: &mut App, action: CommandAction) {
    let Mode::Command(buf) = &mut app.mode else {
        return;
    };
    match action {
        CommandAction::Insert(ch) => buf.insert(ch),
        CommandAction::Backspace => buf.backspace(),
        CommandAction::MoveLeft => buf.move_left(),
        CommandAction::MoveRight => buf.move_right(),
        CommandAction::Cancel => app.mode = Mode::Normal,
        CommandAction::Submit => submit_command(app),
    }
}

fn submit_command(app: &mut App) {
    let Mode::Command(buf) = &app.mode else {
        return;
    };
    let raw = buf.input.trim().to_string();
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
        _ => {
            app.status = QueryStatus::Failed {
                error: format!("unknown command: {cmd}"),
            };
        }
    }
}

fn set_schema_width(app: &mut App, args: &[&str]) {
    let Some(value) = args.first().and_then(|v| v.parse::<u16>().ok()) else {
        app.status = QueryStatus::Failed {
            error: "usage: :width <cols>".into(),
        };
        return;
    };
    app.schema.width = value.clamp(MIN_SCHEMA_WIDTH, MAX_SCHEMA_WIDTH);
}

fn set_theme(app: &mut App, args: &[&str]) {
    match args.first().copied() {
        None | Some("toggle") => toggle_theme(app),
        Some(name) => match ThemeKind::parse(name) {
            Some(k) => app.theme = Theme::for_kind(k),
            None => {
                app.status = QueryStatus::Failed {
                    error: format!("unknown theme: {name} (use dark|light|toggle)"),
                };
            }
        },
    }
}

fn toggle_theme(app: &mut App) {
    app.theme = Theme::for_kind(app.theme.kind.toggled());
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
    let req = app.requests.next();
    let _ = app.cmd_tx.send(WorkerCommand::Introspect { req, target });
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
    };
}

fn apply_result_nav(app: &mut App, nav: ResultNavAction) {
    let Mode::ResultExpanded { id, cursor } = &mut app.mode else {
        return;
    };
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
        WorkerEvent::SchemaLoaded {
            target, payload, ..
        } => on_schema_loaded(app, target, payload),
        WorkerEvent::SchemaFailed { target, error, .. } => {
            on_schema_failed(app, target, error.to_string())
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
    let payload = build_payload(result.rows, total_rows);
    let id = ResultId(app.results.len());
    let query = match &app.status {
        QueryStatus::Running { query, .. } => query.clone(),
        _ => String::new(),
    };

    app.results.push(ResultBlock {
        id,
        query,
        took,
        columns: result.columns,
        payload,
    });
    app.status = QueryStatus::Succeeded {
        rows: total_rows,
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
