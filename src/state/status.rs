use std::time::Duration;

#[derive(Debug, Default)]
pub enum QueryStatus {
    #[default]
    Idle,
    Running {
        query: String,
    },
    Succeeded {
        rows: usize,
        /// Set for INSERT/UPDATE/DELETE/etc. — drives the "X affected" status
        /// label. `None` for SELECT-shaped statements.
        affected: Option<u64>,
        took: Duration,
    },
    Failed {
        error: String,
    },
    Cancelled,
    /// Transient informational message (yank/export confirmation, etc.).
    /// Persists in the bar until the next status change.
    Notice {
        msg: String,
    },
}
