//! Chat panel renderer.
//!
//! Two-pane vertical split: the message log on top, the composer at the
//! bottom. The composer auto-grows up to a small ceiling (it's a multi-line
//! `TextArea`). Tool-call boxes render as a single dim-tinted line in
//! phase 2; phase 4 expands them with args/result preview.
//!
//! Hit-testing data is stashed in `app.layout.chat` via [`layout_for`] so
//! the mouse handler can route clicks back to a panel without re-doing
//! the layout math.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::app::App;
use crate::state::chat::{ChatBlock, ChatMessage, ChatPanel, ChatRole};
use crate::state::focus::Focus;
use crate::state::layout::ChatLayout;
use crate::ui::theme::Theme;

const COMPOSER_MIN_HEIGHT: u16 = 3;
const COMPOSER_MAX_HEIGHT: u16 = 8;

pub struct ChatPane<'a> {
    pub app: &'a App,
}

impl Widget for ChatPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = &self.app.theme;
        let focus = self.app.focus;
        let panel_focused = focus.is_chat();
        let composer_focused = focus == Focus::ChatComposer;
        let chat = &self.app.chat;

        let block = themed_block(theme, panel_focused, status_hint(chat, focus));
        let inner = block.inner(area);
        block.render(area, buf);

        let composer_h = composer_height(chat, inner.height);
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(composer_h)]).split(inner);
        let log_area = chunks[0];
        let composer_area = chunks[1];

        render_log(chat, theme, log_area, buf);
        render_composer(chat, theme, composer_focused, composer_area, buf);
    }
}

/// Pre-render hit-test data: where the log paints vs. where the composer
/// paints. The mouse handler uses this to decide whether a click should
/// focus the composer or scroll the log.
pub fn layout_for(panel: &ChatPanel, area: Rect) -> ChatLayout {
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let composer_h = composer_height(panel, inner.height);
    let log_h = inner.height.saturating_sub(composer_h);
    let log_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: log_h,
    };
    let composer_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(log_h),
        width: inner.width,
        height: composer_h,
    };
    ChatLayout {
        area,
        log_area,
        composer_area,
    }
}

fn composer_height(panel: &ChatPanel, available: u16) -> u16 {
    if available <= COMPOSER_MIN_HEIGHT {
        return available;
    }
    // +2 for the wrapping border around the composer (drawn inside `inner`).
    let lines = panel.composer.lines().len() as u16;
    let want = lines.saturating_add(2);
    want.clamp(COMPOSER_MIN_HEIGHT, COMPOSER_MAX_HEIGHT)
        .min(available.saturating_sub(1))
}

fn status_hint(chat: &ChatPanel, focus: Focus) -> Option<String> {
    if chat.streaming {
        return Some(" streaming… ".into());
    }
    if let Some(err) = &chat.error {
        return Some(format!(" error: {err} "));
    }
    // Mode hint — only meaningful while the chat panel itself is focused.
    // We surface the current modal state so users can tell at a glance
    // whether keystrokes will scroll (normal) or land in the composer
    // (insert).
    let mode = match focus {
        Focus::Chat => Some(" normal · i to type "),
        Focus::ChatComposer => Some(" insert · esc to scroll "),
        _ => None,
    };
    if let Some(m) = mode {
        return Some(m.to_string());
    }
    if chat.messages.is_empty() {
        return None;
    }
    Some(format!(" {} msgs ", chat.messages.len()))
}

fn render_log(chat: &ChatPanel, theme: &Theme, area: Rect, buf: &mut Buffer) {
    if chat.messages.is_empty() {
        let placeholder = Paragraph::new(Line::from(Span::styled(
            "No messages yet. Type below and press Enter.",
            Style::default().fg(theme.fg_dim).bg(theme.bg),
        )))
        .wrap(Wrap { trim: true });
        placeholder.render(area, buf);
        return;
    }

    let lines = build_log_lines(chat, theme);
    let scroll = chat.scroll;
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .style(Style::default().fg(theme.fg).bg(theme.bg))
        .render(area, buf);
}

