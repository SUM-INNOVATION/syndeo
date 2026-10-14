//! Whether an address is on the public internet.
//!
//! Asked of the address a response actually came from — the socket's peer,
//! after resolving, connecting and any fallback — and never of the name in
//! the URL, which can point anywhere. A proxy configured to cache only what
//! came from the public internet uses this to keep this Mac's own services,
//! the local network's, and anything else not publicly reachable out of its
//! store, even when a public name was made to resolve to them.
//!
//! It errs towards "not public": an address in any block the IANA
//! special-purpose registries reserve, or outside IPv6's global unicast
//! space, is not public, whether or not some part of that block happens to
//! be reachable. The cost of a wrong "not public" is one response not cached.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is a public, globally reachable unicast address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// IPv4 blocks that are not the public internet: RFC 6890's registry, with
/// every block not marked globally reachable, and multicast and the
/// reserved space above it.
const NOT_PUBLIC_V4: [(Ipv4Addr, u8); 16] = [
    (Ipv4Addr::new(0, 0, 0, 0), 8),       // "this network"
    (Ipv4Addr::new(10, 0, 0, 0), 8),      // private
    (Ipv4Addr::new(100, 64, 0, 0), 10),   // shared address space (carrier-grade NAT)
    (Ipv4Addr::new(127, 0, 0, 0), 8),     // loopback
    (Ipv4Addr::new(169, 254, 0, 0), 16),  // link-local
    (Ipv4Addr::new(172, 16, 0, 0), 12),   // private
    (Ipv4Addr::new(192, 0, 0, 0), 24),    // IETF protocol assignments
    (Ipv4Addr::new(192, 0, 2, 0), 24),    // documentation (TEST-NET-1)
    (Ipv4Addr::new(192, 88, 99, 0), 24),  // the retired 6to4 relay anycast
    (Ipv4Addr::new(192, 168, 0, 0), 16),  // private
    (Ipv4Addr::new(198, 18, 0, 0), 15),   // benchmarking
    (Ipv4Addr::new(198, 51, 100, 0), 24), // documentation (TEST-NET-2)
    (Ipv4Addr::new(203, 0, 113, 0), 24),  // documentation (TEST-NET-3)
    (Ipv4Addr::new(224, 0, 0, 0), 4),     // multicast
    (Ipv4Addr::new(240, 0, 0, 0), 4),     // reserved, and the broadcast address
    (Ipv4Addr::new(255, 255, 255, 255), 32),
];

fn in_v4(ip: Ipv4Addr, (network, length): (Ipv4Addr, u8)) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(length)).unwrap_or(0);
    u32::from(ip) & mask == u32::from(network) & mask
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    !NOT_PUBLIC_V4.iter().any(|&block| in_v4(ip, block))
}

fn in_v6(ip: Ipv6Addr, network: Ipv6Addr, length: u8) -> bool {
    let mask = u128::MAX.checked_shl(128 - u32::from(length)).unwrap_or(0);
    u128::from(ip) & mask == u128::from(network) & mask
}

/// Global unicast blocks that are still not the public internet.
const NOT_PUBLIC_IN_GLOBAL_V6: [(Ipv6Addr, u8); 4] = [
    // IETF protocol assignments: Teredo, benchmarking, ORCHID and others.
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
    // Documentation.
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32),
    // 6to4, reached through relays nobody here chose.
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
    // Documentation, the newer block.
    (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),
];

