//! What to do when a call fails in a way that might not fail again.
//!
//! Three rules, and the third is the one that matters for a CLI:
//!
//! 1. Only retry what a retry can fix — 429, 5xx, and transport failures. A
//!    400 is the request being wrong, and sending it again is a slower way to
//!    get the same error.
//! 2. Believe `retry-after` when the provider sends one. Backing off less than
//!    asked earns another 429; backing off more wastes the user's time. In
//!    practice that means 429 only: the header is parsed on every response but
//!    it is carried on [`LlmError::RateLimited`] alone, so a 5xx that happens
//!    to send one still backs off on the curve. That is the case the API
//!    documents sending it for; widening it would be guessing.
//! 3. Cap the attempts, and make each one visible. An invisible retry is a
//!    forty-second silence, and a user watching a silent terminal has no way to
//!    tell it from a hang — they hit Ctrl-C, and the work is lost.

use crate::LlmError;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    /// Total tries, not extra tries: `3` means the original plus two retries.
    pub max_attempts: u32,
    /// First backoff; doubles per attempt.
    pub base: Duration,
    /// Ceiling on any single wait, including one the provider asked for. A
    /// `retry-after: 3600` is the provider saying "come back in an hour", which
    /// for an interactive tool means "tell the user now", not "sleep".
    pub cap: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base: Duration::from_millis(500),
            cap: Duration::from_secs(20),
        }
    }
}

impl Retry {
    /// Off — one attempt, no retry. For a caller that does its own scheduling,
    /// or a test that wants a failure surfaced rather than absorbed. Nothing in
    /// the tree calls it today; the provider tests build a `Retry` with
    /// millisecond delays instead, because they want the retry path exercised
    /// and not skipped.
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// How long to wait before attempt `attempt + 1`, or `None` to give up.
    ///
    /// `attempt` is 1-based and counts the try that just failed.
    pub fn delay_for(&self, err: &LlmError, attempt: u32) -> Option<Duration> {
        if !err.retryable() || attempt >= self.max_attempts {
            return None;
        }
        // A provider-supplied delay overrides the curve entirely: it is the
        // only party that knows when the bucket refills.
        if let Some(after) = err.retry_after() {
            return Some(after.min(self.cap));
        }
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1));
        Some(self.base.saturating_mul(factor).min(self.cap))
    }
}

/// Parse a `retry-after` header value. Seconds only — the HTTP-date form is
/// legal but the Messages API sends seconds, and mis-parsing a date into a
/// duration is worse than ignoring it and backing off on the curve.
pub(crate) fn retry_after_seconds(raw: Option<&str>) -> Option<Duration> {
    raw?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate_limited(after: Option<Duration>) -> LlmError {
        LlmError::RateLimited {
            retry_after: after,
            retry_hint: String::new(),
            message: "slow down".into(),
        }
    }

    #[test]
    fn backoff_doubles_and_stops_at_the_attempt_cap() {
        let r = Retry {
            max_attempts: 3,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(20),
        };
        let err = LlmError::Unavailable {
            status: 529,
            message: "overloaded".into(),
        };
        assert_eq!(r.delay_for(&err, 1), Some(Duration::from_millis(100)));
        assert_eq!(r.delay_for(&err, 2), Some(Duration::from_millis(200)));
        // Third try was the last one allowed.
        assert_eq!(r.delay_for(&err, 3), None);
    }

    #[test]
    fn retry_after_wins_over_the_curve() {
        let r = Retry {
            max_attempts: 5,
            base: Duration::from_millis(100),
            cap: Duration::from_secs(60),
        };
        // Curve would say 100ms; the provider said 7s. Waiting 100ms just
        // spends another attempt on a bucket that is still empty.
        assert_eq!(
            r.delay_for(&rate_limited(Some(Duration::from_secs(7))), 1),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn a_retry_after_longer_than_the_cap_is_clamped() {
        let r = Retry {
            cap: Duration::from_secs(20),
            ..Retry::default()
        };
        assert_eq!(
            r.delay_for(&rate_limited(Some(Duration::from_secs(3600))), 1),
            Some(Duration::from_secs(20))
        );
    }

    #[test]
    fn nothing_retryable_is_never_retried() {
        let r = Retry::default();
        let err = LlmError::BadRequest {
            message: "messages: roles must alternate".into(),
        };
        assert_eq!(r.delay_for(&err, 1), None);
    }

    #[test]
    fn header_parsing_ignores_what_it_cannot_read() {
        assert_eq!(retry_after_seconds(Some("3")), Some(Duration::from_secs(3)));
        assert_eq!(
            retry_after_seconds(Some(" 12 ")),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            retry_after_seconds(Some("Wed, 21 Oct 2026 07:28:00 GMT")),
            None
        );
        assert_eq!(retry_after_seconds(None), None);
    }
}
