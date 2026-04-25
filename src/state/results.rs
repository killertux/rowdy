use std::time::Duration;

use crate::datasource::{Cell, Column};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResultId(pub usize);

pub type Row = Vec<Cell>;

#[derive(Debug)]
pub struct ResultBlock {
    pub id: ResultId,
    #[allow(dead_code)] // surfaced once we render query history above each result.
    pub query: String,
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
#[allow(dead_code)] // `Full` is constructed once we support fully-loaded result sets.
pub enum ResultPayload {
    Clipped {
        preview: Vec<Row>,
        total_rows: usize,
    },
    Full {
        rows: Vec<Row>,
    },
}

impl ResultPayload {
    pub fn rows(&self) -> &[Row] {
        match self {
            Self::Clipped { preview, .. } => preview,
            Self::Full { rows } => rows,
        }
    }

    pub fn total_rows(&self) -> usize {
        match self {
            Self::Clipped { total_rows, .. } => *total_rows,
            Self::Full { rows } => rows.len(),
        }
    }
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
