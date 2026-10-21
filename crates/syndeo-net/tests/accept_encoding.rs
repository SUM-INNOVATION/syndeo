//! Whose `Accept-Encoding` reaches the origin, and what comes back.
//!
//! Syndeo's own fetches never invite an encoding: what is cached is what the
//! origin sent, hashed as it is. A caller that decodes what it asked for, as
//! the proxy's browser does, has its own `Accept-Encoding` passed on, and the
//! encoded answer comes back, and is stored, as it came.

mod support;

use std::sync::{Arc, Mutex};
use support::{respond, Origin};
use syndeo_net::{FetchRequest, Net, NetConfig, Source};

fn net(cache_root: &std::path::Path, forward_accept_encoding: bool) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        forward_accept_encoding,
        ..NetConfig::default()
    })
    .unwrap()
}

/// An origin that answers in brotli to whoever asks for it, and keeps each
/// request's `Accept-Encoding`. It leaves `Vary` out, as plenty of origins
/// do, so only the cache's own care keeps the encoded answer from a caller
/// that cannot decode it.
async fn encoding_origin() -> (Origin, Arc<Mutex<Vec<Option<String>>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let origin = Origin::start(Arc::new(move |request, _| {
        let accept = request
            .headers()
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap().to_string());
        let brotli = accept.as_deref().is_some_and(|a| a.contains("br"));
        log.lock().unwrap().push(accept);
        if brotli {
            respond(
                200,
                &[("cache-control", "max-age=600"), ("content-encoding", "br")],
                b"pretend brotli",
            )
        } else {
            respond(200, &[("cache-control", "max-age=600")], b"plain text")
        }
    }))
    .await;
    (origin, seen)
}

async fn fetch(net: &Net, url: &str, accept: &str) -> (Source, Option<String>, Vec<u8>) {
    let response = net
        .fetch(FetchRequest::get(url).header("accept-encoding", accept))
        .await
        .unwrap();
    let coding = response
        .headers
        .get("content-encoding")
        .map(|v| v.to_str().unwrap().to_string());
    let source = response.source;
    let body = response.body.collect().await.unwrap().to_vec();
    (source, coding, body)
}

#[tokio::test]
async fn syndeo_s_own_fetches_never_invite_an_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let (origin, seen) = encoding_origin().await;
    let net = net(dir.path(), false);

    let (source, coding, body) = fetch(&net, &origin.url("/app.js"), "gzip, deflate, br").await;
    assert_eq!(source, Source::Origin);
    assert_eq!(coding, None);
    assert_eq!(body, b"plain text");
    assert_eq!(*seen.lock().unwrap(), [None]);
}

#[tokio::test]
async fn a_caller_that_decodes_gets_the_encoding_it_asked_for_and_the_cache_keeps_it_as_it_came() {
    let dir = tempfile::tempdir().unwrap();
    let (origin, seen) = encoding_origin().await;
    let net = net(dir.path(), true);
    let url = origin.url("/app.js");

    let first = fetch(&net, &url, "gzip, deflate, br").await;
    assert_eq!(
        first,
        (
            Source::Origin,
            Some("br".into()),
            b"pretend brotli".to_vec()
        )
    );
    let again = fetch(&net, &url, "gzip, deflate, br").await;
    assert_eq!(
        again,
        (Source::Cache, Some("br".into()), b"pretend brotli".to_vec()),
        "the encoded answer was not stored as it came"
    );

    // A caller that cannot decode brotli is not handed the stored copy,
    // though the origin never said the answer varies with the encoding.
    let plain = fetch(&net, &url, "identity").await;
    assert_eq!(plain, (Source::Origin, None, b"plain text".to_vec()));

    assert_eq!(
        *seen.lock().unwrap(),
        [
            Some("gzip, deflate, br".to_string()),
            Some("identity".to_string())
        ]
    );
}
