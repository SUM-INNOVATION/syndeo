//! The network process's request loop.
//!
//! This is the only process in the tree with a socket to the outside world.
//! Renderers and the agent reach it through [`syndeo_ipc::NetRequest`] and get
//! bytes back; they never learn a host, an address, or a certificate.

use crate::fetch::{FetchRequest, Net};
use std::sync::Arc;
use syndeo_cache::Integrity;
use syndeo_ipc::protocol::{NetRequest, NetResponse};
use syndeo_ipc::transport::Server;

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
                let response = handle(&net, request).await;
                if framed.send(&response).await.is_err() {
                    return;
                }
            }
        });
    }
}

pub async fn handle(net: &Net, request: NetRequest) -> NetResponse {
    match request {
        NetRequest::Fetch {
            method,
            url,
            headers,
            body,
            integrity,
        } => {
            let Ok(method) = method.parse::<http::Method>() else {
                return NetResponse::Error(format!("unknown method {method}"));
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
                Some(Err(err)) => return NetResponse::Error(format!("integrity: {err}")),
                _ => None,
            };

            match net
                .fetch(FetchRequest {
                    method,
                    url: url.clone(),
                    headers: map,
                    body: bytes::Bytes::from(body),
                })
                .await
            {
                Ok(response) => {
                    // When the caller declared what the bytes must hash to, the
                    // bytes have to hash to it, whatever produced them.
                    if let Some(integrity) = declared {
                        if let Err(err) = integrity.check(&response.body) {
                            tracing::warn!(%url, %err, "integrity check failed");
                            return NetResponse::Error(format!("integrity: {err}"));
                        }
                    }
                    NetResponse::Fetched {
                        status: response.status,
                        headers: response
                            .headers
                            .iter()
                            .filter_map(|(n, v)| {
                                v.to_str().ok().map(|v| (n.as_str().to_string(), v.to_string()))
                            })
                            .collect(),
                        body: response.body.to_vec(),
                        source: response.source.as_str().to_string(),
                        elapsed_ms: response.elapsed_ms,
                        content: response.content.map(|c| c.to_hex()),
                    }
                }
                Err(err) => NetResponse::Error(err.to_string()),
            }
        }

        NetRequest::Stats => match net.cache().stats() {
            Ok(stats) => NetResponse::Stats(serde_json::to_value(stats).unwrap_or_default()),
            Err(err) => NetResponse::Error(err.to_string()),
        },

        NetRequest::Ping => NetResponse::Pong,
    }
}
