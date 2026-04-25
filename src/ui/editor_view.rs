use edtui::{EditorTheme, EditorView, LineNumbers, SyntaxHighlighter};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Widget};

use crate::app::App;
use crate::state::focus::Focus;
use crate::ui::theme::Theme;

pub struct EditorPane<'a> {
    pub app: &'a mut App,
}

impl Widget for EditorPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = self.app.theme;
        let focused = self.app.focus == Focus::Editor;
        let block = themed_block(&theme, focused);

        let editor_theme = EditorTheme::default()
            .block(block)
            .base(Style::default().bg(theme.bg).fg(theme.fg))
            .cursor_style(Style::default().bg(theme.cursor_bg).fg(theme.cursor_fg))
            .selection_style(
                Style::default()
                    .bg(theme.selection_bg)
                    .fg(theme.selection_fg),
            );

        let highlighter = SyntaxHighlighter::new(theme.kind.syntect_theme_name(), "sql").ok();

        EditorView::new(&mut self.app.editor.state)
            .theme(editor_theme)
            .syntax_highlighter(highlighter)
            .line_numbers(LineNumbers::Absolute)
            .wrap(false)
            .render(area, buf);
    }
}

fn themed_block<'a>(theme: &Theme, focused: bool) -> Block<'a> {
    let border = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border).bg(theme.bg))
        .title(" editor ")
        .title_style(Style::default().fg(theme.fg).bg(theme.bg))
        .style(Style::default().bg(theme.bg))
}
