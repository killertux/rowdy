pub mod bottom_bar;
pub mod editor_view;
pub mod results_view;
pub mod schema_view;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Widget};

use crate::app::App;
use crate::state::focus::Mode;
use crate::state::results::ResultBlock;
use bottom_bar::BottomBar;
use editor_view::EditorPane;
use results_view::{ExpandedResult, InlineResult};
use schema_view::SchemaPane;

const INLINE_RESULT_HEIGHT: u16 = 10;
const INLINE_PREVIEW_ROWS: usize = 8;

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    paint_background(frame, area, app);
    let (main, bottom_area) = split_vertical(area);

    let cursor = match app.mode {
        Mode::ResultExpanded { .. } => render_expanded(app, frame, main, bottom_area),
        _ => render_workspace(app, frame, main, bottom_area),
    };

    if let Some(pos) = cursor {
        frame.set_cursor_position(pos);
    }
}

fn paint_background(frame: &mut Frame, area: Rect, app: &App) {
    Block::default()
        .style(Style::default().bg(app.theme.bg).fg(app.theme.fg))
        .render(area, frame.buffer_mut());
}

fn render_workspace(
    app: &mut App,
    frame: &mut Frame,
    main: Rect,
    bottom_area: Rect,
) -> Option<ratatui::layout::Position> {
    let (left, schema_area) = split_horizontal(main, app.schema.width);
    let (editor_area, inline_area) = split_editor_area(left, latest_result(app).is_some());

    let cursor = render_immutable_panes(app, frame, schema_area, bottom_area, inline_area);
    frame.render_widget(EditorPane { app }, editor_area);
    cursor
}

fn render_expanded(
    app: &App,
    frame: &mut Frame,
    main: Rect,
    bottom_area: Rect,
) -> Option<ratatui::layout::Position> {
    let bar = BottomBar::new(app);
    let cursor = bar.cursor_position(bottom_area);
    frame.render_widget(bar, bottom_area);

    let Mode::ResultExpanded { id, cursor: cur } = app.mode else {
        return cursor;
    };
    if let Some(block) = app.results.iter().find(|b| b.id == id) {
        frame.render_widget(
            ExpandedResult {
                block,
                cursor: cur,
                theme: &app.theme,
            },
            main,
        );
    }
    cursor
}

fn render_immutable_panes(
    app: &App,
    frame: &mut Frame,
    schema_area: Rect,
    bottom_area: Rect,
    inline_area: Option<Rect>,
) -> Option<ratatui::layout::Position> {
    frame.render_widget(SchemaPane { app }, schema_area);
    if let (Some(area), Some(block)) = (inline_area, latest_result(app)) {
        frame.render_widget(
            InlineResult {
                block,
                max_preview_rows: INLINE_PREVIEW_ROWS,
                theme: &app.theme,
            },
            area,
        );
    }
    let bar = BottomBar::new(app);
    let cursor = bar.cursor_position(bottom_area);
    frame.render_widget(bar, bottom_area);
    cursor
}

fn latest_result(app: &App) -> Option<&ResultBlock> {
    app.results.last()
}

fn split_vertical(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    (chunks[0], chunks[1])
}

fn split_horizontal(area: Rect, schema_width: u16) -> (Rect, Rect) {
    let chunks =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(schema_width)]).split(area);
    (chunks[0], chunks[1])
}

fn split_editor_area(area: Rect, has_result: bool) -> (Rect, Option<Rect>) {
    if !has_result || area.height < INLINE_RESULT_HEIGHT + 4 {
        return (area, None);
    }
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(INLINE_RESULT_HEIGHT)])
        .split(area);
    (chunks[0], Some(chunks[1]))
}
