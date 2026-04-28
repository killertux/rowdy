//! Chord notation parser.
//!
//! Accepted syntax (matches the README's keymap notation):
//!
//! - Bare characters:        `r`, `R`, `0`, `$`, `:`, `=`
//! - Named keys in `<…>`:    `<Esc>`, `<Enter>`, `<Tab>`, `<Up>`,
//!   `<Down>`, `<Left>`, `<Right>`, `<Home>`, `<End>`, `<PageUp>`,
//!   `<PageDown>`, `<Space>`, `<BackTab>`, `<Backspace>`
//! - Modifier prefixes:      `<C-x>`, `<S-r>`, `<C-S-r>`, `<C-Space>`
//! - Two-step sequences:     `gg`, `<Space>r`, `<C-w>l`
//!
//! A [`Chord`] is up to two [`KeyChord`]s. Three-key sequences are not
//! supported in this iteration; rebinding chord-arming keys
//! (`<Space>`, `<C-w>`, `g`) is structurally disallowed by `Context`.

use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyChord {
    #[allow(dead_code)] // used by tests; kept on the public API for symmetry.
    pub const fn bare(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::NONE,
        }
    }
}

/// One- or two-step chord. Stored as a Vec for ergonomics; bounded to
/// length 2 by the parser.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord(pub Vec<KeyChord>);

impl Chord {
    pub fn single(c: KeyChord) -> Self {
        Self(vec![c])
    }

    #[cfg(test)]
    pub fn pair(a: KeyChord, b: KeyChord) -> Self {
        Self(vec![a, b])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordParseError {
    Empty,
    UnknownNamedKey(String),
    UnknownModifier(char),
    UnmatchedAngle,
    TooManySteps,
    InvalidEscape,
}

impl std::fmt::Display for ChordParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty chord"),
            Self::UnknownNamedKey(s) => write!(f, "unknown named key: <{s}>"),
            Self::UnknownModifier(c) => write!(f, "unknown modifier: {c}"),
            Self::UnmatchedAngle => write!(f, "unmatched `<` in chord"),
            Self::TooManySteps => write!(f, "chord exceeds 2 steps"),
            Self::InvalidEscape => write!(f, "invalid chord escape"),
        }
    }
}

/// Parse a chord string into a [`Chord`]. Empty / malformed inputs
/// return a descriptive [`ChordParseError`].
pub fn parse(s: &str) -> Result<Chord, ChordParseError> {
    if s.is_empty() {
        return Err(ChordParseError::Empty);
    }
    let mut steps: Vec<KeyChord> = Vec::with_capacity(2);
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '<' {
            // Treat `<` as a named-key opener when there's a matching
            // `>` later in the input. The bare `<` schema-grow chord
            // is the *single-character* case; `<` followed by content
            // with no closer is an error (looks like an attempted
            // named key the user forgot to close).
            if let Some(close_offset) = bytes[i + 1..].iter().position(|&ch| ch == '>') {
                let inner: String = bytes[i + 1..i + 1 + close_offset].iter().collect();
                steps.push(parse_named(&inner)?);
                i += close_offset + 2;
            } else if i + 1 == bytes.len() {
                // Lone `<` at end of string — bare schema-grow chord.
                steps.push(parse_bare_char(c));
                i += 1;
            } else {
                return Err(ChordParseError::UnmatchedAngle);
            }
        } else {
            steps.push(parse_bare_char(c));
            i += 1;
        }
        if steps.len() > 2 {
            return Err(ChordParseError::TooManySteps);
        }
    }
    if steps.is_empty() {
        return Err(ChordParseError::Empty);
    }
    Ok(Chord(steps))
}

fn parse_bare_char(c: char) -> KeyChord {
    // Uppercase letters carry no implicit SHIFT modifier — crossterm
    // typically reports `(Char('R'), NONE)` for a Shift+r press, and
    // the original `event::translate_*` matched on `Char('G')` with
    // any modifiers. Users wanting an explicit Shift need to spell it
    // as `<S-r>`. Same convention as Vim's keymap notation.
    KeyChord {
        code: KeyCode::Char(c),
        mods: KeyModifiers::NONE,
    }
}

