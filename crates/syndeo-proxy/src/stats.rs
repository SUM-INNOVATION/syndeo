//! A statistics surface reachable through the proxy itself, so the numbers can
//! be read from the browser that is generating them.

use bytes::Bytes;
use http::StatusCode;
use http_body_util::{BodyExt, Full};
use hyper::Response;
use syndeo_net::Net;

/// The proxy's body type. These replies are short and whole, but they share the
/// shape of the streamed ones so one function can return either.
pub type Body = http_body_util::combinators::UnsyncBoxBody<Bytes, std::io::Error>;

const HOSTS: &[&str] = &["http://syndeo.local/", "https://syndeo.local/"];

/// Answer `syndeo.local` locally instead of sending it to a name server.
pub fn intercept(net: &Net, url: &str) -> Option<Response<Body>> {
    let path = HOSTS.iter().find_map(|prefix| url.strip_prefix(prefix))?;
    let (path, _) = path.split_once('?').unwrap_or((path, ""));

    let stats = match net.cache().stats() {
        Ok(s) => s,
        Err(err) => {
            return Some(reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                "text/plain",
                format!("{err}"),
            ))
        }
    };

    match path {
        "stats.json" => Some(reply(
            StatusCode::OK,
            "application/json",
            serde_json::to_string_pretty(&stats).unwrap_or_default(),
        )),
        "stats" | "" => Some(reply(
            StatusCode::OK,
            "text/plain; charset=utf-8",
            format!("{}\n", stats.render()),
        )),
        _ => Some(reply(
            StatusCode::NOT_FOUND,
            "text/plain",
            "no such endpoint\n".into(),
        )),
    }
}

fn reply(status: StatusCode, content_type: &str, body: String) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CACHE_CONTROL, "no-store")
        .body(
            Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .expect("static response")
}
