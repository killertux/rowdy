pub mod auth_view;
pub mod autocomplete_popover;
pub mod bottom_bar;
pub mod chat_view;
pub mod conn_form_view;
pub mod conn_list_view;
pub mod editor_view;
pub mod help_view;
pub mod llm_settings_view;
pub mod results_view;
pub mod schema_view;
pub mod theme;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Widget};

use crate::app::App;
use crate::state::layout::OverlayLayout;
use crate::state::overlay::Overlay;
use crate::state::results::{ResultBlock, SelectionRect, fit_columns};
use crate::state::right_panel::RightPanelMode;
use crate::state::screen::Screen;
use auth_view::AuthPrompt;
use autocomplete_popover::CompletionPopover;
use bottom_bar::{BottomBar, COMMAND_PREFIX};
use chat_view::ChatPane;
use conn_form_view::ConnForm;
use conn_list_view::ConnList;
use editor_view::EditorPane;
use help_view::HelpPopover;
use llm_settings_view::LlmSettingsForm;
use results_view::{ExpandedResult, InlineResult};
use schema_view::SchemaPane;

const INLINE_RESULT_HEIGHT: u16 = 10;
const INLINE_PREVIEW_ROWS: usize = 8;

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    app.layout.reset_for_render();
    paint_background(frame, area, app);
    let (main, bottom_area) = split_vertical(area);
    app.layout.bottom_bar = Some(bottom_area);

    // Help is the only overlay that takes over the full screen. The
    // other overlays (Command, ConfirmRun, Connecting) are bottom-bar
    // affordances and let the underlying screen keep rendering.
    if matches!(&app.overlay, Some(Overlay::Help { .. })) {
        render_help(app, frame, area, bottom_area);
        return;
    }
    if matches!(&app.overlay, Some(Overlay::LlmSettings(_))) {
        render_llm_settings(app, frame, area, bottom_area);
        return;
    }

    match &app.screen {
        Screen::Auth(_) | Screen::EditConnection(_) | Screen::ConnectionList(_) => {
            render_modal(app, frame, area, bottom_area);
        }
        Screen::ResultExpanded { .. } => render_expanded(app, frame, main, bottom_area),
        _ => render_workspace(app, frame, main, bottom_area),
    }
}

fn render_llm_settings(app: &mut App, frame: &mut Frame, full: Rect, bottom_area: Rect) {
    if let Some(area) = llm_settings_view::inner_box(full) {
        app.layout.overlay = Some(OverlayLayout::LlmSettings { area });
    }
    let app: &App = app;
    frame.render_widget(BottomBar::new(app), bottom_area);
    if let Some(Overlay::LlmSettings(state)) = &app.overlay {
        let form = LlmSettingsForm {
            state,
            theme: &app.theme,
        };
        frame.render_widget(form, full);
    }
}

fn paint_background(frame: &mut Frame, area: Rect, app: &App) {
    Block::default()
        .style(Style::default().bg(app.theme.bg).fg(app.theme.fg))
        .render(area, frame.buffer_mut());
}

fn render_workspace(app: &mut App, frame: &mut Frame, main: Rect, bottom_area: Rect) {
    let (left, right_area) = split_horizontal(main, app.schema.width);
    let (editor_area, inline_area) = split_editor_area(left, latest_result(app).is_some());
    app.layout.editor = Some(editor_area);

    render_immutable_panes(app, frame, right_area, bottom_area, inline_area);
    frame.render_widget(EditorPane { app }, editor_area);

    // After the editor renders, edtui has populated `cursor_screen_position()`;
    // anchor the popover from there so it tracks the cursor exactly.
    if let Some(state) = app.completion.as_ref()
        && let Some(pos) = app.editor.state.cursor_screen_position()
    {
        frame.render_widget(
            CompletionPopover {
                state,
                theme: &app.theme,
                editor_area,
                cursor_screen_pos: pos,
            },
            editor_area,
        );
    }
}

