//! The swarm.
//!
//! TCP, Noise, Yamux, boxed — the same construction SUM Chain settled on, so a
//! reader of one recognises the other. Behaviour is identify, Kademlia, and one
//! request-response protocol.
//!
//! **Why Kademlia and not mDNS.** SUM Chain removed `libp2p-mdns` because it
//! drags in `hickory-proto`, and this tree depends on the same de-umbrellaed
//! sub-crate set for the same reason. Kademlia brings no DNS with it, and it
//! does two jobs rather than one: the routing table grows past the peers that
//! were configured by hand, and provider records answer "who has these bytes"
//! without anyone having to be asked directly.

use crate::codec::{protocol, BlobCodec};
use crate::protocol::{record_key, BlobRequest, BlobResponse, MAX_BODY};
use crate::PeerError;
use futures::StreamExt;
use libp2p_core::upgrade::Version;
use libp2p_core::{Multiaddr, Transport};
use libp2p_identity::Keypair;
use libp2p_kad::store::MemoryStore;
use libp2p_kad::{self as kad, QueryId, RecordKey};
use libp2p_request_response::{self as request_response, ProtocolSupport, ResponseChannel};
use libp2p_swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use syndeo_cache::{Cache, ContentId};
use tokio::sync::{mpsc, oneshot};

pub use libp2p_identity::PeerId;

/// The DHT this tree speaks. Separate from the public IPFS DHT on purpose.
pub const KAD_PROTOCOL: &str = "/syndeo/kad/1.0.0";

// `prelude` points the derive at the libp2p-swarm sub-crate rather than the
// `::libp2p::swarm::derive_prelude` umbrella path, which is not in this tree.
// Same reason SUM Chain does it: the umbrella drags in libp2p-mdns.
#[derive(NetworkBehaviour)]
#[behaviour(prelude = "libp2p_swarm::derive_prelude")]
pub struct Behaviour {
    identify: libp2p_identify::Behaviour,
    kademlia: kad::Behaviour<MemoryStore>,
    blobs: request_response::Behaviour<BlobCodec>,
}

#[derive(Debug, Clone)]
pub struct PeerConfig {
    pub listen: Vec<Multiaddr>,
    pub bootstrap: Vec<Multiaddr>,
    pub request_timeout: Duration,
    /// Serve bodies to peers as well as asking for them.
    pub serve: bool,
    /// Join the DHT, so the set of reachable peers is not limited to the ones
    /// named on the command line.
    pub discovery: bool,
    /// Publish a provider record for each body we are willing to serve. Off
    /// without `serve`, since advertising what we will not hand over is only a
    /// disclosure.
    pub announce: bool,
    /// How often to re-walk the DHT to keep the routing table fresh.
    pub bootstrap_interval: Duration,
    /// How long a fetch may spend looking before it gives up.
    pub fetch_timeout: Duration,
}

impl Default for PeerConfig {
    fn default() -> Self {
        PeerConfig {
            listen: vec!["/ip4/0.0.0.0/tcp/0".parse().expect("a valid multiaddr")],
            bootstrap: Vec::new(),
            request_timeout: Duration::from_secs(10),
            serve: true,
            discovery: true,
            announce: true,
            bootstrap_interval: Duration::from_secs(300),
            fetch_timeout: Duration::from_secs(20),
        }
    }
}

/// What one peer has taken from us and given us.
///
/// The point of keeping this is that a node which only takes should not be
/// indistinguishable from one that gives. It is not a reputation system and
/// cannot be: identities are free, so a peer that exhausts its credit can make
/// a new one. What it does buy is that the cheapest form of freeloading — one
/// identity taking indefinitely — costs something.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ledger {
    pub served: u64,
    pub bytes_served: u64,
    pub received: u64,
    pub bytes_received: u64,
    /// Bodies we have handed over without one coming back. Reset by giving.
    pub debt: u32,
}

/// How many bodies a peer may take before it has given anything.
///
/// Not zero, because a peer that has just joined has nothing to its name and
/// still needs to be able to start.
pub const OPENING_CREDIT: u32 = 32;

