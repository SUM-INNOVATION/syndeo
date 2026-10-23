//! With `cache_public_addresses_only`, nothing that came from an address off
//! the public internet is stored, on any path that stores: a streamed body,
//! one checked against a declared integrity, a revalidation and a background
//! refresh. It is the address the answer came from that counts, so a name
//! resolving to this Mac is no different from its address. And clearing the
//! cache keeps out a body that was still arriving when it was cleared.

mod support;

use std::sync::Arc;
use std::time::Duration;
use support::{respond, Origin};
use syndeo_net::{FetchRequest, Net, NetConfig, Source};

fn net(cache_root: &std::path::Path, public_only: bool) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        cache_public_addresses_only: public_only,
        ..NetConfig::default()
    })
    .unwrap()
}

async fn fetch(net: &Net, request: FetchRequest) -> (Source, Vec<u8>) {
    let response = net.fetch(request).await.unwrap();
    let source = response.source;
    let body = response.body.collect().await.unwrap();
    (source, body.to_vec())
}

/// The entries the cache holds once every write already begun has landed: a
/// streamed body's entry is written just after its end goes to the caller.
async fn entries(net: &Net) -> u64 {
    net.landed().await;
    net.cache().stats().unwrap().entries
}

/// Every byte the cache holds on disk, temporary files included.
fn on_disk(root: &std::path::Path) -> u64 {
    fn walk(dir: &std::path::Path) -> u64 {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| {
                        let path = entry.path();
                        if path.is_dir() {
                            walk(&path)
                        } else {
                            entry.metadata().map(|m| m.len()).unwrap_or(0)
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    walk(&root.join("blobs"))
}

const CACHEABLE: &[(&str, &str)] = &[
    ("cache-control", "public, max-age=600"),
    ("content-type", "text/plain"),
];

#[tokio::test]
async fn an_answer_from_loopback_is_not_stored_by_address_or_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| respond(200, CACHEABLE, b"local"))).await;
    let net = net(dir.path(), true);
    let port = origin.address.port();

    let mut hits = 0;
    for url in [
        format!("http://127.0.0.1:{port}/by-address"),
        format!("http://localhost:{port}/by-name"),
    ] {
        for _ in 0..2 {
            let (source, body) = fetch(&net, FetchRequest::get(&url)).await;
            hits += 1;
            assert_eq!(
                (source, &body[..]),
                (Source::Origin, &b"local"[..]),
                "{url}"
            );
            assert_eq!(origin.hits(), hits, "{url} was answered from the cache");
        }
    }
    assert_eq!(entries(&net).await, 0, "an entry was written");
    assert_eq!(
        on_disk(dir.path()),
        0,
        "a body was written, if only for a moment"
    );
}

#[tokio::test]
async fn an_answer_from_ipv6_loopback_is_not_stored() {
    let Ok(listener) = std::net::TcpListener::bind("[::1]:0") else {
        eprintln!("no IPv6 loopback here; nothing to test");
        return;
    };
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start_on(
        format!("[::1]:{port}").parse().unwrap(),
        Arc::new(|_, _| respond(200, CACHEABLE, b"local")),
    )
    .await;
    let net = net(dir.path(), true);
    let url = format!("http://[::1]:{port}/v6");
    for n in 1..=2 {
        let (source, _) = fetch(&net, FetchRequest::get(&url)).await;
        assert_eq!((source, origin.hits()), (Source::Origin, n));
    }
    assert_eq!(entries(&net).await, 0);
}

/// The same origin, with the rule off: the second fetch is a hit. Without
/// this, the tests above would pass for a cache that stores nothing at all.
#[tokio::test]
async fn without_the_rule_the_same_answer_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| respond(200, CACHEABLE, b"local"))).await;
    let net = net(dir.path(), false);
    let url = origin.url("/kept");
    assert_eq!(fetch(&net, FetchRequest::get(&url)).await.0, Source::Origin);
    assert_eq!(fetch(&net, FetchRequest::get(&url)).await.0, Source::Cache);
    assert_eq!((origin.hits(), entries(&net).await), (1, 1));
}

#[tokio::test]
async fn a_body_checked_against_its_declared_integrity_is_not_stored_either() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| respond(200, CACHEABLE, b"checked"))).await;
    let net = net(dir.path(), true);
    let integrity = syndeo_cache::Integrity {
        hashes: vec![syndeo_cache::sri::Hash::compute(
            syndeo_cache::sri::Algorithm::Sha256,
            b"checked",
        )],
    };
    for n in 1..=2 {
        let request = FetchRequest {
            integrity: Some(integrity.clone()),
            ..FetchRequest::get(origin.url("/script.js"))
        };
        let (source, body) = fetch(&net, request).await;
        assert_eq!(&body[..], b"checked");
        assert_eq!((source, origin.hits()), (Source::PassThrough, n));
    }
    assert_eq!(entries(&net).await, 0);
}

