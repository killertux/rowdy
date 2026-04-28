//! Chat panel state — pure data, no I/O.
//!
//! Holds the message log, the composer text area, scroll offset, and a
//! transient streaming flag. The action layer is the only thing that
//! mutates this; rendering reads it. Filling messages from the LLM worker
//! and persisting to disk both live in later phases — this module just
//! defines the shape they manipulate.

use chrono::{DateTime, Utc};
use ratatui::style::Style;
use ratatui_textarea::TextArea;
use serde::{Deserialize, Serialize};

/// Author of a chat message. The `Tool` role doesn't render as a balloon —
/// it appears as a `ToolResult` content block under the assistant's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
    System,
}

/// One block within a message. Splitting messages into blocks lets us
/// interleave free-form text with tool-call boxes (e.g. "I called
/// `describe_table` → got these columns → here's the SQL").
///
/// `ToolCall` / `ToolResult` get constructed in phase 4 once the LLM
/// worker can request tools; the renderer in `ui/chat_view.rs` already
/// knows how to draw them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[allow(dead_code)]
pub enum ChatBlock {
    Text(String),
    /// A function the LLM asked us to run. Args are the JSON it produced;
    /// the resulting `ToolResult` block follows immediately after.
    ToolCall {
        id: String,
        name: String,
        args_json: String,
    },
    /// Output of a tool call. `error` populated iff the call failed.
    ToolResult {
        call_id: String,
        name: String,
        output: String,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub blocks: Vec<ChatBlock>,
    /// Wall-clock time the message was created. Persisted to disk so
    /// session loads come back in chronological order.
    pub timestamp: DateTime<Utc>,
}

impl ChatMessage {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            blocks: vec![ChatBlock::Text(text.into())],
            timestamp: Utc::now(),
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            blocks: vec![ChatBlock::Text(text.into())],
            timestamp: Utc::now(),
        }
    }

    #[allow(dead_code)] // used by phase 3's system-prompt seeding.
    pub fn system_text(text: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            blocks: vec![ChatBlock::Text(text.into())],
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug)]
pub struct ChatPanel {
    pub messages: Vec<ChatMessage>,
    pub composer: TextArea<'static>,
    /// Topmost line index drawn in the message log. Clamped at render
    /// time against actual content height; stored here so cursor-visibility
    /// scrolling and explicit PgUp/PgDn both write to the same number.
    pub scroll: u16,
    /// True while a worker chat task is in flight. The renderer flips the
    /// composer into a "streaming…" disabled style and the bottom bar
    /// surfaces a live indicator.
    pub streaming: bool,
    /// Last-turn error, if any. Cleared when the user sends a fresh message.
    pub error: Option<String>,
    /// When true, the message log auto-scrolls to the bottom as new content
    /// arrives. Disengaged the moment the user scrolls up to read history;
    /// re-engaged when they scroll back to the bottom (re-engagement happens
    /// in `clamp_scroll`, since that's the only point where we know the
    /// real max-scroll position for the current viewport).
    pub auto_follow: bool,
}

