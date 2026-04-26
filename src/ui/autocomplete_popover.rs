//! Floating popover rendered on top of the editor when a completion is
//! active. Anchors to the cursor's screen position (provided by edtui's
//! `cursor_screen_position()`), flips above when the bottom would clip,
//! and clamps right-edge so the box always fits inside `editor_area`.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Widget};

use crate::state::completion::{CompletionState, MAX_VISIBLE_ROWS};
use crate::ui::theme::Theme;

const MIN_WIDTH: u16 = 16;
const MAX_WIDTH: u16 = 50;

pub struct CompletionPopover<'a> {
    pub state: &'a CompletionState,
    pub theme: &'a Theme,
    pub editor_area: Rect,
    pub cursor_screen_pos: Position,
}

impl Widget for CompletionPopover<'_> {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        if self.state.items.is_empty() {
            return;
        }
        let visible = self.state.items.len().min(MAX_VISIBLE_ROWS);
        let max_label = self
            .state
            .items
            .iter()
            .map(|i| i.label.chars().count())
            .max()
            .unwrap_or(0);
        let max_kind = self
            .state
            .items
            .iter()
            .map(|i| i.kind.label().chars().count())
            .max()
            .unwrap_or(0);

        // Width = " <icon> <label>  <kind> " + 2 borders
        let inner_w = (3 + max_label + 2 + max_kind + 1) as u16;
        let width = inner_w
            .saturating_add(2)
            .clamp(MIN_WIDTH, MAX_WIDTH)
            .min(self.editor_area.width);
        let height = (visible + 2) as u16;
        if height > self.editor_area.height {
            return;
        }

        let cx = self.cursor_screen_pos.x;
        let cy = self.cursor_screen_pos.y;
        let area_bottom = self.editor_area.y + self.editor_area.height;
        let area_right = self.editor_area.x + self.editor_area.width;

        // Prefer below the cursor; flip up if it would clip past the
        // editor area's bottom.
        let y = if cy.saturating_add(1).saturating_add(height) <= area_bottom {
            cy.saturating_add(1)
        } else if cy >= self.editor_area.y + height {
            cy.saturating_sub(height)
        } else {
            return;
        };

        // Anchor at cursor x, clamped to fit inside editor_area on the right.
        let x = cx.min(area_right.saturating_sub(width));
        let x = x.max(self.editor_area.x);
        let popover_area = Rect {
            x,
            y,
            width,
            height,
        };

        // When the candidate list overflows the visible window, surface
        // it in the title so the user knows there's more above/below.
        let total = self.state.items.len();
        let scroll_hint = if total > visible {
            let shown_from = self.state.scroll_offset + 1;
            let shown_to = (self.state.scroll_offset + visible).min(total);
            format!(" [{shown_from}-{shown_to}/{total}]")
        } else {
            String::new()
        };
        let title = if self.state.partial.is_empty() {
            format!(" completions{scroll_hint} ")
        } else {
            format!(" completions: {}{} ", self.state.partial, scroll_hint)
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
        let inner = block.inner(popover_area);
        block.render(popover_area, buf);

        for (row, item) in self
            .state
            .items
            .iter()
            .skip(self.state.scroll_offset)
            .take(visible)
            .enumerate()
        {
            let y = inner.y + row as u16;
            let abs_idx = self.state.scroll_offset + row;
            let selected = abs_idx == self.state.selected;
            let style = if selected {
                Style::default()
                    .bg(self.theme.selection_bg)
                    .fg(self.theme.selection_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(self.theme.bg).fg(self.theme.fg)
            };
            // Pre-fill the row with the row style so the selection bar
            // extends to the full popover width even when the item text
            // is shorter than `inner.width`.
            for col in 0..inner.width {
                if let Some(cell) = buf.cell_mut((inner.x + col, y)) {
                    cell.set_style(style);
                }
            }
            let label_pad = max_label.saturating_sub(item.label.chars().count());
            let line = format!(
                " {} {}{}  {} ",
                item.kind.icon(),
                item.label,
                " ".repeat(label_pad),
                item.kind.label()
            );
            let truncated: String = line.chars().take(inner.width as usize).collect();
            buf.set_string(inner.x, y, &truncated, style);
        }
    }
}
