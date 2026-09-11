# Changelog

The release workflow reads the section matching the tag and puts it in the
release notes, so a heading here is `## <version>` and nothing else.

## 0.1.1

Fixes found by running it against the web rather than against tests.

- **Every Google-operated host failed through the proxy.** It forwarded the
  client's connection headers to the origin, which HTTP/2 makes a protocol
  error; strict servers answered with a stream error instead of a page. Google
  is strict, GitHub is not, so it looked selective. google.com, youtube.com and
  a YouTube watch page all went from 502 to 200.
- **Transport errors carry their cause.** hyper's error stopped at "client error
  (SendRequest)", which names the call and not the reason. The whole source
  chain is reported now, which is what identified the header bug.
- **Scrolling costs one frame, not one per event.** Wheel deltas are summed and
  handed over once per turn of the event loop. At trackpad rate that is 300
  events to 63 composites, against one composite each before.
- **Back and forward exist**, by sideways swipe, Command-arrow, Command-bracket
  and the two side mouse buttons. There is still no button bar.
- **`data:` URLs are no longer cancelled**, so inline SVG icons render.
- **Pages in non-Latin scripts are no longer tofu** — a system font is borrowed
  for the scripts egui's bundled fonts do not cover.
- **Child processes exit when the shell does**, however it dies. A force-quit
  used to leave the network process running, holding the cache.
- **A release profile**, which the tree never had: thin LTO takes the renderer
  from 143 MB to 127 MB and resident code from 37.5 MB to 24 MB.
- **The cache is partitioned by top-level site**, DNS goes over HTTPS by
  default, and unreferenced blobs are collected hourly. See the README.

Known, and documented rather than hidden: Servo cannot play video — its script
engine has no Media Source Extensions — and a YouTube page costs it 789 MB
against Safari's 90 MB. `syndeo-webkit` is an unfinished answer to both and is
not in this release.

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
