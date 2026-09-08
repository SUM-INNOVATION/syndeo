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
