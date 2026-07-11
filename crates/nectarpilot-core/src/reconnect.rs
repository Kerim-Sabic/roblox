use std::{future::Future, time::Duration};

use nectarpilot_contracts::ReconnectProgress;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    maximum_attempts: u8,
    deadline: Duration,
    delays: Vec<Duration>,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            maximum_attempts: 5,
            deadline: Duration::from_secs(15 * 60),
            delays: vec![
                Duration::ZERO,
                Duration::from_secs(5),
                Duration::from_secs(15),
                Duration::from_secs(30),
                Duration::from_secs(60),
            ],
        }
    }
}

impl ReconnectPolicy {
    /// Creates a test/custom policy. Attempts are always clamped to the product
    /// safety ceiling of five.
    #[must_use]
    pub fn new(maximum_attempts: u8, deadline: Duration, delays: Vec<Duration>) -> Self {
        Self {
            maximum_attempts: maximum_attempts.clamp(1, 5),
            deadline,
            delays,
        }
    }

    #[must_use]
    pub const fn maximum_attempts(&self) -> u8 {
        self.maximum_attempts
    }

    #[must_use]
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectOutcome {
    Succeeded {
        attempt: u8,
    },
    Exhausted {
        attempts: u8,
        last_error: Option<String>,
    },
    DeadlineExceeded {
        attempts: u8,
    },
    Cancelled {
        attempts: u8,
    },
}

/// Performs bounded reconnect attempts. Both backoff and each backend attempt
/// are constrained by the same wall-clock deadline.
pub async fn run_bounded_reconnect<F, Fut, P>(
    policy: &ReconnectPolicy,
    cancellation: &CancellationToken,
    mut attempt: F,
    mut progress: P,
) -> ReconnectOutcome
where
    F: FnMut(u8) -> Fut,
    Fut: Future<Output = Result<(), String>>,
    P: FnMut(ReconnectProgress),
{
    let started = Instant::now();
    let mut completed_attempts = 0_u8;
    let mut last_error = None;

    for attempt_number in 1..=policy.maximum_attempts {
        if cancellation.is_cancelled() {
            return ReconnectOutcome::Cancelled {
                attempts: completed_attempts,
            };
        }

        let elapsed = started.elapsed();
        let Some(remaining) = policy.deadline.checked_sub(elapsed) else {
            return ReconnectOutcome::DeadlineExceeded {
                attempts: completed_attempts,
            };
        };

        let delay = policy
            .delays
            .get(usize::from(attempt_number - 1))
            .copied()
            .unwrap_or_else(|| policy.delays.last().copied().unwrap_or(Duration::ZERO));
        if !delay.is_zero() {
            let sleeper = tokio::time::sleep(delay);
            tokio::pin!(sleeper);
            tokio::select! {
                () = cancellation.cancelled() => {
                    return ReconnectOutcome::Cancelled { attempts: completed_attempts };
                }
                result = tokio::time::timeout(remaining, &mut sleeper) => {
                    if result.is_err() {
                        return ReconnectOutcome::DeadlineExceeded { attempts: completed_attempts };
                    }
                }
            }
        }

        let elapsed = started.elapsed();
        let Some(remaining) = policy.deadline.checked_sub(elapsed) else {
            return ReconnectOutcome::DeadlineExceeded {
                attempts: completed_attempts,
            };
        };
        progress(ReconnectProgress {
            attempt: attempt_number,
            maximum_attempts: policy.maximum_attempts,
            elapsed_seconds: elapsed.as_secs(),
            deadline_seconds: policy.deadline.as_secs(),
        });

        let result = tokio::select! {
            () = cancellation.cancelled() => {
                return ReconnectOutcome::Cancelled { attempts: completed_attempts };
            }
            result = tokio::time::timeout(remaining, attempt(attempt_number)) => result,
        };

        completed_attempts = attempt_number;
        match result {
            Ok(Ok(())) => {
                return ReconnectOutcome::Succeeded {
                    attempt: attempt_number,
                };
            }
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => {
                return ReconnectOutcome::DeadlineExceeded {
                    attempts: completed_attempts,
                };
            }
        }
    }

    ReconnectOutcome::Exhausted {
        attempts: completed_attempts,
        last_error,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU8, Ordering},
        },
        time::Duration,
    };

    use tokio_util::sync::CancellationToken;

    use super::{ReconnectOutcome, ReconnectPolicy, run_bounded_reconnect};

    #[tokio::test]
    async fn reconnect_never_exceeds_five_attempts() {
        let attempts = Arc::new(AtomicU8::new(0));
        let attempt_counter = Arc::clone(&attempts);
        let policy = ReconnectPolicy::new(99, Duration::from_secs(1), vec![Duration::ZERO]);
        let result = run_bounded_reconnect(
            &policy,
            &CancellationToken::new(),
            move |_| {
                attempt_counter.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err("still disconnected".to_owned()))
            },
            |_| {},
        )
        .await;

        assert!(matches!(
            result,
            ReconnectOutcome::Exhausted { attempts: 5, .. }
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn hanging_attempt_is_stopped_by_deadline() {
        let policy = ReconnectPolicy::new(5, Duration::from_millis(20), vec![Duration::ZERO]);
        let result = run_bounded_reconnect(
            &policy,
            &CancellationToken::new(),
            |_| async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(())
            },
            |_| {},
        )
        .await;
        assert!(matches!(
            result,
            ReconnectOutcome::DeadlineExceeded { attempts: 1 }
        ));
    }
}
