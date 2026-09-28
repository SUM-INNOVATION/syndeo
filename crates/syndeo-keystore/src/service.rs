//! The keystore's request loop.
//!
//! One connection at a time, one request at a time. There is no concurrency to
//! win here and every reason to keep the surface small.

use crate::keystore::{Keystore, KeystoreError};
use std::sync::Arc;
use std::time::Duration;
use syndeo_ipc::confirm::Confirmer;
use syndeo_ipc::protocol::{KeystoreRequest, KeystoreResponse, ScreenLockReport};
use syndeo_ipc::transport::Server;
use syndeo_ipc::SecretString;

/// How often the idle policy is checked. Fine enough that a locked screen takes
/// effect promptly, coarse enough to cost nothing.
const TICK: Duration = Duration::from_secs(5);

pub async fn serve(keystore: Arc<Keystore>, confirmer: Arc<Confirmer>, server: Server) {
    tracing::info!(socket = %server.endpoint().path().display(), "keystore listening");
    if let Some(timeout) = keystore.status().idle_timeout_secs {
        tracing::info!(
            timeout,
            "the seed is forgotten after this many idle seconds"
        );
    }
    // Nothing was calling `lock`, so an unsealed session lasted as long as the
    // process. This is what ends one.
    tokio::spawn({
        let keystore = keystore.clone();
        async move {
            loop {
                tokio::time::sleep(TICK).await;
                keystore.lock_if_expired();
            }
        }
    });

    loop {
        let mut framed = match server.accept().await {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(%err, "accept failed");
                continue;
            }
        };
        let keystore = keystore.clone();
        let confirmer = confirmer.clone();
        tokio::spawn(async move {
            loop {
                let request: KeystoreRequest = match framed.recv().await {
                    Ok(r) => r,
                    Err(syndeo_ipc::FrameError::Closed) => return,
                    Err(err) => {
                        tracing::debug!(%err, "malformed request");
                        return;
                    }
                };
                let response = handle(&keystore, &confirmer, request);
                if framed.send(&response).await.is_err() {
                    return;
                }
            }
        });
    }
}

pub fn handle(
    keystore: &Keystore,
    confirmer: &Confirmer,
    request: KeystoreRequest,
) -> KeystoreResponse {
    match request {
        KeystoreRequest::SignConfirmed {
            confirmation,
            payload,
        } => match keystore.sign_confirmed(confirmer, &confirmation, &payload) {
            Ok(signed) => {
                tracing::info!(
                    origin = %confirmation.origin,
                    purpose = confirmation.purpose.as_str(),
                    address = %signed.address,
                    "signed after shell confirmation"
                );
                KeystoreResponse::Signature {
                    signature: signed.signature,
                    public_key: signed.public_key,
                    address: signed.address.to_base58(),
                }
            }
            Err(err) => refuse(err),
        },

        KeystoreRequest::PublicIdentity { origin } => match keystore.public_identity(&origin) {
            Ok((public_key, address)) => KeystoreResponse::Identity {
                public_key,
                address: address.to_base58(),
            },
            Err(err) => refuse(err),
        },

        KeystoreRequest::SessionProtection => {
            let status = keystore.status();
            KeystoreResponse::SessionProtection {
                initialized: status.initialized,
                unsealed: status.unsealed,
                idle_timeout_secs: status.idle_timeout_secs,
                // Checked on every tick of the loop above, on every platform.
                sleep_detection: true,
                // Asked now, of this session: the same question the idle
                // watch asks before it decides the screen has locked.
                screen_lock: match crate::session::screen_is_locked() {
                    Some(locked) => ScreenLockReport::Reported { locked },
                    None => ScreenLockReport::NotReported,
                },
            }
        }

        KeystoreRequest::Status => {
            let status = keystore.status();
            KeystoreResponse::Status {
                initialized: status.initialized,
                unsealed: status.unsealed,
                passphrase_required: status.passphrase_required,
                presence_enforced: status.presence_enforced,
                idle_timeout_secs: status.idle_timeout_secs,
                idle_for_secs: status.idle_for_secs,
            }
        }

        // Enrolment is not served here. Both requests write the credential
        // store entry every keystore home on the machine shares, `Restore`
        // unconditionally, so a connection that could send them could replace
        // the wrapping key a sealed seed depends on; and `Initialize` would put
        // a recovery phrase on a socket. Nothing legitimate sends either: the
        // shell and the UI only unseal, and enrolment is `syndeo-keystore init`
        // or `restore` at a terminal, which open the keystore directly. The
        // variants stay in the protocol so the wire format does not move; the
        // keystore is not touched, and what the request carried is scrubbed.
        // (Both arrive as `SecretString`s, which wipe themselves when they
        // are dropped at the end of their arm.)
        KeystoreRequest::Initialize { passphrase } => {
            drop(passphrase);
            refuse_enrolment("initialize")
        }

        KeystoreRequest::Restore {
            mnemonic,
            passphrase,
        } => {
            drop(mnemonic);
            drop(passphrase);
            refuse_enrolment("restore")
        }

        // The passphrase is wiped when this arm ends, whether it unsealed or not.
        KeystoreRequest::Unseal { passphrase } => {
            match keystore.unseal(passphrase.as_ref().map(SecretString::expose)) {
                Ok(()) => KeystoreResponse::Ok,
                Err(err) => refuse(err),
            }
        }

        KeystoreRequest::Lock => {
            keystore.lock();
            KeystoreResponse::Ok
        }
    }
}

