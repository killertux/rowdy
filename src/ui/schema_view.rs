use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::app::App;
use crate::state::focus::Focus;
use crate::state::schema::{LoadState, NodeKind, SchemaPanel, VisibleRow};
use crate::ui::theme::Theme;

pub struct SchemaPane<'a> {
    pub app: &'a App,
}

impl Widget for SchemaPane<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = &self.app.theme;
        let focused = self.app.focus == Focus::Schema;
        let block = themed_block(theme, focused);

        let lines: Vec<Line> = build_lines(&self.app.schema, theme);

        Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(theme.fg).bg(theme.bg))
            .render(area, buf);
    }
}

fn build_lines(schema: &SchemaPanel, theme: &Theme) -> Vec<Line<'static>> {
    if let Some(line) = root_placeholder_line(schema, theme) {
        return vec![line];
    }
    schema
        .visible_rows()
        .iter()
        .map(|row| render_row(schema, *row, theme))
        .collect()
}

fn root_placeholder_line(schema: &SchemaPanel, theme: &Theme) -> Option<Line<'static>> {
    if !schema.roots.is_empty() {
        return None;
    }
    let (text, color) = match &schema.root_load_state {
        LoadState::NotLoaded => ("(idle)".to_string(), theme.fg_dim),
        LoadState::Loading => ("loading catalogs…".to_string(), theme.status_running),
        LoadState::Loaded => ("(no catalogs)".to_string(), theme.fg_dim),
        LoadState::Failed(err) => (format!("failed: {err}"), theme.status_error),
    };
    Some(Line::from(Span::styled(
        text,
        Style::default().fg(color).bg(theme.bg),
    )))
}

fn render_row(schema: &SchemaPanel, row: VisibleRow, theme: &Theme) -> Line<'static> {
    let node = schema.node(row.id);
    let indent = "  ".repeat(row.depth);
    let glyph = node_glyph(node.kind, node.expanded, !node.children.is_empty());
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);

    let label_text = format!("{indent}{glyph} {label}", label = node.label);
    let label_style = label_style(schema, row, theme);
    spans.push(Span::styled(label_text, label_style));

    if let Some((suffix_text, suffix_color)) = load_state_suffix(&node.load_state, theme) {
        spans.push(Span::styled(
            format!(" {suffix_text}"),
            Style::default().fg(suffix_color).bg(theme.bg),
        ));
    }

    Line::from(spans)
}

fn label_style(schema: &SchemaPanel, row: VisibleRow, theme: &Theme) -> Style {
    if Some(row.id) == schema.selected {
        Style::default()
            .fg(theme.selection_fg)
            .bg(theme.selection_bg)
    } else {
        let node = schema.node(row.id);
        let mut base = Style::default().fg(theme.fg).bg(theme.bg);
        if node.load_state.is_failed() {
            base = base.fg(theme.status_error);
        }
        if matches!(node.kind, NodeKind::Folder) {
            base = base.add_modifier(Modifier::ITALIC);
        }
        base
    }
}

fn load_state_suffix(state: &LoadState, theme: &Theme) -> Option<(String, ratatui::style::Color)> {
    match state {
        LoadState::Loading => Some(("(loading…)".to_string(), theme.status_running)),
        LoadState::Failed(err) => Some((format!("(error: {err})"), theme.status_error)),
        _ => None,
    }
}

fn node_glyph(kind: NodeKind, expanded: bool, has_children: bool) -> &'static str {
    if has_children
        || matches!(
            kind,
            NodeKind::Catalog
                | NodeKind::Schema
                | NodeKind::Table
                | NodeKind::View
                | NodeKind::Folder
        )
    {
        if expanded { "▾" } else { "▸" }
    } else {
        match kind {
            NodeKind::Column => "•",
            NodeKind::Index => "◆",
            _ => " ",
        }
    }
}

fn themed_block<'a>(theme: &Theme, focused: bool) -> Block<'a> {
    let border = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border).bg(theme.bg))
        .title(" schema ")
        .title_style(Style::default().fg(theme.fg).bg(theme.bg))
        .style(Style::default().bg(theme.bg))
}
