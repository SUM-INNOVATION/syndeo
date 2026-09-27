//! A body a peer supplies is checked and used, and nothing of it is kept: a
//! peer hands over bytes, not a response a cache entry could be made from.

use std::sync::Arc;
use std::time::Duration;
use syndeo_cache::sri::{Algorithm, Hash};
use syndeo_cache::{Cache, ContentId, Integrity};
use syndeo_net::{FetchRequest, Net, NetConfig, Source};
use syndeo_peer::{PeerConfig, PeerNode};

const LIB: &[u8] = b"export const fromAPeer = true;";

fn loopback() -> PeerConfig {
    PeerConfig {
        listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
        request_timeout: Duration::from_secs(5),
        ..PeerConfig::default()
    }
}

fn every_file(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(every_file(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

#[tokio::test]
async fn a_body_from_a_peer_is_used_and_leaves_nothing_behind() {
    // A node that holds the library and may share it.
    let server_dir = tempfile::tempdir().unwrap();
    let server_cache = Arc::new(Cache::open(server_dir.path()).unwrap());
    let mut headers = http::HeaderMap::new();
    headers.insert("cache-control", "max-age=600".parse().unwrap());
    let now = server_cache.now();
    server_cache
        .store(
            None,
            "GET",
            "https://cdn.test/lib.js",
            &http::HeaderMap::new(),
            200,
            &headers,
            LIB,
            now,
            now,
        )
        .unwrap();
    let declared = Integrity {
        hashes: vec![Hash::compute(Algorithm::Sha384, LIB)],
    };
    assert!(!server_cache
        .grant_peer_eligibility(ContentId::of(LIB), &declared)
        .unwrap()
        .is_empty());
    let server = PeerNode::start(server_cache, loopback()).unwrap();
    let mut address = None;
    for _ in 0..200 {
        if let Some(found) = server.listeners().await.unwrap().into_iter().next() {
            address = Some(found);
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let dial = format!("{}/p2p/{}", address.unwrap(), server.peer_id())
        .parse()
        .unwrap();

    // A browser whose origin is unreachable, so only the peer can answer.
    let client_dir = tempfile::tempdir().unwrap();
    let net = Net::new(NetConfig {
        cache_root: client_dir.path().to_path_buf(),
        peers: Some(PeerConfig {
            bootstrap: vec![dial],
            ..loopback()
        }),
        ..NetConfig::default()
    })
    .unwrap();

    let mut served = None;
    for _ in 0..50 {
        let mut request = FetchRequest::get("http://127.0.0.1:9/lib.js");
        request.integrity = Some(declared.clone());
        if let Ok(response) = net.fetch(request).await {
            let source = response.source;
            let body = response.body.collect().await.unwrap().to_vec();
            served = Some((source, body));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (source, body) = served.expect("the peer never answered");
    assert_eq!(source, Source::Peer);
    assert_eq!(body, LIB);

    assert_eq!(net.cache().stats().unwrap().peer_accepted, 1);
    assert_eq!(net.cache().stats().unwrap().entries, 0);
    assert!(
        every_file(&client_dir.path().join("blobs")).is_empty(),
        "an unindexed blob was left behind: {:?}",
        every_file(&client_dir.path().join("blobs"))
    );
    assert!(!net.cache().has_content(ContentId::of(LIB)));
    assert_eq!(net.peer_status().await.unwrap().announced, 0);
}
