//! Finding and removing a Syndeo authority in a keychain when there is no
//! local copy of it left to name it by.
//!
//! `ca --untrust` normally removes the trust setting for the certificate on
//! disk. If the proxy's directory has gone — a deleted home, a reinstall — the
//! certificate can still be in the login keychain, trusted, with nothing here
//! to identify it but its name. Then every certificate carrying exactly that
//! name is shown by fingerprint, and each is removed, with its trust setting,
//! only if the person at the terminal says so.
//!
//! Every `security` command here names its keychain explicitly.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// The common name every Syndeo authority is issued under.
pub const NAME: &str = "Syndeo Local Measurement CA";

/// One certificate from `security find-certificate -a -Z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub sha256: String,
    pub sha1: String,
    pub label: Option<String>,
}

/// Read `security find-certificate -a -Z` output. Each certificate starts
/// with its SHA-256 line; the label is the `"labl"` attribute.
pub fn parse(output: &str) -> Vec<Listed> {
    let mut listed: Vec<Listed> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(hash) = line.strip_prefix("SHA-256 hash:") {
            listed.push(Listed {
                sha256: hash.trim().to_string(),
                sha1: String::new(),
                label: None,
            });
        } else if let (Some(hash), Some(current)) =
            (line.strip_prefix("SHA-1 hash:"), listed.last_mut())
        {
            current.sha1 = hash.trim().to_string();
        } else if let (Some(value), Some(current)) =
            (line.strip_prefix("\"labl\"<blob>="), listed.last_mut())
        {
            current.label = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_string);
        }
    }
    listed
        .into_iter()
        .filter(|l| is_hex(&l.sha256, 64) && is_hex(&l.sha1, 40))
        .collect()
}

fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// The keychain operations untrusting needs.
pub trait Keychain {
    /// Where it is, for saying so.
    fn describe(&self) -> String;
    /// `find-certificate -a -Z -c <name>` output; empty when nothing matches.
    fn find(&self, name: &str) -> Result<String>;
    /// Delete one certificate, and its user trust setting, by SHA-256.
    fn delete(&self, sha256: &str) -> Result<()>;
}

/// The real thing: `/usr/bin/security`, always against `path`.
pub struct Security {
    pub path: PathBuf,
}

impl Security {
    /// This user's login keychain.
    pub fn login() -> Result<Self> {
        let home = std::env::var("HOME").context("HOME is not set")?;
        Ok(Security {
            path: PathBuf::from(home).join("Library/Keychains/login.keychain-db"),
        })
    }
}

impl Keychain for Security {
    fn describe(&self) -> String {
        self.path.display().to_string()
    }

