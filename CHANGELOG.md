# Changelog

The release workflow reads the section matching the tag and puts it in the
release notes, so a heading here is `## <version>` and nothing else.

## 0.1.4

A release of repairs, from a review of 0.1.3 after it was published. Plain
`http://` pages failed in `syndeo-webkit`; a proxy tunnel could reach an
origin other than the one it named; cookies were cached and replayed to the
next client; peers could fetch any cached body; declared integrity was not
checked before a body was stored; a page could write escape sequences to the
terminal, or hold the parser for as long as it liked; and `syndeo doctor`
described protection the session did not have. This fixes them, and says
what is still true.

**Before you upgrade**

- **The macOS binaries are still not signed or notarized.** Installed with the
  one-line installer they are not quarantined and run; an archive downloaded
  in a browser is quarantined and Gatekeeper rejects it. Unsigned, the
  keystore cannot reach the data protection keychain, so Secure Enclave
  presence is not enforced and the passphrase is mandatory.
- **`syndeo-webkit` needs the 0.1.4 `syndeo-proxy` beside it.** A 0.1.3 proxy
  refuses the options the 0.1.4 browser starts it with, and the browser stops
  with "the proxy exited before it was ready (exit status: 2)". The installer
  always installs the two together.
- **`syndeo-webkit` still sends requests for localhost and loopback addresses
  directly**, not through the proxy. This was seen while testing 0.1.4, is not
  new, and is not changed by it; see *Limits*.

**Security**

- **A `CONNECT` tunnel is held to the origin it named.** Inside a tunnel the
  proxy fetched whatever absolute URL a request gave, so a tunnel opened to one
  site could fetch from another. The tunnel's authority is now the only
  target, and the `Host` header is never consulted. A `CONNECT` without a
  port is refused rather than taken to mean 443.
- **The proxy `syndeo-webkit` starts answers only that browser.** It used to
  listen on 127.0.0.1:8899 with no credential, and the browser took anything
  that answered on that port for its proxy. The browser now starts it on a
  port the system picks, learns the address from the proxy itself, and hands
  it a credential generated for that launch; every request to the proxy has
  to carry it, or gets 407 and reaches nothing. The credential is in no
  argument, environment variable or log line. A `syndeo-proxy run` started by
  hand asks for no credential, as before.
- **The cache never stores or replays cookies.** A cacheable response that set
  a cookie was stored with it and replayed the cookie on every hit, to
  whichever client asked next. Now `Set-Cookie` is never stored, and never
  served from an entry 0.1.3 wrote. A 304's cookies go to the client whose
  request it answered, once, and are stored nowhere.
- **Peers are served only bodies checked against a hash a page declared.** A
  peer could ask for any cached body by its content address or by digests the
  store computed for every body, private responses included, which told it
  where you had been. Now a body is shared only once it has been checked
  against a declared hash, and only under the matching hashes of the strongest
  algorithm declared; those are the only ones announced. A body received from
  a peer is used for the request and not kept, so it is no longer announced
  again.
- **Declared integrity is checked on every path, before anything is stored,
  shared or returned**, including responses from the cache, which were never
  checked. A cached body that does not match is dropped and fetched once more.
  A declaration that names no usable hash — empty, or only unknown algorithms
  — is now refused, where it used to count as no declaration; that is stricter
  than a browser's `integrity` attribute, on purpose. So is a declaration on a
  range request or on a method other than GET.
- **Nothing a page supplies reaches the terminal as a control or format
  character.** `syndeo browse` and the agent printed page titles, text, links
  and attributes as they came, escape sequences included, so a page could
  move the cursor, clear the screen or set the clipboard. `--json` is
  unchanged: JSON escapes them itself. The check that refuses such characters
  in a signing request is unchanged.
- **The agent cannot learn your identity at a site without asking you.** It
  could ask the shell for the public key and address at any origin, and got
  them without anyone being asked, which could link identities that are
  derived per site so that they cannot be linked. The shell now checks the
  origin and asks you; the keystore is asked only if you say yes, and a run
  with nobody to ask declines.
