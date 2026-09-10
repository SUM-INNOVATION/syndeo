//! A tiny origin to fetch from, so the cache-aware paths can be driven end to
//! end without the internet.

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Answers one request, and is told how many came before it.
pub type Handler =
    Arc<dyn Fn(&Request<hyper::body::Incoming>, usize) -> Response<Full<Bytes>> + Send + Sync>;

pub struct Origin {
    pub address: SocketAddr,
    hits: Arc<AtomicUsize>,
}

impl Origin {
    /// Bind on a free loopback port and answer with `handler`.
    pub async fn start(handler: Handler) -> Origin {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));

        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                        let handler = handler.clone();
                        let counter = counter.clone();
                        async move {
                            let n = counter.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, std::convert::Infallible>(handler(&request, n))
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });

        Origin { address, hits }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Wait for the origin to have seen `n` requests, or give up.
    pub async fn wait_for_hits(&self, n: usize, within: Duration) -> usize {
        let deadline = tokio::time::Instant::now() + within;
        while tokio::time::Instant::now() < deadline {
            if self.hits() >= n {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.hits()
    }
}

pub fn respond(status: u16, headers: &[(&str, &str)], body: &'static [u8]) -> Response<Full<Bytes>> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Full::new(Bytes::from_static(body))).unwrap()
}
