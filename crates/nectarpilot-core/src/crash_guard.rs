use std::{collections::VecDeque, time::Duration};

use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct CrashLoopGuard {
    threshold: usize,
    window: chrono::Duration,
    crashes: VecDeque<DateTime<Utc>>,
}

impl Default for CrashLoopGuard {
    fn default() -> Self {
        Self::new(3, Duration::from_secs(10 * 60))
    }
}

impl CrashLoopGuard {
    #[must_use]
    pub fn new(threshold: usize, window: Duration) -> Self {
        Self {
            threshold: threshold.max(1),
            window: chrono::Duration::from_std(window).unwrap_or(chrono::Duration::MAX),
            crashes: VecDeque::new(),
        }
    }

    /// Records a daemon crash. Returns true when safe mode must be entered.
    pub fn record(&mut self, occurred_at: DateTime<Utc>) -> bool {
        self.crashes.push_back(occurred_at);
        let cutoff = occurred_at - self.window;
        while self.crashes.front().is_some_and(|crash| *crash < cutoff) {
            self.crashes.pop_front();
        }
        self.crashes.len() >= self.threshold
    }

    #[must_use]
    pub fn recent_count(&self) -> usize {
        self.crashes.len()
    }

    #[must_use]
    pub fn is_tripped(&self) -> bool {
        self.crashes.len() >= self.threshold
    }

    #[must_use]
    pub fn timestamps(&self) -> Vec<DateTime<Utc>> {
        self.crashes.iter().copied().collect()
    }

    /// Clears the persisted crash window after an explicit user
    /// acknowledgement. This is intentionally not automatic: a repeated
    /// daemon failure still re-enters safe mode immediately.
    pub fn clear(&mut self) {
        self.crashes.clear();
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::CrashLoopGuard;

    #[test]
    fn third_crash_within_ten_minutes_enters_safe_mode() {
        let now = Utc::now();
        let mut guard = CrashLoopGuard::default();
        assert!(!guard.record(now));
        assert!(!guard.record(now + Duration::minutes(2)));
        assert!(guard.record(now + Duration::minutes(9)));
    }
}