- **A page cannot make Syndeo hold an unbounded body.** `syndeo browse`, the
  agent, the window and Servo collect each response whole, and the network
  process streams whatever an origin sends, with or without a length, so an
  origin could send until memory ran out. A whole-body fetch now accepts at
  most 64 MiB: a declared `Content-Length` past it is refused before any of
  the body is read, and a body without one is refused at the piece that
  crosses it. The connection is closed on refusal, which stops the network
  process sending and releases the origin's connection. The proxy streams
  and is not affected.
- **Parsing a page is bounded.** Some markup costs html5ever far more than its
  size: nesting, a tag with many attributes, repeated `<html>` tags, text
  moved out of a table. Every page is now parsed against a fixed budget of
  work, and one that would cost more is parsed only as far as the budget goes
  (see *Changed*).
- **Passphrases are wiped after use.** A scripted `SYNDEO_PASSPHRASE` and the
  keystore's session secret are taken out of the environment before any
  thread exists. Passphrases and recovery phrases are held in memory that is
  wiped when dropped, through the prompts, the IPC messages and their frame
  buffers, in the shell, the window and the keystore. Copies made inside
  libraries — the terminal password reader, the JSON decoder's scratch buffer
  for a string it had to unescape, the window's text field — are not wiped.
- **A used signing confirmation is remembered until it expires**, not until
  4,096 others had been used; at most 65,536 are outstanding at once.
- **The proxy limits request bodies**, to 64 MiB by default, set with
  `--max-request-body`. A larger body is refused with 413 before any of it
  reaches the origin. It keeps the 1,024 most recently used leaf certificates,
  where it kept every one for the life of the process.

**Fixes**

- **Plain `http://` pages load in `syndeo-webkit`.** Every one of them failed in
  0.1.3: WebKit sends them down a `CONNECT` tunnel as plain HTTP, and the proxy
  started a TLS handshake inside every tunnel. It now reads what a tunnel
  carries from its first byte.
- **`syndeo doctor` reports what is true.** 0.1.3 printed a fixed list on every
  platform: that the seed was forgotten on idleness, on sleep and on screen
  lock, and that renderers reach the network only through the net process. It
  now asks the running keystore whether a seed exists, whether it is in memory,
  its idle timeout, and whether this session reports a screen lock, and says
  only what holds. It names `syndeo-webkit` as outside the net process, with
  its loopback bypass.
- **`syndeo browse --json` prints JSON.** The network process and the keystore
  logged to the shell's standard output, so their log lines were mixed into
  what `browse` printed, and `--json` was not valid JSON. They log to standard
  error now.
- **A lost or corrupt cached body is fetched again**, once, instead of failing
  that URL until the entry was evicted. A blob can no longer be deleted while
  a store of the same bytes is committing.
- **Two writes of the same body at once no longer truncate each other**: every
  write has its own temporary file, and what a crashed process leaves behind
  is swept when the cache opens.
- **A request is not sent twice when HTTP/3 fails.** Any failed HTTP/3 attempt
  was retried over TCP; now only one that sent nothing, or an idempotent one.
- **`stale-if-error` is honoured as a window.** A response was served stale on
  error however stale it was, even when it said `no-cache` or
  `must-revalidate`. It is served only within the window the directive gives,
  and never where a directive forbids it.
- **A supervised process that exits before it is ready is reported at once**,
  with its exit status, instead of after a ten-second wait. On Linux, exit
  status 127 adds that a shared library could not be loaded, usually
  `libdbus-1-3` for the keystore.
- **`syndeo-keystore init` checks everything that could refuse before asking
  for a passphrase**, so nobody chooses one for an enrolment that was never
  going to happen.
- **`syndeo-proxy ca --untrust` removes exactly this home's authority**, by its
  SHA-256 fingerprint, and says it is gone only once a fresh listing agrees.
  With no local authority it no longer creates one; it lists what is in the
  login keychain by that exact name and asks before deleting any.
- **`syndeo-webkit` checks the trusted authority by fingerprint.** A leftover
  certificate with the same name and a different key passed the old check,
  and then every https page failed with nothing said.
