//! User-rebindable action IDs.
//!
//! IDs are kebab-case strings stable across rowdy versions. Adding a
//! variant here is a public-API change for the keybindings file.
//!
//! Not every `Action` is bindable — sub-mode-dependent keys (e.g. `Esc`
//! in the expanded result view, `Enter` in chat composer) and chord
//! openers (`<Space>`, `Ctrl+W`, `g`/`G`) stay hardcoded in
//! `event::translate_*` and are intentionally absent from this enum.

use edtui::EditorMode;

use crate::action::Action;
use crate::app::App;
use crate::state::right_panel::RightPanelMode;

/// Every variant has a 1:1 kebab-case string ID for the on-disk
/// keybindings file. The mapping lives in `parse` / `as_str` and is
/// covered by a round-trip test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindableAction {
    // GlobalImmediate (single-press, post-chord-arming).
    OpenCommand,
    FormatBuffer,
    GrowSchema,
    ShrinkSchema,
    OpenCompletionPopover,

    // Leader (after `<Space>`).
    /// `r` — runs the selection in editor Visual mode, otherwise
    /// prompts to run the statement under the cursor.
    RunPromptOrSelection,
    RunStatementUnderCursor,
    CancelQuery,
    ExpandLatestResult,
    ToggleTheme,
    SetRightPanelSchema,
    SetRightPanelChat,
    /// `<Space>n` cycles to the next per-connection editor session.
    /// Other `:session` subcommands stay command-only — keep the
    /// keybinding surface conservative and let users add their own
    /// chord overrides if they want.
    SessionNext,
    /// `<Space>!` … `<Space>(` jump straight to session `N` (1..=9).
    /// The shifted-digit gesture mirrors Vim's quickfix nav and gives
    /// users a single-chord switch when they know the index they
    /// want — no need to spell out `:session N`. Bound with
    /// `<S-1>` in the README's chord notation, but on every layout we
    /// support that key produces the corresponding shifted symbol
    /// (`!`, `@`, …), which is how the keymap stores it.
    SessionSwitch(u8),

    // Schema panel.
    SchemaUp,
    SchemaDown,
    SchemaCollapseOrAscend,
    SchemaExpandOrDescend,
    SchemaToggle,
    SchemaBottom,

    // Expanded result view (stateless keys only — q/Esc/v stay
    // hardcoded since they branch on the result-view sub-mode).
    ResultYank,
    ResultColumnMoveLeft,
    ResultColumnMoveRight,
    ResultColumnHide,
    ResultColumnReset,
    ResultLeft,
    ResultRight,
    ResultUp,
    ResultDown,
    ResultLineStart,
    ResultLineEnd,
    ResultBottom,

    // Chat normal mode (composer dormant; Esc stays hardcoded).
    ChatEnterInsert,
    ChatScrollUp,
    ChatScrollDown,
    ChatPageUp,
    ChatPageDown,
    ChatTop,
    ChatBottom,
}

#[allow(dead_code)] // help-popover refactor (US-011 follow-up) consumes the rest.
impl BindableAction {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open-command" => Self::OpenCommand,
            "format-buffer" => Self::FormatBuffer,
            "grow-schema" => Self::GrowSchema,
            "shrink-schema" => Self::ShrinkSchema,
            "open-completion-popover" => Self::OpenCompletionPopover,

            "run-prompt-or-selection" => Self::RunPromptOrSelection,
            "run-statement-under-cursor" => Self::RunStatementUnderCursor,
            "cancel-query" => Self::CancelQuery,
            "expand-latest-result" => Self::ExpandLatestResult,
            "toggle-theme" => Self::ToggleTheme,
            "set-right-panel-schema" => Self::SetRightPanelSchema,
            "set-right-panel-chat" => Self::SetRightPanelChat,
            "next-session" => Self::SessionNext,
            "session-switch-1" => Self::SessionSwitch(1),
            "session-switch-2" => Self::SessionSwitch(2),
            "session-switch-3" => Self::SessionSwitch(3),
            "session-switch-4" => Self::SessionSwitch(4),
            "session-switch-5" => Self::SessionSwitch(5),
            "session-switch-6" => Self::SessionSwitch(6),
            "session-switch-7" => Self::SessionSwitch(7),
            "session-switch-8" => Self::SessionSwitch(8),
            "session-switch-9" => Self::SessionSwitch(9),

            "schema-up" => Self::SchemaUp,
            "schema-down" => Self::SchemaDown,
            "schema-collapse-or-ascend" => Self::SchemaCollapseOrAscend,
            "schema-expand-or-descend" => Self::SchemaExpandOrDescend,
            "schema-toggle" => Self::SchemaToggle,
            "schema-bottom" => Self::SchemaBottom,

