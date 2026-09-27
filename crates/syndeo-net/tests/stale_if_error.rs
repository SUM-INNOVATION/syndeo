//! `stale-if-error` lets a stored body stand in for an unreachable origin for
//! as long as its window says, and no longer.

use syndeo_net::{FetchRequest, Net, NetConfig, Source};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// An origin that answers the first request with `cache_control`, dated
/// `age` seconds ago, and then goes away: every later connection is closed
/// before a response, which is what an unreachable origin looks like.
async fn origin_that_fails_after_one(cache_control: &str, age: u64) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let date = syndeo_cache::headers::format_http_date(syndeo_cache::headers::now_secs() - age);
    let response = format!(
        "HTTP/1.1 200 OK\r\ndate: {date}\r\ncache-control: {cache_control}\r\n\
         etag: \"v1\"\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbody"
    );
    tokio::spawn(async move {
        let mut first = true;
        while let Ok((mut stream, _)) = listener.accept().await {
            if !first {
                drop(stream);
                continue;
            }
            first = false;
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await.unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    format!("http://{address}/asset")
}

async fn fetched(net: &Net, url: &str) -> Result<Source, syndeo_net::NetError> {
    let response = net.fetch(FetchRequest::get(url)).await?;
    let source = response.source;
    response.body.collect().await?;
    Ok(source)
}

fn net(dir: &std::path::Path) -> Net {
    Net::new(NetConfig {
        cache_root: dir.to_path_buf(),
        ..NetConfig::default()
    })
    .unwrap()
}

#[tokio::test]
async fn within_the_window_the_stored_body_stands_in() {
    let dir = tempfile::tempdir().unwrap();
    // Ten seconds old with a one-second lifetime: nine seconds stale, well
    // inside a ten-minute window.
    let url = origin_that_fails_after_one("max-age=1, stale-if-error=600", 10).await;
    let net = net(dir.path());

    assert_eq!(fetched(&net, &url).await.unwrap(), Source::Origin);
    assert_eq!(fetched(&net, &url).await.unwrap(), Source::StaleOnError);
}

#[tokio::test]
async fn past_the_window_the_origin_error_is_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    // A thousand seconds old: long past a one-minute window.
    let url = origin_that_fails_after_one("max-age=1, stale-if-error=60", 1000).await;
    let net = net(dir.path());

    assert_eq!(fetched(&net, &url).await.unwrap(), Source::Origin);
    assert!(
        fetched(&net, &url).await.is_err(),
        "a body past its stale-if-error window was served"
    );
}
