use crate::state::command::CommandBuffer;
use crate::state::results::{ResultCursor, ResultId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Schema,
}

#[derive(Debug)]
pub enum Mode {
    Normal,
    Command(CommandBuffer),
    ResultExpanded { id: ResultId, cursor: ResultCursor },
    ConfirmRun { statement: String },
}

impl Mode {
    pub fn is_normal(&self) -> bool {
        matches!(self, Self::Normal)
    }

    pub fn command_buffer(&self) -> Option<&CommandBuffer> {
        match self {
            Self::Command(buf) => Some(buf),
            _ => None,
        }
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
