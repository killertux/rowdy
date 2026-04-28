//! `:chat settings` modal.
//!
//! Centered, four-row form: Backend (radio-cycled with ←/→ or `[`/`]`),
//! Model, Base URL, API key (masked). Patterned after
//! `crate::ui::conn_form_view`.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use ratatui_textarea::TextArea;

use crate::llm::LlmBackendKind;
use crate::state::llm_settings::{LlmSettingsField, LlmSettingsState};
use crate::ui::theme::Theme;

const BACKEND_LABEL: &str = "Backend:  ";
const MODEL_LABEL: &str = "Model:    ";
const BASE_URL_LABEL: &str = "Base URL: ";
const API_KEY_LABEL: &str = "API key:  ";

pub struct LlmSettingsForm<'a> {
    pub state: &'a LlmSettingsState,
    pub theme: &'a Theme,
}

impl Widget for LlmSettingsForm<'_> {
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
            .title(" rowdy — LLM settings ")
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
            Constraint::Length(1), // backend
            Constraint::Length(1), // model
            Constraint::Length(1), // base url
            Constraint::Length(1), // api key
            Constraint::Length(1), // blank
            Constraint::Length(2), // hint / error (wrapped)
            Constraint::Length(2), // help (wrapped)
        ])
        .split(inner);

        render_backend_row(buf, chunks[0], self.state, self.theme);
        render_text_field(
            buf,
            chunks[1],
            MODEL_LABEL,
            &self.state.model,
            self.state.focus == LlmSettingsField::Model,
            self.theme,
        );
        render_text_field(
            buf,
            chunks[2],
            BASE_URL_LABEL,
            &self.state.base_url,
            self.state.focus == LlmSettingsField::BaseUrl,
            self.theme,
        );
        render_text_field(
            buf,
            chunks[3],
            API_KEY_LABEL,
            &self.state.api_key,
            self.state.focus == LlmSettingsField::ApiKey,
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
                "Conversations are sent to your provider — never paste secrets in chat.",
                Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
            )),
        };
        Paragraph::new(hint_line)
            .wrap(Wrap { trim: true })
            .render(chunks[5], buf);

        Paragraph::new(Line::from(Span::styled(
            "Tab cycles · ←/→ or h/l change provider · Enter saves · Esc cancels",
            Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
        )))
        .wrap(Wrap { trim: true })
        .render(chunks[6], buf);
    }
}

fn render_backend_row(buf: &mut Buffer, area: Rect, state: &LlmSettingsState, theme: &Theme) {
    let label_focused = state.focus == LlmSettingsField::Backend;
    let label_style = Style::default()
        .fg(if label_focused {
            theme.header_fg
        } else {
            theme.fg
        })
        .bg(theme.bg)
        .add_modifier(Modifier::BOLD);
    Paragraph::new(Line::from(Span::styled(
        BACKEND_LABEL.to_string(),
        label_style,
    )))
    .render(area, buf);

    let label_cols = BACKEND_LABEL.chars().count() as u16;
    let inner = Rect {
        x: area.x + label_cols,
        y: area.y,
        width: area.width.saturating_sub(label_cols),
        height: 1,
    };
    let row = backend_row(state.backend, theme, label_focused);
    Paragraph::new(row).render(inner, buf);
}

fn backend_row(active: LlmBackendKind, theme: &Theme, focused: bool) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, kind) in LlmBackendKind::all().iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                "  ",
                Style::default().fg(theme.fg_dim).bg(theme.bg),
            ));
        }
        let label = kind.as_str();
        let is_active = *kind == active;
        let style = if is_active && focused {
            Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else if is_active {
            Style::default()
                .fg(theme.header_fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg_dim).bg(theme.bg)
        };
        spans.push(Span::styled(label.to_string(), style));
    }
    Line::from(spans)
}

fn render_text_field(
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
    let width = area.width.min(80);
    let height = 13.min(area.height);
    if width < 50 || height < 9 {
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
