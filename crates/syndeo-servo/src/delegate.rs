//! The delegate Servo asks before it loads anything.
//!
//! One rule, and it is the whole crate: every load is intercepted. A load handed
//! back to Servo is a load Servo performs itself, with its own net crate, over
//! its own socket — which is precisely the thing this exists to prevent. So a
//! request the network process cannot answer is answered with a failure rather
//! than declined.
//!
//! # Why it is shaped like this
//!
//! `WebResourceLoad` is not `Send`, and rightly so: it holds a responder that
//! belongs to the embedder's thread. It therefore cannot be carried off to a
//! worker, which rules out the obvious "spawn a thread and block" arrangement.
//! Nor can the embedder thread wait for the answer itself — Servo is waiting on
//! it to return, and a browser whose UI thread blocks on the network is the
//! thing everyone stopped doing in 2005.
//!
//! So the load stays here, on the embedder's thread, in a pending map. Only the
//! request travels: a plain `NetRequest` goes to a tokio task, the answer comes
//! back through a channel, and the event loop is woken so the embedder thread
//! picks it up and applies it. Servo's own event-loop waker is exactly the
//! mechanism for that, and it is already wired for Servo's own use.

use crate::bridge;
use servo::{WebResourceLoad, WebResourceResponse, WebView};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use syndeo_ipc::protocol::{Fetched, NetRequest};
use syndeo_ipc::transport::{Channel, Endpoint};

/// One load's outcome, on its way back to the embedder's thread.
type Answer = Result<Box<Fetched>, String>;

/// Answers Servo's resource loads out of the network process.
pub struct NetworkDelegate {
    net: Endpoint,
    runtime: tokio::runtime::Handle,
    /// Servo's own waker, so a finished fetch gets the event loop turning again
    /// rather than waiting for the next mouse move.
    waker: Box<dyn servo::EventLoopWaker>,

    next: Cell<u64>,
    /// Loads waiting on the network process. Not `Send`, and never leaves this
    /// thread.
    pending: RefCell<HashMap<u64, (WebResourceLoad, std::time::Instant)>>,
    answers: Receiver<(u64, Answer)>,
    answered: Sender<(u64, Answer)>,
    /// Counted so the claim this crate makes can be checked rather than asserted.
    intercepted: Cell<u64>,
}

impl NetworkDelegate {
    pub fn new(
        net: Endpoint,
        runtime: tokio::runtime::Handle,
        waker: Box<dyn servo::EventLoopWaker>,
    ) -> Rc<Self> {
        let (answered, answers) = std::sync::mpsc::channel();
        Rc::new(NetworkDelegate {
            net,
            runtime,
            waker,
            next: Cell::new(0),
            pending: RefCell::new(HashMap::new()),
            answers,
            answered,
            intercepted: Cell::new(0),
        })
    }

    /// How many loads this delegate has taken off Servo. Every load Servo makes
    /// should be in this number; anything missing from it went out over a socket
    /// we do not control.
    pub fn intercepted(&self) -> u64 {
        self.intercepted.get()
    }

    /// Take one of Servo's resource loads.
    ///
    /// Returns immediately. The load is parked until the network process
    /// answers.
    pub fn load_web_resource(&self, _webview: WebView, load: WebResourceLoad) {
        // Schemes that carry their own bytes are left to Servo, and that is not
        // a hole in the boundary: `data:` is base64 in the markup, `about:` and
        // `blob:` are memory the renderer already holds, and none of the three
        // can reach a socket however Servo chooses to handle them. Sending them
        // to the network process instead is what produced `invalid url
        // data:image/svg+xml,...` for every inline icon on a page, and an icon
        // that silently did not draw.
        //
        // An allowlist rather than a denylist, deliberately: a scheme nobody
        // here has thought about goes to the network process, which is the side
        // to be wrong on. `ws:` and `wss:` are exactly that case.
        if bridge::is_self_contained_scheme(&load.request().url) {
            tracing::trace!(url = %load.request().url, "left to the renderer; no network in it");
            return;
        }

        let request = bridge::to_net_request(
            &load.request().method,
            &load.request().url,
            &load.request().headers,
        );
        let url = load.request().url.clone();

        let id = self.next.get();
        self.next.set(id + 1);
        self.intercepted.set(self.intercepted.get() + 1);
        let in_flight = {
            let mut pending = self.pending.borrow_mut();
            pending.insert(id, (load, std::time::Instant::now()));
            pending.len()
        };
        tracing::debug!(%url, in_flight, "requested a renderer load");

        let net = self.net.clone();
        let answered = self.answered.clone();
        let waker = self.waker.clone_box();
        self.runtime.spawn(async move {
            let answer = match fetch(&net, request).await {
                Ok(fetched) => Ok(Box::new(fetched)),
                Err(error) => Err(format!("{error:#}")),
            };
            tracing::trace!(%url, "the network process answered a renderer load");
            let _ = answered.send((id, answer));
            // Without this the answer sits in the channel until something else
            // happens to turn the loop.
            waker.wake();
        });
    }

    /// Apply every answer that has arrived. Called from the event loop.
    ///
    /// Returns how many were applied, because the caller needs to know whether
    /// handing these to Servo might have produced more work — a stylesheet
    /// arriving is how the fonts it references are discovered.
    pub fn deliver(&self) -> usize {
        // Collected first so the borrow is not held across `intercept`.
        let ready: Vec<(u64, Answer)> = self.answers.try_iter().collect();
        let applied = ready.len();
        for (id, answer) in ready {
            let Some((load, requested)) = self.pending.borrow_mut().remove(&id) else {
                continue;
            };
            let waited = requested.elapsed();
            let url = load.request().url.clone();

            match answer {
                Ok(fetched) => {
                    let mut response = WebResourceResponse::new(url.clone());
                    response.headers = bridge::response_headers(&fetched);
                    response.status_code = http::StatusCode::from_u16(fetched.status)
                        .unwrap_or(http::StatusCode::BAD_GATEWAY);
                    tracing::debug!(
                        %url,
                        provenance = bridge::provenance(&fetched),
                        bytes = fetched.body.len(),
                        waited_ms = waited.as_millis(),
                        "answered a renderer load"
                    );

                    let mut intercepted = load.intercept(response);
                    if !fetched.body.is_empty() {
                        intercepted.send_body_data(fetched.body);
                    }
                    intercepted.finish();
                }
                Err(error) => {
                    // Cancelled, not declined. Declining hands the load back to
                    // Servo's own net crate, which is the one outcome this
                    // delegate exists to make impossible.
                    tracing::warn!(%url, %error, "the net process could not answer; refusing the load");
                    load.intercept(WebResourceResponse::new(url)).cancel();
                }
            }
        }
        applied
    }
}

async fn fetch(net: &Endpoint, request: NetRequest) -> anyhow::Result<Fetched> {
    let mut channel = Channel::connect(net).await?;
    Ok(channel.fetch(&request).await?)
}
