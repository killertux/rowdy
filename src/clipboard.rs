//! Thin wrapper around `arboard` for system-clipboard read/write.
//!
//! `arboard::Clipboard` is cheap to instantiate so we open a fresh handle on
//! each call rather than keeping one alive — keeping the handle long-term
//! can hold an X11 connection open and conflict with other apps. Failures
//! (no clipboard, headless, sandboxed) are logged and swallowed; the user
//! shouldn't see a UI hiccup because their session has no clipboard.

use crate::log::Logger;

const TARGET: &str = "clipboard";

/// Reads the current system clipboard text. Returns `None` if the clipboard
/// is unavailable or the contents aren't text.
pub fn read(logger: &Logger) -> Option<String> {
    match arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
        Ok(text) => Some(text),
        Err(err) => {
            logger.warn(TARGET, format!("read failed: {err}"));
            None
        }
    }
}

/// Best-effort write — doesn't block the UI on a failed clipboard. Empty
/// strings are skipped (some platforms reject empty puts).
pub fn write(logger: &Logger, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Err(err) = arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.to_string())) {
        logger.warn(TARGET, format!("write failed: {err}"));
    }
}
