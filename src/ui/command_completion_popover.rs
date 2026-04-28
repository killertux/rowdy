//! Floating popover anchored above the `:` command bar.
//!
//! Renders the live `CommandCompletion` hits (top-level command names
//! that prefix-match the in-progress first token). The selected entry
//! is reverse-highlighted; Tab in the event layer accepts it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::state::command::CommandCompletion;
use crate::ui::theme::Theme;

pub struct CommandCompletionPopover<'a> {
    pub completion: &'a CommandCompletion,
    pub theme: &'a Theme,
}

impl Widget for CommandCompletionPopover<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border).bg(self.theme.bg))
            .style(Style::default().bg(self.theme.bg))
            .title(" :commands ")
            .title_style(Style::default().fg(self.theme.fg_dim).bg(self.theme.bg));
        let inner = block.inner(area);
        block.render(area, buf);

        let lines: Vec<Line<'_>> = self
            .completion
            .hits
            .iter()
            .enumerate()
            .map(|(i, &cmd)| {
                let style = if i == self.completion.selected {
                    Style::default()
                        .fg(self.theme.selection_fg)
                        .bg(self.theme.selection_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(self.theme.fg).bg(self.theme.bg)
                };
                Line::from(Span::styled(format!(" {cmd} "), style))
            })
            .collect();
        Paragraph::new(lines).render(inner, buf);
    }
}
