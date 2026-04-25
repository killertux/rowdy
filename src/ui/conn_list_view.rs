use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::state::conn_list::ConnListState;
use crate::ui::theme::Theme;

pub struct ConnList<'a> {
    pub state: &'a ConnListState,
    pub active: Option<&'a str>,
    pub theme: &'a Theme,
}

impl Widget for ConnList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(box_area) = inner_box(area, self.state.entries.len()) else {
            return;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border_focus).bg(self.theme.bg))
            .title(" rowdy — connections ")
            .title_style(
                Style::default()
                    .fg(self.theme.fg)
                    .bg(self.theme.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(self.theme.bg));
        let inner = block.inner(box_area);
        block.render(box_area, buf);

        // Vertical chunks: list (variable) | blank (1) | footer (2 — wrapped).
        let entries_h = (self.state.entries.len() as u16).max(1);
        let chunks = Layout::vertical([
            Constraint::Length(entries_h),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(inner);

        let entry_lines: Vec<Line> = self
            .state
            .entries
            .iter()
            .enumerate()
            .map(|(i, name)| {
                entry_line(
                    name,
                    i == self.state.selected,
                    Some(name.as_str()) == self.active,
                    self.theme,
                )
            })
            .collect();
        Paragraph::new(entry_lines)
            .style(Style::default().fg(self.theme.fg).bg(self.theme.bg))
            .render(chunks[0], buf);

        let (footer_text, footer_style) = match &self.state.pending_delete {
            Some(name) => (
                format!("Delete {name:?}? y to confirm · n/Esc to cancel"),
                Style::default()
                    .fg(self.theme.status_error)
                    .bg(self.theme.bg)
                    .add_modifier(Modifier::BOLD),
            ),
            None => (
                "j/k move · Enter use · a add · e edit · d delete · Esc close".to_string(),
                Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
            ),
        };
        Paragraph::new(footer_text)
            .style(footer_style)
            .wrap(Wrap { trim: true })
            .render(chunks[2], buf);
    }
}

fn entry_line<'a>(name: &str, selected: bool, active: bool, theme: &Theme) -> Line<'a> {
    let marker = if active { "● " } else { "  " };
    let suffix = if active { "  (active)" } else { "" };
    let bg = if selected { theme.selection_bg } else { theme.bg };
    let fg = if selected {
        theme.selection_fg
    } else if active {
        theme.status_ok
    } else {
        theme.fg
    };
    Line::from(vec![
        Span::styled(marker.to_string(), Style::default().fg(fg).bg(bg)),
        Span::styled(
            name.to_string(),
            Style::default().fg(fg).bg(bg).add_modifier(if active {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        Span::styled(
            suffix.to_string(),
            Style::default().fg(theme.fg_dim).bg(bg),
        ),
    ])
}

/// Centered box sized to fit the list + footer comfortably. Footer needs 3
/// rows (blank + 2 wrapped lines) on top of the entries; borders eat 2 more.
fn inner_box(area: Rect, entry_count: usize) -> Option<Rect> {
    let width = area.width.min(72);
    let needed_inner = (entry_count.max(1) as u16).saturating_add(3);
    let needed = needed_inner.saturating_add(2); // borders
    let height = needed.clamp(8, 24).min(area.height);
    if width < 40 || height < 8 {
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
