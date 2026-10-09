//! Siblings are found beside the image the kernel is running, not wherever a
//! link pointed when one was asked for.
//!
//! The macOS package starts every command through
//! `bin/<name> -> ../libexec/syndeo/current/<name>`, and an upgrade switches
//! `current`. These tests build that chain around a copy of their own binary,
//! start the copy through it, switch `current` while the copy is running, and
//! ask the copy where it lives and where its sibling is. On macOS they also
//! remove the copy's version, as an upgrade does, and check what the kernel
//! can still say about it.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set on the copy: the directory it should wait in, and what to report.
const CHILD: &str = "SYNDEO_INSTALL_DIR_TEST_CHILD";
/// Set on the copy instead, for [`removed_child`]: the file to wait for, and
/// whether to capture its directory before waiting (`early`), only after
/// (`late`), or only after, having asked for it uncaptured first
/// (`uncaptured`).
const REMOVED: &str = "SYNDEO_INSTALL_DIR_TEST_REMOVED";
const SIBLING: &str = "syndeo-install-dir-test-sibling";

/// Not a test of its own. In the copy, with [`CHILD`] set, it reports where it
/// is, waits until the parent has switched `current`, and reports again;
/// anywhere else it does nothing.
#[test]
fn child() {
    let Some(go) = std::env::var_os(CHILD) else {
        return;
    };
    let go = PathBuf::from(go);
    // As `main` does, first.
    let install = syndeo_shell::supervisor::capture_install_dir().unwrap();
    println!(
        "@@before {}",
        syndeo_shell::supervisor::running_image()
            .unwrap()
            .parent()
            .unwrap()
            .display()
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while !go.exists() {
        assert!(Instant::now() < deadline, "the parent never switched");
        std::thread::sleep(Duration::from_millis(20));
    }
    println!(
        "@@after {}",
        syndeo_shell::supervisor::running_image()
            .unwrap()
            .parent()
            .unwrap()
            .display()
    );
    println!(
        "@@install_dir {}",
        syndeo_shell::supervisor::install_dir().unwrap().display()
    );
    println!("@@sibling {}", install.locate(SIBLING).unwrap().display());
}

/// `root/libexec/syndeo/<version>/` holding a copy of this test binary and a
/// sibling.
fn version(root: &Path, version: &str) -> PathBuf {
    let dir = root.join("libexec/syndeo").join(version);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), dir.join("probe")).unwrap();
    std::fs::write(dir.join(SIBLING), version).unwrap();
    dir
}

fn switch(root: &Path, to: &str) {
    let temporary = root.join("libexec/syndeo/.current.new");
    std::os::unix::fs::symlink(to, &temporary).unwrap();
    std::fs::rename(&temporary, root.join("libexec/syndeo/current")).unwrap();
}

#[test]
fn a_command_started_through_the_links_keeps_its_own_version_when_current_moves() {
    let temp = tempfile::tempdir().unwrap();
    // A space, because /usr/local paths have none but a home or a build tree can.
    let root = temp.path().join("install root");
    let one = std::fs::canonicalize(version(&root, "0.0.1")).unwrap();
    let two = std::fs::canonicalize(version(&root, "0.0.2")).unwrap();
    switch(&root, "0.0.1");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::os::unix::fs::symlink("../libexec/syndeo/current/probe", root.join("bin/probe")).unwrap();
    // A sibling of the same name beside the link: the wrong answer.
    std::fs::write(root.join("bin").join(SIBLING), "the link's directory").unwrap();

    let go = temp.path().join("go");
    let mut child = Command::new(root.join("bin/probe"))
        .args(["--exact", "child", "--nocapture", "--test-threads", "1"])
        .env(CHILD, &go)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut reported = Vec::new();
    for line in lines.by_ref() {
        let line = line.unwrap();
        // libtest prints `test child ... ` without a newline, so a report can
        // start partway along a line.
        let first = line.contains("@@before ");
        reported.push(line);
        if first {
            break;
        }
    }
    switch(&root, "0.0.2");
    std::fs::write(&go, b"").unwrap();
    for line in lines {
        reported.push(line.unwrap());
    }
    assert!(child.wait().unwrap().success(), "{reported:#?}");

    let value = |key: &str| {
        reported
            .iter()
            .find_map(|line| line.split_once(key).map(|(_, value)| value))
            .unwrap_or_else(|| panic!("no {key:?} in {reported:#?}"))
            .to_string()
    };
    let one = one.display().to_string();
    assert_eq!(value("@@before "), one);
    assert_eq!(
        value("@@after "),
        one,
        "the running image moved with `current`"
    );
    assert_eq!(value("@@install_dir "), one);
    assert_eq!(
        value("@@sibling "),
        format!("{one}/{SIBLING}"),
        "the sibling came from {}, not the running version",
        two.display()
    );
}

