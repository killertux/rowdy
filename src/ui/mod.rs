pub mod auth_view;
pub mod bottom_bar;
pub mod conn_form_view;
pub mod conn_list_view;
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
use crate::state::results::{ResultBlock, SelectionRect, fit_columns};
use auth_view::AuthPrompt;
use bottom_bar::{BottomBar, COMMAND_PREFIX};
use conn_form_view::ConnForm;
use conn_list_view::ConnList;
use editor_view::EditorPane;
use results_view::{ExpandedResult, InlineResult};
use schema_view::SchemaPane;

const INLINE_RESULT_HEIGHT: u16 = 10;
const INLINE_PREVIEW_ROWS: usize = 8;

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    paint_background(frame, area, app);
    let (main, bottom_area) = split_vertical(area);

    match &app.mode {
        Mode::Auth(_) | Mode::EditConnection(_) | Mode::ConnectionList(_) => {
            render_modal(app, frame, area, bottom_area);
        }
        Mode::ResultExpanded { .. } => render_expanded(app, frame, main, bottom_area),
        _ => render_workspace(app, frame, main, bottom_area),
    }
}

fn paint_background(frame: &mut Frame, area: Rect, app: &App) {
    Block::default()
        .style(Style::default().bg(app.theme.bg).fg(app.theme.fg))
        .render(area, frame.buffer_mut());
}

fn render_workspace(app: &mut App, frame: &mut Frame, main: Rect, bottom_area: Rect) {
    let (left, schema_area) = split_horizontal(main, app.schema.width);
    let (editor_area, inline_area) = split_editor_area(left, latest_result(app).is_some());

    render_immutable_panes(app, frame, schema_area, bottom_area, inline_area);
    frame.render_widget(EditorPane { app }, editor_area);
}

fn render_expanded(app: &mut App, frame: &mut Frame, main: Rect, bottom_area: Rect) {
    frame.render_widget(BottomBar::new(app), bottom_area);

    let (id, cur, prev_col_offset, prev_row_offset, view) = match app.mode {
        Mode::ResultExpanded {
            id,
            cursor,
            col_offset,
            row_offset,
            view,
        } => (id, cursor, col_offset, row_offset, view),
        _ => return,
    };

    let Some(block) = app.results.iter().find(|b| b.id == id) else {
        return;
    };

    let inner_width = main.width.saturating_sub(2);
    let inner_height = main.height.saturating_sub(2);
    let visible_cols = fit_columns(inner_width).min(block.columns.len().max(1));
    let visible_rows = (inner_height.saturating_sub(1) as usize).max(1);

    let new_col_offset = clamp_offset(prev_col_offset, cur.col, visible_cols, block.columns.len());
    let new_row_offset = clamp_offset(prev_row_offset, cur.row, visible_rows, block.rows().len());

    let selection = view.anchor().map(|anchor| SelectionRect::new(anchor, cur));

    frame.render_widget(
        ExpandedResult {
            block,
            cursor: cur,
            col_offset: new_col_offset,
            visible_cols,
            row_offset: new_row_offset,
            visible_rows,
            theme: &app.theme,
            selection,
        },
        main,
    );

    if let Mode::ResultExpanded {
        col_offset: cstored,
        row_offset: rstored,
        ..
    } = &mut app.mode
    {
        *cstored = new_col_offset;
        *rstored = new_row_offset;
    }
}

/// Slide a 1-D viewport so it both stays inside `[0, total)` and contains
/// `cursor`. Used for the expanded-result row and column scroll.
fn clamp_offset(prev: usize, cursor: usize, viewport: usize, total: usize) -> usize {
    let view = viewport.max(1);
    let max_offset = total.saturating_sub(view);
    let mut next = prev.min(max_offset);
    if cursor < next {
        next = cursor;
    } else if cursor >= next + view {
        next = cursor + 1 - view;
    }
    next
}

fn render_immutable_panes(
    app: &mut App,
    frame: &mut Frame,
    schema_area: Rect,
    bottom_area: Rect,
    inline_area: Option<Rect>,
) {
    // Schema panel: clamp scroll first so the selected node stays visible.
    let schema_viewport = schema_area.height.saturating_sub(2) as usize;
    app.schema.clamp_scroll(schema_viewport);

    let app: &App = app;
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
    frame.render_widget(BottomBar::new(app), bottom_area);

    // Command-mode input value rides on top of the bar.
    if let Mode::Command(buf) = &app.mode {
        let input_area = command_input_area(bottom_area);
        frame.render_widget(&buf.input, input_area);
    }
}

fn command_input_area(bottom_area: Rect) -> Rect {
    let prefix = COMMAND_PREFIX.chars().count() as u16;
    Rect {
        x: bottom_area.x + prefix,
        y: bottom_area.y,
        width: bottom_area.width.saturating_sub(prefix),
        height: 1,
    }
}

fn render_modal(app: &mut App, frame: &mut Frame, full: Rect, bottom_area: Rect) {
    let app: &App = app;
    frame.render_widget(BottomBar::new(app), bottom_area);
    match &app.mode {
        Mode::Auth(state) => {
            let prompt = AuthPrompt {
                state,
                theme: &app.theme,
            };
            frame.render_widget(prompt, full);
        }
        Mode::EditConnection(state) => {
            let form = ConnForm {
                state,
                theme: &app.theme,
            };
            frame.render_widget(form, full);
        }
        Mode::ConnectionList(state) => {
            let list = ConnList {
                state,
                active: app.active_connection.as_deref(),
                theme: &app.theme,
            };
            frame.render_widget(list, full);
        }
        _ => {}
    }
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
