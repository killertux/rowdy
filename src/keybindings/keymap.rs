//! Keymap — chord → BindableAction lookup, per Context.
//!
//! `Context::GlobalImmediate`, `Leader`, and `Schema` are wired into
//! `event::translate_*` preludes; `Result`, `ChatNormal`, and
//! `ChatInsert` are populated for the help-popover render but their
//! keys still flow through the hardcoded matches in `event.rs`
//! (their behaviour depends on per-mode sub-state that is awkward to
//! express through a flat keymap).

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyModifiers};

use super::KeybindingsFile;
use super::actions::BindableAction;
use super::chord::{Chord, ChordParseError, KeyChord, parse as parse_chord};

/// Where in the input pipeline a chord is consulted. Note the
/// **deliberate absence** of a `Global` variant: chord-arming keys
/// (`<Space>`, `<C-w>`, `g`/`G`) trigger state transitions into
/// `PendingChord` and are not user-rebindable. The `GlobalImmediate`
/// context only contains single-press keys that *don't* arm a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    GlobalImmediate,
    Leader,
    Schema,
    Result,
    ChatNormal,
    ChatInsert,
}

impl Context {
    #[allow(dead_code)] // exposed for the upcoming help-popover refactor (US-011 follow-up).
    pub const ALL: [Self; 6] = [
        Self::GlobalImmediate,
        Self::Leader,
        Self::Schema,
        Self::Result,
        Self::ChatNormal,
        Self::ChatInsert,
    ];

    /// Lower-case-snake key matching the on-disk TOML table name.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::GlobalImmediate => "global_immediate",
            Self::Leader => "leader",
            Self::Schema => "schema",
            Self::Result => "result",
            Self::ChatNormal => "chat_normal",
            Self::ChatInsert => "chat_insert",
        }
    }

    #[allow(dead_code)] // help-popover follow-up.
    pub fn human(self) -> &'static str {
        match self {
            Self::GlobalImmediate => "Global (single-press)",
            Self::Leader => "Leader (after Space)",
            Self::Schema => "Schema panel",
            Self::Result => "Expanded result view",
            Self::ChatNormal => "Chat (normal mode)",
            Self::ChatInsert => "Chat (insert mode)",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    binds: HashMap<Context, HashMap<Chord, BindableAction>>,
}

#[derive(Debug)]
pub enum MergeError {
    Chord {
        context: Context,
        raw: String,
        err: ChordParseError,
    },
    Action {
        context: Context,
        chord: String,
        raw: String,
    },
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chord { context, raw, err } => {
                write!(f, "[{}] invalid chord {raw:?}: {err}", context.as_key())
            }
            Self::Action { context, chord, raw } => write!(
                f,
                "[{}] {chord:?}: unknown action {raw:?}",
                context.as_key()
            ),
        }
    }
}

impl Keymap {
    #[allow(dead_code)] // public ctor; exercised by tests / future callers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Default keymap that mirrors today's hardcoded behaviour.
    pub fn defaults() -> Self {
        let mut m = Self::default();
        for &(ctx, chord_str, action) in DEFAULTS {
            let chord = parse_chord(chord_str)
                .unwrap_or_else(|e| panic!("default chord {chord_str:?} unparseable: {e}"));
            m.insert(ctx, chord, action);
        }
        m
    }

    fn insert(&mut self, ctx: Context, chord: Chord, action: BindableAction) {
        self.binds.entry(ctx).or_default().insert(chord, action);
    }