impl Ledger {
    pub fn in_credit(&self) -> bool {
        self.debt < OPENING_CREDIT
    }

    /// Given minus taken. Ordering by this asks the most generous peers first.
    pub fn standing(&self) -> i64 {
        self.received as i64 - self.served as i64
    }
}

#[derive(Debug, Clone)]
pub struct PeerReport {
    pub peer: PeerId,
    pub ledger: Ledger,
}

#[derive(Debug, Clone)]
pub struct SwarmStatus {
    pub peer_id: PeerId,
    pub listeners: Vec<Multiaddr>,
    pub connected: Vec<PeerReport>,
    /// Peers in the DHT routing table, which is the number that says whether
    /// discovery is working.
    pub routing_table: usize,
    /// Bodies we have published a provider record for.
    pub announced: usize,
    pub serving: bool,
}

enum Command {
    Fetch {
        request: BlobRequest,
        reply: oneshot::Sender<crate::Result<Vec<u8>>>,
    },
    Announce(Vec<u8>),
    Status(oneshot::Sender<SwarmStatus>),
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

    /// Tell the DHT we are willing to serve these bytes.
    ///
    /// Called for bodies a page declared an integrity hash for, which are
    /// exactly the ones another node could ask for and check.
    pub async fn announce(&self, request: &BlobRequest) -> crate::Result<()> {
        self.commands
            .send(Command::Announce(record_key(request)))
            .await
            .map_err(|_| PeerError::Stopped)
    }

    pub async fn announce_integrity(&self, hash: &syndeo_cache::sri::Hash) -> crate::Result<()> {
        self.announce(&BlobRequest::integrity(hash)).await
    }

    pub async fn status(&self) -> crate::Result<SwarmStatus> {
        let (reply, receiver) = oneshot::channel();
        self.commands
            .send(Command::Status(reply))
            .await
            .map_err(|_| PeerError::Stopped)?;
        receiver.await.map_err(|_| PeerError::Stopped)
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

        // Our own protocol name, not `kad::PROTOCOL_NAME`. The default is
        // `/ipfs/kad/1.0.0`, and adopting it would put every Syndeo node on the
        // public IPFS DHT — announcing which bodies it holds to a network that
        // has nothing to do with this one.
        let mut kad_config = kad::Config::default();
        kad_config.set_protocol_names(vec![libp2p_swarm::StreamProtocol::new(KAD_PROTOCOL)]);
        let mut kademlia =
            kad::Behaviour::with_config(peer_id, MemoryStore::new(peer_id), kad_config);
        // A node that serves is a full DHT participant. One that only reads
        // stays a client, so it is never asked to hold other people's records.
        kademlia.set_mode(Some(if config.serve {
            kad::Mode::Server
        } else {
            kad::Mode::Client
        }));

        let behaviour = Behaviour {
            identify: libp2p_identify::Behaviour::new(libp2p_identify::Config::new(
                crate::PROTOCOL.to_string(),
                keypair.public(),
            )),
            kademlia,
            blobs: request_response::Behaviour::new(
                [(protocol(), ProtocolSupport::Full)],
                request_response::Config::default().with_request_timeout(config.request_timeout),
            ),
        };

        let swarm_config = libp2p_swarm::Config::with_tokio_executor()
            .with_idle_connection_timeout(Duration::from_secs(60));
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

/// One in-flight fetch.
struct Pending {
    request: BlobRequest,
    reply: Option<oneshot::Sender<crate::Result<Vec<u8>>>>,
    /// Requests sent and not yet answered.
    outstanding: usize,
    /// Peers already asked, so a provider we are connected to is not asked twice.
    asked: HashSet<PeerId>,
    /// The DHT query, once the directly-connected peers have come up empty.
    query: Option<QueryId>,
    deadline: Instant,
}

impl Pending {
    fn finish(&mut self, outcome: crate::Result<Vec<u8>>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(outcome);
        }
    }
}

struct State {
    connected: Vec<PeerId>,
    listeners: Vec<Multiaddr>,
    ledgers: HashMap<PeerId, Ledger>,
    announced: HashSet<Vec<u8>>,
    pending: HashMap<request_response::OutboundRequestId, u64>,
    provider_queries: HashMap<QueryId, u64>,
    fetches: HashMap<u64, Pending>,
    next_fetch: u64,
}

impl State {
    /// Connected peers, most generous first. A soft preference, not a gate: a
    /// peer with nothing to its name is still asked, just last.
    fn by_standing(&self) -> Vec<PeerId> {
        let mut peers = self.connected.clone();
        peers.sort_by_key(|p| {
            std::cmp::Reverse(self.ledgers.get(p).copied().unwrap_or_default().standing())
        });
        peers
    }
}

async fn run(
    mut swarm: Swarm<Behaviour>,
    cache: Arc<Cache>,
    config: PeerConfig,
    mut commands: mpsc::Receiver<Command>,
) {
    let mut state = State {
        connected: Vec::new(),
        listeners: Vec::new(),
        ledgers: HashMap::new(),
        announced: HashSet::new(),
        pending: HashMap::new(),
        provider_queries: HashMap::new(),
        fetches: HashMap::new(),
        next_fetch: 0,
    };

    let mut sweep = tokio::time::interval(Duration::from_millis(250));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut refresh = tokio::time::interval(config.bootstrap_interval);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return };
                handle_command(&mut swarm, &config, &mut state, command);
            }

            _ = sweep.tick() => {
                expire(&mut state);
            }

            _ = refresh.tick() => {
                // Walking the DHT is what turns one hand-configured bootstrap
                // peer into a routing table. Without it, discovery would stop
                // at whatever was dialled at startup.
                if config.discovery && !state.connected.is_empty() {
                    let _ = swarm.behaviour_mut().kademlia.bootstrap();
                }
            }

            event = swarm.select_next_some() => {
                handle_event(&mut swarm, &cache, &config, &mut state, event);
            }
        }
    }
}

