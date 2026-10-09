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

    /// Sibling binaries, so a build tree, a tarball install and the macOS
    /// package all work.
    ///
    /// The directory of the running image comes first: see [`install_dir`].
    /// For the macOS package it is the only place looked, and a sibling that
    /// is not there is a [`RemovedByUpgrade`]: see [`is_packaged_dir`].
    /// Anywhere else, after it, the invocation path resolved and then as
    /// given, because on macOS `current_exe` hands back a symlink on `PATH`
    /// rather than its target. `PATH` is the last resort rather than the
    /// first: a sibling is the binary that shipped with this one, and
    /// preferring it means a build tree never picks up an installed copy of a
    /// different version.
    pub fn locate(name: &str) -> Result<PathBuf> {
        let dir = install_dir()?;
        let exe = std::env::current_exe().ok();
        locate_from(
            name,
            dir,
            is_packaged_dir(dir),
            exe.as_deref(),
            std::env::var_os("PATH"),
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

/// The directory of the binary this process is running, found once.
///
/// Siblings are looked for here first. It is where the image the kernel is
/// running lives, not where whatever link started it pointed at the time it is
/// asked: a packaged install starts commands through `/usr/local/bin/<name>`,
/// which leads through `libexec/syndeo/current`, a link an upgrade switches.
/// Resolving that link when a sibling is needed — which for the keystore can be
/// long after start — would find whichever version it names then, and run one
/// release's shell against another's keystore. The image path cannot move.
///
/// It is found the first time it is asked for, and every program that starts
/// siblings asks first thing in `main`, through [`capture_install_dir`]: an
/// upgrade removes the version it replaces, and after that the kernel can no
/// longer say where a running image of it came from. If it could not be found
/// then, it is an error now and for the rest of the process.
pub fn install_dir() -> Result<&'static Path> {
    static DIR: std::sync::OnceLock<Result<PathBuf, String>> = std::sync::OnceLock::new();
    let found = DIR.get_or_init(|| {
        let image = running_image().map_err(|err| format!("{err:#}"))?;
        image
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| format!("{} has no directory", image.display()))
    });
    match found {
        Ok(dir) => Ok(dir),
        Err(err) => bail!("cannot tell where this program is installed: {err}"),
    }
}

/// [`install_dir`], found now. `main` calls it before anything else can
/// start, so that a sibling started lazily, long after, comes from the version
/// this process started as, or from nowhere.
pub fn capture_install_dir() -> Result<&'static Path> {
    install_dir()
}

/// The path of the image this process is running, as the kernel has it.
///
/// On macOS `current_exe` is the path a command was started by, links and all,
/// so the kernel is asked instead, and if it cannot say, that is an error:
/// the path the command was started by leads through `current`, which an
/// upgrade moves to another version. It cannot say for an image whose file has
/// been removed, which is what an upgrade does to the version it replaces. On
/// Linux `current_exe` reads `/proc/self/exe`, which already is the running
/// image.
pub fn running_image() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt;
        extern "C" {
            fn proc_pidpath(pid: i32, buffer: *mut u8, size: u32) -> i32;
            fn getpid() -> i32;
        }
        // PROC_PIDPATHINFO_MAXSIZE.
        let mut buffer = vec![0u8; 4 * 1024];
        // SAFETY: the buffer is as large as the size passed, and proc_pidpath
        // writes at most that many bytes and returns how many it wrote.
        let written = unsafe { proc_pidpath(getpid(), buffer.as_mut_ptr(), buffer.len() as u32) };
        if written <= 0 {
            return Err(std::io::Error::last_os_error())
                .context("asking the kernel which image this process runs (proc_pidpath)");
        }
        buffer.truncate(written as usize);
        Ok(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let exe = std::env::current_exe().context("locating the running binary")?;
        Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
    }
}

/// Where the macOS package installs Syndeo, one directory per version.
pub const PACKAGED_VERSIONS: &str = "/usr/local/libexec/syndeo";

