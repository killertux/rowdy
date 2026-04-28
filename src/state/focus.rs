#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Schema,
    /// Chat panel in *normal* mode — keystrokes scroll the log, globals
    /// like `:` / leader / `Ctrl+W` work, `i` switches into
    /// [`Focus::ChatComposer`]. Mirrors edtui's Normal mode in spirit.
    Chat,
    /// Chat composer (TextArea) capturing keystrokes — `Esc` bounces back
    /// to [`Focus::Chat`] (normal mode).
    ChatComposer,
}

impl Focus {
    /// True for both chat focus modes — used by renderers/mouse handlers
    /// that care about "is the chat panel active" without distinguishing
    /// normal vs. insert.
    pub fn is_chat(self) -> bool {
        matches!(self, Focus::Chat | Focus::ChatComposer)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PendingChord {
    #[default]
    None,
    /// Ctrl+W was pressed; awaiting direction or resize key.
    Window,
    /// Leader key (space) was pressed in editor normal mode.
    Leader,
    /// `g` was pressed; awaiting another `g` for "go to top" of the active context.
    GG,
}
