//! The parts of syndeo-webkit that are tested apart from its window.

#[cfg(target_os = "macos")]
#[path = "proxied_macos.rs"]
pub mod proxied;
