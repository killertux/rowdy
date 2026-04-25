use std::time::Duration;

use crate::datasource::{Cell, Column};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResultId(pub usize);

pub type Row = Vec<Cell>;

#[derive(Debug)]
pub struct ResultBlock {
    pub id: ResultId,
    pub took: Duration,
    pub columns: Vec<Column>,
    pub payload: ResultPayload,
}

impl ResultBlock {
    pub fn rows(&self) -> &[Row] {
        self.payload.rows()
    }

    pub fn total_rows(&self) -> usize {
        self.payload.total_rows()
    }
}

#[derive(Debug)]
pub enum ResultPayload {
    Clipped {
        preview: Vec<Row>,
        total_rows: usize,
    },
}

impl ResultPayload {
    pub fn rows(&self) -> &[Row] {
        let Self::Clipped { preview, .. } = self;
        preview
    }

    pub fn total_rows(&self) -> usize {
        let Self::Clipped { total_rows, .. } = self;
        *total_rows
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
    pub fn move_in(&mut self, drow: i32, dcol: i32, max_rows: usize, max_cols: usize) {
        if max_rows == 0 || max_cols == 0 {
            return;
        }
        let new_row = (self.row as i32 + drow).clamp(0, max_rows as i32 - 1);
        let new_col = (self.col as i32 + dcol).clamp(0, max_cols as i32 - 1);
        self.row = new_row as usize;
        self.col = new_col as usize;
    }

    pub fn jump_to(&mut self, row: usize, col: usize) {
        self.row = row;
        self.col = col;
    }
}

/// Sub-mode within `Mode::ResultExpanded`. The anchor in `Visual` /
/// `YankFormat` plus the live cursor define a rectangular selection.
#[derive(Debug, Clone, Copy)]
pub enum ResultViewMode {
    Normal,
    Visual { anchor: ResultCursor },
    /// Awaiting the user's CSV/TSV/JSON pick after `y` was pressed in Visual.
    /// We keep the anchor so cancelling drops the user back into Visual with
    /// the same selection.
    YankFormat { anchor: ResultCursor },
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
        row >= self.row_start
            && row <= self.row_end
            && col >= self.col_start
            && col <= self.col_end
    }

    pub fn rows(&self) -> usize {
        self.row_end - self.row_start + 1
    }

    pub fn cols(&self) -> usize {
        self.col_end - self.col_start + 1
    }
}
