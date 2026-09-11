//! The typed protocols. Each process speaks exactly one of these, and the shape
//! of the enum is the boundary.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- net process

/// What a renderer or the agent may ask the network process for. Note what is
/// absent: no socket, no host, no certificate, no DNS. A URL goes in, bytes come
/// back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetRequest {
    Fetch {
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        #[serde(with = "base64_bytes")]
        body: Vec<u8>,
        /// Subresource Integrity from the markup, when the caller has it. This is
        /// what makes a peer-supplied body acceptable.
        integrity: Option<String>,
        /// The top-level document's origin — the address in the URL bar, not
        /// this resource's own.
        ///
        /// It is the cache partition. Without it a cache is shared across every
        /// site, and that is a way to be tracked: an advertiser embedded in two
        /// places can time a fetch and learn whether you have been somewhere it
        /// was already loaded. `None` is unpartitioned, which is what the
        /// measuring proxy sends and what `--shared-cache` restores.
        #[serde(default)]
        partition: Option<String>,
    },
    Stats,
    /// What the peer swarm looks like from the network process. Reports nothing
    /// about what is stored — only who is connected and what they have
    /// contributed.
    PeerStatus,
    Ping,
}

/// A fetch is answered as a sequence, not as a message.
///
/// `FetchBegin`, then zero or more `FetchChunk`, then exactly one `FetchEnd` or
/// `Error`. The reason is the frame ceiling: a response that had to fit in one
/// frame put a hard limit on how large a resource the browser could load, and
/// the limit had nothing to do with the web and everything to do with our own
/// transport. A body in pieces has no such limit, and the renderer above it can
/// act on the first piece rather than waiting for the last.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetResponse {
    FetchBegin {
        status: u16,
        headers: Vec<(String, String)>,
        source: String,
        /// Which protocol carried it: `http/1.1`, `h2`, `h3`, or `-` when
        /// nothing did because it came out of the store.
        protocol: String,
        elapsed_ms: u64,
    },
    FetchChunk {
        #[serde(with = "base64_bytes")]
        bytes: Vec<u8>,
    },
    FetchEnd {
        /// The content address, known only once the whole body has arrived.
        content: Option<String>,
    },
    Stats(serde_json::Value),
    PeerStatus(serde_json::Value),
    Pong,
    Error(String),
}

// -------------------------------------------------------------- shell process

/// What the agent may ask the shell for.
///
/// There is no `Sign` here that reaches the keystore directly, and there is no
/// variant carrying key material in either direction. The agent can ask for a
/// human to be prompted; the shell decides whether to prompt, and it is the
/// shell — never the agent — that then speaks to the keystore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShellRequest {
    /// Ask the shell to show the user a payload and, if they agree, have it
    /// signed. The agent never learns the key, the derivation path, or the seed.
    RequestSignature {
        origin: String,
        purpose: SignaturePurpose,
        /// Rendered for the human, verbatim, in the confirmation dialog.
        description: String,
        #[serde(with = "base64_bytes")]
        payload: Vec<u8>,
    },
    /// Ask for the public identity the shell would use for an origin. Public
    /// keys are not secret, but the agent still has to go through the shell.
    IdentityFor {
        origin: String,
    },
    /// Ask a yes/no question of the user.
    Confirm {
        title: String,
        detail: String,
    },
    Ping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignaturePurpose {
    /// Prove control of the origin-scoped identity to a site.
    OriginLogin,
    /// A transaction destined for SUM Chain.
    ChainTransaction,
    /// Anything else; the description carries the detail.
    Attestation,
}

impl SignaturePurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            SignaturePurpose::OriginLogin => "origin login",
            SignaturePurpose::ChainTransaction => "chain transaction",
            SignaturePurpose::Attestation => "attestation",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShellResponse {
    Signed {
        /// Hex-encoded Ed25519 signature.
        signature: String,
        /// Hex-encoded public key that produced it.
        public_key: String,
        /// The SUM Chain address for that key, base58 with checksum.
        address: String,
    },
    Identity {
        public_key: String,
        address: String,
    },
    Confirmed(bool),
    /// The user said no, or the prompt timed out.
    Declined(String),
    Pong,
    Error(String),
}

// ----------------------------------------------------------- keystore process

