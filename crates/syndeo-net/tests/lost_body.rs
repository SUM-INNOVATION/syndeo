//! A body the store has lost is fetched again, once, and never turns into a
//! URL that fails until eviction or a request that loops.

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use support::{respond, Origin};
use syndeo_cache::ContentId;
use syndeo_net::{FetchRequest, Net, NetConfig, NetError, Source};

fn net(cache_root: &Path) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        ..NetConfig::default()
    })
    .unwrap()
}

/// Where the blob store keeps a body: `<root>/blobs/ab/cd/<hex>`.
fn blob_file(cache_root: &Path, body: &[u8]) -> PathBuf {
    let hex = ContentId::of(body).to_hex();
    cache_root
        .join("blobs")
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(&hex)
}

async fn fetch(net: &Net, url: &str) -> Result<(Source, Vec<u8>), NetError> {
    let response = net.fetch(FetchRequest::get(url)).await?;
    let source = response.source;
    let body = response.body.collect().await?.to_vec();
    Ok((source, body))
}

#[tokio::test]
async fn a_body_deleted_from_disk_is_fetched_and_stored_again() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| {
        respond(200, &[("cache-control", "max-age=600")], b"the body")
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/asset");

    assert_eq!(fetch(&net, &url).await.unwrap().0, Source::Origin);
    std::fs::remove_file(blob_file(dir.path(), b"the body")).unwrap();

    let (source, body) = fetch(&net, &url).await.unwrap();
    assert_eq!(
        source,
        Source::Origin,
        "a lost body is a miss, not an error"
    );
    assert_eq!(body, b"the body");

    let (source, body) = fetch(&net, &url).await.unwrap();
    assert_eq!(source, Source::Cache, "and the store is repaired");
    assert_eq!(body, b"the body");
    assert_eq!(origin.hits(), 2);
}

/// An origin that, when asked conditionally, deletes the stored body before
/// answering 304 — the window between the lookup and the 304 in which a body
/// can go missing, made to happen every time. `unconditional` is what it
/// answers a request carrying no validator.
async fn losing_origin(cache_root: PathBuf, unconditional: u16) -> (Origin, Arc<Mutex<Vec<bool>>>) {
    let conditional_seen = Arc::new(Mutex::new(Vec::new()));
    let log = conditional_seen.clone();
    let origin = Origin::start(Arc::new(move |request, n| {
        let conditional = request.headers().contains_key("if-none-match");
        log.lock().unwrap().push(conditional);
        if n == 0 {
            return respond(
                200,
                &[("cache-control", "max-age=0"), ("etag", "\"v1\"")],
                b"confirmed",
            );
        }
        if conditional {
            let _ = std::fs::remove_file(blob_file(&cache_root, b"confirmed"));
            return respond(304, &[("etag", "\"v1\"")], b"");
        }
        respond(
            unconditional,
            &[("cache-control", "max-age=600"), ("etag", "\"v1\"")],
            if unconditional == 200 {
                b"confirmed"
            } else {
                b""
            },
        )
    }))
    .await;
    (origin, conditional_seen)
}

#[tokio::test]
async fn a_body_lost_before_its_304_is_fetched_once_without_a_validator() {
    let dir = tempfile::tempdir().unwrap();
    let (origin, conditional) = losing_origin(dir.path().to_path_buf(), 200).await;
    let net = net(dir.path());
    let url = origin.url("/confirmed");

    assert_eq!(fetch(&net, &url).await.unwrap().0, Source::Origin);

    let (source, body) = fetch(&net, &url).await.unwrap();
    assert_eq!(source, Source::Origin);
    assert_eq!(body, b"confirmed");
    assert_eq!(
        *conditional.lock().unwrap(),
        [false, true, false],
        "one conditional request, then exactly one without a validator"
    );

    assert_eq!(fetch(&net, &url).await.unwrap().0, Source::Cache);
    assert_eq!(origin.hits(), 3);
}

#[tokio::test]
async fn a_304_to_the_unconditional_refetch_is_an_error_and_not_a_loop() {
    let dir = tempfile::tempdir().unwrap();
    let (origin, conditional) = losing_origin(dir.path().to_path_buf(), 304).await;
    let net = net(dir.path());
    let url = origin.url("/confirmed");

    fetch(&net, &url).await.unwrap();

    match fetch(&net, &url).await {
        Err(NetError::LostBody(lost)) => assert_eq!(lost, url),
        other => panic!("expected the lost body to be reported, got {other:?}"),
    }
    assert_eq!(*conditional.lock().unwrap(), [false, true, false]);
    assert_eq!(origin.hits(), 3, "exactly one refetch, and no more");
}
