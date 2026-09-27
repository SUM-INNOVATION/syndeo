//! A declared integrity value is enforced on every path a body can take: from
//! the origin, out of the store fresh, stale or revalidated, and after a
//! redirect. A body that does not satisfy it is never stored, never shared
//! with peers, never announced, and never returned.

mod support;

use std::sync::Arc;
use std::time::Duration;
use support::{respond, Origin};
use syndeo_cache::sri::{Algorithm, Hash};
use syndeo_cache::{ContentId, Integrity};
use syndeo_net::{FetchRequest, Net, NetConfig, NetError, RedirectMode, Source};

fn net(cache_root: &std::path::Path) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        ..NetConfig::default()
    })
    .unwrap()
}

/// A network process that is also a peer node, alone on loopback, so what it
/// announces can be counted.
fn net_with_peers(cache_root: &std::path::Path) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        peers: Some(syndeo_peer::PeerConfig {
            listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
            ..syndeo_peer::PeerConfig::default()
        }),
        ..NetConfig::default()
    })
    .unwrap()
}

fn sha384(body: &[u8]) -> Hash {
    Hash::compute(Algorithm::Sha384, body)
}

fn declared(hashes: &[Hash]) -> Integrity {
    Integrity {
        hashes: hashes.to_vec(),
    }
}

fn with_integrity(url: &str, integrity: Integrity) -> FetchRequest {
    let mut request = FetchRequest::get(url);
    request.integrity = Some(integrity);
    request
}

async fn fetch(net: &Net, request: FetchRequest) -> Result<(Source, Vec<u8>), NetError> {
    let response = net.fetch(request).await?;
    let source = response.source;
    let body = response.body.collect().await?.to_vec();
    Ok((source, body))
}

/// How many provider records the node has published, once the spawned
/// announcements have had a moment to land.
async fn announced(net: &Net, expected: usize) -> usize {
    let mut last = 0;
    for _ in 0..100 {
        last = net.peer_status().await.unwrap().announced;
        if last >= expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Give a wrong extra announcement the chance to show up too.
    tokio::time::sleep(Duration::from_millis(100)).await;
    net.peer_status().await.unwrap().announced.max(last)
}

const LIB: &[u8] = b"export const library = 'the real one';";
const EVIL: &[u8] = b"export const library = 'substituted';";

#[tokio::test]
async fn a_matching_origin_body_is_stored_shared_and_announced() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| {
        respond(200, &[("cache-control", "max-age=600")], LIB)
    }))
    .await;
    let net = net_with_peers(dir.path());

    let (source, body) = fetch(
        &net,
        with_integrity(&origin.url("/lib.js"), declared(&[sha384(LIB)])),
    )
    .await
    .unwrap();
    assert_eq!(source, Source::Origin);
    assert_eq!(body, LIB);
    assert_eq!(
        net.cache().peer_eligibility(ContentId::of(LIB)).unwrap(),
        vec![sha384(LIB)]
    );
    assert_eq!(announced(&net, 1).await, 1);
}

#[tokio::test]
async fn a_mismatching_origin_body_is_refused_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| {
        respond(200, &[("cache-control", "max-age=600")], EVIL)
    }))
    .await;
    let net = net_with_peers(dir.path());

    let result = fetch(
        &net,
        with_integrity(&origin.url("/lib.js"), declared(&[sha384(LIB)])),
    )
    .await;
    assert!(
        matches!(result, Err(NetError::Integrity(_))),
        "the substituted bytes were returned: {result:?}"
    );
    assert_eq!(net.cache().stats().unwrap().entries, 0, "it was stored");
    assert!(net
        .cache()
        .peer_eligibility(ContentId::of(EVIL))
        .unwrap()
        .is_empty());
    assert_eq!(announced(&net, 0).await, 0, "it was announced");
}

#[tokio::test]
async fn only_matching_hashes_at_the_strongest_level_are_shared_and_announced() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| {
        respond(200, &[("cache-control", "max-age=600")], LIB)
    }))
    .await;
    let net = net_with_peers(dir.path());

    // A correct weaker hash, a correct strongest one, and a wrong strongest
    // one. The declaration is satisfied; only one of the three is shareable.
    let weak = Hash::compute(Algorithm::Sha256, LIB);
    let wrong = Hash {
        algorithm: Algorithm::Sha384,
        digest: vec![1; 48],
    };
    fetch(
        &net,
        with_integrity(
            &origin.url("/lib.js"),
            declared(&[weak, wrong, sha384(LIB)]),
        ),
    )
    .await
    .unwrap();

    assert_eq!(
        net.cache().peer_eligibility(ContentId::of(LIB)).unwrap(),
        vec![sha384(LIB)]
    );
    assert_eq!(announced(&net, 1).await, 1);
}

