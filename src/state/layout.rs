//! Render-time layout cache.
//!
//! ratatui recomputes panel rects every frame, so the keyboard-only event
//! pipeline never needed to know where anything was on screen. Mouse events
//! arrive with `(column, row)` coordinates, so we now do — for hit-testing.
//!
//! Each render pass populates this cache as a side-effect of laying out
//! panels. The next `CtEvent::Mouse` reads it to decide which panel was
//! clicked, then routes to a panel-specific handler.
//!
//! The cache is *render output*, not editing state — clearing it on each
//! render is fine, and consumers must tolerate `None` for any field that
//! the latest frame didn't paint (e.g. inline result vanishes after a
//! `:cancel`).

use ratatui::layout::Rect;

use crate::state::schema::NodeId;

#[derive(Debug, Default)]
pub struct LayoutCache {
    pub schema: Option<SchemaLayout>,
    pub chat: Option<ChatLayout>,
    pub editor: Option<Rect>,
    pub inline_result: Option<TableLayout>,
    pub expanded_result: Option<TableLayout>,
    pub bottom_bar: Option<Rect>,
    pub overlay: Option<OverlayLayout>,
    /// Active drag, if any. Set on `MouseDown` over a draggable surface,
    /// cleared on `MouseUp` (or when focus shifts via something else).
    pub drag: Option<DragState>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // log_area / composer_area are used in phase 3+ for click routing.
pub struct ChatLayout {
    /// Outer rect (including the border).
    pub area: Rect,
    /// Where messages paint. Mouse-wheel events inside this rect scroll
    /// the log; clicks are reserved for future "click message to copy".
    pub log_area: Rect,
    /// Where the composer's TextArea paints. Clicks here focus the
    /// composer (i.e. flip `app.focus` to `Chat`).
    pub composer_area: Rect,
}

#[derive(Debug)]
pub struct SchemaLayout {
    pub area: Rect,
    /// `area` minus the border (where the rows actually paint). Hit-testing
    /// happens against this.
    pub rows_area: Rect,
    /// Map from on-screen row index (0-based, top of `rows_area`) to the
    /// `NodeId` painted there. `len()` is the number of rows actually drawn.
    pub rows: Vec<NodeId>,
}

#[derive(Debug, Clone)]
pub struct TableLayout {
    /// Outer rect (including the border).
    pub area: Rect,
    /// First Y of the body (where row 0 of the visible window paints).
    pub body_top_y: u16,
    /// Number of body rows the renderer drew. Useful for clamping clicks
    /// past the last row.
    pub body_rows: u16,
    /// Cumulative X coordinates of column boundaries, in the outer rect's
    /// coord space. `col_x[i]` is the leftmost X of visible column `i`;
    /// `col_x[i+1]` is the leftmost X of the next column (or the right edge
    /// of the visible area for the last entry).
    pub col_x: Vec<u16>,
    /// Absolute index of the leftmost visible column (`col_x[0]` corresponds
    /// to data column `col_offset`).
    pub col_offset: usize,
    /// Absolute index of the topmost visible row (`body_top_y` corresponds
    /// to data row `row_offset`).
    pub row_offset: usize,
}

impl TableLayout {
    /// Hit-test a click at `(x, y)` against this table. Returns the absolute
    /// `(row, col)` of the cell under the cursor, or `None` if the click is
    /// outside the body or past the last drawn row/column.
    pub fn cell_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        if y < self.body_top_y || y >= self.body_top_y + self.body_rows {
            return None;
        }
        if self.col_x.is_empty() {
            return None;
        }
        let col_local = self
            .col_x
            .windows(2)
            .position(|w| x >= w[0] && x < w[1])
            .or_else(|| {
                // Past the last separator but still within the right edge of
                // the last column — `col_x` only stores boundaries up to the
                // last column's start; treat anything to its right as a hit
                // on the last visible column.
                let last_start = *self.col_x.last()?;
                if x >= last_start {
                    Some(self.col_x.len() - 1)
                } else {
                    None
                }
            })?;
        let absolute_col = self.col_offset + col_local;
        let absolute_row = self.row_offset + (y - self.body_top_y) as usize;
        Some((absolute_row, absolute_col))
    }
}

