//! Which panel the right side of the workspace shows.
//!
//! Toggles between the schema tree and the LLM chat. Independent of
//! [`crate::state::focus::Focus`] — focus says where keystrokes go, this
//! says what's painted. Tab rotation reads both: Tab into the right pane
//! sets focus to whichever the panel mode currently shows.
//!
//! Defaults to [`RightPanelMode::Schema`] so the existing UX is unchanged
//! until the user opts into the chat panel.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RightPanelMode {
    #[default]
    Schema,
    Chat,
}

impl RightPanelMode {
    pub fn toggle(self) -> Self {
        match self {
            Self::Schema => Self::Chat,
            Self::Chat => Self::Schema,
        }
    }

    pub fn is_chat(self) -> bool {
        matches!(self, Self::Chat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_round_trips() {
        let m = RightPanelMode::default();
        assert!(!m.is_chat());
        let m = m.toggle();
        assert!(m.is_chat());
        let m = m.toggle();
        assert!(!m.is_chat());
    }
}
