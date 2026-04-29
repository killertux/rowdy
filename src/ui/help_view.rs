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

/// Widget takes `&mut u16` for both scroll values so it can clamp the
/// state during render — the next keystroke then sees a value that
/// reflects the actual content size, instead of accumulating past the
/// limit and forcing the user to press the opposite key the same number
/// of times to "undo" the overshoot.
pub struct HelpPopover<'a> {
    pub scroll: &'a mut u16,
    pub h_scroll: &'a mut u16,
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

        let key_w = key_column_width();
        let lines = build_lines(self.theme, key_w);
        let total = lines.len() as u16;
        let max_line_width = max_line_chars(&lines) as u16;
        let viewport_h = body_area.height;
        let viewport_w = body_area.width;
        let max_scroll = total.saturating_sub(viewport_h);
        let max_h_scroll = max_line_width.saturating_sub(viewport_w);
        // Clamp through the &mut so the next keystroke starts from the
        // already-bounded value — no "press k twenty times to undo
        // overshoot" surprise.
        if *self.scroll > max_scroll {
            *self.scroll = max_scroll;
        }
        if *self.h_scroll > max_h_scroll {
            *self.h_scroll = max_h_scroll;
        }

        Paragraph::new(lines)
            .style(Style::default().fg(self.theme.fg).bg(self.theme.bg))
            .scroll((*self.scroll, *self.h_scroll))
            .render(body_area, buf);

        let footer = Line::from(Span::styled(
            "j/k scroll · h/l side-scroll · Ctrl+d/u half-page · g/G · 0/$ · Esc/q close",
            Style::default().fg(self.theme.fg_dim).bg(self.theme.bg),
        ));
        Paragraph::new(footer).render(footer_area, buf);
    }
}