#[tokio::test]
async fn fresh_stale_and_revalidated_hits_are_checked_and_served() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, _| match request.uri().path() {
        "/fresh.js" => respond(200, &[("cache-control", "max-age=600")], LIB),
        "/stale.js" => respond(
            200,
            &[
                ("cache-control", "max-age=0, stale-while-revalidate=600"),
                ("etag", "\"s\""),
            ],
            LIB,
        ),
        _ => {
            if request.headers().contains_key("if-none-match") {
                respond(
                    304,
                    &[("etag", "\"r\""), ("cache-control", "max-age=0")],
                    b"",
                )
            } else {
                respond(
                    200,
                    &[("etag", "\"r\""), ("cache-control", "max-age=0")],
                    LIB,
                )
            }
        }
    }))
    .await;
    let net = net(dir.path());
    let integrity = declared(&[sha384(LIB)]);

    for (path, second) in [
        ("/fresh.js", Source::Cache),
        ("/stale.js", Source::CacheStale),
        ("/revalidated.js", Source::Revalidated),
    ] {
        let url = origin.url(path);
        fetch(&net, with_integrity(&url, integrity.clone()))
            .await
            .unwrap();
        let (source, body) = fetch(&net, with_integrity(&url, integrity.clone()))
            .await
            .unwrap();
        assert_eq!(source, second, "{path}");
        assert_eq!(body, LIB, "{path}");
    }
}

#[tokio::test]
async fn a_stored_representation_the_declaration_does_not_name_is_replaced_once() {
    // What is stored is representation A, fetched with no integrity. A page
    // then declares integrity for B, which is what the origin now serves.
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, n| {
        let body: &'static [u8] = if n == 0 { EVIL } else { LIB };
        respond(200, &[("cache-control", "max-age=600")], body)
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/lib.js");

    let (_, a) = fetch(&net, FetchRequest::get(&url)).await.unwrap();
    assert_eq!(a, EVIL);

    let (source, b) = fetch(&net, with_integrity(&url, declared(&[sha384(LIB)])))
        .await
        .unwrap();
    assert_eq!(
        source,
        Source::Origin,
        "A was served for a declaration of B"
    );
    assert_eq!(b, LIB);
    assert!(
        net.cache()
            .peer_eligibility(ContentId::of(EVIL))
            .unwrap()
            .is_empty(),
        "a representation that failed the declaration became shareable"
    );
    assert_eq!(origin.hits(), 2, "exactly one fetch to replace A");

    let (source, _) = fetch(&net, with_integrity(&url, declared(&[sha384(LIB)])))
        .await
        .unwrap();
    assert_eq!(source, Source::Cache, "B was not stored");
    assert_eq!(origin.hits(), 2);

    // A declaration nothing will satisfy: one fetch, then an error, and not
    // another request after it.
    let result = fetch(&net, with_integrity(&url, declared(&[sha384(EVIL)]))).await;
    assert!(matches!(result, Err(NetError::Integrity(_))), "{result:?}");
    assert_eq!(origin.hits(), 3);
    assert_eq!(net.cache().stats().unwrap().entries, 0);
}

#[tokio::test]
async fn a_redirect_hop_is_not_checked_but_what_it_leads_to_is() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, _| match request.uri().path() {
        "/old.js" => respond(
            301,
            &[("location", "/lib.js"), ("cache-control", "max-age=600")],
            b"moved permanently",
        ),
        "/lib.js" => respond(200, &[("cache-control", "max-age=600")], LIB),
        _ => respond(200, &[("cache-control", "max-age=600")], EVIL),
    }))
    .await;
    let net = net(dir.path());

    let (_, body) = fetch(
        &net,
        with_integrity(&origin.url("/old.js"), declared(&[sha384(LIB)])),
    )
    .await
    .unwrap();
    assert_eq!(body, LIB);
    assert!(net
        .cache()
        .peer_eligibility(ContentId::of(b"moved permanently"))
        .unwrap()
        .is_empty());
    assert!(!net
        .cache()
        .peer_eligibility(ContentId::of(LIB))
        .unwrap()
        .is_empty());

    // Handed back rather than followed, a redirect is the final answer, and it
    // satisfies no declaration.
    let mut manual = with_integrity(&origin.url("/old.js"), declared(&[sha384(LIB)]));
    manual.redirect = RedirectMode::Manual;
    assert!(matches!(
        fetch(&net, manual).await,
        Err(NetError::Integrity(_))
    ));
}

#[tokio::test]
async fn a_declaration_that_constrains_nothing_is_refused_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| {
        respond(200, &[("cache-control", "max-age=600")], LIB)
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/lib.js");

    // Empty, and made only of algorithms nobody knows.
    for integrity in [
        Integrity::default(),
        Integrity::parse("md5-deadbeef sha1-cafe").unwrap(),
    ] {
        assert!(integrity.is_empty());
        let result = fetch(&net, with_integrity(&url, integrity)).await;
        assert!(matches!(result, Err(NetError::Integrity(_))), "{result:?}");
    }

    // A range, and a method, that a whole-body declaration cannot describe.
    let ranged = with_integrity(&url, declared(&[sha384(LIB)])).header("range", "bytes=0-3");
    assert!(matches!(
        fetch(&net, ranged).await,
        Err(NetError::Integrity(_))
    ));
    let mut head = with_integrity(&url, declared(&[sha384(LIB)]));
    head.method = http::Method::HEAD;
    assert!(matches!(
        fetch(&net, head).await,
        Err(NetError::Integrity(_))
    ));

    assert_eq!(origin.hits(), 0, "a refused declaration reached the origin");
}