#[derive(Debug)]
pub enum OverlayLayout {
    /// Centered popover; click outside closes.
    Help { area: Rect },
    /// Centered modal connection list.
    ConnList { area: Rect },
    /// Connection edit/create form.
    ConnForm { area: Rect },
    /// Auth prompt (password).
    Auth { area: Rect },
}

impl OverlayLayout {
    pub fn area(&self) -> Rect {
        match self {
            Self::Help { area }
            | Self::ConnList { area }
            | Self::ConnForm { area }
            | Self::Auth { area } => *area,
        }
    }
}

/// What the user is currently dragging on. Set on left-button-down over a
/// supported surface and consumed by subsequent `Drag(Left)` events.
#[derive(Debug, Clone, Copy)]
pub enum DragState {
    /// Drag-extending a result-grid selection. Anchor recorded on mouse down.
    ResultSelect,
}

impl LayoutCache {
    /// Wipe every per-frame field. Call at the start of `ui::render` so a
    /// panel that vanished this frame can't haunt the next mouse event.
    /// `drag` survives across frames — it's lifecycle-managed by the mouse
    /// handler, not by the renderer.
    pub fn reset_for_render(&mut self) {
        self.schema = None;
        self.chat = None;
        self.editor = None;
        self.inline_result = None;
        self.expanded_result = None;
        self.bottom_bar = None;
        self.overlay = None;
    }
}

/// Whether `(x, y)` falls inside `r`. ratatui's `Rect::contains_point` exists
/// but takes a `Position`; this is a touch more convenient at call sites.
pub fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x.saturating_add(r.width) && y >= r.y && y < r.y.saturating_add(r.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn rect_contains_handles_edges() {
        let r = rect(2, 3, 4, 5);
        // Inside.
        assert!(rect_contains(r, 2, 3));
        assert!(rect_contains(r, 5, 7));
        // Just past the right/bottom edges (exclusive).
        assert!(!rect_contains(r, 6, 3));
        assert!(!rect_contains(r, 2, 8));
        // Way outside.
        assert!(!rect_contains(r, 0, 0));
    }

    #[test]
    fn table_cell_at_maps_x_to_column() {
        // 3 columns starting at x=10, each ~6 wide.
        let layout = TableLayout {
            area: rect(10, 0, 18, 10),
            body_top_y: 1,
            body_rows: 5,
            col_x: vec![10, 16, 22, 28],
            col_offset: 0,
            row_offset: 0,
        };
        assert_eq!(layout.cell_at(10, 1), Some((0, 0)));
        assert_eq!(layout.cell_at(15, 1), Some((0, 0)));
        assert_eq!(layout.cell_at(16, 1), Some((0, 1)));
        assert_eq!(layout.cell_at(22, 1), Some((0, 2)));
        // Past the last separator but still inside `col_x.last()` semantics:
        // col_x ends at 28 (the right edge), so 27 is still in col 2.
        assert_eq!(layout.cell_at(27, 1), Some((0, 2)));
        // Outside the body.
        assert_eq!(layout.cell_at(15, 0), None);
        assert_eq!(layout.cell_at(15, 6), None);
        // Y-row mapping with offsets.
        assert_eq!(layout.cell_at(15, 3), Some((2, 0)));
    }

    #[test]
    fn table_cell_at_respects_offsets() {
        let layout = TableLayout {
            area: rect(0, 0, 18, 10),
            body_top_y: 1,
            body_rows: 3,
            col_x: vec![0, 6, 12, 18],
            col_offset: 5,
            row_offset: 100,
        };
        // Top-left visible cell maps to (100, 5).
        assert_eq!(layout.cell_at(0, 1), Some((100, 5)));
        // Last visible cell maps to (102, 7).
        assert_eq!(layout.cell_at(17, 3), Some((102, 7)));
    }

    #[test]
    fn reset_clears_per_frame_fields_but_keeps_drag() {
        let mut cache = LayoutCache {
            editor: Some(rect(0, 0, 1, 1)),
            drag: Some(DragState::ResultSelect),
            ..Default::default()
        };
        cache.reset_for_render();
        assert!(cache.editor.is_none());
        assert!(matches!(cache.drag, Some(DragState::ResultSelect)));
    }
}
