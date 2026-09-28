//! Reading a passphrase.
//!
//! A terminal gets a hidden prompt. Everything else — a pipe, a test harness, a
//! provisioning script — gets a line from stdin, because failing with "device
//! not configured" is not a security property, it is just a broken setup path.

use std::io::{BufRead, IsTerminal};
use zeroize::Zeroizing;

/// For scripted enrolment only. Documented, deliberate, and read once: `main`
/// takes it out of the environment before anything else runs, and it is
/// handed to these functions as `scripted`.
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

/// Read one passphrase: `scripted`, if a script supplied one, or else from
/// the person. Zeroed when dropped.
pub fn read(
    prompt: &str,
    scripted: Option<Zeroizing<String>>,
) -> Result<Zeroizing<String>, PassphraseError> {
    if let Some(value) = scripted {
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
    // Cut in place: a trimmed copy would be one more buffer to wipe.
    let kept = line.trim_end_matches(['\n', '\r']).len();
    line.truncate(kept);
    if line.is_empty() {
        return Err(PassphraseError::Empty);
    }
    Ok(line)
}

/// Read a new passphrase, twice, with a floor on length: or `scripted`, once,
/// if a script supplied one. Zeroed when dropped.
pub fn read_new(scripted: Option<Zeroizing<String>>) -> Result<Zeroizing<String>, PassphraseError> {
    if let Some(value) = scripted {
        if value.chars().count() < MINIMUM {
            return Err(PassphraseError::TooShort(MINIMUM));
        }
        return Ok(value);
    }
    let first = read("Passphrase: ", None)?;
    if first.chars().count() < MINIMUM {
        return Err(PassphraseError::TooShort(MINIMUM));
    }
    let again = read("Again: ", None)?;
    if *first != *again {
        return Err(PassphraseError::Mismatch);
    }
    Ok(first)
}
