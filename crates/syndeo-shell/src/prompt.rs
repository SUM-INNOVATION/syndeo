//! Asking the human.
//!
//! What matters here is not the widget. It is that the payload the user is
//! shown is byte-for-byte the payload that gets signed, and that a run with
//! nobody to ask declines rather than hangs. [`Prompter`] is the seam: the
//! terminal and the windowed shell implement it differently and the signing
//! path cannot tell which one it is talking to.
//!
//! What they show is not theirs to decide. [`prompt_lines`] is the one place a
//! request becomes text on screen, and both front ends draw exactly its lines,
//! so the terminal and the window cannot disagree about what was asked.

use std::io::{BufRead, IsTerminal, Write};
use syndeo_ipc::protocol::SignaturePurpose;
use unicode_general_category::{get_general_category, GeneralCategory};

/// Read a passphrase.
///
/// A terminal gets a hidden prompt; a pipe gets a line. Scripted runs may set
/// `SYNDEO_PASSPHRASE`. `main` takes it out of the environment before anything
/// else runs (see `syndeo_ipc::startup`), so nothing the shell spawns — the
/// agent especially — can inherit it, and it is handed over here once.
pub fn read_passphrase(label: &str) -> std::io::Result<String> {
    if let Some(value) = syndeo_ipc::startup::take(syndeo_ipc::startup::PASSPHRASE) {
        if !value.is_empty() {
            return Ok(value.to_string());
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

/// The longest origin, in bytes, once it is in canonical form.
pub const MAX_ORIGIN_BYTES: usize = 256;
/// The longest description, in Unicode scalar values.
pub const MAX_DESCRIPTION_CHARS: usize = 200;
/// The largest payload that is shown as text.
pub const MAX_TEXT_PAYLOAD_BYTES: usize = 4 * 1024;
/// The most lines a text payload may take once it is wrapped.
pub const MAX_TEXT_PAYLOAD_LINES: usize = 64;
/// Where a long line of text is wrapped, in characters.
pub const WRAP_WIDTH: usize = 100;
/// The largest payload that is shown as hex.
pub const MAX_HEX_PAYLOAD_BYTES: usize = 512;
/// Bytes per line of a hex dump.
const HEX_BYTES_PER_LINE: usize = 24;

/// Why a request was refused before anyone was asked about it.
///
/// Every one of these is a request the user could not have been shown
/// faithfully, and a request that cannot be shown cannot be consented to. It is
/// refused whole, never trimmed to fit: a signature over bytes the user did not
/// see is not consent, and neither is one over bytes they saw reordered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not `scheme://host`, or something the URL parser would not accept.
    OriginNotAUrl,
    /// Anything other than `http` or `https`.
    OriginScheme,
    OriginWithoutHost,
    /// A user name or password, which would put a second name in front of the
    /// host and invite the user to read the wrong one.
    OriginCredentials,
    OriginQuery,
    OriginFragment,
    /// A path other than `/`. An origin is scheme, host and port; anything
    /// after it is text a site chose for the user to read as reassurance.
    OriginPath,
    /// Written in a form the URL parser would have rewritten: a Unicode host
    /// that becomes punycode, an IPv4 address in hex, a port with a leading
    /// zero. The key is derived from the origin as written, so accepting a
    /// rewritten one would sign with a key the user never had for that site.
    OriginNotCanonical,
    OriginTooLong,
    OriginCharacter(char),
    DescriptionCharacter(char),
    DescriptionTooLong,
    TextPayloadTooLarge,
    TextPayloadTooManyLines,
    HexPayloadTooLarge,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::OriginNotAUrl => f.write_str("the origin is not a scheme://host URL"),
            Refusal::OriginScheme => f.write_str("the origin must be http or https"),
            Refusal::OriginWithoutHost => f.write_str("the origin has no host"),
            Refusal::OriginCredentials => f.write_str("the origin carries a user name or password"),
            Refusal::OriginQuery => f.write_str("the origin carries a query"),
            Refusal::OriginFragment => f.write_str("the origin carries a fragment"),
            Refusal::OriginPath => f.write_str("the origin carries a path"),
            Refusal::OriginNotCanonical => f.write_str(
                "the origin is not written in its canonical form (lowercase ASCII host, \
                 no redundant port digits, nothing after the host but an optional /)",
            ),
            Refusal::OriginTooLong => write!(
                f,
                "the origin is longer than {MAX_ORIGIN_BYTES} bytes in canonical form"
            ),
            Refusal::OriginCharacter(c) => write!(
                f,
                "the origin contains U+{:04X}, which cannot be shown faithfully",
                *c as u32
            ),
            Refusal::DescriptionCharacter(c) => write!(
                f,
                "the description contains U+{:04X}, which cannot be shown faithfully",
                *c as u32
            ),
            Refusal::DescriptionTooLong => write!(
                f,
                "the description is longer than {MAX_DESCRIPTION_CHARS} characters"
            ),
            Refusal::TextPayloadTooLarge => write!(
                f,
                "the payload is more than {MAX_TEXT_PAYLOAD_BYTES} bytes of text, \
                 too much to show in full"
            ),
            Refusal::TextPayloadTooManyLines => write!(
                f,
                "the payload is more than {MAX_TEXT_PAYLOAD_LINES} lines once wrapped, \
                 too much to show in full"
            ),
            Refusal::HexPayloadTooLarge => write!(
                f,
                "the payload is more than {MAX_HEX_PAYLOAD_BYTES} bytes of binary, \
                 too much to show in full"
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// Everything the user must see before a signature exists.
///
/// Carried as one value rather than four arguments so that a front end cannot
/// render three of them and forget the fourth — and the fourth is the payload,
/// which is the one that matters.
///
/// The fields are private and [`SignatureRequest::new`] is the only way to make
/// one, so holding a `SignatureRequest` means holding one that can be shown in
/// full and without anything on screen reordering or hiding it. The origin in
/// here is the canonical one, and it is the one the user sees, the one the
/// confirmation is minted over, and the one the keystore derives from.
#[derive(Debug, Clone)]
pub struct SignatureRequest {
    origin: String,
    purpose: SignaturePurpose,
    /// What the site says it is asking for, in its own words. Untrusted, which
    /// is why it is checked before it is ever put on screen.
    description: String,
    /// The exact bytes that will be signed. Not a summary of them.
    payload: Vec<u8>,
}

impl SignatureRequest {
    /// Check a request and put its origin in canonical form, or refuse it.
    ///
    /// Everything here is decided before anyone is asked, so a request that
    /// could not be shown faithfully never reaches a prompt, a confirmation or
    /// the keystore.
    pub fn new(
        origin: impl AsRef<str>,
        purpose: SignaturePurpose,
        description: impl Into<String>,
        payload: impl Into<Vec<u8>>,
    ) -> Result<Self, Refusal> {
        let origin = canonical_origin(origin.as_ref())?;
        let description = description.into();
        check_description(&description)?;
        let payload = payload.into();
        check_payload(&payload)?;
        Ok(SignatureRequest {
            origin,
            purpose,
            description,
            payload,
        })
    }

    /// The canonical origin: `scheme://host`, with the port only when it is
    /// not the scheme's default.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn purpose(&self) -> SignaturePurpose {
        self.purpose
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The digest shown beside the payload, so a user who cannot read the bytes
    /// can still compare them against something the site quoted.
    pub fn digest(&self) -> String {
        blake3::hash(&self.payload).to_hex().to_string()
    }

    /// The payload as it should appear on screen: as text when it is text, and
    /// as a hex dump when it is not. A signature over bytes the user could not
    /// read is not consent.
    ///
    /// Never truncated. The constructor refused anything that would not fit.
    pub fn rendered_payload(&self) -> Vec<String> {
        match payload_text(&self.payload) {
            Some(text) => wrap_text(text),
            None => hex_lines(&self.payload),
        }
    }
}

/// Every line a signing prompt shows, in order.
///
/// The terminal and the window both draw exactly these, so what one front end
/// shows the other shows too, and a change to what the user sees is a change in
/// one place.
pub fn prompt_lines(request: &SignatureRequest) -> Vec<String> {
    let form = match payload_text(&request.payload) {
        Some(_) => "text",
        None => "hex",
    };
    let mut lines = vec![
        format!("origin   {}", request.origin),
        format!("purpose  {}", request.purpose.as_str()),
        format!("says     {}", request.description),
        String::new(),
        format!("payload  {} bytes, shown as {form}", request.payload.len()),
    ];
    lines.extend(request.rendered_payload());
    lines.push(String::new());
    lines.push(format!("digest   {}", request.digest()));
    lines
}

/// A character that must not reach the screen from text a requester chose.
///
/// Controls move the cursor, clear lines and ring bells. Format characters are
/// invisible by definition, and some of them — the bidirectional overrides and
/// isolates, and the deprecated U+206A..U+206F — reorder the text around them,
/// so what is read is not what is there. The whole General_Category rather
/// than a list of the ones known today, because a list is only as good as the
/// day it was written. The line and paragraph separators are not controls, but
/// terminals and text widgets treat them as line breaks, which is a way to put
/// a line on screen that looks like it came from the prompt itself.
fn is_unshowable(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::Control | GeneralCategory::Format
    ) || c == '\u{2028}'
        || c == '\u{2029}'
}

/// Scheme, host and non-default port, written once in the form the keystore
/// derives from.
///
/// The keystore's `derive::canonical_origin` lowercases the scheme and host,
/// drops a port of exactly `443` for https or `80` for http, and drops
/// everything after the host. It does not know about punycode, numeric IPv4
/// forms or leading zeros in a port. So an origin is only accepted when the
/// URL parser and that function would agree on it: the result here and the
/// keystore's reading of what was typed are the same string, and nobody's
/// identity moves because this check was added.
fn canonical_origin(raw: &str) -> Result<String, Refusal> {
    if let Some(c) = raw.chars().find(|c| is_unshowable(*c)) {
        return Err(Refusal::OriginCharacter(c));
    }

    // The URL parser accepts `https:host` and `https:/host` too. The keystore
    // reads those as something else entirely, so the separator is required as
    // written.
    let Some((scheme, rest)) = raw.split_once("://") else {
        return Err(Refusal::OriginNotAUrl);
    };
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "https" => "443",
        "http" => "80",
        _ => return Err(Refusal::OriginScheme),
    };

    let url = match url::Url::parse(raw) {
        Ok(url) => url,
        Err(url::ParseError::EmptyHost) => return Err(Refusal::OriginWithoutHost),
        Err(_) => return Err(Refusal::OriginNotAUrl),
    };
    let Some(host) = url.host_str() else {
        return Err(Refusal::OriginWithoutHost);
    };

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);

    // An empty user name with an `@` is still a second name in front of the
    // host, and the parser would silently drop it.
    if !url.username().is_empty() || url.password().is_some() || authority.contains('@') {
        return Err(Refusal::OriginCredentials);
    }
    if url.query().is_some() {
        return Err(Refusal::OriginQuery);
    }
    if url.fragment().is_some() {
        return Err(Refusal::OriginFragment);
    }
    if url.path() != "/" {
        return Err(Refusal::OriginPath);
    }

    // What the parser normalised away has to have been nothing but case, a
    // default port and a trailing slash. `/.` parses to a root path, and the
    // parser reads `\` as a slash where the keystore reads it as part of the
    // host; both are refused here or by the host comparison below.
    if !tail.is_empty() && tail != "/" {
        return Err(Refusal::OriginNotCanonical);
    }
    // Split the way the keystore splits, so the host compared is the host it
    // will derive from.
    let (typed_host, typed_port) = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (authority, None),
    };
    if typed_host.to_ascii_lowercase() != host {
        return Err(Refusal::OriginNotCanonical);
    }
    let port = match (typed_port, url.port()) {
        (None, None) => None,
        (Some(typed), None) if typed == default_port => None,
        (Some(typed), Some(port)) if typed == port.to_string() => Some(port),
        _ => return Err(Refusal::OriginNotCanonical),
    };

    let canonical = match port {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    };
    if canonical.len() > MAX_ORIGIN_BYTES {
        return Err(Refusal::OriginTooLong);
    }
    Ok(canonical)
}

