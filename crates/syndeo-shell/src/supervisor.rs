//! Process supervision.
//!
//! The shell spawns every other process and decides what each one is told. That
//! is where the boundaries are actually drawn: the agent is handed the net
//! socket and the shell socket, and nothing else. It is never told where the
//! keystore listens, and it never receives the session secret.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use syndeo_ipc::confirm::SessionSecret;
use syndeo_ipc::transport::Endpoint;
use tokio::process::{Child, Command};

pub struct Supervisor {
    home: PathBuf,
    children: Vec<(String, Child)>,
    /// The writing ends of each child's parent-watch pipe.
    ///
    /// Never written to, and that is the point: they stay open for exactly as
    /// long as this process lives, and the kernel closes them however it ends.
    /// A child reading end-of-file on its stdin knows the shell is gone and
    /// exits, which `kill_on_drop` alone cannot arrange for a shell that was
    /// force-quit.
    watches: Vec<tokio::process::ChildStdin>,
}

impl Supervisor {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Supervisor {
            home: home.into(),
            children: Vec::new(),
            watches: Vec::new(),
        }
    }

    /// Hold the writing end of this child's parent-watch pipe.
    fn watch(&mut self, child: &mut Child) {
        if let Some(pipe) = child.stdin.take() {
            self.watches.push(pipe);
        }
    }

    pub fn runtime_dir(&self) -> PathBuf {
        syndeo_ipc::transport::runtime_dir_for(&self.home)
    }

    /// Sibling binaries, so a build tree and an install both work.
    ///
    /// The invocation path is resolved before its directory is taken, because a
    /// packaged install is commonly a symlink on `PATH` pointing into a private
    /// directory, and on macOS `current_exe` hands back the symlink rather than
    /// its target. `PATH` is the last resort rather than the first: a sibling is
    /// the binary that shipped with this one, and preferring it means a build
    /// tree never picks up an installed copy of a different version.
    pub fn locate(name: &str) -> Result<PathBuf> {
        let exe = std::env::current_exe().context("locating the running binary")?;
        let resolved = std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone());

        let mut tried = Vec::new();
        for directory in [resolved.parent(), exe.parent()].into_iter().flatten() {
            let candidate = directory.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
            tried.push(candidate);
        }

        if let Some(found) = search_path(name) {
            return Ok(found);
        }

        bail!(
            "cannot find {name}. Looked beside {} and on PATH. \
             Every Syndeo binary has to be installed into the same directory; \
             see the install instructions in the README.",
            tried
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" and ")
        )
    }

    /// The network process. Every other process reaches the outside world
    /// through this one.
    pub async fn start_net(&mut self, dns: &str, peers: &[String]) -> Result<Endpoint> {
        let endpoint = Endpoint::new(self.runtime_dir().join("net.sock"));
        clear_stale(endpoint.path());
        let mut command = Command::new(Self::locate("syndeo-net")?);
        command
            .arg("--socket")
            .arg(endpoint.path())
            .arg("--home")
            .arg(&self.home)
            .arg("--dns")
            .arg(dns);
        if !peers.is_empty() {
            command.arg("--peers");
            for address in peers {
                if address != "on" {
                    command.arg("--bootstrap").arg(address);
                }
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the network process")?;
        self.watch(&mut child);
        wait_ready(&mut child, endpoint.path(), "network").await?;
        self.children.push(("net".into(), child));
        Ok(endpoint)
    }

    /// A network process that is in the swarm, for a node whose job is to seed
    /// rather than to browse.
    pub async fn start_peer_node(
        &mut self,
        dns: &str,
        peers: &[String],
        listen: &[String],
        serve_only: bool,
    ) -> Result<Endpoint> {
        let endpoint = Endpoint::new(self.runtime_dir().join("net.sock"));
        clear_stale(endpoint.path());
        let mut command = Command::new(Self::locate("syndeo-net")?);
        command
            .arg("--socket")
            .arg(endpoint.path())
            .arg("--home")
            .arg(&self.home)
            .arg("--dns")
            .arg(dns)
            .arg("--peers");
        if serve_only {
            command.arg("--serve-only");
        }
        for address in listen {
            command.arg("--listen").arg(address);
        }
        for address in peers {
            if address != "on" {
                command.arg("--bootstrap").arg(address);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the network process")?;
        self.watch(&mut child);
        wait_ready(&mut child, endpoint.path(), "network").await?;
        self.children.push(("net".into(), child));
        Ok(endpoint)
    }

    /// The keystore. The session secret goes across on the environment of this
    /// one spawn and is not put anywhere else.
    pub async fn start_keystore(&mut self, secret: &SessionSecret) -> Result<Endpoint> {
        let endpoint = Endpoint::new(self.runtime_dir().join("keystore.sock"));
        clear_stale(endpoint.path());
        let mut child = Command::new(Self::locate("syndeo-keystore")?)
            .arg("serve")
            .arg("--socket")
            .arg(endpoint.path())
            .arg("--home")
            .arg(&self.home)
            .env("SYNDEO_SESSION_SECRET", secret.to_hex())
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the keystore process")?;
        self.watch(&mut child);
        wait_ready(&mut child, endpoint.path(), "keystore").await?;
        self.children.push(("keystore".into(), child));
        Ok(endpoint)
    }

    /// The agent. Note precisely what it is given, and what it is not.
    pub fn start_agent(&mut self, net: &Endpoint, shell: &Endpoint, task: &str) -> Result<()> {
        let mut child = Command::new(Self::locate("syndeo-agent")?)
            .arg("--net-socket")
            .arg(net.path())
            .arg("--shell-socket")
            .arg(shell.path())
            .arg("--task")
            .arg(task)
            .arg("--tools")
            .arg(self.home.join("tools"))
            // No keystore socket. No session secret. There is nothing in this
            // environment that would let the agent reach a key.
            .env_remove("SYNDEO_SESSION_SECRET")
            .env_remove("SYNDEO_PASSPHRASE")
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the agent process")?;
        self.watch(&mut child);
        self.children.push(("agent".into(), child));
        Ok(())
    }

    /// Wait for one named child to exit.
    pub async fn wait_for_child(&mut self, name: &str) -> Result<std::process::ExitStatus> {
        let Some(index) = self.children.iter().position(|(n, _)| n == name) else {
            bail!("no {name} process is running");
        };
        let (_, child) = &mut self.children[index];
        Ok(child.wait().await?)
    }

    /// Ask the children to stop, then insist.
    ///
    /// `start_kill` is SIGKILL, which runs no destructor in the target. That is
    /// fine for a process holding nothing, and wrong for the network process,
    /// which holds the cache: its index commits statistics and eviction stamps
    /// with relaxed durability and flushes them when it closes, so killing it
    /// outright threw away the record of everything it had just served. The
    /// symptom was `syndeo browse --twice` reporting a cache hit and
    /// `syndeo stats` then reporting none.
    ///
    /// So: SIGTERM, a moment to act on it, and SIGKILL for anything that did
    /// not. The grace period is short because nothing here has much to do — a
    /// commit and a close — and a shell that hangs on exit is its own bug.
    pub async fn shutdown(&mut self) {
        const GRACE: Duration = Duration::from_millis(500);

        for (name, child) in self.children.iter_mut() {
            match child.id() {
                Some(pid) => {
                    // SAFETY: `pid` came from a child this process spawned and
                    // has not been reaped, so it names that child or nothing.
                    unsafe { libc_kill(pid as i32, SIGTERM) };
                    tracing::debug!(process = %name, "asked to stop");
                }
                // Already gone.
                None => continue,
            }
        }

        let deadline = Instant::now() + GRACE;
        for (name, child) in self.children.iter_mut() {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, child.wait()).await {
                Ok(_) => tracing::debug!(process = %name, "stopped"),
                Err(_) => {
                    tracing::debug!(process = %name, "did not stop in time; killing it");
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
            }
        }
        self.children.clear();
    }
}

/// A spawned process has to bind before anyone can connect to it — and may
/// never get that far.
///
/// Waiting for the file to appear is not enough: a process killed with SIGKILL
/// leaves its socket behind, and connecting to that gets "connection refused"
/// rather than a wait. So readiness is an actual connection. And the child is
/// watched at the same time: one that exits first — a missing shared library,
/// a bad argument, a crash — is reported at once, with how it exited, instead
/// of as a socket that never appeared ten seconds later.
async fn wait_ready(child: &mut Child, path: &Path, name: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last: Option<std::io::Error> = None;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            bail!("{}", exited_early(name, status, cfg!(target_os = "linux")));
        }
        match tokio::net::UnixStream::connect(path).await {
            Ok(_) => return Ok(()),
            Err(err) => last = Some(err),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    match last {
        Some(err) => bail!("{} never accepted a connection: {err}", path.display()),
        None => bail!("{} did not appear within ten seconds", path.display()),
    }
}

/// What to say about a child that exited before it was ready.
fn exited_early(name: &str, status: std::process::ExitStatus, linux: bool) -> String {
    let mut message = format!("the {name} process exited before it was ready ({status})");
    // 127 is what the dynamic loader exits with when a shared library the
    // binary needs is not installed — on a minimal Debian or Ubuntu, the D-Bus
    // library the keystore's Secret Service backend links against.
    if linux && status.code() == Some(127) {
        message.push_str(
            ". Exit status 127 is how the loader reports a shared library it could not \
             load; for syndeo-keystore on Debian or Ubuntu that is usually libdbus-1-3 \
             (apt install libdbus-1-3), and on Fedora dbus-libs",
        );
    }
    message
}

/// A socket left behind by a process that was killed is worse than no socket:
/// it looks ready and refuses every connection.
fn clear_stale(path: &Path) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        tracing::warn!(path = %path.display(), "socket path is a symlink; not removing it");
        return;
    }
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return;
    }
    let _ = std::fs::remove_file(path);
}