const OTHER: &[u8] = b"export const library = 'the next release';";

#[tokio::test]
async fn a_cached_body_becomes_shareable_once_when_a_declaration_matches_it() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| {
        respond(200, &[("cache-control", "max-age=600")], LIB)
    }))
    .await;
    let net = net_with_peers(dir.path());
    let url = origin.url("/lib.js");

    // Stored by a fetch that declared nothing: not shareable.
    fetch(&net, FetchRequest::get(&url)).await.unwrap();
    assert!(net
        .cache()
        .peer_eligibility(ContentId::of(LIB))
        .unwrap()
        .is_empty());

    // A page then declares integrity it satisfies, and it is served from the
    // store — which is a verification, and makes it shareable.
    for _ in 0..4 {
        let (source, body) = fetch(&net, with_integrity(&url, declared(&[sha384(LIB)])))
            .await
            .unwrap();
        assert_eq!(source, Source::Cache);
        assert_eq!(body, LIB);
    }
    assert_eq!(
        net.cache().peer_eligibility(ContentId::of(LIB)).unwrap(),
        vec![sha384(LIB)]
    );
    assert_eq!(
        net.announcements(),
        1,
        "four verified hits should announce the one name once"
    );
    assert_eq!(announced(&net, 1).await, 1);
    assert_eq!(origin.hits(), 1);
}

/// Stored with no declaration and born stale, so the next request with a
/// declaration is served from the store and refreshed behind it; the refresh
/// answers with `refreshed`.
async fn refreshing_origin(refreshed: &'static [u8]) -> Origin {
    Origin::start(Arc::new(move |_, n| {
        if n == 0 {
            respond(
                200,
                &[
                    ("cache-control", "max-age=0, stale-while-revalidate=600"),
                    ("etag", "\"one\""),
                ],
                LIB,
            )
        } else {
            respond(
                200,
                &[("cache-control", "max-age=600"), ("etag", "\"two\"")],
                refreshed,
            )
        }
    }))
    .await
}

#[tokio::test]
async fn a_matching_background_refresh_makes_its_new_body_shareable() {
    let dir = tempfile::tempdir().unwrap();
    let origin = refreshing_origin(OTHER).await;
    let net = net_with_peers(dir.path());
    let url = origin.url("/lib.js");
    fetch(&net, FetchRequest::get(&url)).await.unwrap();

    // Either release satisfies the page: any match at the strongest level.
    let either = declared(&[sha384(LIB), sha384(OTHER)]);
    let (source, body) = fetch(&net, with_integrity(&url, either)).await.unwrap();
    assert_eq!(source, Source::CacheStale);
    assert_eq!(body, LIB);

    let mut shared = Vec::new();
    for _ in 0..100 {
        shared = net.cache().peer_eligibility(ContentId::of(OTHER)).unwrap();
        if !shared.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        shared,
        vec![sha384(OTHER)],
        "the refreshed body was not made shareable"
    );
    // The body it replaced is gone from the store, and so is its sharing.
    assert!(net
        .cache()
        .peer_eligibility(ContentId::of(LIB))
        .unwrap()
        .is_empty());
    // One name for the stale body served, one for the refreshed body stored.
    assert_eq!(announced(&net, 2).await, 2);
    assert_eq!(net.announcements(), 2);
}

#[tokio::test]
async fn a_mismatching_background_refresh_stores_shares_and_announces_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let origin = refreshing_origin(EVIL).await;
    let net = net_with_peers(dir.path());
    let url = origin.url("/lib.js");
    fetch(&net, FetchRequest::get(&url)).await.unwrap();

    let (source, _) = fetch(&net, with_integrity(&url, declared(&[sha384(LIB)])))
        .await
        .unwrap();
    assert_eq!(source, Source::CacheStale);
    assert_eq!(origin.wait_for_hits(2, Duration::from_secs(5)).await, 2);
    // Let the refresh finish whatever it was going to do.
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(net
        .cache()
        .peer_eligibility(ContentId::of(EVIL))
        .unwrap()
        .is_empty());
    assert_eq!(
        net.announcements(),
        1,
        "only the verified stale body's name"
    );
    // The stored entry is still the body that satisfies the declaration.
    let (_, body) = fetch(&net, with_integrity(&url, declared(&[sha384(LIB)])))
        .await
        .unwrap();
    assert_eq!(body, LIB);
}
