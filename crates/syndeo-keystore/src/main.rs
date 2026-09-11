//! The keystore process.
//!
//! Normally spawned by the shell, which passes the session secret on the
//! environment and never lets the agent near either the secret or this socket.
//! The subcommands exist so enrolment can be driven from a terminal before the
//! shell has a UI.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use syndeo_ipc::confirm::{Confirmer, SessionSecret};
use syndeo_ipc::transport::{Endpoint, Server};
use syndeo_keystore::{Keystore, OsKeyring};

/// The shell passes the session secret this way and then clears it.
const SECRET_VAR: &str = "SYNDEO_SESSION_SECRET";

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

fn open(home: &std::path::Path) -> Result<Arc<Keystore>> {
    let wrapping = Arc::new(OsKeyring::new("com.sum.syndeo.keystore", "root-seed"));
    Ok(Arc::new(Keystore::open(home, wrapping)?))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("SYNDEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("syndeo_keystore=info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    let home = home(cli.home);
    let keystore = open(&home)?;

    match cli.command.unwrap_or(Command::Status) {
        Command::Serve { socket } => {
            let secret = std::env::var(SECRET_VAR)
                .ok()
                .and_then(|s| {
                    std::env::remove_var(SECRET_VAR);
                    SessionSecret::from_hex(&s)
                })
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
            let status = keystore.status();
            let passphrase = if status.passphrase_required || !status.presence_enforced {
                if !status.presence_enforced {
                    eprintln!(
                        "This platform does not enforce user presence on the credential store,\n\
                         so a passphrase is required rather than optional."
                    );
                }
                Some(read_new_passphrase()?)
            } else {
                None
            };

            let (mnemonic, address) = keystore.initialize(passphrase.as_deref())?;
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
            let mut phrase = String::new();
            std::io::stdin()
                .read_line(&mut phrase)
                .context("reading the recovery phrase")?;
            let passphrase = Some(read_new_passphrase()?);
            let address = keystore.restore(phrase.trim(), passphrase.as_deref())?;
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
            let passphrase = syndeo_keystore::passphrase::read("Passphrase: ")?;
            keystore.unseal(Some(&passphrase))?;
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

fn read_new_passphrase() -> Result<String> {
    Ok(syndeo_keystore::passphrase::read_new()?)
}