fn handle_command(
    swarm: &mut Swarm<Behaviour>,
    config: &PeerConfig,
    state: &mut State,
    command: Command,
) {
    match command {
        Command::Fetch { request, reply } => {
            let id = state.next_fetch;
            state.next_fetch += 1;
            let mut entry = Pending {
                request: request.clone(),
                reply: Some(reply),
                outstanding: 0,
                asked: HashSet::new(),
                query: None,
                deadline: Instant::now() + config.fetch_timeout,
            };

            for peer in state.by_standing() {
                let outbound = swarm.behaviour_mut().blobs.send_request(&peer, request.clone());
                state.pending.insert(outbound, id);
                entry.asked.insert(peer);
                entry.outstanding += 1;
            }

            if entry.outstanding == 0 && !start_provider_query(swarm, config, &mut entry, id, state) {
                entry.finish(Err(PeerError::NotFound));
                return;
            }
            state.fetches.insert(id, entry);
        }

        Command::Announce(key) => {
            if !config.announce || !config.serve {
                return;
            }
            if !state.announced.insert(key.clone()) {
                return;
            }
            if let Err(err) = swarm
                .behaviour_mut()
                .kademlia
                .start_providing(RecordKey::new(&key))
            {
                tracing::debug!(%err, "could not announce a body to the DHT");
                state.announced.remove(&key);
            }
        }

        Command::Status(reply) => {
            let routing_table = swarm
                .behaviour_mut()
                .kademlia
                .kbuckets()
                .map(|bucket| bucket.num_entries())
                .sum();
            let _ = reply.send(SwarmStatus {
                peer_id: *swarm.local_peer_id(),
                listeners: state.listeners.clone(),
                connected: state
                    .connected
                    .iter()
                    .map(|peer| PeerReport {
                        peer: *peer,
                        ledger: state.ledgers.get(peer).copied().unwrap_or_default(),
                    })
                    .collect(),
                routing_table,
                announced: state.announced.len(),
                serving: config.serve,
            });
        }

        Command::Peers(reply) => {
            let _ = reply.send(state.connected.clone());
        }
        Command::Listeners(reply) => {
            let _ = reply.send(state.listeners.clone());
        }
        Command::Dial(address, reply) => {
            let result = swarm
                .dial(address)
                .map_err(|e| PeerError::Transport(e.to_string()));
            let _ = reply.send(result);
        }
    }
}

