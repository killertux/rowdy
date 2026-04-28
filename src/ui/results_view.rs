use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell as TableCell, Paragraph, Row as TableRow, Table, Widget,
};

use crate::datasource::Cell;
use crate::state::layout::TableLayout;
use crate::state::results::{ResultBlock, ResultCursor, SelectionRect, fit_columns};
use crate::ui::theme::Theme;

pub struct InlineResult<'a> {
    pub block: &'a ResultBlock,
    pub max_preview_rows: usize,
    pub theme: &'a Theme,
}

impl Widget for InlineResult<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let total_cols = self.block.columns.len();
        let block_widget = themed_block(self.theme, String::new(), false);
        let inner = block_widget.inner(area);

        let visible_cols = fit_columns(inner.width).min(total_cols.max(1));
        let title = inline_title(self.block, self.max_preview_rows, visible_cols, total_cols);
        let block_widget = themed_block(self.theme, title, false);
        block_widget.render(area, buf);

        if inner.height < 2 {
            return;
        }

        let table = build_table(
            self.block,
            None,
            0,
            self.max_preview_rows,
            0,
            visible_cols,
            self.theme,
            None,
        );
        let widths = column_widths(visible_cols);
        Widget::render(
            Table::new(table.rows, widths)
                .header(table.header)
                .style(Style::default().fg(self.theme.fg).bg(self.theme.bg)),
            inner,
            buf,
        );

        let total_rows = self.block.rows().len();
        let shown = total_rows.min(self.max_preview_rows);
        if shown < total_rows {
            let footer = Line::from(Span::styled(
                format!(
                    " ⤥ {} more rows — press <space>e to expand",
                    total_rows - shown
                ),
                Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
            ));
            let footer_area = Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(1),
                width: inner.width,
                height: 1,
            };
            Paragraph::new(footer).render(footer_area, buf);
        }
    }
}

pub struct ExpandedResult<'a> {
    pub block: &'a ResultBlock,
    pub cursor: ResultCursor,
    pub col_offset: usize,
    pub visible_cols: usize,
    pub row_offset: usize,
    pub visible_rows: usize,
    pub theme: &'a Theme,
    /// `Some` when Visual mode is active; the rectangle is highlighted
    /// in the grid and surfaced in the title bar.
    pub selection: Option<SelectionRect>,
}

impl Widget for ExpandedResult<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let total_cols = self.block.columns.len();
        let total_rows = self.block.rows().len();
        let title = expanded_title(
            self.block,
            self.cursor,
            self.col_offset,
            self.visible_cols,
            total_cols,
            self.row_offset,
            self.visible_rows,
            total_rows,
            self.selection,
        );
        let block_widget = themed_block(self.theme, title, true);
        let inner = block_widget.inner(area);
        block_widget.render(area, buf);

        // Reserve the bottom row for the cell-value badge so wide values
        // remain readable when the column they live in is narrower than them.
        let (table_area, badge_area) = if inner.height >= 2 {
            (
                Rect {
                    height: inner.height - 1,
                    ..inner
                },
                Some(Rect {
                    y: inner.y + inner.height - 1,
                    height: 1,
                    ..inner
                }),
            )
        } else {
            (inner, None)
        };

        let table = build_table(
            self.block,
            Some(self.cursor),
            self.row_offset,
            self.visible_rows,
            self.col_offset,
            self.visible_cols,
            self.theme,
            self.selection,
        );
        let widths = column_widths(self.visible_cols);
        Widget::render(
            Table::new(table.rows, widths)
                .header(table.header)
                .style(Style::default().fg(self.theme.fg).bg(self.theme.bg)),
            table_area,
            buf,
        );

        if let Some(badge_area) = badge_area {
            render_cell_badge(self.block, self.cursor, badge_area, self.theme, buf);
        }
    }
}

fn render_cell_badge(
    block: &ResultBlock,
    cursor: ResultCursor,
    area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
) {
    let col_name = block
        .columns
        .get(cursor.col)
        .map(|c| c.name.as_str())
        .unwrap_or("");
    let raw_value = block
        .rows()
        .get(cursor.row)
        .and_then(|r| r.get(cursor.col))
        .map(|c| c.display())
        .unwrap_or_default();
    // Flatten so a multi-line TEXT value stays on one line — it gets clipped
    // either way, but newlines would push the badge off its own row.
    let value: String = raw_value
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let raw = format!(" {col_name}: {value} ");
    let width = area.width as usize;
    let text = if raw.chars().count() > width {
        let take = width.saturating_sub(1);
        let mut s: String = raw.chars().take(take).collect();
        s.push('…');
        s
    } else {
        raw
    };
    let line = Line::from(Span::styled(
        text,
        Style::default().fg(theme.fg_dim).bg(theme.bg),
    ));
    Paragraph::new(line).render(area, buf);
}

