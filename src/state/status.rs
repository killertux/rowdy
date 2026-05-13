use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub enum QueryStatus {
    #[default]
    Idle,
    Running {
        query: String,
        /// Wall-clock time the worker accepted the statement. Drives the
        /// elapsed-time counter in the bottom bar; not used for routing
        /// (request IDs are the source of truth for that).
        started_at: Instant,
    },
    Succeeded {
        rows: usize,
        /// Set for INSERT/UPDATE/DELETE/etc. — drives the "X affected" status
        /// label. `None` for SELECT-shaped statements.
        affected: Option<u64>,
        took: Duration,
        /// How many semicolon-separated statements contributed to this
        /// run. `1` for the common single-statement case; higher when
        /// the user ran a selection (or buffer) containing several
        /// statements — the bottom bar surfaces the count so the user
        /// knows the whole script went through.
        statements_run: usize,
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
