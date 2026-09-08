//! The keystore's request loop.
//!
//! One connection at a time, one request at a time. There is no concurrency to
//! win here and every reason to keep the surface small.

use crate::keystore::{Keystore, KeystoreError};
use std::sync::Arc;
use syndeo_ipc::confirm::Confirmer;
use syndeo_ipc::protocol::{KeystoreRequest, KeystoreResponse};
use syndeo_ipc::transport::Server;

pub async fn serve(keystore: Arc<Keystore>, confirmer: Arc<Confirmer>, server: Server) {
    tracing::info!(socket = %server.endpoint().path().display(), "keystore listening");
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
fn refuse(err: KeystoreError) -> KeystoreResponse {
    tracing::warn!(%err, "refused");
    KeystoreResponse::Error(err.to_string())
}
