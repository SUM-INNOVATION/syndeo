//! `syndeo browse`, end to end: a page served by a local origin, fetched
//! through the real network process, printed by the real binary. Nothing the
//! page supplies reaches the terminal as a control or format character, and
//! `--json` is left exactly as serde_json writes it.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;

const PAGE: &str = "<!doctype html><html><head>\
    <title>Ti\u{1b}]52;c;cGF3bmVk\u{7}tle\u{202e}</title>\
    <script src=\"/s\u{1b}[2J.js\"></script></head><body>\
    <p>prose\u{1b}[1A\u{9b}31m more</p>\
    <a href=\"/l\u{1b}]8;;https://evil.test\u{7}\">li\u{1b}[31mnk</a>\
    <form action=\"/a\u{1b}[H\" method=\"post\"><input name=\"n\u{1b}[Kame\"></form>\
    </body></html>";

/// An origin that answers every request with the hostile page.
fn origin() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                    PAGE.len()
                )
                .as_bytes(),
            );
        }
    });
    format!("http://{address}/page")
}

/// The network process the shell starts sits beside the shell binary. A
/// workspace test run builds it; a run of this package alone may not have.
fn network_process_is_built() -> bool {
    let shell = std::path::Path::new(env!("CARGO_BIN_EXE_syndeo"));
    let built = shell.with_file_name("syndeo-net").exists();
    if !built {
        assert!(
            std::env::var_os("CI").is_none(),
            "syndeo-net was not built beside syndeo in CI"
        );
        eprintln!("skipped: build syndeo-net (cargo test --workspace) to run this");
    }
    built
}

fn browse(home: &std::path::Path, url: &str, flags: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_syndeo"))
        .args(["--home"])
        .arg(home)
        .args(["--dns", "system", "browse", url])
        .args(flags)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "browse failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn printable(text: &str) -> Result<(), char> {
    match text.chars().find(|&c| {
        (c.is_control() && c != '\n')
            || matches!(
                unicode_general_category::get_general_category(c),
                unicode_general_category::GeneralCategory::Format
            )
            || c == '\u{2028}'
            || c == '\u{2029}'
    }) {
        Some(c) => Err(c),
        None => Ok(()),
    }
}

#[test]
fn a_hostile_page_prints_nothing_that_is_not_text() {
    if !network_process_is_built() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let url = origin();
    for flags in [&[][..], &["--full"]] {
        let shown = browse(home.path(), &url, flags);
        if let Err(c) = printable(&shown) {
            panic!("U+{:04X} reached the terminal:\n{shown}", c as u32);
        }
        assert!(shown.contains("# Ti]52;c;cGF3bmVktle"), "{shown}");
    }
}

#[test]
fn json_is_left_as_serde_json_writes_it() {
    if !network_process_is_built() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let shown = browse(home.path(), &origin(), &["--json"]);
    // JSON escapes controls itself; the page's title arrives intact, escaped.
    assert!(
        shown.contains(r#"Ti\u001b]52;c;cGF3bmVk\u0007tle"#),
        "{shown}"
    );
    let value: serde_json::Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(
        value["title"].as_str().unwrap(),
        "Ti\u{1b}]52;c;cGF3bmVk\u{7}tle\u{202e}"
    );
    assert_eq!(
        shown.trim_end(),
        serde_json::to_string_pretty(&value).unwrap()
    );
}
