//! Filesystem posture for the sealed seed.
//!
//! The sealed blob is not a plaintext key, but it is still the single most
//! valuable file on the machine. It gets a private directory, restrictive modes,
//! no backup, and a check for symlink and ownership before every open — not once
//! at setup, every time.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum CustodyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is a symlink; refusing to open it")]
    Symlink(PathBuf),
    #[error("{0} is not owned by this user")]
    ForeignOwner(PathBuf),
    #[error("{path} has mode {mode:o}; it must be reachable only by its owner")]
    TooPermissive { path: PathBuf, mode: u32 },
    #[error("{0} is not a regular file")]
    NotRegular(PathBuf),
}

pub type Result<T> = std::result::Result<T, CustodyError>;

/// `~/.syndeo/.keystore`, mode 0700, hidden, excluded from backups.
pub struct Vault {
    dir: PathBuf,
}

impl Vault {
    pub fn open(home: impl AsRef<Path>) -> Result<Self> {
        let dir = home.as_ref().join(".keystore");
        let fresh = !dir.exists();
        fs::create_dir_all(&dir)?;
        restrict(&dir, 0o700)?;
        if fresh {
            hide(&dir);
            exclude_from_backups(&dir);
        }
        Ok(Vault { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn sealed_path(&self) -> PathBuf {
        self.dir.join("seed.sealed")
    }

    pub fn exists(&self) -> bool {
        self.sealed_path().exists()
    }

    /// Read the sealed blob, checking the path every time rather than trusting
    /// that it is still what it was at setup.
    pub fn read_sealed(&self) -> Result<Vec<u8>> {
        let path = self.sealed_path();
        audit(&path)?;
        Ok(fs::read(&path)?)
    }

    /// Write the sealed blob atomically, at mode 0600.
    pub fn write_sealed(&self, bytes: &[u8]) -> Result<()> {
        let path = self.sealed_path();
        if path.exists() {
            audit(&path)?;
        }
        let tmp = self
            .dir
            .join(format!("seed.sealed.new.{}", std::process::id()));
        fs::write(&tmp, bytes)?;
        restrict(&tmp, 0o600)?;
        fs::rename(&tmp, &path)?;
        restrict(&path, 0o600)?;
        hide(&path);
        exclude_from_backups(&path);
        Ok(())
    }
}

/// Symlink, ownership and mode, checked before use.
pub fn audit(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(CustodyError::Symlink(path.to_path_buf()));
    }
    if !meta.file_type().is_file() {
        return Err(CustodyError::NotRegular(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        if meta.uid() != current_uid() {
            return Err(CustodyError::ForeignOwner(path.to_path_buf()));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(CustodyError::TooPermissive {
                path: path.to_path_buf(),
                mode,
            });
        }
    }
    Ok(())
}

fn restrict(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

/// Best effort; failure here is cosmetic, not a security property.
fn hide(path: &Path) {
    #[cfg(target_os = "macos")]
    run_briefly(
        "/usr/bin/chflags",
        "hidden",
        path,
        "the keystore directory is visible in Finder",
    );
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// Keep the sealed seed out of Time Machine and any backup that walks it.
fn exclude_from_backups(path: &Path) {
    #[cfg(target_os = "macos")]
    run_briefly(
        "/usr/bin/tmutil",
        "addexclusion",
        path,
        "the sealed seed is not excluded from Time Machine",
    );
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// Run one of the two posture commands, and do not wait forever for it.
///
/// Both of these sit on the keystore's startup path, and `tmutil addexclusion`
/// reaches `backupd` over XPC: on a machine where that daemon is busy, or where
/// this binary has not been granted Full Disk Access, or inside an application
/// sandbox, the call blocks rather than failing. Unbounded, that is a keystore
/// that never binds its socket and a shell reporting a ten-second timeout with
/// nothing to say about the cause — on a user's very first run, which is the
/// only run where `Vault::open` calls either of these.
///
/// Both were already best-effort. Giving up is not a new failure mode, it is
/// the existing one made bounded and made audible.
#[cfg(target_os = "macos")]
fn run_briefly(program: &str, verb: &str, path: &Path, cost: &str) {
    use std::time::{Duration, Instant};

    const LIMIT: Duration = Duration::from_secs(3);

    let mut child = match std::process::Command::new(program)
        .arg(verb)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!(program, %err, "could not run it; {cost}");
            return;
        }
    };

    let deadline = Instant::now() + LIMIT;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => {
                tracing::warn!(program, %err, "could not wait for it; {cost}");
                return;
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    tracing::warn!(
        program,
        path = %path.display(),
        "did not finish within three seconds; {cost}"
    );
}

#[cfg(unix)]
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vault_directory_is_private() {
        let home = tempfile::tempdir().unwrap();
        let vault = Vault::open(home.path()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(vault.dir()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        assert!(!vault.exists());
    }

    #[test]
    fn a_sealed_blob_round_trips_at_mode_0600() {
        let home = tempfile::tempdir().unwrap();
        let vault = Vault::open(home.path()).unwrap();
        vault.write_sealed(b"sealed bytes").unwrap();
        assert_eq!(vault.read_sealed().unwrap(), b"sealed bytes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(vault.sealed_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn a_group_readable_seed_is_refused() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let home = tempfile::tempdir().unwrap();
            let vault = Vault::open(home.path()).unwrap();
            vault.write_sealed(b"sealed").unwrap();
            fs::set_permissions(vault.sealed_path(), fs::Permissions::from_mode(0o640)).unwrap();
            assert!(matches!(
                vault.read_sealed(),
                Err(CustodyError::TooPermissive { .. })
            ));
        }
    }

    #[test]
    fn a_symlinked_seed_is_refused() {
        #[cfg(unix)]
        {
            let home = tempfile::tempdir().unwrap();
            let vault = Vault::open(home.path()).unwrap();
            let elsewhere = home.path().join("elsewhere");
            fs::write(&elsewhere, b"attacker controlled").unwrap();
            std::os::unix::fs::symlink(&elsewhere, vault.sealed_path()).unwrap();
            assert!(matches!(vault.read_sealed(), Err(CustodyError::Symlink(_))));
        }
    }

    /// Opening a fresh vault is the first thing a first run does, and it shells
    /// out to set the filesystem posture. `tmutil addexclusion` blocks instead
    /// of failing wherever it cannot reach `backupd` — a sandbox, or a binary
    /// without Full Disk Access — and an unbounded wait there is a keystore
    /// that never binds its socket.
    ///
    /// Asserted from another thread, because the failure this guards against is
    /// a hang: a test that merely measured elapsed time would hang with it.
    #[test]
    fn opening_a_fresh_vault_does_not_wait_forever() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let home = tempfile::tempdir().unwrap();
            let opened = Vault::open(home.path()).is_ok();
            let _ = tx.send(opened);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(opened) => assert!(opened, "the vault did not open"),
            Err(_) => panic!("Vault::open did not return; a posture command is unbounded"),
        }
    }
}