impl Default for ChatPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatPanel {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            composer: build_composer(),
            scroll: 0,
            streaming: false,
            error: None,
            auto_follow: true,
        }
    }

    pub fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
        self.pin_to_bottom_if_following();
    }

    /// Make sure the most recent message is an assistant turn we can append
    /// blocks to. Used by the streaming + tool-call paths so a single
    /// assistant turn can carry interleaved text / tool-call / tool-result
    /// blocks without splitting into multiple message bubbles.
    fn ensure_assistant_message(&mut self) {
        let needs_new = !matches!(
            self.messages.last(),
            Some(m) if m.role == ChatRole::Assistant
        );
        if needs_new {
            self.messages.push(ChatMessage {
                role: ChatRole::Assistant,
                blocks: Vec::new(),
                timestamp: Utc::now(),
            });
        }
    }

    pub fn append_assistant_text(&mut self, delta: &str) {
        self.ensure_assistant_message();
        let last = self.messages.last_mut().expect("ensure_assistant_message");
        match last.blocks.last_mut() {
            Some(ChatBlock::Text(s)) => s.push_str(delta),
            _ => last.blocks.push(ChatBlock::Text(delta.to_string())),
        }
        self.pin_to_bottom_if_following();
    }

    pub fn append_tool_call(&mut self, id: String, name: String, args_json: String) {
        self.ensure_assistant_message();
        self.messages
            .last_mut()
            .expect("ensure_assistant_message")
            .blocks
            .push(ChatBlock::ToolCall {
                id,
                name,
                args_json,
            });
        self.pin_to_bottom_if_following();
    }

    pub fn append_tool_result(
        &mut self,
        call_id: String,
        name: String,
        output: String,
        error: Option<String>,
    ) {
        self.ensure_assistant_message();
        self.messages
            .last_mut()
            .expect("ensure_assistant_message")
            .blocks
            .push(ChatBlock::ToolResult {
                call_id,
                name,
                output,
                error,
            });
        self.pin_to_bottom_if_following();
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll = 0;
        self.streaming = false;
        self.error = None;
        self.auto_follow = true;
    }

    /// Sentinel-set the scroll to the maximum so the next render's
    /// `clamp_scroll` snaps it to the actual last viewport-page. Cheap:
    /// we don't need to know the line count here, the renderer does.
    fn pin_to_bottom_if_following(&mut self) {
        if self.auto_follow {
            self.scroll = u16::MAX;
        }
    }

    /// Trimmed first-line view of the composer. The chat composer is
    /// multi-line in principle but we send it as a single "user message"
    /// — newlines in the buffer are preserved verbatim, only leading and
    /// trailing whitespace at the message boundaries is trimmed.
    pub fn composer_text(&self) -> String {
        self.composer.lines().join("\n").trim().to_string()
    }

    /// Replace the composer with an empty one. Called after submit so the
    /// user lands in a fresh buffer.
    pub fn reset_composer(&mut self) {
        self.composer = build_composer();
    }

    /// Move the message log up by `n` lines. Saturates at 0. Disengages
    /// auto-follow because the user is explicitly looking at history;
    /// mid-stream tokens shouldn't yank them back to the bottom.
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
        self.auto_follow = false;
    }

    /// Move the message log down by `n` lines. The render-time
    /// `clamp_scroll` re-engages `auto_follow` when this lands at the
    /// bottom, so the user just scrolling down to "now" automatically
    /// re-subscribes to live updates without a separate keybind.
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }

    /// Jump to the top of the log. Stops following the bottom — same
    /// reasoning as `scroll_up`.
    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
        self.auto_follow = false;
    }

    /// Jump to the bottom of the log and re-engage auto-follow.
    /// `u16::MAX` is the sentinel "render at the actual last line";
    /// `clamp_scroll` lowers it to the real max for the current viewport.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll = u16::MAX;
        self.auto_follow = true;
    }

    /// Clamp `scroll` against the actual rendered height. Called from
    /// `render_workspace` because content/viewport heights are
    /// render-time properties (driven by the area's width and the
    /// paragraph wrap) — the state layer has no view of either.
    ///
    /// Side effect: when the clamped scroll equals the maximum, we
    /// re-engage `auto_follow`. That's how PgDn back to "now" lets the
    /// user resume riding new tokens automatically.
    pub fn clamp_scroll(&mut self, content_height: u16, viewport_height: u16) {
        let max_scroll = content_height.saturating_sub(viewport_height);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        if self.scroll >= max_scroll {
            self.auto_follow = true;
        }
    }

    /// Total rendered height of the message log at this width. Exact —
    /// it walks the same visual-line layout the renderer uses, so
    /// `clamp_scroll` snaps to a max where the bottom slice truly ends
    /// on the last visual line. (The previous `ceil(chars/width)`
    /// estimator drifted from `Paragraph::wrap`'s word-aware behaviour,
    /// causing the tail to render below the viewport — which the user
    /// saw as text "behind" the composer.)
    pub fn content_height(&self, width: u16) -> u16 {
        if self.messages.is_empty() {
            return 1; // placeholder line "No messages yet…"
        }
        let mut total: u16 = 0;
        for (idx, msg) in self.messages.iter().enumerate() {
            if idx > 0 {
                total = total.saturating_add(1); // separator
            }
            // Role header is short ("▌ rowdy"); treat as 1 visual line.
            total = total.saturating_add(1);
            for block in &msg.blocks {
                total = total.saturating_add(block_visual_lines(block, width));
            }
        }
        total
    }
}

fn block_visual_lines(block: &ChatBlock, width: u16) -> u16 {
    match block {
        ChatBlock::Text(s) => {
            let mut total: u16 = 0;
            for raw_line in s.split('\n') {
                total = total.saturating_add(wrap_visual_count(raw_line, width));
            }
            total
        }
        ChatBlock::ToolCall {
            name, args_json, ..
        } => {
            let raw = format!("◆ tool: {name}({args_json})");
            wrap_visual_count(&raw, width)
        }
        ChatBlock::ToolResult { name, error, .. } => {
            let prefix = if error.is_some() {
                "✗ result"
            } else {
                "✓ result"
            };
            let raw = format!("{prefix}: {name}");
            wrap_visual_count(&raw, width)
        }
    }
}

fn wrap_visual_count(text: &str, width: u16) -> u16 {
    let n = wrap_text(text, width).len();
    n.min(u16::MAX as usize) as u16
}

