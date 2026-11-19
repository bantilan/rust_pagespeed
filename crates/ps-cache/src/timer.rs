//! Time constants (`pagespeed/kernel/base/timer.h`).
//!
//! All values in milliseconds unless suffixed otherwise. Months are 31
//! days and years 365 days, exactly as in C++.
#![forbid(unsafe_code)]

/// Milliseconds per second.
pub const SECOND_MS: i64 = 1000;
/// Microseconds per millisecond.
pub const MS_US: i64 = 1000;
/// Microseconds per second.
pub const SECOND_US: i64 = MS_US * SECOND_MS;
/// Nanoseconds per second.
pub const SECOND_NS: i64 = 1000 * SECOND_US;
/// Milliseconds per minute.
pub const MINUTE_MS: i64 = 60 * SECOND_MS;
/// Microseconds per minute.
pub const MINUTE_US: i64 = 60 * SECOND_US;
/// Milliseconds per hour.
pub const HOUR_MS: i64 = 60 * MINUTE_MS;
/// Milliseconds per day.
pub const DAY_MS: i64 = 24 * HOUR_MS;
/// Milliseconds per week.
pub const WEEK_MS: i64 = 7 * DAY_MS;
/// Milliseconds per (31-day) month.
pub const MONTH_MS: i64 = 31 * DAY_MS;
/// Milliseconds per (365-day) year.
pub const YEAR_MS: i64 = 365 * DAY_MS;

/// Controllable clock (`MockTimer`).
#[derive(Debug, Clone)]
pub struct MockTimer {
    now_ms: i64,
}

impl MockTimer {
    /// Builds a clock reading `start_ms`.
    #[must_use]
    pub fn new(start_ms: i64) -> Self {
        MockTimer { now_ms: start_ms }
    }

    /// Current time (`NowMs`).
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// Advances the clock (`AdvanceMs`).
    pub fn advance_ms(&mut self, delta_ms: i64) {
        self.now_ms += delta_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_values() {
        assert_eq!(SECOND_MS, 1000);
        assert_eq!(MINUTE_MS, 60_000);
        assert_eq!(HOUR_MS, 3_600_000);
        assert_eq!(DAY_MS, 86_400_000);
        assert_eq!(WEEK_MS, 604_800_000);
        assert_eq!(MONTH_MS, 2_678_400_000);
        assert_eq!(YEAR_MS, 31_536_000_000);
        assert_eq!(SECOND_US, 1_000_000);
    }
}
