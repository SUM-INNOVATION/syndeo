//! Wire codec: a four-byte length and a bincode body, matching what SUM Chain's
//! own request-response codec does, so the two are recognisably the same shape.

use crate::protocol::{BlobRequest, BlobResponse, MAX_BODY, PROTOCOL};
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p_swarm::StreamProtocol;
use std::io;

#[derive(Debug, Clone, Default)]
pub struct BlobCodec;

/// A request names a hash, so it is small by construction. Anything larger is
/// not a request we sent.
const MAX_REQUEST: usize = 1024;

#[async_trait::async_trait]
impl libp2p_request_response::Codec for BlobCodec {
    type Protocol = StreamProtocol;
    type Request = BlobRequest;
    type Response = BlobResponse;

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> io::Result<BlobRequest>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(io, MAX_REQUEST).await
    }

    async fn read_response<T>(&mut self, _: &StreamProtocol, io: &mut T) -> io::Result<BlobResponse>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(io, MAX_BODY + 1024).await
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        request: BlobRequest,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(io, &request).await
    }

    async fn write_response<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        response: BlobResponse,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(io, &response).await
    }
}

async fn read_frame<T, V>(io: &mut T, limit: usize) -> io::Result<V>
where
    T: AsyncRead + Unpin + Send,
    V: serde::de::DeserializeOwned,
{
    let mut length = [0u8; 4];
    io.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {length} bytes exceeds the {limit} byte ceiling"),
        ));
    }
    let mut body = vec![0u8; length];
    io.read_exact(&mut body).await?;
    bincode::deserialize(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

async fn write_frame<T, V>(io: &mut T, value: &V) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    V: serde::Serialize,
{
    let body = bincode::serialize(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    io.write_all(&(body.len() as u32).to_be_bytes()).await?;
    io.write_all(&body).await?;
    io.flush().await
}

pub fn protocol() -> StreamProtocol {
    StreamProtocol::new(PROTOCOL)
}
