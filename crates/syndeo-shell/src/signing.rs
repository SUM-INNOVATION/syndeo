//! `syndeo sign`: check the request, and only then start a keystore.
//!
//! The order is the point. A request that could not be shown faithfully is
//! refused before anything is started, so it never costs a keystore process,
//! a socket connection, a passphrase prompt or a signing prompt. Once a
//! keystore has been started for the command it is stopped again on every way
//! out, and the one validated request is what is shown, confirmed, signed and
//! printed.

use crate::prompt::{self, Prompter, SignatureRequest, TerminalPrompter};
use crate::service::Shell;
use crate::supervisor::Supervisor;
use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::sync::Arc;
use syndeo_ipc::confirm::{Confirmer, SessionSecret};
use syndeo_ipc::protocol::{KeystoreRequest, KeystoreResponse, ShellResponse, SignaturePurpose};
use syndeo_ipc::transport::{Channel, Endpoint};

/// How the person at the terminal agrees to what they typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Show the request and ask.
    Prompted,
    /// `--yes`: typing the message was the agreement.
    Typed,
}

/// A signature, and the canonical origin it was made for — the form the
/// keystore derived the key from, not whatever was typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    pub origin: String,
    pub address: String,
    pub public_key: String,
    pub signature: String,
}

/// The purposes `syndeo sign --purpose` accepts.
pub fn parse_purpose(purpose: &str) -> Result<SignaturePurpose> {
    Ok(match purpose {
        "login" => SignaturePurpose::OriginLogin,
        "transaction" => SignaturePurpose::ChainTransaction,
        "attestation" => SignaturePurpose::Attestation,
        other => bail!("unknown purpose {other}; use login, transaction or attestation"),
    })
}

/// Sign `message` for `origin`, the way `syndeo sign` does.
///
/// The message is both what the request says and the bytes that are signed.
/// It is checked first; only a request that passes starts a keystore under
/// `home`, which is unsealed from the terminal and stopped again before this
/// returns.
pub async fn sign_message(
    home: &Path,
    origin: &str,
    message: &str,
    purpose: SignaturePurpose,
    consent: Consent,
) -> Result<Signed> {
    sign_with(
        origin,
        message,
        purpose,
        consent,
        || SupervisedKeystore::new(Supervisor::new(home)),
        Arc::new(TerminalPrompter),
    )
    .await
}

/// Ask the keystore what it needs, then supply it. The passphrase is read here,
/// in the shell, and sent to the keystore — it never reaches the agent.
pub async fn unseal(keystore: &Endpoint) -> Result<()> {
    let mut channel = Channel::connect(keystore).await?;
    let status: KeystoreResponse = channel.call(&KeystoreRequest::Status).await?;
    let KeystoreResponse::Status {
        initialized,
        passphrase_required,
        ..
    } = status
    else {
        bail!("the keystore did not report a status");
    };
    if !initialized {
        bail!("no keystore yet — run `syndeo-keystore init` first");
    }

    let passphrase = if passphrase_required {
        Some(prompt::read_passphrase("Keystore passphrase: ").context("reading the passphrase")?)
    } else {
        None
    };

    match channel
        .call(&KeystoreRequest::Unseal { passphrase })
        .await?
    {
        KeystoreResponse::Ok => Ok(()),
        KeystoreResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply from the keystore"),
    }
}

/// A keystore started and unsealed for one command.
pub(crate) trait KeystoreSession {
    /// Start and unseal. On failure, whatever was started has been stopped
    /// before this returns.
    async fn open(&mut self) -> Result<(Endpoint, SessionSecret)>;
    /// Stop what `open` started.
    async fn close(&mut self);
}

/// The processes behind a [`SupervisedKeystore`]: a [`Supervisor`] outside
/// tests.
pub(crate) trait KeystoreProcesses {
    async fn start_keystore(&mut self, secret: &SessionSecret) -> Result<Endpoint>;
    async fn shutdown(&mut self);
}

impl KeystoreProcesses for Supervisor {
    async fn start_keystore(&mut self, secret: &SessionSecret) -> Result<Endpoint> {
        Supervisor::start_keystore(self, secret).await
    }

    async fn shutdown(&mut self) {
        Supervisor::shutdown(self).await
    }
}

/// A keystore process, started and unsealed for one command.
pub(crate) struct SupervisedKeystore<P> {
    processes: P,
}

