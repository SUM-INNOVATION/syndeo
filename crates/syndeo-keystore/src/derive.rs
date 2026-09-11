//! SLIP-0010 hierarchical derivation for Ed25519, so each origin gets its own
//! keypair and no site can correlate the user across sites by public key.
//!
//! Ed25519 has no public-key derivation, so every level is hardened. That is not
//! a limitation here: we never want a site to be able to walk the tree anyway.

use crate::address::Address;
use ed25519_dalek::{Signature, Signer, SigningKey};
use hmac::{Hmac, Mac};
use sha2::Sha512;
use zeroize::{Zeroize, ZeroizeOnDrop};

type HmacSha512 = Hmac<Sha512>;

const CURVE: &[u8] = b"ed25519 seed";
const HARDENED: u32 = 0x8000_0000;

/// SLIP-0044 coin type for SUM. **Pinned.**
///
/// SUM has no registered SLIP-0044 index, so this is ours by declaration rather
/// than by allocation. It is pinned at 8848 because that is the value every
/// address this tree has ever derived already used: the alternative was to
/// change every address for no gain, which is free today and impossible the
/// moment anyone holds a balance. If an index is registered later it will have
/// to be this one.
///
/// Changing it changes every address the browser will ever show. The test vector
/// in this module exists so that a change is caught by the suite rather than
/// discovered by a user whose funds went somewhere else.
pub const SUM_COIN_TYPE: u32 = 8848;

/// SUM Chain's network id, which is **not** the coin type and must never be
/// substituted for it.
///
/// They are two different numbers doing two different jobs: the chain id
/// identifies the network a transaction is valid on, and the coin type
/// identifies the branch of the key tree an address is derived from. Recorded
/// here because 1 and 8848 sitting in separate files is exactly how one ends up
/// in the other's place.
pub const SUM_CHAIN_ID: u64 = 1;

/// BIP-44 purpose.
const PURPOSE: u32 = 44;

/// An extended key: the 32-byte scalar seed and its chain code.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct ExtendedKey {
    key: [u8; 32],
    chain_code: [u8; 32],
}

impl std::fmt::Debug for ExtendedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExtendedKey(redacted)")
    }
}

impl ExtendedKey {
    /// SLIP-0010 master generation from a BIP-39 seed.
    pub fn master(seed: &[u8]) -> Self {
        let mut mac = HmacSha512::new_from_slice(CURVE).expect("hmac accepts any key");
        mac.update(seed);
        let output = mac.finalize().into_bytes();
        let mut key = [0u8; 32];
        let mut chain_code = [0u8; 32];
        key.copy_from_slice(&output[0..32]);
        chain_code.copy_from_slice(&output[32..64]);
        ExtendedKey { key, chain_code }
    }

    /// One hardened step. `index` is given without the hardening bit.
    pub fn derive_hardened(&self, index: u32) -> Self {
        let hardened = index | HARDENED;
        let mut mac = HmacSha512::new_from_slice(&self.chain_code).expect("hmac accepts any key");
        mac.update(&[0x00]);
        mac.update(&self.key);
        mac.update(&hardened.to_be_bytes());
        let output = mac.finalize().into_bytes();
        let mut key = [0u8; 32];
        let mut chain_code = [0u8; 32];
        key.copy_from_slice(&output[0..32]);
        chain_code.copy_from_slice(&output[32..64]);
        ExtendedKey { key, chain_code }
    }

    pub fn derive_path(&self, path: &[u32]) -> Self {
        let mut current = self.clone();
        for index in path {
            current = current.derive_hardened(*index);
        }
        current
    }

    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.key)
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key().verifying_key().to_bytes()
    }

    pub fn address(&self) -> Address {
        Address::from_public_key(&self.public_key())
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key().sign(message)
    }
}

/// The derivation path for one origin: `m/44'/8848'/a'/b'/c'`, where a, b and c
/// are 31-bit slices of BLAKE3 over the canonical origin.
///
/// Deterministic, so the same origin always yields the same identity on any
/// device restored from the same mnemonic, and unlinkable, because two origins
/// share no key material.
pub fn origin_path(origin: &str) -> [u32; 5] {
    let canonical = canonical_origin(origin);
    let digest = blake3::hash(canonical.as_bytes());
    let bytes = digest.as_bytes();
    let slice = |offset: usize| {
        u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) & 0x7fff_ffff
    };
    [PURPOSE, SUM_COIN_TYPE, slice(0), slice(4), slice(8)]
}

/// Scheme, host and port — the web's own origin tuple. Path and query are not
/// part of an origin, so `https://a.test/x` and `https://a.test/y` share one key.
pub fn canonical_origin(origin: &str) -> String {
    let trimmed = origin.trim();
    let (scheme, rest) = match trimmed.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("https".to_string(), trimmed),
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_ascii_lowercase();
    let authority = authority
        .rsplit_once('@')
        .map(|(_, a)| a)
        .unwrap_or(&authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h.to_string(), Some(p.to_string()))
        }
        _ => (authority.to_string(), None),
    };
    let default_port = matches!(
        (scheme.as_str(), port.as_deref()),
        ("https", Some("443")) | ("http", Some("80"))
    );
    match port {
        Some(p) if !default_port => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    }
}

