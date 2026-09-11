//! The network process's request loop.
//!
//! This is the only process in the tree with a socket to the outside world.
//! Renderers and the agent reach it through [`syndeo_ipc::NetRequest`] and get
//! bytes back; they never learn a host, an address, or a certificate.

use crate::fetch::{FetchRequest, Net};
use futures::StreamExt;
use std::sync::Arc;
use syndeo_cache::Integrity;
use syndeo_ipc::protocol::{NetRequest, NetResponse};
use syndeo_ipc::transport::Server;
use syndeo_ipc::{FrameError, Framed};

pub async fn serve(net: Arc<Net>, server: Server) {
    tracing::info!(socket = %server.endpoint().path().display(), "net listening");
    loop {
        let mut framed = match server.accept().await {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!(%err, "accept failed");
                continue;
            }
        };
        let net = net.clone();
        tokio::spawn(async move {
            loop {
                let request: NetRequest = match framed.recv().await {
                    Ok(r) => r,
                    Err(syndeo_ipc::FrameError::Closed) => return,
                    Err(err) => {
                        tracing::debug!(%err, "malformed request");
                        return;
                    }
                };
                if respond(&net, request, &mut framed).await.is_err() {
                    return;
                }
            }
        });
    }
}

/// Serve one request.
///
/// A fetch is answered as a sequence of frames rather than as one message,
/// because the alternative is a hard ceiling on how large a resource the browser
/// can load — a ceiling imposed by our own transport rather than by the web.
/// Everything else is a single reply.
pub async fn respond<S>(
    net: &Net,
    request: NetRequest,
    framed: &mut Framed<S>,
) -> Result<(), FrameError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match request {
        NetRequest::Fetch {
            method,
            url,
            headers,
            body,
            integrity,
            partition,
        } => {
            fetch(
                net, method, url, headers, body, integrity, partition, framed,
            )
            .await
        }
        other => {
            let response = handle(net, other).await;
            framed.send(&response).await
        }
    }
}

/// Largest body chunk we put in one frame. Well under the frame ceiling, so a
/// large read from the origin is split rather than refused.
const CHUNK: usize = 512 * 1024;

#[allow(clippy::too_many_arguments)]
async fn fetch<S>(
    net: &Net,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    integrity: Option<String>,
    partition: Option<String>,
    framed: &mut Framed<S>,
) -> Result<(), FrameError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Ok(parsed) = method.parse::<http::Method>() else {
        return framed
            .send(&NetResponse::Error(format!("unknown method {method}")))
            .await;
    };
    let mut map = http::HeaderMap::new();
    for (name, value) in &headers {
        if let (Ok(n), Ok(v)) = (
            http::HeaderName::from_bytes(name.as_bytes()),
            http::HeaderValue::from_str(value),
        ) {
            map.append(n, v);
        }
    }

    let declared = match integrity.as_deref().map(Integrity::parse) {
        Some(Ok(i)) if !i.is_empty() => Some(i),
        Some(Err(err)) => {
            return framed
                .send(&NetResponse::Error(format!("integrity: {err}")))
                .await
        }
        _ => None,
    };

    // Timed so that "the browser feels slow" can be attributed rather than
    // argued about: this is the network process's own share, with the IPC round
    // trip and the renderer's work outside it.
    let started = std::time::Instant::now();
    let response = match net
        .fetch(FetchRequest {
            method: parsed,
            url: url.clone(),
            headers: map,
            body: bytes::Bytes::from(body),
            integrity: declared.clone(),
            // Dropped here rather than at the caller, so that turning
            // partitioning off is one decision in one place and cannot be
            // half-applied.
            partition: partition.filter(|_| net.config().partition_cache),
        })
        .await
    {
        Ok(response) => response,
        Err(err) => return framed.send(&NetResponse::Error(err.to_string())).await,
    };
    tracing::debug!(
        %url,
        source = ?response.source,
        served_ms = started.elapsed().as_millis(),
        "served"
    );

    // A caller that declared what the bytes must hash to gets bytes that hash to
    // it, whatever produced them. `Net::fetch` buffers such a response for
    // exactly this reason, so the check happens before anything is sent on.
    if let Some(integrity) = &declared {
        let whole = match response.body.collect().await {
            Ok(bytes) => bytes,
            Err(err) => return framed.send(&NetResponse::Error(err.to_string())).await,
        };
        if let Err(err) = integrity.check(&whole) {
            tracing::warn!(%url, %err, "integrity check failed");
            return framed
                .send(&NetResponse::Error(format!("integrity: {err}")))
                .await;
        }
        framed
            .send(&NetResponse::FetchBegin {
                status: response.status,
                headers: header_pairs(&response.headers),
                source: response.source.as_str().to_string(),
                protocol: response.protocol.as_str().to_string(),
                elapsed_ms: response.elapsed_ms,
            })
            .await?;
        for chunk in whole.chunks(CHUNK) {
            framed
                .send(&NetResponse::FetchChunk {
                    bytes: chunk.to_vec(),
                })
                .await?;
        }
        return framed
            .send(&NetResponse::FetchEnd {
                content: response.content.map(|c| c.to_hex()),
            })
            .await;
    }

    framed
        .send(&NetResponse::FetchBegin {
            status: response.status,
            headers: header_pairs(&response.headers),
            source: response.source.as_str().to_string(),
            protocol: response.protocol.as_str().to_string(),
            elapsed_ms: response.elapsed_ms,
        })
        .await?;

    let content = response.content;
    let mut stream = response.body.into_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                for piece in bytes.chunks(CHUNK) {
                    framed
                        .send(&NetResponse::FetchChunk {
                            bytes: piece.to_vec(),
                        })
                        .await?;
                }
            }
            // The body was cut short. `Error` after `FetchBegin` is how the
            // caller learns that what it has is not the whole thing.
            Err(err) => return framed.send(&NetResponse::Error(err.to_string())).await,
        }
    }

    framed
        .send(&NetResponse::FetchEnd {
            content: content.map(|c| c.to_hex()),
        })
        .await
}

fn header_pairs(headers: &http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(n, v)| {
            v.to_str()
                .ok()
                .map(|v| (n.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Everything that is not a fetch, which is everything that fits in one reply.
///
/// Public because it is the whole surface for anything embedding the network
/// process in-tree rather than across a socket.
pub async fn handle(net: &Net, request: NetRequest) -> NetResponse {
    match request {
        NetRequest::Fetch { .. } => {
            NetResponse::Error("a fetch is answered in frames; use `respond`".into())
        }

        NetRequest::Stats => match net.cache().stats() {
            Ok(stats) => NetResponse::Stats(serde_json::to_value(stats).unwrap_or_default()),
            Err(err) => NetResponse::Error(err.to_string()),
        },

        NetRequest::PeerStatus => match net.peer_status().await {
            Some(status) => NetResponse::PeerStatus(serde_json::json!({
                "peer_id": status.peer_id.to_string(),
                "serving": status.serving,
                "listeners": status.listeners.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
                "routing_table": status.routing_table,
                "announced": status.announced,
                "connected": status.connected.iter().map(|report| serde_json::json!({
                    "peer": report.peer.to_string(),
                    "served": report.ledger.served,
                    "bytes_served": report.ledger.bytes_served,
                    "received": report.ledger.received,
                    "bytes_received": report.ledger.bytes_received,
                    "debt": report.ledger.debt,
                    "standing": report.ledger.standing(),
                })).collect::<Vec<_>>(),
            })),
            None => NetResponse::Error("this node is not in the peer swarm".into()),
        },

        NetRequest::Ping => NetResponse::Pong,
    }
}
