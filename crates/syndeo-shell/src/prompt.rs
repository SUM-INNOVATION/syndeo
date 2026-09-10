//! The confirmation dialog.
//!
//! A terminal for now; the same three questions will be asked by the windowed
//! shell later. What matters is not the widget but that the payload the user is
//! shown is byte-for-byte the payload that gets signed.

use std::io::{BufRead, IsTerminal, Write};
use syndeo_ipc::protocol::SignaturePurpose;

/// Read a passphrase.
///
/// A terminal gets a hidden prompt; a pipe gets a line. Scripted runs may set
/// `SYNDEO_PASSPHRASE`, which is read once and then removed from this process so
/// it cannot be inherited by anything the shell spawns — the agent especially.
pub fn read_passphrase(label: &str) -> std::io::Result<String> {
    if let Ok(value) = std::env::var("SYNDEO_PASSPHRASE") {
        std::env::remove_var("SYNDEO_PASSPHRASE");
        if !value.is_empty() {
            return Ok(value);
        }
    }
    if std::io::stdin().is_terminal() {
        return rpassword::prompt_password(label);
    }
    eprint!("{label}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Yes,
    No,
}

pub fn ask(title: &str, detail: &str) -> Decision {
    eprintln!();
    eprintln!("  {title}");
    if !detail.is_empty() {
        eprintln!("  {detail}");
    }
    read_yes_no()
}

pub fn ask_to_sign(
    origin: &str,
    purpose: SignaturePurpose,
    description: &str,
    payload: &[u8],
) -> Decision {
    eprintln!();
    eprintln!("  ┌─ Signature requested ───────────────────────────────");
    eprintln!("  │ origin   {origin}");
    eprintln!("  │ purpose  {}", purpose.as_str());
    eprintln!("  │ says     {description}");
    eprintln!("  │");
    for line in render_payload(payload) {
        eprintln!("  │ {line}");
    }
    eprintln!("  │");
    eprintln!("  │ digest   {}", blake3::hash(payload).to_hex());
    eprintln!("  └─────────────────────────────────────────────────────");
    read_yes_no()
}

/// Show the bytes as text when they are text, and as a hex dump when they are
/// not. A signature over bytes the user could not read is not consent.
fn render_payload(payload: &[u8]) -> Vec<String> {
    const LIMIT: usize = 512;
    match std::str::from_utf8(payload) {
        Ok(text) if text.chars().all(|c| !c.is_control() || c == '\n' || c == '\t') => {
            let mut lines: Vec<String> = text
                .lines()
                .take(16)
                .map(|l| {
                    if l.len() > 100 {
                        format!("{}…", &l[..100])
                    } else {
                        l.to_string()
                    }
                })
                .collect();
            if text.lines().count() > 16 {
                lines.push(format!("… {} more lines", text.lines().count() - 16));
            }
            lines
        }
        _ => {
            let shown = &payload[..payload.len().min(LIMIT)];
            let mut lines: Vec<String> = shown
                .chunks(24)
                .map(|chunk| {
                    chunk
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            if payload.len() > LIMIT {
                lines.push(format!("… {} more bytes", payload.len() - LIMIT));
            }
            lines
        }
    }
}

fn read_yes_no() -> Decision {
    if !std::io::stdin().is_terminal() {
        eprintln!("  (no terminal to ask on — declining)");
        return Decision::No;
    }
    loop {
        eprint!("  Approve? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            return Decision::No;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Decision::Yes,
            "" | "n" | "no" => return Decision::No,
            _ => continue,
        }
    }
}