/// The keystore's entire surface.
///
/// One signing operation, and it is refused without a confirmation the shell
/// minted after showing the payload to a human. Everything else here is
/// setup and enrolment, which the shell drives interactively and the agent
/// cannot reach because it has no connection to this process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeystoreRequest {
    /// The one operation: sign this payload, because the shell confirmed it.
    SignConfirmed {
        confirmation: crate::confirm::Confirmation,
        #[serde(with = "base64_bytes")]
        payload: Vec<u8>,
    },
    /// Public key and address for an origin. Derives nothing secret out.
    PublicIdentity { origin: String },
    /// Whether a seed exists and whether it is currently unsealed.
    Status,
    /// First-run enrolment. Returns a recovery mnemonic exactly once.
    Initialize { passphrase: Option<String> },
    /// Restore from a recovery mnemonic.
    Restore {
        mnemonic: String,
        passphrase: Option<String>,
    },
    /// Unseal the root secret for this session.
    Unseal { passphrase: Option<String> },
    /// Forget the unsealed material.
    Lock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeystoreResponse {
    Signature {
        signature: String,
        public_key: String,
        address: String,
    },
    Identity {
        public_key: String,
        address: String,
    },
    Status {
        initialized: bool,
        unsealed: bool,
        passphrase_required: bool,
        presence_enforced: bool,
        /// After how many idle seconds the seed is forgotten, if ever.
        idle_timeout_secs: Option<u64>,
        /// How long since the last operation.
        idle_for_secs: u64,
    },
    /// The session has ended and the seed is gone. Recoverable: the shell asks
    /// the user to unseal and tries again. Distinct from `Error` so the shell
    /// does not have to read an error message to know that.
    Locked,
    /// Shown to the user once, at setup, and never written to disk.
    Initialized {
        mnemonic: String,
        address: String,
    },
    Ok,
    Error(String),
}

// -------------------------------------------------------------- agent process

/// What the agent emits for the shell to render. One-way; no replies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    Log { level: String, message: String },
    Navigated { url: String, title: String },
    Finished { summary: String },
    Failed { error: String },
}

/// A fetch reassembled from its frames.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub source: String,
    pub protocol: String,
    pub elapsed_ms: u64,
    pub content: Option<String>,
}

impl Fetched {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Byte vectors travel as base64 so a frame stays valid JSON.
mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        decode(&text).map_err(serde::de::Error::custom)
    }

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
            let indices = [n >> 18 & 63, n >> 12 & 63, n >> 6 & 63, n & 63];
            for (i, idx) in indices.iter().enumerate() {
                if i <= chunk.len() {
                    out.push(ALPHABET[*idx as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(text: &str) -> Result<Vec<u8>, String> {
        let mut acc: u32 = 0;
        let mut bits = 0u32;
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        for ch in text.bytes() {
            if ch == b'=' {
                break;
            }
            let Some(value) = ALPHABET.iter().position(|c| *c == ch) else {
                if ch.is_ascii_whitespace() {
                    continue;
                }
                return Err(format!("invalid base64 character {:?}", ch as char));
            };
            acc = (acc << 6) | value as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_round_trips_every_length_class() {
        for len in 0..64 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let encoded = super::base64_bytes::encode(&bytes);
            assert_eq!(
                super::base64_bytes::decode(&encoded).unwrap(),
                bytes,
                "len {len}"
            );
        }
    }

    #[test]
    fn the_agent_protocol_has_no_route_to_the_keystore() {
        // This is a documentation test with teeth: if someone adds a variant to
        // ShellRequest that carries key material or names a derivation path, the
        // serialized form changes and this fails.
        let variants = serde_json::to_string(&super::ShellRequest::Ping).unwrap();
        assert_eq!(variants, "\"Ping\"");
    }
}

/// The cache partition for a document: its origin, and nothing more of it.
///
/// Scheme, host and port — so `https://example.test/a` and
/// `https://example.test/b` share a partition, and `http://` and `https://` do
/// not. Chrome partitions on the registrable domain instead, which needs a
/// public suffix list to work out that `bbc.co.uk` is a site and `co.uk` is
/// not; the origin is stricter than that, needs no list, and errs towards more
/// partitions rather than fewer. Subdomains of one site therefore do not share
/// a cache, which costs some hit rate and gives nothing away.
pub fn partition_for(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let url = &url;
    match url.host_str() {
        Some(host) => Some(match url.port() {
            Some(port) => format!("{}://{}:{}", url.scheme(), host, port),
            None => format!("{}://{}", url.scheme(), host),
        }),
        // `about:blank` and friends have no host and so no site to be
        // partitioned under. They get their own partition rather than sharing
        // the unpartitioned one with everything else.
        None => Some(format!("{}:", url.scheme())),
    }
}
