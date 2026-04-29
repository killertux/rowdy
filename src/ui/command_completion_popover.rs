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
        // Paint an opaque background across the whole popover rect
        // first. `Block::style().bg(...)` ought to do this on its own
        // but in practice ratatui leaves cells with no glyph alone, so
        // unselected line tails and the row beyond the last hit would
        // bleed the editor through. One explicit fill keeps it crisp.
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(self.theme.bg);
                    cell.set_fg(self.theme.fg);
                    cell.set_symbol(" ");
                }
            }
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.border).bg(self.theme.bg))
            .style(Style::default().bg(self.theme.bg))
            .title(" :commands ")
            .title_style(Style::default().fg(self.theme.fg_dim).bg(self.theme.bg));
        let inner = block.inner(area);
        block.render(area, buf);

        // Pad each hit to the inner width so the selection highlight
        // covers the whole row instead of just the text glyphs.
        let pad_to = inner.width as usize;
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
                let mut text = format!(" {cmd}");
                let cur = text.chars().count();
                if cur < pad_to {
                    text.extend(std::iter::repeat_n(' ', pad_to - cur));
                }
                Line::from(Span::styled(text, style))
            })
            .collect();
        Paragraph::new(lines)
            .style(Style::default().bg(self.theme.bg))
            .render(inner, buf);
    }
}