/// One line of the requester's own words, and nothing that can move it.
///
/// Tabs and newlines are controls, so they are refused with the rest: a
/// newline in a description is how a site draws a line of its own that looks
/// like part of the prompt.
fn check_description(description: &str) -> Result<(), Refusal> {
    if let Some(c) = description.chars().find(|c| is_unshowable(*c)) {
        return Err(Refusal::DescriptionCharacter(c));
    }
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(Refusal::DescriptionTooLong);
    }
    Ok(())
}

/// Whether the payload fits on screen in full, in the form it will be shown.
fn check_payload(payload: &[u8]) -> Result<(), Refusal> {
    match payload_text(payload) {
        Some(text) => {
            if payload.len() > MAX_TEXT_PAYLOAD_BYTES {
                return Err(Refusal::TextPayloadTooLarge);
            }
            if wrap_text(text).len() > MAX_TEXT_PAYLOAD_LINES {
                return Err(Refusal::TextPayloadTooManyLines);
            }
            Ok(())
        }
        None if payload.len() > MAX_HEX_PAYLOAD_BYTES => Err(Refusal::HexPayloadTooLarge),
        None => Ok(()),
    }
}

/// The payload as text, when it can be shown as text without anything in it
/// changing how the rest reads. Newlines and tabs are layout, not tricks, and
/// are the only controls kept. Anything else goes to hex, which cannot be
/// reordered.
fn payload_text(payload: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(payload).ok()?;
    text.chars()
        .all(|c| c == '\n' || c == '\t' || !is_unshowable(c))
        .then_some(text)
}

