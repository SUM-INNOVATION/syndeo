//! `run --announce` says where the proxy listens, once it does, and says
//! nothing else on stdout. Without it, nothing about a plain `run` changes.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;

const PROXY: &str = env!("CARGO_BIN_EXE_syndeo-proxy");

/// Everything a pipe produces until it closes, collected on its own thread.
fn collect(mut pipe: impl Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let _ = pipe.read_to_end(&mut all);
        let _ = tx.send(all);
    });
    rx
}

#[test]
fn the_record_comes_first_names_a_real_port_and_is_all_that_stdout_carries() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(PROXY)
        .args([
            "run",
            "--listen",
            "127.0.0.1:0",
            "--announce",
            "--dns",
            "system",
        ])
        .env("SYNDEO_HOME", home.path())
        // Logging on, so a log line on stdout would show.
        .env("SYNDEO_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = collect(child.stderr.take().unwrap());
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        tx.send(line).unwrap();
        let mut rest = Vec::new();
        let _ = stdout.read_to_end(&mut rest);
        rest
    });
    let line = rx
        .recv_timeout(Duration::from_secs(20))
        .expect("the proxy never announced");
    let address: SocketAddr = line
        .strip_prefix("SYNDEO-PROXY-READY 1 ")
        .and_then(|rest| rest.strip_suffix('\n'))
        .expect("the record's shape")
        .parse()
        .unwrap();
    assert!(address.ip().is_loopback());
    assert_ne!(
        address.port(),
        0,
        "it must name the port it got, not the one asked for"
    );

    // It is listening there, and a request through it is logged — elsewhere.
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .write_all(b"GET http://syndeo.local/stats HTTP/1.1\r\nHost: syndeo.local\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");

    child.kill().unwrap();
    child.wait().unwrap();
    let rest = reader.join().unwrap();
    assert!(
        rest.is_empty(),
        "stdout carried more than the record: {:?}",
        String::from_utf8_lossy(&rest)
    );
    let logs = stderr.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        String::from_utf8_lossy(&logs).contains("proxy up"),
        "the logs should have gone to stderr"
    );
}

#[test]
fn without_announce_a_plain_run_logs_to_stdout_as_before() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(PROXY)
        .args(["run", "--listen", "127.0.0.1:0", "--dns", "system"])
        .env("SYNDEO_HOME", home.path())
        .env("SYNDEO_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while stdout.read_line(&mut line).unwrap_or(0) > 0 {
            if line.contains("proxy up") {
                let _ = tx.send(line.clone());
                return;
            }
            assert!(!line.starts_with("SYNDEO-PROXY-READY"), "{line}");
            line.clear();
        }
    });
    let found = rx.recv_timeout(Duration::from_secs(20));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(found.is_ok(), "a plain run should still log to stdout");
}
