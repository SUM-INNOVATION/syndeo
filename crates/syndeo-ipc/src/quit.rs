//! How a child this process owns is stopped when this process quits on
//! purpose.
//!
//! Closing the pipe a child reads is how it is told: the proxy, for one,
//! answers by carrying what its cache holds only in memory to disk and
//! exiting. It is given a bounded time to do that, then asked to terminate,
//! and only then killed. What a crash or a forced kill of the owner costs is
//! outside this: the child then sees its pipe close however the owner went,
//! and nothing waits for it.

use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

/// How long a child has to exit by itself once its pipe is closed, before it
/// is sent SIGTERM. The proxy, writing its cache's statistics with one
/// durable commit, was measured exiting 11 to 13 ms after its pipe closed
/// (ten quits, crates/syndeo-proxy/tests/quit.rs, on an M-series Mac); this
/// is a bound on how long a quit can wait for one that hangs, not an
/// estimate of how long it takes.
pub const QUIT_GRACE: Duration = Duration::from_secs(2);

/// How long a child has after SIGTERM, before it is killed.
pub const QUIT_AFTER_TERM: Duration = Duration::from_secs(1);

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quit {
    /// By itself, within [`QUIT_GRACE`] of its pipe closing.
    Exited(ExitStatus),
    /// Only once it was sent SIGTERM.
    Terminated(ExitStatus),
    /// Only once it was killed; how, if it could be reaped.
    Killed(Option<ExitStatus>),
}

/// Stop `child` as a normal quit does: close its stdin if this process still
/// holds it there (a caller that took it closes it first), wait up to
/// [`QUIT_GRACE`] for it to exit by itself, then send SIGTERM and wait up to
/// [`QUIT_AFTER_TERM`], then kill it; and reap it whichever way it went.
///
/// Until it is reaped, the child's pid cannot be given to another process,
/// so neither signal can reach anything but this child.
pub fn quit_child(child: &mut Child) -> Quit {
    drop(child.stdin.take());
    if let Some(status) = exits_within(child, QUIT_GRACE) {
        return Quit::Exited(status);
    }
    terminate(child);
    if let Some(status) = exits_within(child, QUIT_AFTER_TERM) {
        return Quit::Terminated(status);
    }
    let _ = child.kill();
    Quit::Killed(child.wait().ok())
}

/// How `child` exited, if it does within `within`; reaped if so.
fn exits_within(child: &mut Child, within: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + within;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn terminate(child: &Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill(2) with a pid this process spawned and has not reaped.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

#[cfg(not(unix))]
fn terminate(_child: &Child) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn sh(script: &str) -> Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn a_child_that_exits_when_its_pipe_closes_is_left_to_exit() {
        let mut child = sh("cat >/dev/null; exit 0");
        let started = Instant::now();
        match quit_child(&mut child) {
            Quit::Exited(status) => assert!(status.success(), "{status}"),
            other => panic!("not left to exit by itself: {other:?}"),
        }
        assert!(started.elapsed() < QUIT_GRACE);
    }

    #[test]
    fn a_child_that_ignores_its_pipe_is_terminated_after_the_grace() {
        let mut child = sh("exec sleep 60");
        let started = Instant::now();
        assert!(matches!(quit_child(&mut child), Quit::Terminated(_)));
        assert!(started.elapsed() >= QUIT_GRACE);
    }

    #[test]
    fn a_child_that_ignores_termination_is_killed() {
        let mut child = sh("trap '' TERM; while :; do sleep 1; done");
        assert!(matches!(quit_child(&mut child), Quit::Killed(Some(_))));
    }
}