/// Ask the DHT who has these bytes. Returns whether a query was started.
fn start_provider_query(
    swarm: &mut Swarm<Behaviour>,
    config: &PeerConfig,
    entry: &mut Pending,
    id: u64,
    state: &mut State,
) -> bool {
    if !config.discovery || entry.query.is_some() {
        return false;
    }
    let query = swarm
        .behaviour_mut()
        .kademlia
        .get_providers(RecordKey::new(&record_key(&entry.request)));
    entry.query = Some(query);
    state.provider_queries.insert(query, id);
    true
}

/// Fail fetches that have run out of time.
///
/// Request-response has its own per-request timeout, but a DHT query that finds
/// nothing produces no failure event at all, so without this a fetch with no
/// providers would wait forever.
fn expire(state: &mut State) {
    let now = Instant::now();
    let expired: Vec<u64> = state
        .fetches
        .iter()
        .filter(|(_, entry)| entry.deadline <= now)
        .map(|(id, _)| *id)
        .collect();
    for id in expired {
        if let Some(mut entry) = state.fetches.remove(&id) {
            entry.finish(Err(PeerError::Timeout));
        }
    }
}

fn handle_event(
    swarm: &mut Swarm<Behaviour>,
    cache: &Arc<Cache>,
    config: &PeerConfig,
    state: &mut State,
    event: SwarmEvent<BehaviourEvent>,
) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!(%address, "peer listening");
            state.listeners.push(address);
        }
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            if !state.connected.contains(&peer_id) {
                state.connected.push(peer_id);
            }
        }
        SwarmEvent::ConnectionClosed {
            peer_id,
            num_established,
            ..
        } => {
            if num_established == 0 {
                state.connected.retain(|p| *p != peer_id);
            }
        }

        SwarmEvent::Behaviour(BehaviourEvent::Identify(libp2p_identify::Event::Received {
            peer_id,
            info,
            ..
        })) => {
            // Identify is where addresses come from. Feeding them to Kademlia is
            // what lets the routing table outgrow the bootstrap list, and
            // feeding them to the blob protocol is what lets a provider found in
            // the DHT actually be dialled.
            for address in info.listen_addrs {
                swarm
                    .behaviour_mut()
                    .kademlia
                    .add_address(&peer_id, address.clone());
                // So the blob protocol can dial a provider the DHT named but
                // that we have never been connected to.
                swarm.add_peer_address(peer_id, address);
            }
        }

        SwarmEvent::Behaviour(BehaviourEvent::Kademlia(event)) => {
            handle_kademlia_event(swarm, state, event);
        }

        SwarmEvent::Behaviour(BehaviourEvent::Blobs(event)) => {
            handle_blob_event(swarm, cache, config, state, event);
        }

        _ => {}
    }
}

fn handle_kademlia_event(swarm: &mut Swarm<Behaviour>, state: &mut State, event: kad::Event) {
    let kad::Event::OutboundQueryProgressed { id, result, .. } = event else {
        return;
    };
    let kad::QueryResult::GetProviders(result) = result else {
        return;
    };
    let Some(&fetch_id) = state.provider_queries.get(&id) else {
        return;
    };

    // Progress, not completion: more providers may still arrive, so the fetch
    // stays open even if none of these turn out to be new.
    if let Ok(kad::GetProvidersOk::FoundProviders { providers, .. }) = &result {
        let local = *swarm.local_peer_id();
        if let Some(entry) = state.fetches.get_mut(&fetch_id) {
            for peer in providers {
                if *peer == local || !entry.asked.insert(*peer) {
                    continue;
                }
                let outbound = swarm
                    .behaviour_mut()
                    .blobs
                    .send_request(peer, entry.request.clone());
                state.pending.insert(outbound, fetch_id);
                entry.outstanding += 1;
            }
        }
        return;
    }

    // The query is over: either it finished, or it failed. Either way the DHT
    // has no more to say, and a fetch with nothing outstanding has its answer —
    // waiting for the deadline instead would turn "nobody has it" into a stall.
    state.provider_queries.remove(&id);
    let Some(entry) = state.fetches.get_mut(&fetch_id) else {
        return;
    };
    entry.query = None;
    if entry.outstanding == 0 {
        let mut entry = state.fetches.remove(&fetch_id).expect("just looked it up");
        entry.finish(Err(PeerError::NotFound));
    }
}

