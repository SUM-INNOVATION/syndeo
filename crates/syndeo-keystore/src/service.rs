//! The keystore's request loop.
//!
//! One connection at a time, one request at a time. There is no concurrency to
//! win here and every reason to keep the surface small.

use crate::keystore::{Keystore, KeystoreError};
use std::sync::Arc;
use std::time::Duration;
use syndeo_ipc::confirm::Confirmer;
use syndeo_ipc::protocol::{KeystoreRequest, KeystoreResponse};
use syndeo_ipc::transport::Server;

/// How often the idle policy is checked. Fine enough that a locked screen takes
/// effect promptly, coarse enough to cost nothing.
const TICK: Duration = Duration::from_secs(5);

pub async fn serve(keystore: Arc<Keystore>, confirmer: Arc<Confirmer>, server: Server) {
    tracing::info!(socket = %server.endpoint().path().display(), "keystore listening");
    if let Some(timeout) = keystore.status().idle_timeout_secs {
        tracing::info!(timeout, "the seed is forgotten after this many idle seconds");
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

        KeystoreRequest::Initialize { passphrase } => {
            match keystore.initialize(passphrase.as_deref()) {
                Ok((mnemonic, address)) => KeystoreResponse::Initialized {
                    mnemonic: mnemonic.to_string(),
                    address: address.to_base58(),
                },
                Err(err) => refuse(err),
            }
        }

        KeystoreRequest::Restore {
            mnemonic,
            passphrase,
        } => match keystore.restore(&mnemonic, passphrase.as_deref()) {
            Ok(address) => KeystoreResponse::Identity {
                public_key: String::new(),
                address: address.to_base58(),
            },
            Err(err) => refuse(err),
        },

        KeystoreRequest::Unseal { passphrase } => match keystore.unseal(passphrase.as_deref()) {
            Ok(()) => KeystoreResponse::Ok,
            Err(err) => refuse(err),
        },

        KeystoreRequest::Lock => {
            keystore.lock();
            KeystoreResponse::Ok
        }
    }
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
