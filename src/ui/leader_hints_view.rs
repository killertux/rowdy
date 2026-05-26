//! "which-key" style popover that lists every follow-up binding when
//! the user has armed a chord (`<Space>`, `<C-w>`, `g`). Mirrors the
//! ergonomics of the imbuia editor's leader hint: the popup appears in
//! the bottom-right corner and auto-closes the moment the next
//! keystroke arrives — the chord either fires (popup closes via the
//! normal pending-chord reset) or is silently cancelled.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::keybindings::keymap::{Context, Keymap};
use crate::state::focus::PendingChord;
use crate::ui::theme::Theme;

/// One row in the hint popover.
struct Hint {
    key: String,
    desc: String,
}

pub struct LeaderHints<'a> {
    pub pending: PendingChord,
    pub keymap: &'a Keymap,
    pub theme: &'a Theme,
}

impl Widget for LeaderHints<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (title, hints) = match self.pending {
            PendingChord::Leader => (
                " <Space> ".to_string(),
                collect_keymap(self.keymap, Context::Leader),
            ),
            PendingChord::Window => (" <C-w> ".to_string(), window_hints()),
            // Single-key follow-ups don't benefit from a popover; suppress.
            PendingChord::GG | PendingChord::None => return,
        };
        if hints.is_empty() {
            return;
        }
        let Some(box_area) = inner_box(area, &hints) else {
            return;
        };

        // Clear the cells underneath so the editor doesn't bleed through
        // the popover background (same caveat as the other modals:
        // `Block::style` only sets style, not the glyph).
        Clear.render(box_area, buf);
        for y in box_area.y..box_area.y + box_area.height {
            for x in box_area.x..box_area.x + box_area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(self.theme.bg);
                    cell.set_fg(self.theme.fg);
                }
            }
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(self.theme.border_focus)
                    .bg(self.theme.bg),
            )
            .title(title)
            .title_style(
                Style::default()
                    .fg(self.theme.fg)
                    .bg(self.theme.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(self.theme.bg));
        let inner = block.inner(box_area);
        block.render(box_area, buf);

        let key_col = key_column_width(&hints);
        let lines: Vec<Line> = hints
            .iter()
            .map(|h| hint_line(h, key_col, self.theme))
            .chain(std::iter::once(footer_line(self.theme)))
            .collect();
        Paragraph::new(lines)
            .style(Style::default().fg(self.theme.fg).bg(self.theme.bg))
            .render(inner, buf);
    }
}

fn hint_line<'a>(hint: &Hint, key_col: usize, theme: &Theme) -> Line<'a> {
    let key = format!("{:<width$}", hint.key, width = key_col);
    Line::from(vec![
        Span::styled(
            key,
            Style::default()
                .fg(theme.header_fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ", Style::default().bg(theme.bg)),
        Span::styled(
            hint.desc.clone(),
            Style::default().fg(theme.fg_dim).bg(theme.bg),
        ),
    ])
}

fn footer_line<'a>(theme: &Theme) -> Line<'a> {
    Line::from(Span::styled(
        "<Esc> cancel",
        Style::default().fg(theme.fg_dim).bg(theme.bg),
    ))
}

/// Pull every binding in `ctx` from the keymap, sort by key for stable
/// rendering, and map to display rows.
fn collect_keymap(keymap: &Keymap, ctx: Context) -> Vec<Hint> {
    let mut hints: Vec<Hint> = keymap
        .iter_context(ctx)
        .map(|(chord, action)| Hint {
            key: chord.human(),
            desc: action.description().to_string(),
        })
        // Drop session-switch shortcuts past the first few digits to
        // keep the popover compact; the 0–4 cluster is enough to convey
        // the pattern, and `:session N` still works for the rest.
        .filter(|h| !is_high_session_switch(&h.desc))
        .collect();
    hints.sort_by_key(|h| sort_key(&h.key));
    hints
}

fn is_high_session_switch(desc: &str) -> bool {
    // `BindableAction::description()` returns "Switch directly to session N"
    // for each digit; trim the tail so we keep 0–4 and drop 5–9.
    if let Some(rest) = desc.strip_prefix("Switch directly to session ")
        && let Some(first) = rest.chars().next()
        && first.is_ascii_digit()
    {
        let n: u32 = first.to_digit(10).unwrap_or(0);
        return n >= 5;
    }
    false
}

/// Key-aware sort: lowercase first, then uppercase, then named keys.
/// Sorts case-insensitively then breaks ties so `r`/`R` neighbour.
fn sort_key(k: &str) -> (u8, String, String) {
    let bucket = if k.starts_with('<') {
        2
    } else if k.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
        0
    } else {
        1
    };
    (bucket, k.to_ascii_lowercase(), k.to_string())
}

/// Hardcoded follow-ups for the `<C-w>` chord — these are not wired
/// through the keymap (they live in `event::translate_window_chord`),
/// so the popover reads from a parallel table here. Keep in lockstep.
fn window_hints() -> Vec<Hint> {
    vec![
        Hint {
            key: "h".to_string(),
            desc: "Focus editor".to_string(),
        },
        Hint {
            key: "l".to_string(),
            desc: "Focus right pane (schema / chat)".to_string(),
        },
        Hint {
            key: "<".to_string(),
            desc: "Grow schema panel".to_string(),
        },
        Hint {
            key: ">".to_string(),
            desc: "Shrink schema panel".to_string(),
        },
    ]
}

fn key_column_width(hints: &[Hint]) -> usize {
    hints
        .iter()
        .map(|h| h.key.chars().count())
        .max()
        .unwrap_or(1)
        .max(3)
}

/// Centered bottom-right corner box sized to the entry list. Returns
/// `None` if the terminal is too small for a useful popover (the
/// chord still works — we just suppress the cosmetic hint).
fn inner_box(area: Rect, hints: &[Hint]) -> Option<Rect> {
    let key_col = key_column_width(hints) as u16;
    let desc_col = hints
        .iter()
        .map(|h| h.desc.chars().count() as u16)
        .max()
        .unwrap_or(20);
    // borders (2) + content width + 2-space separator
    let inner_w = key_col + 2 + desc_col;
    let width = (inner_w + 2).clamp(20, 60).min(area.width);
    let needed_rows = hints.len() as u16 + 1; // +1 footer
    let height = (needed_rows + 2).min(area.height); // +2 borders
    if width < 16 || height < 4 {
        return None;
    }
    // Bottom-right, with a 1-cell margin from the edge so the bar
    // underneath stays readable.
    let x = area.x + area.width.saturating_sub(width).saturating_sub(1);
    let y = area.y + area.height.saturating_sub(height).saturating_sub(1);
    Some(Rect {
        x,
        y,
        width,
        height,
    })
}

