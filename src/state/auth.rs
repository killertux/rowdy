use ratatui::style::Style;
use ratatui_textarea::TextArea;

use crate::config::CryptoBlock;

pub const MAX_ATTEMPTS: u8 = 3;

#[derive(Debug)]
pub struct AuthState {
    pub input: TextArea<'static>,
    /// Number of *failed* attempts so far.
    pub attempts: u8,
    /// Last attempt's failure message — `None` on a fresh prompt.
    pub error: Option<String>,
    pub kind: AuthKind,
}

/// What the prompt is actually authenticating against.
#[derive(Debug, Clone)]
pub enum AuthKind {
    /// First launch with no `[crypto]` block in config. Empty submit means
    /// "store stays plaintext"; non-empty initialises a fresh crypto block
    /// using the entered password.
    FirstSetup,
    /// A `[crypto]` block exists. Submit derives a key and checks it against
    /// the verifier blob; failure increments `attempts`.
    Unlock { block: CryptoBlock },
}

impl AuthState {
    pub fn new(kind: AuthKind) -> Self {
        Self {
            input: build_input(),
            attempts: 0,
            error: None,
            kind,
        }
    }

    /// Wipe the input buffer between attempts. `TextArea` doesn't zeroize on
    /// drop — replacing the whole widget at least drops the old `String`
    /// promptly so the password doesn't sit around in our state.
    pub fn clear_input(&mut self) {
        self.input = build_input();
    }

    /// True when one more failed attempt would exhaust `MAX_ATTEMPTS`.
    pub fn attempts_remaining(&self) -> u8 {
        MAX_ATTEMPTS.saturating_sub(self.attempts)
    }
}

fn build_input() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_mask_char('•');
    input.set_placeholder_text("Please enter your password");
    // Disable the "current line" highlight so a single-line input looks
    // like an inline field, not a code editor row.
    input.set_cursor_line_style(Style::default());
    input
}
