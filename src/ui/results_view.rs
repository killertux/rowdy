use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell as TableCell, Paragraph, Row as TableRow, Table, Widget,
};

use crate::datasource::Cell;
use crate::state::results::{ResultBlock, ResultCursor, ResultPayload};
use crate::ui::theme::Theme;

pub struct InlineResult<'a> {
    pub block: &'a ResultBlock,
    pub max_preview_rows: usize,
    pub theme: &'a Theme,
}

impl Widget for InlineResult<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = format!(
            " result #{} — {} preview / {} total — {:?} ",
            self.block.id.0 + 1,
            self.block.rows().len().min(self.max_preview_rows),
            self.block.total_rows(),
            self.block.took,
        );
        let block_widget = themed_block(self.theme, title, false);

        let inner = block_widget.inner(area);
        block_widget.render(area, buf);

        if inner.height < 2 {
            return;
        }

        let table = build_table(self.block, None, self.max_preview_rows, self.theme);
        let widths = column_widths(self.block.columns.len());
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
    pub theme: &'a Theme,
}

impl Widget for ExpandedResult<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = format!(
            " result #{} — {} rows shown / {} total — cell ({}, {}) — q/Esc to close ",
            self.block.id.0 + 1,
            self.block.rows().len(),
            self.block.total_rows(),
            self.cursor.row + 1,
            self.cursor.col + 1,
        );
        let block_widget = themed_block(self.theme, title, true);
        let inner = block_widget.inner(area);
        block_widget.render(area, buf);

        let max_rows = inner.height.saturating_sub(1) as usize;
        let table = build_table(self.block, Some(self.cursor), max_rows, self.theme);
        let widths = column_widths(self.block.columns.len());
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

fn build_table<'a>(
    block: &'a ResultBlock,
    cursor: Option<ResultCursor>,
    max_rows: usize,
    theme: &Theme,
) -> BuiltTable<'a> {
    let header = TableRow::new(block.columns.iter().map(|c| {
        TableCell::from(c.name.as_str()).style(
            Style::default()
                .fg(theme.header_fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        )
    }));
    let visible = block.rows().iter().take(max_rows.max(1));
    let rows = visible
        .enumerate()
        .map(|(r, row)| build_row(row, r, cursor, theme))
        .collect();
    BuiltTable { header, rows }
}

fn build_row<'a>(
    row: &[Cell],
    r: usize,
    cursor: Option<ResultCursor>,
    theme: &Theme,
) -> TableRow<'a> {
    TableRow::new(row.iter().enumerate().map(|(c, value)| {
        let cell_style = if matches!(cursor, Some(cur) if cur.row == r && cur.col == c) {
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
