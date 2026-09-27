//! A response's cookies go to the client whose request produced it, and to
//! nobody after that: nothing served out of the store carries one.

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

struct Got {
    source: Source,
    cookies: Vec<String>,
    body: Vec<u8>,
}

async fn fetch(net: &Net, url: &str) -> Got {
    let response = net.fetch(FetchRequest::get(url)).await.unwrap();
    let cookies = response
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    let source = response.source;
    // Collecting is also what writes a streamed body into the store.
    let body = response.body.collect().await.unwrap().to_vec();
    Got {
        source,
        cookies,
        body,
    }
}

#[tokio::test]
async fn the_origin_response_carries_every_cookie_and_the_hit_carries_none() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, _| {
        respond(
            200,
            &[
                ("cache-control", "max-age=600"),
                ("set-cookie", "session=abc; Path=/"),
                ("set-cookie", "theme=dark; Path=/"),
            ],
            b"page",
        )
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/page");

    let first = fetch(&net, &url).await;
    assert_eq!(first.source, Source::Origin);
    assert_eq!(
        first.cookies,
        ["session=abc; Path=/", "theme=dark; Path=/"],
        "the client that caused the fetch gets every cookie, in order"
    );

    let second = fetch(&net, &url).await;
    assert_eq!(second.source, Source::Cache);
    assert!(second.cookies.is_empty(), "replayed {:?}", second.cookies);
    assert_eq!(second.body, b"page");
    assert_eq!(origin.hits(), 1);
}

#[tokio::test]
async fn a_304_delivers_its_own_cookies_once_and_never_the_stored_ones() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, _| {
        if request.headers().contains_key("if-none-match") {
            return respond(
                304,
                &[
                    ("cache-control", "max-age=600"),
                    ("etag", "\"v1\""),
                    ("set-cookie", "renewed=1; Path=/"),
                    ("set-cookie", "also=2; Path=/"),
                ],
                b"",
            );
        }
        respond(
            200,
            &[
                ("cache-control", "max-age=0"),
                ("etag", "\"v1\""),
                ("set-cookie", "original=1; Path=/"),
            ],
            b"body",
        )
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/revalidated");

    let first = fetch(&net, &url).await;
    assert_eq!(first.cookies, ["original=1; Path=/"]);

    let second = fetch(&net, &url).await;
    assert_eq!(second.source, Source::Revalidated);
    assert_eq!(
        second.cookies,
        ["renewed=1; Path=/", "also=2; Path=/"],
        "the 304's own cookies, in order, and not the stored response's"
    );
    assert_eq!(second.body, b"body");

    let third = fetch(&net, &url).await;
    assert_eq!(third.source, Source::Cache);
    assert!(third.cookies.is_empty(), "replayed {:?}", third.cookies);
    assert_eq!(origin.hits(), 2);
}

#[tokio::test]
async fn stale_while_revalidate_serves_no_cookie_and_keeps_none_from_the_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|_, n| {
        if n == 0 {
            respond(
                200,
                &[
                    ("cache-control", "max-age=0, stale-while-revalidate=600"),
                    ("etag", "\"v1\""),
                    ("set-cookie", "first=1"),
                ],
                b"one",
            )
        } else {
            // The background refresh: a new body, with a cookie nobody is
            // waiting for.
            respond(
                200,
                &[
                    ("cache-control", "max-age=600"),
                    ("set-cookie", "refresh=1"),
                ],
                b"two",
            )
        }
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/swr");

    assert_eq!(fetch(&net, &url).await.cookies, ["first=1"]);

    let stale = fetch(&net, &url).await;
    assert_eq!(stale.source, Source::CacheStale);
    assert!(stale.cookies.is_empty(), "replayed {:?}", stale.cookies);

    assert_eq!(origin.wait_for_hits(2, Duration::from_secs(5)).await, 2);
    // The refresh lands asynchronously; wait until the new body is served.
    let mut refreshed = fetch(&net, &url).await;
    for _ in 0..100 {
        if refreshed.body == b"two" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        refreshed = fetch(&net, &url).await;
    }
    assert_eq!(refreshed.body, b"two", "the refresh never landed");
    assert_eq!(refreshed.source, Source::Cache);
    assert!(
        refreshed.cookies.is_empty(),
        "replayed {:?}",
        refreshed.cookies
    );
}

#[tokio::test]
async fn the_destination_of_a_followed_redirect_is_served_without_its_cookie() {
    let dir = tempfile::tempdir().unwrap();
    let origin = Origin::start(Arc::new(|request, _| match request.uri().path() {
        "/start" => respond(
            301,
            &[
                ("cache-control", "max-age=600"),
                ("location", "/end"),
                ("set-cookie", "hop=1"),
            ],
            b"",
        ),
        _ => respond(
            200,
            &[("cache-control", "max-age=600"), ("set-cookie", "end=1")],
            b"destination",
        ),
    }))
    .await;
    let net = net(dir.path());
    let url = origin.url("/start");

    let first = fetch(&net, &url).await;
    assert_eq!(first.body, b"destination");
    assert_eq!(first.cookies, ["end=1"]);

    // The destination comes from the store now, without the cookie it set.
    // (The hop itself is not cached here: its unread body is dropped when the
    // redirect is followed. A cached redirect is covered in the cache and
    // proxy suites.)
    let second = fetch(&net, &url).await;
    assert_eq!(second.source, Source::Cache);
    assert_eq!(second.body, b"destination");
    assert!(second.cookies.is_empty(), "replayed {:?}", second.cookies);
}