fn handle_blob_event(
    swarm: &mut Swarm<Behaviour>,
    cache: &Arc<Cache>,
    config: &PeerConfig,
    state: &mut State,
    event: request_response::Event<BlobRequest, BlobResponse>,
) {
    match event {
        request_response::Event::Message { peer, message, .. } => match message {
            request_response::Message::Request {
                request, channel, ..
            } => {
                let ledger = state.ledgers.entry(peer).or_default();
                let response = if !config.serve {
                    BlobResponse::Missing
                } else if !ledger.in_credit() {
                    tracing::debug!(%peer, debt = ledger.debt, "a peer has taken more than it gave");
                    BlobResponse::Throttled
                } else {
                    serve(cache, &request)
                };
                if let BlobResponse::Have(body) = &response {
                    ledger.served += 1;
                    ledger.bytes_served += body.len() as u64;
                    ledger.debt = ledger.debt.saturating_add(1);
                }
                answer(swarm, channel, response);
            }

            request_response::Message::Response {
                request_id,
                response,
            } => {
                let Some(fetch_id) = state.pending.remove(&request_id) else {
                    return;
                };
                let Some(entry) = state.fetches.get_mut(&fetch_id) else {
                    return;
                };
                entry.outstanding = entry.outstanding.saturating_sub(1);

                if let BlobResponse::Have(body) = response {
                    // The only place a peer's bytes are ever accepted, and they
                    // are checked against the hash that was asked for.
                    if body.len() <= MAX_BODY && entry.request.is_satisfied_by(&body) {
                        let ledger = state.ledgers.entry(peer).or_default();
                        ledger.received += 1;
                        ledger.bytes_received += body.len() as u64;
                        // Giving earns back the right to take.
                        ledger.debt = ledger.debt.saturating_sub(1);

                        let mut entry = state.fetches.remove(&fetch_id).expect("just looked it up");
                        entry.finish(Ok(body));
                        return;
                    }
                    tracing::warn!(
                        %peer,
                        request = %entry.request.describe(),
                        "a peer answered with bytes that do not hash to the request"
                    );
                }

                exhausted(swarm, config, state, fetch_id);
            }
        },

        request_response::Event::OutboundFailure { request_id, .. } => {
            let Some(fetch_id) = state.pending.remove(&request_id) else {
                return;
            };
            if let Some(entry) = state.fetches.get_mut(&fetch_id) {
                entry.outstanding = entry.outstanding.saturating_sub(1);
            }
            exhausted(swarm, config, state, fetch_id);
        }

        _ => {}
    }
}

/// Every peer asked so far has come up empty. Widen the search, or give up.
fn exhausted(swarm: &mut Swarm<Behaviour>, config: &PeerConfig, state: &mut State, fetch_id: u64) {
    let Some(entry) = state.fetches.get_mut(&fetch_id) else {
        return;
    };
    if entry.outstanding > 0 {
        return;
    }

    // Directly-connected peers had nothing. The DHT is the wider question, and
    // it is only asked once per fetch.
    let mut entry = state.fetches.remove(&fetch_id).expect("just looked it up");
    if start_provider_query(swarm, config, &mut entry, fetch_id, state) {
        state.fetches.insert(fetch_id, entry);
        return;
    }
    entry.finish(Err(PeerError::NotFound));
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
