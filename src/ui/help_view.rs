//! `:help` popover. Source of truth for what bindings and commands the
//! user can reach. Anyone touching the keymap in `src/event.rs` or the
//! command parser in `src/action.rs::run_command_line` is expected to
//! update `HELP_SECTIONS` below — there are reminder comments at those
//! sites pointing here.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::ui::theme::Theme;

pub struct HelpPopover<'a> {
    pub scroll: u16,
    pub theme: &'a Theme,
}

impl Widget for HelpPopover<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(box_area) = inner_box(area) else {
            return;
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(self.theme.border_focus)
                    .bg(self.theme.bg),
            )
            .title(" rowdy — help ")
            .title_style(
                Style::default()
                    .fg(self.theme.fg)
                    .bg(self.theme.bg)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(self.theme.bg));
        let inner = block.inner(box_area);
        block.render(box_area, buf);

        // Footer takes the bottom row. The body fills the rest.
        let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
        let body_area = chunks[0];
        let footer_area = chunks[1];

        let lines = build_lines(self.theme, key_column_width());
        let total = lines.len() as u16;
        let viewport = body_area.height;
        let max_scroll = total.saturating_sub(viewport);
        let scroll = self.scroll.min(max_scroll);

        Paragraph::new(lines)
            .style(Style::default().fg(self.theme.fg).bg(self.theme.bg))
            .scroll((scroll, 0))
            .render(body_area, buf);

        let footer = Line::from(Span::styled(
            "j/k scroll · Ctrl+d/u half-page · g/G top/bottom · Esc/q close",
            Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
        ));
        Paragraph::new(footer).render(footer_area, buf);
    }
}

/// Centered popover. Fixed cap on width so the two-column layout doesn't
/// stretch into uselessness on a wide terminal; height takes most of the
/// available area so the body has room to scroll naturally.
fn inner_box(area: Rect) -> Option<Rect> {
    let width = area.width.min(80);
    let height = area.height.saturating_sub(2);
    if width < 50 || height < 12 {
        return None;
    }
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Some(Rect {
        x,
        y,
        width,
        height,
    })
}

fn key_column_width() -> usize {
    HELP_SECTIONS
        .iter()
        .flat_map(|s| s.entries.iter())
        .map(|e| e.keys.chars().count())
        .max()
        .unwrap_or(0)
        .max(8)
}

fn build_lines<'a>(theme: &Theme, key_width: usize) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    for (i, section) in HELP_SECTIONS.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(Span::styled(
            section.title.to_string(),
            Style::default()
                .fg(theme.header_fg)
                .bg(theme.bg)
                .add_modifier(Modifier::BOLD),
        )));
        for entry in section.entries {
            // Pad in chars (not bytes) so multi-byte glyphs don't break the
            // column gutter. Most of our key labels are ASCII, but the leader
            // chord uses `<Space>` which is fine; future symbols (e.g. `Ctrl+→`)
            // would otherwise misalign.
            let pad = key_width.saturating_sub(entry.keys.chars().count());
            let keys_padded = format!("{}{}  ", entry.keys, " ".repeat(pad));
            lines.push(Line::from(vec![
                Span::styled(
                    keys_padded,
                    Style::default()
                        .fg(theme.fg)
                        .bg(theme.bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    entry.desc.to_string(),
                    Style::default().fg(theme.fg).bg(theme.bg),
                ),
            ]));
        }
    }
    lines
}

struct HelpEntry {
    keys: &'static str,
    desc: &'static str,
}

struct HelpSection {
    title: &'static str,
    entries: &'static [HelpEntry],
}

