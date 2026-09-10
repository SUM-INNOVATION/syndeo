//! When the seed is forgotten without anyone asking for it to be.
//!
//! `Keystore::lock` has always worked; nothing called it. For a browser that
//! runs for days that is the wrong default — an unattended machine is one
//! exploit away from a signing oracle for as long as the process lives.
//!
//! Three things end a session here, and only the first is ours:
//!
//! * **Idleness.** No operation for the configured timeout.
//! * **Sleep.** Detected without any platform API, by comparing the wall clock
//!   against the monotonic clock: neither Linux's `CLOCK_MONOTONIC` nor macOS's
//!   `mach_absolute_time` advances while a machine is suspended, so wall time
//!   running ahead of monotonic time is the machine having been away.
//! * **Screen lock**, where the platform will tell us. See [`crate::session`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One reading of both clocks, in seconds.
///
/// Both, because either alone is fooled: monotonic time misses a suspend, and
/// wall time can be moved by the user or by NTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub monotonic: u64,
    pub wall: u64,
}

/// Source of readings. Injectable so a test can advance time rather than wait.
pub type Clock = Arc<dyn Fn() -> Reading + Send + Sync>;

/// The real clocks.
pub fn system_clock() -> Reading {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let origin = ORIGIN.get_or_init(Instant::now);
    Reading {
        monotonic: origin.elapsed().as_secs(),
        wall: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

/// Why the keystore locked itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Idle,
    Slept,
    ScreenLocked,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Idle => "idle",
            Reason::Slept => "the machine slept",
            Reason::ScreenLocked => "the screen locked",
        }
    }
}

/// How far the wall clock may run ahead of the monotonic clock before we call it
/// a suspend rather than scheduling noise.
const SLEEP_SLACK: u64 = 30;

/// The idle policy, and the last time anything used the seed.
pub struct Watch {
    timeout: Option<Duration>,
    clock: Clock,
    last: Mutex<Reading>,
}

impl Watch {
    pub fn new(timeout: Option<Duration>) -> Self {
        Self::with_clock(timeout, Arc::new(system_clock))
    }

    pub fn with_clock(timeout: Option<Duration>, clock: Clock) -> Self {
        let now = clock();
        Watch {
            timeout,
            clock,
            last: Mutex::new(now),
        }
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    pub fn now(&self) -> Reading {
        (self.clock)()
    }

    /// Record activity. Every operation that touches the seed calls this.
    pub fn touch(&self) {
        let now = self.now();
        *self.last.lock().expect("idle watch is not poisoned") = now;
    }

    /// Seconds since the last operation, by the monotonic clock.
    pub fn idle_for(&self) -> u64 {
        let last = *self.last.lock().expect("idle watch is not poisoned");
        self.now().monotonic.saturating_sub(last.monotonic)
    }

    /// Whether the session should end, and why. Does not itself lock anything.
    pub fn expired(&self) -> Option<Reason> {
        let now = self.now();
        let last = *self.last.lock().expect("idle watch is not poisoned");

        // A suspend shows up as wall time having moved much further than
        // monotonic time. Checked first: it is the stronger signal, and it is
        // true even when the idle timeout has not been reached.
        let monotonic_delta = now.monotonic.saturating_sub(last.monotonic);
        let wall_delta = now.wall.saturating_sub(last.wall);
        if wall_delta > monotonic_delta.saturating_add(SLEEP_SLACK) {
            return Some(Reason::Slept);
        }

        if crate::session::screen_is_locked() == Some(true) {
            return Some(Reason::ScreenLocked);
        }

        match self.timeout {
            Some(timeout) if monotonic_delta >= timeout.as_secs() => Some(Reason::Idle),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A clock the test moves by hand. `monotonic` and `wall` advance together
    /// unless a test deliberately separates them.
    #[derive(Default)]
    struct Fake {
        monotonic: AtomicU64,
        wall: AtomicU64,
    }

    fn fake() -> (Arc<Fake>, Clock) {
        let state = Arc::new(Fake::default());
        let handle = state.clone();
        let clock: Clock = Arc::new(move || Reading {
            monotonic: handle.monotonic.load(Ordering::SeqCst),
            wall: handle.wall.load(Ordering::SeqCst),
        });
        (state, clock)
    }

    fn advance(state: &Fake, secs: u64) {
        state.monotonic.fetch_add(secs, Ordering::SeqCst);
        state.wall.fetch_add(secs, Ordering::SeqCst);
    }

    #[test]
    fn a_session_expires_after_the_timeout_and_activity_resets_it() {
        let (state, clock) = fake();
        let watch = Watch::with_clock(Some(Duration::from_secs(300)), clock);

        advance(&state, 299);
        assert_eq!(watch.expired(), None);

        // Something used the seed with one second to spare.
        watch.touch();
        advance(&state, 299);
        assert_eq!(watch.expired(), None, "activity did not reset the timer");

        advance(&state, 1);
        assert_eq!(watch.expired(), Some(Reason::Idle));
    }

    #[test]
    fn no_timeout_means_no_idle_expiry() {
        let (state, clock) = fake();
        let watch = Watch::with_clock(None, clock);
        advance(&state, 86_400);
        assert_eq!(watch.expired(), None);
    }

    #[test]
    fn a_suspend_ends_the_session_even_well_inside_the_timeout() {
        let (state, clock) = fake();
        let watch = Watch::with_clock(Some(Duration::from_secs(3600)), clock);

        // The lid closed for two hours: wall time moved, monotonic time did not.
        state.wall.fetch_add(7200, Ordering::SeqCst);
        state.monotonic.fetch_add(1, Ordering::SeqCst);
        assert_eq!(watch.expired(), Some(Reason::Slept));
    }

    #[test]
    fn ordinary_scheduling_jitter_is_not_a_suspend() {
        let (state, clock) = fake();
        let watch = Watch::with_clock(Some(Duration::from_secs(3600)), clock);
        state.wall.fetch_add(20, Ordering::SeqCst);
        state.monotonic.fetch_add(1, Ordering::SeqCst);
        assert_eq!(watch.expired(), None);
    }
}
