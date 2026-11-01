//! Rate-limited diagnostics, mirroring internal/logs/ratelimit/ratelimit.go and
//! internal/logs/exporter/rate_limited_logger.go at the pinned host commit.

use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc, Mutex, PoisonError,
};
use std::time::{Duration, Instant};

/// Gates a recurring diagnostic to at most one emit per `floor`, counting
/// suppressed attempts in between.
pub struct Limiter {
    floor: Duration,
    last_logged: Mutex<Option<Instant>>,
    suppressed: AtomicI64,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl Limiter {
    pub fn new(floor: Duration) -> Self {
        Self::with_clock(floor, Arc::new(Instant::now))
    }

    pub fn with_clock(floor: Duration, now: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            floor,
            last_logged: Mutex::new(None),
            suppressed: AtomicI64::new(0),
            now,
        }
    }

    /// Returns `(true, suppressed_since_last)` when the caller should emit.
    pub fn allow(&self) -> (bool, i64) {
        let now = (self.now)();
        let mut last_logged = self
            .last_logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if last_logged.is_some_and(|last| now.saturating_duration_since(last) <= self.floor) {
            self.suppressed.fetch_add(1, Ordering::SeqCst);
            return (false, 0);
        }
        *last_logged = Some(now);
        (true, self.suppressed.swap(0, Ordering::SeqCst))
    }
}

pub struct RateLimitedLogger {
    limiter: Limiter,
    emit: Arc<dyn Fn(String) + Send + Sync>,
}

impl RateLimitedLogger {
    pub fn new() -> Self {
        Self {
            limiter: Limiter::new(Duration::from_secs(60)),
            emit: Arc::new(move |message| eprintln!("{message}")),
        }
    }

    /// Test seam: deterministic clock and captured output.
    pub fn with_clock_and_emit(
        floor: Duration,
        now: Arc<dyn Fn() -> Instant + Send + Sync>,
        emit: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        Self {
            limiter: Limiter::with_clock(floor, now),
            emit,
        }
    }

    pub fn log(&self, message: impl Into<String>) {
        let (allowed, suppressed) = self.limiter.allow();
        if allowed {
            (self.emit)(format!(
                "{} ({} suppressed since last log)",
                message.into(),
                suppressed
            ));
        }
    }
}

impl Default for RateLimitedLogger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Limiter, RateLimitedLogger};
    use std::sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    };
    use std::time::{Duration, Instant};

    fn clock() -> (Arc<AtomicI64>, Arc<dyn Fn() -> Instant + Send + Sync>) {
        let millis = Arc::new(AtomicI64::new(0));
        let handle = millis.clone();
        let base = Instant::now();
        (
            millis,
            Arc::new(move || base + Duration::from_millis(handle.load(Ordering::SeqCst) as u64)),
        )
    }

    #[test]
    fn allows_one_emit_per_floor_and_counts_suppressed_attempts() {
        let (millis, now) = clock();
        let limiter = Limiter::with_clock(Duration::from_secs(60), now);
        assert_eq!(limiter.allow(), (true, 0));
        millis.store(59_999, Ordering::SeqCst);
        assert_eq!(limiter.allow(), (false, 0));
        assert_eq!(limiter.allow(), (false, 0));
        // time.Since(last) <= floor suppresses the boundary too.
        millis.store(60_000, Ordering::SeqCst);
        assert_eq!(limiter.allow(), (false, 0));
        millis.store(60_001, Ordering::SeqCst);
        assert_eq!(limiter.allow(), (true, 3));
    }

    #[test]
    fn a_new_window_starts_with_a_zero_suppressed_count() {
        let (millis, now) = clock();
        let limiter = Limiter::with_clock(Duration::from_secs(60), now);
        assert_eq!(limiter.allow(), (true, 0));
        millis.store(120_001, Ordering::SeqCst);
        assert_eq!(limiter.allow(), (true, 0));
    }

    #[test]
    fn rate_limited_logger_emits_the_suppressed_suffix_only_when_allowed() {
        let (millis, now) = clock();
        let emitted = Arc::new(Mutex::new(Vec::new()));
        let sink = emitted.clone();
        let logger = RateLimitedLogger::with_clock_and_emit(
            Duration::from_secs(60),
            now,
            Arc::new(move |message| sink.lock().unwrap().push(message)),
        );
        logger.log("first");
        logger.log("second");
        millis.store(60_001, Ordering::SeqCst);
        logger.log("third");
        let emitted = emitted.lock().unwrap();
        assert_eq!(
            *emitted,
            vec![
                "first (0 suppressed since last log)".to_owned(),
                "third (1 suppressed since last log)".to_owned(),
            ]
        );
    }
}