    fn find(&self, name: &str) -> Result<String> {
        let output = std::process::Command::new("/usr/bin/security")
            .args(["find-certificate", "-a", "-Z", "-c", name])
            .arg(&self.path)
            .output()
            .context("running security find-certificate")?;
        // It exits non-zero when nothing matches, which is an answer, not a
        // failure.
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn delete(&self, sha256: &str) -> Result<()> {
        let status = std::process::Command::new("/usr/bin/security")
            .args(["delete-certificate", "-t", "-Z", sha256])
            .arg(&self.path)
            .status()
            .context("running security delete-certificate")?;
        if !status.success() {
            anyhow::bail!("security could not delete {sha256} ({status})");
        }
        Ok(())
    }
}

/// Asks the person at the terminal.
pub trait Ask {
    fn confirm(&self, question: &str) -> bool;
}

/// Standard input: only exactly `yes` is yes.
pub struct Terminal;

impl Ask for Terminal {
    fn confirm(&self, question: &str) -> bool {
        use std::io::Write;
        print!("{question} Type 'yes' to delete it: ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && answer.trim() == "yes"
    }
}

/// The SHA-256 fingerprint of a certificate's DER, as `security` prints it.
pub fn sha256_hex(der: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

fn lists(keychain: &dyn Keychain, sha256: &str) -> Result<bool> {
    Ok(parse(&keychain.find(NAME)?)
        .iter()
        .any(|l| l.sha256.eq_ignore_ascii_case(sha256)))
}

/// Remove this home's own authority, identified by its fingerprint alone,
/// with its trust setting, from `keychain`.
///
/// Never by name: a stale authority from another home carries the same one,
/// and `delete-certificate -c` then either refuses or reaches the wrong
/// certificate. Success is said only once the deletion has succeeded and a
/// fresh listing no longer shows the fingerprint; otherwise it is an error.
pub fn untrust_local(
    keychain: &dyn Keychain,
    sha256: &str,
    out: &mut dyn std::io::Write,
) -> Result<()> {
    if !lists(keychain, sha256)? {
        writeln!(
            out,
            "This authority (SHA-256 {sha256}) is not in {}; nothing was removed.",
            keychain.describe()
        )?;
        return Ok(());
    }
    keychain
        .delete(sha256)
        .with_context(|| format!("removing SHA-256 {sha256} from {}", keychain.describe()))?;
    if lists(keychain, sha256)? {
        anyhow::bail!(
            "SHA-256 {sha256} is still in {} after deleting it; its trust setting may remain",
            keychain.describe()
        );
    }
    writeln!(
        out,
        "Removed this authority (SHA-256 {sha256}) and its trust setting from {}.",
        keychain.describe()
    )?;
    Ok(())
}

/// Remove, on confirmation, every certificate named exactly [`NAME`]. Returns
/// how many were removed.
pub fn untrust_by_name(
    keychain: &dyn Keychain,
    ask: &dyn Ask,
    out: &mut dyn std::io::Write,
) -> Result<usize> {
    let candidates: Vec<Listed> = parse(&keychain.find(NAME)?)
        .into_iter()
        // `-c` matches any name containing the text; only the exact one is ours.
        .filter(|l| l.label.as_deref() == Some(NAME))
        .collect();
    if candidates.is_empty() {
        writeln!(
            out,
            "No certificate named {NAME:?} in {}.",
            keychain.describe()
        )?;
        return Ok(0);
    }
    writeln!(
        out,
        "There is no local authority here, but {} certificate(s) named {NAME:?} \
         are in {}:",
        candidates.len(),
        keychain.describe()
    )?;
    let mut removed = 0;
    for candidate in &candidates {
        writeln!(out)?;
        writeln!(out, "  SHA-256 {}", candidate.sha256)?;
        writeln!(out, "  SHA-1   {}", candidate.sha1)?;
        if ask.confirm("Delete this certificate and its trust setting?") {
            keychain.delete(&candidate.sha256)?;
            writeln!(out, "  deleted")?;
            removed += 1;
        } else {
            writeln!(out, "  kept")?;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// The shape of real `security find-certificate -a -Z` output: two
    /// records captured from the system roots, and two of ours around a name
    /// that only contains ours.
    const LISTING: &str = r#"SHA-256 hash: 63343ABFB89A6A03EBB57E9B3F5FA7BE7C4F5C756F3017B3A8C488C3653E9179
SHA-1 hash: B52CB02FD567E0359FE8FA4D4C41037970FE01B0
keychain: "/System/Library/Keychains/SystemRootCertificates.keychain"
version: 256
class: 0x80001000
attributes:
    "alis"<blob>="Apple Root CA - G3"
    "labl"<blob>="Apple Root CA - G3"
SHA-256 hash: 1111111111111111111111111111111111111111111111111111111111111111
SHA-1 hash: 2222222222222222222222222222222222222222
keychain: "/Users/someone/Library/Keychains/login.keychain-db"
attributes:
    "labl"<blob>="Syndeo Local Measurement CA"
SHA-256 hash: 3333333333333333333333333333333333333333333333333333333333333333
SHA-1 hash: 4444444444444444444444444444444444444444
attributes:
    "labl"<blob>="Syndeo Local Measurement CA (someone else's)"
SHA-256 hash: 5555555555555555555555555555555555555555555555555555555555555555
SHA-1 hash: 6666666666666666666666666666666666666666
attributes:
    "labl"<blob>="Syndeo Local Measurement CA"
"#;

    struct Fake {
        listing: &'static str,
        deleted: RefCell<Vec<String>>,
    }

    impl Keychain for Fake {
        fn describe(&self) -> String {
            "a test keychain".into()
        }
        fn find(&self, name: &str) -> Result<String> {
            assert_eq!(name, NAME);
            Ok(self.listing.to_string())
        }
        fn delete(&self, sha256: &str) -> Result<()> {
            self.deleted.borrow_mut().push(sha256.to_string());
            Ok(())
        }
    }

    struct Answers(RefCell<Vec<bool>>);

    impl Ask for Answers {
        fn confirm(&self, _question: &str) -> bool {
            self.0.borrow_mut().remove(0)
        }
    }

    #[test]
    fn a_listing_is_read_certificate_by_certificate() {
        let listed = parse(LISTING);
        assert_eq!(listed.len(), 4);
        assert_eq!(
            listed[0],
            Listed {
                sha256: "63343ABFB89A6A03EBB57E9B3F5FA7BE7C4F5C756F3017B3A8C488C3653E9179".into(),
                sha1: "B52CB02FD567E0359FE8FA4D4C41037970FE01B0".into(),
                label: Some("Apple Root CA - G3".into()),
            }
        );
        assert!(parse("").is_empty());
        assert!(parse("SHA-256 hash: not-a-hash\n").is_empty());
    }

    #[test]
    fn only_certificates_named_exactly_ours_are_offered_and_only_yes_deletes() {
        let keychain = Fake {
            listing: LISTING,
            deleted: RefCell::new(Vec::new()),
        };
        let mut out = Vec::new();
        let removed = untrust_by_name(
            &keychain,
            &Answers(RefCell::new(vec![false, true])),
            &mut out,
        )
        .unwrap();
        assert_eq!(removed, 1);
        assert_eq!(*keychain.deleted.borrow(), vec!["5".repeat(64)]);
        let said = String::from_utf8(out).unwrap();
        assert!(said.contains(&"1".repeat(64)) && said.contains(&"2".repeat(40)));
        assert!(
            !said.contains(&"3".repeat(64)),
            "a near-miss name was offered"
        );
    }

    #[test]
    fn declining_deletes_nothing() {
        let keychain = Fake {
            listing: LISTING,
            deleted: RefCell::new(Vec::new()),
        };
        let removed = untrust_by_name(
            &keychain,
            &Answers(RefCell::new(vec![false, false])),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(removed, 0);
        assert!(keychain.deleted.borrow().is_empty());
    }

    /// Lists what it is told, and fails or pretends as told.
    struct Scripted {
        listings: RefCell<Vec<String>>,
        delete_fails: bool,
        deleted: RefCell<Vec<String>>,
    }

    impl Keychain for Scripted {
        fn describe(&self) -> String {
            "a scripted keychain".into()
        }
        fn find(&self, _: &str) -> Result<String> {
            Ok(self.listings.borrow_mut().remove(0))
        }
        fn delete(&self, sha256: &str) -> Result<()> {
            self.deleted.borrow_mut().push(sha256.to_string());
            if self.delete_fails {
                anyhow::bail!("security could not delete it");
            }
            Ok(())
        }
    }

    fn listing_of(hashes: &[&str]) -> String {
        hashes
            .iter()
            .map(|h| {
                format!(
                    "SHA-256 hash: {h}\nSHA-1 hash: {}\n    \"labl\"<blob>=\"{NAME}\"\n",
                    "0".repeat(40)
                )
            })
            .collect()
    }

    fn scripted(listings: Vec<String>, delete_fails: bool) -> Scripted {
        Scripted {
            listings: RefCell::new(listings),
            delete_fails,
            deleted: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn a_failed_deletion_is_an_error_and_never_reported_as_success() {
        let (ours, theirs) = ("A".repeat(64), "B".repeat(64));
        let keychain = scripted(vec![listing_of(&[&ours, &theirs])], true);
        let mut out = Vec::new();
        assert!(untrust_local(&keychain, &ours, &mut out).is_err());
        assert_eq!(
            *keychain.deleted.borrow(),
            vec![ours.clone()],
            "by fingerprint only"
        );
        assert!(!String::from_utf8(out).unwrap().contains("Removed"));
    }

    #[test]
    fn a_deletion_that_leaves_the_fingerprint_behind_is_an_error() {
        let ours = "A".repeat(64);
        let keychain = scripted(vec![listing_of(&[&ours]), listing_of(&[&ours])], false);
        let mut out = Vec::new();
        assert!(untrust_local(&keychain, &ours, &mut out).is_err());
        assert!(!String::from_utf8(out).unwrap().contains("Removed"));
    }

    #[test]
    fn only_a_verified_removal_is_reported_and_nothing_is_touched_when_absent() {
        let (ours, theirs) = ("A".repeat(64), "B".repeat(64));
        let keychain = scripted(
            vec![listing_of(&[&ours, &theirs]), listing_of(&[&theirs])],
            false,
        );
        let mut out = Vec::new();
        untrust_local(&keychain, &ours, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("Removed"));

        let absent = scripted(vec![listing_of(&[&theirs])], false);
        let mut out = Vec::new();
        untrust_local(&absent, &ours, &mut out).unwrap();
        assert!(absent.deleted.borrow().is_empty());
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("nothing was removed"));
    }

    /// A real keychain, but a throwaway one: created here, named on every
    /// command, and deleted however the test ends, with a check that the
    /// user's keychain search list is exactly as it was. CI only.
    struct Throwaway {
        dir: tempfile::TempDir,
        path: PathBuf,
        search_list_before: String,
    }

    fn search_list() -> String {
        let out = std::process::Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    impl Throwaway {
        fn create() -> Option<Self> {
            if std::env::var("SYNDEO_KEYCHAIN_TEST").as_deref() != Ok("1") {
                return None;
            }
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("throwaway.keychain-db");
            let search_list_before = search_list();
            let password: String = (0..24)
                .map(|i| char::from(b'a' + (i * 7 % 26) as u8))
                .collect();
            let created = std::process::Command::new("/usr/bin/security")
                .args(["create-keychain", "-p", &password])
                .arg(&path)
                .status()
                .unwrap();
            assert!(created.success());
            let throwaway = Throwaway {
                dir,
                path,
                search_list_before,
            };
            throwaway.assert_search_list_untouched();
            Some(throwaway)
        }

        fn assert_search_list_untouched(&self) {
            assert_eq!(
                search_list(),
                self.search_list_before,
                "the search list changed"
            );
        }

        /// A new self-signed authority under our name, imported here. Returns
        /// its SHA-256 fingerprint.
        fn import_authority(&self, file: &str) -> String {
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, NAME);
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            let key = rcgen::KeyPair::generate().unwrap();
            let cert = params.self_signed(&key).unwrap();
            let pem = self.dir.path().join(file);
            std::fs::write(&pem, cert.pem()).unwrap();
            let imported = std::process::Command::new("/usr/bin/security")
                .arg("import")
                .arg(&pem)
                .arg("-k")
                .arg(&self.path)
                .status()
                .unwrap();
            assert!(imported.success());
            sha256_hex(cert.der())
        }

        fn keychain(&self) -> Security {
            Security {
                path: self.path.clone(),
            }
        }
    }

    impl Drop for Throwaway {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/usr/bin/security")
                .arg("delete-keychain")
                .arg(&self.path)
                .status();
        }
    }

    #[test]
    fn untrusting_the_local_authority_removes_it_and_leaves_a_same_named_one() {
        let Some(throwaway) = Throwaway::create() else {
            return;
        };
        let ours = throwaway.import_authority("ours.pem");
        let theirs = throwaway.import_authority("theirs.pem");
        let keychain = throwaway.keychain();
        assert!(lists(&keychain, &ours).unwrap() && lists(&keychain, &theirs).unwrap());

        let mut out = Vec::new();
        untrust_local(&keychain, &ours, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("Removed"));
        assert!(!lists(&keychain, &ours).unwrap(), "ours is still there");
        assert!(lists(&keychain, &theirs).unwrap(), "the other one went too");
        throwaway.assert_search_list_untouched();
    }

    #[test]
    fn a_stale_certificate_is_found_and_deleted_from_a_throwaway_keychain() {
        let Some(throwaway) = Throwaway::create() else {
            return;
        };
        throwaway.import_authority("stale.pem");
        let keychain = throwaway.keychain();
        let listed: Vec<Listed> = parse(&keychain.find(NAME).unwrap())
            .into_iter()
            .filter(|l| l.label.as_deref() == Some(NAME))
            .collect();
        assert_eq!(listed.len(), 1, "{listed:?}");

        struct Yes;
        impl Ask for Yes {
            fn confirm(&self, _: &str) -> bool {
                true
            }
        }
        assert_eq!(
            untrust_by_name(&keychain, &Yes, &mut Vec::new()).unwrap(),
            1
        );
        assert!(parse(&keychain.find(NAME).unwrap()).is_empty());
        throwaway.assert_search_list_untouched();
    }
}
