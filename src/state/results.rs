use std::time::Duration;

use crate::datasource::{Cell, Column, DriverKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResultId(pub usize);

pub type Row = Vec<Cell>;

#[derive(Debug)]
pub struct ResultBlock {
    pub id: ResultId,
    pub took: Duration,
    pub columns: Vec<Column>,
    /// Every row returned by the query. The inline preview slices the first
    /// few; the expanded view paginates over the full set.
    pub rows: Vec<Row>,
    /// The SQL that produced this block. Stored so `:export sql` can run
    /// source-table inference against the original query even after the
    /// editor buffer has moved on.
    pub sql: String,
    /// The driver kind active when the query ran. Snapshotted onto the
    /// block so a `:conn use` switch later doesn't change the export
    /// dialect of older results.
    pub dialect: DriverKind,
}

impl ResultBlock {
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn total_rows(&self) -> usize {
        self.rows.len()
    }
}

/// Minimum chars per column (`Constraint::Min`) plus the 1-cell separator
/// ratatui's `Table` inserts between columns.
const MIN_COL_WIDTH: u16 = 8;
const COL_SEPARATOR: u16 = 1;

/// How many columns can fit side-by-side in `width` cells, given the
/// per-column floor + separator. Always at least 1 so we can show *something*
/// even on absurdly narrow terminals.
pub fn fit_columns(width: u16) -> usize {
    let cell = MIN_COL_WIDTH + COL_SEPARATOR;
    let raw = ((width as u32 + COL_SEPARATOR as u32) / cell as u32) as usize;
    raw.max(1)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ResultCursor {
    pub row: usize,
    pub col: usize,
}

impl ResultCursor {
    pub fn jump_to(&mut self, row: usize, col: usize) {
        self.row = row;
        self.col = col;
    }
}

/// Per-result column display state for the expanded view. Tracks
/// physical column indices in display order so the user can shuffle
/// columns left/right and hide ones they don't care about. Lives on
/// `Screen::ResultExpanded` and resets every time the view re-opens
/// against a different `ResultId` — column ops are intentionally local
/// to a single grid view, not stored on the underlying `ResultBlock`.
#[derive(Debug, Clone)]
pub struct ColumnView {
    /// Physical column indices, in display order. Hidden columns are
    /// not present. `len()` is the number of currently visible columns.
    pub visible: Vec<usize>,
    /// The total number of columns the originating `ResultBlock` has.
    /// Snapshotted so `reset()` knows what to repopulate `visible`
    /// with — the block itself is not held by reference here.
    total: usize,
}

impl ColumnView {
    pub fn new(total: usize) -> Self {
        Self {
            visible: (0..total).collect(),
            total,
        }
    }

    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// Visual position of a physical column, if currently visible.
    pub fn visual_position(&self, physical: usize) -> Option<usize> {
        self.visible.iter().position(|&p| p == physical)
    }

    /// Swap the focused column with the one to its left in display
    /// order. No-op when it's already leftmost or hidden.
    pub fn move_left(&mut self, physical: usize) {
        if let Some(v) = self.visual_position(physical)
            && v > 0
        {
            self.visible.swap(v, v - 1);
        }
    }

    /// Swap the focused column with the one to its right in display
    /// order. No-op when it's already rightmost or hidden.
    pub fn move_right(&mut self, physical: usize) {
        if let Some(v) = self.visual_position(physical)
            && v + 1 < self.visible.len()
        {
            self.visible.swap(v, v + 1);
        }
    }

    /// Hide the focused column. Returns the physical index of the
    /// column the cursor should land on next (the entry that took the
    /// hidden slot, or the new last if we hid the last visible). `None`
    /// if hiding would empty the view — the caller refuses in that case.
    pub fn hide(&mut self, physical: usize) -> Option<usize> {
        let v = self.visual_position(physical)?;
        if self.visible.len() <= 1 {
            return None;
        }
        self.visible.remove(v);
        let next_visual = v.min(self.visible.len() - 1);
        Some(self.visible[next_visual])
    }

    /// Restore identity order with every column visible.
    pub fn reset(&mut self) {
        self.visible = (0..self.total).collect();
    }
}

/// Sub-mode within `Screen::ResultExpanded`. The anchor in `Visual` /
/// `YankFormat` plus the live cursor define a rectangular selection.
#[derive(Debug, Clone, Copy)]
pub enum ResultViewMode {
    Normal,
    Visual {
        anchor: ResultCursor,
    },
    /// Awaiting the user's CSV/TSV/JSON pick after `y` was pressed in Visual.
    /// We keep the anchor so cancelling drops the user back into Visual with
    /// the same selection.
    YankFormat {
        anchor: ResultCursor,
    },
}

impl ResultViewMode {
    pub fn anchor(&self) -> Option<ResultCursor> {
        match self {
            Self::Normal => None,
            Self::Visual { anchor } | Self::YankFormat { anchor } => Some(*anchor),
        }
    }
}

/// Inclusive cell rectangle expressed as `(row_start, col_start, row_end,
/// col_end)`. Returned by `selection_rect` so the renderer can highlight
/// every cell inside it.
#[derive(Debug, Clone, Copy)]
pub struct SelectionRect {
    pub row_start: usize,
    pub col_start: usize,
    pub row_end: usize,
    pub col_end: usize,
}

impl SelectionRect {
    pub fn new(a: ResultCursor, b: ResultCursor) -> Self {
        Self {
            row_start: a.row.min(b.row),
            col_start: a.col.min(b.col),
            row_end: a.row.max(b.row),
            col_end: a.col.max(b.col),
        }
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.row_start && row <= self.row_end && col >= self.col_start && col <= self.col_end
    }

    pub fn rows(&self) -> usize {
        self.row_end - self.row_start + 1
    }

    pub fn cols(&self) -> usize {
        self.col_end - self.col_start + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_view_starts_in_identity_order() {
        let v = ColumnView::new(4);
        assert_eq!(v.visible(), &[0, 1, 2, 3]);
    }

    #[test]
    fn move_right_swaps_with_visual_neighbor() {
        let mut v = ColumnView::new(3);
        v.move_right(0); // 0 → 1
        assert_eq!(v.visible(), &[1, 0, 2]);
        v.move_right(0); // 0 → 2
        assert_eq!(v.visible(), &[1, 2, 0]);
        v.move_right(0); // already last → no-op
        assert_eq!(v.visible(), &[1, 2, 0]);
    }

    #[test]
    fn move_left_swaps_with_visual_neighbor() {
        let mut v = ColumnView::new(3);
        v.move_left(2); // 2 → 1
        assert_eq!(v.visible(), &[0, 2, 1]);
        v.move_left(2); // 2 → 0
        assert_eq!(v.visible(), &[2, 0, 1]);
        v.move_left(2); // already first → no-op
        assert_eq!(v.visible(), &[2, 0, 1]);
    }

    #[test]
    fn hide_drops_column_and_returns_next_focus() {
        let mut v = ColumnView::new(3);
        let next = v.hide(1).expect("focus carry-over");
        assert_eq!(v.visible(), &[0, 2]);
        // The slot the hidden column held now belongs to physical 2.
        assert_eq!(next, 2);
    }

    #[test]
    fn hide_last_column_refuses() {
        let mut v = ColumnView::new(2);
        v.hide(0);
        // One column left: hiding it would empty the view.
        assert!(v.hide(1).is_none());
        assert_eq!(v.visible(), &[1]);
    }

    #[test]
    fn reset_restores_identity_order_and_unhides() {
        let mut v = ColumnView::new(3);
        v.move_right(0);
        v.hide(1);
        assert_ne!(v.visible(), &[0, 1, 2]);
        v.reset();
        assert_eq!(v.visible(), &[0, 1, 2]);
    }
}