- **The agent says when Landlock is only partly in force.** A kernel that
  applied part of the ruleset was reported as confined. The agent now logs a
  warning, "partially confined", and names what that kernel's Landlock cannot
  restrict: below ABI 4, TCP; below 3, truncation; below 2, renaming or
  linking across directories.
- **`--shared=false` and `--trace-requests=false` work.** Both were on with no
  way to turn them off.
- **A deeply nested page no longer overflows the stack.** Every walk over a
  parsed page keeps a stack of its own instead of recursing. Hostile nesting
  in a page is cut short by the parser's work budget, and what was parsed of
  it is read iteratively.
- **The installer names the right file for your shell** when the install
  directory is not on `PATH` — `.zshrc`, `.bash_profile` on macOS, `.bashrc`
  on Linux, `fish_add_path` for fish, `.profile` otherwise — and on Linux
  warns when the keystore cannot start, and what to install.

**Changed**

- **Bare `syndeo-proxy` resolves over DNS-over-HTTPS**, like `syndeo-proxy run`
  and the README always said. It used the system resolver.
- **A page that would cost more than the parser's budget is cut short.**
  `syndeo browse` says so, and how much was parsed; `--json` adds a
  `cut_short` field, `{"parsed": <bytes>, "of": <bytes>}`, or `null` for a page
  parsed whole; the agent and the window say so too. An ordinary 16 MiB page is parsed whole
  with room to spare. Three different limits are involved, and none stands in
  for another: the parser's budget bounds the work of parsing a body that has
  arrived; the whole-body ceiling, 64 MiB, bounds how much of a response is
  collected to be parsed at all; and the network process's `max_body_bytes`,
  also 64 MiB by default, bounds only what it buffers to cache a response or
  to check its declared integrity — a larger response still streams, it is
  just not cached.
- **Page text is grouped into blocks differently**, valid pages included.
  Each piece of text now belongs to the nearest block element around it, once.
  0.1.3 also gave a table one block holding all of its text, beside the blocks
  inside it, so that text appeared twice; that combined block is gone.
- **Single-line fields are printed with every kind of whitespace as one
  space**: tabs, line breaks, no-break and other Unicode spaces, with runs
  collapsed, so padded columns line up.
- **`syndeo-webkit`'s help and start-up log say what the proxy does not
  cover**: localhost and loopback addresses, and transports WebKit does not
  send through its proxy setting.

**For code built on these crates**

- `NetError::LostBody` and `NetError::Integrity` are new.
- `Net::announcements()` counts the hashes handed to the swarm to announce.
- Cache statistics gain `corrupt_entries` and `swept_temporaries`.
- The cache policy's `stale_if_error` is the number of seconds a response may
  still be served stale, or `None` when it may not be, rather than the
  directive's raw value.
- `Document::cut_short()` and `syndeo_dom::WORK_BUDGET`.
- `Framed::fetch` takes the connection and closes it however it returns, and
  refuses a body past `syndeo_ipc::frame::MAX_WHOLE_BODY` with
  `FrameError::BodyTooLarge`; `Framed::fetch_within` takes the ceiling as an
  argument.
- The keystore protocol gains a `SessionProtection` request and response,
  appended after every existing message; a 0.1.3 keystore closes the
  connection on it. Every existing message encodes byte for byte as in 0.1.3,
  which a test pins. Passphrase and recovery-phrase fields are now
  `SecretString`, which encodes exactly as the string it holds.

**Limits, stated rather than fixed**

- `syndeo-webkit` sends requests for localhost and loopback addresses
  directly, not through the proxy; WebRTC's transports are not measured;
  WebSockets do not work through the proxy.
- A signature does not say what it is for: the purpose is bound into the
  shell's confirmation, not into the signature.
- The seed is forgotten on screen lock only on macOS, and only in a session
  that reports it — not over ssh or as a daemon, and never on Linux. Sleep and
  the idle timeout apply everywhere.
- A plain `syndeo-proxy run` asks for no credential. Neither proxy limits how
  many connections it accepts.

**What was and was not tested**

- Linux: CI on Ubuntu 22.04, x86_64. Not tested: a bare Debian host, a Linux
  desktop session, a running Landlock kernel.
