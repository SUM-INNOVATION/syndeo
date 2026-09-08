//! SUM Chain addresses.
//!
//! The rule is the chain's, not ours: BLAKE3 the Ed25519 public key, take the
//! last 20 bytes, render base58 with a four-byte double-BLAKE3 checksum. Matching
//! `sumchain-wire`'s `Address::from_public_key` exactly is the whole point — an
//! address the browser shows must be the address the chain will credit.

pub const ADDRESS_SIZE: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address([u8; ADDRESS_SIZE]);

impl Address {
    pub fn from_public_key(public_key: &[u8; 32]) -> Self {
        let hash = blake3::hash(public_key);
        let mut bytes = [0u8; ADDRESS_SIZE];
        bytes.copy_from_slice(&hash.as_bytes()[12..32]);
        Address(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; ADDRESS_SIZE] {
        &self.0
    }

    pub fn to_base58(&self) -> String {
        let checksum = checksum(&self.0);
        let mut buffer = Vec::with_capacity(ADDRESS_SIZE + 4);
        buffer.extend_from_slice(&self.0);
        buffer.extend_from_slice(&checksum);
        bs58::encode(buffer).into_string()
    }

    pub fn from_base58(s: &str) -> Option<Self> {
        let decoded = bs58::decode(s).into_vec().ok()?;
        if decoded.len() != ADDRESS_SIZE + 4 {
            return None;
        }
        let (bytes, given) = decoded.split_at(ADDRESS_SIZE);
        if checksum(bytes) != given {
            return None;
        }
        let mut out = [0u8; ADDRESS_SIZE];
        out.copy_from_slice(bytes);
        Some(Address(out))
    }

    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_base58())
    }
}

fn checksum(bytes: &[u8]) -> [u8; 4] {
    let once = blake3::hash(bytes);
    let twice = blake3::hash(once.as_bytes());
    let mut out = [0u8; 4];
    out.copy_from_slice(&twice.as_bytes()[0..4]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derivation, spelled out independently of the implementation, so this
    /// test would catch a change to either.
    #[test]
    fn matches_the_chain_rule_for_from_public_key() {
        let public_key = [7u8; 32];
        let expected = {
            let hash = blake3::hash(&public_key);
            let mut bytes = [0u8; 20];
            bytes.copy_from_slice(&hash.as_bytes()[12..32]);
            bytes
        };
        assert_eq!(Address::from_public_key(&public_key).as_bytes(), &expected);
    }

    #[test]
    fn base58_round_trips_with_a_checksum() {
        let address = Address::from_public_key(&[42u8; 32]);
        let text = address.to_base58();
        assert_eq!(Address::from_base58(&text), Some(address));
    }

    #[test]
    fn a_corrupted_address_fails_the_checksum() {
        let address = Address::from_public_key(&[1u8; 32]);
        let mut text: Vec<char> = address.to_base58().chars().collect();
        // Flip one character to something else in the alphabet.
        text[3] = if text[3] == 'z' { 'y' } else { 'z' };
        let corrupted: String = text.into_iter().collect();
        assert_eq!(Address::from_base58(&corrupted), None);
    }
}