/// Not a test of its own. In the copy, with [`REMOVED`] set, it captures its
/// directory before or after its version is removed, and reports what the
/// lookups say after the removal; anywhere else it does nothing.
#[test]
fn removed_child() {
    use syndeo_shell::supervisor::{capture_install_dir, install_dir, running_image};
    let Ok(setting) = std::env::var(REMOVED) else {
        return;
    };
    let (when, go) = setting.split_once(':').unwrap();
    let early = (when == "early").then(|| capture_install_dir().unwrap());
    if when == "uncaptured" {
        // Asked for before any capture, with the image still in place.
        match install_dir() {
            Ok(dir) => println!("@@uncaptured {}", dir.display()),
            Err(err) => println!("@@uncaptured_error {err:#}"),
        }
    }
    println!("@@waiting");
    let go = PathBuf::from(go);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !go.exists() {
        assert!(
            Instant::now() < deadline,
            "the parent never removed the version"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    if early.is_none() {
        // Only now, after the removal.
        match capture_install_dir() {
            Ok(install) => println!("@@captured {}", install.path().display()),
            Err(err) => println!("@@capture_error {err:#}"),
        }
    }
    match install_dir() {
        Ok(dir) => println!("@@install_dir {}", dir.display()),
        Err(err) => println!("@@install_dir_error {err:#}"),
    }
    match running_image() {
        Ok(image) => println!("@@running_image {}", image.display()),
        Err(err) => println!("@@running_image_error {err:#}"),
    }
}

/// Start a copy through `bin/probe` with [`REMOVED`] set to `when`; once it is
/// waiting, remove 0.0.1 entirely and switch `current` to 0.0.2, as an upgrade
/// does; return what it reported.
#[cfg(target_os = "macos")]
fn run_removed(when: &str) -> (PathBuf, Vec<String>) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("install root");
    let one = std::fs::canonicalize(version(&root, "0.0.1")).unwrap();
    version(&root, "0.0.2");
    switch(&root, "0.0.1");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::os::unix::fs::symlink("../libexec/syndeo/current/probe", root.join("bin/probe")).unwrap();

    let go = temp.path().join("go");
    let mut child = Command::new(root.join("bin/probe"))
        .args([
            "--exact",
            "removed_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(REMOVED, format!("{when}:{}", go.display()))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut reported = Vec::new();
    for line in lines.by_ref() {
        let line = line.unwrap();
        let waiting = line.contains("@@waiting");
        reported.push(line);
        if waiting {
            break;
        }
    }
    std::fs::remove_dir_all(&one).unwrap();
    std::fs::remove_file(root.join("libexec/syndeo/current")).unwrap();
    switch(&root, "0.0.2");
    std::fs::write(&go, b"").unwrap();
    for line in lines {
        reported.push(line.unwrap());
    }
    assert!(child.wait().unwrap().success(), "{reported:#?}");
    (one, reported)
}

#[cfg(target_os = "macos")]
fn reported_value<'a>(reported: &'a [String], key: &str) -> Option<&'a str> {
    reported
        .iter()
        .find_map(|line| line.split_once(key).map(|(_, value)| value))
}

#[cfg(target_os = "macos")]
#[test]
fn a_directory_found_before_the_version_was_removed_is_kept_and_the_kernel_no_longer_says() {
    let (one, reported) = run_removed("early");
    assert_eq!(
        reported_value(&reported, "@@install_dir "),
        Some(one.display().to_string().as_str()),
        "{reported:#?}"
    );
    let image = reported_value(&reported, "@@running_image_error ");
    assert!(
        image.is_some_and(|e| e.contains("proc_pidpath")),
        "asked again, the kernel named an image: {reported:#?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_directory_captured_after_the_version_was_removed_is_an_error_not_a_guess() {
    let (_, reported) = run_removed("late");
    let captured = reported_value(&reported, "@@capture_error ");
    assert!(
        captured.is_some_and(|e| e.contains("proc_pidpath")),
        "the directory was guessed: {reported:#?}"
    );
    let after = reported_value(&reported, "@@install_dir_error ");
    assert!(
        after.is_some_and(|e| e.contains("proc_pidpath")),
        "install_dir does not say the capture failed: {reported:#?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn asked_for_before_capture_it_is_a_mistake_said_and_nothing_is_looked_up() {
    let (_, reported) = run_removed("uncaptured");
    assert_eq!(
        reported_value(&reported, "@@uncaptured_error "),
        Some(syndeo_shell::supervisor::NOT_CAPTURED),
        "{reported:#?}"
    );
    // Had install_dir captured anything, from the kernel or from
    // current_exe, it would have been 0.0.1, while it was there, and the
    // capture after the removal would have returned it. It is an error.
    let captured = reported_value(&reported, "@@capture_error ");
    assert!(
        captured.is_some_and(|e| e.contains("proc_pidpath")),
        "something was captured before the capture: {reported:#?}"
    );
}
