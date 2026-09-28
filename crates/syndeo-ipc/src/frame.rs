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
    /// [`WipingBuffer`]): a keystore request can carry a passphrase of any
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

    /// Send a fetch and reassemble the reply from its frames.
    ///
    /// For a caller that was going to hold the whole body anyway — the agent
    /// summarising a page, the shell printing one. A caller that wants the first
    /// bytes early reads the frames itself.
    pub async fn fetch(
        &mut self,
        request: &crate::protocol::NetRequest,
    ) -> Result<crate::protocol::Fetched, FrameError> {
        use crate::protocol::NetResponse;

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

        let mut body = Vec::new();
        loop {
            match self.recv::<NetResponse>().await? {
                NetResponse::FetchChunk { bytes } => body.extend_from_slice(&bytes),
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
