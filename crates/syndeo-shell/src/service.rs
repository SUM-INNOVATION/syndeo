//! The shell's side of the agent boundary.
//!
//! The agent asks; the shell decides. Every request that could move value or
//! prove identity goes in front of a human first, with the exact bytes rendered,
//! and only then does the shell — never the agent — speak to the keystore.

use crate::prompt::{Decision, Prompter, SignatureRequest};
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
    /// How this front end asks a human. A run with nobody to ask holds a
    /// `NonInteractive`, so an agent cannot hang waiting on someone who is not
    /// there — the guarantee is in the type rather than in a flag.
    prompter: Arc<dyn Prompter>,
}

impl Shell {
    pub fn new(confirmer: Arc<Confirmer>, keystore: Endpoint, prompter: Arc<dyn Prompter>) -> Self {
        Shell {
            confirmer,
            keystore,
            lock: Mutex::new(()),
            prompter,
        }
    }

    pub fn prompter(&self) -> &Arc<dyn Prompter> {
        &self.prompter
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
                if !self.prompter.is_interactive() {
                    return ShellResponse::Declined("this run is not interactive".into());
                }
                match self.prompter.ask(&title, &detail) {
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
        if !self.prompter.is_interactive() {
            return ShellResponse::Declined(
                "a signature needs a human, and this run is not interactive".into(),
            );
        }

        // The user sees the origin, the purpose, the description and the exact
        // bytes. Nothing is signed that was not on screen.
        let request = SignatureRequest {
            origin: origin.clone(),
            purpose,
            description: description.clone(),
            payload: payload.clone(),
        };
        if self.prompter.ask_to_sign(&request) == Decision::No {
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
    ///
    /// A locked keystore is not a failure to report upward. The seed is
    /// forgotten on a timer now, so an operation arriving after a quiet spell is
    /// the normal case, and the right answer is to ask the user to unseal and
    /// carry on — not to make them retype the command.
    pub async fn keystore_call(&self, request: KeystoreRequest) -> Result<KeystoreResponse> {
        let _guard = self.lock.lock().await;

        match self.call_once(&request).await? {
            KeystoreResponse::Locked => {}
            other => return Ok(other),
        }

        if !self.prompter.is_interactive() {
            return Ok(KeystoreResponse::Error(
                "the keystore is locked and this run has no one to ask for a passphrase".into(),
            ));
        }

        self.unseal_now().await?;
        // Once. A second `Locked` means the session ended again between the
        // unseal and the retry, and looping on that is how you get a prompt
        // storm rather than a working keystore.
        match self.call_once(&request).await? {
            KeystoreResponse::Locked => Ok(KeystoreResponse::Error(
                "the keystore locked again immediately after unsealing".into(),
            )),
            other => Ok(other),
        }
    }

    /// One request, on its own connection. Does not take the lock.
    async fn call_once(&self, request: &KeystoreRequest) -> Result<KeystoreResponse> {
        let mut channel = Channel::connect(&self.keystore).await?;
        Ok(channel.call(request).await?)
    }

    /// Ask the keystore what it needs, then supply it. The passphrase is read
    /// here, in the shell, and never reaches the agent.
    async fn unseal_now(&self) -> Result<()> {
        let KeystoreResponse::Status {
            passphrase_required,
            ..
        } = self.call_once(&KeystoreRequest::Status).await?
        else {
            anyhow::bail!("the keystore did not report a status");
        };

        let passphrase = if passphrase_required {
            Some(
                self.prompter
                    .read_passphrase("The keystore locked itself. Passphrase: ")?,
            )
        } else {
            None
        };

        match self.call_once(&KeystoreRequest::Unseal { passphrase }).await? {
            KeystoreResponse::Ok => Ok(()),
            KeystoreResponse::Error(e) => anyhow::bail!(e),
            _ => anyhow::bail!("unexpected reply from the keystore"),
        }
    }
}
