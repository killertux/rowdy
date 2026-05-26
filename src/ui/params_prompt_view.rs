//! "Fill in query parameters" popup. Renders one `label: textarea`
//! row per unique placeholder in the about-to-run statement.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use ratatui_textarea::TextArea;

use crate::state::params_prompt::ParamsPromptState;
use crate::ui::theme::Theme;

const TITLE: &str = " rowdy — query parameters ";
const HINT: &str = "strings need quotes — 'foo' · NULL · any SQL expression";
const FOOTER: &str = "Tab to switch · Enter to run · Esc to cancel";

/// Width budget for the form's label column. Wider than the connection
/// form's `Name: ` because labels here may be `:long_identifier`.
const LABEL_MIN_COLS: u16 = 12;
const LABEL_MAX_COLS: u16 = 24;

pub struct ParamsPrompt<'a> {
    pub state: &'a ParamsPromptState,
    pub theme: &'a Theme,
}

impl Widget for ParamsPrompt<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(box_area) = inner_box(area, self.state.fields.len()) else {
            return;
        };

        // Wipe the cells under the popover first. Block::style only sets
        // each cell's style — it doesn't overwrite the glyph — so without
        // an explicit Clear the editor's characters show through the
        // popover background.
        Clear.render(box_area, buf);
        // Repaint with the theme bg so the cleared cells aren't terminal
        // default (which can be transparent in some emulators).
        for y in box_area.y..box_area.y + box_area.height {
            for x in box_area.x..box_area.x + box_area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(self.theme.bg);
                    cell.set_fg(self.theme.fg);
                }
            }
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(self.theme.border_focus)
                    .bg(self.theme.bg),
            )
            .title(TITLE)
            .title_style(
                Style::default()
                    .fg(self.theme.fg)
                    .bg(self.theme.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(self.theme.bg));
        let inner = block.inner(box_area);
        block.render(box_area, buf);

        let field_count = self.state.fields.len().max(1) as u16;
        let constraints: Vec<Constraint> =
            std::iter::repeat_n(Constraint::Length(1), field_count as usize)
                .chain([
                    Constraint::Length(1), // blank
                    Constraint::Length(2), // hint
                    Constraint::Length(2), // footer
                ])
                .collect();
        let rows = Layout::vertical(constraints).split(inner);

        let label_cols = compute_label_cols(self.state);
        for (i, field) in self.state.fields.iter().enumerate() {
            let row = rows[i];
            let focused = i == self.state.focus;
            render_field(
                buf,
                row,
                &field.label,
                label_cols,
                &field.input,
                focused,
                self.theme,
            );
        }

        let hint_idx = self.state.fields.len() + 1;
        if let Some(hint_row) = rows.get(hint_idx) {
            Paragraph::new(Line::from(Span::styled(
                HINT,
                Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
            )))
            .wrap(Wrap { trim: true })
            .render(*hint_row, buf);
        }
        if let Some(footer_row) = rows.get(hint_idx + 1) {
            Paragraph::new(Line::from(Span::styled(
                FOOTER,
                Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
            )))
            .wrap(Wrap { trim: true })
            .render(*footer_row, buf);
        }
    }
}

fn render_field(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    label_cols: u16,
    input: &TextArea<'_>,
    focused: bool,
    theme: &Theme,
) {
    let label_style = Style::default()
        .fg(if focused { theme.header_fg } else { theme.fg })
        .bg(theme.bg)
        .add_modifier(Modifier::BOLD);
    let padded = format!(
        "{label:<width$}",
        label = label,
        width = label_cols as usize
    );
    Paragraph::new(Line::from(Span::styled(padded, label_style))).render(area, buf);

    let label_actual = label_cols.saturating_add(1);
    let input_area = Rect {
        x: area.x + label_actual,
        y: area.y,
        width: area.width.saturating_sub(label_actual),
        height: 1,
    };
    input.render(input_area, buf);
}

fn compute_label_cols(state: &ParamsPromptState) -> u16 {
    let widest = state
        .fields
        .iter()
        .map(|f| f.label.chars().count() as u16)
        .max()
        .unwrap_or(LABEL_MIN_COLS);
    widest.clamp(LABEL_MIN_COLS, LABEL_MAX_COLS)
}

pub fn inner_box(area: Rect, field_count: usize) -> Option<Rect> {
    let width = area.width.min(70);
    // 2 (border) + N field rows + blank + 2 hint + 2 footer.
    let needed = 2 + field_count as u16 + 1 + 2 + 2;
    let height = needed.min(area.height);
    if width < 30 || height < 8 {
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
