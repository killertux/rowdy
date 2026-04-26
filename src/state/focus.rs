use crate::state::auth::AuthState;
use crate::state::command::CommandBuffer;
use crate::state::conn_form::ConnFormState;
use crate::state::conn_list::ConnListState;
use crate::state::results::{ResultCursor, ResultId, ResultViewMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Schema,
}

// `TextArea` is ~700 bytes (the `Auth`/`EditConnection` variants carry one or
// two each), so the variants are uneven. Mode lives once per App and is
// swapped in place — boxing buys nothing here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Mode {
    Normal,
    Command(CommandBuffer),
    ResultExpanded {
        id: ResultId,
        cursor: ResultCursor,
        /// Absolute index of the leftmost visible column. Render keeps this in
        /// sync with `cursor.col` so the active cell is always on-screen.
        col_offset: usize,
        /// Absolute index of the topmost visible row. Same render-time clamp
        /// as `col_offset`, but for vertical scroll.
        row_offset: usize,
        /// Visual selection / yank-format prompt sub-state.
        view: ResultViewMode,
    },
    ConfirmRun {
        statement: String,
    },
    /// Pre-app password prompt — shown when the store is encrypted, or on
    /// first launch before the user has chosen plaintext-vs-encrypted mode.
    Auth(AuthState),
    /// Inline create/edit form for a single connection.
    EditConnection(ConnFormState),
    /// Browseable list of saved connections — opens via `:conn`.
    ConnectionList(ConnListState),
    /// Async connection in flight; UI shows "connecting to <name>…" and
    /// keys are mostly inert until `Connected`/`ConnectFailed` lands.
    Connecting {
        name: String,
    },
    /// Centered popover listing every keybinding and command. Opened
    /// with `:help` / `:?`. `scroll` is the topmost line shown;
    /// `h_scroll` is the leftmost column shown (for help entries that
    /// don't fit the popover width). Both are clamped at render time
    /// against the actual content size, so the next keystroke sees a
    /// sane value rather than an accumulated "past-the-end" count.
    Help {
        scroll: u16,
        h_scroll: u16,
    },
}

impl Mode {
    pub fn is_normal(&self) -> bool {
        matches!(self, Self::Normal)
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