    /// Apply sparse user overrides. Whole-load rollback: on the first
    /// error the input keymap is left untouched.
    pub fn merge_overrides(&mut self, file: &KeybindingsFile) -> Result<(), MergeError> {
        let entries = [
            (Context::GlobalImmediate, &file.global_immediate),
            (Context::Leader, &file.leader),
            (Context::Schema, &file.schema),
            (Context::Result, &file.result),
            (Context::ChatNormal, &file.chat_normal),
            (Context::ChatInsert, &file.chat_insert),
        ];
        // Validate all entries first; only mutate after every entry
        // parses cleanly. Keeps the all-or-nothing rollback invariant
        // explicit (B.4 in the work plan).
        let mut staged: Vec<(Context, Chord, BindableAction)> = Vec::new();
        for (ctx, table) in entries {
            for (raw_chord, raw_action) in table.iter() {
                let chord = parse_chord(raw_chord).map_err(|err| MergeError::Chord {
                    context: ctx,
                    raw: raw_chord.clone(),
                    err,
                })?;
                let action =
                    BindableAction::parse(raw_action).ok_or_else(|| MergeError::Action {
                        context: ctx,
                        chord: raw_chord.clone(),
                        raw: raw_action.clone(),
                    })?;
                staged.push((ctx, chord, action));
            }
        }
        for (ctx, chord, action) in staged {
            self.insert(ctx, chord, action);
        }
        Ok(())
    }

    pub fn lookup_chord(&self, ctx: Context, chord: &Chord) -> Option<BindableAction> {
        self.binds.get(&ctx).and_then(|t| t.get(chord)).copied()
    }

    /// Convenience for single-key lookups (most context-prelude calls
    /// from `event.rs` only need a length-1 chord).
    pub fn lookup_key(
        &self,
        ctx: Context,
        code: KeyCode,
        mods: KeyModifiers,
    ) -> Option<BindableAction> {
        let chord = Chord::single(KeyChord { code, mods });
        self.lookup_chord(ctx, &chord)
    }

    #[allow(dead_code)] // help-popover follow-up consumer (PRD US-011).
    pub fn iter_context(
        &self,
        ctx: Context,
    ) -> impl Iterator<Item = (&Chord, &BindableAction)> {
        self.binds.get(&ctx).into_iter().flat_map(|t| t.iter())
    }
}

