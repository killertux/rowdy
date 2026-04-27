//! Transient input-preempting layer floating over a [`Screen`].
//!
//! When `app.overlay` is `Some(_)`, the active overlay decides what to
//! do with keystrokes (and what the bottom bar / centered popover
//! shows); when it's `None`, the underlying [`Screen`] is in charge.
//! Closing an overlay just clears the option, so the screen the user
//! was on stays put — the run prompt over the editor returns to the
//! editor; `:help` over `ResultExpanded` returns to the grid.

use crate::state::command::CommandBuffer;

/// The layer that floats over the current [`crate::state::screen::Screen`].
//
// `CommandBuffer` carries a `TextArea` (~700 bytes) while the other variants
// are tiny — but `Overlay` lives in an `Option` on `App` and is replaced as a
// whole, so boxing buys nothing here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Overlay {
    /// `:` command bar at the bottom. The buffer carries the in-flight
    /// edit until Enter submits it.
    Command(CommandBuffer),
    /// "▶ run highlighted statement?" prompt — the matching SQL is
    /// snapshotted here so the dispatch on Enter doesn't need to
    /// re-derive it.
    ConfirmRun { statement: String },
    /// Async connection in flight; UI shows "connecting to <name>…"
    /// and keys are inert until `Connected`/`ConnectFailed` lands.
    Connecting { name: String },
    /// Centered popover listing every keybinding and command. Opened
    /// with `:help` / `:?`. `scroll` is the topmost line shown;
    /// `h_scroll` is the leftmost column shown (for help entries that
    /// don't fit the popover width). Both are clamped at render time
    /// against the actual content size, so the next keystroke sees a
    /// sane value rather than an accumulated "past-the-end" count.
    Help { scroll: u16, h_scroll: u16 },
}
