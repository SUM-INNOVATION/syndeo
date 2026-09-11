//! Servo, with its resource loading replaced by the network process.
//!
//! Step four of the build order. The point of it is not that a page appears —
//! that is a consequence — but that boundary one is enforced against a *real*
//! renderer rather than against the agent, which was the only thing available to
//! test it with before.
//!
//! # How the boundary is drawn
//!
//! Servo asks the embedder about every HTTP load it is about to make, through
//! `WebViewDelegate::load_web_resource`. Every one of them is intercepted here
//! and answered from `syndeo-net`, over the same unix socket the agent and the
//! windowed shell use. Nothing is declined, because a declined load is one Servo
//! would then perform itself with its own net crate — so a load we cannot answer
//! is answered with an error, not handed back.
//!
//! What that buys, concretely: the renderer opens no socket, resolves no name,
//! and sees no certificate. It also inherits the cache, the peer fetch and the
//! DNS policy without knowing that any of them exist, which is the property the
//! whole architecture is arranged around.
//!
//! Registering a protocol handler would have been the tidier-looking route and
//! does not work: Servo's `ProtocolRegistry` refuses `http` and `https` by
//! design, in `FORBIDDEN_SCHEMES`. Resource-load interception is the supported
//! way to get in front of them, and it streams, which suits a net process whose
//! bodies already arrive in pieces.
//!
//! # What is not here
//!
//! Servo is behind the `renderer` feature, off by default: it brings
//! SpiderMonkey, Stylo and WebRender with it, which is an hour of compilation
//! and a toolchain not every machine has. Everything that could be written and
//! tested without it — [`bridge`], which is the half that decides what crosses
//! the boundary — is outside the gate.

pub mod bridge;

#[cfg(feature = "renderer")]
pub mod delegate;

#[cfg(feature = "renderer")]
pub use delegate::NetworkDelegate;
