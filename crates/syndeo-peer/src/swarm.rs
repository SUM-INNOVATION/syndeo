//! The swarm.
//!
//! TCP, Noise, Yamux, boxed — the same construction SUM Chain settled on, so a
//! reader of one recognises the other. Behaviour is identify plus one
//! request-response protocol.

use crate::codec::{protocol, BlobCodec};
use crate::protocol::{BlobRequest, BlobResponse, MAX_BODY};
use crate::PeerError;
use futures::StreamExt;
use libp2p_core::upgrade::Version;
use libp2p_core::{Multiaddr, Transport};
use libp2p_identity::Keypair;
use libp2p_request_response::{self as request_response, ProtocolSupport, ResponseChannel};
use libp2p_swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use syndeo_cache::{Cache, ContentId};
use tokio::sync::{mpsc, oneshot};

pub use libp2p_identity::PeerId;

// `prelude` points the derive at the libp2p-swarm sub-crate rather than the
// `::libp2p::swarm::derive_prelude` umbrella path, which is not in this tree.
// Same reason SUM Chain does it: the umbrella drags in libp2p-mdns.
#[derive(NetworkBehaviour)]
#[behaviour(prelude = "libp2p_swarm::derive_prelude")]
pub struct Behaviour {
    identify: libp2p_identify::Behaviour,
    blobs: request_response::Behaviour<BlobCodec>,
}

#[derive(Debug, Clone)]
pub struct PeerConfig {
    pub listen: Vec<Multiaddr>,
    pub bootstrap: Vec<Multiaddr>,
    pub request_timeout: Duration,
    /// Serve bodies to peers as well as asking for them.
    pub serve: bool,
}

impl Default for PeerConfig {
    fn default() -> Self {
        PeerConfig {
            listen: vec!["/ip4/0.0.0.0/tcp/0".parse().expect("a valid multiaddr")],
            bootstrap: Vec::new(),
            request_timeout: Duration::from_secs(10),
            serve: true,
        }
    }
}

enum Command {
    Fetch {
        request: BlobRequest,
        reply: oneshot::Sender<crate::Result<Vec<u8>>>,
    },
    Peers(oneshot::Sender<Vec<PeerId>>),
    Listeners(oneshot::Sender<Vec<Multiaddr>>),
    Dial(Multiaddr, oneshot::Sender<crate::Result<()>>),
}

/// A handle to a running swarm. Cloneable, and the only way to reach it.
#[derive(Clone)]
pub struct PeerHandle {
    commands: mpsc::Sender<Command>,
    peer_id: PeerId,
}

impl PeerHandle {
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Ask the swarm for a body. The bytes that come back have already been
    /// checked against the hash in the request; there is no unverified path out
    /// of this function.
    pub async fn fetch(&self, request: BlobRequest) -> crate::Result<Vec<u8>> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(Command::Fetch { request, reply })
            .await
            .map_err(|_| PeerError::Stopped)?;
        receiver.await.map_err(|_| PeerError::Stopped)?
    }

    pub async fn fetch_content(&self, id: ContentId) -> crate::Result<Vec<u8>> {
        self.fetch(BlobRequest::content(id)).await
    }

    pub async fn fetch_integrity(&self, hash: &syndeo_cache::sri::Hash) -> crate::Result<Vec<u8>> {
        self.fetch(BlobRequest::integrity(hash)).await
    }

    pub async fn peers(&self) -> crate::Result<Vec<PeerId>> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(Command::Peers(reply))
            .await
            .map_err(|_| PeerError::Stopped)?;
        receiver.await.map_err(|_| PeerError::Stopped)
    }

    pub async fn listeners(&self) -> crate::Result<Vec<Multiaddr>> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(Command::Listeners(reply))
            .await
            .map_err(|_| PeerError::Stopped)?;
        receiver.await.map_err(|_| PeerError::Stopped)
    }

    pub async fn dial(&self, address: Multiaddr) -> crate::Result<()> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(Command::Dial(address, reply))
            .await
            .map_err(|_| PeerError::Stopped)?;
        receiver.await.map_err(|_| PeerError::Stopped)?
    }
}

