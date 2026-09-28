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
    leaves: Mutex<Leaves>,
}

/// How many hosts' leaves are kept. A client names the host of every CONNECT,
/// so without a bound the map grows with whatever it chooses to name.
const LEAF_CAPACITY: usize = 1024;

/// Minted leaves, least recently used first to go.
struct Leaves {
    by_host: HashMap<String, (Arc<rustls::ServerConfig>, u64)>,
    clock: u64,
    capacity: usize,
}

impl Leaves {
    fn new(capacity: usize) -> Self {
        Leaves {
            by_host: HashMap::new(),
            clock: 0,
            capacity,
        }
    }

    fn get(&mut self, host: &str) -> Option<Arc<rustls::ServerConfig>> {
        self.clock += 1;
        let clock = self.clock;
        self.by_host.get_mut(host).map(|(config, used)| {
            *used = clock;
            config.clone()
        })
    }

    fn insert(&mut self, host: &str, config: Arc<rustls::ServerConfig>) {
        self.clock += 1;
        if !self.by_host.contains_key(host) && self.by_host.len() >= self.capacity {
            let oldest = self
                .by_host
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(host, _)| host.clone());
            if let Some(oldest) = oldest {
                self.by_host.remove(&oldest);
            }
        }
        self.by_host.insert(host.to_string(), (config, self.clock));
    }
}

const CERTIFICATE: &str = "syndeo-ca.pem";
const KEY: &str = "syndeo-ca.key";

impl CertificateAuthority {
    /// Load the authority from disk, generating it on first run.
    ///
    /// The directory and both files are checked before anything is read: see
    /// [`posture`] for what that means. A pair that is there is loaded as it
    /// is, never replaced. A first run that was interrupted leaves the key
    /// without its certificate, because the key is written first; the
    /// certificate is then issued again from that key, so no key is thrown
    /// away. A certificate without its key cannot sign anything, and is
    /// replaced along with a new key.
    pub fn load_or_create(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        posture::prepare_directory(&dir)?;
        let cert_path = dir.join(CERTIFICATE);
        let key_path = dir.join(KEY);

        let (ca_pem, key_pem) = match (
            posture::is_present(&cert_path)?,
            posture::is_present(&key_path)?,
        ) {
            (true, true) => (
                posture::read_public(&cert_path)?,
                posture::read_private(&key_path)?,
            ),
            (false, true) => {
                let key_pem = posture::read_private(&key_path)?;
                let cert_pem = certificate_for(&key_pem)?;
                posture::write_new(&dir, CERTIFICATE, &cert_pem, 0o644)?;
                tracing::warn!(
                    path = %cert_path.display(),
                    "the authority's key was here without its certificate; issued the certificate again from the same key"
                );
                (cert_pem, key_pem)
            }
            (true, false) => {
                tracing::warn!(
                    path = %cert_path.display(),
                    "the authority's certificate was here without its key, which cannot sign anything; \
                     replacing both. Remove any trust you gave the old one in Keychain Access"
                );
                generate_into(&dir)?
            }
            (false, false) => generate_into(&dir)?,
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
            leaves: Mutex::new(Leaves::new(LEAF_CAPACITY)),
        })
    }

    /// The stored authority certificate, as it is on disk: what goes in the
    /// chain, and what `ca --untrust` fingerprints to find it in the keychain.
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

    /// How many leaf certificates have been minted and kept.
    #[cfg(test)]
    pub fn leaf_count(&self) -> usize {
        self.leaves.lock().unwrap().by_host.len()
    }

    /// Whether a leaf for `host` is kept right now.
    #[cfg(test)]
    pub fn has_leaf(&self, host: &str) -> bool {
        self.leaves.lock().unwrap().by_host.contains_key(host)
    }

    /// Keep at most `capacity` leaves, for a test that cannot mint a thousand.
    #[cfg(test)]
    pub fn with_leaf_capacity(self, capacity: usize) -> Self {
        *self.leaves.lock().unwrap() = Leaves::new(capacity);
        self
    }

    /// A rustls server config for one origin, minted on demand and kept — the
    /// most recently used [`LEAF_CAPACITY`] of them.
    pub fn server_config(&self, host: &str) -> Result<Arc<rustls::ServerConfig>> {
        if let Some(existing) = self.leaves.lock().unwrap().get(host) {
            return Ok(existing);
        }

        let (chain, key) = self.leaf_chain(host)?;
        let mut config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];

        let config = Arc::new(config);
        self.leaves.lock().unwrap().insert(host, config.clone());
        Ok(config)
    }
}

