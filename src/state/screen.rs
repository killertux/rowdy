//! The persistent UI layer — what the user is *on*, regardless of any
//! transient prompt or popover floating above it.
//!
//! [`Screen`] is exhaustive: every full-app surface lives here. Things
//! that float on top (the `:` command bar, the run-confirmation prompt,
//! the in-flight connect spinner, the `:help` popover) live in
//! [`crate::state::overlay::Overlay`] instead. The split keeps "where am
//! I?" separate from "what's interrupting me?", so opening Help from
//! the result-grid doesn't lose the result-grid.

use crate::state::auth::AuthState;
use crate::state::conn_form::ConnFormState;
use crate::state::conn_list::ConnListState;
use crate::state::results::{ColumnView, ResultCursor, ResultId, ResultViewMode};

// `TextArea` is ~700 bytes (the `Auth`/`EditConnection` variants carry one or
// two each), so the variants are uneven. Screen lives once per App and is
// swapped in place — boxing buys nothing here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Screen {
    /// The default editor + schema browser layout.
    Normal,
    /// Full-screen result grid; `view` carries Visual / YankFormat sub-state.
    ResultExpanded {
        id: ResultId,
        cursor: ResultCursor,
        /// Absolute index of the leftmost visible column. Render keeps
        /// this in sync with `cursor.col` so the active cell is always
        /// on-screen.
        col_offset: usize,
        /// Absolute index of the topmost visible row. Same render-time
        /// clamp as `col_offset`, but for vertical scroll.
        row_offset: usize,
        /// Visual selection / yank-format prompt sub-state.
        view: ResultViewMode,
        /// Per-grid column reorder + hide state. Reset every time the
        /// expanded view opens.
        column_view: ColumnView,
    },
    /// Pre-app password prompt — shown when the store is encrypted, or
    /// on first launch before the user has chosen plaintext-vs-encrypted
    /// mode.
    Auth(AuthState),
    /// Inline create/edit form for a single connection.
    EditConnection(ConnFormState),
    /// Browseable list of saved connections — opens via `:conn`.
    ConnectionList(ConnListState),
}
