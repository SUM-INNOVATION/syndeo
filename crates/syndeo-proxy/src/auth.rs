//! The credential a proxy started by syndeo-webkit requires of every client.
//!
//! syndeo-webkit generates a token for each launch and writes it once, as one
//! frame, down the stdin pipe it already holds for `--exit-with-parent`. The
//! web view is configured with the matching username and password, and
//! WebKit answers a 407 with them. So a process on this machine that finds
//! the proxy's port can neither fetch through it nor read its statistics.
//!
//! The token appears in no argument, no environment variable, no log line, no
//! error message and no `Debug` output, and it is compared in constant time.

use base64::Engine;
use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use hyper::body::Incoming;
use hyper::{Request, Response};

/// The frame on stdin: this magic, the 32-byte token, and a newline.
const MAGIC: &[u8; 4] = b"SYA1";
const FRAME: usize = 4 + 32 + 1;

/// The username WebKit is configured with. Not a secret; the token is.
pub const USER: &str = "syndeo";

pub struct ProxyAuth {
    /// The one `Proxy-Authorization` value accepted, in full.
    expected: Vec<u8>,
}

impl std::fmt::Debug for ProxyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProxyAuth(redacted)")
    }
}

impl Drop for ProxyAuth {
    fn drop(&mut self) {
        self.expected.iter_mut().for_each(|b| *b = 0);
    }
}

/// What was wrong with a frame. Never what was in it.
#[derive(Debug, PartialEq, Eq)]
pub enum FrameRefusal {
    /// Fewer bytes than a frame before stdin ended or failed.
    Short,
    BadMagic,
    NoNewline,
}

impl std::fmt::Display for FrameRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FrameRefusal::Short => "the credential frame on stdin ended early",
            FrameRefusal::BadMagic => "stdin did not start with a credential frame",
            FrameRefusal::NoNewline => "the credential frame on stdin was not terminated",
        })
    }
}

impl ProxyAuth {
    /// Read exactly one frame, and nothing past it.
    pub fn read_frame(from: &mut impl std::io::Read) -> Result<Self, FrameRefusal> {
        let mut frame = [0u8; FRAME];
        let read = from.read_exact(&mut frame);
        let outcome = match read {
            Err(_) => Err(FrameRefusal::Short),
            Ok(()) if &frame[..4] != MAGIC => Err(FrameRefusal::BadMagic),
            Ok(()) if frame[FRAME - 1] != b'\n' => Err(FrameRefusal::NoNewline),
            Ok(()) => Ok(Self::for_token(&frame[4..FRAME - 1])),
        };
        frame.iter_mut().for_each(|b| *b = 0);
        outcome
    }

    fn for_token(token: &[u8]) -> Self {
        let password: String = token.iter().map(|b| format!("{b:02x}")).collect();
        let credential = format!("{USER}:{password}");
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(credential.as_bytes())
        );
        ProxyAuth {
            expected: expected.into_bytes(),
        }
    }

    /// Exactly one `Proxy-Authorization`, and exactly the expected one.
    /// Missing, repeated, malformed, another scheme, another user, or a token
    /// from another launch: all the same refusal.
    pub fn admits(&self, headers: &HeaderMap) -> bool {
        let mut presented = headers.get_all(http::header::PROXY_AUTHORIZATION).iter();
        let (Some(value), None) = (presented.next(), presented.next()) else {
            return false;
        };
        constant_time_eq(value.as_bytes(), &self.expected)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// The answer to a request without the credential: 407, asking for Basic.
///
/// The connection stays open, so a client can answer on the same socket, as
/// WebKit does. The exception is a request that carries a body: its body has
/// not been read, and rather than read an unauthenticated client's upload to
/// keep the connection usable, the connection is closed after the answer.
pub fn required(request: &Request<Incoming>) -> Response<super::Body> {
    let mut response = Response::builder()
        .status(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
        .header(http::header::PROXY_AUTHENTICATE, "Basic realm=\"syndeo\"")
        .header(http::header::CONTENT_LENGTH, "0");
    if carries_body(request.headers()) {
        response = response.header(http::header::CONNECTION, "close");
    }
    response
        .body(super::whole(Bytes::new()))
        .expect("static response")
}

fn carries_body(headers: &HeaderMap) -> bool {
    headers.contains_key(http::header::TRANSFER_ENCODING)
        || headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim() != "0")
            .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn frame_for(token: &[u8; 32]) -> Vec<u8> {
    let mut frame = MAGIC.to_vec();
    frame.extend_from_slice(token);
    frame.push(b'\n');
    frame
}

#[cfg(test)]
pub(crate) fn header_for(token: &[u8; 32]) -> String {
    String::from_utf8(ProxyAuth::for_token(token).expected.clone()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: [u8; 32] = [7u8; 32];

    fn with(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(http::header::PROXY_AUTHORIZATION, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn exactly_one_frame_is_read_and_nothing_past_it() {
        let mut input = frame_for(&TOKEN);
        input.extend_from_slice(b"what follows is the parent watch's");
        let mut reader = std::io::Cursor::new(input);
        let auth = ProxyAuth::read_frame(&mut reader).unwrap();
        assert_eq!(reader.position() as usize, FRAME);
        assert!(auth.admits(&with(&[&header_for(&TOKEN)])));
    }

    #[test]
    fn a_frame_that_is_short_unmarked_or_unterminated_is_refused() {
        let good = frame_for(&TOKEN);
        let short = &good[..FRAME - 1];
        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        let mut no_newline = good.clone();
        no_newline[FRAME - 1] = b' ';
        for (input, refusal) in [
            (&b""[..], FrameRefusal::Short),
            (short, FrameRefusal::Short),
            (&bad_magic[..], FrameRefusal::BadMagic),
            (&no_newline[..], FrameRefusal::NoNewline),
        ] {
            assert_eq!(
                ProxyAuth::read_frame(&mut std::io::Cursor::new(input)).unwrap_err(),
                refusal
            );
        }
    }

    #[test]
    fn anything_but_the_one_exact_credential_is_refused() {
        let auth = ProxyAuth::for_token(&TOKEN);
        let right = header_for(&TOKEN);
        let stale = header_for(&[8u8; 32]);
        let other_user = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("admin:{}", "07".repeat(32)))
        );
        assert!(auth.admits(&with(&[&right])));
        for refused in [
            vec![],
            vec![right.clone(), right.clone()],
            vec![stale.clone()],
            vec![other_user.clone()],
            vec![right.to_lowercase()],
            vec![right.replace("Basic ", "Bearer ")],
            vec![format!("{right} ")],
            vec!["Basic".to_string()],
            vec!["Basic !!!not base64!!!".to_string()],
        ] {
            let refs: Vec<&str> = refused.iter().map(String::as_str).collect();
            assert!(!auth.admits(&with(&refs)), "{refused:?}");
        }
    }

    #[test]
    fn nothing_prints_the_credential() {
        let auth = ProxyAuth::for_token(&TOKEN);
        let debug = format!("{auth:?}");
        assert_eq!(debug, "ProxyAuth(redacted)");
        for refusal in [
            FrameRefusal::Short,
            FrameRefusal::BadMagic,
            FrameRefusal::NoNewline,
        ] {
            let said = format!("{refusal} {refusal:?}");
            assert!(!said.contains("07070707"), "{said}");
        }
    }
}
