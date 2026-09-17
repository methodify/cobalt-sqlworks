use std::sync::atomic::{AtomicUsize, Ordering};

/// Process-wide accounting of result bytes held in RAM. Shared by all result sets.
#[derive(Debug)]
pub struct MemoryBudget {
    limit: AtomicUsize,
    used: AtomicUsize,
}

impl MemoryBudget {
    pub fn new(limit_bytes: usize) -> Self {
        Self { limit: AtomicUsize::new(limit_bytes), used: AtomicUsize::new(0) }
    }
    pub fn unlimited() -> Self {
        Self::new(usize::MAX)
    }
    pub fn set_limit(&self, bytes: usize) {
        self.limit.store(bytes, Ordering::Relaxed);
    }
    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::Relaxed)
    }
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }
    pub fn over_limit(&self) -> bool {
        self.used() > self.limit()
    }
    pub(crate) fn add(&self, bytes: usize) {
        self.used.fetch_add(bytes, Ordering::Relaxed);
    }
    pub(crate) fn sub(&self, bytes: usize) {
        let _ = self.used.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| Some(u.saturating_sub(bytes)));
    }
}