/// Single source of truth for the popover. Mirror any change to a binding in
/// `src/event.rs` or a command in `src/action.rs::run_command_line` here so
/// `:help` stays accurate.
const HELP_SECTIONS: &[HelpSection] = &[
    HelpSection {
        title: "Global",
        entries: &[
            HelpEntry {
                keys: ":",
                desc: "Open command prompt",
            },
            HelpEntry {
                keys: "Ctrl+W h / l",
                desc: "Focus editor / schema",
            },
            HelpEntry {
                keys: "Ctrl+W < / >",
                desc: "Grow / shrink schema panel",
            },
            HelpEntry {
                keys: "Ctrl+C",
                desc: "Panic exit (use :q for a clean quit)",
            },
        ],
    },
    HelpSection {
        title: "Editor leader (Space)",
        entries: &[
            HelpEntry {
                keys: "<Space> r",
                desc: "Run statement under cursor (Normal: confirm; Visual: run selection)",
            },
            HelpEntry {
                keys: "<Space> R",
                desc: "Run statement under cursor immediately",
            },
            HelpEntry {
                keys: "<Space> e",
                desc: "Expand the latest result",
            },
            HelpEntry {
                keys: "<Space> c",
                desc: "Cancel the in-flight query",
            },
            HelpEntry {
                keys: "<Space> t",
                desc: "Toggle Dark / Light theme",
            },
        ],
    },
    HelpSection {
        title: "Schema panel",
        entries: &[
            HelpEntry {
                keys: "j / k",
                desc: "Move selection down / up",
            },
            HelpEntry {
                keys: "h",
                desc: "Collapse / move to parent",
            },
            HelpEntry {
                keys: "l",
                desc: "Expand / descend",
            },
            HelpEntry {
                keys: "o, Enter",
                desc: "Toggle expand / collapse",
            },
            HelpEntry {
                keys: "gg / G",
                desc: "Top / bottom",
            },
            HelpEntry {
                keys: "< / >",
                desc: "Grow / shrink the panel width",
            },
        ],
    },
    HelpSection {
        title: "Expanded result view",
        entries: &[
            HelpEntry {
                keys: "h j k l",
                desc: "Move cell cursor",
            },
            HelpEntry {
                keys: "0 / $",
                desc: "First / last column in row",
            },
            HelpEntry {
                keys: "gg / G",
                desc: "First / last row",
            },
            HelpEntry {
                keys: "v",
                desc: "Toggle Visual mode (rectangular cell selection)",
            },
            HelpEntry {
                keys: "y",
                desc: "Yank cell (Normal) / selection (Visual, prompts for format)",
            },
            HelpEntry {
                keys: "q, Esc",
                desc: "Visual: exit Visual · Normal: close expanded view",
            },
        ],
    },
    HelpSection {
        title: "Yank-format prompt (after y in Visual)",
        entries: &[
            HelpEntry {
                keys: "c / t / j",
                desc: "Copy as CSV / TSV / JSON",
            },
            HelpEntry {
                keys: "Esc",
                desc: "Cancel back to Visual",
            },
        ],
    },
    HelpSection {
        title: "Connection list",
        entries: &[
            HelpEntry {
                keys: "j / k",
                desc: "Move selection",
            },
            HelpEntry {
                keys: "g / G",
                desc: "Top / bottom",
            },
            HelpEntry {
                keys: "Enter, u",
                desc: "Switch to the selected connection",
            },
            HelpEntry {
                keys: "a",
                desc: "Add a new connection",
            },
            HelpEntry {
                keys: "e",
                desc: "Edit the selected (form opens, pre-filled)",
            },
            HelpEntry {
                keys: "d",
                desc: "Delete the selected (y/Enter to confirm)",
            },
            HelpEntry {
                keys: "q, Esc",
                desc: "Close",
            },
        ],
    },
    HelpSection {
        title: "Confirm-run prompt",
        entries: &[
            HelpEntry {
                keys: "Enter",
                desc: "Run the highlighted statement",
            },
            HelpEntry {
                keys: "Esc",
                desc: "Cancel",
            },
        ],
    },
    HelpSection {
        title: "Help (this screen)",
        entries: &[
            HelpEntry {
                keys: "j / k",
                desc: "Scroll one line",
            },
            HelpEntry {
                keys: "Ctrl+d / Ctrl+u",
                desc: "Half-page down / up",
            },
            HelpEntry {
                keys: "g / G",
                desc: "Jump to top / bottom",
            },
            HelpEntry {
                keys: "q, Esc",
                desc: "Close",
            },
        ],
    },
    HelpSection {
        title: "Commands (type after :)",
        entries: &[
            HelpEntry {
                keys: ":q, :quit",
                desc: "Quit",
            },
            HelpEntry {
                keys: ":help, :?",
                desc: "Open this help",
            },
            HelpEntry {
                keys: ":run, :r",
                desc: "Run statement under cursor (no confirmation)",
            },
            HelpEntry {
                keys: ":cancel",
                desc: "Cancel the in-flight query",
            },
            HelpEntry {
                keys: ":expand, :e",
                desc: "Expand the latest result",
            },
            HelpEntry {
                keys: ":collapse, :c",
                desc: "Close the expanded result view",
            },
            HelpEntry {
                keys: ":width N",
                desc: "Set schema panel width (12–80)",
            },
            HelpEntry {
                keys: ":theme dark|light|toggle",
                desc: "Switch / toggle theme",
            },
            HelpEntry {
                keys: ":export csv|tsv|json [path]",
                desc: "Export latest result or Visual selection (clipboard, or to path)",
            },
            HelpEntry {
                keys: ":conn, :conn list",
                desc: "Open the connection list",
            },
            HelpEntry {
                keys: ":conn add <name>",
                desc: "Open form to create connection",
            },
            HelpEntry {
                keys: ":conn edit <name>",
                desc: "Edit connection (overwrite on save)",
            },
            HelpEntry {
                keys: ":conn delete <name>",
                desc: "Delete connection",
            },
            HelpEntry {
                keys: ":conn use <name>",
                desc: "Switch active connection",
            },
        ],
    },
];
