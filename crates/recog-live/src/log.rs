//! Log lines on stderr, and `recog/log_utils.py`'s rate limit for hot paths.
//!
//! The rate limit's contract is Python's: the first message of a key goes out at once,
//! repeats within the interval are counted and suppressed, and the next one that goes
//! out says `(+N suppressed)`. The lines keep the Python prefixes (`[mic]`, `[pulse]`).
//!
//! A [`Logger`] carries where lines go and whose limiter counts them: the process's
//! stderr and one limiter for the process ([`Logger::stderr`]), or a list a test reads
//! back ([`Logger::capture`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::clock::{SharedClock, SystemClock};

/// The default interval of [`Logger::rate_limited`], `log_rate_limited`'s `interval_s`.
pub const RATE_LIMIT_INTERVAL_S: f64 = 30.0;

/// Per-key state of the rate limit: when a message last went out, and how many were
/// suppressed since.
pub struct RateLimiter {
    clock: SharedClock,
    state: Mutex<HashMap<String, (f64, u64)>>,
}

impl RateLimiter {
    /// A limiter reading `clock`'s monotonic time.
    #[must_use]
    pub fn new(clock: SharedClock) -> Self {
        Self { clock, state: Mutex::new(HashMap::new()) }
    }

    /// The line to print for `msg` under `key`, or None while it is suppressed.
    pub fn admit(&self, key: &str, msg: &str, interval_s: f64) -> Option<String> {
        let now = self.clock.monotonic();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let suppressed = match state.get_mut(key) {
            Some((last, suppressed)) if now - *last < interval_s => {
                *suppressed += 1;
                return None;
            }
            Some((_, suppressed)) => *suppressed,
            None => 0,
        };
        state.insert(key.to_owned(), (now, 0));
        Some(if suppressed > 0 { format!("{msg} (+{suppressed} suppressed)") } else { msg.to_owned() })
    }
}

type Sink = Arc<dyn Fn(&str) + Send + Sync>;

/// Where log lines go, with the rate limiter that counts them.
#[derive(Clone)]
pub struct Logger {
    sink: Sink,
    limiter: Arc<RateLimiter>,
}

/// The lines a [`Logger::capture`] logger received.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<String>>>);

impl Captured {
    /// Every line so far, in order.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Every line so far joined by newlines, as captured stderr reads.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Logger")
    }
}

impl Logger {
    /// The process's stderr, with the one rate limiter of the process (Python's is a
    /// module global).
    #[must_use]
    pub fn stderr() -> Self {
        static LIMITER: OnceLock<Arc<RateLimiter>> = OnceLock::new();
        let limiter = LIMITER.get_or_init(|| Arc::new(RateLimiter::new(SystemClock::shared()))).clone();
        Self { sink: Arc::new(|line| eprintln!("{line}")), limiter }
    }

    /// A logger whose lines a test reads back, with its own limiter on `clock`.
    #[must_use]
    pub fn capture(clock: SharedClock) -> (Self, Captured) {
        let captured = Captured::default();
        let lines = captured.0.clone();
        let sink: Sink =
            Arc::new(move |line| lines.lock().unwrap_or_else(PoisonError::into_inner).push(line.to_owned()));
        (Self { sink, limiter: Arc::new(RateLimiter::new(clock)) }, captured)
    }

    /// Prints one line, as `print(..., file=sys.stderr)`.
    pub fn line(&self, msg: &str) {
        (self.sink)(msg);
    }

    /// `log_rate_limited(key, msg)`: at most one line per key in 30 s.
    pub fn rate_limited(&self, key: &str, msg: &str) {
        self.rate_limited_every(key, msg, RATE_LIMIT_INTERVAL_S);
    }

    /// `log_rate_limited(key, msg, interval_s)`.
    pub fn rate_limited_every(&self, key: &str, msg: &str, interval_s: f64) {
        if let Some(line) = self.limiter.admit(key, msg, interval_s) {
            (self.sink)(&line);
        }
    }
}

impl Default for Logger {
    fn default() -> Self {
        Self::stderr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    #[test]
    fn repeats_within_the_interval_rate_limited_suppresses_them_and_counts_on_the_next_line() {
        let clock = ManualClock::new(0.0);
        let (log, captured) = Logger::capture(clock.shared());
        log.rate_limited("k", "first");
        clock.advance(10.0);
        log.rate_limited("k", "second");
        log.rate_limited("k", "third");
        clock.advance(25.0);
        log.rate_limited("k", "fourth");
        assert_eq!(captured.lines(), vec!["first", "fourth (+2 suppressed)"]);
    }

    #[test]
    fn two_keys_rate_limited_counts_each_on_its_own() {
        let clock = ManualClock::new(0.0);
        let (log, captured) = Logger::capture(clock.shared());
        log.rate_limited("a", "a1");
        log.rate_limited("b", "b1");
        log.rate_limited("a", "a2");
        assert_eq!(captured.lines(), vec!["a1", "b1"]);
    }

    #[test]
    fn exactly_the_interval_later_rate_limited_lets_the_line_out() {
        let clock = ManualClock::new(0.0);
        let (log, captured) = Logger::capture(clock.shared());
        log.rate_limited_every("k", "one", 5.0);
        clock.advance(5.0);
        log.rate_limited_every("k", "two", 5.0);
        assert_eq!(captured.lines(), vec!["one", "two"]);
    }
}
