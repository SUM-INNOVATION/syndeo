//! The shell, as a library.
//!
//! It owns the process model: it spawns the network process, the keystore and
//! the agent; it decides what each of them is told; and it is the only thing in
//! the tree that ever speaks to the keystore.
//!
//! There are two front ends over this — the `syndeo` command and the windowed
//! `syndeo-ui` — and the thing that lets both exist without either of them
//! knowing about the keystore is [`Prompter`]. Asking a human is the shell's
//! job; *how* it asks is the front end's.

pub mod prompt;
pub mod service;
pub mod supervisor;

pub use prompt::{Decision, NonInteractive, Prompter, SignatureRequest, TerminalPrompter};
pub use service::Shell;
pub use supervisor::Supervisor;
