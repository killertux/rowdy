use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);

#[derive(Debug, Default)]
pub struct RequestCounter {
    next: AtomicU64,
}

impl RequestCounter {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    pub fn next(&self) -> RequestId {
        RequestId(self.next.fetch_add(1, Ordering::Relaxed))
    }
}