/// The identity used for an origin.
pub fn origin_key(master: &ExtendedKey, origin: &str) -> ExtendedKey {
    master.derive_path(&origin_path(origin))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master() -> ExtendedKey {
        ExtendedKey::master(b"a seed that is long enough to be a bip39 seed value")
    }

    #[test]
    fn slip_0010_test_vector_one() {
        // SLIP-0010, Test vector 1 for ed25519, seed 000102030405060708090a0b0c0d0e0f.
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();
        let m = ExtendedKey::master(&seed);
        assert_eq!(
            hex::encode(m.key),
            "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7"
        );
        assert_eq!(
            hex::encode(m.chain_code),
            "90046a93de5380a72b5e45010748567d5ea02bbf6522f979e05c0d8d8ca9fffb"
        );

        // m/0'
        let child = m.derive_hardened(0);
        assert_eq!(
            hex::encode(child.key),
            "68e0fe46dfb67e368c75379acec591dad19df3cde26e63b93a8e704f1dade7a3"
        );
        assert_eq!(
            hex::encode(child.chain_code),
            "8b59aa11380b624e81507a27fedda59fea6d0b779a778918a2fd3590e16e9c69"
        );

        // m/0'/1'/2'/2'/1000000000'
        let deep = m.derive_path(&[0, 1, 2, 2, 1_000_000_000]);
        assert_eq!(
            hex::encode(deep.key),
            "8f94d394a8e8fd6b1bc2f3f49f5c47e385281d5c17e65324b0f62483e37e8793"
        );
    }

    /// Mnemonic in, address out.
    ///
    /// This is the vector that pins the derivation. Every part of the path
    /// contributes: BIP-39 to seed, SLIP-0010 master, `m/44'/8848'/0'` for the
    /// identity, and `m/44'/8848'/a'/b'/c'` for an origin. A change to any of
    /// them — including to [`SUM_COIN_TYPE`] — moves these addresses, and moving
    /// them after anyone holds a balance strands it.
    #[test]
    fn the_pinned_derivation_produces_these_addresses_and_no_others() {
        use bip39::{Language, Mnemonic};

        // The BIP-39 English test mnemonic. Chosen because it is published, so
        // this vector can be reproduced by hand against any other SLIP-0010
        // implementation.
        const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon \
                              abandon abandon abandon abandon abandon abandon abandon abandon \
                              abandon abandon abandon abandon abandon abandon abandon art";

        let mnemonic = Mnemonic::parse_in_normalized(Language::English, PHRASE).unwrap();
        let seed = mnemonic.to_seed_normalized("");
        let master = ExtendedKey::master(&seed);

        assert_eq!(SUM_COIN_TYPE, 8848, "the coin type is pinned");
        assert_eq!(SUM_CHAIN_ID, 1, "the chain id is not the coin type");

        let identity = master.derive_path(&[PURPOSE, SUM_COIN_TYPE, 0]);
        assert_eq!(
            identity.address().to_base58(),
            "6d3w7V1x5bVHK7xf6s75JpHQWd9Ed6Xsg"
        );
        assert_eq!(
            hex::encode(identity.public_key()),
            "35a597be28cd361d3f33143093ca0f120276b75ea701c47e7c45a49a76b9fc5d"
        );

        for (origin, address) in [
            ("https://wallet.test", "8rNvk67chDLXGE4pgD6Ns49b6StyrARNd"),
            ("https://example.test", "AsU26q61p6mSRgZDM8iWRhiBfHFngvjyh"),
        ] {
            assert_eq!(
                origin_key(&master, origin).address().to_base58(),
                address,
                "the derived identity for {origin} moved"
            );
        }
    }

    #[test]
    fn each_origin_gets_its_own_identity() {
        let m = master();
        let a = origin_key(&m, "https://a.test");
        let b = origin_key(&m, "https://b.test");
        assert_ne!(a.public_key(), b.public_key());
        assert_ne!(a.address(), b.address());
    }

    #[test]
    fn derivation_is_stable_across_calls() {
        let m = master();
        assert_eq!(
            origin_key(&m, "https://a.test").address(),
            origin_key(&m, "https://a.test").address()
        );
    }

    #[test]
    fn path_and_case_do_not_change_the_origin() {
        for equivalent in [
            "https://Example.test/some/path?q=1",
            "https://example.test",
            "https://example.test:443/",
            "https://user:pw@example.test/",
        ] {
            assert_eq!(canonical_origin(equivalent), "https://example.test");
        }
    }

    #[test]
    fn scheme_and_port_are_part_of_the_origin() {
        assert_ne!(
            canonical_origin("https://example.test"),
            canonical_origin("http://example.test")
        );
        assert_ne!(
            canonical_origin("https://example.test"),
            canonical_origin("https://example.test:8443")
        );
    }

    #[test]
    fn signatures_verify_against_the_derived_public_key() {
        use ed25519_dalek::{Verifier, VerifyingKey};
        let key = origin_key(&master(), "https://wallet.test");
        let message = b"transfer 10 SUM";
        let signature = key.sign(message);
        let public = VerifyingKey::from_bytes(&key.public_key()).unwrap();
        assert!(public.verify(message, &signature).is_ok());
        assert!(public.verify(b"transfer 1000 SUM", &signature).is_err());
    }

    #[test]
    fn every_level_of_the_path_is_hardened() {
        // A path index that already carries the hardening bit must not double it.
        let m = master();
        assert_eq!(m.derive_hardened(0).key, m.derive_hardened(0).key);
        assert_ne!(m.derive_hardened(0).key, m.derive_hardened(1).key);
    }
}