            "result-yank" => Self::ResultYank,
            "result-column-move-left" => Self::ResultColumnMoveLeft,
            "result-column-move-right" => Self::ResultColumnMoveRight,
            "result-column-hide" => Self::ResultColumnHide,
            "result-column-reset" => Self::ResultColumnReset,
            "result-left" => Self::ResultLeft,
            "result-right" => Self::ResultRight,
            "result-up" => Self::ResultUp,
            "result-down" => Self::ResultDown,
            "result-line-start" => Self::ResultLineStart,
            "result-line-end" => Self::ResultLineEnd,
            "result-bottom" => Self::ResultBottom,

            "chat-enter-insert" => Self::ChatEnterInsert,
            "chat-scroll-up" => Self::ChatScrollUp,
            "chat-scroll-down" => Self::ChatScrollDown,
            "chat-page-up" => Self::ChatPageUp,
            "chat-page-down" => Self::ChatPageDown,
            "chat-top" => Self::ChatTop,
            "chat-bottom" => Self::ChatBottom,

            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenCommand => "open-command",
            Self::FormatBuffer => "format-buffer",
            Self::GrowSchema => "grow-schema",
            Self::ShrinkSchema => "shrink-schema",
            Self::OpenCompletionPopover => "open-completion-popover",

            Self::RunPromptOrSelection => "run-prompt-or-selection",
            Self::RunStatementUnderCursor => "run-statement-under-cursor",
            Self::CancelQuery => "cancel-query",
            Self::ExpandLatestResult => "expand-latest-result",
            Self::ToggleTheme => "toggle-theme",
            Self::SetRightPanelSchema => "set-right-panel-schema",
            Self::SetRightPanelChat => "set-right-panel-chat",
            Self::SessionNext => "next-session",
            Self::SessionSwitch(1) => "session-switch-1",
            Self::SessionSwitch(2) => "session-switch-2",
            Self::SessionSwitch(3) => "session-switch-3",
            Self::SessionSwitch(4) => "session-switch-4",
            Self::SessionSwitch(5) => "session-switch-5",
            Self::SessionSwitch(6) => "session-switch-6",
            Self::SessionSwitch(7) => "session-switch-7",
            Self::SessionSwitch(8) => "session-switch-8",
            Self::SessionSwitch(9) => "session-switch-9",
            // Out-of-range payload — every constructor we expose
            // (parse, all, the keymap defaults) is gated to 1..=9, so
            // hitting this arm means a programmer error elsewhere
            // rather than a user-facing bug. Round-trip through
            // `parse` returns `None` for the placeholder string.
            Self::SessionSwitch(_) => "session-switch-invalid",

            Self::SchemaUp => "schema-up",
            Self::SchemaDown => "schema-down",
            Self::SchemaCollapseOrAscend => "schema-collapse-or-ascend",
            Self::SchemaExpandOrDescend => "schema-expand-or-descend",
            Self::SchemaToggle => "schema-toggle",
            Self::SchemaBottom => "schema-bottom",

            Self::ResultYank => "result-yank",
            Self::ResultColumnMoveLeft => "result-column-move-left",
            Self::ResultColumnMoveRight => "result-column-move-right",
            Self::ResultColumnHide => "result-column-hide",
            Self::ResultColumnReset => "result-column-reset",
            Self::ResultLeft => "result-left",
            Self::ResultRight => "result-right",
            Self::ResultUp => "result-up",
            Self::ResultDown => "result-down",
            Self::ResultLineStart => "result-line-start",
            Self::ResultLineEnd => "result-line-end",
            Self::ResultBottom => "result-bottom",

