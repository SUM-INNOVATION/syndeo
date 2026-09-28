//! The keystore process.
//!
//! Normally spawned by the shell, which passes the session secret on the
//! environment and never lets the agent near either the secret or this socket.
//! The subcommands exist so enrolment can be driven from a terminal before the
//! shell has a UI.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use syndeo_ipc::confirm::{Confirmer, SessionSecret};
use syndeo_ipc::startup::StartupSecrets;
use syndeo_ipc::transport::{Endpoint, Server};
use syndeo_keystore::{Address, Keystore, OsKeyring, WrappingKeyStore};
use zeroize::Zeroizing;

/// The shell passes the session secret this way and then clears it.
const SECRET_VAR: &str = syndeo_ipc::startup::SESSION_SECRET;

#[derive(Parser)]
#[command(
    name = "syndeo-keystore",
    version,
    about = "Holds keys; signs only what the shell confirms"
)]
struct Cli {
    /// Where `.keystore` lives.
    #[arg(long, global = true)]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the keystore protocol on a socket.
    Serve {
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Create a keystore and print the recovery phrase once.
    Init,
    /// Restore from a recovery phrase.
    Restore,
    /// Report what exists and what it requires.
    Status,
    /// Show the public identity for an origin.
    Identity { origin: String },
}

fn home(override_path: Option<PathBuf>) -> PathBuf {
    override_path.unwrap_or_else(|| {
        std::env::var_os("SYNDEO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".syndeo")
            })
    })
}

/// The operating system's credential store entry for the wrapping key.
///
/// One per machine user, not one per home: every `--home` and `SYNDEO_HOME`
/// shares it, which is why enrolment asks whether it is taken before writing.
fn os_keyring() -> Arc<dyn WrappingKeyStore> {
    Arc::new(OsKeyring::new("com.sum.syndeo.keystore", "root-seed"))
}

fn open(home: &Path, wrapping: Arc<dyn WrappingKeyStore>) -> Result<Arc<Keystore>> {
    Ok(Arc::new(Keystore::open(home, wrapping)?))
}

/// `init`, once the terminal has been read.
///
/// Opens the keystore here, in this process, and never over the socket: the
/// service refuses enrolment, and the person who is shown the recovery phrase
/// has to be the one at this terminal. The credential store is a parameter so
/// the tests can run the command against one that is not the user's.
fn init(
    home: &Path,
    wrapping: Arc<dyn WrappingKeyStore>,
    passphrase: Option<&str>,
) -> Result<(Zeroizing<String>, Address)> {
    Ok(open(home, wrapping)?.initialize(passphrase)?)
}

/// `init` as the command runs it: everything that could refuse is checked
/// before `passphrase` is asked for anything, so a person is never made to
/// choose a passphrase for an enrolment that was never going to happen.
fn init_command(
    home: &Path,
    wrapping: Arc<dyn WrappingKeyStore>,
    passphrase: impl FnOnce() -> Result<Zeroizing<String>>,
) -> Result<(Zeroizing<String>, Address)> {
    let keystore = open(home, wrapping.clone())?;
    keystore.check_can_initialize()?;
    let status = keystore.status();
    drop(keystore);
    let passphrase = if status.passphrase_required || !status.presence_enforced {
        if !status.presence_enforced {
            eprintln!(
                "This platform does not enforce user presence on the credential store,\n\
                 so a passphrase is required rather than optional."
            );
        }
        Some(passphrase()?)
    } else {
        None
    };
    init(home, wrapping, passphrase.as_ref().map(|p| p.as_str()))
}

/// `restore`, once the terminal has been read. Direct for the same reasons as
/// [`init`], and, unlike it, replaces whatever wrapping key is enrolled.
fn restore(
    home: &Path,
    wrapping: Arc<dyn WrappingKeyStore>,
    phrase: &str,
    passphrase: Option<&str>,
) -> Result<Address> {
    Ok(open(home, wrapping)?.restore(phrase, passphrase)?)
}

fn main() -> Result<()> {
    // First, while this is still one thread: the session secret and any
    // scripted passphrase leave the environment before a runtime, a logger or
    // anything else exists to read or inherit them.
    let secrets = syndeo_ipc::startup::capture(&[SECRET_VAR, syndeo_keystore::passphrase::ENV_VAR]);
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the runtime")?
        .block_on(run(secrets))
}

