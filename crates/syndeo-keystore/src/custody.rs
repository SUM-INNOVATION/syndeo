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
        let tmp = self.dir.join(format!("seed.sealed.new.{}", std::process::id()));
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
    {
        let _ = std::process::Command::new("/usr/bin/chflags")
            .arg("hidden")
            .arg(path)
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// Keep the sealed seed out of Time Machine and any backup that walks it.
fn exclude_from_backups(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("/usr/bin/tmutil")
            .arg("addexclusion")
            .arg(path)
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = path;
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
            let mode = fs::metadata(vault.sealed_path()).unwrap().permissions().mode() & 0o777;
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
}
