//! Syndeo for macOS: the desktop application.
//!
//! A native AppKit application around a WebKit web view, bundled as
//! `Syndeo.app`. It starts with no arguments, from Finder, the Dock or
//! Launch Services, and needs no terminal, no shell and no `PATH`. Its one
//! helper, `syndeo-proxy`, comes from the bundle's `Contents/Helpers` and
//! nowhere else. It never starts the keystore, and asks for no passphrase.

#[cfg(target_os = "macos")]
mod app_macos;
#[cfg(target_os = "macos")]
mod browser_macos;
#[cfg(target_os = "macos")]
mod chrome_macos;
#[cfg(target_os = "macos")]
mod settings_macos;
// Plain Rust, and used only by the macOS application; tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod address;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod passkeys;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    // Before anything starts: the directory of the image this process runs,
    // which inside the bundle is how its helpers are found, and only there.
    // See syndeo_shell::supervisor::bundle_helpers.
    let install = syndeo_shell::supervisor::capture_install_dir()?;
    app_macos::run(install)
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Syndeo's desktop application is a macOS application; it does not run here.");
    std::process::exit(1);
}