/// Word-aware greedy wrap. Whitespace runs are absorbed at line breaks;
/// non-whitespace runs longer than `width` are hard-wrapped by chars.
/// Empty input returns one empty line so a blank message still occupies
/// a visual row.
pub fn wrap_text(text: &str, width: u16) -> Vec<String> {
    let w = width.max(1) as usize;
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let is_ws = chars[i].is_whitespace();
        while i < chars.len() && chars[i].is_whitespace() == is_ws {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        let token_w = i - start;
        if current_w + token_w <= w {
            current.push_str(&token);
            current_w += token_w;
            continue;
        }
        // Doesn't fit on the current line.
        if is_ws {
            // Whitespace at a wrap boundary is dropped — start a new line.
            lines.push(std::mem::take(&mut current));
            current_w = 0;
            continue;
        }
        // Non-whitespace token. Flush whatever's on the current line —
        // current_w is always overwritten in the branches below.
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if token_w > w {
            // Token longer than the panel — hard-wrap by char count.
            let mut buf = String::new();
            let mut buf_w = 0usize;
            for ch in token.chars() {
                buf.push(ch);
                buf_w += 1;
                if buf_w == w {
                    lines.push(std::mem::take(&mut buf));
                    buf_w = 0;
                }
            }
            current = buf;
            current_w = buf_w;
        } else {
            current.push_str(&token);
            current_w = token_w;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn build_composer() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text("Ask anything · Enter to send · Shift+Enter for newline");
    input.set_cursor_line_style(Style::default());
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_assistant_text_concatenates_into_last_block() {
        let mut chat = ChatPanel::new();
        chat.push_message(ChatMessage::user_text("hi"));
        chat.append_assistant_text("hel");
        chat.append_assistant_text("lo");
        assert_eq!(chat.messages.len(), 2);
        let last = &chat.messages[1];
        assert_eq!(last.role, ChatRole::Assistant);
        match &last.blocks[..] {
            [ChatBlock::Text(s)] => assert_eq!(s, "hello"),
            _ => panic!("expected single text block"),
        }
    }

    #[test]
    fn append_assistant_after_user_starts_new_message() {
        let mut chat = ChatPanel::new();
        chat.push_message(ChatMessage::user_text("first"));
        chat.append_assistant_text("answer");
        chat.push_message(ChatMessage::user_text("second"));
        chat.append_assistant_text("again");
        assert_eq!(chat.messages.len(), 4);
        assert_eq!(chat.messages[1].role, ChatRole::Assistant);
        assert_eq!(chat.messages[3].role, ChatRole::Assistant);
    }

    #[test]
    fn tool_call_and_result_attach_to_current_assistant_turn() {
        let mut chat = ChatPanel::new();
        chat.push_message(ChatMessage::user_text("describe the users table"));
        chat.append_assistant_text("Let me check.");
        chat.append_tool_call("c1".into(), "describe_table".into(), "{}".into());
        chat.append_tool_result("c1".into(), "describe_table".into(), "{}".into(), None);
        chat.append_assistant_text("Here it is.");
        assert_eq!(chat.messages.len(), 2);
        let assistant = &chat.messages[1];
        assert_eq!(assistant.blocks.len(), 4);
        assert!(matches!(&assistant.blocks[0], ChatBlock::Text(s) if s == "Let me check."));
        assert!(matches!(&assistant.blocks[1], ChatBlock::ToolCall { .. }));
        assert!(matches!(&assistant.blocks[2], ChatBlock::ToolResult { .. }));
        assert!(matches!(&assistant.blocks[3], ChatBlock::Text(s) if s == "Here it is."));
    }

    #[test]
    fn tool_call_without_prior_text_creates_assistant_message() {
        let mut chat = ChatPanel::new();
        chat.push_message(ChatMessage::user_text("query"));
        chat.append_tool_call("c1".into(), "list_tables".into(), "{}".into());
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[1].role, ChatRole::Assistant);
        assert!(matches!(
            &chat.messages[1].blocks[0],
            ChatBlock::ToolCall { .. }
        ));
    }

    #[test]
    fn clear_resets_everything() {
        let mut chat = ChatPanel::new();
        chat.push_message(ChatMessage::user_text("x"));
        chat.streaming = true;
        chat.error = Some("oops".into());
        chat.scroll = 5;
        chat.clear();
        assert!(chat.messages.is_empty());
        assert!(!chat.streaming);
        assert!(chat.error.is_none());
        assert_eq!(chat.scroll, 0);
    }

    #[test]
    fn scroll_up_saturates_at_zero() {
        let mut chat = ChatPanel::new();
        chat.scroll = 3;
        chat.scroll_up(10);
        assert_eq!(chat.scroll, 0);
    }

    #[test]
    fn auto_follow_pins_scroll_when_pushing_messages() {
        let mut chat = ChatPanel::new();
        // Default state follows the bottom.
        assert!(chat.auto_follow);
        chat.push_message(ChatMessage::user_text("hi"));
        // The sentinel is set; render-time clamp_scroll lowers it.
        assert_eq!(chat.scroll, u16::MAX);
    }

    #[test]
    fn scroll_up_disengages_auto_follow() {
        let mut chat = ChatPanel::new();
        chat.scroll = 12;
        chat.auto_follow = true;
        chat.scroll_up(3);
        assert!(!chat.auto_follow);
        assert_eq!(chat.scroll, 9);
    }

    #[test]
    fn streaming_does_not_pin_after_user_scrolled_up() {
        let mut chat = ChatPanel::new();
        chat.push_message(ChatMessage::user_text("first"));
        chat.scroll_up(5);
        let scroll_before = chat.scroll;
        chat.append_assistant_text("token");
        // User had scrolled up to read history; mid-stream tokens
        // should not yank them back to the bottom.
        assert!(!chat.auto_follow);
        assert_eq!(chat.scroll, scroll_before);
    }

    #[test]
    fn clamp_scroll_lowers_overshoot_and_reengages_follow() {
        let mut chat = ChatPanel::new();
        chat.scroll = u16::MAX;
        chat.auto_follow = false;
        // 50 lines content, 10 lines viewport → max_scroll = 40.
        chat.clamp_scroll(50, 10);
        assert_eq!(chat.scroll, 40);
        assert!(chat.auto_follow);
    }

    #[test]
    fn clamp_scroll_leaves_scroll_alone_when_browsing() {
        let mut chat = ChatPanel::new();
        chat.scroll = 5;
        chat.auto_follow = false;
        chat.clamp_scroll(50, 10);
        assert_eq!(chat.scroll, 5);
        assert!(!chat.auto_follow);
    }

    #[test]
    fn scroll_to_bottom_re_engages_auto_follow() {
        let mut chat = ChatPanel::new();
        chat.auto_follow = false;
        chat.scroll = 7;
        chat.scroll_to_bottom();
        assert!(chat.auto_follow);
        assert_eq!(chat.scroll, u16::MAX);
    }

    #[test]
    fn content_height_handles_empty_and_wrapped_messages() {
        let mut chat = ChatPanel::new();
        // Empty: placeholder line.
        assert_eq!(chat.content_height(40), 1);

        chat.push_message(ChatMessage::user_text("a".repeat(45)));
        // Width 20 → message wraps into 3 lines (one 45-char word
        // hard-wraps: 20 + 20 + 5) + 1 header.
        assert_eq!(chat.content_height(20), 4);
    }

    #[test]
    fn wrap_text_breaks_on_word_boundaries() {
        let lines = wrap_text("the quick brown fox", 10);
        // "the quick " is 10 chars (fits), "brown fox" wraps after.
        assert!(lines.iter().all(|l| l.chars().count() <= 10));
        // No partial words across lines (each line is a complete prefix).
        let recombined: String = lines.join("");
        // Whitespace at wrap boundaries is dropped, so recombined may
        // omit one separator space — assert that we kept all the
        // non-whitespace content.
        let nws: String = recombined.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(nws, "thequickbrownfox");
    }

    #[test]
    fn wrap_text_hard_wraps_oversize_words() {
        // 25 a's at width 10 → 10 + 10 + 5.
        let lines = wrap_text(&"a".repeat(25), 10);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].chars().count(), 10);
        assert_eq!(lines[1].chars().count(), 10);
        assert_eq!(lines[2].chars().count(), 5);
    }

    #[test]
    fn wrap_text_empty_returns_one_blank_line() {
        // A blank line still occupies one row in the rendered log so
        // separators don't collapse on the way to the bottom.
        assert_eq!(wrap_text("", 20), vec![String::new()]);
    }

    #[test]
    fn content_height_matches_renderer_visual_lines() {
        // Regression for the "tail behind composer" bug: clamp_scroll
        // must use the same line count as the renderer would emit.
        let mut chat = ChatPanel::new();
        chat.push_message(ChatMessage::user_text(
            "hi there friend, this is a longish message that will wrap",
        ));
        chat.append_assistant_text("answering with several short words here");
        // Compute expected = sum of role-header (1 each) + separator (1)
        // + wrapped lines per text block.
        let width = 20;
        let user_lines = wrap_text(
            "hi there friend, this is a longish message that will wrap",
            width,
        )
        .len() as u16;
        let asst_lines = wrap_text("answering with several short words here", width).len() as u16;
        let expected =
            1 /* user header */ + user_lines + 1 /* sep */ + 1 /* asst header */ + asst_lines;
        assert_eq!(chat.content_height(width), expected);
    }
}
