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

// ------------------------------------------------ the credential on stdin

const TOKEN: [u8; 32] = [0x5a; 32];

fn frame(token: &[u8; 32]) -> Vec<u8> {
    let mut frame = b"SYA1".to_vec();
    frame.extend_from_slice(token);
    frame.push(b'\n');
    frame
}

fn credential(token: &[u8; 32]) -> String {
    use base64::Engine;
    let password: String = token.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("syndeo:{password}"))
    )
}

fn stats_through(address: SocketAddr, credential: Option<&str>) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    let auth = credential
        .map(|c| format!("Proxy-Authorization: {c}\r\n"))
        .unwrap_or_default();
    stream
        .write_all(
            format!(
                "GET http://syndeo.local/stats HTTP/1.1\r\nHost: syndeo.local\r\n{auth}Connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn a_credential_handed_over_on_stdin_is_required_and_never_printed() {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(PROXY)
        .args([
            "run",
            "--listen",
            "127.0.0.1:0",
            "--announce",
            "--auth-stdin",
            "--exit-with-parent",
            "--dns",
            "system",
        ])
        .env("SYNDEO_HOME", home.path())
        // As much logging as there is, so a leak would show.
        .env("SYNDEO_LOG", "trace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = collect(child.stderr.take().unwrap());
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&frame(&TOKEN)).unwrap();
    stdin.flush().unwrap();

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let address: SocketAddr = line
        .trim_end()
        .strip_prefix("SYNDEO-PROXY-READY 1 ")
        .unwrap()
        .parse()
        .unwrap();
    let rest = collect(stdout);

    assert!(stats_through(address, None).starts_with("HTTP/1.1 407"));
    assert!(stats_through(address, Some(&credential(&[0xa5; 32]))).starts_with("HTTP/1.1 407"));
    assert!(stats_through(address, Some(&credential(&TOKEN))).starts_with("HTTP/1.1 200"));

    // The parent watch still owns stdin after the frame: closing it ends the
    // proxy.
    drop(stdin);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the proxy outlived its parent's pipe"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");

    let mut printed = rest.recv_timeout(Duration::from_secs(5)).unwrap();
    printed.extend(stderr.recv_timeout(Duration::from_secs(5)).unwrap());
    let printed = String::from_utf8_lossy(&printed);
    let hex: String = TOKEN.iter().map(|b| format!("{b:02x}")).collect();
    for secret in [hex.as_str(), &credential(&TOKEN)[6..]] {
        assert!(!printed.contains(secret), "the credential was printed");
    }
    assert!(printed.contains("proxy up"), "trace logging was not on");
}

#[test]
fn a_bad_credential_frame_ends_the_proxy_with_status_2_and_says_nothing_of_it() {
    let good = frame(&TOKEN);
    let mut bad_magic = good.clone();
    bad_magic[0] = b'X';
    let mut no_newline = good.clone();
    no_newline[36] = b'!';
    for input in [good[..20].to_vec(), bad_magic, no_newline] {
        let home = tempfile::tempdir().unwrap();
        let mut child = Command::new(PROXY)
            .args([
                "run",
                "--listen",
                "127.0.0.1:0",
                "--auth-stdin",
                "--dns",
                "system",
            ])
            .env("SYNDEO_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = collect(child.stderr.take().unwrap());
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&input).unwrap();
        drop(stdin);
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(2), "{input:?}");
        let said = String::from_utf8_lossy(&stderr.recv_timeout(Duration::from_secs(5)).unwrap())
            .to_string();
        assert!(said.contains("credential frame"), "{said}");
        assert!(!said.contains("ZZZZ") && !said.contains("5a5a"), "{said}");
    }
}
