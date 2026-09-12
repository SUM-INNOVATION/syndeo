//! A renderer that works, confined to a proxy it cannot bypass.
//!
//! macOS only, and not by omission: this embeds `WKWebView`, and the whole
//! design rests on two things only WebKit on this platform provides — a data
//! store that can be pointed at a proxy it cannot route around, and an
//! authentication challenge we can answer with a pinned authority. There is no
//! meaningful "port" of that to another platform; there is a different design,
//! for a different engine, which is a different piece of work.
//!
//! Building it elsewhere used to drag wry's GTK backend into the workspace and
//! break the Linux build on `atk-sys`, for a binary that could never have run.
//! So the dependencies are macOS-only and this is a stub everywhere else.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
#[path = "pin_macos.rs"]
mod pin;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!(
        "syndeo-webkit embeds WKWebView and runs on macOS only.\n\
         Every other binary in this release works here; this one has nothing to embed."
    );
    std::process::exit(1);
}