            Self::ChatEnterInsert => "chat-enter-insert",
            Self::ChatScrollUp => "chat-scroll-up",
            Self::ChatScrollDown => "chat-scroll-down",
            Self::ChatPageUp => "chat-page-up",
            Self::ChatPageDown => "chat-page-down",
            Self::ChatTop => "chat-top",
            Self::ChatBottom => "chat-bottom",
        }
    }

    pub const fn all() -> &'static [Self] {
        &[
            Self::OpenCommand,
            Self::FormatBuffer,
            Self::GrowSchema,
            Self::ShrinkSchema,
            Self::OpenCompletionPopover,
            Self::RunPromptOrSelection,
            Self::RunStatementUnderCursor,
            Self::CancelQuery,
            Self::ExpandLatestResult,
            Self::ToggleTheme,
            Self::SetRightPanelSchema,
            Self::SetRightPanelChat,
            Self::SessionNext,
            Self::SessionSwitch(1),
            Self::SessionSwitch(2),
            Self::SessionSwitch(3),
            Self::SessionSwitch(4),
            Self::SessionSwitch(5),
            Self::SessionSwitch(6),
            Self::SessionSwitch(7),
            Self::SessionSwitch(8),
            Self::SessionSwitch(9),
            Self::SchemaUp,
            Self::SchemaDown,
            Self::SchemaCollapseOrAscend,
            Self::SchemaExpandOrDescend,
            Self::SchemaToggle,
            Self::SchemaBottom,
            Self::ResultYank,
            Self::ResultColumnMoveLeft,
            Self::ResultColumnMoveRight,
            Self::ResultColumnHide,
            Self::ResultColumnReset,
            Self::ResultLeft,
            Self::ResultRight,
            Self::ResultUp,
            Self::ResultDown,
            Self::ResultLineStart,
            Self::ResultLineEnd,
            Self::ResultBottom,
            Self::ChatEnterInsert,
            Self::ChatScrollUp,
            Self::ChatScrollDown,
            Self::ChatPageUp,
            Self::ChatPageDown,
            Self::ChatTop,
            Self::ChatBottom,
        ]
    }

    /// One-line description for the `:help` popover. Stable enough to
    /// snapshot.
    pub fn description(self) -> &'static str {
        match self {
            Self::OpenCommand => "Open command prompt",
            Self::FormatBuffer => "Format SQL (Visual: selection; Normal: whole buffer)",
            Self::GrowSchema => "Grow schema panel width",
            Self::ShrinkSchema => "Shrink schema panel width",
            Self::OpenCompletionPopover => "Open SQL autocomplete popover",
            Self::RunPromptOrSelection => {
                "Run selection (Visual) / prompt to run statement (Normal)"
            }
            Self::RunStatementUnderCursor => "Run the statement under the cursor — no prompt",
            Self::CancelQuery => "Cancel the in-flight query",
            Self::ExpandLatestResult => "Expand the latest result to full view",
            Self::ToggleTheme => "Toggle Dark / Light theme",
            Self::SetRightPanelSchema => "Switch right panel to schema (and focus it)",
            Self::SetRightPanelChat => "Switch right panel to chat (and focus it)",
            Self::SessionNext => "Cycle to the next per-connection editor session",
            Self::SessionSwitch(1) => "Switch directly to session 1",
            Self::SessionSwitch(2) => "Switch directly to session 2",
            Self::SessionSwitch(3) => "Switch directly to session 3",
            Self::SessionSwitch(4) => "Switch directly to session 4",
            Self::SessionSwitch(5) => "Switch directly to session 5",
            Self::SessionSwitch(6) => "Switch directly to session 6",
            Self::SessionSwitch(7) => "Switch directly to session 7",
            Self::SessionSwitch(8) => "Switch directly to session 8",
            Self::SessionSwitch(9) => "Switch directly to session 9",
            Self::SessionSwitch(_) => "Switch directly to session N (invalid index)",
            Self::SchemaUp => "Schema: move selection up",
            Self::SchemaDown => "Schema: move selection down",
            Self::SchemaCollapseOrAscend => "Schema: collapse node or move to parent",
            Self::SchemaExpandOrDescend => "Schema: expand node (loads on first expand) or descend",
            Self::SchemaToggle => "Schema: toggle expand / collapse",
            Self::SchemaBottom => "Schema: jump to bottom",
            Self::ResultYank => "Result view: yank cell or selection",
            Self::ResultColumnMoveLeft => "Result view: move column left",
            Self::ResultColumnMoveRight => "Result view: move column right",
            Self::ResultColumnHide => "Result view: hide column",
            Self::ResultColumnReset => "Result view: reset column layout",
            Self::ResultLeft => "Result view: move cursor left",
            Self::ResultRight => "Result view: move cursor right",
            Self::ResultUp => "Result view: move cursor up",
            Self::ResultDown => "Result view: move cursor down",
            Self::ResultLineStart => "Result view: first column in row",
            Self::ResultLineEnd => "Result view: last column in row",
            Self::ResultBottom => "Result view: last row",
            Self::ChatEnterInsert => "Chat: enter insert mode (focus composer)",
            Self::ChatScrollUp => "Chat: scroll log up by one line",
            Self::ChatScrollDown => "Chat: scroll log down by one line",
            Self::ChatPageUp => "Chat: scroll log up by a page",
            Self::ChatPageDown => "Chat: scroll log down by a page",
            Self::ChatTop => "Chat: jump to top of log",
            Self::ChatBottom => "Chat: jump to bottom of log",
        }
    }

    /// Resolve a bindable action to a concrete `Action`. Most are 1:1;
    /// the editor-mode-dependent variant branches on `app.editor`.
    pub fn into_action(self, app: &App) -> Action {
        let is_visual = app.editor.editor_mode() == EditorMode::Visual;
        self.to_action(is_visual)
    }

    /// App-less variant: equivalent to `into_action` from a non-Visual
    /// editor mode. Used by callers that don't have an `&App` handle
    /// (e.g. `event::translate_schema_key`, where the schema panel
    /// has focus and the editor cannot be in Visual mode anyway).
    pub fn into_action_no_visual(self) -> Action {
        self.to_action(false)
    }

    fn to_action(self, editor_in_visual: bool) -> Action {
        use crate::action::{ChatAction, ResultColumnAction, ResultNavAction};

        match self {
            Self::OpenCommand => Action::OpenCommand,
            Self::FormatBuffer => Action::FormatEditor(crate::command::FormatScope::Cursor),
            // Today's `<` grows / `>` shrinks (see `event::translate_global`).
            // Magnitudes mirror the existing literals.
            Self::GrowSchema => Action::ResizeSchema(2),
            Self::ShrinkSchema => Action::ResizeSchema(-2),
            Self::OpenCompletionPopover => {
                Action::Completion(crate::action::CompletionAction::Open)
            }

            Self::RunPromptOrSelection => {
                if editor_in_visual {
                    Action::RunSelection
                } else {
                    Action::PrepareConfirmRun
                }
            }
            Self::RunStatementUnderCursor => Action::RunStatementUnderCursor,
            Self::CancelQuery => Action::CancelQuery,
            Self::ExpandLatestResult => Action::ExpandLatestResult,
            Self::ToggleTheme => Action::ToggleTheme,
            Self::SetRightPanelSchema => Action::SetRightPanel(RightPanelMode::Schema),
            Self::SetRightPanelChat => Action::SetRightPanel(RightPanelMode::Chat),
            Self::SessionNext => Action::Session(crate::action::SessionAction::Next),
            Self::SessionSwitch(n) => {
                Action::Session(crate::action::SessionAction::Switch(n as usize))
            }

            Self::SchemaUp => Action::Schema(crate::action::SchemaAction::Up),
            Self::SchemaDown => Action::Schema(crate::action::SchemaAction::Down),
            Self::SchemaCollapseOrAscend => {
                Action::Schema(crate::action::SchemaAction::CollapseOrAscend)
            }
            Self::SchemaExpandOrDescend => {
                Action::Schema(crate::action::SchemaAction::ExpandOrDescend)
            }
            Self::SchemaToggle => Action::Schema(crate::action::SchemaAction::Toggle),
            Self::SchemaBottom => Action::Schema(crate::action::SchemaAction::Bottom),

            Self::ResultYank => Action::ResultYank,
            Self::ResultColumnMoveLeft => Action::ResultColumn(ResultColumnAction::MoveLeft),
            Self::ResultColumnMoveRight => Action::ResultColumn(ResultColumnAction::MoveRight),
            Self::ResultColumnHide => Action::ResultColumn(ResultColumnAction::Hide),
            Self::ResultColumnReset => Action::ResultColumn(ResultColumnAction::Reset),
            Self::ResultLeft => Action::ResultNav(ResultNavAction::Left),
            Self::ResultRight => Action::ResultNav(ResultNavAction::Right),
            Self::ResultUp => Action::ResultNav(ResultNavAction::Up),
            Self::ResultDown => Action::ResultNav(ResultNavAction::Down),
            Self::ResultLineStart => Action::ResultNav(ResultNavAction::LineStart),
            Self::ResultLineEnd => Action::ResultNav(ResultNavAction::LineEnd),
            Self::ResultBottom => Action::ResultNav(ResultNavAction::Bottom),

            Self::ChatEnterInsert => Action::FocusPanel(crate::state::focus::Focus::ChatComposer),
            Self::ChatScrollUp => Action::Chat(ChatAction::ScrollUp(1)),
            Self::ChatScrollDown => Action::Chat(ChatAction::ScrollDown(1)),
            Self::ChatPageUp => Action::Chat(ChatAction::ScrollUp(8)),
            Self::ChatPageDown => Action::Chat(ChatAction::ScrollDown(8)),
            Self::ChatTop => Action::Chat(ChatAction::ScrollToTop),
            Self::ChatBottom => Action::Chat(ChatAction::ScrollToBottom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_as_str_round_trip_every_variant() {
        for &v in BindableAction::all() {
            let s = v.as_str();
            assert_eq!(
                BindableAction::parse(s),
                Some(v),
                "round-trip failed for {s}"
            );
        }
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(BindableAction::parse("nope"), None);
        assert_eq!(BindableAction::parse(""), None);
        assert_eq!(BindableAction::parse("Run-Statement-Under-Cursor"), None);
    }

    #[test]
    fn descriptions_are_non_empty() {
        for &v in BindableAction::all() {
            assert!(!v.description().is_empty(), "empty desc for {}", v.as_str());
        }
    }
}
