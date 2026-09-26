# Changelog

The release workflow reads the section matching the tag and puts it in the
release notes, so a heading here is `## <version>` and nothing else.

## 0.1.3

A release of repairs. A review before publishing found a vulnerable TLS
library, a renderer the installer never installed, a proxy that could be
made to loop or to run one site's page as another's, a keystore setup that
could overwrite the key another home's seed depended on, and several things
the documentation said that were not true. This fixes them and says what is true instead.

**Before you upgrade**

- **The macOS binaries are not signed or notarized** — as with every release
  so far. Installed with the one-line installer they are not quarantined and
  run. An archive downloaded in a browser is quarantined, and Gatekeeper
  rejects its unsigned executables. Unsigned, the keystore cannot reach the
  data protection keychain, so Secure Enclave presence is not enforced: it uses
  the ordinary keychain, and the passphrase is mandatory.
- **The proxy's cache moves** to `<SYNDEO_HOME>/proxy/cache` and starts empty.
  The command-line cache at `<SYNDEO_HOME>/cache` is untouched.
- **If you unpacked the 0.1.2 archive by hand and ran its `syndeo-webkit`,
  upgrade.** Its proxy followed redirects itself, so a page could run as the
  site that redirected to it. The installer never installed that renderer.

**Security**

- **rustls 0.23.45**, for RUSTSEC-2026-0285: TLS 1.3 handshake messages were
  accepted at the wrong encryption level. It is the TLS under every connection
  that ships.
- **The proxy hands redirects to the browser.** It used to follow them and
  answer the first URL with the destination's page, so the destination ran as
  the site that redirected to it, and any cookie the redirect set was lost.
- **A request that comes back to the proxy is stopped** with 508 Loop
  Detected — whatever name it used to get there — instead of looping until the
  machine ran out of connections. Forwarded requests now carry
  `Via: 1.1 syndeo`, so a site can tell they came through Syndeo's proxy.
- **The proxy's certificate authority key is private from its first byte**:
  created 0600 in a 0700 directory whatever the umask, loose modes on an
  existing key tightened before it is read, and symlinks refused.
- **One response header can no longer stop HTTPS.** An `Alt-Svc` max-age too
  large to add to the clock panicked with a lock held, and every HTTPS request
  after it failed until restart. Now clamped to thirty days, and the lock
  recovers.
- **The keystore never enrols over a wrapping key another home depends on.**
  The operating system entry is shared by every `--home`, and `init` in a second
  one used to replace the first one's key; it now refuses, and says that
  `syndeo-keystore restore` is the deliberate way to replace it. The keystore
  socket no longer accepts `Initialize` or `Restore` at all.
- **A signing request is checked before anyone is asked about it.** The origin
  must be a plain http or https origin, and is put in the canonical form the
  key is derived from; the description may not contain control or formatting
  characters; the payload is shown in full, or the request is refused. The
  window and the terminal show the same lines, and what is shown is what is
  confirmed and signed. `syndeo sign` checks all of this before it starts a
  keystore or asks for a passphrase, and prints the canonical origin. So a
  typed message longer than 200 characters, or containing a newline, is now
  refused, as is an origin with a path.

**Fixes**

- `syndeo-webkit` is installed on macOS from 0.1.3 on, beside `syndeo-proxy`.
  Installing an earlier version installs the core binaries only, and removes a
  newer `syndeo-webkit` once that install has succeeded.
- `syndeo-webkit` stops the proxy it starts, however it exits, and refuses to
  start one where the port is already taken.
- The proxy has its own cache, so `syndeo-webkit` and `syndeo browse` can run
  at the same time.
- `syndeo-servo` is no longer in the release tarballs, and the release build no
  longer compiles it. It stays in the source tree, and now says on `--help` and
  every time it starts that it is experimental and unsafe for untrusted sites.
- The weekly report on the renderer's dependencies runs again; it had never
  run. The advisory gate on what ships is unchanged.
- `ci/verify-release.sh` knows which binaries each version and platform should
  have, checks the signing state it is told to expect, and asks Gatekeeper
  about every binary.

**Corrected**

- The 0.1.2 notes and the README said `syndeo-webkit` could not get past the
  proxy, as though a sandbox or the kernel held it there. It is configured to
  send its traffic through syndeo-proxy, with the proxy's certificate pinned —
  a configuration, not a sandbox. Corrected in place in the 0.1.2 section.
- The README said macOS releases were signed and notarized. None has been.
- Cache partitioning applies to what goes through `syndeo-net` directly, not
  to the proxy or `syndeo-webkit`.
- `syndeo-proxy ca --trust` said macOS would ask for consent. It does not;
  the command asks for `yes` itself.

**Known, and not fixed here**

- A cacheable response that sets a cookie is stored with its `Set-Cookie` and
  replays it on a cache hit.
- A cache entry whose stored body is missing or corrupt fails that URL until it
  is evicted, instead of fetching it again.
- WebSockets do not work through the proxy, and whether WebKit sends WebRTC
  traffic outside its proxy setting has not been measured.

## 0.1.2

`syndeo-webkit`: a renderer that plays video, in tabs, with every byte still
going through our own cache. macOS only, and in the tarball for the first time.

Servo cannot play a video and was never going to — its script engine contains
one mention of `MediaSource` and it is a `TODO` — so this embeds WebKit
instead, and keeps boundary one by configuration: the web view is configured
to send its traffic through `syndeo-proxy`, with the proxy's certificate pinned.
(Corrected in 0.1.3. This paragraph first said the web view could not get past
the proxy, as though a sandbox held it there; it is a configuration.)
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
weaker claim — configured to send its traffic through syndeo-proxy, with the
proxy's certificate pinned. (Corrected in 0.1.3: this line first credited a
sandbox with keeping it there.)

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