pub struct PeerNode;

impl PeerNode {
    /// Start a swarm. The returned handle drives it; dropping every clone stops it.
    pub fn start(cache: Arc<Cache>, config: PeerConfig) -> crate::Result<PeerHandle> {
        let keypair = Keypair::generate_ed25519();
        let peer_id = PeerId::from(keypair.public());

        let transport = libp2p_tcp::tokio::Transport::new(libp2p_tcp::Config::default())
            .upgrade(Version::V1Lazy)
            .authenticate(
                libp2p_noise::Config::new(&keypair)
                    .map_err(|e| PeerError::Transport(e.to_string()))?,
            )
            .multiplex(libp2p_yamux::Config::default())
            .boxed();

        let behaviour = Behaviour {
            identify: libp2p_identify::Behaviour::new(libp2p_identify::Config::new(
                crate::PROTOCOL.to_string(),
                keypair.public(),
            )),
            blobs: request_response::Behaviour::new(
                [(protocol(), ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(config.request_timeout),
            ),
        };

        let swarm_config =
            libp2p_swarm::Config::with_tokio_executor().with_idle_connection_timeout(Duration::from_secs(60));
        let mut swarm = Swarm::new(transport, behaviour, peer_id, swarm_config);

        for address in &config.listen {
            swarm
                .listen_on(address.clone())
                .map_err(|e| PeerError::Transport(e.to_string()))?;
        }
        for address in &config.bootstrap {
            if let Err(err) = swarm.dial(address.clone()) {
                tracing::warn!(%address, %err, "could not dial bootstrap peer");
            }
        }

        let (sender, receiver) = mpsc::channel(64);
        tokio::spawn(run(swarm, cache, config, receiver));

        Ok(PeerHandle {
            commands: sender,
            peer_id,
        })
    }
}

/// One in-flight fetch, fanned out to every connected peer.
struct Pending {
    request: BlobRequest,
    reply: Option<oneshot::Sender<crate::Result<Vec<u8>>>>,
    outstanding: usize,
}

async fn run(
    mut swarm: Swarm<Behaviour>,
    cache: Arc<Cache>,
    config: PeerConfig,
    mut commands: mpsc::Receiver<Command>,
) {
    let mut connected: Vec<PeerId> = Vec::new();
    let mut listeners: Vec<Multiaddr> = Vec::new();
    let mut pending: HashMap<request_response::OutboundRequestId, u64> = HashMap::new();
    let mut fetches: HashMap<u64, Pending> = HashMap::new();
    let mut next_fetch = 0u64;

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return };
                match command {
                    Command::Fetch { request, reply } => {
                        if connected.is_empty() {
                            let _ = reply.send(Err(PeerError::NotFound));
                            continue;
                        }
                        let id = next_fetch;
                        next_fetch += 1;
                        let mut outstanding = 0;
                        for peer in &connected {
                            let outbound = swarm
                                .behaviour_mut()
                                .blobs
                                .send_request(peer, request.clone());
                            pending.insert(outbound, id);
                            outstanding += 1;
                        }
                        fetches.insert(id, Pending { request, reply: Some(reply), outstanding });
                    }
                    Command::Peers(reply) => { let _ = reply.send(connected.clone()); }
                    Command::Listeners(reply) => { let _ = reply.send(listeners.clone()); }
                    Command::Dial(address, reply) => {
                        let result = swarm
                            .dial(address)
                            .map_err(|e| PeerError::Transport(e.to_string()));
                        let _ = reply.send(result);
                    }
                }
            }

            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    tracing::info!(%address, "peer listening");
                    listeners.push(address);
                }
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    if !connected.contains(&peer_id) {
                        connected.push(peer_id);
                    }
                }
                SwarmEvent::ConnectionClosed { peer_id, num_established, .. } => {
                    if num_established == 0 {
                        connected.retain(|p| *p != peer_id);
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Blobs(event)) => {
                    handle_blob_event(&mut swarm, &cache, &config, &mut pending, &mut fetches, event);
                }
                _ => {}
            }
        }
    }
}