fn is_public_v6(ip: Ipv6Addr) -> bool {
    // An IPv4 address carried in IPv6 — mapped, or translated by the
    // well-known NAT64 prefix — is as public as that IPv4 address.
    if in_v6(ip, Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96)
        || in_v6(ip, Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96)
    {
        let [.., a, b, c, d] = ip.octets();
        return is_public_v4(Ipv4Addr::new(a, b, c, d));
    }
    // Only global unicast, 2000::/3, is public at all: everything else —
    // loopback, unspecified, unique local, link-local, site-local, multicast,
    // discard-only, local-use NAT64 — is not.
    in_v6(ip, Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3)
        && !NOT_PUBLIC_IN_GLOBAL_V6
            .iter()
            .any(|&(network, length)| in_v6(ip, network, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(text: &str) -> bool {
        is_public(text.parse().unwrap())
    }

    #[test]
    fn the_public_internet_is_public() {
        for address in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.215.14",
            "104.20.23.154",
            "2606:4700::6810:1",
            "2a00:1450:4001:80b::200e",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(public(address), "{address} should be public");
        }
    }

    #[test]
    fn this_mac_the_local_network_and_reserved_space_are_not() {
        for address in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "10.255.42.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "192.168.64.8",
            "169.254.42.1",
            "100.64.0.1",
            "100.127.255.255",
            "0.0.0.0",
            "0.1.2.3",
            "192.0.0.9",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fd42:5a7e:6e5d::1",
            "fc00::1",
            "fe80::1",
            "febf:ffff::1",
            "fec0::1",
            "ff02::1",
            "100::1",
            "64:ff9b:1::1",
            "2001::1",
            "2001:db8::1",
            "2002:c000:0204::1",
            "3fff::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
            "::127.0.0.1",
            "4000::1",
        ] {
            assert!(!public(address), "{address} should not be public");
        }
    }

    /// Each block ends where it says it does: the first address past either
    /// end is public, the first and last inside it are not.
    #[test]
    fn each_block_ends_where_it_says() {
        for (inside, outside) in [
            (
                ["10.0.0.0", "10.255.255.255"],
                ["9.255.255.255", "11.0.0.0"],
            ),
            (
                ["100.64.0.0", "100.127.255.255"],
                ["100.63.255.255", "100.128.0.0"],
            ),
            (
                ["127.0.0.0", "127.255.255.255"],
                ["126.255.255.255", "128.0.0.0"],
            ),
            (
                ["169.254.0.0", "169.254.255.255"],
                ["169.253.255.255", "169.255.0.0"],
            ),
            (
                ["172.16.0.0", "172.31.255.255"],
                ["172.15.255.255", "172.32.0.0"],
            ),
            (
                ["192.168.0.0", "192.168.255.255"],
                ["192.167.255.255", "192.169.0.0"],
            ),
            (
                ["198.18.0.0", "198.19.255.255"],
                ["198.17.255.255", "198.20.0.0"],
            ),
            (
                ["224.0.0.0", "255.255.255.255"],
                ["223.255.255.255", "1.0.0.0"],
            ),
            (
                ["2001::", "2001:1ff:ffff:ffff:ffff:ffff:ffff:ffff"],
                ["2000:ffff:ffff:ffff:ffff:ffff:ffff:ffff", "2001:200::"],
            ),
            (
                ["2002::", "2002:ffff:ffff:ffff:ffff:ffff:ffff:ffff"],
                ["2001:ffff:ffff:ffff:ffff:ffff:ffff:ffff", "2003::"],
            ),
            (
                ["3fff::", "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff"],
                ["3ffe:ffff:ffff:ffff:ffff:ffff:ffff:ffff", "3fff:1000::"],
            ),
        ] {
            for address in inside {
                assert!(!public(address), "{address} is inside a block");
            }
            for address in outside {
                assert!(public(address), "{address} is outside it");
            }
        }
        // An IPv4 address carried in IPv6 is exactly as public as itself, at
        // every edge of every block, which is how an answer from a peer
        // reported in that form is judged.
        for (inside, outside) in [
            (
                ["127.0.0.0", "127.255.255.255"],
                ["126.255.255.255", "128.0.0.0"],
            ),
            (
                ["10.0.0.0", "10.255.255.255"],
                ["9.255.255.255", "11.0.0.0"],
            ),
            (
                ["172.16.0.0", "172.31.255.255"],
                ["172.15.255.255", "172.32.0.0"],
            ),
            (
                ["192.168.0.0", "192.168.255.255"],
                ["192.167.255.255", "192.169.0.0"],
            ),
            (
                ["169.254.0.0", "169.254.255.255"],
                ["169.253.255.255", "169.255.0.0"],
            ),
            (
                ["100.64.0.0", "100.127.255.255"],
                ["100.63.255.255", "100.128.0.0"],
            ),
        ] {
            for address in inside {
                let mapped = format!("::ffff:{address}");
                assert!(!public(&mapped), "{mapped} is inside a block");
            }
            for address in outside {
                let mapped = format!("::ffff:{address}");
                assert!(public(&mapped), "{mapped} is outside it");
            }
        }
        // IPv6's public space is 2000::/3 and nothing else.
        assert!(!public("1fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"));
        assert!(public("2000::1"));
        assert!(public("3fff:1000::1"));
        assert!(!public("4000::"));
    }
}
