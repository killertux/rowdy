use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::state::saved_query_picker::SavedQueryPickerState;
use crate::ui::theme::Theme;

pub struct SavedQueryPicker<'a> {
    pub state: &'a SavedQueryPickerState,
    pub connection: Option<&'a str>,
    pub theme: &'a Theme,
}

impl Widget for SavedQueryPicker<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(box_area) = inner_box(area, self.state.entries.len()) else {
            return;
        };
        let title = match self.connection {
            Some(c) => format!(" run saved query — {c} "),
            None => " run saved query ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(self.theme.border_focus)
                    .bg(self.theme.bg),
            )
            .title(title)
            .title_style(
                Style::default()
                    .fg(self.theme.fg)
                    .bg(self.theme.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(self.theme.bg));
        let inner = block.inner(box_area);
        block.render(box_area, buf);

        let entries_h = (self.state.entries.len() as u16).max(1);
        let chunks = Layout::vertical([
            Constraint::Length(entries_h),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(inner);

        if self.state.entries.is_empty() {
            Paragraph::new("(no saved queries)")
                .style(Style::default().fg(self.theme.fg_dim).bg(self.theme.bg))
                .render(chunks[0], buf);
        } else {
            let lines: Vec<Line> = self
                .state
                .entries
                .iter()
                .enumerate()
                .map(|(i, name)| entry_line(name, i == self.state.selected, self.theme))
                .collect();
            Paragraph::new(lines)
                .style(Style::default().fg(self.theme.fg).bg(self.theme.bg))
                .render(chunks[0], buf);
        }

        let footer = "j/k move · Enter pick · Esc close";
        Paragraph::new(footer)
            .style(Style::default().fg(self.theme.fg_dim).bg(self.theme.bg))
            .wrap(Wrap { trim: true })
            .render(chunks[2], buf);
    }
}

fn entry_line<'a>(name: &str, selected: bool, theme: &Theme) -> Line<'a> {
    let bg = if selected {
        theme.selection_bg
    } else {
        theme.bg
    };
    let fg = if selected {
        theme.selection_fg
    } else {
        theme.fg
    };
    Line::from(vec![
        Span::styled("  ".to_string(), Style::default().fg(fg).bg(bg)),
        Span::styled(name.to_string(), Style::default().fg(fg).bg(bg)),
    ])
}

pub fn inner_box(area: Rect, entry_count: usize) -> Option<Rect> {
    let width = area.width.min(72);
    let needed_inner = (entry_count.max(1) as u16).saturating_add(3);
    let needed = needed_inner.saturating_add(2);
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
