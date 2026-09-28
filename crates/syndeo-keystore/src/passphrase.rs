//! Reading a passphrase.
//!
//! A terminal gets a hidden prompt. Everything else — a pipe, a test harness, a
//! provisioning script — gets a line from stdin, because failing with "device
//! not configured" is not a security property, it is just a broken setup path.

use std::io::{BufRead, IsTerminal};
use zeroize::Zeroizing;

/// For scripted enrolment only. Documented, deliberate, and read once: `main`
/// takes it out of the environment before anything else runs, and it is
/// handed over here through `syndeo_ipc::startup`.
pub const ENV_VAR: &str = syndeo_ipc::startup::PASSPHRASE;

#[derive(Debug, thiserror::Error)]
pub enum PassphraseError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no passphrase was supplied")]
    Empty,
    #[error("the passphrases do not match")]
    Mismatch,
    #[error("use at least {0} characters")]
    TooShort(usize),
}

pub const MINIMUM: usize = 8;

/// Read one passphrase. Zeroed when dropped.
pub fn read(prompt: &str) -> Result<Zeroizing<String>, PassphraseError> {
    if let Some(value) = syndeo_ipc::startup::take(ENV_VAR) {
        if !value.is_empty() {
            return Ok(value);
        }
    }
    if std::io::stdin().is_terminal() {
        return Ok(Zeroizing::new(rpassword::prompt_password(prompt)?));
    }
    eprint!("{prompt}");
    let mut line = Zeroizing::new(String::new());
    std::io::stdin().lock().read_line(&mut line)?;
    let line = Zeroizing::new(line.trim_end_matches(['\n', '\r']).to_string());
    if line.is_empty() {
        return Err(PassphraseError::Empty);
    }
    Ok(line)
}

/// Read a new passphrase, twice, with a floor on length. Zeroed when dropped.
pub fn read_new() -> Result<Zeroizing<String>, PassphraseError> {
    if let Some(value) = syndeo_ipc::startup::take(ENV_VAR) {
        if value.chars().count() < MINIMUM {
            return Err(PassphraseError::TooShort(MINIMUM));
        }
        return Ok(value);
    }
    let first = read("Passphrase: ")?;
    if first.chars().count() < MINIMUM {
        return Err(PassphraseError::TooShort(MINIMUM));
    }
    let again = read("Again: ")?;
    if *first != *again {
        return Err(PassphraseError::Mismatch);
    }
    Ok(first)
}