fn parse_named(inner: &str) -> Result<KeyChord, ChordParseError> {
    if inner.is_empty() {
        return Err(ChordParseError::InvalidEscape);
    }
    // Modifier prefixes: `C-`, `S-`, `A-`, repeatable. The split-at-`-`
    // approach is unambiguous because the inner-most token is always
    // either a single char or a named key (never starts with `-`).
    let mut mods = KeyModifiers::NONE;
    let mut rest = inner;
    loop {
        match rest.as_bytes() {
            [b'C', b'-', ..] => {
                mods |= KeyModifiers::CONTROL;
                rest = &rest[2..];
            }
            [b'S', b'-', ..] => {
                mods |= KeyModifiers::SHIFT;
                rest = &rest[2..];
            }
            [b'A', b'-', ..] => {
                mods |= KeyModifiers::ALT;
                rest = &rest[2..];
            }
            // Reject other ASCII modifier-shapes; let the named-key
            // resolver handle the remainder.
            [_, b'-', ..] if rest.len() >= 3 && !rest.starts_with(char::is_alphabetic) => {
                return Err(ChordParseError::UnknownModifier(rest.chars().next().unwrap()));
            }
            _ => break,
        }
    }

    let code = match rest {
        "Esc" => KeyCode::Esc,
        "Enter" | "Return" => KeyCode::Enter,
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Backspace" | "BS" => KeyCode::Backspace,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Space" => KeyCode::Char(' '),
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap();
            // Named-form chars don't auto-add SHIFT for uppercase — the
            // user is in explicit-modifier territory inside `<…>`.
            KeyCode::Char(c)
        }
        other => return Err(ChordParseError::UnknownNamedKey(other.to_string())),
    };
    Ok(KeyChord { code, mods })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(c: char) -> KeyChord {
        KeyChord::bare(KeyCode::Char(c))
    }

    fn shift(c: char) -> KeyChord {
        KeyChord {
            code: KeyCode::Char(c),
            mods: KeyModifiers::SHIFT,
        }
    }

    fn ctrl(c: char) -> KeyChord {
        KeyChord {
            code: KeyCode::Char(c),
            mods: KeyModifiers::CONTROL,
        }
    }

    #[test]
    fn parse_bare_chars() {
        assert_eq!(parse("r").unwrap(), Chord::single(bare('r')));
        assert_eq!(parse(":").unwrap(), Chord::single(bare(':')));
        assert_eq!(parse("0").unwrap(), Chord::single(bare('0')));
        assert_eq!(parse("$").unwrap(), Chord::single(bare('$')));
    }

    #[test]
    fn parse_uppercase_no_implicit_shift() {
        // Crossterm reports Shift+r as `(Char('R'), NONE)` on most
        // terminals; the chord parser follows the same convention.
        // Use `<S-r>` for explicit modifier.
        assert_eq!(parse("R").unwrap(), Chord::single(bare('R')));
        assert_eq!(parse("G").unwrap(), Chord::single(bare('G')));
    }

    #[test]
    fn parse_named_keys() {
        assert_eq!(
            parse("<Esc>").unwrap(),
            Chord::single(KeyChord::bare(KeyCode::Esc))
        );
        assert_eq!(
            parse("<Enter>").unwrap(),
            Chord::single(KeyChord::bare(KeyCode::Enter))
        );
        assert_eq!(
            parse("<Space>").unwrap(),
            Chord::single(KeyChord::bare(KeyCode::Char(' ')))
        );
        assert_eq!(
            parse("<Up>").unwrap(),
            Chord::single(KeyChord::bare(KeyCode::Up))
        );
    }

    #[test]
    fn parse_modifier_prefixes() {
        assert_eq!(parse("<C-x>").unwrap(), Chord::single(ctrl('x')));
        assert_eq!(parse("<S-r>").unwrap(), Chord::single(shift('r')));
        assert_eq!(
            parse("<C-S-r>").unwrap(),
            Chord::single(KeyChord {
                code: KeyCode::Char('r'),
                mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            })
        );
        assert_eq!(
            parse("<C-Space>").unwrap(),
            Chord::single(KeyChord {
                code: KeyCode::Char(' '),
                mods: KeyModifiers::CONTROL,
            })
        );
    }

    #[test]
    fn parse_two_step_sequences() {
        assert_eq!(parse("gg").unwrap(), Chord::pair(bare('g'), bare('g')));
        assert_eq!(
            parse("<Space>r").unwrap(),
            Chord::pair(KeyChord::bare(KeyCode::Char(' ')), bare('r'))
        );
        assert_eq!(parse("<C-w>l").unwrap(), Chord::pair(ctrl('w'), bare('l')));
    }

    #[test]
    fn rejects_three_step_sequences() {
        assert_eq!(parse("ggg").unwrap_err(), ChordParseError::TooManySteps);
    }

    #[test]
    fn rejects_empty_input() {
        assert_eq!(parse("").unwrap_err(), ChordParseError::Empty);
    }

    #[test]
    fn rejects_unmatched_angle() {
        assert_eq!(parse("<Esc").unwrap_err(), ChordParseError::UnmatchedAngle);
    }

    #[test]
    fn rejects_unknown_named_key() {
        match parse("<Wat>").unwrap_err() {
            ChordParseError::UnknownNamedKey(s) => assert_eq!(s, "Wat"),
            other => panic!("expected UnknownNamedKey, got {other:?}"),
        }
    }
}
