//! `stale-while-revalidate` is a promise that the *next* request is fast too.
//! Serving the stale body and then doing nothing keeps half of it.

mod support;

use std::sync::Arc;
use std::time::Duration;
use support::{respond, Origin};
use syndeo_net::{FetchRequest, Net, NetConfig, Source};

fn net(cache_root: &std::path::Path) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        ..NetConfig::default()
    })
    .unwrap()
}

/// Born stale, with a wide refresh window: every request after the first is a
/// serve-stale that should also trigger a revalidation behind itself.
const STALE: &[(&str, &str)] = &[
    ("cache-control", "max-age=0, stale-while-revalidate=600"),
    ("etag", "\"v1\""),
    ("content-type", "text/plain"),
];

#[tokio::test]
async fn a_served_stale_entry_is_refreshed_without_a_second_client_request() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, n| {
        // The revalidation arrives conditionally, and the origin confirms the
        // body it already sent — with a lifetime this time.
        if n > 0 && request.headers().contains_key("if-none-match") {
            return respond(304, &[("cache-control", "max-age=600"), ("etag", "\"v1\"")], b"");
        }
        respond(200, STALE, b"one")
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/asset.txt");

    let first = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(first.source, Source::Origin);
    assert_eq!(&first.body[..], b"one");
    assert_eq!(origin.hits(), 1);

    // Answered from the store while stale, and a refresh goes out behind it.
    let second = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(second.source, Source::CacheStale);
    assert_eq!(&second.body[..], b"one");

    assert_eq!(
        origin.wait_for_hits(2, Duration::from_secs(5)).await,
        2,
        "the entry was served stale and never refreshed"
    );

    // The refresh made the entry fresh, so the next request is a plain hit and
    // the origin is not touched again.
    let third = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(third.source, Source::Cache, "the entry did not become fresh");
    assert_eq!(&third.body[..], b"one");
    assert_eq!(origin.hits(), 2, "the third request went to the origin");
}

#[tokio::test]
async fn concurrent_requests_for_a_stale_entry_collapse_onto_one_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, n| {
        if n > 0 && request.headers().contains_key("if-none-match") {
            // Slow enough that the other requests are all in flight while the
            // first refresh is still running.
            std::thread::sleep(Duration::from_millis(150));
            return respond(304, &[("cache-control", "max-age=600"), ("etag", "\"v1\"")], b"");
        }
        respond(200, STALE, b"one")
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/hot.txt");

    net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(origin.hits(), 1);

    // Twenty readers hit a popular entry the moment it goes stale.
    let mut all = Vec::new();
    for _ in 0..20 {
        all.push(net.fetch(FetchRequest::get(&url)));
    }
    for response in futures::future::join_all(all).await {
        let response = response.unwrap();
        assert!(
            matches!(response.source, Source::CacheStale | Source::Cache),
            "a stale entry should be served from the store, got {:?}",
            response.source
        );
    }

    origin.wait_for_hits(2, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        origin.hits(),
        2,
        "twenty stale hits fanned out into more than one refresh"
    );
}

#[tokio::test]
async fn a_failed_refresh_leaves_the_stored_entry_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_request, n| {
        if n > 0 {
            // The origin has fallen over since it served the body.
            return respond(500, &[("cache-control", "no-store")], b"boom");
        }
        respond(
            200,
            &[
                ("cache-control", "max-age=0, stale-while-revalidate=600, stale-if-error=600"),
                ("etag", "\"v1\""),
            ],
            b"one",
        )
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/fragile.txt");

    net.fetch(FetchRequest::get(&url)).await.unwrap();
    let second = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(second.source, Source::CacheStale);

    origin.wait_for_hits(2, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The 500 did not replace anything, and staleness is still servable.
    let third = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(third.source, Source::CacheStale);
    assert_eq!(&third.body[..], b"one", "a failed refresh overwrote the body");
}