/// Split on every newline, keeping blank lines and a trailing empty one, and
/// wrap each line at [`WRAP_WIDTH`] characters. `str::lines` would lose the
/// trailing newline, and slicing by byte would split a character in two.
fn wrap_text(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            lines.push(String::new());
            continue;
        }
        for piece in chars.chunks(WRAP_WIDTH) {
            lines.push(piece.iter().collect());
        }
    }
    lines
}

fn hex_lines(payload: &[u8]) -> Vec<String> {
    payload
        .chunks(HEX_BYTES_PER_LINE)
        .map(|chunk| {
            chunk
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
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
        ask_to_sign(request)
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

/// The signing prompt, on the terminal. It takes a [`SignatureRequest`] rather
/// than its parts, so nothing that skipped the checks can be put in front of
/// the user.
pub fn ask_to_sign(request: &SignatureRequest) -> Decision {
    eprintln!();
    eprintln!("  ┌─ Signature requested ───────────────────────────────");
    for line in prompt_lines(request) {
        eprintln!("  │ {line}");
    }
    eprintln!("  └─────────────────────────────────────────────────────");
    read_yes_no()
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

#[cfg(test)]
mod tests {
    use super::*;

    const PURPOSE: SignaturePurpose = SignaturePurpose::Attestation;

    fn with_origin(origin: &str) -> Result<SignatureRequest, Refusal> {
        SignatureRequest::new(origin, PURPOSE, "Log in", b"payload".to_vec())
    }

    fn with_description(description: &str) -> Result<SignatureRequest, Refusal> {
        SignatureRequest::new("https://wallet.test", PURPOSE, description, b"x".to_vec())
    }

    fn with_payload(payload: Vec<u8>) -> Result<SignatureRequest, Refusal> {
        SignatureRequest::new("https://wallet.test", PURPOSE, "Sign this", payload)
    }

    /// Every Format character named in the review, and the whole deprecated
    /// block U+206A..U+206F, which a hand-kept list of bidi controls misses.
    fn format_characters() -> Vec<char> {
        let mut chars = vec!['\u{202E}', '\u{2066}', '\u{200B}', '\u{FEFF}', '\u{E0001}'];
        chars.extend(('\u{206A}'..='\u{206F}').collect::<Vec<_>>());
        chars
    }

    // ---------------------------------------------------------------- origin

    #[test]
    fn an_origin_that_is_not_http_or_https_is_refused() {
        for origin in [
            "ftp://wallet.test",
            "file://wallet.test",
            "javascript://wallet.test",
            "wss://wallet.test",
        ] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginScheme,
                "{origin}"
            );
        }
        for origin in ["wallet.test", "https:wallet.test", "https:/wallet.test", ""] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginNotAUrl,
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_without_a_host_is_refused() {
        for origin in ["https://", "http://", "https://:8080"] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginWithoutHost,
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_with_a_user_name_or_password_is_refused() {
        for origin in [
            "https://user@wallet.test",
            "https://user:pw@wallet.test",
            "https://:pw@wallet.test",
            "https://@wallet.test",
        ] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginCredentials,
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_with_a_query_is_refused_even_an_empty_one() {
        for origin in [
            "https://wallet.test?",
            "https://wallet.test/?",
            "https://wallet.test?a=1",
        ] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginQuery,
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_with_a_fragment_is_refused_even_an_empty_one() {
        for origin in [
            "https://wallet.test#",
            "https://wallet.test/#",
            "https://wallet.test#top",
        ] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginFragment,
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_with_a_path_is_refused() {
        for origin in [
            "https://wallet.test/login",
            "https://wallet.test//",
            "https://wallet.test/a/",
        ] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginPath,
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_the_parser_would_rewrite_is_refused() {
        for origin in [
            // Unicode host: the parser makes it punycode, the keystore would not.
            "https://bücher.test",
            // Numeric IPv4 forms the parser rewrites to dotted decimal.
            "http://0x7f.0.0.1",
            "http://127.1",
            // A port the parser reads as the default, and the keystore does not.
            "https://wallet.test:0443",
            "https://wallet.test:",
            // Paths that parse to a root the keystore never sees.
            "https://wallet.test/.",
            "https://wallet.test\\",
            "https:///wallet.test",
            // Trailing space the parser trims.
            "https://wallet.test ",
            // IPv6 written longhand, which the parser compresses.
            "http://[0:0::1]",
        ] {
            assert_eq!(
                with_origin(origin).unwrap_err(),
                Refusal::OriginNotCanonical,
                "{origin}"
            );
        }
    }

    #[test]
    fn an_origin_longer_than_the_limit_after_canonicalization_is_refused() {
        // 8 + 248 = 256 bytes: the limit exactly.
        let host = format!("{}.test", "a".repeat(243));
        let at_limit = format!("https://{host}");
        assert_eq!(at_limit.len(), MAX_ORIGIN_BYTES);
        assert_eq!(with_origin(&at_limit).unwrap().origin(), at_limit);

        // Longer as typed, but not once the default port and slash are gone.
        let typed = format!("https://{}:443/", host.to_ascii_uppercase());
        assert!(typed.len() > MAX_ORIGIN_BYTES);
        assert_eq!(with_origin(&typed).unwrap().origin(), at_limit);

        let over = format!("https://a{host}");
        assert_eq!(with_origin(&over).unwrap_err(), Refusal::OriginTooLong);
    }

    #[test]
    fn format_and_control_characters_in_an_origin_are_refused() {
        for c in format_characters()
            .into_iter()
            .chain(['\u{0}', '\t', '\n', '\u{1b}', '\u{7f}', '\u{85}'])
            .chain(['\u{2028}', '\u{2029}'])
        {
            let origin = format!("https://wal{c}let.test");
            assert_eq!(
                with_origin(&origin).unwrap_err(),
                Refusal::OriginCharacter(c),
                "U+{:04X}",
                c as u32
            );
        }
    }

    #[test]
    fn the_whole_deprecated_format_block_is_covered() {
        for c in '\u{206A}'..='\u{206F}' {
            assert_eq!(get_general_category(c), GeneralCategory::Format);
            assert!(is_unshowable(c), "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn origins_are_put_in_canonical_form() {
        for (typed, canonical) in [
            ("https://Wallet.Test", "https://wallet.test"),
            ("HTTPS://WALLET.TEST", "https://wallet.test"),
            ("https://wallet.test:443", "https://wallet.test"),
            ("http://wallet.test:80", "http://wallet.test"),
            ("https://wallet.test/", "https://wallet.test"),
            ("https://Wallet.Test:443/", "https://wallet.test"),
            ("http://example.test:8080", "http://example.test:8080"),
            ("http://example.test:8080/", "http://example.test:8080"),
            ("https://example.test:80", "https://example.test:80"),
            ("http://example.test:443", "http://example.test:443"),
            ("http://127.0.0.1:3000", "http://127.0.0.1:3000"),
            ("http://[::1]:8080/", "http://[::1]:8080"),
            ("https://xn--bcher-kva.test", "https://xn--bcher-kva.test"),
        ] {
            assert_eq!(with_origin(typed).unwrap().origin(), canonical, "{typed}");
        }
    }

    /// The keystore derives an origin's key from what it is sent. Before this
    /// check existed it was sent what the requester typed; now it is sent the
    /// canonical form. The two have to derive the same key, or accepting a
    /// request would move somebody's identity.
    #[test]
    fn the_canonical_origin_derives_the_same_key_as_the_origin_typed() {
        use syndeo_keystore::derive;
        for typed in [
            "https://Wallet.Test:443/",
            "https://wallet.test",
            "https://wallet.test/",
            "HTTPS://WALLET.TEST",
            "http://example.test:8080",
            "http://Example.Test:8080/",
            "http://example.test:80",
            "https://example.test:8443",
            "http://example.test:443",
            "http://127.0.0.1:3000",
            "http://[::1]:8080",
            "http://[::1]",
            "https://xn--bcher-kva.test",
            "https://a-b.c-d.test",
        ] {
            let canonical = with_origin(typed).unwrap();
            assert_eq!(
                derive::canonical_origin(canonical.origin()),
                derive::canonical_origin(typed),
                "{typed}"
            );
            assert_eq!(
                derive::origin_path(canonical.origin()),
                derive::origin_path(typed),
                "{typed}"
            );
            // And the canonical form is already what the keystore would make
            // of it, so it is a fixed point rather than a coincidence.
            assert_eq!(
                derive::canonical_origin(canonical.origin()),
                canonical.origin()
            );
        }
    }

    // ----------------------------------------------------------- description

    #[test]
    fn format_and_control_characters_in_a_description_are_refused() {
        for c in format_characters()
            .into_iter()
            .chain(['\u{0}', '\u{7}', '\u{1b}', '\u{7f}', '\u{85}', '\r'])
            .chain(['\u{2028}', '\u{2029}'])
        {
            let description = format!("Send 10 SUM{c} to alice");
            assert_eq!(
                with_description(&description).unwrap_err(),
                Refusal::DescriptionCharacter(c),
                "U+{:04X}",
                c as u32
            );
        }
    }

    #[test]
    fn a_tab_or_newline_in_a_description_is_refused() {
        assert_eq!(
            with_description("Send\t10 SUM").unwrap_err(),
            Refusal::DescriptionCharacter('\t')
        );
        assert_eq!(
            with_description("Send 10 SUM\norigin   https://bank.test").unwrap_err(),
            Refusal::DescriptionCharacter('\n')
        );
    }

    #[test]
    fn a_description_is_at_most_two_hundred_characters() {
        // Counted in characters, not bytes: 200 multibyte characters are fine.
        let at_limit = "é".repeat(MAX_DESCRIPTION_CHARS);
        assert!(with_description(&at_limit).is_ok());
        let over = "é".repeat(MAX_DESCRIPTION_CHARS + 1);
        assert_eq!(
            with_description(&over).unwrap_err(),
            Refusal::DescriptionTooLong
        );
    }

    // --------------------------------------------------------------- payload

    #[test]
    fn a_text_payload_over_four_kibibytes_is_refused() {
        // 64 lines, the last one a byte longer: 4096 bytes, at both limits.
        let at_limit = format!("{}b", vec!["a".repeat(63); 64].join("\n"));
        assert_eq!(at_limit.len(), MAX_TEXT_PAYLOAD_BYTES);
        let request = with_payload(at_limit.clone().into_bytes()).unwrap();
        assert_eq!(request.rendered_payload().len(), MAX_TEXT_PAYLOAD_LINES);

        let over = format!("{at_limit}c");
        assert_eq!(
            with_payload(over.into_bytes()).unwrap_err(),
            Refusal::TextPayloadTooLarge
        );
    }

    #[test]
    fn a_text_payload_over_sixty_four_wrapped_lines_is_refused() {
        // One line of 4000 characters has no newline in it, and would pass a
        // count of newlines. It wraps to 40 lines; 24 more make 64, the limit.
        let at_limit = format!("{}{}", "x".repeat(40 * WRAP_WIDTH), "\n".repeat(24));
        let request = with_payload(at_limit.clone().into_bytes()).unwrap();
        assert_eq!(request.rendered_payload().len(), MAX_TEXT_PAYLOAD_LINES);

        let over = format!("{at_limit}\n");
        assert!(over.len() <= MAX_TEXT_PAYLOAD_BYTES);
        assert_eq!(
            with_payload(over.into_bytes()).unwrap_err(),
            Refusal::TextPayloadTooManyLines
        );

        // A wrap by character, not byte: 64 lines of 100 two-byte characters
        // would be too large, but 20 of them wrap to exactly 20 lines.
        let wide = "é".repeat(20 * WRAP_WIDTH);
        assert_eq!(
            with_payload(wide.into_bytes())
                .unwrap()
                .rendered_payload()
                .len(),
            20
        );

        // Blank lines count too: 64 of them are 63 newlines.
        let blanks = "\n".repeat(MAX_TEXT_PAYLOAD_LINES - 1);
        assert!(with_payload(blanks.clone().into_bytes()).is_ok());
        assert_eq!(
            with_payload(format!("{blanks}\n").into_bytes()).unwrap_err(),
            Refusal::TextPayloadTooManyLines
        );
    }

    #[test]
    fn a_hex_payload_over_five_hundred_and_twelve_bytes_is_refused() {
        let at_limit = vec![0xffu8; MAX_HEX_PAYLOAD_BYTES];
        let request = with_payload(at_limit).unwrap();
        let shown: usize = request
            .rendered_payload()
            .iter()
            .map(|line| line.split(' ').count())
            .sum();
        assert_eq!(shown, MAX_HEX_PAYLOAD_BYTES, "every byte is on screen");

        assert_eq!(
            with_payload(vec![0xffu8; MAX_HEX_PAYLOAD_BYTES + 1]).unwrap_err(),
            Refusal::HexPayloadTooLarge
        );
    }

    // ------------------------------------------------------------- rendering

    #[test]
    fn a_multibyte_character_across_byte_one_hundred_renders_without_panicking() {
        // 99 ASCII bytes, then a three-byte character occupying bytes 99..102.
        let line = format!("{}€{}", "a".repeat(99), "b".repeat(50));
        assert!(!line.is_char_boundary(100));
        let request = with_payload(line.clone().into_bytes()).unwrap();
        let rendered = request.rendered_payload();
        assert_eq!(rendered[0].chars().count(), WRAP_WIDTH);
        assert!(rendered[0].ends_with('€'));
        assert_eq!(rendered.concat(), line);
        // And the whole prompt, which is what the terminal prints.
        assert!(prompt_lines(&request).iter().any(|l| l == &rendered[0]));
    }

    #[test]
    fn a_long_line_wraps_into_pieces_that_are_the_original() {
        let line: String = (0..300)
            .map(|i| char::from_u32('a' as u32 + (i % 26)).unwrap())
            .collect();
        let rendered = with_payload(line.clone().into_bytes())
            .unwrap()
            .rendered_payload();
        assert_eq!(rendered.len(), 3);
        assert!(rendered.iter().all(|l| l.chars().count() == WRAP_WIDTH));
        assert_eq!(rendered.concat(), line);
    }

    #[test]
    fn blank_lines_and_a_trailing_newline_are_shown() {
        let rendered = with_payload(b"first\n\n\nlast\n".to_vec())
            .unwrap()
            .rendered_payload();
        assert_eq!(rendered, vec!["first", "", "", "last", ""]);
        assert_eq!(rendered.join("\n"), "first\n\n\nlast\n");

        // Two payloads that differ only by the trailing newline look different.
        assert_ne!(
            with_payload(b"last".to_vec()).unwrap().rendered_payload(),
            rendered[3..].to_vec()
        );
    }

    #[test]
    fn a_payload_with_a_bidi_override_is_shown_as_hex() {
        let payload = "pay alice\u{202E}01 MUS".as_bytes().to_vec();
        let request = with_payload(payload.clone()).unwrap();
        let rendered = request.rendered_payload();
        assert!(rendered.iter().all(|l| l.is_ascii()));
        let bytes: Vec<u8> = rendered
            .join(" ")
            .split(' ')
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect();
        assert_eq!(bytes, payload);
        assert!(prompt_lines(&request)
            .iter()
            .any(|l| l.contains("shown as hex")));
    }

    #[test]
    fn payloads_with_other_unshowable_characters_are_shown_as_hex() {
        for c in format_characters()
            .into_iter()
            .chain(['\r', '\u{1b}', '\u{0}', '\u{2028}', '\u{2029}'])
        {
            let request = with_payload(format!("a{c}b").into_bytes()).unwrap();
            assert!(
                payload_text(request.payload()).is_none(),
                "U+{:04X}",
                c as u32
            );
        }
        // Tabs and newlines are layout, and stay text.
        assert!(payload_text(b"a\tb\nc").is_some());
        // So is invalid UTF-8.
        assert!(payload_text(&[0x61, 0xff]).is_none());
    }

    #[test]
    fn the_prompt_shows_every_field_and_the_canonical_origin() {
        let request = SignatureRequest::new(
            "https://Wallet.Test:443/",
            SignaturePurpose::ChainTransaction,
            "Send 10 SUM to alice",
            b"transfer 10 SUM to alice".to_vec(),
        )
        .unwrap();
        let lines = prompt_lines(&request);
        assert_eq!(
            lines,
            vec![
                "origin   https://wallet.test".to_string(),
                "purpose  chain transaction".to_string(),
                "says     Send 10 SUM to alice".to_string(),
                String::new(),
                "payload  24 bytes, shown as text".to_string(),
                "transfer 10 SUM to alice".to_string(),
                String::new(),
                format!("digest   {}", request.digest()),
            ]
        );
    }

    /// Signing validation and terminal display are separate, and the first is
    /// unchanged by the second: every character the display filter replaces
    /// or removes is one signing already refuses, or ordinary whitespace.
    #[test]
    fn signing_refuses_at_least_everything_the_display_filter_cleans() {
        for c in (0..=0x10FFFFu32).filter_map(char::from_u32) {
            let text = c.to_string();
            let cleaned = syndeo_dom::terminal::single_line(&text);
            if cleaned != text && !matches!(c, ' ') {
                assert!(
                    is_unshowable(c),
                    "U+{:04X} is cleaned for display but accepted for signing",
                    c as u32
                );
            }
        }
        // A newline in a description is still refused, not cleaned.
        for c in [
            '\n', '\r', '\t', '\u{2028}', '\u{2029}', '\u{1b}', '\u{202e}',
        ] {
            assert!(is_unshowable(c), "U+{:04X}", c as u32);
        }
    }
}
