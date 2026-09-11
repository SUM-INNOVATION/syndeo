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
            None,
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
            None,
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
    assert!(client_cache
        .content_for_integrity(&declared)
        .unwrap()
        .is_none());

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
            None,
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

// ------------------------------------------------------- discovery and standing

/// A body stored in a cache, and the integrity hash a page would declare for it.
fn seed(cache: &Arc<Cache>, body: &[u8]) -> Hash {
    let mut headers = http::HeaderMap::new();
    headers.insert("cache-control", "max-age=600".parse().unwrap());
    let now = cache.now();
    cache
        .store(
            None,
            "GET",
            "https://example.test/lib.js",
            &http::HeaderMap::new(),
            200,
            &headers,
            body,
            now,
            now,
        )
        .unwrap();
    Hash::compute(Algorithm::Sha384, body)
}

#[tokio::test]
async fn a_node_reaches_a_peer_it_was_never_told_about() {
    // Three nodes in a line: the newcomer only ever hears about the introducer,
    // and has to find the holder through it. Without a DHT this is a miss.
    let holder_dir = tempfile::tempdir().unwrap();
    let introducer_dir = tempfile::tempdir().unwrap();
    let newcomer_dir = tempfile::tempdir().unwrap();

    let body = vec![b'd'; 40_000];
    let holder_cache = cache(holder_dir.path());
    let hash = seed(&holder_cache, &body);

    let holder = PeerNode::start(holder_cache, config()).unwrap();
    let introducer = PeerNode::start(cache(introducer_dir.path()), config()).unwrap();
    let newcomer = PeerNode::start(cache(newcomer_dir.path()), config()).unwrap();

    // The holder tells the DHT it has these bytes.
    connect(&introducer, &holder).await;
    holder.announce_integrity(&hash).await.unwrap();

    // The newcomer knows the introducer and nothing else.
    connect(&introducer, &newcomer).await;
    assert!(
        !newcomer.peers().await.unwrap().contains(&holder.peer_id()),
        "the newcomer was told about the holder after all"
    );

    let found = wait_for(|| async {
        newcomer
            .fetch_integrity(&hash)
            .await
            .ok()
            .filter(|found| found == &body)
    })
    .await;
    assert!(
        found.is_some(),
        "the body was never found through a peer nobody named"
    );
}

#[tokio::test]
async fn what_a_peer_gave_and_took_is_recorded() {
    let server_dir = tempfile::tempdir().unwrap();
    let client_dir = tempfile::tempdir().unwrap();

    let body = vec![b'l'; 10_000];
    let server_cache = cache(server_dir.path());
    let hash = seed(&server_cache, &body);

    let server = PeerNode::start(server_cache, config()).unwrap();
    let client = PeerNode::start(cache(client_dir.path()), config()).unwrap();
    connect(&server, &client).await;

    assert_eq!(client.fetch_integrity(&hash).await.unwrap(), body);

    let client_view = client.status().await.unwrap();
    let server_side = client_view
        .connected
        .iter()
        .find(|report| report.peer == server.peer_id())
        .expect("the server is connected");
    assert_eq!(
        server_side.ledger.received, 1,
        "the client did not record the gift"
    );
    assert_eq!(server_side.ledger.bytes_received, body.len() as u64);
    assert!(
        server_side.ledger.standing() > 0,
        "a giver should stand well"
    );

    let server_view = server.status().await.unwrap();
    let client_side = server_view
        .connected
        .iter()
        .find(|report| report.peer == client.peer_id())
        .expect("the client is connected");
    assert_eq!(
        client_side.ledger.served, 1,
        "the server did not record the gift"
    );
    assert_eq!(client_side.ledger.debt, 1);
    assert!(
        client_side.ledger.standing() < 0,
        "a taker should not look like a giver"
    );
}

#[tokio::test]
async fn a_peer_that_only_takes_is_eventually_asked_to_wait() {
    use syndeo_peer::swarm::OPENING_CREDIT;

    let server_dir = tempfile::tempdir().unwrap();
    let client_dir = tempfile::tempdir().unwrap();

    let server_cache = cache(server_dir.path());
    // More distinct bodies than the opening credit allows.
    let hashes: Vec<Hash> = (0..OPENING_CREDIT + 4)
        .map(|i| {
            let body = format!("body number {i}").repeat(64).into_bytes();
            let hash = Hash::compute(Algorithm::Sha384, &body);
            let mut headers = http::HeaderMap::new();
            headers.insert("cache-control", "max-age=600".parse().unwrap());
            let now = server_cache.now();
            server_cache
                .store(
                    None,
                    "GET",
                    &format!("https://example.test/{i}.js"),
                    &http::HeaderMap::new(),
                    200,
                    &headers,
                    &body,
                    now,
                    now,
                )
                .unwrap();
            hash
        })
        .collect();

    let server = PeerNode::start(server_cache, config()).unwrap();
    let client = PeerNode::start(cache(client_dir.path()), config()).unwrap();
    connect(&server, &client).await;

    let mut given = 0;
    for hash in &hashes {
        if client.fetch_integrity(hash).await.is_ok() {
            given += 1;
        }
    }

    assert_eq!(
        given, OPENING_CREDIT as usize,
        "a node that only takes should get exactly its opening credit and no more"
    );
    let view = server.status().await.unwrap();
    let report = view
        .connected
        .iter()
        .find(|r| r.peer == client.peer_id())
        .expect("the client is connected");
    assert!(!report.ledger.in_credit(), "the taker still has credit");
}