fn handle_blob_event(
    swarm: &mut Swarm<Behaviour>,
    cache: &Arc<Cache>,
    config: &PeerConfig,
    pending: &mut HashMap<request_response::OutboundRequestId, u64>,
    fetches: &mut HashMap<u64, Pending>,
    event: request_response::Event<BlobRequest, BlobResponse>,
) {
    match event {
        request_response::Event::Message { message, .. } => match message {
            request_response::Message::Request {
                request, channel, ..
            } => {
                let response = if config.serve {
                    serve(cache, &request)
                } else {
                    BlobResponse::Missing
                };
                answer(swarm, channel, response);
            }

            request_response::Message::Response {
                request_id,
                response,
            } => {
                let Some(fetch_id) = pending.remove(&request_id) else {
                    return;
                };
                let Some(entry) = fetches.get_mut(&fetch_id) else {
                    return;
                };
                entry.outstanding = entry.outstanding.saturating_sub(1);

                if let BlobResponse::Have(body) = response {
                    // The only place a peer's bytes are ever accepted, and they
                    // are checked against the hash that was asked for.
                    if body.len() <= MAX_BODY && entry.request.is_satisfied_by(&body) {
                        if let Some(reply) = entry.reply.take() {
                            let _ = reply.send(Ok(body));
                        }
                        fetches.remove(&fetch_id);
                        return;
                    }
                    tracing::warn!(
                        request = %entry.request.describe(),
                        "a peer answered with bytes that do not hash to the request"
                    );
                }

                if entry.outstanding == 0 {
                    if let Some(reply) = entry.reply.take() {
                        let _ = reply.send(Err(PeerError::NotFound));
                    }
                    fetches.remove(&fetch_id);
                }
            }
        },

        request_response::Event::OutboundFailure { request_id, .. } => {
            let Some(fetch_id) = pending.remove(&request_id) else {
                return;
            };
            let Some(entry) = fetches.get_mut(&fetch_id) else {
                return;
            };
            entry.outstanding = entry.outstanding.saturating_sub(1);
            if entry.outstanding == 0 {
                if let Some(reply) = entry.reply.take() {
                    let _ = reply.send(Err(PeerError::NotFound));
                }
                fetches.remove(&fetch_id);
            }
        }

        _ => {}
    }
}

/// Answer a peer's request out of our own blob store.
///
/// Note what this cannot leak: the request is a hash, so we never learn which
/// URL the peer is after, and we never tell them one either.
fn serve(cache: &Arc<Cache>, request: &BlobRequest) -> BlobResponse {
    let content = match request {
        BlobRequest::Content(id) => Some(ContentId(*id)),
        BlobRequest::Integrity { algorithm, digest } => {
            let Some(algorithm) = crate::protocol::algorithm_from_tag(*algorithm) else {
                return BlobResponse::Missing;
            };
            let hash = syndeo_cache::sri::Hash {
                algorithm,
                digest: digest.clone(),
            };
            cache.content_for_integrity(&hash).ok().flatten()
        }
    };

    let Some(content) = content else {
        return BlobResponse::Missing;
    };
    match cache.body_by_content(content) {
        Ok(body) if body.len() <= MAX_BODY => BlobResponse::Have(body),
        _ => BlobResponse::Missing,
    }
}

fn answer(
    swarm: &mut Swarm<Behaviour>,
    channel: ResponseChannel<BlobResponse>,
    response: BlobResponse,
) {
    if swarm.behaviour_mut().blobs.send_response(channel, response).is_err() {
        tracing::debug!("the peer went away before we could answer");
    }
}
