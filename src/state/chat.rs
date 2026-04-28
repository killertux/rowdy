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

/// Author of a chat message. The `Tool` role doesn't render as a balloon —
/// it appears as a `ToolResult` content block under the assistant's turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub blocks: Vec<ChatBlock>,
    /// Wall-clock time the message was created. Phase 5 reads this for
    /// session persistence ordering; phase 2 just stores it.
    #[allow(dead_code)]
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
        }
    }

    pub fn push_message(&mut self, msg: ChatMessage) {
        self.messages.push(msg);
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
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll = 0;
        self.streaming = false;
        self.error = None;
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

    /// Move the message log up by `n` lines. Saturates at 0.
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Move the message log down by `n` lines. Caller is responsible for
    /// clamping at render time against actual content height — we don't
    /// know it here.
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }
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
}
