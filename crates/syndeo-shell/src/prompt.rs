//! Asking the human.
//!
//! What matters here is not the widget. It is that the payload the user is
//! shown is byte-for-byte the payload that gets signed, and that a run with
//! nobody to ask declines rather than hangs. [`Prompter`] is the seam: the
//! terminal and the windowed shell implement it differently and the signing
//! path cannot tell which one it is talking to.

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

/// Everything the user must see before a signature exists.
///
/// Carried as one value rather than four arguments so that a front end cannot
/// render three of them and forget the fourth — and the fourth is the payload,
/// which is the one that matters.
#[derive(Debug, Clone)]
pub struct SignatureRequest {
    pub origin: String,
    pub purpose: SignaturePurpose,
    /// What the site says it is asking for, in its own words. Untrusted.
    pub description: String,
    /// The exact bytes that will be signed. Not a summary of them.
    pub payload: Vec<u8>,
}

impl SignatureRequest {
    /// The digest shown beside the payload, so a user who cannot read the bytes
    /// can still compare them against something the site quoted.
    pub fn digest(&self) -> String {
        blake3::hash(&self.payload).to_hex().to_string()
    }

    /// The payload as it should appear on screen: as text when it is text, and
    /// as a hex dump when it is not. A signature over bytes the user could not
    /// read is not consent.
    pub fn rendered_payload(&self) -> Vec<String> {
        render_payload(&self.payload)
    }
}

/// How a front end asks a human.
///
/// Implementations must never block forever: a run with nobody to ask returns
/// [`Decision::No`], which is why [`NonInteractive`] exists as a real type
/// rather than as a flag somebody could forget to check.
pub trait Prompter: Send + Sync {
    /// Show a payload and ask whether to sign it.
    fn ask_to_sign(&self, request: &SignatureRequest) -> Decision;

    /// Ask a yes/no question.
    fn ask(&self, title: &str, detail: &str) -> Decision;

    /// Read a passphrase, to unseal the keystore.
    fn read_passphrase(&self, label: &str) -> std::io::Result<String>;

    /// Whether there is anyone to ask at all.
    fn is_interactive(&self) -> bool {
        true
    }
}

/// The terminal.
#[derive(Debug, Default, Clone, Copy)]
pub struct TerminalPrompter;

impl Prompter for TerminalPrompter {
    fn ask_to_sign(&self, request: &SignatureRequest) -> Decision {
        ask_to_sign(
            &request.origin,
            request.purpose,
            &request.description,
            &request.payload,
        )
    }

    fn ask(&self, title: &str, detail: &str) -> Decision {
        ask(title, detail)
    }

    fn read_passphrase(&self, label: &str) -> std::io::Result<String> {
        read_passphrase(label)
    }

    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }
}

/// Nobody is there. Everything is declined, immediately.
///
/// The point of this being a type is that an agent cannot hang waiting on a
/// human who is not present, and the way that property is guaranteed is by
/// there being no code path that waits.
#[derive(Debug, Default, Clone, Copy)]
pub struct NonInteractive;

impl Prompter for NonInteractive {
    fn ask_to_sign(&self, _request: &SignatureRequest) -> Decision {
        Decision::No
    }

    fn ask(&self, _title: &str, _detail: &str) -> Decision {
        Decision::No
    }

    fn read_passphrase(&self, _label: &str) -> std::io::Result<String> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "this run has nobody to ask for a passphrase",
        ))
    }

    fn is_interactive(&self) -> bool {
        false
    }
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
pub fn render_payload(payload: &[u8]) -> Vec<String> {
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
