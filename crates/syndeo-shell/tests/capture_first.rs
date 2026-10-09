//! Every program that starts Syndeo's siblings captures where its own image
//! is installed before it does anything else in `main`; only a secret taken
//! out of the environment may come first.
//!
//! The capture is a capability: nothing that starts a sibling can be made
//! without the `InstallDir` it returns, so a program that drops the call does
//! not compile. These tests keep it first, before anything that could take
//! long enough for an upgrade to remove the version, and cover syndeo-servo,
//! which compiles only with its renderer, built weekly rather than here.

use std::path::{Path, PathBuf};

const CAPTURE: &str = "let install = syndeo_shell::supervisor::capture_install_dir()?;";

/// The programs that start a sibling, by their `main`.
const PROGRAMS: [&str; 4] = [
    "syndeo-shell/src/main.rs",
    "syndeo-ui/src/main.rs",
    "syndeo-webkit/src/main.rs",
    "syndeo-servo/src/main.rs",
];

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn every_program_that_starts_siblings_captures_first_thing_in_main() {
    for program in PROGRAMS {
        let text = std::fs::read_to_string(crates().join(program)).unwrap();
        let body = text
            .split("fn main() -> ")
            .nth(1)
            .and_then(|rest| rest.split_once('{'))
            .map(|(_, body)| body)
            .unwrap_or_else(|| panic!("{program} has no main that returns a Result"));
        let statements: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .collect();
        let at = statements
            .iter()
            .position(|line| *line == CAPTURE)
            .unwrap_or_else(|| panic!("{program}: main does not begin with {CAPTURE}"));
        for before in &statements[..at] {
            assert!(
                before.starts_with("let secrets = syndeo_ipc::startup::capture("),
                "{program}: main does {before:?} before it captures where it is installed"
            );
        }
    }
}

#[test]
fn every_crate_that_makes_a_supervisor_is_one_of_those_programs() {
    let mut makers = Vec::new();
    for entry in std::fs::read_dir(crates()).unwrap() {
        let krate = entry.unwrap().path();
        let mut sources = vec![krate.join("src")];
        while let Some(dir) = sources.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && std::fs::read_to_string(&path)
                        .unwrap()
                        .contains("Supervisor::new(")
                {
                    makers.push(krate.file_name().unwrap().to_string_lossy().into_owned());
                }
            }
        }
    }
    assert!(!makers.is_empty(), "no Supervisor is made anywhere");
    for maker in makers {
        assert!(
            PROGRAMS
                .iter()
                .any(|program| program.starts_with(&format!("{maker}/"))),
            "{maker} makes a Supervisor and is not in PROGRAMS"
        );
    }
}
