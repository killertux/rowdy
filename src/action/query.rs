//! Query lifecycle: confirm prompts, dispatch (with destructive +
//! placeholder guardrails), cancellation, and the worker-event landing
//! sites for `QueryDone` / `QueryFailed`.

use std::time::Instant;

use crate::app::App;
use crate::datasource::QueryResult;
use crate::state::overlay::Overlay;
use crate::state::results::{ResultBlock, ResultId};
use crate::state::status::QueryStatus;
use crate::worker::WorkerCommand;

pub(super) fn prepare_confirm_run(app: &mut App) {
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
        reason: crate::state::overlay::ConfirmRunReason::Manual,
    });
}

pub(super) fn confirm_run_submit(app: &mut App) {
    let Some(Overlay::ConfirmRun { statement, .. }) = app.overlay.take() else {
        return;
    };
    crate::state::editor::clear_confirm_highlight(&mut app.editor.state);
    dispatch_query(app, statement);
}

pub(super) fn confirm_run_cancel(app: &mut App) {
    if !matches!(app.overlay, Some(Overlay::ConfirmRun { .. })) {
        return;
    }
    app.overlay = None;
    crate::state::editor::clear_confirm_highlight(&mut app.editor.state);
}

pub(super) fn run_statement_under_cursor(app: &mut App) {
    let Some(range) = crate::state::editor::statement_under_cursor(&app.editor.state) else {
        app.status = QueryStatus::Failed {
            error: "no statement under cursor".into(),
        };
        return;
    };
    dispatch_query(app, range.text);
}

pub(super) fn run_selection(app: &mut App) {
    let Some(text) = crate::state::editor::selection_text(&app.editor.state) else {
        app.status = QueryStatus::Failed {
            error: "no selection to run".into(),
        };
        return;
    };
    dispatch_query(app, text);
}

pub(super) fn cancel_query(app: &mut App) {
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

pub(super) fn dispatch_query(app: &mut App, sql: String) {
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
    // Destructive-statement guardrail: bare UPDATE/DELETE without WHERE
    // and any TRUNCATE bounce through a confirm overlay. Reuses the
    // `<leader>r` confirm machinery — Enter dispatches the held SQL
    // (which lands back here, but the overlay is gone by then so we
    // don't loop). Skipped when the user already passed through a
    // manual confirm (overlay is consumed before re-dispatch).
    if app.overlay.is_none()
        && let Some(dialect) = destructive_dialect(app)
        && let Some(reason) =
            crate::datasource::sql::requires_destructive_confirmation(&trimmed, dialect.as_ref())
    {
        app.overlay = Some(Overlay::ConfirmRun {
            statement: trimmed,
            reason: crate::state::overlay::ConfirmRunReason::Destructive(reason),
        });
        return;
    }
    // Placeholders (`$N` / `:name`) bounce through a popup so the user
    // can fill values in. The original (unsubstituted) statement
    // stays on the overlay; the substituted form is what we eventually
    // send to the worker via `send_to_worker`.
    if app.overlay.is_none()
        && let Some(dialect) = destructive_dialect(app)
    {
        let scan = crate::datasource::sql::placeholders::scan(&trimmed, dialect.as_ref());
        let unique = crate::datasource::sql::placeholders::unique_params(&scan);
        if !unique.is_empty() {
            open_params_prompt(app, trimmed, scan, unique);
            return;
        }
    }
    send_to_worker(app, trimmed);
}

/// Tail of `dispatch_query` — marks the query in flight and hands the
/// (already finalised) SQL to the worker. Extracted so the params
/// popup's Submit handler can reuse the exact same logging / status /
/// in-flight bookkeeping after substitution.
pub(super) fn send_to_worker(app: &mut App, sql: String) {
    app.preview_hidden = false;
    let req = app.requests.next();
    app.in_flight_query = Some(crate::app::InFlightQuery {
        req,
        sql: sql.clone(),
    });
    app.status = QueryStatus::Running {
        query: sql.clone(),
        started_at: Instant::now(),
    };
    let _ = app.cmd_tx.send(WorkerCommand::Execute { req, sql });
}

fn open_params_prompt(
    app: &mut App,
    statement: String,
    placeholders: Vec<crate::datasource::sql::placeholders::Placeholder>,
    keys: Vec<crate::datasource::sql::placeholders::ParamKey>,
) {
    // Pre-fill from per-connection history when we have a match for the
    // exact same statement text. No active connection → no history.
    let prefill_map = app
        .active_connection
        .clone()
        .and_then(|conn| crate::param_history::lookup(app, &conn, &statement));

    let state =
        crate::state::params_prompt::ParamsPromptState::new(statement, placeholders, keys, |key| {
            prefill_map
                .as_ref()
                .and_then(|m| m.get(&key.label()).cloned())
        });
    app.overlay = Some(Overlay::ParamsPrompt(state));
}

/// Pick a sqlparser dialect to feed `requires_destructive_confirmation`.
/// Falls back to `Generic` when no connection is active so the guardrail
/// still fires for queries typed before connecting (rare but possible).
fn destructive_dialect(app: &App) -> Option<Box<dyn sqlparser::dialect::Dialect>> {
    use crate::datasource::DriverKind;
    let kind = app.active_dialect.unwrap_or(DriverKind::Sqlite);
    Some(match kind {
        DriverKind::Postgres => Box::new(sqlparser::dialect::PostgreSqlDialect {}),
        DriverKind::Mysql => Box::new(sqlparser::dialect::MySqlDialect {}),
        DriverKind::Sqlite => Box::new(sqlparser::dialect::SQLiteDialect {}),
    })
}

pub(super) fn on_query_done(app: &mut App, req: crate::worker::RequestId, result: QueryResult) {
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
        // Coalesce back-to-back DDLs: if a prior reload hasn't reported
        // `CacheStage::Reloaded` yet, skip this one. The in-flight
        // reload's final stage already covers the new schema state, so
        // queueing a second pass just doubles the introspection cost on
        // large catalogs (a real freeze symptom we've hit in practice).
        if !app.schema_reload_in_flight {
            app.schema_reload_in_flight = true;
            let _ = app.cmd_tx.send(WorkerCommand::Reload { connection: name });
        }
    }

    let took = result.elapsed;
    let total_rows = result.rows.len();
    let affected = result.affected;

    // Statements run via `execute()` (DML/DDL) report no columns — there's
    // nothing to render in a result block, so skip pushing one. Also hide
    // the inline preview so a stale grid from an earlier SELECT doesn't
    // linger on screen after a `DELETE`/`UPDATE` lands.
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
    } else {
        app.preview_hidden = true;
    }

    app.status = QueryStatus::Succeeded {
        rows: total_rows,
        affected,
        took,
        statements_run: result.statements_run.max(1),
    };
}

pub(super) fn on_query_failed(app: &mut App, req: crate::worker::RequestId, error: String) {
    let Some(in_flight) = app.in_flight_query.as_ref() else {
        return;
    };
    if in_flight.req != req {
        return;
    }
    app.in_flight_query = None;
    app.status = QueryStatus::Failed { error };
}
