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
}

impl Supervisor {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Supervisor {
            home: home.into(),
            children: Vec::new(),
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
        let child = command
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the network process")?;
        self.children.push(("net".into(), child));
        wait_for(endpoint.path()).await?;
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
        let child = command
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the network process")?;
        self.children.push(("net".into(), child));
        wait_for(endpoint.path()).await?;
        Ok(endpoint)
    }

    /// The keystore. The session secret goes across on the environment of this
    /// one spawn and is not put anywhere else.
    pub async fn start_keystore(&mut self, secret: &SessionSecret) -> Result<Endpoint> {
        let endpoint = Endpoint::new(self.runtime_dir().join("keystore.sock"));
        clear_stale(endpoint.path());
        let child = Command::new(Self::locate("syndeo-keystore")?)
            .arg("serve")
            .arg("--socket")
            .arg(endpoint.path())
            .arg("--home")
            .arg(&self.home)
            .env("SYNDEO_SESSION_SECRET", secret.to_hex())
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the keystore process")?;
        self.children.push(("keystore".into(), child));
        wait_for(endpoint.path()).await?;
        Ok(endpoint)
    }

    /// The agent. Note precisely what it is given, and what it is not.
    pub fn start_agent(&mut self, net: &Endpoint, shell: &Endpoint, task: &str) -> Result<()> {
        let child = Command::new(Self::locate("syndeo-agent")?)
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
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("spawning the agent process")?;
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

    pub async fn shutdown(&mut self) {
        for (name, child) in self.children.iter_mut() {
            let _ = child.start_kill();
            tracing::debug!(process = %name, "stopped");
        }
        for (_, child) in self.children.iter_mut() {
            let _ = child.wait().await;
        }
        self.children.clear();
    }
}

/// A spawned process has to bind before anyone can connect to it.
///
/// Waiting for the file to appear is not enough: a process killed with SIGKILL
/// leaves its socket behind, and connecting to that gets "connection refused"
/// rather than a wait. So the readiness check is an actual connection.
async fn wait_for(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last: Option<std::io::Error> = None;
    while Instant::now() < deadline {
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
