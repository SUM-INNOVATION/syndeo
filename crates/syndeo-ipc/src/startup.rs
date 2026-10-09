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
//! `main`, before it builds a runtime or starts a thread, and passes the
//! [`StartupSecrets`] it gets back into the code that runs. Whatever takes a
//! value out of it takes it once; whatever is never taken is wiped when the
//! value is dropped, which is when `run` returns at the latest. Nothing is
//! kept in a static, because a static is never dropped and so never wiped.
//! Nothing else in the tree touches the environment.

use std::collections::HashMap;
use std::fmt;
use zeroize::Zeroizing;

/// The session secret the shell gives the keystore it spawns.
pub const SESSION_SECRET: &str = "SYNDEO_SESSION_SECRET";
/// A passphrase supplied by a script instead of a person.
pub const PASSPHRASE: &str = "SYNDEO_PASSPHRASE";

/// The secrets a process was started with, owned by whoever holds this.
///
/// Not `Clone`: there is one of these, and each value in it is handed over
/// once. Dropping it wipes whatever was not taken.
#[must_use = "dropping the captured secrets wipes them; pass them to what needs them"]
pub struct StartupSecrets {
    values: HashMap<&'static str, Zeroizing<String>>,
}

impl StartupSecrets {
    /// The captured value of `name`, handed over once. `None` if it was not
    /// set, was not captured, or has already been taken.
    pub fn take(&mut self, name: &str) -> Option<Zeroizing<String>> {
        self.values.remove(name)
    }

    /// Nothing at all: for code run without a `main` that captured anything.
    pub fn none() -> Self {
        StartupSecrets {
            values: HashMap::new(),
        }
    }
}

impl fmt::Debug for StartupSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The names are not secret; the values are.
        f.debug_set().entries(self.values.keys()).finish()
    }
}

/// Move `names` out of the environment and into the returned value.
///
/// Call once, first thing in `main`, while the process is still one thread:
/// before a runtime is built, before logging starts a thread, before anything
/// is spawned. A name that is not set is simply absent; a value that is not
/// UTF-8 is removed from the environment all the same and not kept.
pub fn capture(names: &[&'static str]) -> StartupSecrets {
    let mut values = HashMap::new();
    for name in names {
        if let Some(value) = std::env::var_os(name) {
            std::env::remove_var(name);
            if let Ok(value) = value.into_string() {
                values.insert(*name, Zeroizing::new(value));
            }
        }
    }
    StartupSecrets { values }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_captured_secret_leaves_the_environment_and_is_handed_over_once() {
        const NAME: &str = "SYNDEO_TEST_STARTUP_SECRET";
        std::env::set_var(NAME, "hunter2 but longer");
        let mut secrets = capture(&[NAME, "SYNDEO_TEST_STARTUP_NEVER_SET"]);

        assert!(std::env::var_os(NAME).is_none(), "still in the environment");
        // Nor in what a child is given.
        let child = std::process::Command::new("env").output().unwrap();
        assert!(!String::from_utf8_lossy(&child.stdout).contains(NAME));

        // Its name can be logged; its value cannot.
        let shown = format!("{secrets:?}");
        assert!(
            shown.contains(NAME) && !shown.contains("hunter2"),
            "{shown}"
        );

        assert_eq!(
            secrets.take(NAME).as_deref().map(String::as_str),
            Some("hunter2 but longer")
        );
        assert!(secrets.take(NAME).is_none(), "handed over twice");
        assert!(secrets.take("SYNDEO_TEST_STARTUP_NEVER_SET").is_none());
    }

    /// No secret is kept where it can never be dropped. A `static` in this
    /// file is how 0.1.4's first draft kept them, and what a value never taken
    /// out of it stayed in until the process ended.
    #[test]
    fn nothing_here_is_kept_in_a_static() {
        let text = include_str!("startup.rs");
        let code = text.split("#[cfg(test)]").next().unwrap();
        for line in code.lines().map(str::trim_start) {
            if line.starts_with("//") {
                continue;
            }
            let declared = line.strip_prefix("pub ").unwrap_or(line);
            assert!(!declared.starts_with("static "), "a static: {line}");
            for word in ["OnceLock", "OnceCell", "lazy_static!", "thread_local!"] {
                assert!(!line.contains(word), "a {word}: {line}");
            }
        }
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
                first.starts_with("let secrets = syndeo_ipc::startup::capture("),
                "{binary}: the first thing main does is {first:?}"
            );
            // And what was captured goes to `run`, first, which owns it from
            // there. The shell also hands it where it is installed.
            assert!(
                body.contains(".block_on(run(secrets))")
                    || body.contains(".block_on(run(secrets, "),
                "{binary}: main does not hand the captured secrets to run"
            );
        }
    }
}
