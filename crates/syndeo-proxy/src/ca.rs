//! The proxy's own certificate authority.
//!
//! Interception is only useful if an ordinary browser will talk to us, which
//! means minting a leaf per origin. The authority is generated locally, never
//! leaves the machine, and exists solely so hit rate can be measured on real
//! traffic before any of the browser is written.

use anyhow::{bail, Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct CertificateAuthority {
    /// The signing identity, derived from the stored certificate rather than
    /// from a fresh one minted to look like it.
    issuer: Issuer<'static, KeyPair>,
    /// The stored authority, verbatim. This is what goes in the chain, so what a
    /// client is offered is byte-for-byte what the user installed.
    issuer_der: Vec<u8>,
    ca_pem: String,
    dir: PathBuf,
    leaves: Mutex<HashMap<String, Arc<rustls::ServerConfig>>>,
}

impl CertificateAuthority {
    /// Load the authority from disk, generating it on first run.
    pub fn load_or_create(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let cert_path = dir.join("syndeo-ca.pem");
        let key_path = dir.join("syndeo-ca.key");

        let (ca_pem, key_pem) = if cert_path.exists() && key_path.exists() {
            (
                std::fs::read_to_string(&cert_path)?,
                std::fs::read_to_string(&key_path)?,
            )
        } else {
            let (cert_pem, key_pem) = generate()?;
            std::fs::write(&cert_path, &cert_pem)?;
            write_private(&key_path, &key_pem)?;
            tracing::info!(path = %cert_path.display(), "generated a new proxy authority");
            (cert_pem, key_pem)
        };

        let issuer_key = KeyPair::from_pem(&key_pem).context("reading the authority key")?;
        // Not `from_ca_cert_pem` plus `self_signed`, which mints a *different*
        // certificate on every run — a new serial, and a randomised ECDSA
        // signature regardless — and then chains leaves against that copy rather
        // than against the one the user actually installed. `Issuer` takes the
        // stored certificate's identity without reissuing it.
        let issuer = Issuer::from_ca_cert_pem(&ca_pem, issuer_key)
            .context("reading the authority certificate")?;
        let issuer_der = der_from_pem(&ca_pem)?;

        Ok(CertificateAuthority {
            issuer,
            issuer_der,
            ca_pem,
            dir,
            leaves: Mutex::new(HashMap::new()),
        })
    }

    /// The stored authority certificate, as it is on disk. Read by the test
    /// that holds the chain to it; the running proxy uses `issuer_der` directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn issuer_der(&self) -> &[u8] {
        &self.issuer_der
    }

    /// A leaf for one origin, and the chain that vouches for it. Split out from
    /// [`server_config`] so a test can look at what a client would be offered.
    ///
    /// [`server_config`]: CertificateAuthority::server_config
    fn leaf_chain(
        &self,
        host: &str,
    ) -> Result<(
        Vec<rustls::pki_types::CertificateDer<'static>>,
        rustls::pki_types::PrivateKeyDer<'static>,
    )> {
        let leaf_key = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec![host.to_string()])?;
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, host);
        params.distinguished_name = name;
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

        let leaf = params.signed_by(&leaf_key, &self.issuer)?;
        let chain = vec![
            rustls::pki_types::CertificateDer::from(leaf.der().to_vec()),
            rustls::pki_types::CertificateDer::from(self.issuer_der.clone()),
        ];
        let key = rustls::pki_types::PrivateKeyDer::try_from(leaf_key.serialize_der())
            .map_err(|e| anyhow::anyhow!("leaf key: {e}"))?;
        Ok((chain, key))
    }

    pub fn certificate_path(&self) -> PathBuf {
        self.dir.join("syndeo-ca.pem")
    }

    /// Where the authority's private key lives.
    ///
    /// Worth being able to name, because anyone who takes this key can
    /// impersonate any site to a machine that trusts the certificate — so the
    /// command that asks a user to trust it says out loud which file they are
    /// now responsible for.
    pub fn key_path(&self) -> PathBuf {
        self.dir.join("syndeo-ca.key")
    }

    pub fn certificate_pem(&self) -> &str {
        &self.ca_pem
    }

    /// A rustls server config for one origin, minted on demand and kept.
    pub fn server_config(&self, host: &str) -> Result<Arc<rustls::ServerConfig>> {
        if let Some(existing) = self.leaves.lock().unwrap().get(host) {
            return Ok(existing.clone());
        }

        let (chain, key) = self.leaf_chain(host)?;
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];

        let config = Arc::new(config);
        self.leaves
            .lock()
            .unwrap()
            .insert(host.to_string(), config.clone());
        Ok(config)
    }
}

fn generate() -> Result<(String, String)> {
    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Syndeo Local Measurement CA");
    name.push(DnType::OrganizationName, "Syndeo");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let cert = params.self_signed(&key_pair)?;
    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// The first CERTIFICATE block of a PEM document, as DER.
fn der_from_pem(pem: &str) -> Result<Vec<u8>> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let first = rustls_pemfile::certs(&mut reader)
        .next()
        .transpose()
        .context("parsing the authority certificate")?;
    match first {
        Some(certificate) => Ok(certificate.to_vec()),
        None => bail!("the authority file contains no certificate"),
    }
}

/// The authority key is as sensitive as any private key on the machine.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authority_served_in_the_chain_is_the_one_on_disk() {
        let dir = tempfile::tempdir().unwrap();

        let first = CertificateAuthority::load_or_create(dir.path()).unwrap();
        let on_disk = std::fs::read_to_string(first.certificate_path()).unwrap();
        let expected = der_from_pem(&on_disk).unwrap();
        assert_eq!(
            first.issuer_der(),
            expected.as_slice(),
            "the authority was regenerated on first load"
        );

        // The failure this guards against is a second run minting a lookalike:
        // same subject and key, different serial and signature, so leaves chain
        // against a certificate the user never installed.
        let second = CertificateAuthority::load_or_create(dir.path()).unwrap();
        assert_eq!(
            first.issuer_der(),
            second.issuer_der(),
            "the authority differs between two loads of the same file"
        );
        assert_eq!(first.certificate_pem(), second.certificate_pem());
    }

    #[test]
    fn a_leaf_is_offered_with_the_stored_authority_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let ca = CertificateAuthority::load_or_create(dir.path()).unwrap();

        let (chain, _) = ca.leaf_chain("example.test").unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(
            chain[1].as_ref(),
            ca.issuer_der(),
            "the chain offers a regenerated authority, not the installed one"
        );

        // And across a reload, which is where the regeneration used to happen.
        let reloaded = CertificateAuthority::load_or_create(dir.path()).unwrap();
        let (again, _) = reloaded.leaf_chain("example.test").unwrap();
        assert_eq!(chain[1], again[1]);
    }

    #[test]
    fn a_host_is_signed_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ca = CertificateAuthority::load_or_create(dir.path()).unwrap();
        let config = ca.server_config("example.test").unwrap();
        let again = ca.server_config("example.test").unwrap();
        assert!(Arc::ptr_eq(&config, &again));
    }
}
