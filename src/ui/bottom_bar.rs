use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::state::command::CommandBuffer;
use crate::state::focus::Mode;
use crate::state::status::QueryStatus;
use crate::ui::theme::Theme;

pub struct BottomBar<'a> {
    app: &'a App,
}

impl<'a> BottomBar<'a> {
    pub fn new(app: &'a App) -> Self {
        Self { app }
    }

    pub fn cursor_position(&self, area: Rect) -> Option<Position> {
        let buf = self.app.mode.command_buffer()?;
        let prefix_cols = 1; // ':'
        let col = area.x + prefix_cols + cursor_columns_before(buf) as u16;
        Some(Position::new(col, area.y))
    }
}

impl Widget for BottomBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        paint_background(area, buf, &self.app.theme);
        match &self.app.mode {
            Mode::Command(cmd) => render_command(cmd, area, buf, &self.app.theme),
            Mode::ConfirmRun { .. } => render_confirm(area, buf, &self.app.theme),
            _ => render_status(&self.app.status, area, buf, &self.app.theme),
        }
    }
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
        QueryStatus::Succeeded { rows, took } => {
            (theme.status_ok, format!("ok — {rows} rows in {took:?}"))
        }
        QueryStatus::Failed { error } => (theme.status_error, format!("error — {error}")),
        QueryStatus::Cancelled => (theme.status_idle, "cancelled".to_string()),
    }
}

fn render_command(cmd: &CommandBuffer, area: Rect, buf: &mut Buffer, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled(
            ":",
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            cmd.input.clone(),
            Style::default().fg(theme.fg).bg(theme.bg),
        ),
    ]);
    line.render(area, buf);
}

fn cursor_columns_before(buf: &CommandBuffer) -> usize {
    buf.input[..buf.cursor].chars().count()
}