/// The answer to an enrolment request, whoever sent it.
fn refuse_enrolment(operation: &str) -> KeystoreResponse {
    tracing::warn!(
        operation,
        "refused: enrolment is not offered over the socket"
    );
    KeystoreResponse::Error(format!(
        "{operation} is not available over the keystore socket; \
         run `syndeo-keystore {operation}` in a terminal"
    ))
}

/// Errors cross the boundary as text. Nothing derived from key material, the
/// passphrase, or the seed is ever put in one.
///
/// A locked keystore is the exception, and gets its own variant: the shell's
/// answer to it is to ask the user to unseal and try again, and matching on a
/// string to decide that would be a bug waiting for someone to reword an error.
fn refuse(err: KeystoreError) -> KeystoreResponse {
    if matches!(err, KeystoreError::Locked) {
        tracing::info!("refused: the keystore is locked");
        return KeystoreResponse::Locked;
    }
    tracing::warn!(%err, "refused");
    KeystoreResponse::Error(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapping::{InMemoryKeyStore, WrappingKeyStore};
    use syndeo_ipc::confirm::SessionSecret;

    const PASS: &str = "a passphrase the user chose";

    /// A phrase that is not the one the keystore holds, so a restore that got
    /// through would visibly change both the seed and the wrapping key.
    const OTHER_PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon \
        abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
        abandon abandon abandon abandon abandon abandon art";

    fn keystore() -> (tempfile::TempDir, Keystore, Arc<InMemoryKeyStore>) {
        let dir = tempfile::tempdir().unwrap();
        let wrapping = Arc::new(InMemoryKeyStore::default());
        let keystore = Keystore::open(dir.path(), wrapping.clone()).unwrap();
        (dir, keystore, wrapping)
    }

    fn sealed(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join(".keystore").join("seed.sealed")
    }

    fn confirmer() -> Confirmer {
        Confirmer::new(SessionSecret::generate())
    }

    fn enrolment_requests() -> Vec<KeystoreRequest> {
        vec![
            KeystoreRequest::Initialize {
                passphrase: Some(PASS.into()),
            },
            KeystoreRequest::Initialize { passphrase: None },
            KeystoreRequest::Restore {
                mnemonic: OTHER_PHRASE.into(),
                passphrase: Some(PASS.into()),
            },
        ]
    }

    #[test]
    fn enrolment_over_the_socket_changes_nothing_that_exists() {
        let (dir, keystore, wrapping) = keystore();
        keystore.initialize(Some(PASS)).unwrap();
        keystore.lock();
        let seed_before = std::fs::read(sealed(&dir)).unwrap();
        let key_before = wrapping.stored().unwrap();

        for request in enrolment_requests() {
            let response = handle(&keystore, &confirmer(), request);
            assert!(
                matches!(response, KeystoreResponse::Error(_)),
                "enrolment was served: {response:?}"
            );
            assert_eq!(std::fs::read(sealed(&dir)).unwrap(), seed_before);
            assert_eq!(wrapping.stored(), Some(key_before));
            assert!(!keystore.status().unsealed);
        }

        // What was there still opens.
        keystore.unseal(Some(PASS)).unwrap();
    }

    #[test]
    fn enrolment_over_the_socket_creates_nothing_either() {
        let (dir, keystore, wrapping) = keystore();

        for request in enrolment_requests() {
            let response = handle(&keystore, &confirmer(), request);
            let KeystoreResponse::Error(message) = response else {
                panic!("enrolment was served: {response:?}");
            };
            assert!(message.contains("in a terminal"), "{message}");
            assert!(!sealed(&dir).exists(), "a seed was sealed over the socket");
            assert_eq!(wrapping.stored(), None);
            assert!(!wrapping.exists().unwrap());
            assert!(!keystore.status().initialized);
        }
    }

    #[test]
    fn session_protection_reports_this_keystore_and_this_session() {
        let (_dir, keystore, _) = keystore();
        let before = handle(&keystore, &confirmer(), KeystoreRequest::SessionProtection);
        let KeystoreResponse::SessionProtection {
            initialized,
            unsealed,
            sleep_detection,
            screen_lock,
            ..
        } = before
        else {
            panic!("not answered: {before:?}");
        };
        assert!(!initialized && !unsealed && sleep_detection);
        // Whatever this machine's session says, and nothing it does not.
        let expected = match crate::session::screen_is_locked() {
            Some(locked) => ScreenLockReport::Reported { locked },
            None => ScreenLockReport::NotReported,
        };
        assert_eq!(screen_lock, expected);

        keystore.initialize(Some(PASS)).unwrap();
        let after = handle(&keystore, &confirmer(), KeystoreRequest::SessionProtection);
        assert!(
            matches!(
                after,
                KeystoreResponse::SessionProtection {
                    initialized: true,
                    ..
                }
            ),
            "{after:?}"
        );
    }
}