struct BuiltTable<'a> {
    header: TableRow<'a>,
    rows: Vec<TableRow<'a>>,
}

#[allow(clippy::too_many_arguments)]
fn build_table<'a>(
    block: &'a ResultBlock,
    cursor: Option<ResultCursor>,
    row_offset: usize,
    visible_rows: usize,
    col_offset: usize,
    visible_cols: usize,
    theme: &Theme,
    selection: Option<SelectionRect>,
) -> BuiltTable<'a> {
    let col_end = (col_offset + visible_cols).min(block.columns.len());
    let header = TableRow::new(block.columns[col_offset..col_end].iter().map(|c| {
        TableCell::from(c.name.as_str()).style(
            Style::default()
                .fg(theme.header_fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        )
    }));
    let row_end = (row_offset + visible_rows).min(block.rows().len());
    let row_start = row_offset.min(row_end);
    let rows = block.rows()[row_start..row_end]
        .iter()
        .enumerate()
        .map(|(local, row)| {
            let absolute_row = row_offset + local;
            build_row(
                row,
                absolute_row,
                col_offset,
                col_end,
                cursor,
                theme,
                selection,
            )
        })
        .collect();
    BuiltTable { header, rows }
}

#[allow(clippy::too_many_arguments)]
fn build_row<'a>(
    row: &[Cell],
    absolute_row: usize,
    col_offset: usize,
    col_end: usize,
    cursor: Option<ResultCursor>,
    theme: &Theme,
    selection: Option<SelectionRect>,
) -> TableRow<'a> {
    // Slice defensively — a row that lost cells (driver bug or NULL handling
    // mismatch) shouldn't panic the renderer.
    let end = col_end.min(row.len());
    let start = col_offset.min(end);
    TableRow::new(row[start..end].iter().enumerate().map(|(local, value)| {
        let absolute_col = col_offset + local;
        let is_cursor =
            matches!(cursor, Some(cur) if cur.row == absolute_row && cur.col == absolute_col);
        let in_selection = selection
            .map(|s| s.contains(absolute_row, absolute_col))
            .unwrap_or(false);
        // Cursor wins over selection so the active cell stays distinguishable
        // even when it's inside the highlighted rectangle. We darken the
        // selection one notch (REVERSED) so the two layers stay visually
        // separable on every theme.
        let cell_style = if is_cursor {
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg)
        } else if in_selection {
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::DIM)
        } else if value.is_null() {
            Style::default().fg(theme.fg_dim).bg(theme.bg)
        } else {
            Style::default().fg(theme.fg).bg(theme.bg)
        };
        TableCell::from(value.display()).style(cell_style)
    }))
}

fn column_widths(n: usize) -> Vec<Constraint> {
    (0..n).map(|_| Constraint::Min(8)).collect()
}

/// Distribute the inner area across `n` columns the same way ratatui's
/// `Table` widget will, given `column_widths(n)` constraints and the
/// default 1-cell column spacing. Returns the cumulative X coordinates
/// where each visible column starts, plus a sentinel at the right edge —
/// i.e. a `Vec<u16>` of length `n + 1` such that column `i` spans
/// `[col_x[i], col_x[i+1])`. Hit-testing simply binary-searches into this.
fn distribute_columns(inner: Rect, n: usize) -> Vec<u16> {
    if n == 0 || inner.width == 0 {
        return Vec::new();
    }
    let constraints = column_widths(n);
    // ratatui's Table inserts a 1-cell gap between columns (the default
    // `column_spacing`); reproduce it via `Layout::spacing` so the boundaries
    // match exactly. The Layout solver handles all the over/underflow
    // arithmetic — we just read off the resulting rects.
    let parts = Layout::horizontal(constraints).spacing(1).split(inner);
    let mut out: Vec<u16> = parts.iter().map(|r| r.x).collect();
    if let Some(last) = parts.last() {
        out.push(last.x.saturating_add(last.width));
    }
    out
}

