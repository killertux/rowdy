use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::state::auth::{AuthKind, AuthState};
use crate::ui::theme::Theme;

pub struct AuthPrompt<'a> {
    pub state: &'a AuthState,
    pub theme: &'a Theme,
}

impl Widget for AuthPrompt<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(box_area) = inner_box(area) else {
            return;
        };

        let title = match self.state.kind {
            AuthKind::FirstSetup => " rowdy — set password ",
            AuthKind::Unlock { .. } => " rowdy — unlock store ",
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
            Constraint::Length(3), // explanation (wrapped — 1–3 lines)
            Constraint::Length(1), // blank
            Constraint::Length(1), // prompt
            Constraint::Length(1), // blank
            Constraint::Length(2), // error (wrapped — 1–2 lines)
            Constraint::Length(2), // help (wrapped — 1–2 lines)
        ])
        .split(inner);

        // [0] explanation
        Paragraph::new(explanation_text(&self.state.kind))
            .style(Style::default().fg(self.theme.fg).bg(self.theme.bg))
            .wrap(Wrap { trim: true })
            .render(chunks[0], buf);

        // [2] prompt — "Password: " label + masked TextArea for the value
        let prompt_row = chunks[2];
        let label = Line::from(Span::styled(
            PROMPT_LABEL,
            Style::default()
                .fg(self.theme.fg)
                .bg(self.theme.bg)
                .add_modifier(Modifier::BOLD),
        ));
        Paragraph::new(label).render(prompt_row, buf);

        let label_cols = PROMPT_LABEL.chars().count() as u16;
        let input_area = Rect {
            x: prompt_row.x + label_cols,
            y: prompt_row.y,
            width: prompt_row.width.saturating_sub(label_cols),
            height: 1,
        };
        // `&TextArea` implements `Widget` — render into the input slot.
        (&self.state.input).render(input_area, buf);

        // [4] error
        if let Some(err) = &self.state.error {
            Paragraph::new(err.clone())
                .style(
                    Style::default()
                        .fg(self.theme.status_error)
                        .bg(self.theme.bg),
                )
                .wrap(Wrap { trim: true })
                .render(chunks[4], buf);
        }

        // [5] help
        Paragraph::new(help_text(&self.state.kind))
            .style(Style::default().fg(self.theme.fg_dim).bg(self.theme.bg))
            .wrap(Wrap { trim: true })
            .render(chunks[5], buf);
    }
}

const PROMPT_LABEL: &str = "Password: ";

fn explanation_text(kind: &AuthKind) -> String {
    match kind {
        AuthKind::FirstSetup => "Your password encrypts the connection strings rowdy saves to \
                                 .rowdy/config.toml. Leave it empty to store them as plaintext."
            .to_string(),
        AuthKind::Unlock { .. } => "Enter the password you set when you first stored an encrypted \
                                    connection. It decrypts your saved URLs in memory only."
            .to_string(),
    }
}

fn help_text(kind: &AuthKind) -> String {
    match kind {
        AuthKind::FirstSetup => "Enter to confirm · empty = no encryption · Esc to quit".into(),
        AuthKind::Unlock { .. } => "Enter to unlock · Esc to quit".into(),
    }
}

fn inner_box(area: Rect) -> Option<Rect> {
    let width = area.width.min(64);
    let height = 14.min(area.height);
    if width < 36 || height < 12 {
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
