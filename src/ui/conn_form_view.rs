use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use ratatui_textarea::TextArea;

use crate::state::conn_form::{ConnFormField, ConnFormState};
use crate::ui::theme::Theme;

const NAME_LABEL: &str = "Name: ";
const URL_LABEL: &str = "URL:  ";

pub struct ConnForm<'a> {
    pub state: &'a ConnFormState,
    pub theme: &'a Theme,
}

impl Widget for ConnForm<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(box_area) = inner_box(area) else {
            return;
        };

        let title = match &self.state.original {
            Some(name) => format!(" rowdy — edit connection {name} "),
            None => " rowdy — create connection ".to_string(),
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

        let chunks = Layout::vertical([
            Constraint::Length(1), // name row
            Constraint::Length(1), // url row
            Constraint::Length(1), // blank
            Constraint::Length(2), // hint / error (wrapped)
            Constraint::Length(2), // help (wrapped)
        ])
        .split(inner);

        let name_focused = self.state.focus == ConnFormField::Name;
        let url_focused = self.state.focus == ConnFormField::Url;

        render_field(
            buf,
            chunks[0],
            NAME_LABEL,
            &self.state.name,
            name_focused,
            self.theme,
        );
        render_field(
            buf,
            chunks[1],
            URL_LABEL,
            &self.state.url,
            url_focused,
            self.theme,
        );

        let hint_line = match &self.state.error {
            Some(err) => Line::from(Span::styled(
                err.clone(),
                Style::default()
                    .fg(self.theme.status_error)
                    .bg(self.theme.bg),
            )),
            None => Line::from(Span::styled(
                "schemes: sqlite:, postgres:, mysql:, mariadb:",
                Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
            )),
        };
        Paragraph::new(hint_line)
            .wrap(Wrap { trim: true })
            .render(chunks[3], buf);

        Paragraph::new(Line::from(Span::styled(
            "Tab to switch · Enter to save · Esc to quit",
            Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
        )))
        .wrap(Wrap { trim: true })
        .render(chunks[4], buf);
    }
}

fn render_field(
    buf: &mut Buffer,
    area: Rect,
    label: &str,
    input: &TextArea<'_>,
    focused: bool,
    theme: &Theme,
) {
    let label_style = Style::default()
        .fg(if focused { theme.header_fg } else { theme.fg })
        .bg(theme.bg)
        .add_modifier(Modifier::BOLD);
    Paragraph::new(Line::from(Span::styled(label.to_string(), label_style))).render(area, buf);

    let label_cols = label.chars().count() as u16;
    let input_area = Rect {
        x: area.x + label_cols,
        y: area.y,
        width: area.width.saturating_sub(label_cols),
        height: 1,
    };
    input.render(input_area, buf);
}

pub fn inner_box(area: Rect) -> Option<Rect> {
    let width = area.width.min(70);
    let height = 10.min(area.height);
    if width < 30 || height < 7 {
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
