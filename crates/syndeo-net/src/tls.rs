//! TLS, rustls only. No OpenSSL anywhere in the tree — cargo-deny enforces it.

use crate::error::{NetError, Result};
use std::sync::Arc;

/// A client config trusting the operating system's root store.
pub fn client_config() -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(NetError::Tls(
            "the operating system root store is empty".into(),
        ));
    }

    // ALPN is deliberately left unset: the connector negotiates h2 and http/1.1
    // itself, and refuses a config that has already decided.
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
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