const SIGTERM: i32 = 15;

unsafe fn libc_kill(pid: i32, signal: i32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, signal);
}

/// The last resort when the sibling lookup finds nothing.
///
/// Only entries that are actually executable count, so a directory of the same
/// name on `PATH` is not mistaken for the binary.
fn search_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn a_service_that_exits_first_is_reported_at_once_with_its_status() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let err = wait_ready(&mut sh("exit 3"), &dir.path().join("never.sock"), "network")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("the network process exited before it was ready"),
            "{err}"
        );
        assert!(err.contains("3"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it waited for the socket instead of noticing the exit"
        );
    }

    #[tokio::test]
    async fn a_service_that_binds_late_is_still_ready() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.sock");
        let binding = {
            let path = path.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let listener = tokio::net::UnixListener::bind(&path).unwrap();
                let _ = listener.accept().await;
            })
        };
        wait_ready(&mut sh("exec sleep 30"), &path, "keystore")
            .await
            .unwrap();
        binding.await.unwrap();
    }

    #[test]
    fn a_missing_library_on_linux_is_named() {
        use std::os::unix::process::ExitStatusExt;
        let loader = std::process::ExitStatus::from_raw(127 << 8);
        let said = exited_early("keystore", loader, true);
        assert!(said.contains("libdbus-1-3"), "{said}");
        // Anywhere else, and for any other status, no guess is offered.
        assert!(!exited_early("keystore", loader, false).contains("libdbus"));
        let other = std::process::ExitStatus::from_raw(3 << 8);
        assert!(!exited_early("keystore", other, true).contains("libdbus"));
    }
}
