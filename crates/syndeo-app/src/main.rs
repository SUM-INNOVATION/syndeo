//! Syndeo for macOS: the desktop application.
//!
//! A native AppKit application around a WebKit web view, bundled as
//! `Syndeo.app`. It starts with no arguments, from Finder, the Dock or
//! Launch Services, and needs no terminal, no shell and no `PATH`. Its one
//! helper, `syndeo-proxy`, comes from the bundle's `Contents/Helpers` and
//! nowhere else. It never starts the keystore, and asks for no passphrase.
//!
//! The same executable, run by another program as `Syndeo control …` or
//! `Syndeo mcp …`, is instead a client through which that program asks the
//! running application to let it drive Syndeo (`agent`, `agent_client`).
//! Launched by Launch Services it is always the application.

#[cfg(target_os = "macos")]
mod app_macos;
#[cfg(target_os = "macos")]
mod browser_macos;
#[cfg(target_os = "macos")]
mod control_macos;
#[cfg(target_os = "macos")]
mod peer_macos;
#[cfg(target_os = "macos")]
mod settings_macos;
// Plain Rust, and used only by the macOS application; tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod address;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod agent;
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod agent_client;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    // A client, when another program runs this with `control` or `mcp`; never
    // when Launch Services launched it. Decided before anything else starts.
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let parent = unsafe { libc::getppid() } as u32;
    match agent::mode(&args, parent) {
        agent::Mode::Control(rest) => std::process::exit(agent_client::control(rest)),
        agent::Mode::Mcp(rest) => std::process::exit(agent_client::mcp(rest)),
        agent::Mode::App => {}
    }
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
