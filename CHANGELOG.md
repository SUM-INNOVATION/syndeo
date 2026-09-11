# Changelog

The release workflow reads the section matching the tag and puts it in the
release notes, so a heading here is `## <version>` and nothing else.

## 0.1.0

First release. Six binaries: `syndeo`, `syndeo-net`, `syndeo-keystore`,
`syndeo-agent`, `syndeo-proxy` and `syndeo-ui`.

- **Cache.** RFC 9111 policy with a conformance suite, a versioned redb index,
  BLAKE3-addressed and deduplicating blobs, Subresource Integrity, ranges in
  both directions, `HEAD` answered from a stored `GET`, and a size bound.
- **Network.** The only process that opens a socket: hyper and rustls over
  TCP, HTTP/3 over QUIC behind `Alt-Svc`, hickory DNS, verification through the
  platform verifier, streamed bodies, and stale-while-revalidate that actually
  revalidates.
- **Keystore.** One operation — sign what the shell confirmed. The seed is
  sealed twice, by Argon2id over a passphrase and by a wrapping key the
  operating system holds; it is forgotten after five idle minutes, on sleep and
  on screen lock. SLIP-0044 coin type pinned at 8848 with a committed test
  vector over the derivation path.
- **Agent.** Confined by Seatbelt on macOS and Landlock on Linux, with no
  keystore socket and no session secret. WebAssembly tools run in wasmtime with
  no imports at all.
- **Peer fetch.** libp2p over a private Kademlia DHT, where a request names a
  hash and nothing else, and a body that does not hash to it is discarded.
- **Renderer.** Servo embedded behind the `renderer` feature, with every
  resource load answered by the network process. Not in the release tarballs:
  it is about 1,200 crates to build.
- **Window.** `syndeo-ui`, on winit, wgpu and egui, with the accessibility tree
  published from the start and the signing dialog the shell exists for.

Known limits are in the README under "Known gaps", and Windows and ChromeOS are
[#17](https://github.com/SUM-INNOVATION/syndeo/issues/17).