/// Whether `dir` is a version directory of the macOS package: directly inside
/// [`PACKAGED_VERSIONS`], which only the package writes. The kernel may name
/// `/usr/local` through the data volume, as
/// `/System/Volumes/Data/usr/local`; that is the same directory.
///
/// The package keeps one version, and an upgrade removes the one it replaces.
/// A process still running from that version has nowhere left to find its
/// siblings: the invocation path, `current` and `PATH` all lead to the new
/// version now, and starting one of its programs would pair two releases. So
/// for a packaged process the lookup fails instead, with
/// [`RemovedByUpgrade`].
pub fn is_packaged_dir(dir: &Path) -> bool {
    let (Some(parent), Some(_)) = (dir.parent(), dir.file_name()) else {
        return false;
    };
    let parent = match parent.strip_prefix("/System/Volumes/Data") {
        Ok(rest) => Path::new("/").join(rest),
        Err(_) => parent.to_path_buf(),
    };
    parent == Path::new(PACKAGED_VERSIONS)
}

/// A packaged process's sibling is not in the version directory it started
/// from: an upgrade has removed that version while this process ran.
#[derive(Debug)]
pub struct RemovedByUpgrade {
    pub name: String,
    pub dir: PathBuf,
}

impl std::fmt::Display for RemovedByUpgrade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is not in {}: this version was removed during an upgrade; quit and restart Syndeo",
            self.name,
            self.dir.display()
        )
    }
}

impl std::error::Error for RemovedByUpgrade {}

/// A sibling beside [`install_dir`] and nowhere else, for a program that has
/// no other place to look. For the macOS package, missing is a
/// [`RemovedByUpgrade`].
pub fn beside_install_dir(name: &str) -> Result<PathBuf> {
    let dir = install_dir()?;
    let beside = dir.join(name);
    if is_packaged_dir(dir) {
        if beside.is_file() {
            return Ok(beside);
        }
        return Err(RemovedByUpgrade {
            name: name.to_string(),
            dir: dir.to_path_buf(),
        }
        .into());
    }
    if beside.exists() {
        return Ok(beside);
    }
    bail!(
        "cannot find {name} next to this binary, in {}. Every Syndeo binary has to be \
         installed into the same directory.",
        dir.display()
    )
}

