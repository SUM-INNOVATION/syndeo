//! The shell's side of the agent boundary.
//!
//! The agent asks; the shell decides. Every request that could move value or
//! prove identity goes in front of a human first, with the exact bytes rendered,
//! and only then does the shell — never the agent — speak to the keystore.

use crate::prompt::{self, Decision};
use anyhow::Result;
use std::sync::Arc;
use syndeo_ipc::confirm::Confirmer;
use syndeo_ipc::protocol::{
    KeystoreRequest, KeystoreResponse, ShellRequest, ShellResponse, SignaturePurpose,
};
use syndeo_ipc::transport::{Channel, Endpoint, Server};
use tokio::sync::Mutex;

/// Everything the shell needs to answer the agent. The keystore endpoint lives
/// here and is never handed out.
pub struct Shell {
    confirmer: Arc<Confirmer>,
    keystore: Endpoint,
    /// One keystore conversation at a time; the keystore has no concurrency to win.
    lock: Mutex<()>,
    /// When false, every prompt is auto-declined instead of asking. Used by
    /// non-interactive runs so an agent can never hang waiting on a human that
    /// is not there.
    interactive: bool,
}

impl Shell {
    pub fn new(confirmer: Arc<Confirmer>, keystore: Endpoint, interactive: bool) -> Self {
        Shell {
            confirmer,
            keystore,
            lock: Mutex::new(()),
            interactive,
        }
    }

    pub async fn serve(self: Arc<Self>, server: Server) {
        tracing::info!(socket = %server.endpoint().path().display(), "shell listening for the agent");
        loop {
            let mut framed = match server.accept().await {
                Ok(f) => f,
                Err(err) => {
                    tracing::warn!(%err, "accept failed");
                    continue;
                }
            };
            let shell = self.clone();
            tokio::spawn(async move {
                loop {
                    let request: ShellRequest = match framed.recv().await {
                        Ok(r) => r,
                        Err(syndeo_ipc::FrameError::Closed) => return,
                        Err(err) => {
                            tracing::debug!(%err, "malformed request");
                            return;
                        }
                    };
                    let response = shell.handle(request).await;
                    if framed.send(&response).await.is_err() {
                        return;
                    }
                }
            });
        }
    }

    pub async fn handle(&self, request: ShellRequest) -> ShellResponse {
        match request {
            ShellRequest::RequestSignature {
                origin,
                purpose,
                description,
                payload,
            } => self.sign(origin, purpose, description, payload).await,

            ShellRequest::IdentityFor { origin } => {
                match self.keystore_call(KeystoreRequest::PublicIdentity { origin }).await {
                    Ok(KeystoreResponse::Identity {
                        public_key,
                        address,
                    }) => ShellResponse::Identity {
                        public_key,
                        address,
                    },
                    Ok(KeystoreResponse::Error(e)) => ShellResponse::Error(e),
                    Ok(_) => ShellResponse::Error("unexpected keystore reply".into()),
                    Err(e) => ShellResponse::Error(e.to_string()),
                }
            }

            ShellRequest::Confirm { title, detail } => {
                if !self.interactive {
                    return ShellResponse::Declined("this run is not interactive".into());
                }
                match prompt::ask(&title, &detail) {
                    Decision::Yes => ShellResponse::Confirmed(true),
                    Decision::No => ShellResponse::Confirmed(false),
                }
            }

            ShellRequest::Ping => ShellResponse::Pong,
        }
    }

    /// Sign something the user typed themselves.
    ///
    /// `syndeo sign --origin ... --message ... --yes` is a human stating the
    /// exact payload on a command line, which is consent to that payload by the
    /// same standard the dialog applies. It is deliberately not reachable from
    /// the agent boundary: an agent's payload was never typed by anyone.
    pub async fn sign_with_typed_consent(
        &self,
        origin: String,
        purpose: SignaturePurpose,
        description: String,
        payload: Vec<u8>,
    ) -> ShellResponse {
        let confirmation = self
            .confirmer
            .issue(&origin, purpose, &description, &payload);
        match self
            .keystore_call(KeystoreRequest::SignConfirmed {
                confirmation,
                payload,
            })
            .await
        {
            Ok(KeystoreResponse::Signature {
                signature,
                public_key,
                address,
            }) => ShellResponse::Signed {
                signature,
                public_key,
                address,
            },
            Ok(KeystoreResponse::Error(e)) => ShellResponse::Error(e),
            Ok(_) => ShellResponse::Error("unexpected keystore reply".into()),
            Err(e) => ShellResponse::Error(e.to_string()),
        }
    }

    /// The whole point of the boundary, in one function.
    async fn sign(
        &self,
        origin: String,
        purpose: SignaturePurpose,
        description: String,
        payload: Vec<u8>,
    ) -> ShellResponse {
        if !self.interactive {
            return ShellResponse::Declined(
                "a signature needs a human, and this run is not interactive".into(),
            );
        }

        // The user sees the origin, the purpose, the description and the exact
        // bytes. Nothing is signed that was not on screen.
        if prompt::ask_to_sign(&origin, purpose, &description, &payload) == Decision::No {
            return ShellResponse::Declined("the user declined".into());
        }

        // The confirmation is minted here, in the shell, over exactly those
        // bytes. The agent never holds the secret that makes it valid.
        let confirmation = self
            .confirmer
            .issue(&origin, purpose, &description, &payload);

        match self
            .keystore_call(KeystoreRequest::SignConfirmed {
                confirmation,
                payload,
            })
            .await
        {
            Ok(KeystoreResponse::Signature {
                signature,
                public_key,
                address,
            }) => ShellResponse::Signed {
                signature,
                public_key,
                address,
            },
            Ok(KeystoreResponse::Error(e)) => ShellResponse::Error(e),
            Ok(_) => ShellResponse::Error("unexpected keystore reply".into()),
            Err(e) => ShellResponse::Error(e.to_string()),
        }
    }

    /// The only path to the keystore in the whole tree.
    pub async fn keystore_call(&self, request: KeystoreRequest) -> Result<KeystoreResponse> {
        let _guard = self.lock.lock().await;
        let mut channel = Channel::connect(&self.keystore).await?;
        Ok(channel.call(&request).await?)
    }
}
