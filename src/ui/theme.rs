use ratatui::style::Color;

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

    pub fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub kind: ThemeKind,
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
    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }

    /// Catppuccin-Mocha-inspired dark palette tuned for AAA-ish contrast on standard text.
    fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            bg: rgb(0x1E, 0x1E, 0x2E),
            fg: rgb(0xCD, 0xD6, 0xF4),
            fg_dim: rgb(0x9A, 0xA0, 0xB6),
            border: rgb(0x45, 0x47, 0x5A),
            border_focus: rgb(0x89, 0xB4, 0xFA),
            selection_bg: rgb(0x58, 0x5B, 0x70),
            selection_fg: rgb(0xF5, 0xF7, 0xFA),
            cursor_bg: rgb(0xF5, 0xF7, 0xFA),
            cursor_fg: rgb(0x1E, 0x1E, 0x2E),
            status_idle: rgb(0x9A, 0xA0, 0xB6),
            status_running: rgb(0xF9, 0xE2, 0xAF),
            status_ok: rgb(0xA6, 0xE3, 0xA1),
            status_error: rgb(0xF3, 0x8B, 0xA8),
            header_fg: rgb(0xF5, 0xC2, 0xE7),
        }
    }

    /// Catppuccin-Latte-inspired light palette with darkened accents for crisp contrast.
    fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            bg: rgb(0xEF, 0xF1, 0xF5),
            fg: rgb(0x2C, 0x2F, 0x44),
            fg_dim: rgb(0x6C, 0x6F, 0x85),
            border: rgb(0xBC, 0xC0, 0xCC),
            border_focus: rgb(0x1E, 0x66, 0xF5),
            selection_bg: rgb(0xCC, 0xD0, 0xDA),
            selection_fg: rgb(0x1F, 0x22, 0x36),
            cursor_bg: rgb(0x2C, 0x2F, 0x44),
            cursor_fg: rgb(0xEF, 0xF1, 0xF5),
            status_idle: rgb(0x6C, 0x6F, 0x85),
            status_running: rgb(0xBE, 0x6A, 0x00),
            status_ok: rgb(0x2D, 0x80, 0x1F),
            status_error: rgb(0xC4, 0x0E, 0x33),
            header_fg: rgb(0xC8, 0x44, 0xA9),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::for_kind(ThemeKind::Dark)
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}
