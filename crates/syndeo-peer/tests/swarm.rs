//! Two nodes, one body, and a peer that lies.

use std::sync::Arc;
use std::time::Duration;
use syndeo_cache::sri::{Algorithm, Hash};
use syndeo_cache::{Cache, ContentId};
use syndeo_peer::{BlobRequest, PeerConfig, PeerHandle, PeerNode};

fn cache(dir: &std::path::Path) -> Arc<Cache> {
    Arc::new(Cache::open(dir).unwrap())
}

fn config() -> PeerConfig {
    PeerConfig {
        listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
        request_timeout: Duration::from_secs(5),
        ..PeerConfig::default()
    }
}

/// Connect `client` to `server` and wait until both agree it happened.
async fn connect(server: &PeerHandle, client: &PeerHandle) {
    let address = wait_for(|| async {
        let listeners = server.listeners().await.unwrap();
        listeners.into_iter().next()
    })
    .await
    .expect("the server never announced a listen address");

    client.dial(address).await.unwrap();

    wait_for(|| async {
        let peers = client.peers().await.unwrap();
        peers.contains(&server.peer_id()).then_some(())
    })
    .await
    .expect("the two peers never connected");
}

async fn wait_for<T, F, Fut>(mut probe: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..200 {
        if let Some(value) = probe().await {
            return Some(value);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    None
}

#[tokio::test]
async fn a_peer_serves_a_body_by_its_content_address() {
    let server_dir = tempfile::tempdir().unwrap();
    let client_dir = tempfile::tempdir().unwrap();

    let body = vec![b'p'; 20_000];
    let server_cache = cache(server_dir.path());
    let headers = {
        let mut h = http::HeaderMap::new();
        h.insert("cache-control", "max-age=600".parse().unwrap());
        h
    };
    let now = syndeo_cache::headers::now_secs();
    server_cache
        .store(
            "GET",
            "https://origin.test/asset.js",
            &http::HeaderMap::new(),
            200,
            &headers,
            &body,
            now,
            now,
        )
        .unwrap();

    let server = PeerNode::start(server_cache, config()).unwrap();
    let client = PeerNode::start(cache(client_dir.path()), config()).unwrap();
    connect(&server, &client).await;

    let fetched = client.fetch_content(ContentId::of(&body)).await.unwrap();
    assert_eq!(fetched, body);
}

#[tokio::test]
async fn a_peer_serves_a_body_by_the_integrity_a_page_declared() {
    let server_dir = tempfile::tempdir().unwrap();
    let client_dir = tempfile::tempdir().unwrap();

    let body = b"console.log('the real library');".to_vec();
    let server_cache = cache(server_dir.path());
    let mut headers = http::HeaderMap::new();
    headers.insert("cache-control", "max-age=600".parse().unwrap());
    let now = syndeo_cache::headers::now_secs();
    server_cache
        .store(
            "GET",
            "https://cdn.test/lib.js",
            &http::HeaderMap::new(),
            200,
            &headers,
            &body,
            now,
            now,
        )
        .unwrap();

    let server = PeerNode::start(server_cache, config()).unwrap();
    let client_cache = cache(client_dir.path());
    let client = PeerNode::start(client_cache.clone(), config()).unwrap();
    connect(&server, &client).await;

    // The client has never fetched this. All it has is the hash from the markup,
    // which is exactly the case peer fetch exists for.
    let declared = Hash::compute(Algorithm::Sha384, &body);
    assert!(client_cache.content_for_integrity(&declared).unwrap().is_none());

    let fetched = client.fetch_integrity(&declared).await.unwrap();
    assert_eq!(fetched, body);
}

#[tokio::test]
async fn bytes_that_do_not_hash_to_the_request_are_refused() {
    let server_dir = tempfile::tempdir().unwrap();
    let client_dir = tempfile::tempdir().unwrap();

    let real = b"console.log('the real library');".to_vec();
    let substituted = b"console.log('exfiltrate everything');".to_vec();

    // A peer that will answer the real library's digest with different bytes.
    let server_cache = cache(server_dir.path());
    let mut headers = http::HeaderMap::new();
    headers.insert("cache-control", "max-age=600".parse().unwrap());
    let now = syndeo_cache::headers::now_secs();
    server_cache
        .store(
            "GET",
            "https://cdn.test/lib.js",
            &http::HeaderMap::new(),
            200,
            &headers,
            &substituted,
            now,
            now,
        )
        .unwrap();
    let declared = Hash::compute(Algorithm::Sha384, &real);
    server_cache
        .associate_integrity(&declared, ContentId::of(&substituted))
        .unwrap();

    let server = PeerNode::start(server_cache, config()).unwrap();
    let client = PeerNode::start(cache(client_dir.path()), config()).unwrap();
    connect(&server, &client).await;

    // The peer answers. The bytes do not hash to what was asked for, so they are
    // discarded and the fetch fails rather than succeeding with a lie.
    let result = client.fetch_integrity(&declared).await;
    assert!(result.is_err(), "substituted bytes must never be returned");
}

#[tokio::test]
async fn a_hash_nobody_has_is_reported_as_missing() {
    let server_dir = tempfile::tempdir().unwrap();
    let client_dir = tempfile::tempdir().unwrap();

    let server = PeerNode::start(cache(server_dir.path()), config()).unwrap();
    let client = PeerNode::start(cache(client_dir.path()), config()).unwrap();
    connect(&server, &client).await;

    let result = client
        .fetch(BlobRequest::content(ContentId::of(b"nobody has this")))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn with_no_peers_a_fetch_fails_immediately_rather_than_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let node = PeerNode::start(cache(dir.path()), config()).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        node.fetch_content(ContentId::of(b"anything")),
    )
    .await;
    assert!(result.is_ok(), "it should not have waited");
    assert!(result.unwrap().is_err());
}
