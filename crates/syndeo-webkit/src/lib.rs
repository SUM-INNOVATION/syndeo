//! The parts of syndeo-webkit that are tested apart from its window, and that
//! the Syndeo app shares with it: the proxy configuration, the pinned
//! authority, and the proxy a renderer starts and owns.

#[cfg(target_os = "macos")]
#[path = "proxied_macos.rs"]
pub mod proxied;

#[cfg(target_os = "macos")]
#[path = "pin_macos.rs"]
pub mod pin;

#[cfg(target_os = "macos")]
#[path = "owned_macos.rs"]
pub mod owned;
