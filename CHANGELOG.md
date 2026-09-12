# Changelog

The release workflow reads the section matching the tag and puts it in the
release notes, so a heading here is `## <version>` and nothing else.

## 0.1.2

`syndeo-webkit`: a renderer that plays video, in tabs, with every byte still
going through our own cache. macOS only, and in the tarball for the first time.

Servo cannot play a video and was never going to — its script engine contains
one mention of `MediaSource` and it is a `TODO` — so this embeds WebKit
instead, and keeps boundary one by containment rather than by asking the engine
nicely: the web view is pointed at `syndeo-proxy` and cannot route around it.
Measured on a YouTube watch page, sequential DASH segments through our cache:

    videoplayback?...&rn=15   200  1678ms
    videoplayback?...&rn=16   200   360ms
    videoplayback?...&rn=17   200   650ms
    videoplayback?...&rn=18   200   159ms

- **Tabs.** Command-T opens, Command-W closes, Command-[ and Command-] cycle,
  Command-1 to 9 jump. Hidden rather than destroyed, so a tab keeps its scroll
  position, its heap and its playing video — and all of them share one process,
  where Servo paid a whole browser's fixed cost per page.
- **One command.** It starts its own proxy and uses that proxy's authority, so
  `syndeo-webkit <url>` is the whole of it after a one-time
  `syndeo-proxy ca --trust`.
- **The authority is per-user**, SSL-policy only, no `sudo`, and
  `syndeo-proxy ca --untrust` removes it. `--trust` asks first, because macOS
  does not prompt for a trust setting in your own login keychain — a browser
  that added a root silently would be one you should not run. It is needed
  because WebKit validates subresources in its networking process, which never
  consults an in-process pinned anchor: without it a page loads its document
  and silently drops the other seventy-odd resources.
- **Memory, measured properly.** One window on a YouTube watch page costs about
  1051 MB against Safari's 1293 MB, and rust-lang.org 43.6 MB of WebContent
  against 46.2. Two Safari figures published earlier, 50 MB and 90 MB, were
  wrong in our favour: one summed a Safari tab with ours, the other read a
  page's small iframe process instead of its main frame.
  `ci/measure-memory.sh` does it carefully now.
- **CI is green**, which took admitting that setting `CC` did nothing because
  mozangle calls unversioned `clang`, and that `update-alternatives --install`
  also did nothing because `/usr/bin/clang` is a real file on those runners.
  A symlink, and an assertion that fails in the first minute rather than forty
  minutes into Servo.

Unchanged and still true: Servo remains in the tree as the only configuration
where a renderer provably opens no socket at all, and `syndeo-webkit` is the
weaker claim — one socket, to loopback, with the sandbox making it the only one.

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