// Single source of truth for default chords. `event::translate_*` for
// GlobalImmediate / Leader / Schema reads from this via
// `Keymap::lookup_*`; the help popover walks the same map. The other
// three contexts populate the help popover only — `event.rs` keeps
// hardcoded matches for them.
const DEFAULTS: &[(Context, &str, BindableAction)] = &[
    // --- GlobalImmediate (single press, post-chord-arming) ---
    (Context::GlobalImmediate, ":", BindableAction::OpenCommand),
    (Context::GlobalImmediate, "=", BindableAction::FormatBuffer),
    (Context::GlobalImmediate, "<", BindableAction::GrowSchema),
    (Context::GlobalImmediate, ">", BindableAction::ShrinkSchema),
    // Ctrl+Space stays hardcoded in `event::translate_normal_key`
    // because today's behavior gates it on `focus == Editor` —
    // surfacing it through GlobalImmediate would fire it from the
    // schema/chat panels too, which is a silent behavior change.
    // --- Leader (after `<Space>`) ---
    (Context::Leader, "r", BindableAction::RunPromptOrSelection),
    (
        Context::Leader,
        "R",
        BindableAction::RunStatementUnderCursor,
    ),
    (Context::Leader, "c", BindableAction::CancelQuery),
    (Context::Leader, "e", BindableAction::ExpandLatestResult),
    (Context::Leader, "t", BindableAction::ToggleTheme),
    (Context::Leader, "S", BindableAction::SetRightPanelSchema),
    (Context::Leader, "C", BindableAction::SetRightPanelChat),
    // --- Schema panel ---
    (Context::Schema, "j", BindableAction::SchemaDown),
    (Context::Schema, "k", BindableAction::SchemaUp),
    (Context::Schema, "h", BindableAction::SchemaCollapseOrAscend),
    (Context::Schema, "l", BindableAction::SchemaExpandOrDescend),
    (Context::Schema, "o", BindableAction::SchemaToggle),
    (Context::Schema, "<Enter>", BindableAction::SchemaToggle),
    (Context::Schema, "G", BindableAction::SchemaBottom),
    // --- Expanded result view (help-only; event.rs stays hardcoded) ---
    (Context::Result, "y", BindableAction::ResultYank),
    (Context::Result, "H", BindableAction::ResultColumnMoveLeft),
    (Context::Result, "L", BindableAction::ResultColumnMoveRight),
    (Context::Result, "x", BindableAction::ResultColumnHide),
    (Context::Result, "R", BindableAction::ResultColumnReset),
    (Context::Result, "h", BindableAction::ResultLeft),
    (Context::Result, "l", BindableAction::ResultRight),
    (Context::Result, "j", BindableAction::ResultDown),
    (Context::Result, "k", BindableAction::ResultUp),
    (Context::Result, "0", BindableAction::ResultLineStart),
    (Context::Result, "$", BindableAction::ResultLineEnd),
    (Context::Result, "G", BindableAction::ResultBottom),
    // --- Chat normal (help-only) ---
    (Context::ChatNormal, "i", BindableAction::ChatEnterInsert),
    (Context::ChatNormal, "k", BindableAction::ChatScrollUp),
    (Context::ChatNormal, "j", BindableAction::ChatScrollDown),
    (Context::ChatNormal, "<PageUp>", BindableAction::ChatPageUp),
    (Context::ChatNormal, "<PageDown>", BindableAction::ChatPageDown),
    (Context::ChatNormal, "<Home>", BindableAction::ChatTop),
    (Context::ChatNormal, "<End>", BindableAction::ChatBottom),
    (Context::ChatNormal, "G", BindableAction::ChatBottom),
    // --- Chat insert (help-only; only the chat-specific actions, not
    // the composer's text input or Enter-submit) ---
    (Context::ChatInsert, "<C-Up>", BindableAction::ChatScrollUp),
    (Context::ChatInsert, "<C-Down>", BindableAction::ChatScrollDown),
    (Context::ChatInsert, "<PageUp>", BindableAction::ChatPageUp),
    (Context::ChatInsert, "<PageDown>", BindableAction::ChatPageDown),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn defaults_table_parses_without_panicking() {
        let _ = Keymap::defaults();
    }

    #[test]
    fn default_leader_chords_match_event_rs() {
        let m = Keymap::defaults();
        assert_eq!(
            m.lookup_key(Context::Leader, KeyCode::Char('r'), KeyModifiers::NONE),
            Some(BindableAction::RunPromptOrSelection)
        );
        assert_eq!(
            m.lookup_key(Context::Leader, KeyCode::Char('R'), KeyModifiers::NONE),
            Some(BindableAction::RunStatementUnderCursor)
        );
        assert_eq!(
            m.lookup_key(Context::Leader, KeyCode::Char('c'), KeyModifiers::NONE),
            Some(BindableAction::CancelQuery)
        );
        assert_eq!(
            m.lookup_key(Context::Leader, KeyCode::Char('S'), KeyModifiers::NONE),
            Some(BindableAction::SetRightPanelSchema)
        );
    }

    #[test]
    fn default_global_immediate_chords_match_event_rs() {
        let m = Keymap::defaults();
        assert_eq!(
            m.lookup_key(
                Context::GlobalImmediate,
                KeyCode::Char(':'),
                KeyModifiers::NONE
            ),
            Some(BindableAction::OpenCommand)
        );
        assert_eq!(
            m.lookup_key(
                Context::GlobalImmediate,
                KeyCode::Char('='),
                KeyModifiers::NONE
            ),
            Some(BindableAction::FormatBuffer)
        );
        assert_eq!(
            m.lookup_key(
                Context::GlobalImmediate,
                KeyCode::Char('<'),
                KeyModifiers::NONE
            ),
            Some(BindableAction::GrowSchema)
        );
        assert_eq!(
            m.lookup_key(
                Context::GlobalImmediate,
                KeyCode::Char('>'),
                KeyModifiers::NONE
            ),
            Some(BindableAction::ShrinkSchema)
        );
        // Ctrl+Space is intentionally NOT in `GlobalImmediate` — the
        // editor-only autocomplete popover stays hardcoded in
        // `event::translate_normal_key` because today's behaviour
        // gates it on `focus == Editor`.
        assert_eq!(
            m.lookup_key(
                Context::GlobalImmediate,
                KeyCode::Char(' '),
                KeyModifiers::CONTROL
            ),
            None
        );
    }

    #[test]
    fn default_schema_chords_match_event_rs() {
        let m = Keymap::defaults();
        assert_eq!(
            m.lookup_key(Context::Schema, KeyCode::Char('j'), KeyModifiers::NONE),
            Some(BindableAction::SchemaDown)
        );
        assert_eq!(
            m.lookup_key(Context::Schema, KeyCode::Enter, KeyModifiers::NONE),
            Some(BindableAction::SchemaToggle)
        );
        assert_eq!(
            m.lookup_key(Context::Schema, KeyCode::Char('G'), KeyModifiers::NONE),
            Some(BindableAction::SchemaBottom)
        );
    }

    #[test]
    fn merge_overrides_applies_valid_sparse_table() {
        let mut m = Keymap::defaults();
        let mut file = KeybindingsFile::default();
        file.leader.insert("r".into(), "cancel-query".into());
        m.merge_overrides(&file).unwrap();

        // Override applied.
        assert_eq!(
            m.lookup_key(Context::Leader, KeyCode::Char('r'), KeyModifiers::NONE),
            Some(BindableAction::CancelQuery)
        );
        // Other defaults intact.
        assert_eq!(
            m.lookup_key(Context::Leader, KeyCode::Char('c'), KeyModifiers::NONE),
            Some(BindableAction::CancelQuery)
        );
        assert_eq!(
            m.lookup_key(Context::Leader, KeyCode::Char('t'), KeyModifiers::NONE),
            Some(BindableAction::ToggleTheme)
        );
    }

    #[test]
    fn merge_overrides_rolls_back_on_unknown_action() {
        let mut m = Keymap::defaults();
        let snapshot = format!("{:?}", m.binds);

        let mut file = KeybindingsFile::default();
        file.leader.insert("r".into(), "cancel-query".into()); // valid
        file.leader.insert("R".into(), "no-such-action".into()); // bad
        let err = m.merge_overrides(&file).unwrap_err();
        match err {
            MergeError::Action { ref raw, .. } => assert_eq!(raw, "no-such-action"),
            other => panic!("expected Action error, got {other:?}"),
        }
        // Keymap unchanged — the `r` override above must NOT have been
        // applied because the `R` entry was bad.
        assert_eq!(
            format!("{:?}", m.binds),
            snapshot,
            "merge_overrides must roll back on any error"
        );
    }

    #[test]
    fn merge_overrides_rolls_back_on_unparseable_chord() {
        let mut m = Keymap::defaults();
        let snapshot = format!("{:?}", m.binds);

        let mut file = KeybindingsFile::default();
        file.leader.insert("<NoEnd".into(), "cancel-query".into());
        let err = m.merge_overrides(&file).unwrap_err();
        assert!(matches!(err, MergeError::Chord { .. }));
        assert_eq!(format!("{:?}", m.binds), snapshot);
    }

    #[test]
    fn empty_file_leaves_defaults_intact() {
        let mut m = Keymap::defaults();
        let before = format!("{:?}", m.binds);
        let file = KeybindingsFile::default();
        m.merge_overrides(&file).unwrap();
        assert_eq!(before, format!("{:?}", m.binds));
    }

    #[test]
    fn context_keys_match_keybindings_file_field_names() {
        // Sanity: the on-disk TOML table names line up with the
        // `KeybindingsFile` struct fields. This is a guard against
        // accidental drift.
        let f: KeybindingsFile = toml::from_str(
            r#"
[global_immediate]
[leader]
[schema]
[result]
[chat_normal]
[chat_insert]
"#,
        )
        .unwrap();
        let _ = f; // round-trip via serde guarantees the keys parse.
        let mut keys: BTreeMap<&'static str, ()> = BTreeMap::new();
        for ctx in Context::ALL {
            keys.insert(ctx.as_key(), ());
        }
        assert!(keys.contains_key("global_immediate"));
        assert!(keys.contains_key("leader"));
        assert!(keys.contains_key("schema"));
        assert!(keys.contains_key("result"));
        assert!(keys.contains_key("chat_normal"));
        assert!(keys.contains_key("chat_insert"));
    }
}
