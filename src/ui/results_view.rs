use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell as TableCell, Paragraph, Row as TableRow, Table, Widget,
};

use crate::datasource::Cell;
use crate::state::results::{ResultBlock, ResultCursor, ResultPayload, fit_columns};
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
        );
        let widths = column_widths(visible_cols);
        Widget::render(
            Table::new(table.rows, widths)
                .header(table.header)
                .style(Style::default().fg(self.theme.fg).bg(self.theme.bg)),
            inner,
            buf,
        );

        if let ResultPayload::Clipped {
            total_rows,
            preview,
        } = &self.block.payload
            && preview.len() < *total_rows
        {
            let footer = Line::from(Span::styled(
                format!(
                    " ⤥ {} more rows — press <space>e to expand",
                    total_rows - preview.len()
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
        );
        let block_widget = themed_block(self.theme, title, true);
        let inner = block_widget.inner(area);
        block_widget.render(area, buf);

        let table = build_table(
            self.block,
            Some(self.cursor),
            self.row_offset,
            self.visible_rows,
            self.col_offset,
            self.visible_cols,
            self.theme,
        );
        let widths = column_widths(self.visible_cols);
        Widget::render(
            Table::new(table.rows, widths)
                .header(table.header)
                .style(Style::default().fg(self.theme.fg).bg(self.theme.bg)),
            inner,
            buf,
        );
    }
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
            build_row(row, absolute_row, col_offset, col_end, cursor, theme)
        })
        .collect();
    BuiltTable { header, rows }
}

fn build_row<'a>(
    row: &[Cell],
    absolute_row: usize,
    col_offset: usize,
    col_end: usize,
    cursor: Option<ResultCursor>,
    theme: &Theme,
) -> TableRow<'a> {
    // Slice defensively — a row that lost cells (driver bug or NULL handling
    // mismatch) shouldn't panic the renderer.
    let end = col_end.min(row.len());
    let start = col_offset.min(end);
    TableRow::new(row[start..end].iter().enumerate().map(|(local, value)| {
        let absolute_col = col_offset + local;
        let cell_style = if matches!(cursor, Some(cur) if cur.row == absolute_row && cur.col == absolute_col) {
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg)
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

fn inline_title(
    block: &ResultBlock,
    max_preview_rows: usize,
    visible_cols: usize,
    total_cols: usize,
) -> String {
    let shown_rows = block.rows().len().min(max_preview_rows);
    let cols = if visible_cols < total_cols {
        format!(" — {visible_cols}/{total_cols} cols (+{} →)", total_cols - visible_cols)
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
) -> String {
    let cols_end = (col_offset + visible_cols).min(total_cols);
    let rows_end = (row_offset + visible_rows).min(total_rows);
    let cols_left = if col_offset > 0 { "‹ " } else { "" };
    let cols_right = if cols_end < total_cols { " ›" } else { "" };
    let rows_up = if row_offset > 0 { "↑ " } else { "" };
    let rows_down = if rows_end < total_rows { " ↓" } else { "" };
    format!(
        " result #{} — {}rows {}-{} of {}{} (loaded {}) — {}cols {}-{} of {}{} — cell ({}, {}) — q/Esc to close ",
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
