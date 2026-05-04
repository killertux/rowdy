//! Modal theme-picker overlay. Two panes: the bundled theme list on the
//! left grouped by Dark/Light, a fixed sample SQL + result table preview
//! on the right. The hovered theme drives the colors of *both* panes so
//! the user sees what selecting it would look like.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::state::theme_picker::{ThemePickerItem, ThemePickerState};
use crate::ui::theme::{Theme, ThemeKind};

pub struct ThemePicker<'a> {
    pub state: &'a ThemePickerState,
    pub theme: &'a Theme,
}

impl Widget for ThemePicker<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(box_area) = inner_box(area) else {
            return;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(self.theme.border_focus)
                    .bg(self.theme.bg),
            )
            .title(" :theme — pick a theme ")
            .title_style(
                Style::default()
                    .fg(self.theme.fg)
                    .bg(self.theme.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(self.theme.bg));
        let inner = block.inner(box_area);
        block.render(box_area, buf);

        let chunks =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
        let body_area = chunks[0];
        let footer_area = chunks[1];

        let split = Layout::horizontal([
            Constraint::Percentage(45),
            Constraint::Percentage(55),
        ])
        .split(body_area);

        render_list(self.state, self.theme, split[0], buf);
        render_preview(self.theme, split[1], buf);

        let footer = Line::from(Span::styled(
            "j/k arrows · g/G top/bottom · Enter apply · Esc cancel",
            Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
        ));
        Paragraph::new(footer).render(footer_area, buf);
    }
}

fn render_list(state: &ThemePickerState, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let lines = build_list_lines(state, theme, area.width as usize);
    Paragraph::new(lines)
        .style(Style::default().fg(theme.fg).bg(theme.bg))
        .render(area, buf);
}

fn build_list_lines<'a>(
    state: &'a ThemePickerState,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut last_kind: Option<ThemeKind> = None;
    for (idx, item) in state.items.iter().enumerate() {
        if Some(item.kind) != last_kind {
            if last_kind.is_some() {
                lines.push(Line::from(""));
            }
            let header = match item.kind {
                ThemeKind::Dark => "── Dark ──",
                ThemeKind::Light => "── Light ──",
            };
            lines.push(Line::from(Span::styled(
                header.to_string(),
                Style::default()
                    .fg(theme.header_fg)
                    .bg(theme.bg)
                    .add_modifier(Modifier::BOLD),
            )));
            last_kind = Some(item.kind);
        }
        lines.push(build_row(item, state, theme, idx, width));
    }
    lines
}

fn build_row<'a>(
    item: &'a ThemePickerItem,
    state: &ThemePickerState,
    theme: &Theme,
    idx: usize,
    width: usize,
) -> Line<'a> {
    let is_hovered = idx == state.cursor;
    let is_current = item.name == state.current_theme_name;
    let prefix = if is_current { " * " } else { "   " };
    let label = item.name.as_str();
    let suffix = item
        .source_path
        .as_deref()
        .map(|p| format!(" ({p})"))
        .unwrap_or_default();
    let content_chars = prefix.chars().count() + label.chars().count() + suffix.chars().count();
    let pad = width.saturating_sub(content_chars);
    let pad_str: String = " ".repeat(pad);
    let row_style = if is_hovered {
        Style::default()
            .fg(theme.selection_fg)
            .bg(theme.selection_bg)
    } else if is_current {
        Style::default().fg(theme.header_fg).bg(theme.bg)
    } else {
        Style::default().fg(theme.fg).bg(theme.bg)
    };
    Line::from(vec![Span::styled(
        format!("{prefix}{label}{suffix}{pad_str}"),
        row_style,
    )])
}

fn render_preview(theme: &Theme, area: Rect, buf: &mut Buffer) {
    // Border separator + padded inner so the preview reads as its own pane.
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.border).bg(theme.bg))
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    block.render(area, buf);

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Preview",
        Style::default()
            .fg(theme.header_fg)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    for sql_line in SAMPLE_SQL.lines() {
        let span = if sql_line.trim_start().starts_with("--") {
            Span::styled(sql_line, Style::default().fg(theme.fg_dim).bg(theme.bg))
        } else {
            Span::styled(sql_line, Style::default().fg(theme.fg).bg(theme.bg))
        };
        lines.push(Line::from(span));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Result:",
        Style::default()
            .fg(theme.header_fg)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    )));
    for (i, row) in SAMPLE_TABLE.iter().enumerate() {
        let style = if i == 0 {
            Style::default()
                .fg(theme.header_fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD)
        } else if i == 2 {
            // Highlight one row to demonstrate selection_bg.
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg)
        } else {
            Style::default().fg(theme.fg).bg(theme.bg)
        };
        lines.push(Line::from(Span::styled(*row, style)));
    }
    Paragraph::new(lines)
        .style(Style::default().bg(theme.bg))
        .render(inner, buf);
}

const SAMPLE_SQL: &str = "-- Sample query\nSELECT id, name, active, created_at\nFROM users\nWHERE active = TRUE\nORDER BY created_at DESC;";

const SAMPLE_TABLE: &[&str] = &[
    "| id | name  | active | created_at          |",
    "| 1  | Alice | true   | 2026-01-01 00:00:00 |",
    "| 2  | Bob   | false  | 2026-02-15 12:34:56 |",
    "| 3  | NULL  | true   | 2026-03-21 09:00:12 |",
];

pub fn inner_box(area: Rect) -> Option<Rect> {
    let width = area.width.min(96);
    let height = area.height.saturating_sub(2);
    if width < 60 || height < 14 {
        return None;
    }
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Some(Rect {
        x,
        y,
        width,
        height,
    })
}
