use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell as TableCell, Paragraph, Row as TableRow, Table, Widget,
};

use crate::datasource::Cell;
use crate::state::layout::TableLayout;
use crate::state::results::{ColumnView, ResultBlock, ResultCursor, SelectionRect, fit_columns};
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

        // Inline preview ignores column-view ops — the user reaches it
        // outside the expanded screen, where reorder/hide aren't bound.
        let identity: Vec<usize> = (0..visible_cols).collect();
        let table = build_table(
            self.block,
            None,
            0,
            self.max_preview_rows,
            &identity,
            self.theme,
            None,
        );
        let widths = column_widths(visible_cols, inner.width);
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
    /// Per-grid column reorder + hide state. Render walks
    /// `column_view.visible()[col_offset..col_offset+visible_cols]`
    /// rather than a contiguous slice of `block.columns`.
    pub column_view: &'a ColumnView,
}

impl Widget for ExpandedResult<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let total_cols = self.column_view.visible().len();
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

        let visible_slice = visible_slice(self.column_view, self.col_offset, self.visible_cols);
        let table = build_table(
            self.block,
            Some(self.cursor),
            self.row_offset,
            self.visible_rows,
            visible_slice,
            self.theme,
            self.selection,
        );
        let widths = column_widths(self.visible_cols, table_area.width);
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

fn visible_slice(view: &ColumnView, col_offset: usize, visible_cols: usize) -> &[usize] {
    let v = view.visible();
    let end = (col_offset + visible_cols).min(v.len());
    let start = col_offset.min(end);
    &v[start..end]
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
    let raw_value: std::borrow::Cow<'_, str> = block
        .rows()
        .get(cursor.row)
        .and_then(|r| r.get(cursor.col))
        .map(|c| c.display())
        .unwrap_or(std::borrow::Cow::Borrowed(""));
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

fn build_table<'a>(
    block: &'a ResultBlock,
    cursor: Option<ResultCursor>,
    row_offset: usize,
    visible_rows: usize,
    visible_cols: &[usize],
    theme: &Theme,
    selection: Option<SelectionRect>,
) -> BuiltTable<'a> {
    let header = TableRow::new(visible_cols.iter().map(|&physical| {
        let name = block
            .columns
            .get(physical)
            .map(|c| c.name.as_str())
            .unwrap_or("");
        TableCell::from(name).style(
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
            build_row(row, absolute_row, visible_cols, cursor, theme, selection)
        })
        .collect();
    BuiltTable { header, rows }
}

fn build_row<'a>(
    row: &'a [Cell],
    absolute_row: usize,
    visible_cols: &[usize],
    cursor: Option<ResultCursor>,
    theme: &Theme,
    selection: Option<SelectionRect>,
) -> TableRow<'a> {
    TableRow::new(visible_cols.iter().map(|&physical| {
        // Slice defensively — a row that lost cells (driver bug or NULL
        // handling mismatch) shouldn't panic the renderer.
        let value = match row.get(physical) {
            Some(v) => v,
            None => return TableCell::from("").style(Style::default().fg(theme.fg).bg(theme.bg)),
        };
        let absolute_col = physical;
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

/// Per-column widths summing (with the 1-cell column gaps) to
/// `inner_width`. Returned as `Constraint::Length` rather than `Min`
/// because ratatui's `Table` runs the kasuari solver over its column
/// constraints, and kasuari hangs (issue #18) when fed many identical
/// `Min(_)` constraints — the simplex pivots cycle on the degenerate
/// ties. Pinning each column to an explicit `Length` keeps the solver
/// off the pathological path entirely. We do the even-split arithmetic
/// here so the result still scales with terminal width.
fn column_widths(n: usize, inner_width: u16) -> Vec<Constraint> {
    if n == 0 {
        return Vec::new();
    }
    even_split(inner_width, n)
        .into_iter()
        .map(Constraint::Length)
        .collect()
}

/// Split `total` into `n` integer pieces, accounting for the 1-cell
/// gaps that sit *between* columns (so `n - 1` gaps total). The
/// remainder is distributed across the leading pieces, mirroring how
/// ratatui's Layout solver would have spread a leftover cell. If the
/// available width is smaller than the gaps alone, every column is
/// forced to width 0 — still well-defined, just nothing to render.
fn even_split(total: u16, n: usize) -> Vec<u16> {
    debug_assert!(n > 0);
    let n_u = n as u32;
    let gaps = n_u.saturating_sub(1);
    let content = (total as u32).saturating_sub(gaps);
    let base = content / n_u;
    let extra = (content % n_u) as usize;
    (0..n)
        .map(|i| {
            let w = if i < extra { base + 1 } else { base };
            w as u16
        })
        .collect()
}

/// Distribute the inner area across `n` columns the same way ratatui's
/// `Table` widget will, given `column_widths(n, inner.width)` and the
/// default 1-cell column spacing. Returns the cumulative X coordinates
/// where each visible column starts, plus a sentinel at the right edge —
/// i.e. a `Vec<u16>` of length `n + 1` such that column `i` spans
/// `[col_x[i], col_x[i+1])`. Hit-testing simply binary-searches into this.
fn distribute_columns(inner: Rect, n: usize) -> Vec<u16> {
    if n == 0 || inner.width == 0 {
        return Vec::new();
    }
    let widths = even_split(inner.width, n);
    let mut out: Vec<u16> = Vec::with_capacity(n + 1);
    let mut x = inner.x;
    for w in &widths {
        out.push(x);
        x = x.saturating_add(*w).saturating_add(1); // +1 for column gap
    }
    // Last entry was advanced past a trailing gap that doesn't exist;
    // back it out so the sentinel sits flush with the right edge of
    // the final column (matching the original Layout-derived behaviour).
    let last_width = widths.last().copied().unwrap_or(0);
    let last_x = *out.last().unwrap_or(&inner.x);
    out.push(last_x.saturating_add(last_width));
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