- macOS: CI drives a real WKWebView, on the default data store of a runner
  that is thrown away, through a stand-in proxy that asks for the credential,
  and finds and deletes certificates in a throwaway keychain. Not tested: a
  page loaded through `syndeo-proxy` with its authority trusted in a real
  login keychain, the fingerprint check against a real login keychain,
  changing trust settings (`ca --trust` and `--untrust` against the login
  keychain), and a screen lock reported in a real graphical session.
- The end-to-end run on macOS in a clean, temporary user account — the
  installer, the release verifier, and the smoke tests of the window, the
  WebKit browser and video — was waived by the owner. It is untested, not
  passed.

**Corrected**

- The 0.1.3 and 0.1.2 notes said `syndeo-webkit`'s HTTP and HTTPS loads, or
  its traffic generally, went through the proxy. Only its HTTPS loads did:
  plain HTTP loads failed in both releases, and requests for localhost and
  loopback addresses went directly. Rewritten in place in both sections.
- The README said the keystore forgets the seed when the screen locks, on
  every platform. It does only on macOS, in a session that reports it.
- The crate documentation of `syndeo-ipc` and `syndeo-net` said renderers
  never talk to the network, without saying which renderers: those the
  process model serves — `syndeo-ui`, `syndeo-servo` and the agent — and not
  `syndeo-webkit`.

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
- The README, the 0.1.2 notes, and `syndeo-webkit`'s own help and log said
  every byte of its traffic went through the proxy, and nothing else. Its
  HTTPS loads did. Its plain HTTP loads failed entirely, in this release and
  in 0.1.2: the proxy began a TLS handshake inside every tunnel. Requests for
  localhost and loopback addresses went directly, never to the proxy. And
  what WebKit sends outside its proxy setting was not covered; WebRTC has not
  been measured. (Corrected in 0.1.4, which found the last three; this entry
  first said its HTTP and HTTPS loads went through the proxy.)
- The README, SECURITY.md and a CI comment said `syndeo-servo` was in no
  release. v0.1.1 and v0.1.2 included it; it is out of the release archives
  from v0.1.3 on.
- The README and the `syndeo-servo` crate documentation said its resource
  loading streams. It does not: each response is fetched whole and handed to
  Servo in one piece, with no size cap.

**Known, and not fixed here**

- A cacheable response that sets a cookie is stored with its `Set-Cookie` and
  replays it on a cache hit.
- A cache entry whose stored body is missing or corrupt fails that URL until it
  is evicted, instead of fetching it again.
- WebSockets do not work through the proxy, and whether WebKit sends WebRTC
  traffic outside its proxy setting has not been measured.

## 0.1.2

`syndeo-webkit`: a renderer that plays video, in tabs, with its HTTPS loads
going through our own cache. macOS only, and in the tarball for the first time.
Its plain HTTP pages failed, because the proxy expected TLS inside every
tunnel, and its requests for localhost and loopback addresses went directly,
never to the proxy. (Corrected in 0.1.3 and again in 0.1.4: this line first
said every byte, then its HTTP and HTTPS loads.)

Servo cannot play a video and was never going to — its script engine contains
one mention of `MediaSource` and it is a `TODO` — so this embeds WebKit
instead, and keeps boundary one by configuration: the web view is configured
to send its HTTP and HTTPS traffic through `syndeo-proxy`, with the proxy's
certificate pinned. What that covered was its HTTPS loads. Plain HTTP loads
failed at the proxy, localhost and loopback requests went directly, and
transports WebKit does not send through its proxy setting were not covered.
(Corrected in 0.1.3 and 0.1.4. This paragraph first said the web view could not
get past the proxy, as though a sandbox held it there; it is a configuration,
with the exceptions above.)
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
weaker claim — configured to send its HTTP and HTTPS traffic through
syndeo-proxy, with the proxy's certificate pinned, which in this release
covered its HTTPS loads and not its plain HTTP ones, which failed, or its
localhost and loopback requests, which went directly. (Corrected in 0.1.3 and
0.1.4: this line first credited a sandbox with keeping it there, and said its
traffic generally went through the proxy.)

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
