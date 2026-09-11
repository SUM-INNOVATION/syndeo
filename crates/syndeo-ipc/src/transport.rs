//! Unix domain socket transport.
//!
//! Each process gets one socket, and a process is only given the paths of the
//! services it is allowed to reach. The agent is never told where the keystore
//! listens, which is the crude half of rule two; the confirmation MAC is the
//! half that still holds if the crude half fails.

use crate::frame::Framed;
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is a symlink; refusing to use it")]
    Symlink(PathBuf),
    #[error("{0} is not owned by this user")]
    ForeignOwner(PathBuf),
    #[error("{path} has mode {mode:o}; it must not be reachable by other users")]
    TooPermissive { path: PathBuf, mode: u32 },
}

/// Where one service listens.
#[derive(Debug, Clone)]
pub struct Endpoint(PathBuf);

impl Endpoint {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Endpoint(path.into())
    }

    /// `<runtime dir>/<name>.sock`, with the directory locked to this user.
    pub fn in_runtime_dir(dir: impl AsRef<Path>, name: &str) -> Result<Self, TransportError> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        restrict_dir(dir)?;
        Ok(Endpoint(dir.join(format!("{name}.sock"))))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Longest socket path a `sockaddr_un` will hold: 104 bytes on macOS, 108 on
/// Linux. Take the smaller, and leave room for the longest file name we use.
const MAX_SOCKET_PATH: usize = 104;

/// Where this home's sockets should live.
///
/// `<home>/run` when it fits, and a short directory under the temporary
/// directory when it does not — a deeply nested home would otherwise make every
/// socket path unbindable, with an error that says nothing about the cause.
pub fn runtime_dir_for(home: &Path) -> PathBuf {
    const LONGEST_NAME: usize = "/keystore.sock".len();
    let preferred = home.join("run");
    if preferred.as_os_str().len() + LONGEST_NAME < MAX_SOCKET_PATH {
        return preferred;
    }

    let digest = blake3::hash(home.to_string_lossy().as_bytes());
    let name = format!("syndeo-{}-{}", current_uid(), &digest.to_hex()[..12]);
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let candidate = base.join(&name);
        if candidate.as_os_str().len() + LONGEST_NAME < MAX_SOCKET_PATH {
            return candidate;
        }
    }
    preferred
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc_getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[derive(Debug)]
pub struct Server {
    listener: UnixListener,
    endpoint: Endpoint,
}

impl Server {
    pub fn bind(endpoint: Endpoint) -> Result<Self, TransportError> {
        // A stale socket from a crashed run is fine to replace; anything else at
        // that path is not, and neither is a symlink pointing elsewhere.
        let path = endpoint.path();
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err(TransportError::Symlink(path.to_path_buf()));
            }
            std::fs::remove_file(path)?;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            restrict_dir(parent)?;
        }

        let listener = UnixListener::bind(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Server { listener, endpoint })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub async fn accept(&self) -> Result<Framed<UnixStream>, TransportError> {
        let (stream, _) = self.listener.accept().await?;
        Ok(Framed::new(stream))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.endpoint.path());
    }
}

/// A client's end of one service.
pub struct Channel;

impl Channel {
    pub async fn connect(endpoint: &Endpoint) -> Result<Framed<UnixStream>, TransportError> {
        let path = endpoint.path();
        check_not_a_symlink(path)?;
        check_owner(path)?;
        Ok(Framed::new(UnixStream::connect(path).await?))
    }
}

fn check_not_a_symlink(path: &Path) -> Result<(), TransportError> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(TransportError::Symlink(path.to_path_buf()));
    }
    Ok(())
}

fn check_owner(path: &Path) -> Result<(), TransportError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path)?;
        if meta.uid() != unsafe { libc_getuid() } {
            return Err(TransportError::ForeignOwner(path.to_path_buf()));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn restrict_dir(dir: &Path) -> Result<(), TransportError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(unix)]
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{NetRequest, NetResponse};

    #[tokio::test]
    async fn a_request_crosses_the_boundary_and_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::in_runtime_dir(dir.path(), "net").unwrap();
        let server = Server::bind(endpoint.clone()).unwrap();

        tokio::spawn(async move {
            let mut framed = server.accept().await.unwrap();
            let request: NetRequest = framed.recv().await.unwrap();
            assert!(matches!(request, NetRequest::Ping));
            framed.send(&NetResponse::Pong).await.unwrap();
            // Hold the server until the client has read the reply.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let mut client = Channel::connect(&endpoint).await.unwrap();
        let response: NetResponse = client.call(&NetRequest::Ping).await.unwrap();
        assert!(matches!(response, NetResponse::Pong));
    }

    #[test]
    fn the_runtime_directory_is_not_readable_by_others() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let nested = dir.path().join("run");
            Endpoint::in_runtime_dir(&nested, "keystore").unwrap();
            let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }

    #[test]
    fn a_deeply_nested_home_still_gets_a_bindable_socket_path() {
        let deep = PathBuf::from("/tmp").join("x".repeat(120));
        let dir = runtime_dir_for(&deep);
        assert!(
            dir.as_os_str().len() + "/keystore.sock".len() < MAX_SOCKET_PATH,
            "{} is still too long",
            dir.display()
        );

        let shallow = PathBuf::from("/tmp/syndeo-home");
        assert_eq!(runtime_dir_for(&shallow), shallow.join("run"));
    }

    #[test]
    fn a_symlinked_socket_path_is_refused() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("real.sock");
            std::fs::write(&real, b"").unwrap();
            let link = dir.path().join("link.sock");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let err = Server::bind(Endpoint::new(link)).unwrap_err();
            assert!(matches!(err, TransportError::Symlink(_)));
        }
    }
}
