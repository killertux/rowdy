use std::collections::HashMap;
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use include_dir::{Dir, include_dir};
use ratatui::style::Color;
use serde::Deserialize;

static THEMES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/themes");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub kind: ThemeKind,
    /// Name of the syntect theme paired with this palette. The string
    /// must match an entry in edtui's bundled theme set (e.g.
    /// `"OneHalfDark"`, `"dracula"`, `"solarized-light"`). Unknown
    /// names cause edtui to skip syntax highlighting silently. Read
    /// once on theme load and leaked to `'static` so `Theme` stays
    /// `Copy` — the registry lives behind a `OnceLock` so the leak is
    /// bounded.
    pub syntect_theme_name: &'static str,
    pub bg: Color,
    pub fg: Color,
    pub fg_dim: Color,
    pub border: Color,
    pub border_focus: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub cursor_bg: Color,
    pub cursor_fg: Color,
    pub status_idle: Color,
    pub status_running: Color,
    pub status_ok: Color,
    pub status_error: Color,
    pub header_fg: Color,
}

impl Theme {
    /// Canonical theme for a kind. Prefers the bundled `dark.toml` /
    /// `light.toml` so the result is deterministic regardless of how
    /// many extra theme files ship in `themes/`. Falls back to any
    /// matching kind if the canonical file is missing. Panics if no
    /// theme matches — at least one `dark` and one `light` theme must
    /// ship with the binary.
    pub fn for_kind(kind: ThemeKind) -> Self {
        let canonical = match kind {
            ThemeKind::Dark => "dark",
            ThemeKind::Light => "light",
        };
        Self::by_name(canonical)
            .or_else(|| themes().values().find(|t| t.kind == kind).copied())
            .unwrap_or_else(|| panic!("no bundled theme for kind {kind:?}"))
    }

    /// Look up a theme by file stem (e.g. `"dark"` → `themes/dark.toml`).
    /// Returns `None` if no `<name>.toml` was bundled.
    pub fn by_name(name: &str) -> Option<Self> {
        themes().get(name).copied()
    }
}

/// All bundled themes sorted alphabetically by name. Returned as
/// owned name strings so callers can mutate / store without holding
/// the registry lock.
pub fn all_themes_sorted() -> Vec<(String, Theme)> {
    let mut out: Vec<(String, Theme)> = themes().iter().map(|(k, v)| (k.clone(), *v)).collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

impl Default for Theme {
    fn default() -> Self {
        Self::for_kind(ThemeKind::Dark)
    }
}

#[derive(Debug, Deserialize)]
struct ThemeFile {
    kind: String,
    syntect_theme_name: String,
    bg: String,
    fg: String,
    fg_dim: String,
    border: String,
    border_focus: String,
    selection_bg: String,
    selection_fg: String,
    cursor_bg: String,
    cursor_fg: String,
    status_idle: String,
    status_running: String,
    status_ok: String,
    status_error: String,
    header_fg: String,
}

fn themes() -> &'static HashMap<String, Theme> {
    static CELL: OnceLock<HashMap<String, Theme>> = OnceLock::new();
    CELL.get_or_init(|| load_themes().expect("failed to load bundled themes"))
}

fn load_themes() -> Result<HashMap<String, Theme>> {
    let mut map = HashMap::new();
    for file in THEMES_DIR.files() {
        let path = file.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("invalid theme file name: {path:?}"))?
            .to_string();
        let contents = file
            .contents_utf8()
            .ok_or_else(|| anyhow!("theme file {path:?} is not utf-8"))?;
        let parsed: ThemeFile =
            toml::from_str(contents).with_context(|| format!("failed to parse theme {path:?}"))?;
        let kind = ThemeKind::parse(&parsed.kind).ok_or_else(|| {
            anyhow!(
                "unknown theme kind {:?} in {path:?} (expected \"dark\" or \"light\")",
                parsed.kind
            )
        })?;
        let theme = Theme {
            kind,
            syntect_theme_name: Box::leak(parsed.syntect_theme_name.into_boxed_str()),
            bg: parse_color(&parsed.bg)?,
            fg: parse_color(&parsed.fg)?,
            fg_dim: parse_color(&parsed.fg_dim)?,
            border: parse_color(&parsed.border)?,
            border_focus: parse_color(&parsed.border_focus)?,
            selection_bg: parse_color(&parsed.selection_bg)?,
            selection_fg: parse_color(&parsed.selection_fg)?,
            cursor_bg: parse_color(&parsed.cursor_bg)?,
            cursor_fg: parse_color(&parsed.cursor_fg)?,
            status_idle: parse_color(&parsed.status_idle)?,
            status_running: parse_color(&parsed.status_running)?,
            status_ok: parse_color(&parsed.status_ok)?,
            status_error: parse_color(&parsed.status_error)?,
            header_fg: parse_color(&parsed.header_fg)?,
        };
        map.insert(name, theme);
    }
    if map.is_empty() {
        bail!("no themes found in bundled themes directory");
    }
    Ok(map)
}

