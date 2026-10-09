//! Siblings are found beside the image the kernel is running, not wherever a
//! link pointed when one was asked for.
//!
//! The macOS package starts every command through
//! `bin/<name> -> ../libexec/syndeo/current/<name>`, and an upgrade switches
//! `current`. This test builds that chain around a copy of its own binary,
//! starts the copy through it, switches `current` while the copy is running,
//! and asks the copy where it lives and where its sibling is.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set on the copy: the directory it should wait in, and what to report.
const CHILD: &str = "SYNDEO_INSTALL_DIR_TEST_CHILD";
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
    println!(
        "@@sibling {}",
        syndeo_shell::Supervisor::locate(SIBLING).unwrap().display()
    );
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