/// A new key and a certificate for it, written key first so an interruption
/// can only ever leave the key.
fn generate_into(dir: &Path) -> Result<(String, String)> {
    let (cert_pem, key_pem) = generate()?;
    posture::write_new(dir, KEY, &key_pem, 0o600)?;
    posture::write_new(dir, CERTIFICATE, &cert_pem, 0o644)?;
    tracing::info!(path = %dir.join(CERTIFICATE).display(), "generated a new proxy authority");
    Ok((cert_pem, key_pem))
}

fn generate() -> Result<(String, String)> {
    let key_pair = KeyPair::generate()?;
    let cert = authority_params()?.self_signed(&key_pair)?;
    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// The authority's certificate, issued again from a key already on disk.
fn certificate_for(key_pem: &str) -> Result<String> {
    let key_pair = KeyPair::from_pem(key_pem).context("reading the authority key")?;
    Ok(authority_params()?.self_signed(&key_pair)?.pem())
}

fn authority_params() -> Result<CertificateParams> {
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
    Ok(params)
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

/// How the authority's files are kept.
///
/// The key is as sensitive as any private key on the machine: anyone who reads
/// it can impersonate any site to a user who trusts the certificate. So on
/// Unix the directory is the user's own and nobody else's (0700), the key is
/// the user's alone (0600) from the first byte it has, and nothing here follows
/// a symlink someone else could have put in its place.
///
/// - The directory is created 0700, must not be a symlink, must belong to the
///   effective user, and is tightened to 0700 if it is any wider.
/// - Both files must be regular files and not symlinks.
/// - The key is opened without following symlinks, checked on the open handle,
///   and tightened to 0600 before a byte of it is read.
/// - New files are created exclusively with their final mode, written, synced
///   and renamed into place, so no reader ever sees a partial or permissive
///   one; a temporary file is removed if anything fails on the way.
///
/// Elsewhere these are plain file operations, as they were.
mod posture {
    use anyhow::{bail, Context, Result};
    use std::io::Write;
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    pub fn prepare_directory(dir: &Path) -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        match std::fs::symlink_metadata(dir) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
            Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
            Ok(_) => {}
        }
        let meta = std::fs::symlink_metadata(dir)?;
        if meta.file_type().is_symlink() {
            bail!(
                "{} is a symlink; the authority's directory has to be a real one",
                dir.display()
            );
        }
        if !meta.is_dir() {
            bail!("{} is not a directory", dir.display());
        }
        // Opened without following links and checked on the handle, so the
        // directory changed is the directory checked.
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(dir)
            .with_context(|| format!("opening {}", dir.display()))?;
        let meta = handle.metadata()?;
        if meta.uid() != effective_user() {
            bail!(
                "{} belongs to another user; the authority's key cannot live there",
                dir.display()
            );
        }
        if meta.mode() & 0o077 != 0 {
            handle.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn prepare_directory(dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
    }

    /// Whether the file is there, refusing anything that is not a plain file.
    pub fn is_present(path: &Path) -> Result<bool> {
        match std::fs::symlink_metadata(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!("{} is a symlink; refusing to follow it", path.display())
            }
            Ok(meta) if !meta.is_file() => bail!("{} is not a regular file", path.display()),
            Ok(_) => Ok(true),
        }
    }

    pub fn read_public(path: &Path) -> Result<String> {
        read(path, None)
    }

    /// The key, after making sure nobody else can read it.
    pub fn read_private(path: &Path) -> Result<String> {
        read(path, Some(0o600))
    }

    #[cfg(unix)]
    fn read(path: &Path, tighten_to: Option<u32>) -> Result<String> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            bail!("{} is not a regular file", path.display());
        }
        if let Some(mode) = tighten_to {
            if meta.uid() != effective_user() {
                bail!("{} belongs to another user", path.display());
            }
            if meta.mode() & 0o777 & !mode != 0 {
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
        }
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(contents)
    }

    #[cfg(not(unix))]
    fn read(path: &Path, _tighten_to: Option<u32>) -> Result<String> {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }

    /// Write `name` in `dir` whole or not at all: an exclusive temporary file
    /// with its final mode, synced, then renamed over the name.
    pub fn write_new(dir: &Path, name: &str, contents: &str, mode: u32) -> Result<()> {
        let temporary = Temporary(Some(dir.join(format!(
            ".{name}.{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ))));
        let path = temporary.path();
        let mut file =
            create_exclusive(path, mode).with_context(|| format!("creating {}", path.display()))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(path, dir.join(name))
            .with_context(|| format!("putting {} in place", dir.join(name).display()))?;
        temporary.keep();
        sync_directory(dir);
        Ok(())
    }

    #[cfg(unix)]
    fn create_exclusive(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
    }

    #[cfg(not(unix))]
    fn create_exclusive(path: &Path, _mode: u32) -> std::io::Result<std::fs::File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }

    /// Make the rename itself durable. Best effort: a directory that cannot be
    /// synced still holds a complete file.
    fn sync_directory(dir: &Path) {
        #[cfg(unix)]
        if let Ok(handle) = std::fs::File::open(dir) {
            let _ = handle.sync_all();
        }
        #[cfg(not(unix))]
        let _ = dir;
    }

    /// A temporary file that is removed unless it was renamed into place.
    struct Temporary(Option<PathBuf>);

    impl Temporary {
        fn path(&self) -> &Path {
            self.0.as_deref().expect("present until kept")
        }
        fn keep(mut self) {
            self.0 = None;
        }
    }

    impl Drop for Temporary {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    #[cfg(unix)]
    fn effective_user() -> u32 {
        // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
        unsafe { libc::geteuid() }
    }
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

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).unwrap().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The names in a directory, sorted, so a stray temporary file shows up.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    #[test]
    fn a_new_authority_is_private_from_the_start() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("proxy");
        CertificateAuthority::load_or_create(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(KEY)), 0o600);
        assert_eq!(names(&dir), [KEY, CERTIFICATE], "nothing temporary is left");
    }

    /// Run in a child process of its own, because a umask is process-wide and
    /// the other tests run beside this one.
    #[cfg(unix)]
    #[test]
    fn a_permissive_umask_still_makes_a_private_key() {
        if let Some(dir) = std::env::var_os("SYNDEO_CA_UMASK_CHILD") {
            // SAFETY: umask only replaces the process's file-creation mask.
            unsafe { libc::umask(0) };
            CertificateAuthority::load_or_create(PathBuf::from(dir)).unwrap();
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("proxy");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ca::tests::a_permissive_umask_still_makes_a_private_key",
                "--test-threads=1",
            ])
            .env("SYNDEO_CA_UMASK_CHILD", &dir)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(KEY)), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_authority_is_tightened_and_kept() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("proxy");
        let first = CertificateAuthority::load_or_create(&dir).unwrap();
        let key_before = std::fs::read(dir.join(KEY)).unwrap();
        set_mode(&dir.join(KEY), 0o644);
        set_mode(&dir, 0o755);

        let second = CertificateAuthority::load_or_create(&dir).unwrap();

        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(KEY)), 0o600);
        assert_eq!(std::fs::read(dir.join(KEY)).unwrap(), key_before);
        assert_eq!(
            first.issuer_der(),
            second.issuer_der(),
            "the pair was replaced"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_place_of_either_file_is_refused() {
        for name in [KEY, CERTIFICATE] {
            let home = tempfile::tempdir().unwrap();
            let dir = home.path().join("proxy");
            CertificateAuthority::load_or_create(&dir).unwrap();
            let elsewhere = home.path().join("elsewhere");
            std::fs::rename(dir.join(name), &elsewhere).unwrap();
            set_mode(&elsewhere, 0o644);
            let before = std::fs::read(&elsewhere).unwrap();
            std::os::unix::fs::symlink(&elsewhere, dir.join(name)).unwrap();

            let refused = CertificateAuthority::load_or_create(&dir);

            assert!(refused.is_err(), "{name} through a symlink was accepted");
            assert_eq!(mode(&elsewhere), 0o644, "the link's target was changed");
            assert_eq!(std::fs::read(&elsewhere).unwrap(), before);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join("real");
        CertificateAuthority::load_or_create(&real).unwrap();
        let link = home.path().join("proxy");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(CertificateAuthority::load_or_create(&link).is_err());
    }

    #[test]
    fn an_interrupted_first_run_keeps_the_key_it_wrote() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("proxy");
        CertificateAuthority::load_or_create(&dir).unwrap();
        let key = std::fs::read_to_string(dir.join(KEY)).unwrap();
        std::fs::remove_file(dir.join(CERTIFICATE)).unwrap();

        let ca = CertificateAuthority::load_or_create(&dir).unwrap();

        assert_eq!(std::fs::read_to_string(dir.join(KEY)).unwrap(), key);
        // The new certificate carries that key's public half.
        let public = KeyPair::from_pem(&key).unwrap().public_key_raw().to_vec();
        assert!(ca.issuer_der().windows(public.len()).any(|w| w == public));
        assert_eq!(names(&dir), [KEY, CERTIFICATE]);
    }

    #[test]
    fn a_certificate_without_its_key_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("proxy");
        let old = CertificateAuthority::load_or_create(&dir).unwrap();
        std::fs::remove_file(dir.join(KEY)).unwrap();

        let new = CertificateAuthority::load_or_create(&dir).unwrap();

        assert_ne!(old.issuer_der(), new.issuer_der());
        assert_eq!(names(&dir), [KEY, CERTIFICATE]);
    }

    #[test]
    fn a_failed_write_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should go makes the rename fail after
        // the temporary file has been written.
        std::fs::create_dir(dir.path().join(KEY)).unwrap();
        std::fs::write(dir.path().join(KEY).join("occupied"), b"x").unwrap();

        assert!(posture::write_new(dir.path(), KEY, "secret", 0o600).is_err());

        assert_eq!(
            names(dir.path()),
            [KEY],
            "the temporary file was left behind"
        );
    }

    #[test]
    fn a_host_is_signed_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ca = CertificateAuthority::load_or_create(dir.path()).unwrap();
        let config = ca.server_config("example.test").unwrap();
        let again = ca.server_config("example.test").unwrap();
        assert!(Arc::ptr_eq(&config, &again));
    }

    #[test]
    fn leaves_are_kept_for_the_most_recently_used_hosts_only() {
        let dir = tempfile::tempdir().unwrap();
        let authority = CertificateAuthority::load_or_create(dir.path())
            .unwrap()
            .with_leaf_capacity(3);
        for host in ["a.test", "b.test", "c.test"] {
            authority.server_config(host).unwrap();
        }
        // a is used again, so b is now the one used longest ago.
        let a = authority.server_config("a.test").unwrap();
        authority.server_config("d.test").unwrap();

        assert_eq!(authority.leaf_count(), 3);
        assert!(authority.has_leaf("a.test"));
        assert!(
            !authority.has_leaf("b.test"),
            "the least recently used was kept"
        );
        assert!(authority.has_leaf("c.test") && authority.has_leaf("d.test"));
        // Kept means reused, not minted again.
        assert!(Arc::ptr_eq(&a, &authority.server_config("a.test").unwrap()));
    }
}
