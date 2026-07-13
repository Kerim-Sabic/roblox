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
        self.retain_recent(occurred_at);
        self.crashes.len() >= self.threshold
    }

    /// Restores a persisted crash window relative to *now*, not relative to
    /// each historical timestamp. Old, out-of-order, and future values cannot
    /// permanently trip safe mode after a normal restart.
    pub fn restore(
        &mut self,
        timestamps: impl IntoIterator<Item = DateTime<Utc>>,
        now: DateTime<Utc>,
    ) {
        let cutoff = now - self.window;
        let mut recent = timestamps
            .into_iter()
            .filter(|timestamp| *timestamp >= cutoff && *timestamp <= now)
            .collect::<Vec<_>>();
        recent.sort_unstable();
        self.crashes = recent.into();
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

    fn retain_recent(&mut self, reference: DateTime<Utc>) {
        let cutoff = reference - self.window;
        let mut recent = self
            .crashes
            .drain(..)
            .filter(|timestamp| *timestamp >= cutoff && *timestamp <= reference)
            .collect::<Vec<_>>();
        recent.sort_unstable();
        self.crashes = recent.into();
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

    #[test]
    fn restore_drops_expired_future_and_out_of_order_entries() {
        let now = Utc::now();
        let mut guard = CrashLoopGuard::default();
        guard.restore(
            [
                now - Duration::days(2),
                now + Duration::minutes(1),
                now - Duration::minutes(2),
                now - Duration::minutes(9),
            ],
            now,
        );
        assert_eq!(guard.recent_count(), 2);
        assert!(!guard.is_tripped());
        assert_eq!(
            guard.timestamps(),
            vec![now - Duration::minutes(9), now - Duration::minutes(2)]
        );
    }
}
