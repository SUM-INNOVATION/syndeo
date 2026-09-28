//! A whole-body fetch over IPC has a ceiling, and a body refused for crossing
//! it stops all the way back to the origin.
//!
//! The origin here sends a chunked body with no length, 64 MiB of it, far past
//! the ceiling the test sets. Through the real network service, a caller that collects whole
//! bodies refuses it at the ceiling and closes its connection; the service's
//! next write fails, it drops the response, and the origin's body is dropped
//! with the connection it was being sent on.

mod support;

use futures::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use support::Origin;
use syndeo_ipc::frame::FrameError;
use syndeo_ipc::protocol::NetRequest;
use syndeo_ipc::Framed;
use syndeo_net::{Net, NetConfig};

const PIECE: usize = 16 * 1024;

/// Set when the origin's body is dropped, which happens when its connection
/// goes.
struct Released(Arc<AtomicBool>);

impl Drop for Released {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// An origin sending `pieces` pieces of `PIECE` bytes, or without end for
/// `None`, chunked, uncacheable. Counts what it sent, and says when the body
/// it was sending was let go.
async fn origin(
    pieces: Option<usize>,
    sent: Arc<AtomicUsize>,
    released: Arc<AtomicBool>,
) -> Origin {
    use futures::stream;
    use hyper::body::{Bytes, Frame};

    Origin::start_streaming(Arc::new(move |_request, _n| {
        let sent = sent.clone();
        let guard = Released(released.clone());
        let body = stream::unfold((0usize, guard), move |(i, guard)| {
            let sent = sent.clone();
            async move {
                if pieces.is_some_and(|n| i >= n) {
                    return None;
                }
                sent.fetch_add(1, Ordering::SeqCst);
                let chunk: Result<Frame<Bytes>, std::io::Error> =
                    Ok(Frame::data(Bytes::from(vec![b'z'; PIECE])));
                Some((chunk, (i + 1, guard)))
            }
        });
        hyper::Response::builder()
            .status(200)
            .header("cache-control", "no-store")
            .header("content-type", "application/octet-stream")
            .body(http_body_util::StreamBody::new(body.boxed()))
            .unwrap()
    }))
    .await
}

fn fetch(url: String) -> NetRequest {
    NetRequest::Fetch {
        method: "GET".into(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
        integrity: None,
        partition: None,
    }
}

/// Serve one request with the real network service on one end of an
/// in-memory stream, and hand back the other end.
fn service(
    net: Arc<Net>,
) -> (
    Framed<tokio::io::DuplexStream>,
    tokio::task::JoinHandle<bool>,
) {
    let (ours, theirs) = tokio::io::duplex(256 * 1024);
    let serving = tokio::spawn(async move {
        let mut server = Framed::new(theirs);
        let request: NetRequest = server.recv().await.unwrap();
        syndeo_net::service::respond(&net, request, &mut server)
            .await
            .is_ok()
    });
    (Framed::new(ours), serving)
}

fn net(dir: &std::path::Path) -> Arc<Net> {
    Arc::new(
        Net::new(NetConfig {
            cache_root: dir.to_path_buf(),
            ..NetConfig::default()
        })
        .unwrap(),
    )
}

#[tokio::test]
async fn a_body_past_the_ceiling_is_refused_and_the_origin_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let sent = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicBool::new(false));
    // 64 MiB, so a fetch with no ceiling would collect it all and succeed.
    const PIECES: usize = 4096;
    let origin = origin(Some(PIECES), sent.clone(), released.clone()).await;
    let request = fetch(origin.url("/long"));
    let (client, serving) = service(net(dir.path()));

    let limit = 1024 * 1024;
    let refused = client.fetch_within(&request, limit).await;
    assert!(
        matches!(refused, Err(FrameError::BodyTooLarge(l)) if l == limit),
        "{refused:?}"
    );

    // The service finds the caller gone at its next write, and stops.
    let finished = tokio::time::timeout(Duration::from_secs(30), serving)
        .await
        .expect("the service went on serving a caller that had gone")
        .unwrap();
    assert!(!finished, "the service thought it had delivered the body");

    // And the origin's body is dropped with its connection. Waited for, with
    // a bound only so a regression fails rather than hangs.
    let mut waited = 0;
    while !released.load(Ordering::SeqCst) {
        assert!(
            waited < 3000,
            "the origin is still sending, {} pieces in",
            sent.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += 1;
    }
    // Let go long before the end: what was sent is what fits in the
    // ceiling and the buffers between, not the body.
    let total = sent.load(Ordering::SeqCst);
    assert!(
        total < PIECES / 4,
        "the origin sent {total} of {PIECES} pieces"
    );
}

#[tokio::test]
async fn a_chunked_body_of_exactly_the_ceiling_arrives_whole() {
    let dir = tempfile::tempdir().unwrap();
    let sent = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicBool::new(false));
    let origin = origin(Some(64), sent, released).await;
    let request = fetch(origin.url("/exact"));
    let (client, serving) = service(net(dir.path()));

    let limit = (64 * PIECE) as u64;
    let fetched = client.fetch_within(&request, limit).await.unwrap();
    assert_eq!(fetched.body.len() as u64, limit);
    assert!(fetched.body.iter().all(|&b| b == b'z'));
    assert!(serving.await.unwrap());

    // One byte less of ceiling, and the same body is refused.
    let request = fetch(origin.url("/exact"));
    let other = tempfile::tempdir().unwrap();
    let (client, serving) = service(net(other.path()));
    let refused = client.fetch_within(&request, limit - 1).await;
    assert!(
        matches!(refused, Err(FrameError::BodyTooLarge(_))),
        "{refused:?}"
    );
    let _ = serving.await;
}