/// An entry stored when the rule was off (as a public answer would be), then
/// revalidated with the rule on: the 304 is believed for this response, and
/// not written, so the next request asks the origin again.
#[tokio::test]
async fn a_revalidation_from_loopback_is_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, _| {
        if request.headers().contains_key("if-none-match") {
            return respond(
                304,
                &[("cache-control", "max-age=600"), ("etag", "\"v1\"")],
                b"",
            );
        }
        respond(
            200,
            &[("cache-control", "no-cache"), ("etag", "\"v1\"")],
            b"body",
        )
    }))
    .await;
    let url = origin.url("/revalidated");
    {
        let net = net(dir.path(), false);
        assert_eq!(fetch(&net, FetchRequest::get(&url)).await.0, Source::Origin);
        assert_eq!(entries(&net).await, 1);
    }
    let net = net(dir.path(), true);
    let (source, body) = fetch(&net, FetchRequest::get(&url)).await;
    assert_eq!((source, &body[..]), (Source::Revalidated, &b"body"[..]));
    assert_eq!(origin.hits(), 2);
    let (source, _) = fetch(&net, FetchRequest::get(&url)).await;
    assert_eq!(
        (source, origin.hits()),
        (Source::Revalidated, 3),
        "the 304's max-age=600 was written, so the origin was not asked"
    );
}

#[tokio::test]
async fn a_background_refresh_from_loopback_is_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, _| {
        if request.headers().contains_key("if-none-match") {
            return respond(
                304,
                &[("cache-control", "max-age=600"), ("etag", "\"v1\"")],
                b"",
            );
        }
        respond(
            200,
            &[
                ("cache-control", "max-age=0, stale-while-revalidate=600"),
                ("etag", "\"v1\""),
            ],
            b"stale",
        )
    }))
    .await;
    let url = origin.url("/refreshed");
    {
        let net = net(dir.path(), false);
        fetch(&net, FetchRequest::get(&url)).await;
        // Its entry, written just after the body ends, before the cache is
        // opened again.
        net.landed().await;
    }
    let net = net(dir.path(), true);
    for round in 1..=2 {
        let (source, _) = fetch(&net, FetchRequest::get(&url)).await;
        assert_eq!(source, Source::CacheStale, "round {round}");
        assert_eq!(
            origin
                .wait_for_hits(1 + round, Duration::from_secs(5))
                .await,
            1 + round,
            "round {round}: no refresh behind it"
        );
        // Long enough for a refresh that was going to be written to be.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A body still arriving when the cache is cleared is passed on whole and
/// not stored; what is asked for afterwards is stored as before.
#[tokio::test]
async fn clearing_keeps_out_a_body_that_was_still_arriving() {
    let dir = tempfile::tempdir().unwrap();
    let (release, released) = tokio::sync::watch::channel(false);
    let origin = Origin::start_streaming(Arc::new(move |_, _| {
        let mut released = released.clone();
        let rest = async move {
            let _ = released.wait_for(|go| *go).await;
            Ok(hyper::body::Frame::data(hyper::body::Bytes::from_static(
                b" second",
            )))
        };
        let frames = futures::stream::iter([Ok(hyper::body::Frame::data(
            hyper::body::Bytes::from_static(b"first"),
        ))])
        .chain(futures::stream::once(rest));
        hyper::Response::builder()
            .status(200)
            .header("cache-control", "public, max-age=600")
            .body(http_body_util::StreamBody::new(futures::StreamExt::boxed(
                frames,
            )))
            .unwrap()
    }))
    .await;
    let net = net(dir.path(), false);
    let earlier = Origin::start(Arc::new(|_, _| respond(200, CACHEABLE, b"earlier"))).await;
    fetch(&net, FetchRequest::get(earlier.url("/earlier"))).await;
    assert_eq!(entries(&net).await, 1);

    let response = net
        .fetch(FetchRequest::get(origin.url("/arriving")))
        .await
        .unwrap();
    let mut stream = response.body.into_stream();
    use futures::StreamExt;
    assert_eq!(&stream.next().await.unwrap().unwrap()[..], b"first");
    assert_eq!(net.clear_cache().unwrap(), 1);
    release.send(true).unwrap();
    let mut rest = Vec::new();
    while let Some(chunk) = stream.next().await {
        rest.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(&rest[..], b" second", "the body is passed on whole");
    assert_eq!(
        entries(&net).await,
        0,
        "a body asked for before the clear was stored"
    );
    assert_eq!(on_disk(dir.path()), 0);

    fetch(&net, FetchRequest::get(earlier.url("/after"))).await;
    assert_eq!(
        entries(&net).await,
        1,
        "the cache stores again after a clear"
    );
}
