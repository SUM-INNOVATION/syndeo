//! Length-prefixed JSON frames.
//!
//! Deliberately boring: a four-byte big-endian length and a JSON body, with a
//! hard ceiling so a peer cannot make us allocate on demand.

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

/// Largest single message we will read.
///
/// This is a bound on one frame, not on a response: a fetch is answered as a
/// `FetchBegin`, a run of `FetchChunk`s and a `FetchEnd`, so a resource larger
/// than this still arrives. It exists so a peer cannot make us allocate on
/// demand, which is a different question from how large the web is allowed to be.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// The largest body [`Framed::fetch`] collects into memory.
///
/// A whole-body fetch — the shell printing a page, the agent reading one, the
/// window, Servo's loads — holds every byte of the response at once, and the
/// network process streams an ordinary response on as it arrives, chunked or
/// not, with no length given. Without a ceiling here an origin could send
/// until this process ran out of memory. A caller that streams, reading the frames
/// itself, is not bound by it; nor is the proxy, which never collects a body
/// this way.
///
/// It is separate from the network process's `max_body_bytes`, which bounds
/// what that process buffers — a response with declared integrity, one to an
/// unsafe method, a revalidation answered in full, a background refresh —
/// and only stops an ordinary response being cached; and from the parser's
/// work budget, which bounds what parsing a body that did arrive may cost.
pub const MAX_WHOLE_BODY: u64 = 64 * 1024 * 1024;

/// What a frame is encoded into before it first has to grow.
const INITIAL_FRAME: usize = 4 * 1024;

/// What a frame is encoded into. It never lets go of memory it has written
/// to without wiping it first: when it has to grow, it copies into a larger
/// allocation and wipes the old one, all of it, before that is freed. A
/// `Vec` left to grow by itself reallocates, and may free the old block with
/// the message still in it, whatever size the message is.
struct WipingBuffer(Zeroizing<Vec<u8>>);

impl WipingBuffer {
    fn new() -> Self {
        WipingBuffer(Zeroizing::new(Vec::with_capacity(INITIAL_FRAME)))
    }
}

impl std::io::Write for WipingBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let needed = self.0.len() + bytes.len();
        if needed > self.0.capacity() {
            let mut larger = Vec::with_capacity(needed.max(self.0.capacity() * 2));
            larger.extend_from_slice(&self.0);
            // The old allocation is wiped, spare capacity included, as it is
            // dropped here.
            self.0 = Zeroizing::new(larger);
        }
        // Within capacity, so this never reallocates.
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {0} bytes exceeds the {MAX_FRAME} byte ceiling")]
    TooLarge(u32),
    #[error("malformed message: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("the peer closed the connection")]
    Closed,
    #[error("{0}")]
    Refused(String),
    #[error("unexpected reply: {0}")]
    Unexpected(String),
    #[error("the response body is larger than the {0} bytes a whole-body fetch accepts")]
    BodyTooLarge(u64),
}

