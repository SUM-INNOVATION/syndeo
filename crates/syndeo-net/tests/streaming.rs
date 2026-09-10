//! A body that is still arriving is still a body.
//!
//! What these hold to is the thing the buffering ceiling used to make
//! impossible: the caller sees the first bytes long before the last ones exist,
//! a body larger than any frame or buffer still arrives, and the cache fills in
//! behind the transfer rather than in front of it.

mod support;

use futures::StreamExt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::Origin;
use syndeo_net::{FetchRequest, Net, NetConfig, Source};

fn net(cache_root: &std::path::Path, max_body_bytes: u64) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        max_body_bytes,
        ..NetConfig::default()
    })
    .unwrap()
}

/// An origin that sends a body in pieces, slowly, so "did it wait for the end"
/// is answerable by a clock.
async fn dripping_origin(pieces: usize, gap: Duration) -> Origin {
    use futures::stream;
    use http_body_util::StreamBody;
    use hyper::body::{Bytes, Frame};

    Origin::start_streaming(Arc::new(move |_request, _n| {
        let pieces = stream::unfold(0usize, move |i| async move {
            if i >= pieces {
                return None;
            }
            if i > 0 {
                tokio::time::sleep(gap).await;
            }
            let chunk: Result<Frame<Bytes>, std::io::Error> =
                Ok(Frame::data(Bytes::from(vec![b'a' + (i % 26) as u8; 1000])));
            Some((chunk, i + 1))
        });
        let body = StreamBody::new(pieces.boxed());
        hyper::Response::builder()
            .status(200)
            .header("cache-control", "max-age=600")
            .header("content-type", "application/octet-stream")
            .body(body)
            .unwrap()
    }))
    .await
}

#[tokio::test]
async fn the_first_bytes_arrive_before_the_last_ones_exist() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dripping_origin(10, Duration::from_millis(80)).await;
    let net = net(dir.path(), 64 * 1024 * 1024);

    let started = Instant::now();
    let response = net
        .fetch(FetchRequest::get(origin.url("/slow.bin")))
        .await
        .unwrap();
    assert!(
        response.body.is_stream(),
        "an ordinary fetch should not have been buffered"
    );

    let mut stream = response.body.into_stream();
    let first = stream.next().await.expect("a first chunk").unwrap();
    let first_byte_at = started.elapsed();
    assert_eq!(first.len(), 1000);

    let mut total = first.len();
    while let Some(chunk) = stream.next().await {
        total += chunk.unwrap().len();
    }
    let last_byte_at = started.elapsed();

    assert_eq!(total, 10_000);
    assert!(
        first_byte_at < last_byte_at / 2,
        "the first byte took {first_byte_at:?} of the {last_byte_at:?} the whole body took, \
         which is what buffering looks like"
    );
}

#[tokio::test]
async fn a_streamed_body_is_in_the_cache_once_it_has_all_arrived() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dripping_origin(4, Duration::from_millis(10)).await;
    let net = net(dir.path(), 64 * 1024 * 1024);
    let url = origin.url("/asset.bin");

    let first = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(first.source, Source::Origin);
    assert!(
        first.content.is_none(),
        "a body that has not finished arriving has no address yet"
    );
    let body = first.body.collect().await.unwrap();
    assert_eq!(body.len(), 4000);

    // Reading the stream to the end is what commits the entry.
    let second = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(second.source, Source::Cache, "the streamed body was not stored");
    assert_eq!(second.body.collect().await.unwrap(), body);
    assert_eq!(origin.hits(), 1);
}

#[tokio::test]
async fn a_transfer_that_stops_early_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dripping_origin(20, Duration::from_millis(30)).await;
    let net = net(dir.path(), 64 * 1024 * 1024);
    let url = origin.url("/abandoned.bin");

    {
        let response = net.fetch(FetchRequest::get(&url)).await.unwrap();
        let mut stream = response.body.into_stream();
        // Take two chunks and walk away, the way a cancelled navigation does.
        let _ = stream.next().await;
        let _ = stream.next().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let stats = net.cache().stats().unwrap();
    assert_eq!(
        stats.entries, 0,
        "a body that never finished arriving was stored as if it had"
    );
    assert_eq!(stats.blobs, 0, "a partial body was left in the blob store");
}

#[tokio::test]
async fn a_body_past_the_cache_budget_still_arrives_in_full() {
    let dir = tempfile::tempdir().unwrap();
    let origin = dripping_origin(20, Duration::from_millis(1)).await;
    // A budget far below the body: what used to fail the fetch outright.
    let net = net(dir.path(), 4_096);
    let url = origin.url("/large.bin");

    let response = net.fetch(FetchRequest::get(&url)).await.unwrap();
    let body = response.body.collect().await.unwrap();
    assert_eq!(
        body.len(),
        20_000,
        "the ceiling refused a body instead of declining to cache it"
    );

    tokio::time::sleep(Duration::from_millis(50)).await;
    let stats = net.cache().stats().unwrap();
    assert_eq!(stats.entries, 0, "a body over the budget was cached anyway");

    // And the next request goes to the origin, because nothing was kept.
    let again = net.fetch(FetchRequest::get(&url)).await.unwrap();
    assert_eq!(again.source, Source::Origin);
    let _ = again.body.collect().await.unwrap();
    assert_eq!(origin.hits(), 2);
}

#[tokio::test]
async fn a_resource_with_declared_integrity_is_checked_before_any_of_it_is_believed() {
    use syndeo_cache::sri::{Algorithm, Hash};

    let dir = tempfile::tempdir().unwrap();
    let origin = dripping_origin(3, Duration::from_millis(5)).await;
    let net = net(dir.path(), 64 * 1024 * 1024);

    let expected: Vec<u8> = (0..3)
        .flat_map(|i| vec![b'a' + (i % 26) as u8; 1000])
        .collect();
    let token = Hash::compute(Algorithm::Sha384, &expected).to_token();

    let mut request = FetchRequest::get(origin.url("/lib.js"));
    request.integrity = Some(syndeo_cache::Integrity::parse(&token).unwrap());

    let response = net.fetch(request).await.unwrap();
    assert!(
        !response.body.is_stream(),
        "a hash cannot be checked against bytes that have already been handed out"
    );
    assert_eq!(response.body.collect().await.unwrap(), expected);
}
