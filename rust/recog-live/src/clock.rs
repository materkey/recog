//! Time as the Python modules read it: `time.time()`, `time.monotonic()` and
//! `time.sleep()`, behind a trait so tests can run on a clock they move themselves.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The three clock operations the live modules use, in seconds as `f64`.
pub trait Clock: Send + Sync {
    /// `time.time()`: seconds since the Unix epoch.
    fn wall(&self) -> f64;
    /// `time.monotonic()`: seconds from an arbitrary start, never going back.
    fn monotonic(&self) -> f64;
    /// `time.sleep(seconds)`.
    fn sleep(&self, seconds: f64);
}

/// A shared clock handle.
pub type SharedClock = Arc<dyn Clock>;

/// The system's clocks.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    #[must_use]
    pub fn new() -> Self {
        Self { origin: Instant::now() }
    }

    /// The system clock as a shared handle.
    #[must_use]
    pub fn shared() -> SharedClock {
        Arc::new(Self::new())
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn wall(&self) -> f64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
    }

    fn monotonic(&self) -> f64 {
        self.origin.elapsed().as_secs_f64()
    }

    fn sleep(&self, seconds: f64) {
        if seconds > 0.0 && seconds.is_finite() {
            std::thread::sleep(Duration::from_secs_f64(seconds));
        }
    }
}

/// A clock that moves only when told to; `sleep` moves it at once.
///
/// The wall clock is the monotonic one plus a fixed epoch offset, so both advance
/// together. `wait_until` lets a test thread block until another thread's sleeps have
/// moved the clock far enough.
#[derive(Debug, Clone)]
pub struct ManualClock {
    inner: Arc<(Mutex<ManualState>, Condvar)>,
}

#[derive(Debug)]
struct ManualState {
    now: f64,
    epoch: f64,
    sleeps: Vec<f64>,
}

impl ManualClock {
    /// A clock at monotonic `0.0` whose wall clock reads `epoch` then.
    #[must_use]
    pub fn new(epoch: f64) -> Self {
        Self { inner: Arc::new((Mutex::new(ManualState { now: 0.0, epoch, sleeps: Vec::new() }), Condvar::new())) }
    }

    /// Moves the clock forward by `seconds`.
    pub fn advance(&self, seconds: f64) {
        let (lock, cond) = &*self.inner;
        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).now += seconds;
        cond.notify_all();
    }

    /// Sets the monotonic reading.
    pub fn set(&self, monotonic: f64) {
        let (lock, cond) = &*self.inner;
        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).now = monotonic;
        cond.notify_all();
    }

    /// Every `sleep` call so far, in seconds, in order.
    #[must_use]
    pub fn sleeps(&self) -> Vec<f64> {
        self.inner.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).sleeps.clone()
    }

    /// Blocks until the monotonic reading is at least `monotonic`, or `timeout` of
    /// real time passed; returns whether it got there.
    pub fn wait_until(&self, monotonic: f64, timeout: Duration) -> bool {
        let (lock, cond) = &*self.inner;
        let guard = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (guard, _) = cond
            .wait_timeout_while(guard, timeout, |s| s.now < monotonic)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.now >= monotonic
    }

    /// The clock as a shared handle.
    #[must_use]
    pub fn shared(&self) -> SharedClock {
        Arc::new(self.clone())
    }
}

impl Clock for ManualClock {
    fn wall(&self) -> f64 {
        let s = self.inner.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        s.epoch + s.now
    }

    fn monotonic(&self) -> f64 {
        self.inner.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).now
    }

    fn sleep(&self, seconds: f64) {
        let (lock, cond) = &*self.inner;
        let mut s = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        s.sleeps.push(seconds);
        if seconds > 0.0 {
            s.now += seconds;
        }
        cond.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_sleep_moves_both_clocks_and_is_recorded() {
        let clock = ManualClock::new(1000.0);
        clock.sleep(1.5);
        clock.advance(0.5);
        assert!((clock.monotonic() - 2.0).abs() < 1e-12);
        assert!((clock.wall() - 1002.0).abs() < 1e-12);
        assert_eq!(clock.sleeps(), vec![1.5]);
    }

    #[test]
    fn manual_clock_wait_until_another_thread_sleeps_returns_true() {
        let clock = ManualClock::new(0.0);
        let other = clock.clone();
        let t = std::thread::spawn(move || other.sleep(3.0));
        assert!(clock.wait_until(3.0, Duration::from_secs(5)));
        t.join().unwrap();
    }

    #[test]
    fn system_clock_monotonic_never_goes_back() {
        let clock = SystemClock::new();
        let a = clock.monotonic();
        let b = clock.monotonic();
        assert!(b >= a);
        assert!(clock.wall() > 1.7e9);
    }
}
