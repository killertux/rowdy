use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub enum QueryStatus {
    #[default]
    Idle,
    Running {
        query: String,
        // Surfaced once the bottom bar renders elapsed-time for in-flight queries.
        #[allow(dead_code)]
        started_at: Instant,
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
}