fn max_line_chars(lines: &[Line<'_>]) -> usize {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0)
}

/// Centered popover. Fixed cap on width so the two-column layout doesn't
/// stretch into uselessness on a wide terminal; height takes most of the
/// available area so the body has room to scroll naturally.
pub fn inner_box(area: Rect) -> Option<Rect> {
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
        // Anything in this section fires from Editor (Normal/Visual),
        // Schema, or Chat normal — i.e. every focus except text-input
        // modes (Editor Insert, Chat composer, modal forms). In an
        // insert mode, press Esc first.
        title: "Global (works in any non-insert focus)",
        entries: &[
            HelpEntry {
                keys: ":",
                desc: "Open command prompt",
            },
            HelpEntry {
                keys: "Esc",
                desc: "From Schema or Chat: focus editor (right panel keeps painting)",
            },
            HelpEntry {
                keys: "Ctrl+W h / l",
                desc: "Focus editor / right panel (right panel = schema or chat)",
            },
            HelpEntry {
                keys: "< / >",
                desc: "Grow / shrink schema panel (also Ctrl+W < / >)",
            },
            HelpEntry {
                keys: "Ctrl+C",
                desc: "Panic exit (use :q for a clean quit)",
            },
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
                keys: "Q  (also :close)",
                desc: "Hide the inline result preview (next query un-hides)",
            },
            HelpEntry {
                keys: "<Space> c",
                desc: "Cancel the in-flight query",
            },
            HelpEntry {
                keys: "<Space> t",
                desc: "Toggle Dark / Light theme",
            },
            HelpEntry {
                keys: "<Space> S",
                desc: "Switch right panel to schema (and focus it)",
            },
            HelpEntry {
                keys: "<Space> C",
                desc: "Switch right panel to chat (and focus it)",
            },
            HelpEntry {
                keys: "=",
                desc: "Format the statement under the cursor (or Visual selection)",
            },
            HelpEntry {
                keys: "Ctrl+Space",
                desc: "Open SQL autocomplete (auto-triggers on . or 2+ ident chars)",
            },
        ],
    },
    HelpSection {
        title: "Autocomplete popover",
        entries: &[
            HelpEntry {
                keys: "Up, Ctrl+P",
                desc: "Previous candidate",
            },
            HelpEntry {
                keys: "Down, Ctrl+N",
                desc: "Next candidate",
            },
            HelpEntry {
                keys: "Tab, Enter",
                desc: "Accept the highlighted candidate",
            },
            HelpEntry {
                keys: "Esc",
                desc: "Close + snooze auto-trigger for this word",
            },
        ],
    },
    HelpSection {
        title: "Command bar autocomplete (`:`)",
        entries: &[
            HelpEntry {
                keys: "(any letter)",
                desc: "Live prefix-match against top-level commands",
            },
            HelpEntry {
                keys: "Up / Down / Ctrl+P / Ctrl+N",
                desc: "Move popover selection",
            },
            HelpEntry {
                keys: "Tab",
                desc: "Replace the in-progress command name with the highlighted one",
            },
            HelpEntry {
                keys: "(Space)",
                desc: "Closes the popover — assumed you've committed to the command and are typing args",
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
                keys: "Esc",
                desc: "Focus editor (right panel keeps painting schema)",
            },
        ],
    },
    HelpSection {
        title: "LLM chat — commands",
        entries: &[
            HelpEntry {
                keys: ":chat",
                desc: "Toggle right panel between schema and chat (focus follows)",
            },
            HelpEntry {
                keys: ":chat clear",
                desc: "Wipe the message log",
            },
            HelpEntry {
                keys: ":chat settings",
                desc: "Configure provider, model, and API key",
            },
        ],
    },
    HelpSection {
        title: "LLM chat — normal mode (panel focused, composer dormant)",
        entries: &[
            HelpEntry {
                keys: "i",
                desc: "Enter insert mode (focus the composer to type)",
            },
            HelpEntry {
                keys: "↑ / k / h",
                desc: "Scroll the message log up by one line",
            },
            HelpEntry {
                keys: "↓ / j / l",
                desc: "Scroll the message log down by one line",
            },
            HelpEntry {
                keys: "PgUp / PgDn",
                desc: "Scroll the message log by a page",
            },
            HelpEntry {
                keys: "gg / G",
                desc: "Jump to the top / bottom of the log (G re-engages auto-follow)",
            },
            HelpEntry {
                keys: "Home / End",
                desc: "Jump to the top / bottom of the log",
            },
            HelpEntry {
                keys: "Esc",
                desc: "Focus editor (chat keeps painting on the right)",
            },
            HelpEntry {
                keys: ": / <Space> / Ctrl+W",
                desc: "Globals work — see the Global section above",
            },
        ],
    },
    HelpSection {
        title: "LLM chat — insert mode (composer focused)",
        entries: &[
            HelpEntry {
                keys: "Enter",
                desc: "Submit composer · Shift+Enter inserts a newline",
            },
            HelpEntry {
                keys: "Esc",
                desc: "Drop back to chat normal mode (composer keeps its contents)",
            },
            HelpEntry {
                keys: "Ctrl+U",
                desc: "Clear the composer (message log untouched)",
            },
            HelpEntry {
                keys: "PgUp / PgDn",
                desc: "Scroll the message log by a page",
            },
            HelpEntry {
                keys: "Ctrl+↑ / Ctrl+↓",
                desc: "Scroll the message log line by line",
            },
            HelpEntry {
                keys: "Ctrl+Home / Ctrl+End",
                desc: "Jump to the top / bottom of the log (End re-engages auto-follow)",
            },
        ],
    },
    HelpSection {
        title: "Form fields (any modal: settings, conn-form, auth, : prompt)",
        entries: &[
            HelpEntry {
                keys: "Tab / Shift+Tab",
                desc: "Move between fields",
            },
            HelpEntry {
                keys: "Ctrl+V / Ctrl+C / Ctrl+X",
                desc: "Paste / copy / cut (system clipboard)",
            },
            HelpEntry {
                keys: "Ctrl+U",
                desc: "Clear the focused field",
            },
            HelpEntry {
                keys: "Enter / Esc",
                desc: "Submit / cancel",
            },
            HelpEntry {
                keys: "← → or [ ]",
                desc: ":chat settings — change provider (only on Backend field)",
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
                keys: "H / L",
                desc: "Move focused column left / right (local to this view)",
            },
            HelpEntry {
                keys: "x",
                desc: "Hide focused column (R restores)",
            },
            HelpEntry {
                keys: "R",
                desc: "Reset column order + visibility (un-hide all)",
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
                keys: "s",
                desc: "Copy as SQL INSERTs (table inferred from query)",
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
        title: "Auto-update prompt",
        entries: &[
            HelpEntry {
                keys: "y / Enter",
                desc: "Download and install the new release in place",
            },
            HelpEntry {
                keys: "n / Esc",
                desc: "Dismiss; we won't re-prompt for this version",
            },
            HelpEntry {
                keys: "(opt-out)",
                desc: "Set check_for_updates = false in ~/.rowdy/config.toml",
            },
        ],
    },
    HelpSection {
        title: "Mouse",
        entries: &[
            HelpEntry {
                keys: "Click",
                desc: "Focus pane / select schema row / position cursor",
            },
            HelpEntry {
                keys: "Click chevron",
                desc: "Expand or collapse a schema node",
            },
            HelpEntry {
                keys: "Click cell",
                desc: "Select a cell in the expanded result grid",
            },
            HelpEntry {
                keys: "Click + drag",
                desc: "Multi-select cells (visual mode)",
            },
            HelpEntry {
                keys: "Click inline result",
                desc: "Open expanded view at the clicked cell",
            },
            HelpEntry {
                keys: "Wheel",
                desc: "Scroll schema / results / help by 3 rows",
            },
            HelpEntry {
                keys: "Click outside",
                desc: "Dismiss help / connection picker / command bar",
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
                keys: "h / l",
                desc: "Scroll left / right (for long entries)",
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
                keys: "0 / $",
                desc: "Jump to far left / far right",
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
                keys: ":export sql [table] [path]",
                desc: "Export as INSERT statements (table inferred for simple SELECTs)",
            },
            HelpEntry {
                keys: ":format, :fmt",
                desc: "Format the statement under the cursor (or Visual selection)",
            },
            HelpEntry {
                keys: ":format all",
                desc: "Format the entire editor buffer",
            },
            HelpEntry {
                keys: ":reload",
                desc: "Re-prime the autocomplete schema cache",
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
            HelpEntry {
                keys: ":update",
                desc: "Check GitHub for a new release (manual; bypasses 24h throttle)",
            },
        ],
    },
];
