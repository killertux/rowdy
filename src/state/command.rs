use ratatui::style::Style;
use ratatui_textarea::TextArea;

/// Wrapper around a `TextArea` so the rest of the app keeps pattern-matching
/// on `Mode::Command(CommandBuffer)` while the editor primitives live in
/// `ratatui_textarea`.
#[derive(Debug)]
pub struct CommandBuffer {
    pub input: TextArea<'static>,
}

impl Default for CommandBuffer {
    fn default() -> Self {
        let mut input = TextArea::default();
        // Single-line command line — drop the cursor-line highlight so it
        // doesn't paint a band across the whole bar.
        input.set_cursor_line_style(Style::default());
        Self { input }
    }
}

impl CommandBuffer {
    pub fn text(&self) -> &str {
        self.input
            .lines()
            .first()
            .map(String::as_str)
            .unwrap_or("")
    }
}
