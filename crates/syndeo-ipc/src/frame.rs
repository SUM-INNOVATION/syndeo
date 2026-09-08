//! Length-prefixed JSON frames.
//!
//! Deliberately boring: a four-byte big-endian length and a JSON body, with a
//! hard ceiling so a peer cannot make us allocate on demand.

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest single message we will read. Bodies travel as blobs elsewhere; this
/// carries control messages only.
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
        self.stream.write_all(&(body.len() as u32).to_be_bytes()).await?;
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