/// `secrets` is everything `main` took out of the environment. Each command
/// takes what it uses; the rest is wiped when this returns.
async fn run(mut secrets: StartupSecrets) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_keystore=info")),
        )
        .with_target(false)
        // Logs go to stderr: stdout is the shell's, and a child that
        // shares it would write into what the shell prints, --json included.
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let home = home(cli.home);
    let wrapping = os_keyring();
    let keystore = open(&home, wrapping.clone())?;

    match cli.command.unwrap_or(Command::Status) {
        Command::Serve { socket } => {
            // Only when serving: the subcommands are run by a person at a
            // terminal, where stdin is theirs and closing it means nothing.
            syndeo_ipc::exit_when_parent_does();

            let secret = secrets
                .take(SECRET_VAR)
                .and_then(|s| SessionSecret::from_hex(&s))
                .context(
                    "no session secret; the keystore is spawned by the shell, which supplies one",
                )?;

            let endpoint = match socket {
                Some(path) => Endpoint::new(path),
                None => Endpoint::in_runtime_dir(
                    syndeo_ipc::transport::runtime_dir_for(&home),
                    "keystore",
                )?,
            };
            let server = Server::bind(endpoint)?;
            syndeo_keystore::service::serve(keystore, Arc::new(Confirmer::new(secret)), server)
                .await;
            Ok(())
        }

        Command::Init => {
            drop(keystore);
            let scripted = secrets.take(syndeo_keystore::passphrase::ENV_VAR);
            let (mnemonic, address) =
                init_command(&home, wrapping, || read_new_passphrase(scripted))?;
            println!();
            println!("Recovery phrase — write it down now. It is shown once and never stored.");
            println!();
            for (i, word) in mnemonic.split_whitespace().enumerate() {
                print!("{:>2}. {:<12}", i + 1, word);
                if (i + 1) % 4 == 0 {
                    println!();
                }
            }
            println!();
            println!("Identity: {address}");
            println!();
            println!(
                "Both the credential store entry and the passphrase are required to unseal.\n\
                 Losing both is unrecoverable except from this phrase."
            );
            Ok(())
        }

        Command::Restore => {
            eprint!("Recovery phrase: ");
            let mut phrase = Zeroizing::new(String::new());
            std::io::stdin()
                .read_line(&mut phrase)
                .context("reading the recovery phrase")?;
            let passphrase = Some(read_new_passphrase(
                secrets.take(syndeo_keystore::passphrase::ENV_VAR),
            )?);
            let address = restore(
                &home,
                wrapping,
                phrase.trim(),
                passphrase.as_ref().map(|p| p.as_str()),
            )?;
            println!("Restored: {address}");
            Ok(())
        }

        Command::Status => {
            let status = keystore.status();
            println!("initialized          {}", status.initialized);
            println!("unsealed             {}", status.unsealed);
            println!("passphrase required  {}", status.passphrase_required);
            println!("presence enforced    {}", status.presence_enforced);
            if let Some(hint) = keystore.identity_hint() {
                println!("identity             {hint}");
            }
            if !status.presence_enforced {
                println!();
                if cfg!(target_os = "macos") {
                    println!(
                        "The Secure Enclave is not gating the wrapping key on this build.\n\
                         The binding is there; the data protection keychain it needs is\n\
                         only reachable from a binary signed with a keychain access group:\n\
                         \n\
                         \x20 codesign --force --sign \"Apple Development: you\" \\\n\
                         \x20   --entitlements crates/syndeo-keystore/Syndeo.entitlements \\\n\
                         \x20   target/release/syndeo-keystore\n\
                         \n\
                         It has to be a real signing identity. Ad-hoc signing (--sign -)\n\
                         with this entitlement gets the process killed at launch, because\n\
                         the access group has no team prefix behind it.\n\
                         \n\
                         Until then the passphrase is mandatory rather than optional, and\n\
                         per-signature consent rests on shell confirmation of each payload."
                    );
                } else {
                    println!(
                        "This platform's credential store cannot enforce user presence,\n\
                         so the passphrase is mandatory and per-signature consent rests on\n\
                         shell confirmation of each payload. See syndeo_keystore::presence."
                    );
                }
            }
            Ok(())
        }

        Command::Identity { origin } => {
            let passphrase = syndeo_keystore::passphrase::read(
                "Passphrase: ",
                secrets.take(syndeo_keystore::passphrase::ENV_VAR),
            )?;
            keystore.unseal(Some(passphrase.as_str()))?;
            let (public_key, address) = keystore.public_identity(&origin)?;
            println!(
                "origin      {}",
                syndeo_keystore::derive::canonical_origin(&origin)
            );
            println!("public key  {public_key}");
            println!("address     {address}");
            keystore.lock();
            Ok(())
        }
    }
}

