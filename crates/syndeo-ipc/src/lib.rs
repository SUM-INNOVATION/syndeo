//! The process boundaries.
//!
//! Three rules make this architecture work, and they are all enforced here
//! rather than by convention:
//!
//! 1. **Renderers never talk to the network.** They hold a [`Channel`] speaking
//!    [`NetRequest`], and nothing else. There is no socket to open.
//! 2. **The agent never talks to the keystore.** The agent's protocol
//!    ([`ShellRequest`]) has no keystore variant. It can ask the shell to prompt
//!    a human; it cannot ask for a signature directly, and it is never given the
//!    keystore's socket path.
//! 3. **The keystore exposes exactly one operation.** [`KeystoreRequest`] has one
//!    signing variant, and it is refused without a [`Confirmation`] that only the
//!    shell can mint.
//!
//! Get these right at the start and everything else is refactorable.

pub mod confirm;
pub mod frame;
pub mod protocol;
pub mod transport;

pub use confirm::{Confirmation, ConfirmationError, Confirmer, SessionSecret};
pub use frame::{FrameError, Framed};
pub use protocol::{
    AgentEvent, KeystoreRequest, KeystoreResponse, NetRequest, NetResponse, ShellRequest,
    ShellResponse, SignaturePurpose,
};
pub use transport::{runtime_dir_for, Channel, Endpoint, Server, TransportError};

/// Exit when the process that started this one goes away.
///
/// The shell spawns the network process, the keystore and the agent, and
/// `kill_on_drop` takes them down with it — but only when the shell's own
/// destructors run. A shell that is force-quit, crashes, or is `kill -9`'d runs
/// no destructors, and every child it started outlives it: still holding the
/// cache, still listening on a socket, invisible until someone runs `ps`. Eight
/// of them accumulated on one machine in an afternoon of testing.
///
/// The parent keeps the write end of this pipe open for exactly as long as it
/// is alive. There is nothing to send on it; the read returning end-of-file is
/// the whole message, and the kernel delivers that however the parent died.
///
/// A child started by hand has no such parent, and `stdin` is then a terminal
/// or `/dev/null` rather than that pipe — so this waits on a handle that never
/// closes, and nothing happens. Which is right: a process nobody is supervising
/// should not exit because nobody is supervising it.
pub fn exit_when_parent_does() {
    std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(|| {
            use std::io::Read;
            let mut stdin = std::io::stdin();
            let mut byte = [0u8; 1];
            loop {
                match stdin.read(&mut byte) {
                    // End of file: the parent's end of the pipe is gone, which
                    // means the parent is gone.
                    Ok(0) => break,
                    // Nothing is ever sent down this pipe, so anything read is
                    // someone else's stdin and none of our business.
                    Ok(_) => continue,
                    Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            tracing::info!("the process that started this one is gone; exiting");
            std::process::exit(0);
        })
        .ok();
}
