//! Hosts whose browser refused this proxy's certificate the way macOS 15 and
//! earlier refuse it for the sites on their built-in HSTS preload list.
//!
//! For those sites macOS accepts only a publicly trusted certificate,
//! whatever the application answers (#58). Measured on hosted macOS 14 and
//! 15 runners: WebKit's networking closes the tunnel after this proxy's
//! first flight, tries again, and then once more with a legacy ClientHello
//! that has no `signature_algorithms` extension, which rustls refuses as
//! `PeerIncompatible::SignatureAlgorithmsExtensionRequired`. That last step
//! came only for the refused sites (github.com, paypal.com, wikipedia.org,
//! go.dev, www.dropbox.com, and www.google-analytics.com loaded by a page),
//! never for another host and never on macOS 26. A closed tunnel alone is
//! not the sign: browsers close tunnels for many reasons.
//!
//! The owner, Syndeo.app, reads each such host once, as a line on this
//! proxy's stdout ([`line`]), and decides what to do with it: it sends a host
//! it had itself vouched for directly. Nothing here changes how this proxy
//! answers anyone.

use std::collections::HashSet;
use std::sync::Mutex;

/// What a line saying a host was refused starts with.
pub const REFUSED: &str = "SYNDEO-PROXY-REFUSED 1";

/// How many hosts are said in one run, at most.
const CAPACITY: usize = 4096;

/// The line that says `host` was refused.
pub fn line(host: &str) -> String {
    format!("{REFUSED} {host}\n")
}

/// Whether a failed TLS handshake is that last step: a ClientHello with no
/// `signature_algorithms`, which no browser sends on its first try.
pub fn is_refusal(err: &std::io::Error) -> bool {
    matches!(
        err.get_ref()
            .and_then(|e| e.downcast_ref::<rustls::Error>()),
        Some(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::SignatureAlgorithmsExtensionRequired
        ))
    )
}

/// How a refused host is said.
type Say = Box<dyn Fn(&str) + Send + Sync>;

/// Says each refused host once, to whoever started this proxy, if anyone.
pub struct Refusals {
    say: Option<Say>,
    said: Mutex<HashSet<String>>,
}

impl Refusals {
    /// Says nothing: no owner is listening.
    pub fn silent() -> Self {
        Refusals {
            say: None,
            said: Mutex::new(HashSet::new()),
        }
    }

    /// Says each refused host with `say`.
    pub fn saying(say: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Refusals {
            say: Some(Box::new(say)),
            said: Mutex::new(HashSet::new()),
        }
    }

    /// A TLS handshake in a tunnel to `host` failed with `err`. If that is
    /// the refusal, say `host`, the first time only; whether it was.
    pub fn handshake_failed(&self, host: &str, err: &std::io::Error) -> bool {
        if !is_refusal(err) {
            return false;
        }
        let host = host.to_ascii_lowercase();
        // A name, and nothing that could break the line it goes in.
        let name = !host.is_empty()
            && host.len() <= 253
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if name {
            if let Some(say) = &self.say {
                let mut said = self.said.lock().unwrap_or_else(|e| e.into_inner());
                if said.len() < CAPACITY && said.insert(host.clone()) {
                    say(&host);
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn refusal() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            rustls::Error::PeerIncompatible(
                rustls::PeerIncompatible::SignatureAlgorithmsExtensionRequired,
            ),
        )
    }

    #[test]
    fn only_a_hello_without_signature_algorithms_is_the_refusal() {
        assert!(is_refusal(&refusal()));
        for other in [
            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "tls handshake eof"),
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                rustls::Error::AlertReceived(rustls::AlertDescription::CertificateUnknown),
            ),
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                rustls::Error::PeerIncompatible(rustls::PeerIncompatible::NoCipherSuitesInCommon),
            ),
            std::io::Error::other("anything"),
        ] {
            assert!(!is_refusal(&other), "{other}");
        }
    }

    #[test]
    fn each_refused_host_is_said_once_and_nothing_else_is() {
        let said = Arc::new(Mutex::new(Vec::new()));
        let refusals = {
            let said = said.clone();
            Refusals::saying(move |host| said.lock().unwrap().push(host.to_string()))
        };
        assert!(refusals.handshake_failed("GitHub.com", &refusal()));
        assert!(refusals.handshake_failed("github.com", &refusal()));
        let eof = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof");
        assert!(!refusals.handshake_failed("example.com", &eof));
        // Nothing that could break the owner's line.
        assert!(refusals.handshake_failed("bad host\nSYNDEO", &refusal()));
        assert!(refusals.handshake_failed("", &refusal()));
        assert_eq!(*said.lock().unwrap(), ["github.com"]);
        assert_eq!(line("github.com"), "SYNDEO-PROXY-REFUSED 1 github.com\n");
    }

    #[test]
    fn a_silent_proxy_says_nothing_and_still_knows_the_refusal() {
        assert!(Refusals::silent().handshake_failed("github.com", &refusal()));
    }
}
