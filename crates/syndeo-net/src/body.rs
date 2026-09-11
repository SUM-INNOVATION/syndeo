//! Response bodies, which are not always in memory.
//!
//! The cache stores bodies under a hash of all of them, which reads like an
//! argument for buffering: you cannot name the bytes until you have them all.
//! You can, though, hash them as they pass and name them at the end. That is
//! what [`FetchBody::Stream`] does — bytes go to the caller and to a
//! [`BlobWriter`] at the same time, and the entry is written when the last one
//! arrives.
//!
//! [`BlobWriter`]: syndeo_cache::blob::BlobWriter

use crate::error::{NetError, Result};
use bytes::Bytes;
use futures::stream::{BoxStream, StreamExt};

/// Bytes, either already here or on their way.
pub enum FetchBody {
    /// Already in memory: served from the cache, from a peer, or buffered
    /// because the caller declared what the bytes must hash to.
    Bytes(Bytes),
    /// Arriving. Nothing has read them yet, including us.
    Stream(BoxStream<'static, Result<Bytes>>),
}

impl std::fmt::Debug for FetchBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchBody::Bytes(b) => write!(f, "FetchBody::Bytes({} bytes)", b.len()),
            FetchBody::Stream(_) => f.write_str("FetchBody::Stream"),
        }
    }
}

impl FetchBody {
    pub fn empty() -> Self {
        FetchBody::Bytes(Bytes::new())
    }

    pub fn is_stream(&self) -> bool {
        matches!(self, FetchBody::Stream(_))
    }

    /// Length, when it is known without waiting.
    pub fn known_len(&self) -> Option<usize> {
        match self {
            FetchBody::Bytes(b) => Some(b.len()),
            FetchBody::Stream(_) => None,
        }
    }

    /// Wait for all of it. Undoes the streaming, and is the right thing for a
    /// caller that was going to hold the whole body anyway.
    pub async fn collect(self) -> Result<Bytes> {
        match self {
            FetchBody::Bytes(bytes) => Ok(bytes),
            FetchBody::Stream(mut stream) => {
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk?);
                }
                Ok(Bytes::from(out))
            }
        }
    }

    /// Consume it as a stream, whichever it is.
    pub fn into_stream(self) -> BoxStream<'static, Result<Bytes>> {
        match self {
            FetchBody::Bytes(bytes) => {
                if bytes.is_empty() {
                    futures::stream::empty().boxed()
                } else {
                    futures::stream::once(async move { Ok(bytes) }).boxed()
                }
            }
            FetchBody::Stream(stream) => stream,
        }
    }
}

impl From<Bytes> for FetchBody {
    fn from(bytes: Bytes) -> Self {
        FetchBody::Bytes(bytes)
    }
}

impl From<Vec<u8>> for FetchBody {
    fn from(bytes: Vec<u8>) -> Self {
        FetchBody::Bytes(Bytes::from(bytes))
    }
}

/// What a streamed body should be written into as it passes.
pub struct Tee {
    writer: Option<syndeo_cache::blob::BlobWriter>,
    /// Set once the body has outgrown what we are willing to cache. The transfer
    /// continues; only the caching stops.
    over_budget: bool,
    limit: u64,
}

impl Tee {
    pub fn new(writer: syndeo_cache::blob::BlobWriter, limit: u64) -> Self {
        Tee {
            writer: Some(writer),
            over_budget: false,
            limit,
        }
    }

    /// Pass a chunk through. Never fails the transfer: a cache that cannot keep
    /// up is a cache miss, not a broken download.
    pub fn observe(&mut self, chunk: &Bytes) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        if writer.len() + chunk.len() as u64 > self.limit {
            tracing::debug!(
                limit = self.limit,
                "body outgrew the cache budget; not storing it"
            );
            self.over_budget = true;
            if let Some(writer) = self.writer.take() {
                writer.abandon();
            }
            return;
        }
        if let Err(err) = writer.write(chunk) {
            tracing::warn!(%err, "could not write a streamed body to the cache");
            self.writer.take();
        }
    }

    /// The writer, if the body is still worth storing.
    pub fn take(&mut self) -> Option<syndeo_cache::blob::BlobWriter> {
        self.writer.take()
    }

    pub fn discard(&mut self) {
        if let Some(writer) = self.writer.take() {
            writer.abandon();
        }
    }
}

/// A body whose bytes never arrived in full.
pub fn truncated(reason: impl std::fmt::Display) -> NetError {
    NetError::Transport(format!("the body was cut short: {reason}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_buffered_body_and_a_streamed_one_collect_the_same() {
        let body = Bytes::from_static(b"one two three");
        assert_eq!(
            FetchBody::Bytes(body.clone()).collect().await.unwrap(),
            body
        );

        let chunks: Vec<Result<Bytes>> = vec![
            Ok(Bytes::from_static(b"one ")),
            Ok(Bytes::from_static(b"two ")),
            Ok(Bytes::from_static(b"three")),
        ];
        let streamed = FetchBody::Stream(futures::stream::iter(chunks).boxed());
        assert_eq!(streamed.collect().await.unwrap(), body);
    }

    #[tokio::test]
    async fn an_empty_body_yields_no_chunks() {
        let chunks: Vec<_> = FetchBody::empty().into_stream().collect::<Vec<_>>().await;
        assert!(chunks.is_empty());
    }

    #[tokio::test]
    async fn a_failing_stream_surfaces_the_failure_rather_than_a_short_body() {
        let chunks: Vec<Result<Bytes>> = vec![
            Ok(Bytes::from_static(b"partial")),
            Err(truncated("connection reset")),
        ];
        let streamed = FetchBody::Stream(futures::stream::iter(chunks).boxed());
        assert!(streamed.collect().await.is_err());
    }
}