/// Parse a TOML color literal. Accepts `#RRGGBB` / bare `RRGGBB` hex,
/// or a named ANSI / base16 color (`black`, `red`, …, `brightwhite`,
/// `reset`). Named colors map to ratatui's indexed [`Color`] variants
/// — they render against the user's terminal palette, which is the
/// whole point of base16-style themes. Hex always renders true-color.
fn parse_color(s: &str) -> Result<Color> {
    if let Some(named) = parse_named_color(s) {
        return Ok(named);
    }
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() != 6 {
        bail!(
            "invalid color {s:?}: expected 6-digit hex like \"#1E1E2E\" \
             or a named ANSI color (black/red/green/yellow/blue/magenta/cyan/white, \
             plus bright_* / light_* / darkgray / brightwhite / reset)"
        );
    }
    let r = u8::from_str_radix(&hex[0..2], 16).with_context(|| format!("invalid color {s:?}"))?;
    let g = u8::from_str_radix(&hex[2..4], 16).with_context(|| format!("invalid color {s:?}"))?;
    let b = u8::from_str_radix(&hex[4..6], 16).with_context(|| format!("invalid color {s:?}"))?;
    Ok(Color::Rgb(r, g, b))
}

/// Map a TOML string to a ratatui indexed color. Case-insensitive.
/// Convention follows ratatui's enum: `Color::Gray` is ANSI 7
/// (terminal's "white"); `Color::White` is ANSI 15 (bright white). The
/// alias table accepts both shorthand (`darkgray`) and explicit
/// (`bright_black`) spellings so themes ported from other tools just
/// work.
fn parse_named_color(s: &str) -> Option<Color> {
    Some(match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        // ANSI 7 — the terminal palette's "white" (a.k.a. light gray
        // in many themes). Use `brightwhite` for ANSI 15.
        "gray" | "grey" | "white" => Color::Gray,
        "darkgray" | "darkgrey" | "dark_gray" | "dark_grey" | "brightblack" | "bright_black" => {
            Color::DarkGray
        }
        "lightred" | "light_red" | "brightred" | "bright_red" => Color::LightRed,
        "lightgreen" | "light_green" | "brightgreen" | "bright_green" => Color::LightGreen,
        "lightyellow" | "light_yellow" | "brightyellow" | "bright_yellow" => Color::LightYellow,
        "lightblue" | "light_blue" | "brightblue" | "bright_blue" => Color::LightBlue,
        "lightmagenta" | "light_magenta" | "brightmagenta" | "bright_magenta" => {
            Color::LightMagenta
        }
        "lightcyan" | "light_cyan" | "brightcyan" | "bright_cyan" => Color::LightCyan,
        "brightwhite" | "bright_white" => Color::White,
        "reset" | "default" => Color::Reset,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use edtui::SyntaxHighlighter;

    #[test]
    fn syntect_theme_names_resolve_for_sql() {
        // Every bundled palette must point at a syntect theme that
        // edtui can actually load — otherwise the editor falls back
        // to no highlighting silently and the user sees plain text.
        for (name, theme) in themes() {
            assert!(
                SyntaxHighlighter::new(theme.syntect_theme_name, "sql").is_ok(),
                "theme {name:?} → syntect {:?} did not resolve",
                theme.syntect_theme_name
            );
        }
    }

    #[test]
    fn for_kind_returns_matching_theme() {
        let dark = Theme::for_kind(ThemeKind::Dark);
        assert_eq!(dark.kind, ThemeKind::Dark);
        assert_eq!(dark.bg, Color::Rgb(0x1E, 0x1E, 0x2E));
        assert_eq!(dark.header_fg, Color::Rgb(0xF5, 0xC2, 0xE7));

        let light = Theme::for_kind(ThemeKind::Light);
        assert_eq!(light.kind, ThemeKind::Light);
        assert_eq!(light.bg, Color::Rgb(0xEF, 0xF1, 0xF5));
    }

    #[test]
    fn parse_color_accepts_hex() {
        assert_eq!(parse_color("#1E1E2E").unwrap(), Color::Rgb(30, 30, 46));
        assert_eq!(parse_color("1E1E2E").unwrap(), Color::Rgb(30, 30, 46));
    }

    #[test]
    fn parse_color_rejects_bad_input() {
        assert!(parse_color("#zzzzzz").is_err());
        assert!(parse_color("#abc").is_err());
        assert!(parse_color("not_a_color").is_err());
    }

    #[test]
    fn parse_color_accepts_named_ansi() {
        assert_eq!(parse_color("black").unwrap(), Color::Black);
        assert_eq!(parse_color("red").unwrap(), Color::Red);
        assert_eq!(parse_color("Cyan").unwrap(), Color::Cyan);
        assert_eq!(parse_color("YELLOW").unwrap(), Color::Yellow);
        // White vs brightwhite split — ANSI 7 vs ANSI 15.
        assert_eq!(parse_color("white").unwrap(), Color::Gray);
        assert_eq!(parse_color("gray").unwrap(), Color::Gray);
        assert_eq!(parse_color("brightwhite").unwrap(), Color::White);
        assert_eq!(parse_color("bright_white").unwrap(), Color::White);
        // Bright/light aliases collapse to the same Light* variant.
        assert_eq!(parse_color("bright_red").unwrap(), Color::LightRed);
        assert_eq!(parse_color("lightred").unwrap(), Color::LightRed);
        // Dark gray = ANSI 8.
        assert_eq!(parse_color("darkgray").unwrap(), Color::DarkGray);
        assert_eq!(parse_color("bright_black").unwrap(), Color::DarkGray);
        // Reset falls back to the terminal default.
        assert_eq!(parse_color("reset").unwrap(), Color::Reset);
        assert_eq!(parse_color("default").unwrap(), Color::Reset);
    }
}
