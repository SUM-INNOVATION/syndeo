//! Who follows a redirect.
//!
//! By default the network process follows it, because the shell, the agent and
//! the renderers want the resource. A proxy asks for `RedirectMode::Manual` and
//! gets the redirect itself — status, every header and the body — because the
//! browser behind it has to follow it for the page to end up as the right
//! origin and for the redirect's cookies to arrive.

mod support;

use std::sync::Arc;
use support::{respond, Origin};
use syndeo_net::{FetchRequest, Net, NetConfig, RedirectMode};

fn net(cache_root: &std::path::Path) -> Net {
    Net::new(NetConfig {
        cache_root: cache_root.to_path_buf(),
        ..NetConfig::default()
    })
    .unwrap()
}

/// `/start` redirects to `/target`, setting two cookies on the way.
async fn redirecting_origin() -> Origin {
    Origin::start(Arc::new(|request, _n| match request.uri().path() {
        "/start" => respond(
            302,
            &[
                ("location", "/target"),
                ("set-cookie", "a=1; Path=/"),
                ("set-cookie", "b=2; Path=/"),
                ("cache-control", "no-store"),
            ],
            b"moved",
        ),
        "/target" => respond(200, &[("cache-control", "no-store")], b"arrived"),
        _ => respond(404, &[], b"no"),
    }))
    .await
}

#[tokio::test]
async fn an_ordinary_fetch_follows_the_redirect() {
    let dir = tempfile::tempdir().unwrap();
    let origin = redirecting_origin().await;
    let net = net(dir.path());

    let response = net
        .fetch(FetchRequest::get(origin.url("/start")))
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.final_url, origin.url("/target"));
    assert_eq!(response.redirects, 1);
    assert_eq!(&response.body.collect().await.unwrap()[..], b"arrived");
    assert_eq!(origin.hits(), 2);
}

#[tokio::test]
async fn a_manual_fetch_returns_the_redirect_itself() {
    let dir = tempfile::tempdir().unwrap();
    let origin = redirecting_origin().await;
    let net = net(dir.path());

    let mut request = FetchRequest::get(origin.url("/start"));
    request.redirect = RedirectMode::Manual;
    let response = net.fetch(request).await.unwrap();

    assert_eq!(response.status, 302);
    assert_eq!(response.final_url, origin.url("/start"));
    assert_eq!(response.redirects, 0);
    assert_eq!(response.headers.get("location").unwrap(), "/target");
    let cookies: Vec<&str> = response
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .collect();
    assert_eq!(cookies, ["a=1; Path=/", "b=2; Path=/"]);
    assert_eq!(&response.body.collect().await.unwrap()[..], b"moved");
    // The destination was never asked for.
    assert_eq!(origin.hits(), 1);
}