/// [`Supervisor::locate`], given everything it looks at: the running image's
/// directory, whether that is the package's, the invocation path, and `PATH`.
fn locate_from(
    name: &str,
    dir: &Path,
    packaged: bool,
    exe: Option<&Path>,
    path: Option<std::ffi::OsString>,
) -> Result<PathBuf> {
    if packaged {
        // This version's own directory or nothing: every other place leads
        // to whatever version an upgrade installed since.
        let beside = dir.join(name);
        if beside.is_file() {
            return Ok(beside);
        }
        return Err(RemovedByUpgrade {
            name: name.to_string(),
            dir: dir.to_path_buf(),
        }
        .into());
    }

    let resolved = exe.map(|exe| std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf()));
    let mut tried: Vec<PathBuf> = Vec::new();
    for directory in [
        Some(dir),
        resolved.as_deref().and_then(Path::parent),
        exe.and_then(Path::parent),
    ]
    .into_iter()
    .flatten()
    {
        let candidate = directory.join(name);
        if tried.contains(&candidate) {
            continue;
        }
        if candidate.exists() {
            return Ok(candidate);
        }
        tried.push(candidate);
    }

    if let Some(found) = search_path(name, path) {
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

/// The last resort when the sibling lookup finds nothing, outside the package.
///
/// Only entries that are actually executable count, so a directory of the same
/// name on `PATH` is not mistaken for the binary.
fn search_path(name: &str, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let path = path?;
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

    const SIBLINGS: [&str; 6] = [
        "syndeo-net",
        "syndeo-keystore",
        "syndeo-agent",
        "syndeo-proxy",
        "syndeo-ui",
        "syndeo-webkit",
    ];

    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// What an upgrade from 0.0.9 to 0.0.10 leaves under `root`, laid out as
    /// the package lays it out: 0.0.9's directory gone, 0.0.10 complete, and
    /// `current` and every command link in `bin` leading to 0.0.10. Returns
    /// the directory 0.0.9 was in, and `bin`.
    fn upgraded(root: &Path) -> (PathBuf, PathBuf) {
        let versions = root.join("libexec/syndeo");
        let new = versions.join("0.0.10");
        let bin = root.join("bin");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink("0.0.10", versions.join("current")).unwrap();
        for name in std::iter::once("syndeo").chain(SIBLINGS) {
            executable(&new.join(name));
            std::os::unix::fs::symlink(format!("../libexec/syndeo/current/{name}"), bin.join(name))
                .unwrap();
        }
        (versions.join("0.0.9"), bin)
    }

    #[test]
    fn the_packaged_layout_is_a_directory_directly_inside_the_packages_own() {
        for packaged in [
            "/usr/local/libexec/syndeo/0.1.6",
            "/usr/local/libexec/syndeo/0.0.9",
            "/System/Volumes/Data/usr/local/libexec/syndeo/0.1.6",
        ] {
            assert!(is_packaged_dir(Path::new(packaged)), "{packaged}");
        }
        for not in [
            "/usr/local/libexec/syndeo",
            "/usr/local/libexec/syndeo/0.1.6/tools",
            "/usr/local/libexec/syndeo/..",
            "/usr/local/libexec/other/0.1.6",
            "/usr/local/bin",
            "/tmp/usr/local/libexec/syndeo/0.1.6",
            "/Users/someone/.local/bin",
            "/",
        ] {
            assert!(!is_packaged_dir(Path::new(not)), "{not}");
        }
    }

    #[test]
    fn a_packaged_process_whose_version_was_removed_finds_no_sibling_anywhere() {
        let temp = tempfile::tempdir().unwrap();
        let (gone, bin) = upgraded(temp.path());
        let new = std::fs::canonicalize(temp.path().join("libexec/syndeo/0.0.10")).unwrap();
        let invoked = bin.join("syndeo");
        for name in SIBLINGS {
            let err = locate_from(name, &gone, true, Some(&invoked), Some(bin.clone().into()))
                .unwrap_err();
            assert!(err.downcast_ref::<RemovedByUpgrade>().is_some(), "{err:#}");
            let said = format!("{err:#}");
            assert!(
                said.contains(
                    "this version was removed during an upgrade; quit and restart Syndeo"
                ),
                "{said}"
            );
            // What the fallbacks would have handed it: the new version's.
            let fallback =
                locate_from(name, &gone, false, Some(&invoked), Some(bin.clone().into())).unwrap();
            assert!(
                std::fs::canonicalize(&fallback).unwrap().starts_with(&new),
                "{}",
                fallback.display()
            );
        }
    }

    #[test]
    fn a_packaged_process_takes_its_siblings_from_its_own_version_only() {
        let temp = tempfile::tempdir().unwrap();
        let (_, bin) = upgraded(temp.path());
        let own = temp.path().join("libexec/syndeo/0.0.10");
        for name in SIBLINGS {
            assert_eq!(
                locate_from(name, &own, true, None, None).unwrap(),
                own.join(name)
            );
        }
        // Partway through a removal, what is left is not enough.
        std::fs::remove_file(own.join("syndeo-keystore")).unwrap();
        let err = locate_from(
            "syndeo-keystore",
            &own,
            true,
            Some(&bin.join("syndeo")),
            Some(bin.clone().into()),
        )
        .unwrap_err();
        assert!(err.downcast_ref::<RemovedByUpgrade>().is_some(), "{err:#}");
    }

    #[test]
    fn outside_the_package_the_invocation_path_and_path_are_still_looked_at() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("target/release");
        let installed = temp.path().join("installed");
        let linked = temp.path().join("linked");
        let on_path = temp.path().join("on path");
        for dir in [&image, &installed, &linked, &on_path] {
            std::fs::create_dir_all(dir).unwrap();
        }
        executable(&installed.join("syndeo"));
        executable(&installed.join("syndeo-net"));
        executable(&on_path.join("syndeo-agent"));
        std::os::unix::fs::symlink(installed.join("syndeo"), linked.join("syndeo")).unwrap();
        let invoked = linked.join("syndeo");
        let path = std::env::join_paths([&on_path]).unwrap();

        let net = locate_from(
            "syndeo-net",
            &image,
            false,
            Some(&invoked),
            Some(path.clone()),
        )
        .unwrap();
        assert_eq!(net, installed.canonicalize().unwrap().join("syndeo-net"));
        let agent = locate_from(
            "syndeo-agent",
            &image,
            false,
            Some(&invoked),
            Some(path.clone()),
        )
        .unwrap();
        assert_eq!(agent, on_path.join("syndeo-agent"));
        let err = locate_from("syndeo-ui", &image, false, Some(&invoked), Some(path)).unwrap_err();
        assert!(err.downcast_ref::<RemovedByUpgrade>().is_none(), "{err:#}");
        assert!(
            format!("{err:#}").contains("cannot find syndeo-ui"),
            "{err:#}"
        );
    }
}
