//! TLS, rustls only. No OpenSSL anywhere in the tree — cargo-deny enforces it.
//!
//! Verification is the *platform's*, not ours. Loading the operating system's
//! root store and then verifying with our own logic gets the right roots and the
//! wrong policy: a certificate an administrator has distrusted locally still
//! verifies, revocation follows rustls' configuration rather than the system's,
//! and on macOS neither App Transport Security nor system-level pinning is
//! consulted. `rustls-platform-verifier` hands the chain to the operating
//! system's own verifier instead, so local administrative configuration applies
//! to this browser the way it applies to every other program on the machine.

use crate::error::{NetError, Result};
use rustls_platform_verifier::BuilderVerifierExt;
use std::sync::Arc;

/// A client config that verifies with the operating system's own verifier.
pub fn client_config() -> Result<Arc<rustls::ClientConfig>> {
    install_crypto_provider();

    // ALPN is deliberately left unset: the connector negotiates h2 and http/1.1
    // itself, and refuses a config that has already decided.
    //
    // There is no root-store emptiness check to make any more. The platform
    // verifier does not expose a store to inspect; its failure mode is a
    // verifier that refuses chains, and it surfaces here as a build error.
    let config = rustls::ClientConfig::builder()
        .with_platform_verifier()
        .map_err(|e| NetError::Tls(format!("the platform verifier is unavailable: {e}")))?
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Install the process-wide crypto provider exactly once.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_platform_verifier_builds_and_declines_to_preempt_alpn() {
        let config = super::client_config().expect("a platform verifier on this host");
        assert!(
            config.alpn_protocols.is_empty(),
            "the connector negotiates versions; a config that has already decided is refused"
        );
    }
}
