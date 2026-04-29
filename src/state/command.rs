use ratatui::style::Style;
use ratatui_textarea::TextArea;

/// Wrapper around a `TextArea` so the rest of the app keeps pattern-matching
/// on `Overlay::Command(CommandBuffer)` while the editor primitives live in
/// `ratatui_textarea`.
#[derive(Debug)]
pub struct CommandBuffer {
    pub input: TextArea<'static>,
    /// Live autocomplete state — recomputed every time the input changes.
    /// `None` when the buffer is empty or no candidates match.
    pub completion: Option<CommandCompletion>,
}

/// Top-level command names + aliases offered to the autocomplete
/// popover. Order matches the `:help` ordering so the user sees a
/// stable, predictable list.
const REGISTRY: &[&str] = &[
    "quit", "q", "help", "?", "width", "run", "r", "cancel", "expand", "e", "collapse", "c",
    "close", "hide", "theme", "export", "format", "fmt", "reload", "conn", "conns", "chat",
    "update",
];

/// Snapshot of the autocomplete popover state. Built fresh on every
/// input event by [`CommandBuffer::recompute_completion`].
#[derive(Debug, Clone)]
pub struct CommandCompletion {
    pub hits: Vec<&'static str>,
    pub selected: usize,
}

impl CommandCompletion {
    /// Build from the current buffer text. Returns `None` if the
    /// buffer has no input or nothing in the registry matches the
    /// first whitespace-delimited token.
    pub fn for_input(text: &str) -> Option<Self> {
        let token = text
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if token.is_empty() {
            return None;
        }
        // Only show the popover *while* the user is still typing the
        // first token — once they've added a space we assume they've
        // committed to a command name and want to type its arguments
        // freely (e.g. `:export csv ...`).
        let first_word_done = text.contains(char::is_whitespace);
        if first_word_done {
            return None;
        }
        let hits: Vec<&'static str> = REGISTRY
            .iter()
            .copied()
            .filter(|cmd| cmd.starts_with(&token))
            .collect();
        if hits.is_empty() {
            return None;
        }
        Some(Self { hits, selected: 0 })
    }

    pub fn move_selection(&mut self, delta: i32) {
        if self.hits.is_empty() {
            return;
        }
        let len = self.hits.len() as i32;
        let next = (self.selected as i32 + delta).rem_euclid(len);
        self.selected = next as usize;
    }

    pub fn current(&self) -> Option<&'static str> {
        self.hits.get(self.selected).copied()
    }
}

impl Default for CommandBuffer {
    fn default() -> Self {
        let mut input = TextArea::default();
        // Single-line command line — drop the cursor-line highlight so it
        // doesn't paint a band across the whole bar.
        input.set_cursor_line_style(Style::default());
        Self {
            input,
            completion: None,
        }
    }
}

impl CommandBuffer {
    pub fn text(&self) -> &str {
        self.input.lines().first().map(String::as_str).unwrap_or("")
    }

    /// Recompute the popover from the current buffer text. Call after
    /// every keystroke that mutates `input` (insertion, deletion,
    /// paste, clear).
    pub fn recompute_completion(&mut self) {
        self.completion = CommandCompletion::for_input(self.text());
    }

    /// Replace the in-progress first token with `cmd`. Preserves any
    /// arguments the user has already typed after a space (which would
    /// have closed the popover anyway, but the round-trip stays clean).
    pub fn accept_completion(&mut self, cmd: &str) {
        let current = self.text().to_string();
        let rest = current
            .split_once(char::is_whitespace)
            .map(|(_, r)| r)
            .unwrap_or("");
        let next = if rest.is_empty() {
            cmd.to_string()
        } else {
            format!("{cmd} {rest}")
        };
        self.input = TextArea::new(vec![next]);
        self.input.set_cursor_line_style(Style::default());
        // Move the cursor to end-of-line for a natural "now type the
        // arguments" feel.
        let len = self.text().chars().count();
        self.input
            .move_cursor(ratatui_textarea::CursorMove::Jump(0, len as u16));
        self.recompute_completion();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_prefix_matches_top_level_commands() {
        let c = CommandCompletion::for_input("exp").expect("hits");
        assert!(c.hits.contains(&"expand"));
        assert!(c.hits.contains(&"export"));
        // `:e` is also an alias for expand — included.
        let c = CommandCompletion::for_input("e").expect("hits");
        assert!(c.hits.contains(&"e"));
        assert!(c.hits.contains(&"expand"));
        assert!(c.hits.contains(&"export"));
    }

    #[test]
    fn completion_includes_update() {
        let c = CommandCompletion::for_input("up").expect("hits");
        assert!(
            c.hits.contains(&"update"),
            ":update must be discoverable via the autocomplete popover; got {:?}",
            c.hits
        );
    }

    #[test]
    fn completion_closes_after_first_space() {
        // Once the user types a space after the command name we
        // assume they're typing args — the popover gets out of the way.
        assert!(CommandCompletion::for_input("export ").is_none());
        assert!(CommandCompletion::for_input("export csv").is_none());
    }

    #[test]
    fn completion_returns_none_for_empty_or_unknown() {
        assert!(CommandCompletion::for_input("").is_none());
        assert!(CommandCompletion::for_input("zzzz").is_none());
    }

    #[test]
    fn move_selection_wraps_around() {
        let mut c = CommandCompletion::for_input("e").expect("hits");
        let len = c.hits.len();
        c.move_selection(-1);
        assert_eq!(c.selected, len - 1);
        c.move_selection(1);
        assert_eq!(c.selected, 0);
    }

    #[test]
    fn accept_completion_replaces_first_token() {
        let mut buf = CommandBuffer {
            input: TextArea::new(vec!["exp".into()]),
            ..Default::default()
        };
        buf.accept_completion("export");
        assert_eq!(buf.text(), "export");
    }
}
