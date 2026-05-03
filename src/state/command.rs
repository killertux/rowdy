use ratatui::style::Style;
use ratatui_textarea::TextArea;

use crate::command::{COMMAND_TREE, CommandSpec};

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

/// Snapshot of the autocomplete popover state. Built fresh on every
/// input event by [`CommandBuffer::recompute_completion`].
#[derive(Debug, Clone)]
pub struct CommandCompletion {
    pub hits: Vec<&'static str>,
    pub selected: usize,
}

impl CommandCompletion {
    /// Build from the current buffer text. Returns `None` if the
    /// input is empty at the top level, the parent token chain is
    /// unknown (typo), or no candidate matches the active prefix.
    ///
    /// Walks [`COMMAND_TREE`] one token at a time:
    /// - already-typed parent tokens descend into `children`;
    /// - the trailing partial token (or empty after a space) is the
    ///   prefix to filter the current children list against;
    /// - aliases are matched the same as canonical names but only
    ///   the canonical name appears in the hit list — typing `q`
    ///   surfaces `quit`, never both.
    pub fn for_input(text: &str) -> Option<Self> {
        let trailing_space = text.ends_with(char::is_whitespace);
        let mut tokens: Vec<&str> = text.split_whitespace().collect();

        // The token currently being typed. After a trailing space
        // we're starting a fresh empty token; otherwise the last
        // whitespace-separated chunk is the partial.
        let prefix: String = if trailing_space {
            String::new()
        } else {
            tokens
                .pop()
                .map(str::to_ascii_lowercase)
                .unwrap_or_default()
        };

        // Walk the tree by consuming the already-typed parent
        // tokens. Unknown parents hide the popover rather than
        // silently suggesting from the wrong scope.
        let mut node_children: &[CommandSpec] = COMMAND_TREE;
        for tok in &tokens {
            let lower = tok.to_ascii_lowercase();
            let matched = node_children
                .iter()
                .find(|s| s.name == lower || s.aliases.iter().any(|a| *a == lower));
            match matched {
                Some(spec) => node_children = spec.children,
                None => return None,
            }
        }

        // Don't pop a popover for empty input at the top level —
        // matches the existing "you must type at least one char"
        // UX from before the rework.
        let at_top_level = tokens.is_empty();
        if at_top_level && prefix.is_empty() {
            return None;
        }
        if node_children.is_empty() {
            return None;
        }

        let hits: Vec<&'static str> = node_children
            .iter()
            .filter(|s| {
                s.name.starts_with(&prefix) || s.aliases.iter().any(|a| a.starts_with(&prefix))
            })
            .map(|s| s.name)
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

    /// Replace the partial token the user is currently typing with
    /// `cmd`, preserving everything before it. Handles both the
    /// "first token" case (`exp` → `export`) and the "sub-token"
    /// case (`chat se` → `chat settings`). After splicing, the
    /// popover recomputes — if the chosen token has children the
    /// popover stays open for a follow-up; if it's a leaf and the
    /// user types a space next, the popover hides.
    pub fn accept_completion(&mut self, cmd: &str) {
        let current = self.text().to_string();
        let trailing_space = current.ends_with(char::is_whitespace);

        // Everything up to (and including) the last whitespace
        // stays intact; the partial token after it (if any) is
        // replaced.
        let prefix: String = if trailing_space {
            current.clone()
        } else {
            match current.rfind(char::is_whitespace) {
                Some(idx) => current[..=idx].to_string(),
                None => String::new(),
            }
        };

        let next = format!("{prefix}{cmd}");
        self.input = TextArea::new(vec![next]);
        self.input.set_cursor_line_style(Style::default());
        // Move the cursor to end-of-line for a natural "now type
        // the next token" feel.
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
    fn top_level_prefix_match() {
        let c = CommandCompletion::for_input("exp").expect("hits");
        assert_eq!(c.hits, vec!["expand", "export"]);
    }

    #[test]
    fn top_level_alias_resolves_to_canonical() {
        // Typing `q` matches `quit` via its alias — and only the
        // canonical name appears in the popover (no duplicate row).
        let c = CommandCompletion::for_input("q").expect("hits");
        assert_eq!(c.hits, vec!["quit"]);
    }

    #[test]
    fn top_level_alias_e_matches_only_expand() {
        // Pre-rework behaviour included `e` (alias) as its own hit.
        // After the rework only canonical names show up; both
        // `expand` and `export` start with `e` so both surface, but
        // `e` itself never appears as a hit.
        let c = CommandCompletion::for_input("e").expect("hits");
        assert_eq!(c.hits, vec!["expand", "export"]);
    }

    #[test]
    fn top_level_includes_source() {
        // The flat REGISTRY missed `:source`. The tree includes it.
        let c = CommandCompletion::for_input("so").expect("hits");
        assert!(c.hits.contains(&"source"), "got: {:?}", c.hits);
    }

    #[test]
    fn top_level_includes_update() {
        let c = CommandCompletion::for_input("up").expect("hits");
        assert!(c.hits.contains(&"update"), "got: {:?}", c.hits);
    }

    #[test]
    fn subcommand_completion_after_space() {
        let c = CommandCompletion::for_input("chat ").expect("hits");
        assert_eq!(c.hits, vec!["clear", "settings"]);
    }

    #[test]
    fn subcommand_prefix_filters_children() {
        let c = CommandCompletion::for_input("chat se").expect("hits");
        assert_eq!(c.hits, vec!["settings"]);
        let c = CommandCompletion::for_input("conn l").expect("hits");
        assert_eq!(c.hits, vec!["list"]);
    }

    #[test]
    fn subcommand_alias_resolves_to_canonical() {
        // `rm` is an alias for `delete`. Typing `rm` after `conn `
        // surfaces the canonical `delete` (not `rm`).
        let c = CommandCompletion::for_input("conn rm").expect("hits");
        assert_eq!(c.hits, vec!["delete"]);
        // `ls` similarly maps to `list`.
        let c = CommandCompletion::for_input("conn ls").expect("hits");
        assert_eq!(c.hits, vec!["list"]);
    }

    #[test]
    fn export_subcommands_surface_after_space() {
        let c = CommandCompletion::for_input("export ").expect("hits");
        assert_eq!(c.hits, vec!["csv", "tsv", "json", "sql"]);
    }

    #[test]
    fn unknown_parent_hides_popover() {
        assert!(CommandCompletion::for_input("nonsense ").is_none());
        assert!(CommandCompletion::for_input("nope sub").is_none());
    }

    #[test]
    fn leaf_hides_popover_on_trailing_space() {
        // `chat clear` has no children — once the user types a
        // space after it the popover gets out of the way.
        assert!(CommandCompletion::for_input("chat clear ").is_none());
        assert!(CommandCompletion::for_input("export csv ").is_none());
    }

    #[test]
    fn empty_input_hides_popover() {
        assert!(CommandCompletion::for_input("").is_none());
        assert!(CommandCompletion::for_input(" ").is_none());
    }

    #[test]
    fn unknown_token_hides_popover() {
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

    #[test]
    fn accept_completion_splices_subtoken() {
        let mut buf = CommandBuffer {
            input: TextArea::new(vec!["chat se".into()]),
            ..Default::default()
        };
        buf.accept_completion("settings");
        assert_eq!(buf.text(), "chat settings");
    }

    #[test]
    fn accept_completion_after_trailing_space() {
        let mut buf = CommandBuffer {
            input: TextArea::new(vec!["chat ".into()]),
            ..Default::default()
        };
        buf.accept_completion("clear");
        assert_eq!(buf.text(), "chat clear");
    }
}