fn render_expanded(app: &mut App, frame: &mut Frame, main: Rect, bottom_area: Rect) {
    frame.render_widget(BottomBar::new(app), bottom_area);

    let (id, cur, prev_col_offset, prev_row_offset, view) = match app.screen {
        Screen::ResultExpanded {
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
    // -2 reserves the header row and the bottom cell-value badge.
    let visible_rows = (inner_height.saturating_sub(2) as usize).max(1);

    let new_col_offset = clamp_offset(prev_col_offset, cur.col, visible_cols, block.columns.len());
    let new_row_offset = clamp_offset(prev_row_offset, cur.row, visible_rows, block.rows().len());

    let selection = view.anchor().map(|anchor| SelectionRect::new(anchor, cur));

    let expanded_layout = results_view::expanded_layout(
        block,
        main,
        new_col_offset,
        visible_cols,
        new_row_offset,
        visible_rows,
    );
    app.layout.expanded_result = Some(expanded_layout);

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

    if let Screen::ResultExpanded {
        col_offset: cstored,
        row_offset: rstored,
        ..
    } = &mut app.screen
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
    right_area: Rect,
    bottom_area: Rect,
    inline_area: Option<Rect>,
) {
    // Right pane layout depends on which panel is active. Both populate
    // their respective layout-cache slot so the mouse handler can
    // hit-test against whichever is painted.
    match app.right_panel {
        RightPanelMode::Schema => {
            // Schema panel: clamp scroll first so the selected node stays visible.
            let schema_viewport = right_area.height.saturating_sub(2) as usize;
            app.schema.clamp_scroll(schema_viewport);
            let schema_layout = schema_view::layout_for(&app.schema, right_area);
            app.layout.schema = Some(schema_layout);
        }
        RightPanelMode::Chat => {
            let chat_layout = chat_view::layout_for(&app.chat, right_area);
            // Clamp the chat scroll against actual content+viewport
            // before we hand &App to ChatPane. Mirrors the schema
            // panel's pattern so the renderer itself stays read-only.
            let log_w = chat_layout.log_area.width;
            let log_h = chat_layout.log_area.height;
            let content_h = app.chat.content_height(log_w);
            app.chat.clamp_scroll(content_h, log_h);
            app.layout.chat = Some(chat_layout);
        }
    }

    let inline_layout = match (inline_area, latest_result(app)) {
        (Some(area), Some(block)) => Some(results_view::inline_layout(block, area)),
        _ => None,
    };
    app.layout.inline_result = inline_layout;

    let app: &App = app;
    match app.right_panel {
        RightPanelMode::Schema => frame.render_widget(SchemaPane { app }, right_area),
        RightPanelMode::Chat => frame.render_widget(ChatPane { app }, right_area),
    }
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

    // Command overlay's input value rides on top of the bottom bar.
    if let Some(Overlay::Command(buf)) = &app.overlay {
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

/// Help is the only modal that mutates state during render (clamping
/// scroll against the actual content size), so it gets the `&mut App`.
fn render_help(app: &mut App, frame: &mut Frame, full: Rect, bottom_area: Rect) {
    frame.render_widget(BottomBar::new(app), bottom_area);
    if let Some(area) = help_view::inner_box(full) {
        app.layout.overlay = Some(OverlayLayout::Help { area });
    }
    let App { overlay, theme, .. } = &mut *app;
    if let Some(Overlay::Help { scroll, h_scroll }) = overlay {
        let popover = HelpPopover {
            scroll,
            h_scroll,
            theme,
        };
        frame.render_widget(popover, full);
    }
}

fn render_modal(app: &mut App, frame: &mut Frame, full: Rect, bottom_area: Rect) {
    // Capture overlay rect before the borrow flips to &App.
    match &app.screen {
        Screen::Auth(_) => {
            if let Some(area) = auth_view::inner_box(full) {
                app.layout.overlay = Some(OverlayLayout::Auth { area });
            }
        }
        Screen::EditConnection(_) => {
            if let Some(area) = conn_form_view::inner_box(full) {
                app.layout.overlay = Some(OverlayLayout::ConnForm { area });
            }
        }
        Screen::ConnectionList(state) => {
            if let Some(area) = conn_list_view::inner_box(full, state.entries.len()) {
                app.layout.overlay = Some(OverlayLayout::ConnList { area });
            }
        }
        _ => {}
    }

    let app: &App = app;
    frame.render_widget(BottomBar::new(app), bottom_area);
    match &app.screen {
        Screen::Auth(state) => {
            let prompt = AuthPrompt {
                state,
                theme: &app.theme,
            };
            frame.render_widget(prompt, full);
        }
        Screen::EditConnection(state) => {
            let form = ConnForm {
                state,
                theme: &app.theme,
            };
            frame.render_widget(form, full);
        }
        Screen::ConnectionList(state) => {
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
    if app.preview_hidden {
        return None;
    }
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
