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

            ShellRequest::IdentityFor { origin } => self.identity_for(&origin).await,

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

    /// The agent asks which identity this user has at a site.
    ///
    /// A public key is not a secret, but the set of them is: the identity for
    /// each site is derived separately precisely so that nobody can tell two
    /// sites' identities belong to one person. An agent that could collect
    /// them for any sites it named could link them. So the origin is checked
    /// and put in canonical form exactly as a signing request's is, a run with
    /// nobody to ask declines, and the person at the screen is asked. Only a
    /// yes reaches the keystore: every other path contacts it not at all.
    async fn identity_for(&self, origin: &str) -> ShellResponse {
        let origin = match crate::prompt::ValidatedOrigin::parse(origin) {
            Ok(origin) => origin,
            Err(refusal) => return ShellResponse::Error(format!("refused: {refusal}")),
        };
        if !self.prompter.is_interactive() {
            return ShellResponse::Declined("this run is not interactive".into());
        }
        let detail = format!(
            "The agent is asking for your public key and address at {origin}. \
             Anything it is given can be matched against what it collects for \
             other sites."
        );
        if self
            .prompter
            .ask("Reveal your identity for this site to the agent?", &detail)
            != Decision::Yes
        {
            return ShellResponse::Declined("the user declined".into());
        }
        match self
            .keystore_call(KeystoreRequest::PublicIdentity {
                origin: origin.to_string(),
            })
            .await
        {
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

    /// Sign something the user typed themselves.
    ///
    /// `syndeo sign --origin ... --message ... --yes` is a human stating the
    /// exact payload on a command line, which is consent to that payload by the
    /// same standard the dialog applies. It is deliberately not reachable from
    /// the agent boundary: an agent's payload was never typed by anyone.
    ///
    /// The same standard includes the checks, which is why this takes a
    /// [`SignatureRequest`] rather than its parts: the only way to have one is
    /// to have passed them. Nobody is shown this request, but it is minted and
    /// signed over the same validated, canonical fields a prompted one would
    /// be, so `--yes` is never a way around them.
    pub async fn sign_with_typed_consent(&self, request: &SignatureRequest) -> ShellResponse {
        self.sign_confirmed(request).await
    }

    /// Ask the human about a request that has already been checked, and sign it
    /// if they agree.
    ///
    /// For a front end that validated the request itself, before doing
    /// anything else — `syndeo sign` validates before it starts a keystore. The
    /// request that is shown is the one that is confirmed and signed.
    pub async fn sign_request(&self, request: &SignatureRequest) -> ShellResponse {
        if !self.prompter.is_interactive() {
            return ShellResponse::Declined(
                "a signature needs a human, and this run is not interactive".into(),
            );
        }

        // The user sees the origin, the purpose, the description and the exact
        // bytes. Nothing is signed that was not on screen.
        if self.prompter.ask_to_sign(request) == Decision::No {
            return ShellResponse::Declined("the user declined".into());
        }

        self.sign_confirmed(request).await
    }

    /// The whole point of the boundary, in one function: what arrives from the
    /// agent is raw, so it is checked here before anything else happens.
    async fn sign(
        &self,
        origin: String,
        purpose: SignaturePurpose,
        description: String,
        payload: Vec<u8>,
    ) -> ShellResponse {
        // Checked first, before anything else happens. A request that could not
        // be shown faithfully is refused here, and nobody is asked about it,
        // nothing is minted for it, and the keystore never hears of it.
        let request = match SignatureRequest::new(origin, purpose, description, payload) {
            Ok(request) => request,
            Err(refusal) => return ShellResponse::Declined(refusal.to_string()),
        };
        self.sign_request(&request).await
    }

    /// Mint the confirmation and ask the keystore to honour it.
    ///
    /// Everything comes from the one validated request: the origin, purpose,
    /// description and payload the confirmation covers are the ones that were
    /// on screen, and the payload sent is the payload the confirmation names.
    /// There are no separate copies to drift apart.
    async fn sign_confirmed(&self, request: &SignatureRequest) -> ShellResponse {
        // The confirmation is minted here, in the shell, over exactly those
        // bytes. The agent never holds the secret that makes it valid.
        let confirmation = self.confirmer.issue(
            request.origin(),
            request.purpose(),
            request.description(),
            request.payload(),
        );

        match self
            .keystore_call(KeystoreRequest::SignConfirmed {
                confirmation,
                payload: request.payload().to_vec(),
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

        match self
            .call_once(&KeystoreRequest::Unseal { passphrase })
            .await?
        {
            KeystoreResponse::Ok => Ok(()),
            KeystoreResponse::Error(e) => anyhow::bail!(e),
            _ => anyhow::bail!("unexpected reply from the keystore"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{CountingPrompter, FakeKeystore};
    use syndeo_ipc::confirm::SessionSecret;

    /// A shell whose keystore is a listener this test owns and that refuses
    /// everything, so a call returns cleanly without anything being signed.
    struct Harness {
        keystore: FakeKeystore,
        confirmer: Arc<Confirmer>,
        prompter: Arc<CountingPrompter>,
        shell: Shell,
    }

    impl Harness {
        fn start(answer: Decision) -> Self {
            let keystore = FakeKeystore::refusing();
            let confirmer = Arc::new(Confirmer::new(SessionSecret::generate()));
            let prompter = CountingPrompter::new(answer);
            let shell = Shell::new(
                confirmer.clone(),
                keystore.endpoint.clone(),
                prompter.clone(),
            );
            Harness {
                keystore,
                confirmer,
                prompter,
                shell,
            }
        }

        fn prompts(&self) -> usize {
            self.prompter.asked()
        }

        fn connections(&self) -> usize {
            self.keystore.connections()
        }

        /// The agent's way in: raw fields, checked inside the shell.
        async fn prompted(&self, origin: &str, description: &str, payload: &[u8]) -> ShellResponse {
            self.shell
                .handle(ShellRequest::RequestSignature {
                    origin: origin.into(),
                    purpose: SignaturePurpose::ChainTransaction,
                    description: description.into(),
                    payload: payload.to_vec(),
                })
                .await
        }
    }

    fn request(origin: &str, description: &str, payload: &[u8]) -> SignatureRequest {
        SignatureRequest::new(
            origin,
            SignaturePurpose::ChainTransaction,
            description,
            payload,
        )
        .unwrap()
    }

    const ORIGIN: &str = "https://wallet.test";
    const DESCRIPTION: &str = "Send 10 SUM to alice";
    const PAYLOAD: &[u8] = b"transfer 10 SUM to alice";

    #[tokio::test]
    async fn an_unsafe_description_from_the_agent_is_refused_before_anyone_is_asked() {
        let harness = Harness::start(Decision::Yes);
        for description in ["Send 10 SUM to \u{202E}ecila", "Send 10 SUM\nto alice"] {
            let prompted = harness.prompted(ORIGIN, description, PAYLOAD).await;
            assert!(
                matches!(prompted, ShellResponse::Declined(_)),
                "{prompted:?}"
            );
        }

        assert_eq!(harness.prompts(), 0);
        assert_eq!(harness.connections(), 0);
    }

    #[tokio::test]
    async fn an_oversized_payload_from_the_agent_is_refused_before_anyone_is_asked() {
        let harness = Harness::start(Decision::Yes);
        for payload in [
            vec![b'a'; crate::prompt::MAX_TEXT_PAYLOAD_BYTES + 1],
            vec![0xff; crate::prompt::MAX_HEX_PAYLOAD_BYTES + 1],
        ] {
            let prompted = harness.prompted(ORIGIN, DESCRIPTION, &payload).await;
            assert!(
                matches!(prompted, ShellResponse::Declined(_)),
                "{prompted:?}"
            );
        }

        assert_eq!(harness.prompts(), 0);
        assert_eq!(harness.connections(), 0);
    }

    #[tokio::test]
    async fn a_valid_request_the_user_declines_never_reaches_the_keystore() {
        let harness = Harness::start(Decision::No);
        let response = harness.prompted(ORIGIN, DESCRIPTION, PAYLOAD).await;
        assert!(
            matches!(response, ShellResponse::Declined(_)),
            "{response:?}"
        );
        assert_eq!(harness.prompts(), 1);
        assert_eq!(harness.connections(), 0);
    }

    #[tokio::test]
    async fn typed_consent_sends_the_keystore_the_canonical_origin_once() {
        let harness = Harness::start(Decision::No);
        let request = request("HTTPS://Wallet.Test:443/", DESCRIPTION, PAYLOAD);
        let response = harness.shell.sign_with_typed_consent(&request).await;
        assert!(
            matches!(&response, ShellResponse::Error(e) if e == "the fake keystore"),
            "{response:?}"
        );
        assert_eq!(harness.prompts(), 0);
        assert_eq!(harness.connections(), 1);

        let received = harness.keystore.received();
        let [KeystoreRequest::SignConfirmed {
            confirmation,
            payload,
        }] = received.as_slice()
        else {
            panic!("expected one SignConfirmed, got {received:?}");
        };
        assert_eq!(confirmation.origin, ORIGIN);
        assert_eq!(payload.as_slice(), PAYLOAD);
        assert_eq!(
            confirmation.description_hash,
            blake3::hash(DESCRIPTION.as_bytes()).to_hex().to_string()
        );
        // Minted by this shell's confirmer over exactly what was sent.
        assert_eq!(harness.confirmer.verify(confirmation, payload), Ok(()));
    }

    #[tokio::test]
    async fn an_approved_request_is_signed_over_the_canonical_origin() {
        let harness = Harness::start(Decision::Yes);
        let response = harness
            .prompted("https://WALLET.test/", DESCRIPTION, PAYLOAD)
            .await;
        assert!(matches!(response, ShellResponse::Error(_)), "{response:?}");
        assert_eq!(harness.prompts(), 1);
        assert_eq!(harness.connections(), 1);

        let received = harness.keystore.received();
        let [KeystoreRequest::SignConfirmed { confirmation, .. }] = received.as_slice() else {
            panic!("expected one SignConfirmed, got {received:?}");
        };
        assert_eq!(confirmation.origin, ORIGIN);
    }

    // ------------------------------------------------ identity for the agent

    fn identity_harness(answer: Decision) -> (FakeKeystore, Arc<CountingPrompter>, Shell) {
        let keystore = FakeKeystore::start(Arc::new(|request| match request {
            KeystoreRequest::PublicIdentity { .. } => KeystoreResponse::Identity {
                public_key: "k".into(),
                address: "a".into(),
            },
            _ => KeystoreResponse::Error("unexpected".into()),
        }));
        let prompter = CountingPrompter::new(answer);
        let shell = Shell::new(
            Arc::new(Confirmer::new(SessionSecret::generate())),
            keystore.endpoint.clone(),
            prompter.clone(),
        );
        (keystore, prompter, shell)
    }

    async fn ask_identity(shell: &Shell, origin: &str) -> ShellResponse {
        shell
            .handle(ShellRequest::IdentityFor {
                origin: origin.into(),
            })
            .await
    }

    #[tokio::test]
    async fn an_invalid_origin_is_refused_before_anyone_is_asked() {
        let (keystore, prompter, shell) = identity_harness(Decision::Yes);
        for origin in [
            "not a url",
            "ftp://a.test",
            "https://a.test/path",
            "https://a\u{202e}.test",
        ] {
            let response = ask_identity(&shell, origin).await;
            assert!(
                matches!(response, ShellResponse::Error(_)),
                "{origin}: {response:?}"
            );
        }
        assert_eq!(prompter.asked(), 0);
        assert_eq!(keystore.connections(), 0);
    }

    /// Says nobody is there — and would say yes if asked anyway, so only the
    /// shell's own check keeps a non-interactive run from reaching the keystore.
    struct AbsentButAgreeable {
        asked: std::sync::atomic::AtomicUsize,
    }

    impl Prompter for AbsentButAgreeable {
        fn ask_to_sign(&self, _: &SignatureRequest) -> Decision {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Decision::Yes
        }
        fn ask(&self, _: &str, _: &str) -> Decision {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Decision::Yes
        }
        fn read_passphrase(&self, _: &str) -> std::io::Result<syndeo_ipc::SecretString> {
            Err(std::io::Error::other("nobody"))
        }
        fn is_interactive(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn a_run_with_nobody_to_ask_declines_without_asking_or_touching_the_keystore() {
        let keystore = FakeKeystore::refusing();
        let prompter = Arc::new(AbsentButAgreeable {
            asked: Default::default(),
        });
        let shell = Shell::new(
            Arc::new(Confirmer::new(SessionSecret::generate())),
            keystore.endpoint.clone(),
            prompter.clone(),
        );
        let response = ask_identity(&shell, "https://a.test").await;
        assert!(
            matches!(response, ShellResponse::Declined(_)),
            "{response:?}"
        );
        assert_eq!(prompter.asked.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(keystore.connections(), 0);
    }

    #[tokio::test]
    async fn no_means_no_keystore_call_at_all() {
        let (keystore, prompter, shell) = identity_harness(Decision::No);
        let response = ask_identity(&shell, "https://a.test").await;
        assert!(
            matches!(response, ShellResponse::Declined(_)),
            "{response:?}"
        );
        assert_eq!(prompter.asked(), 1);
        assert_eq!(keystore.connections(), 0);
    }

    #[tokio::test]
    async fn yes_asks_the_keystore_once_for_the_canonical_origin() {
        let (keystore, prompter, shell) = identity_harness(Decision::Yes);
        let response = ask_identity(&shell, "HTTPS://A.test:443/").await;
        assert!(
            matches!(response, ShellResponse::Identity { .. }),
            "{response:?}"
        );
        assert_eq!(prompter.asked(), 1);
        assert_eq!(keystore.connections(), 1);
        let received = keystore.received();
        let [KeystoreRequest::PublicIdentity { origin }] = received.as_slice() else {
            panic!("expected one PublicIdentity, got {received:?}");
        };
        assert_eq!(origin, "https://a.test");
    }
}
