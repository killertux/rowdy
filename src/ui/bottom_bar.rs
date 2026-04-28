use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::app::App;
use crate::state::overlay::Overlay;
use crate::state::results::ResultViewMode;
use crate::state::screen::Screen;
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
        // Overlay wins — it's the layer the user is actively interacting
        // with. Only fall through to screen-specific bars when nothing
        // is preempting input.
        match &self.app.overlay {
            Some(Overlay::Command(_)) => {
                render_command_prefix(area, buf, &self.app.theme);
                return;
            }
            Some(Overlay::ConfirmRun { reason, .. }) => {
                render_confirm(reason, area, buf, &self.app.theme);
                return;
            }
            Some(Overlay::Connecting { name }) => {
                render_connecting(name, area, buf, &self.app.theme);
                return;
            }
            Some(Overlay::Help { .. }) => {
                // Help popover owns its own footer — leave the bar blank.
                return;
            }
            Some(Overlay::LlmSettings(_)) => {
                // Settings modal owns its own footer.
                return;
            }
            None => {}
        }
        match &self.app.screen {
            // Modal screens own their own help text — keep the status
            // bar empty so the user isn't reading two things at once.
            Screen::Auth(_) | Screen::EditConnection(_) | Screen::ConnectionList(_) => {}
            Screen::ResultExpanded {
                view: ResultViewMode::YankFormat { .. },
                ..
            } => {
                render_yank_format_prompt(area, buf, &self.app.theme);
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
        Span::styled(
            "⌛ ",
            Style::default().fg(theme.status_running).bg(theme.bg),
        ),
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

fn render_confirm(
    reason: &crate::state::overlay::ConfirmRunReason,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    use crate::state::overlay::ConfirmRunReason;
    // Destructive variant gets a louder leader and the AST-derived
    // reason ("DELETE without WHERE", "TRUNCATE"); the standard manual
    // confirm keeps its existing copy.
    let (leader, leader_color, headline) = match reason {
        ConfirmRunReason::Manual => ("▶ ", theme.status_running, "run highlighted statement?"),
        ConfirmRunReason::Destructive(why) => ("⚠ ", theme.status_error, *why),
    };
    let mut spans = vec![Span::styled(
        leader,
        Style::default().fg(leader_color).bg(theme.bg),
    )];
    spans.push(Span::styled(
        headline,
        Style::default()
            .fg(theme.fg)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    ));
    if matches!(reason, ConfirmRunReason::Destructive(_)) {
        spans.push(Span::styled(
            " — confirm to run",
            Style::default().fg(theme.fg).bg(theme.bg),
        ));
    }
    spans.push(Span::styled(
        "  Enter to confirm · Esc to cancel",
        Style::default().fg(theme.fg_dim).bg(theme.bg),
    ));
    Line::from(spans).render(area, buf);
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
        QueryStatus::Running { query, started_at } => (
            theme.status_running,
            format!(
                "running ({}): {query}",
                format_elapsed(started_at.elapsed())
            ),
        ),
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
        QueryStatus::Notice { msg } => (theme.status_ok, msg.clone()),
    }
}

/// Human-friendly elapsed time. Sub-second values render in milliseconds so
/// the user sees the counter move from the first frame; longer queries shift
/// to seconds (one decimal) and minutes-and-seconds.
fn format_elapsed(d: Duration) -> String {
    let total_ms = d.as_millis();
    if total_ms < 1000 {
        return format!("{total_ms}ms");
    }
    let total_secs = d.as_secs();
    if total_secs < 60 {
        return format!("{:.1}s", d.as_secs_f32());
    }
    let m = total_secs / 60;
    let s = total_secs % 60;
    format!("{m}m {s}s")
}

fn render_yank_format_prompt(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled("⎘ ", Style::default().fg(theme.status_ok).bg(theme.bg)),
        Span::styled(
            "yank as: ",
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "[c]sv  [t]sv  [j]son  [s]ql",
            Style::default().fg(theme.fg).bg(theme.bg),
        ),
        Span::styled(
            "  ·  Esc cancel",
            Style::default().fg(theme.fg_dim).bg(theme.bg),
        ),
    ]);
    line.render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_uses_ms_below_one_second() {
        assert_eq!(format_elapsed(Duration::from_millis(0)), "0ms");
        assert_eq!(format_elapsed(Duration::from_millis(7)), "7ms");
        assert_eq!(format_elapsed(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn format_elapsed_uses_decimal_seconds_under_one_minute() {
        assert_eq!(format_elapsed(Duration::from_millis(1000)), "1.0s");
        assert_eq!(format_elapsed(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_elapsed(Duration::from_secs(59)), "59.0s");
    }

    #[test]
    fn format_elapsed_uses_minutes_seconds_at_or_above_one_minute() {
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m 0s");
        assert_eq!(format_elapsed(Duration::from_secs(83)), "1m 23s");
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "60m 0s");
    }
}
