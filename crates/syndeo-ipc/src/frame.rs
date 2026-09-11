//! Length-prefixed JSON frames.
//!
//! Deliberately boring: a four-byte big-endian length and a JSON body, with a
//! hard ceiling so a peer cannot make us allocate on demand.

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest single message we will read.
///
/// This is a bound on one frame, not on a response: a fetch is answered as a
/// `FetchBegin`, a run of `FetchChunk`s and a `FetchEnd`, so a resource larger
/// than this still arrives. It exists so a peer cannot make us allocate on
/// demand, which is a different question from how large the web is allowed to be.
pub const MAX_FRAME: u32 = 16 * 1024 * 1024;

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

    pub async fn send<T: Serialize>(&mut self, message: &T) -> Result<(), FrameError> {
        let body = serde_json::to_vec(message)?;
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
        let mut body = vec![0u8; length as usize];
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