fn read_new_passphrase(scripted: Option<Zeroizing<String>>) -> Result<Zeroizing<String>> {
    Ok(syndeo_keystore::passphrase::read_new(scripted)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use syndeo_keystore::wrapping::InMemoryKeyStore;
    use syndeo_keystore::KeystoreError;

    const PASS: &str = "a passphrase the user chose";

    fn sealed(home: &Path) -> PathBuf {
        home.join(".keystore").join("seed.sealed")
    }

    fn keystore_error(err: &anyhow::Error) -> Option<&KeystoreError> {
        err.downcast_ref::<KeystoreError>()
    }

    #[test]
    fn init_seals_a_seed_in_the_home_it_was_given() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemoryKeyStore::default());

        let (mnemonic, address) = init(home.path(), store.clone(), Some(PASS)).unwrap();
        assert_eq!(mnemonic.split_whitespace().count(), 24);
        assert!(sealed(home.path()).is_file());
        assert!(store.stored().is_some());

        // And the command left behind something that opens.
        let keystore = open(home.path(), store).unwrap();
        assert_eq!(keystore.identity_hint(), Some(address.to_base58()));
        keystore.unseal(Some(PASS)).unwrap();
    }

    /// The defect this guards against: the credential store entry is shared by
    /// every home, and `init` in a second one used to replace the first one's
    /// wrapping key, leaving its seed unopenable.
    #[test]
    fn init_in_a_second_home_leaves_the_first_homes_key_alone() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let shared = Arc::new(InMemoryKeyStore::default());

        init(first.path(), shared.clone(), Some(PASS)).unwrap();
        let key = shared.stored().unwrap();

        let err = init(second.path(), shared.clone(), Some(PASS)).unwrap_err();
        assert!(
            matches!(keystore_error(&err), Some(KeystoreError::WrappingKeyExists)),
            "{err:#}"
        );
        let message = err.to_string();
        assert!(message.contains("nothing was changed"), "{message}");
        assert!(message.contains("syndeo-keystore restore"), "{message}");

        assert_eq!(shared.stored(), Some(key), "the first home's key changed");
        assert!(!sealed(second.path()).exists());

        // The first home still opens, which is the whole point.
        open(first.path(), shared)
            .unwrap()
            .unseal(Some(PASS))
            .unwrap();
    }

    #[test]
    fn init_refuses_when_the_credential_store_cannot_say() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemoryKeyStore::default());
        store.fail_existence_checks();

        let err = init(home.path(), store.clone(), Some(PASS)).unwrap_err();
        assert!(
            matches!(
                keystore_error(&err),
                Some(KeystoreError::WrappingKeyUnknown(_))
            ),
            "{err:#}"
        );
        assert_eq!(store.stored(), None);
        assert!(!sealed(home.path()).exists());
    }

    /// Counts how often the passphrase is asked for.
    fn counted(asked: &std::cell::Cell<usize>) -> impl FnOnce() -> Result<Zeroizing<String>> + '_ {
        move || {
            asked.set(asked.get() + 1);
            Ok(Zeroizing::new(PASS.to_string()))
        }
    }

    #[test]
    fn init_refuses_before_asking_for_a_passphrase() {
        let asked = std::cell::Cell::new(0);

        // Already initialized here.
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemoryKeyStore::default());
        init(home.path(), store.clone(), Some(PASS)).unwrap();
        let err = init_command(home.path(), store, counted(&asked)).unwrap_err();
        assert!(
            matches!(
                keystore_error(&err),
                Some(KeystoreError::AlreadyInitialized)
            ),
            "{err:#}"
        );

        // A second home, whose credential store already holds a key.
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let shared = Arc::new(InMemoryKeyStore::default());
        init(first.path(), shared.clone(), Some(PASS)).unwrap();
        let err = init_command(second.path(), shared, counted(&asked)).unwrap_err();
        assert!(
            matches!(keystore_error(&err), Some(KeystoreError::WrappingKeyExists)),
            "{err:#}"
        );

        // A credential store that cannot say.
        let unknown = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemoryKeyStore::default());
        store.fail_existence_checks();
        let err = init_command(unknown.path(), store, counted(&asked)).unwrap_err();
        assert!(
            matches!(
                keystore_error(&err),
                Some(KeystoreError::WrappingKeyUnknown(_))
            ),
            "{err:#}"
        );

        assert_eq!(asked.get(), 0, "a passphrase was asked for a refused init");
    }

    #[test]
    fn init_that_can_go_ahead_asks_at_most_once_and_seals() {
        let home = tempfile::tempdir().unwrap();
        let asked = std::cell::Cell::new(0);
        let store = Arc::new(InMemoryKeyStore::default());
        init_command(home.path(), store, counted(&asked)).unwrap();
        assert!(asked.get() <= 1);
        assert!(sealed(home.path()).is_file());
    }

    #[test]
    fn restore_recovers_the_identity_the_phrase_names() {
        let original = tempfile::tempdir().unwrap();
        let (mnemonic, address) = init(
            original.path(),
            Arc::new(InMemoryKeyStore::default()),
            Some(PASS),
        )
        .unwrap();

        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(InMemoryKeyStore::default());
        let restored = restore(
            home.path(),
            store.clone(),
            &mnemonic,
            Some("a different passphrase"),
        )
        .unwrap();
        assert_eq!(restored, address);
        assert!(sealed(home.path()).is_file());
        assert!(store.stored().is_some());
    }
}
