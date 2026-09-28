//! Secrets a process is started with, taken out of its environment first.
//!
//! The keystore is handed its session secret on its environment, and a
//! scripted run may set `SYNDEO_PASSPHRASE`. Both have to leave the
//! environment before anything else can read it or inherit it — and
//! changing the environment is only sound while the process has one thread,
//! because another thread can be reading it at the same moment (glibc does
//! not synchronise that, and Rust 2024 makes the call `unsafe` for it).
//!
//! So each binary that takes such a secret calls [`capture`] at the top of
//! `main`, before it builds a runtime or starts a thread, and code that needs
//! a value later calls [`take`], which hands it over once. Nothing else in the
//! tree touches the environment.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use zeroize::Zeroizing;

/// The session secret the shell gives the keystore it spawns.
pub const SESSION_SECRET: &str = "SYNDEO_SESSION_SECRET";
/// A passphrase supplied by a script instead of a person.
pub const PASSPHRASE: &str = "SYNDEO_PASSPHRASE";

static CAPTURED: OnceLock<Mutex<HashMap<&'static str, Zeroizing<String>>>> = OnceLock::new();

fn captured() -> &'static Mutex<HashMap<&'static str, Zeroizing<String>>> {
    CAPTURED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Move `names` out of the environment and into this process's keeping.
///
/// Call once, first thing in `main`, while the process is still one thread:
/// before a runtime is built, before logging starts a thread, before anything
/// is spawned. A name that is not set is simply absent; a value that is not
/// UTF-8 is removed from the environment all the same and not kept.
pub fn capture(names: &[&'static str]) {
    let mut kept = captured().lock().unwrap_or_else(|e| e.into_inner());
    for name in names {
        if let Some(value) = std::env::var_os(name) {
            std::env::remove_var(name);
            if let Ok(value) = value.into_string() {
                kept.insert(name, Zeroizing::new(value));
            }
        }
    }
}

/// The captured value of `name`, handed over once. `None` if it was not set,
/// was not captured, or has already been taken.
pub fn take(name: &str) -> Option<Zeroizing<String>> {
    captured()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_captured_secret_leaves_the_environment_and_is_handed_over_once() {
        const NAME: &str = "SYNDEO_TEST_STARTUP_SECRET";
        std::env::set_var(NAME, "hunter2 but longer");
        capture(&[NAME, "SYNDEO_TEST_STARTUP_NEVER_SET"]);

        assert!(std::env::var_os(NAME).is_none(), "still in the environment");
        // Nor in what a child is given.
        let child = std::process::Command::new("env").output().unwrap();
        assert!(!String::from_utf8_lossy(&child.stdout).contains(NAME));

        assert_eq!(
            take(NAME).as_deref().map(String::as_str),
            Some("hunter2 but longer")
        );
        assert!(take(NAME).is_none(), "handed over twice");
        assert!(take("SYNDEO_TEST_STARTUP_NEVER_SET").is_none());
    }

    /// Changing the environment is confined to [`capture`]. Every other
    /// `set_var` or `remove_var` in the tree, outside tests, is a regression.
    #[test]
    fn nothing_else_in_the_tree_changes_the_environment() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut offenders = Vec::new();
        for krate in std::fs::read_dir(&crates).unwrap().flatten() {
            let src = krate.path().join("src");
            let mut stack = vec![src];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if path.extension().and_then(|e| e.to_str()) != Some("rs")
                        || path.ends_with("syndeo-ipc/src/startup.rs")
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(&path).unwrap();
                    // Test modules may set up their own environment.
                    let code = text.split("#[cfg(test)]").next().unwrap_or("");
                    for (n, line) in code.lines().enumerate() {
                        let line = line.trim_start();
                        if line.starts_with("//") {
                            continue;
                        }
                        if line.contains("env::set_var(") || line.contains("env::remove_var(") {
                            offenders.push(format!("{}:{}", path.display(), n + 1));
                        }
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the environment is changed at {offenders:#?}"
        );
    }

    /// The binaries that take a secret capture it before anything else in
    /// `main`, and do not let `#[tokio::main]` build a runtime ahead of that.
    #[test]
    fn capture_comes_first_where_secrets_are_taken() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for binary in ["syndeo-keystore/src/main.rs", "syndeo-shell/src/main.rs"] {
            let text = std::fs::read_to_string(crates.join(binary)).unwrap();
            assert!(
                !text.contains("#[tokio::main]"),
                "{binary} builds its runtime first"
            );
            let body = text
                .split("fn main() -> Result<()> {")
                .nth(1)
                .unwrap_or_else(|| panic!("{binary} has no main"));
            let first = body
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with("//"))
                .unwrap();
            assert!(
                first.starts_with("syndeo_ipc::startup::capture("),
                "{binary}: the first thing main does is {first:?}"
            );
        }
    }
}