/// Layout for the inline preview table — the small one above the bottom
/// bar. No header/footer subtleties; just the same Table widget paint.
pub fn inline_layout(block: &ResultBlock, area: Rect) -> TableLayout {
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let total_cols = block.columns.len();
    let visible_cols = fit_columns(inner.width).min(total_cols.max(1));
    // Header takes one row, body fills the rest. There's also a footer line
    // ("⤥ N more rows…") painted on the bottom row when the preview is
    // truncated; we still consider that row part of the table for hit-testing
    // since clicks on the footer should fall through to inline-click semantics.
    let body_top_y = inner.y.saturating_add(1);
    let body_rows = inner.height.saturating_sub(1);
    let col_x = distribute_columns(inner, visible_cols);
    TableLayout {
        area,
        body_top_y,
        body_rows,
        col_x,
        col_offset: 0,
        row_offset: 0,
    }
}

/// Layout for the full-screen expanded result. `visible_cols` and
/// `visible_rows` come from `ui::render`'s clamp pass.
pub fn expanded_layout(
    block: &ResultBlock,
    area: Rect,
    col_offset: usize,
    visible_cols: usize,
    row_offset: usize,
    visible_rows: usize,
) -> TableLayout {
    let _ = block;
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    // Header takes one row at top, the cell-value badge takes one row at
    // bottom (when inner.height >= 2). Body fills the rest.
    let body_top_y = inner.y.saturating_add(1);
    let body_rows = (visible_rows as u16).min(inner.height.saturating_sub(2).max(1));
    let col_x = distribute_columns(inner, visible_cols);
    TableLayout {
        area,
        body_top_y,
        body_rows,
        col_x,
        col_offset,
        row_offset,
    }
}

fn inline_title(
    block: &ResultBlock,
    max_preview_rows: usize,
    visible_cols: usize,
    total_cols: usize,
) -> String {
    let shown_rows = block.rows().len().min(max_preview_rows);
    let cols = if visible_cols < total_cols {
        format!(
            " — {visible_cols}/{total_cols} cols (+{} →)",
            total_cols - visible_cols
        )
    } else {
        format!(" — {total_cols} cols")
    };
    format!(
        " result #{} — {} preview / {} total{} — {:?} ",
        block.id.0 + 1,
        shown_rows,
        block.total_rows(),
        cols,
        block.took,
    )
}

#[allow(clippy::too_many_arguments)]
fn expanded_title(
    block: &ResultBlock,
    cursor: ResultCursor,
    col_offset: usize,
    visible_cols: usize,
    total_cols: usize,
    row_offset: usize,
    visible_rows: usize,
    total_rows: usize,
    selection: Option<SelectionRect>,
) -> String {
    let cols_end = (col_offset + visible_cols).min(total_cols);
    let rows_end = (row_offset + visible_rows).min(total_rows);
    let cols_left = if col_offset > 0 { "‹ " } else { "" };
    let cols_right = if cols_end < total_cols { " ›" } else { "" };
    let rows_up = if row_offset > 0 { "↑ " } else { "" };
    let rows_down = if rows_end < total_rows { " ↓" } else { "" };
    let visual = match selection {
        Some(s) => format!(" — VISUAL · {}×{}", s.rows(), s.cols()),
        None => String::new(),
    };
    format!(
        " result #{} — {}rows {}-{} of {}{} (loaded {}) — {}cols {}-{} of {}{} — cell ({}, {}){} — q/Esc to close ",
        block.id.0 + 1,
        rows_up,
        row_offset + 1,
        rows_end,
        total_rows,
        rows_down,
        block.total_rows(),
        cols_left,
        col_offset + 1,
        cols_end,
        total_cols,
        cols_right,
        cursor.row + 1,
        cursor.col + 1,
        visual,
    )
}

fn themed_block<'a>(theme: &Theme, title: String, focused: bool) -> Block<'a> {
    let border = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border).bg(theme.bg))
        .title(title)
        .title_style(Style::default().fg(theme.fg).bg(theme.bg))
        .style(Style::default().bg(theme.bg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, w: u16) -> Rect {
        Rect {
            x,
            y: 0,
            width: w,
            height: 1,
        }
    }

    #[test]
    fn distribute_columns_returns_n_plus_one_xs() {
        let xs = distribute_columns(rect(0, 30), 3);
        assert_eq!(xs.len(), 4);
        // Boundaries are strictly increasing.
        assert!(xs.windows(2).all(|w| w[1] > w[0]));
        // Right edge equals inner.x + inner.width.
        assert_eq!(*xs.last().unwrap(), 30);
        assert_eq!(xs[0], 0);
    }

    #[test]
    fn distribute_columns_offsets_by_inner_x() {
        let xs = distribute_columns(rect(10, 30), 3);
        assert_eq!(xs[0], 10);
        assert_eq!(*xs.last().unwrap(), 40);
    }

    #[test]
    fn distribute_columns_zero_cols_is_empty() {
        assert!(distribute_columns(rect(0, 18), 0).is_empty());
    }
}
