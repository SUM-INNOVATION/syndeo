//! The windowed confirmation dialog.
//!
//! What matters about this is not that it is a window. It is that the payload
//! on screen is byte-for-byte the payload that gets signed — the same property
//! the terminal prompt has, carried across the port unchanged, which is why
//! [`SignatureRequest`] travels whole rather than as four arguments a renderer
//! could get three of.
//!
//! The mechanics: the shell's request loop runs on a worker thread and blocks on
//! a channel; the UI thread draws the dialog and sends the answer back. A window
//! that is closed answers `No`, so a signing request can never outlive the
//! window that was supposed to authorise it.

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use syndeo_shell::prompt::{Decision, Prompter, SignatureRequest};

/// A question waiting for the user, and where to send the answer.
pub enum Ask {
    Sign {
        request: Box<SignatureRequest>,
        answer: SyncSender<Decision>,
    },
    Confirm {
        title: String,
        detail: String,
        answer: SyncSender<Decision>,
    },
    Passphrase {
        label: String,
        answer: SyncSender<Option<String>>,
    },
}

impl Ask {
    /// Answer without asking. Used when the window has gone.
    fn decline(self) {
        match self {
            Ask::Sign { answer, .. } => {
                let _ = answer.send(Decision::No);
            }
            Ask::Confirm { answer, .. } => {
                let _ = answer.send(Decision::No);
            }
            Ask::Passphrase { answer, .. } => {
                let _ = answer.send(None);
            }
        }
    }
}

/// The shell's side: posts a question to the window and waits for the answer.
pub struct WindowPrompter {
    asks: SyncSender<Ask>,
    /// Woken so the answer appears without waiting for the next frame.
    repaint: Mutex<Option<egui::Context>>,
    /// Set when the window has gone. Everything after that is declined rather
    /// than left waiting on a thread that will never draw again.
    closed: Arc<std::sync::atomic::AtomicBool>,
}

/// How long a question waits before it is treated as unanswered.
///
/// The confirmation MAC expires in two minutes regardless, so waiting longer
/// than that would only produce a signature request that cannot be honoured.
const PATIENCE: Duration = Duration::from_secs(115);

impl WindowPrompter {
    pub fn new(asks: SyncSender<Ask>, closed: Arc<std::sync::atomic::AtomicBool>) -> Self {
        WindowPrompter {
            asks,
            repaint: Mutex::new(None),
            closed,
        }
    }

    /// Give the prompter a handle to wake the window with.
    pub fn attach(&self, context: egui::Context) {
        *self.repaint.lock().expect("repaint handle") = Some(context);
    }

    fn is_open(&self) -> bool {
        !self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn wake(&self) {
        if let Some(context) = self.repaint.lock().expect("repaint handle").as_ref() {
            context.request_repaint();
        }
    }

    /// Post a question and wait, bounded.
    fn ask_window<T>(&self, ask: Ask, receiver: Receiver<T>, on_silence: T) -> T {
        if !self.is_open() {
            ask.decline();
            return on_silence;
        }
        if self.asks.send(ask).is_err() {
            return on_silence;
        }
        self.wake();
        match receiver.recv_timeout(PATIENCE) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => {
                tracing::info!("nobody answered; treating it as a refusal");
                on_silence
            }
            Err(RecvTimeoutError::Disconnected) => on_silence,
        }
    }
}

impl Prompter for WindowPrompter {
    fn ask_to_sign(&self, request: &SignatureRequest) -> Decision {
        let (answer, receiver) = std::sync::mpsc::sync_channel(1);
        self.ask_window(
            Ask::Sign {
                request: Box::new(request.clone()),
                answer,
            },
            receiver,
            Decision::No,
        )
    }

    fn ask(&self, title: &str, detail: &str) -> Decision {
        let (answer, receiver) = std::sync::mpsc::sync_channel(1);
        self.ask_window(
            Ask::Confirm {
                title: title.to_string(),
                detail: detail.to_string(),
                answer,
            },
            receiver,
            Decision::No,
        )
    }

    fn read_passphrase(&self, label: &str) -> std::io::Result<String> {
        let (answer, receiver) = std::sync::mpsc::sync_channel(1);
        let given = self.ask_window(
            Ask::Passphrase {
                label: label.to_string(),
                answer,
            },
            receiver,
            None,
        );
        given.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "the passphrase prompt was dismissed",
            )
        })
    }

    fn is_interactive(&self) -> bool {
        self.is_open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use syndeo_ipc::protocol::SignaturePurpose;

    fn request() -> SignatureRequest {
        SignatureRequest {
            origin: "https://wallet.test".into(),
            purpose: SignaturePurpose::ChainTransaction,
            description: "Send 10 SUM to alice".into(),
            payload: b"transfer 10 SUM to alice".to_vec(),
        }
    }

    #[test]
    fn a_closed_window_declines_rather_than_waits() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(true));
        let prompter = WindowPrompter::new(sender, closed);

        let started = std::time::Instant::now();
        assert_eq!(prompter.ask_to_sign(&request()), Decision::No);
        assert!(prompter.read_passphrase("x").is_err());
        assert!(!prompter.is_interactive());
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "it waited on a window that is not there"
        );
    }

    #[test]
    fn an_answer_from_the_window_comes_back() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let prompter = WindowPrompter::new(sender, closed);

        // Stand in for the UI thread.
        std::thread::spawn(move || match receiver.recv().unwrap() {
            Ask::Sign { request, answer } => {
                // What the window draws is what gets signed.
                assert_eq!(request.payload, b"transfer 10 SUM to alice".to_vec());
                assert_eq!(request.rendered_payload()[0], "transfer 10 SUM to alice");
                answer.send(Decision::Yes).unwrap();
            }
            _ => panic!("expected a signing request"),
        });

        assert_eq!(prompter.ask_to_sign(&request()), Decision::Yes);
    }

    #[test]
    fn a_window_that_never_answers_does_not_hold_a_request_forever() {
        // The real patience is just under the confirmation's own expiry; this
        // asserts the shape rather than sitting here for two minutes.
        assert!(
            PATIENCE < Duration::from_secs(120),
            "waiting past the confirmation's expiry would produce a request that cannot be honoured"
        );
    }
}
