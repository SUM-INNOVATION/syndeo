//! A proxy that syndeo-webkit started goes when syndeo-webkit goes, however it
//! goes; a proxy anybody else started stays up.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PROXY: &str = env!("CARGO_BIN_EXE_syndeo-proxy");

/// A free port below the ephemeral range. Polling an ephemeral port that is
/// not yet listening can be given that same port as its source, which is a TCP
/// connection to itself and looks like the proxy answering.
///
/// Each call starts further along, because the tests run at once in one
/// process and would otherwise all be handed the same port.
fn free_address() -> SocketAddr {
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let offset = CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) * 100;
    (20000..30000)
        .map(|port| SocketAddr::from(([127, 0, 0, 1], port)))
        .skip((std::process::id() as usize % 5000 + offset) % 9000)
        .find(|address| TcpListener::bind(address).is_ok())
        .expect("a free port below the ephemeral range")
}

fn proxy(home: &std::path::Path, address: SocketAddr, watch: bool, stdin: Stdio) -> Child {
    let mut command = Command::new(PROXY);
    command
        .args(["run", "--dns", "system", "--listen", &address.to_string()])
        .env("SYNDEO_HOME", home)
        .env("SYNDEO_LOG", "warn")
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if watch {
        command.arg("--exit-with-parent");
    }
    command.spawn().unwrap()
}

fn wait_until_listening(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect(address).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the proxy never listened on {address}");
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn gone_within(pid: u32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn it_exits_when_the_pipe_from_its_parent_closes() {
    let home = tempfile::tempdir().unwrap();
    let address = free_address();
    let mut child = proxy(home.path(), address, true, Stdio::piped());
    wait_until_listening(address);

    drop(child.stdin.take());

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the proxy outlived its parent");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success());
}

/// The parent dies by SIGKILL, which runs no destructors and closes nothing
/// on purpose: the kernel closing its end of the pipe is the whole signal.
#[test]
fn it_exits_when_its_parent_is_killed() {
    if let Ok(address) = std::env::var("SYNDEO_PROXY_PARENT") {
        // The parent: start the proxy, say its pid, and wait to be killed.
        let home = std::env::var("SYNDEO_HOME").unwrap();
        let mut child = proxy(
            std::path::Path::new(&home),
            address.parse().unwrap(),
            true,
            Stdio::piped(),
        );
        // After a marker, because libtest prints its own "test ... " prefix on
        // the same line.
        println!("proxy-pid={}", child.id());
        // `wait` would close the child's stdin first, which is exactly the
        // signal under test, so the pipe is kept here and outlives the wait.
        let _pipe = child.stdin.take();
        // Blocks until the test kills this process, which is the point.
        let _ = child.wait();
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let address = free_address();
    let mut parent = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "it_exits_when_its_parent_is_killed",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("SYNDEO_PROXY_PARENT", address.to_string())
        .env("SYNDEO_HOME", home.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(parent.stdout.take().unwrap()).lines();
    let pid: u32 = lines
        .by_ref()
        .map_while(Result::ok)
        .find_map(|line| {
            let (_, pid) = line.split_once("proxy-pid=")?;
            pid.trim().parse().ok()
        })
        .expect("the parent reported the proxy's pid");
    wait_until_listening(address);
    assert!(alive(pid));

    parent.kill().unwrap();
    parent.wait().unwrap();

    assert!(
        gone_within(pid, Duration::from_secs(5)),
        "the proxy outlived a parent that was killed"
    );
}

#[test]
fn without_the_flag_a_closed_stdin_is_not_a_reason_to_exit() {
    let home = tempfile::tempdir().unwrap();
    let address = free_address();
    let mut child = proxy(home.path(), address, false, Stdio::null());
    wait_until_listening(address);

    std::thread::sleep(Duration::from_secs(2));

    assert!(
        child.try_wait().unwrap().is_none(),
        "a proxy started like `nohup syndeo-proxy run` exited on its own"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Taking commands, the proxy answers `clear-cache` on its stdout after its
/// announcement, and still goes when the pipe from its parent closes.
#[test]
fn taking_commands_it_clears_when_asked_and_exits_when_the_pipe_closes() {
    use std::io::Write;
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(PROXY)
        .args([
            "run",
            "--dns",
            "system",
            "--listen",
            "127.0.0.1:0",
            "--announce",
            "--exit-with-parent",
            "--control-stdin",
        ])
        .env("SYNDEO_HOME", home.path())
        .env("SYNDEO_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(line.starts_with("SYNDEO-PROXY-READY 1 "), "{line:?}");

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"clear-cache\nsomething else\n").unwrap();
    stdin.flush().unwrap();
    for expected in ["SYNDEO-PROXY-CLEARED 1 0\n", "SYNDEO-PROXY-UNKNOWN 1\n"] {
        line.clear();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, expected);
    }

    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the proxy outlived the pipe from its parent"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success());
}