/// A message-oriented wrapper over a byte stream.
pub struct Framed<S> {
    stream: S,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Framed<S> {
    pub fn new(stream: S) -> Self {
        Framed { stream }
    }

    /// Every frame's bytes are wiped once they are written or read, and so is
    /// every allocation the encoding outgrew on the way (see
    /// `WipingBuffer`): a keystore request can carry a passphrase of any
    /// length, and its JSON holds it as plainly as the message did.
    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), FrameError> {
        let mut buffer = WipingBuffer::new();
        serde_json::to_writer(&mut buffer, message)?;
        let body = buffer.0;
        if body.len() as u64 > MAX_FRAME as u64 {
            return Err(FrameError::TooLarge(body.len() as u32));
        }
        self.stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .await?;
        self.stream.write_all(&body).await?;
        self.stream.flush().await?;
        Ok(())
    }

    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T, FrameError> {
        let mut length = [0u8; 4];
        match self.stream.read_exact(&mut length).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(FrameError::Closed)
            }
            Err(e) => return Err(FrameError::Io(e)),
        }
        let length = u32::from_be_bytes(length);
        if length > MAX_FRAME {
            return Err(FrameError::TooLarge(length));
        }
        let mut body = Zeroizing::new(vec![0u8; length as usize]);
        self.stream.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// Send a fetch and reassemble the reply from its frames, up to
    /// [`MAX_WHOLE_BODY`] bytes.
    ///
    /// For a caller that was going to hold the whole body anyway — the agent
    /// summarising a page, the shell printing one. A caller that wants the first
    /// bytes early reads the frames itself.
    ///
    /// Takes the connection, and closes it however this returns: a body
    /// refused part way is still arriving, and closing is what tells the
    /// network process to stop sending it and let go of the origin.
    pub async fn fetch(
        self,
        request: &crate::protocol::NetRequest,
    ) -> Result<crate::protocol::Fetched, FrameError> {
        self.fetch_within(request, MAX_WHOLE_BODY).await
    }

    /// [`Framed::fetch`], with a ceiling of `limit` bytes. A body of exactly
    /// `limit` bytes is accepted; one byte more is [`FrameError::BodyTooLarge`],
    /// refused as soon as it is known — from a declared `Content-Length`
    /// before any of the body is read, or else from the chunk that crosses it.
    pub async fn fetch_within(
        mut self,
        request: &crate::protocol::NetRequest,
        limit: u64,
    ) -> Result<crate::protocol::Fetched, FrameError> {
        use crate::protocol::{NetRequest, NetResponse};

        self.send(request).await?;
        let (status, headers, source, protocol, elapsed_ms) =
            match self.recv::<NetResponse>().await? {
                NetResponse::FetchBegin {
                    status,
                    headers,
                    source,
                    protocol,
                    elapsed_ms,
                } => (status, headers, source, protocol, elapsed_ms),
                NetResponse::Error(e) => return Err(FrameError::Refused(e)),
                other => return Err(FrameError::Unexpected(format!("{other:?}"))),
            };

        // A HEAD response's length describes a body that is not coming.
        let has_body = !matches!(request, NetRequest::Fetch { method, .. } if method.eq_ignore_ascii_case("HEAD"));
        let declared = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<u64>().ok());
        if has_body && declared.is_some_and(|length| length > limit) {
            return Err(FrameError::BodyTooLarge(limit));
        }

        let mut body = Vec::new();
        loop {
            match self.recv::<NetResponse>().await? {
                NetResponse::FetchChunk { bytes } => {
                    if body.len() as u64 + bytes.len() as u64 > limit {
                        return Err(FrameError::BodyTooLarge(limit));
                    }
                    body.extend_from_slice(&bytes);
                }
                NetResponse::FetchEnd { content } => {
                    return Ok(crate::protocol::Fetched {
                        status,
                        headers,
                        body,
                        source,
                        protocol,
                        elapsed_ms,
                        content,
                    })
                }
                // A body cut short mid-transfer. Returning what arrived would be
                // handing the caller a truncated page as if it were the page.
                NetResponse::Error(e) => return Err(FrameError::Refused(e)),
                other => return Err(FrameError::Unexpected(format!("{other:?}"))),
            }
        }
    }

    /// One round trip: send a request, read the reply.
    pub async fn call<Req: Serialize, Res: DeserializeOwned>(
        &mut self,
        request: &Req,
    ) -> Result<Res, FrameError> {
        self.send(request).await?;
        self.recv().await
    }

    pub fn into_inner(self) -> S {
        self.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{NetRequest, NetResponse};

    fn get(method: &str) -> NetRequest {
        NetRequest::Fetch {
            method: method.into(),
            url: "https://a.test/big".into(),
            headers: Vec::new(),
            body: Vec::new(),
            integrity: None,
            partition: None,
        }
    }

    fn begin(headers: Vec<(String, String)>) -> NetResponse {
        NetResponse::FetchBegin {
            status: 200,
            headers,
            source: "origin".into(),
            protocol: "http/1.1".into(),
            elapsed_ms: 1,
        }
    }

    /// A network process that answers any fetch with `begin`, then sends
    /// `chunks` pieces of `size` bytes, then an end,
    /// stopping at the first send that fails. Returns how many pieces it sent
    /// and whether a send failed, which is how it learns the caller has gone.
    fn serve(
        theirs: tokio::io::DuplexStream,
        headers: Vec<(String, String)>,
        size: usize,
        chunks: usize,
    ) -> tokio::task::JoinHandle<(usize, bool)> {
        tokio::spawn(async move {
            let mut server = Framed::new(theirs);
            let _: NetRequest = server.recv().await.unwrap();
            if server.send(&begin(headers)).await.is_err() {
                return (0, true);
            }
            let mut sent = 0;
            while sent < chunks {
                let piece = NetResponse::FetchChunk {
                    bytes: vec![b'x'; size],
                };
                if server.send(&piece).await.is_err() {
                    return (sent, true);
                }
                sent += 1;
            }
            let end = NetResponse::FetchEnd { content: None };
            (sent, server.send(&end).await.is_err())
        })
    }

    #[tokio::test]
    async fn a_body_of_exactly_the_limit_is_collected() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let server = serve(theirs, Vec::new(), 1000, 10);
        let fetched = Framed::new(ours)
            .fetch_within(&get("GET"), 10_000)
            .await
            .unwrap();
        assert_eq!(fetched.body.len(), 10_000);
        assert_eq!(server.await.unwrap(), (10, false));
    }

    /// Chunked, with no length given: refused at the chunk that crosses the
    /// limit, and the connection is closed, so the sender's next write fails
    /// and it stops, rather than sending into a buffer nobody reads.
    #[tokio::test]
    async fn one_byte_over_the_limit_is_refused_and_the_connection_closed() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        // Ten thousand pieces, so a fetch with no ceiling would take them all
        // and succeed rather than run on.
        let server = serve(theirs, Vec::new(), 1000, 10_000);
        let refused = Framed::new(ours).fetch_within(&get("GET"), 9_999).await;
        assert!(
            matches!(refused, Err(FrameError::BodyTooLarge(9_999))),
            "{refused:?}"
        );
        let (sent, stopped) = server.await.unwrap();
        assert!(stopped, "the sender was never told");
        // What it managed to send is bounded by what the pipe could hold
        // after the refusal, not by how long it ran.
        assert!(sent < 10 + 64, "{sent} pieces sent");
    }

    /// A declared length past the limit is refused before any of the body is
    /// read.
    #[tokio::test]
    async fn a_declared_length_over_the_limit_is_refused_before_the_body() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let length = vec![("Content-Length".to_string(), "10001".to_string())];
        // The sender offers nothing after the head, so a fetch that waited
        // for the body would never return.
        let server = serve(theirs, length, 1000, 0);
        let refused = Framed::new(ours).fetch_within(&get("GET"), 10_000).await;
        assert!(
            matches!(refused, Err(FrameError::BodyTooLarge(10_000))),
            "{refused:?}"
        );
        server.await.unwrap();

        // At the limit exactly, the declaration is no reason to refuse.
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let length = vec![("content-length".to_string(), "10000".to_string())];
        let server = serve(theirs, length, 1000, 10);
        let fetched = Framed::new(ours)
            .fetch_within(&get("GET"), 10_000)
            .await
            .unwrap();
        assert_eq!(fetched.body.len(), 10_000);
        server.await.unwrap();
    }

    /// A HEAD response declares the length of a body that is not sent.
    #[tokio::test]
    async fn a_head_response_is_not_refused_for_the_length_it_declares() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let length = vec![("content-length".to_string(), "999999999".to_string())];
        let server = serve(theirs, length, 1, 0);
        let fetched = Framed::new(ours)
            .fetch_within(&get("HEAD"), 10)
            .await
            .unwrap();
        assert!(fetched.body.is_empty());
        server.await.unwrap();
    }
}