impl<P> SupervisedKeystore<P> {
    pub(crate) fn new(processes: P) -> Self {
        SupervisedKeystore { processes }
    }
}

impl<P: KeystoreProcesses> KeystoreSession for SupervisedKeystore<P> {
    async fn open(&mut self) -> Result<(Endpoint, SessionSecret)> {
        let secret = SessionSecret::generate();
        // A failure on either step stops whatever did start, here and now,
        // rather than leaving it to a destructor at some later point.
        let keystore = match self.processes.start_keystore(&secret).await {
            Ok(keystore) => keystore,
            Err(err) => {
                self.processes.shutdown().await;
                return Err(err);
            }
        };
        if let Err(err) = unseal(&keystore).await {
            self.processes.shutdown().await;
            return Err(err);
        }
        Ok((keystore, secret))
    }

    async fn close(&mut self) {
        self.processes.shutdown().await;
    }
}

/// `syndeo sign`, with the keystore and the prompter as parameters.
///
/// `make_session` is only called once the request has passed, so an invalid
/// request is refused before a supervisor even exists.
pub(crate) async fn sign_with<S: KeystoreSession>(
    origin: &str,
    message: &str,
    purpose: SignaturePurpose,
    consent: Consent,
    make_session: impl FnOnce() -> S,
    prompter: Arc<dyn Prompter>,
) -> Result<Signed> {
    let request = SignatureRequest::new(origin, purpose, message, message.as_bytes())
        .map_err(|refusal| anyhow!("declined: {refusal}"))?;

    let mut session = make_session();
    let (keystore, secret) = session.open().await?;
    let shell = Shell::new(Arc::new(Confirmer::new(secret)), keystore, prompter);
    let response = match consent {
        Consent::Prompted => shell.sign_request(&request).await,
        Consent::Typed => shell.sign_with_typed_consent(&request).await,
    };
    // Whatever the answer was, the keystore this command started stops here.
    session.close().await;

    match response {
        ShellResponse::Signed {
            signature,
            public_key,
            address,
        } => Ok(Signed {
            origin: request.origin().to_string(),
            address,
            public_key,
            signature,
        }),
        ShellResponse::Declined(reason) => bail!("declined: {reason}"),
        ShellResponse::Error(e) => bail!(e),
        _ => bail!("unexpected reply"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::Decision;
    use crate::test_support::{CountingPrompter, FakeKeystore, Reply};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// How often a session was made, opened and closed.
    #[derive(Default)]
    struct Counts {
        made: AtomicUsize,
        opened: AtomicUsize,
        closed: AtomicUsize,
    }

    impl Counts {
        fn get(&self) -> (usize, usize, usize) {
            (
                self.made.load(Ordering::SeqCst),
                self.opened.load(Ordering::SeqCst),
                self.closed.load(Ordering::SeqCst),
            )
        }
    }

    /// A session that is already unsealed: it hands over the fake keystore and
    /// a known secret, and counts what is done with it.
    struct FakeSession {
        keystore: Endpoint,
        secret: String,
        counts: Arc<Counts>,
    }

    impl KeystoreSession for FakeSession {
        async fn open(&mut self) -> Result<(Endpoint, SessionSecret)> {
            self.counts.opened.fetch_add(1, Ordering::SeqCst);
            Ok((
                self.keystore.clone(),
                SessionSecret::from_hex(&self.secret).unwrap(),
            ))
        }

        async fn close(&mut self) {
            self.counts.closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A fake keystore that signs whatever it is asked to.
    fn signing() -> FakeKeystore {
        FakeKeystore::start(Arc::new(|request| match request {
            KeystoreRequest::SignConfirmed { .. } => KeystoreResponse::Signature {
                signature: "ab".into(),
                public_key: "cd".into(),
                address: "SUMaddress".into(),
            },
            _ => KeystoreResponse::Error("not expected".into()),
        }))
    }

    struct Run {
        result: Result<Signed>,
        counts: Arc<Counts>,
        prompts: usize,
        connections: usize,
        keystore: FakeKeystore,
        secret: String,
    }

    async fn run(
        keystore: FakeKeystore,
        origin: &str,
        message: &str,
        consent: Consent,
        answer: Decision,
    ) -> Run {
        let counts = Arc::new(Counts::default());
        let secret = SessionSecret::generate().to_hex();
        let prompter = CountingPrompter::new(answer);
        let result = sign_with(
            origin,
            message,
            SignaturePurpose::ChainTransaction,
            consent,
            || {
                counts.made.fetch_add(1, Ordering::SeqCst);
                FakeSession {
                    keystore: keystore.endpoint.clone(),
                    secret: secret.clone(),
                    counts: counts.clone(),
                }
            },
            prompter.clone(),
        )
        .await;
        Run {
            result,
            counts,
            prompts: prompter.asked(),
            connections: keystore.connections(),
            keystore,
            secret,
        }
    }

    const BOTH: [Consent; 2] = [Consent::Prompted, Consent::Typed];

    #[tokio::test]
    async fn an_unsafe_message_is_refused_before_a_keystore_is_started() {
        for consent in BOTH {
            let run = run(
                signing(),
                "https://wallet.test",
                "pay \u{202E}1 SUM",
                consent,
                Decision::Yes,
            )
            .await;
            let message = run.result.unwrap_err().to_string();
            assert!(message.starts_with("declined:"), "{consent:?}: {message}");
            assert_eq!(run.counts.get(), (0, 0, 0), "{consent:?}");
            assert_eq!(run.prompts, 0, "{consent:?}");
            assert_eq!(run.connections, 0, "{consent:?}");
        }
    }

    #[tokio::test]
    async fn an_oversized_message_is_refused_before_a_keystore_is_started() {
        let message = "a".repeat(crate::prompt::MAX_TEXT_PAYLOAD_BYTES + 1);
        for consent in BOTH {
            let run = run(
                signing(),
                "https://wallet.test",
                &message,
                consent,
                Decision::Yes,
            )
            .await;
            let refused = run.result.unwrap_err().to_string();
            assert!(refused.starts_with("declined:"), "{consent:?}: {refused}");
            assert_eq!(run.counts.get(), (0, 0, 0), "{consent:?}");
            assert_eq!(run.prompts, 0, "{consent:?}");
            assert_eq!(run.connections, 0, "{consent:?}");
        }
    }

    #[tokio::test]
    async fn an_unknown_purpose_is_refused_before_anything() {
        assert!(parse_purpose("everything").is_err());
        assert_eq!(
            parse_purpose("transaction").unwrap(),
            SignaturePurpose::ChainTransaction
        );
    }

    #[tokio::test]
    async fn a_valid_request_the_user_declines_still_closes_the_keystore() {
        let run = run(
            signing(),
            "https://wallet.test",
            "transfer 10 SUM",
            Consent::Prompted,
            Decision::No,
        )
        .await;
        assert_eq!(
            run.result.unwrap_err().to_string(),
            "declined: the user declined"
        );
        assert_eq!(run.counts.get(), (1, 1, 1));
        assert_eq!(run.prompts, 1);
        assert_eq!(run.connections, 0);
    }

    #[tokio::test]
    async fn a_keystore_error_still_closes_the_keystore() {
        for consent in BOTH {
            let run = run(
                FakeKeystore::refusing(),
                "https://wallet.test",
                "transfer 10 SUM",
                consent,
                Decision::Yes,
            )
            .await;
            assert_eq!(run.result.unwrap_err().to_string(), "the fake keystore");
            assert_eq!(run.counts.get(), (1, 1, 1), "{consent:?}");
            assert_eq!(run.connections, 1, "{consent:?}");
        }
    }

    /// One validated request, one canonical origin: the one printed, the one
    /// the confirmation covers, and the one the confirmation verifies against.
    #[tokio::test]
    async fn the_origin_printed_confirmed_and_signed_is_the_canonical_one() {
        for (consent, prompts) in [(Consent::Prompted, 1), (Consent::Typed, 0)] {
            let run = run(
                signing(),
                "HTTPS://Wallet.Test:443/",
                "transfer 10 SUM",
                consent,
                Decision::Yes,
            )
            .await;
            let signed = run.result.unwrap();
            assert_eq!(signed.origin, "https://wallet.test", "{consent:?}");
            assert_eq!(signed.address, "SUMaddress");
            assert_eq!(run.counts.get(), (1, 1, 1), "{consent:?}");
            assert_eq!(run.prompts, prompts, "{consent:?}");
            assert_eq!(run.connections, 1, "{consent:?}");

            let received = run.keystore.received();
            let [KeystoreRequest::SignConfirmed {
                confirmation,
                payload,
            }] = received.as_slice()
            else {
                panic!("expected one SignConfirmed, got {received:?}");
            };
            assert_eq!(confirmation.origin, signed.origin, "{consent:?}");
            assert_eq!(payload.as_slice(), b"transfer 10 SUM");
            let verifier = Confirmer::new(SessionSecret::from_hex(&run.secret).unwrap());
            assert_eq!(verifier.verify(confirmation, payload), Ok(()));
        }
    }

    /// Stands in for the supervisor: says whether it was asked to start or to
    /// stop, and "starts" a fake keystore that answers the status question.
    struct FakeProcesses {
        keystore: Option<FakeKeystore>,
        start_fails: bool,
        started: Arc<AtomicUsize>,
        stopped: Arc<AtomicUsize>,
    }

    impl KeystoreProcesses for FakeProcesses {
        async fn start_keystore(&mut self, _secret: &SessionSecret) -> Result<Endpoint> {
            self.started.fetch_add(1, Ordering::SeqCst);
            if self.start_fails {
                bail!("the keystore did not come up");
            }
            Ok(self.keystore.as_ref().unwrap().endpoint.clone())
        }

        async fn shutdown(&mut self) {
            self.stopped.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn status(initialized: bool) -> Reply {
        Arc::new(move |request| match request {
            KeystoreRequest::Status => KeystoreResponse::Status {
                initialized,
                unsealed: false,
                passphrase_required: false,
                presence_enforced: false,
                idle_timeout_secs: None,
                idle_for_secs: 0,
            },
            KeystoreRequest::Unseal { .. } => KeystoreResponse::Ok,
            KeystoreRequest::SignConfirmed { .. } => KeystoreResponse::Signature {
                signature: "ab".into(),
                public_key: "cd".into(),
                address: "SUMaddress".into(),
            },
            _ => KeystoreResponse::Error("not expected".into()),
        })
    }

    fn processes(keystore: Option<FakeKeystore>, start_fails: bool) -> FakeProcesses {
        FakeProcesses {
            keystore,
            start_fails,
            started: Arc::new(AtomicUsize::new(0)),
            stopped: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[tokio::test]
    async fn a_keystore_that_cannot_be_unsealed_is_stopped_before_the_error_returns() {
        let fake = processes(Some(FakeKeystore::start(status(false))), false);
        let (started, stopped) = (fake.started.clone(), fake.stopped.clone());
        let mut session = SupervisedKeystore::new(fake);

        let refused = session.open().await.err().unwrap().to_string();

        assert!(refused.contains("no keystore yet"), "{refused}");
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(stopped.load(Ordering::SeqCst), 1, "left running");
    }

    #[tokio::test]
    async fn a_keystore_that_fails_to_start_is_stopped_before_the_error_returns() {
        let fake = processes(None, true);
        let stopped = fake.stopped.clone();
        let mut session = SupervisedKeystore::new(fake);

        assert!(session.open().await.is_err());
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn the_supervised_keystore_is_stopped_once_after_a_signature() {
        let fake = processes(Some(FakeKeystore::start(status(true))), false);
        let (started, stopped) = (fake.started.clone(), fake.stopped.clone());
        let slot = std::sync::Mutex::new(Some(fake));

        let signed = sign_with(
            "https://wallet.test",
            "transfer 10 SUM",
            SignaturePurpose::ChainTransaction,
            Consent::Typed,
            || SupervisedKeystore::new(slot.lock().unwrap().take().unwrap()),
            CountingPrompter::new(Decision::No),
        )
        .await
        .unwrap();

        assert_eq!(signed.origin, "https://wallet.test");
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_invalid_request_never_makes_a_supervised_keystore() {
        let fake = processes(Some(FakeKeystore::start(status(true))), false);
        let (started, stopped) = (fake.started.clone(), fake.stopped.clone());
        let slot = std::sync::Mutex::new(Some(fake));

        let refused = sign_with(
            "https://wallet.test/a/path",
            "transfer 10 SUM",
            SignaturePurpose::ChainTransaction,
            Consent::Typed,
            || SupervisedKeystore::new(slot.lock().unwrap().take().unwrap()),
            CountingPrompter::new(Decision::Yes),
        )
        .await;

        assert!(refused.is_err());
        assert!(slot.lock().unwrap().is_some(), "a session was made");
        assert_eq!(started.load(Ordering::SeqCst), 0);
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
    }
}