fn build_log_lines<'a>(chat: &'a ChatPanel, theme: &'a Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    for (idx, msg) in chat.messages.iter().enumerate() {
        if idx > 0 {
            lines.push(Line::from(""));
        }
        lines.push(role_header_line(msg, theme));
        for block in &msg.blocks {
            push_block_lines(block, msg, theme, &mut lines);
        }
    }
    lines
}

fn role_header_line<'a>(msg: &ChatMessage, theme: &'a Theme) -> Line<'a> {
    let (label, color) = match msg.role {
        ChatRole::User => ("you", theme.header_fg),
        ChatRole::Assistant => ("rowdy", theme.status_running),
        ChatRole::System => ("system", theme.fg_dim),
    };
    Line::from(Span::styled(
        format!("▌ {label}"),
        Style::default()
            .fg(color)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    ))
}

fn push_block_lines<'a>(
    block: &'a ChatBlock,
    msg: &ChatMessage,
    theme: &'a Theme,
    out: &mut Vec<Line<'a>>,
) {
    match block {
        ChatBlock::Text(s) => {
            for line in s.split('\n') {
                out.push(Line::from(Span::styled(
                    line.to_string(),
                    text_style(msg.role, theme),
                )));
            }
        }
        ChatBlock::ToolCall {
            name, args_json, ..
        } => {
            out.push(Line::from(Span::styled(
                format!("◆ tool: {name}({args_json})"),
                Style::default().fg(theme.fg_dim).bg(theme.bg),
            )));
        }
        ChatBlock::ToolResult { name, error, .. } => {
            let (prefix, color) = match error {
                Some(_) => ("✗ result", theme.status_error),
                None => ("✓ result", theme.fg_dim),
            };
            out.push(Line::from(Span::styled(
                format!("{prefix}: {name}"),
                Style::default().fg(color).bg(theme.bg),
            )));
        }
    }
}

fn text_style(role: ChatRole, theme: &Theme) -> Style {
    let fg = match role {
        ChatRole::User => theme.fg,
        ChatRole::Assistant => theme.fg,
        ChatRole::System => theme.fg_dim,
    };
    Style::default().fg(fg).bg(theme.bg)
}

fn render_composer(chat: &ChatPanel, theme: &Theme, focused: bool, area: Rect, buf: &mut Buffer) {
    let border = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border).bg(theme.bg))
        .title(" message ")
        .title_style(Style::default().fg(theme.fg_dim).bg(theme.bg))
        .style(Style::default().bg(theme.bg));
    let inner = block.inner(area);
    block.render(area, buf);
    chat.composer.render(inner, buf);
}

fn themed_block<'a>(theme: &Theme, focused: bool, hint: Option<String>) -> Block<'a> {
    let border = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    let title = match hint {
        Some(extra) => format!(" chat{extra}"),
        None => " chat ".to_string(),
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border).bg(theme.bg))
        .title(title)
        .title_style(
            Style::default()
                .fg(theme.fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(theme.bg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn layout_for_splits_inner_into_log_and_composer() {
        let panel = ChatPanel::new();
        let layout = layout_for(&panel, rect(0, 0, 30, 20));
        // 1-cell border on each side → inner = 28 × 18.
        assert_eq!(layout.log_area.x, 1);
        assert_eq!(layout.composer_area.x, 1);
        // Log + composer cover the full inner height.
        assert_eq!(layout.log_area.height + layout.composer_area.height, 18);
        // Composer is at the bottom of the inner box.
        assert_eq!(layout.composer_area.y, 1 + layout.log_area.height);
    }

    #[test]
    fn layout_for_clamps_when_panel_is_short() {
        // 5 high → inner is 3 → composer_height should not exceed available
        // and the renderer should still produce non-overlapping rects.
        let panel = ChatPanel::new();
        let layout = layout_for(&panel, rect(0, 0, 30, 5));
        assert!(layout.log_area.height + layout.composer_area.height <= 3);
    }
}
