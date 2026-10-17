//! A normal quit keeps what the cache counted. The owner closes the pipe the
//! proxy takes its commands on, as Syndeo.app does when it quits
//! (`syndeo_ipc::quit_child`), and the proxy carries its statistics to disk
//! before it exits; opened again, they are where they were, and from there
//! they only grow. A kill, or an exit that skips writing them, loses every
//! count since the cache last wrote them, which in a short session is all of
//! them.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PROXY: &str = env!("CARGO_BIN_EXE_syndeo-proxy");

/// An origin on this machine that answers every request with a small page.
fn origin() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => request.push(byte[0]),
                    _ => break,
                }
            }
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
        }
    });
    address
}

/// The proxy, as a browser's own is run: announcing where it listens, taking
/// commands on its stdin, and gone with its owner.
fn start(home: &std::path::Path) -> (Child, SocketAddr) {
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
        .env("SYNDEO_HOME", home)
        .env("SYNDEO_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let address = line
        .trim_end()
        .strip_prefix("SYNDEO-PROXY-READY 1 ")
        .unwrap_or_else(|| panic!("not an announcement: {line:?}"))
        .parse()
        .unwrap();
    (child, address)
}

/// GET `url` through the proxy, and the whole answer.
fn get(proxy: SocketAddr, url: &str) -> String {
    let host = url.trim_start_matches("http://").split('/').next().unwrap();
    let mut stream = TcpStream::connect(proxy).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "GET {url} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    answer
}

/// GET the origin's page through the proxy, and require it.
fn fetched(proxy: SocketAddr, page: &str) {
    let answer = get(proxy, page);
    assert!(
        answer.starts_with("HTTP/1.1 200 ") && answer.contains("\r\nok\r\n"),
        "{answer:?}"
    );
}

/// The proxy's counters, from its own page, which it does not count:
/// "requests N  hits N  stale N  misses N  revalidations N (304: N)".
fn counters(proxy: SocketAddr) -> String {
    let page = get(proxy, "http://syndeo.local/stats");
    page.lines()
        .find(|line| line.starts_with("requests "))
        .unwrap_or_else(|| panic!("no counters on the proxy's page: {page:?}"))
        .to_string()
}

fn requests(counters: &str) -> u64 {
    counters
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no request count in {counters:?}"))
}

/// Quit as Syndeo.app does, and require the proxy to have exited by itself.
fn quit(mut child: Child) -> Duration {
    let started = Instant::now();
    match syndeo_ipc::quit_child(&mut child) {
        syndeo_ipc::Quit::Exited(status) => assert!(status.success(), "{status}"),
        other => panic!("the proxy did not exit by itself when its pipe closed: {other:?}"),
    }
    started.elapsed()
}

#[test]
fn a_normal_quit_keeps_the_statistics_and_they_only_grow_across_reopening() {
    let home = tempfile::tempdir().unwrap();
    let origin = origin();
    let page = format!("http://{origin}/page");

    let (child, proxy) = start(home.path());
    for _ in 0..3 {
        fetched(proxy, &page);
    }
    let first = counters(proxy);
    assert!(requests(&first) >= 3, "{first}");
    let took = quit(child);
    eprintln!("quit, first: {took:?}");

    let (child, proxy) = start(home.path());
    let reopened = counters(proxy);
    assert_eq!(
        reopened, first,
        "reopened, the counters are not where the quit left them"
    );
    for _ in 0..2 {
        fetched(proxy, &page);
    }
    let second = counters(proxy);
    assert!(
        requests(&second) >= requests(&first) + 2,
        "{first} then {second}"
    );
    let took = quit(child);
    eprintln!("quit, second: {took:?}");

    let (child, proxy) = start(home.path());
    let again = counters(proxy);
    assert_eq!(
        again, second,
        "reopened again, the counters are not where the quit left them"
    );
    quit(child);
}
