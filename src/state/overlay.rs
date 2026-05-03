//! Transient input-preempting layer floating over a [`Screen`].
//!
//! When `app.overlay` is `Some(_)`, the active overlay decides what to
//! do with keystrokes (and what the bottom bar / centered popover
//! shows); when it's `None`, the underlying [`Screen`] is in charge.
//! Closing an overlay just clears the option, so the screen the user
//! was on stays put — the run prompt over the editor returns to the
//! editor; `:help` over `ResultExpanded` returns to the grid.

use crate::state::command::CommandBuffer;
use crate::state::llm_settings::LlmSettingsState;

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
    /// re-derive it. `reason` distinguishes a manually requested
    /// confirm (`<leader>r`) from an auto-prompt fired because the
    /// statement looked dangerous (`UPDATE` / `DELETE` without WHERE,
    /// `TRUNCATE`).
    ConfirmRun {
        statement: String,
        reason: ConfirmRunReason,
    },
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
    /// `:chat settings` modal — choose a provider, enter / update an
    /// API key. The state struct owns the `TextArea`s and focus.
    LlmSettings(LlmSettingsState),
    /// "rowdy v0.6.2 → v0.7.0 — update? (y/n)" prompt fired by the
    /// background `crate::update` check on startup. Dismissal records
    /// the latest tag in user-config so we don't re-prompt for the
    /// same version.
    UpdateAvailable { current: String, latest: String },
    /// Chat agent asked to use a filesystem read tool while
    /// `ReadToolsMode::Ask` is active. The actual `oneshot::Sender`
    /// the worker is waiting on lives in `app.pending_approval_tools`
    /// (overlays must stay `Debug`-able and clonable-ish, so the
    /// sender doesn't fit here directly). On y/Enter the action layer
    /// drains the pending entry and runs the tool; on n/Esc it sends
    /// a refusal back to the LLM so the turn doesn't stall.
    ConfirmToolUse {
        call_id: String,
        name: String,
        args_json: String,
    },
}

/// Why the confirm-run overlay opened. Drives the headline at the top
/// of the prompt so users know whether they hit the dangerous-statement
/// guardrail or just the normal `<leader>r` confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmRunReason {
    /// Standard "press Enter to run" prompt. No extra warning copy.
    Manual,
    /// Auto-fired because the SQL looked destructive (e.g. "DELETE
    /// without WHERE", "TRUNCATE"). The string is the user-facing
    /// reason shown in the headline.
    Destructive(&'static str),
}
