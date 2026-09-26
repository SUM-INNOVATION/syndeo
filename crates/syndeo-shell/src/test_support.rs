//! Stand-ins the shell's tests share: a prompter that counts how often it is
//! asked, and a keystore that is a socket the test owns.

use crate::prompt::{Decision, Prompter, SignatureRequest};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use syndeo_ipc::protocol::{KeystoreRequest, KeystoreResponse};
use syndeo_ipc::transport::{Endpoint, Server};

/// Counts how often it is asked, and gives the same answer every time.
pub struct CountingPrompter {
    answer: Decision,
    asked: AtomicUsize,
}

impl CountingPrompter {
    pub fn new(answer: Decision) -> Arc<Self> {
        Arc::new(CountingPrompter {
            answer,
            asked: AtomicUsize::new(0),
        })
    }

    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl Prompter for CountingPrompter {
    fn ask_to_sign(&self, _request: &SignatureRequest) -> Decision {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer
    }

    fn ask(&self, _title: &str, _detail: &str) -> Decision {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer
    }

    fn read_passphrase(&self, _label: &str) -> std::io::Result<String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::other("no passphrase in this test"))
    }
}

/// How the fake keystore answers a request.
pub type Reply = Arc<dyn Fn(&KeystoreRequest) -> KeystoreResponse + Send + Sync>;

/// A keystore endpoint that is a listener this test owns. It counts
/// connections, keeps every request it was sent, and answers each with
/// `reply` — for as many requests as a connection sends, since unsealing asks
/// for the status and then unseals on one connection.
pub struct FakeKeystore {
    pub endpoint: Endpoint,
    connections: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<KeystoreRequest>>>,
    _serving: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

impl FakeKeystore {
    pub fn start(reply: Reply) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::new(dir.path().join("keystore.sock"));
        let server = Server::bind(endpoint.clone()).unwrap();

        let connections = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(Mutex::new(Vec::new()));
        let serving = tokio::spawn({
            let connections = connections.clone();
            let received = received.clone();
            async move {
                while let Ok(mut framed) = server.accept().await {
                    connections.fetch_add(1, Ordering::SeqCst);
                    while let Ok(request) = framed.recv::<KeystoreRequest>().await {
                        let answer = reply(&request);
                        received.lock().unwrap().push(request);
                        if framed.send(&answer).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });

        FakeKeystore {
            endpoint,
            connections,
            received,
            _serving: serving,
            _dir: dir,
        }
    }

    /// Answers everything with an error, so a call returns cleanly without
    /// anything being signed.
    pub fn refusing() -> Self {
        Self::start(Arc::new(|_| {
            KeystoreResponse::Error("the fake keystore".into())
        }))
    }

    /// Exact at the moment a call returns: a call that connected also waited
    /// for the reply, which the listener sends after counting.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    pub fn received(&self) -> MutexGuard<'_, Vec<KeystoreRequest>> {
        self.received.lock().unwrap()
    }
}
