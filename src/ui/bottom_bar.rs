use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::state::focus::Mode;
use crate::state::status::QueryStatus;
use crate::ui::theme::Theme;

pub const COMMAND_PREFIX: &str = ":";

pub struct BottomBar<'a> {
    app: &'a App,
}

impl<'a> BottomBar<'a> {
    pub fn new(app: &'a App) -> Self {
        Self { app }
    }
}

impl Widget for BottomBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        paint_background(area, buf, &self.app.theme);
        match &self.app.mode {
            Mode::Command(_) => render_command_prefix(area, buf, &self.app.theme),
            Mode::ConfirmRun { .. } => render_confirm(area, buf, &self.app.theme),
            Mode::Auth(_) | Mode::EditConnection(_) | Mode::ConnectionList(_) => {
                // Modal screens own their own help text — keep the status
                // bar empty so the user isn't reading two things at once.
            }
            Mode::Connecting { name } => {
                render_connecting(name, area, buf, &self.app.theme);
            }
            _ => render_status(&self.app.status, area, buf, &self.app.theme),
        }
    }
}

/// Just paints the leading `:` — the input value itself is rendered by the
/// `TextArea` that the parent layer drops on top of the rest of the bar.
fn render_command_prefix(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let line = Line::from(Span::styled(
        COMMAND_PREFIX,
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    ));
    line.render(area, buf);
}

fn render_connecting(name: &str, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled("⌛ ", Style::default().fg(theme.status_running).bg(theme.bg)),
        Span::styled(
            format!("connecting to {name}…"),
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    line.render(area, buf);
}

fn render_confirm(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled("▶ ", Style::default().fg(theme.status_running).bg(theme.bg)),
        Span::styled(
            "run highlighted statement?",
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  Enter to confirm · Esc to cancel",
            Style::default().fg(theme.fg_dim).bg(theme.bg),
        ),
    ]);
    line.render(area, buf);
}

fn paint_background(area: Rect, buf: &mut Buffer, theme: &Theme) {
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell_mut((x, area.y)) {
            cell.set_bg(theme.bg);
            cell.set_fg(theme.fg);
        }
    }
}

fn render_status(status: &QueryStatus, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let (icon_color, text) = describe(status, theme);
    let line = Line::from(vec![
        Span::styled("● ", Style::default().fg(icon_color).bg(theme.bg)),
        Span::styled(text, Style::default().fg(theme.fg).bg(theme.bg)),
    ]);
    line.render(area, buf);
}

fn describe(status: &QueryStatus, theme: &Theme) -> (Color, String) {
    match status {
        QueryStatus::Idle => (theme.status_idle, "idle".to_string()),
        QueryStatus::Running { query, .. } => (theme.status_running, format!("running: {query}")),
        QueryStatus::Succeeded {
            rows,
            affected,
            took,
        } => {
            let summary = match affected {
                Some(n) => format!("{n} affected"),
                None => format!("{rows} rows"),
            };
            (theme.status_ok, format!("ok — {summary} in {took:?}"))
        }
        QueryStatus::Failed { error } => (theme.status_error, format!("error — {error}")),
        QueryStatus::Cancelled => (theme.status_idle, "cancelled".to_string()),
    }
}
