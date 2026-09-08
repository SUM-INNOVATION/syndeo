//! The proxy's own certificate authority.
//!
//! Interception is only useful if an ordinary browser will talk to us, which
//! means minting a leaf per origin. The authority is generated locally, never
//! leaves the machine, and exists solely so hit rate can be measured on real
//! traffic before any of the browser is written.

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct CertificateAuthority {
    issuer: rcgen::Certificate,
    issuer_key: KeyPair,
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
        let params = CertificateParams::from_ca_cert_pem(&ca_pem)
            .context("reading the authority certificate")?;
        let issuer = params.self_signed(&issuer_key)?;

        Ok(CertificateAuthority {
            issuer,
            issuer_key,
            ca_pem,
            dir,
            leaves: Mutex::new(HashMap::new()),
        })
    }

    pub fn certificate_path(&self) -> PathBuf {
        self.dir.join("syndeo-ca.pem")
    }

    pub fn certificate_pem(&self) -> &str {
        &self.ca_pem
    }

    /// A rustls server config for one origin, minted on demand and kept.
    pub fn server_config(&self, host: &str) -> Result<Arc<rustls::ServerConfig>> {
        if let Some(existing) = self.leaves.lock().unwrap().get(host) {
            return Ok(existing.clone());
        }

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

        let leaf = params.signed_by(&leaf_key, &self.issuer, &self.issuer_key)?;

        let chain = vec![
            rustls::pki_types::CertificateDer::from(leaf.der().to_vec()),
            rustls::pki_types::CertificateDer::from(self.issuer.der().to_vec()),
        ];
        let key = rustls::pki_types::PrivateKeyDer::try_from(leaf_key.serialize_der())
            .map_err(|e| anyhow::anyhow!("leaf key: {e}"))?;

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
