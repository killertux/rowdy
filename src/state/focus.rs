#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Schema,
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
