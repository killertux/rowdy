//! Result-grid interactions: drag selection, navigation, visual mode,
//! yank, and CSV/TSV/JSON/SQL export. Used by both the expanded grid
//! screen (`Screen::ResultExpanded`) and the inline preview.

use crate::app::App;
use crate::clipboard;
use crate::datasource::{Cell, Column};
use crate::export::{self, ExportFormat};
use crate::state::layout::DragState;
use crate::state::results::{ResultBlock, ResultCursor, ResultId, ResultViewMode, SelectionRect};
use crate::state::screen::Screen;
use crate::state::status::QueryStatus;

use super::{ExportTarget, ResultColumnAction, ResultNavAction};

pub(super) fn result_drag_start(app: &mut App, row: usize, col: usize) {
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

pub(super) fn result_drag_to(app: &mut App, row: usize, col: usize) {
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

pub(super) fn result_drag_end(app: &mut App) {
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

pub(super) fn result_scroll(app: &mut App, delta: i32) {
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

pub(super) fn inline_result_jump(app: &mut App, row: usize, col: usize) {
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
        column_view: crate::state::results::ColumnView::new(max_cols),
    };
}

pub(super) fn expand_latest(app: &mut App) {
    let Some(block) = app.results.last() else {
        app.status = QueryStatus::Failed {
            error: "no results to expand".into(),
        };
        return;
    };
    let total_cols = block.columns.len();
    app.screen = Screen::ResultExpanded {
        id: block.id,
        cursor: ResultCursor::default(),
        col_offset: 0,
        row_offset: 0,
        view: ResultViewMode::Normal,
        column_view: crate::state::results::ColumnView::new(total_cols),
    };
}

/// User-driven dismiss of the inline result preview. Doesn't touch
/// `app.results` so `:expand` can still pull the same block back up;
/// the next `dispatch_query` un-hides automatically.
pub(super) fn dismiss_result(app: &mut App) {
    if app.results.last().is_none() {
        app.status = QueryStatus::Failed {
            error: "no result preview to close".into(),
        };
        return;
    }
    app.preview_hidden = true;
}

pub(super) fn apply_result_column(app: &mut App, op: ResultColumnAction) {
    let Screen::ResultExpanded {
        cursor,
        view,
        column_view,
        ..
    } = &mut app.screen
    else {
        return;
    };
    // Reordering invalidates a Visual rectangle (anchor and cursor are
    // physical column indices, but the user's selection was visual);
    // drop back to Normal so we don't leave a stale highlight on the grid.
    if matches!(view, ResultViewMode::Visual { .. }) {
        *view = ResultViewMode::Normal;
    }
    // Locked while the format prompt is open — mirrors the nav guard.
    if matches!(view, ResultViewMode::YankFormat { .. }) {
        return;
    }
    match op {
        ResultColumnAction::MoveLeft => column_view.move_left(cursor.col),
        ResultColumnAction::MoveRight => column_view.move_right(cursor.col),
        ResultColumnAction::Hide => {
            if let Some(next_col) = column_view.hide(cursor.col) {
                cursor.col = next_col;
            } else {
                app.status = QueryStatus::Failed {
                    error: "can't hide the last visible column".into(),
                };
            }
        }
        ResultColumnAction::Reset => column_view.reset(),
    }
}

pub(super) fn apply_result_nav(app: &mut App, nav: ResultNavAction) {
    let Screen::ResultExpanded {
        id,
        cursor,
        view,
        column_view,
        ..
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
    apply_nav_step(cursor, nav, max_rows, column_view.visible());
}

pub(super) fn apply_nav_step(
    cursor: &mut ResultCursor,
    nav: ResultNavAction,
    max_rows: usize,
    visible: &[usize],
) {
    if visible.is_empty() {
        return;
    }
    let visual = visible.iter().position(|&p| p == cursor.col).unwrap_or(0);
    match nav {
        ResultNavAction::Left => {
            if visual > 0 {
                cursor.jump_to(cursor.row, visible[visual - 1]);
            }
        }
        ResultNavAction::Right => {
            if visual + 1 < visible.len() {
                cursor.jump_to(cursor.row, visible[visual + 1]);
            }
        }
        ResultNavAction::Up => {
            if cursor.row > 0 {
                cursor.row -= 1;
            }
        }
        ResultNavAction::Down => {
            if cursor.row + 1 < max_rows {
                cursor.row += 1;
            }
        }
        ResultNavAction::LineStart => cursor.jump_to(cursor.row, visible[0]),
        ResultNavAction::LineEnd => cursor.jump_to(cursor.row, *visible.last().unwrap()),
        ResultNavAction::Top => cursor.jump_to(0, cursor.col),
        ResultNavAction::Bottom => cursor.jump_to(max_rows.saturating_sub(1), cursor.col),
    }
}

pub(super) fn result_enter_visual(app: &mut App) {
    let Screen::ResultExpanded { cursor, view, .. } = &mut app.screen else {
        return;
    };
    if matches!(view, ResultViewMode::Normal) {
        *view = ResultViewMode::Visual { anchor: *cursor };
    }
}

pub(super) fn result_exit_visual(app: &mut App) {
    let Screen::ResultExpanded { view, .. } = &mut app.screen else {
        return;
    };
    *view = ResultViewMode::Normal;
}

pub(super) fn result_yank(app: &mut App) {
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
                .map(|cell| cell.display().into_owned())
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

pub(super) fn result_yank_format(app: &mut App, fmt: ExportFormat) {
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

pub(super) fn result_cancel_yank_format(app: &mut App) {
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
pub(super) fn export_sql_command(app: &mut App, table: Option<String>, target: ExportTarget) {
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

pub(super) fn export_command(app: &mut App, fmt: ExportFormat, target: ExportTarget) {
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
