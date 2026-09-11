# Syndeo

A browser built cache-first, with the network, the keys and the agent in
separate processes.

The cache is the product. Everything else is arranged so the cache can be
swapped, shared, or fed from a peer without anything above it noticing.

## Install

macOS on Apple Silicon, or Linux on x86_64 or arm64:

```sh
curl -fsSL https://raw.githubusercontent.com/SUM-INNOVATION/syndeo/main/install.sh | sh
```

It downloads the release built for your machine, checks it against the published
`SHA256SUMS`, and puts the binaries in `~/.local/bin`. No `sudo`, and nothing
written outside your home directory. `SYNDEO_INSTALL_DIR` moves them;
`SYNDEO_VERSION` pins a version.

They all have to live in the same directory. The shell starts the network
process and the keystore by looking beside itself, which is what keeps a build
tree and an install both working without either being told where the other is.

Then:

```sh
syndeo browse https://www.rust-lang.org/ --twice   # the second fetch says cache
syndeo doctor                                      # what is set up, and where
```

Piping a script into a shell is worth being unhappy about. The script is
[`install.sh`](install.sh) — read it first, or skip it: the tarballs and the
`SHA256SUMS` covering them are on the
[releases page](https://github.com/SUM-INNOVATION/syndeo/releases), and
unpacking one yourself does the same thing.

### What it needs

- **macOS 13 or later, Apple Silicon.** Intel Macs have no prebuilt release;
  they build from source.
- **Linux with glibc 2.35 or later** — Ubuntu 22.04, Debian 12, Fedora 36, and
  anything since. `syndeo-ui` additionally wants a Wayland or X11 session and a
  GPU that Vulkan or GL can reach.
- **A Secret Service implementation on Linux** — gnome-keyring or KWallet —
  before `syndeo-keystore init` will work, because the wrapping key is never a
  file we wrote. Everything that is not the keystore runs headless.
- **Windows and ChromeOS**: not yet, and tracked at
  [#17](https://github.com/SUM-INNOVATION/syndeo/issues/17).

macOS releases are signed and notarized, so a browser download is not
quarantined. `syndeo-keystore status` reports whether that build reaches the
data protection keychain, which is what decides whether Touch ID is enforced by
the Secure Enclave or the passphrase is mandatory instead.

### Verifying a download yourself

```sh
curl -fsSLO https://github.com/SUM-INNOVATION/syndeo/releases/latest/download/SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
```

## The three boundaries

These are load-bearing. Get them right at the start and everything else is
refactorable; get them wrong and no amount of later work recovers it.

1. **Renderers never talk to the network.** They send a `NetRequest` and receive
   bytes. Sockets, DNS, certificates and the cache all live behind that one call,
   in `syndeo-net`.
2. **The agent never talks to the keystore.** `ShellRequest` has no keystore
   variant, so there is nothing to call. The agent is not told where the keystore
   listens, and it refuses to start if it finds a session secret in its
   environment — a leak surfaces at startup rather than in an incident report.
3. **The keystore exposes exactly one operation**: sign this payload, because the
   shell confirmed it. A signature needs a confirmation carrying a MAC over the
   origin, the purpose, the payload hash and the text the user actually read. The
   shell and keystore share the secret that makes one valid; the agent does not.

## Layout

| Crate | What it is |
| --- | --- |
| `syndeo-cache` | RFC 9111 policy, redb index, BLAKE3-addressed blob store, Subresource Integrity |
| `syndeo-net` | tokio, hyper, rustls, hickory DNS, and the fetch API the rest of the tree uses |
| `syndeo-peer` | libp2p peer fetch, where a request names a hash and nothing else |
| `syndeo-ipc` | framed transport, the typed protocols, and shell-issued signing confirmations |
| `syndeo-keystore` | sealed root secret, SLIP-0010 per-origin derivation, one operation |
| `syndeo-dom` | a headless DOM: prose, links, forms, subresources, integrity metadata |
| `syndeo-agent` | the agent process, sandboxed by what it is given |
| `syndeo-shell` | the process model and the prompt, as a library, plus the `syndeo` command |
| `syndeo-ui` | the windowed shell: winit, wgpu, egui, accesskit |
| `syndeo-servo` | Servo embedded, with its resource loading replaced by the net process |
| `syndeo-proxy` | a local intercepting proxy, to measure the cache on real traffic |

## Build order

Steps one and two are where the product risk lives, and they are the cheapest
steps. Everything from four onward is integration work that is mostly a function
of hours.

1. **Cache crate standalone**, with an RFC 9111 conformance suite, a redb index
   and content-addressed blobs. No browser. — *done*
2. **Wrap it in a local intercepting proxy.** Point ordinary Chrome at it. Measure
   hit rate and dedupe ratio on real traffic. This is the go/no-go. — *done*
3. **Net process**: hyper, rustls, the cache behind a fetch API. Still no
   browser. — *done, including HTTP/3 over QUIC behind `Alt-Svc`*
4. **Embed Servo**, replace its net crate with this one. — *done, behind the
   `renderer` feature; see below for what that costs to build*
5. **Split the process model out properly.** Keystore, then agent. — *done, ahead
   of step four, because the boundaries are cheaper to draw before there is a
   renderer to draw them around*
6. **Replace the shell UI** with our own. — *done; `syndeo-ui` is a window, and
   the terminal front end is still there beside it*

The agent-first alternative to step four — a headless DOM rather than pixels — is
`syndeo-dom`, and it is what the agent reads today.

## Build from source

A stable Rust toolchain and, on Linux, the development packages for D-Bus and
the window system:

```sh
sudo apt-get install -y pkg-config libdbus-1-dev libxkbcommon-dev libwayland-dev \
  libx11-dev libxrandr-dev libxi-dev libxcursor-dev libxinerama-dev \
  libgl1-mesa-dev libegl1-mesa-dev
```

Then:

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

`syndeo-servo` is not in that build, or in any release tarball; see below for
what it costs.

## Running it

### A page, rendered

```sh
cargo build -p syndeo-servo --features renderer     # long; see below
syndeo-servo https://www.rust-lang.org/
```

A real renderer — SpiderMonkey, Stylo, WebRender — that opens no socket. Servo
asks the embedder about every HTTP load through `load_web_resource`, and every
one of them is answered from the network process instead. Nothing is ever handed
back, because a declined load is one Servo would perform itself.

What that is worth, measured rather than asserted: loading `rust-lang.org` makes
78 resource loads, all 78 go through the net process, and on the second visit all
78 come back marked `cache`. While it runs, `lsof` on the renderer shows no
network sockets at all and `lsof` on the net process shows the TCP connections
and the QUIC socket. The renderer inherits the cache, the peer fetch and the DNS
policy without knowing any of them exist.

Registering a protocol handler would have looked tidier and does not work:
Servo's `ProtocolRegistry` refuses `http` and `https` by design. Resource-load
interception is the supported way in front of them, and it streams.

**Building it is the expensive part.** The feature is off by default because
Servo brings SpiderMonkey, Stylo and WebRender: about 1,200 crates and a 450 MB
debug binary. Everything in `syndeo-servo` that could be written and tested
without Servo is outside the gate, in `bridge.rs`.

It also needs a Python ≥3.11 as `python3` on `PATH`, for Servo's WebIDL codegen.
The system Python on macOS is 3.9, and the failure does not name the cause — it
is a `SyntaxError` on a `match` statement, hundreds of crates in. On macOS:

```sh
brew install python@3.12
mkdir -p .python && ln -sf "$(brew --prefix python@3.12)/bin/python3.12" .python/python3
PATH="$PWD/.python:$PATH" cargo build -p syndeo-servo --features renderer
```

Servo's build script looks for `uv` first, which does not help here: the
published crate ships no `uv.lock`, so `uv run --frozen` has no project to run
in and the fallback to `python3` is what actually decides the version.

### The window

```sh
syndeo-ui https://www.rust-lang.org/
syndeo-ui --no-keys https://example.test/   # browse without opening the keystore
```

`winit`, `wgpu`, `egui` and `accesskit` — the stack servoshell uses, so there is
a working reference for the day a renderer needs a surface. The reader pane shows
the headless DOM rather than a rendered page, and says so; step four is what puts
layout behind it. The side panels are the cache counters and the peer ledger.

Accessibility is wired from the start rather than retrofitted: the accessibility
tree is published, and the controls whose visible text is a glyph — back, reload,
the panel toggles — carry explicit names, because `←` is not a name.

### Read a page from the terminal

```sh
syndeo browse https://www.rust-lang.org/ --full --twice
```

The second fetch reports `cache`. `--full` lists links, forms, and every
subresource with whether it declared an integrity hash — which is the same thing
as whether it is eligible for peer fetch.

### Measure the cache on real traffic

```sh
syndeo-proxy ca            # prints the authority path and how to trust it
syndeo-proxy run           # listens on 127.0.0.1:8899

/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
  --proxy-server=http://127.0.0.1:8899 --user-data-dir=/tmp/syndeo-measure
```

Every response carries `x-syndeo-source`: `cache`, `revalidated`, `origin`,
`peer`, `stale-on-error`, or `pass-through`. Statistics are at
`http://syndeo.local/stats` through the proxy, or `syndeo-proxy stats`.

Trust the authority for the duration of the measurement and remove it after. It
exists so an ordinary browser will talk to us before any of the browser is
written, and for no other reason.

### Keys

```sh
syndeo-keystore init                      # shows a recovery phrase once
syndeo identity https://wallet.test       # the key that origin sees, and no other
syndeo sign --origin https://wallet.test --message "transfer 10 SUM" \
  --purpose transaction
```

`syndeo sign` shows the payload and asks. `--yes` takes the invocation itself as
the confirmation, which is legitimate only because the user typed the payload;
it is not reachable from the agent boundary.

### The agent

```sh
syndeo agent "read https://www.rust-lang.org/"
syndeo agent "crawl https://www.rust-lang.org/"
syndeo agent "sign https://wallet.test anything"     # goes in front of a human
```

### Peer fetch

```sh
syndeo browse https://example.test/ --peer on
syndeo browse https://example.test/ --peer /ip4/10.0.0.5/tcp/4001
```

A peer is asked only for a body the page already named by hash. No declared
integrity, no peer request.

To seed rather than browse, run a node that stays up:

```sh
syndeo peer serve                      # prints the address to dial it at
syndeo peer serve --serve-only         # answer, never ask
syndeo --peer <multiaddr> peer status  # who is connected, and what they have given
```

Discovery is Kademlia on `/syndeo/kad/1.0.0` — deliberately not the public IPFS
DHT — so a node reaches peers nobody named to it. Each peer has a ledger of what
it gave and what it took; a node that only takes gets an opening allowance and is
then asked to wait.

What this does and does not buy is in `syndeo-peer`'s crate documentation, in
detail. The short version: a peer never learns a URL from you, but it does learn
which hashes you want and when, and that is not the same as anonymous.

### Tools

The agent can run WebAssembly tools, loaded with no imports at all — no
filesystem, no clock, no sockets — so a tool reaches exactly what it is handed.

```sh
syndeo agent "tools"                                  # what is installed
syndeo agent "tool wordcount https://example.test/"   # run one over a page
```

`crates/syndeo-agent/tools/wordcount.wat` is an example, written as readable text
rather than a binary on purpose.

## Memory, measured against Safari

Same page, same interval, both sides sampled rather than snapshotted:

| YouTube watch page | Syndeo | Safari |
| --- | --- | --- |
| main WebContent | 915 MB | 969 MB |
| second WebContent (the page's iframe) | 36 MB | 46 MB |
| host / browser process | 100 MB | 159 MB |
| **one window, one page** | **~1051 MB** | **~1293 MB** |

| rust-lang.org | Syndeo | Safari |
| --- | --- | --- |
| WebContent | 43.6 MB | 46.2 MB |

The engine costs what Safari's engine costs, because it *is* Safari's engine;
the difference is the host process, where ours is smaller. Safari's fixed
overhead is amortised across its other tabs and ours is not, so past roughly a
dozen tabs the comparison turns around.

Getting this right took three attempts and produced two published numbers that
were wrong, both in our favour, from two mistakes worth naming:

- **Attribution.** WebKit's WebContent processes are XPC services parented to
  `launchd`, so "the new process" is whichever appeared in the window — and with
  Safari open, some of those are Safari's.
- **Picking the wrong process.** A page with an iframe gets two WebContent
  processes, a main frame and a small one for the embed. Reading the small one
  gave "Safari uses 50 MB on YouTube" while its main frame was using 969 MB.

[`ci/measure-memory.sh`](ci/measure-memory.sh) does it properly — every process
printed, ours told from Safari's by container, both sides sampled over the same
interval — so the next person does not repeat it.

## What it does not tell anyone

Privacy here is a set of defaults, not a setting. Each of these is on without
being asked for, and each costs something that is named rather than hidden.

**The cache is partitioned by the top-level site.** A cache keyed on the URL
alone is shared across every site you visit, and that is a way to be tracked: an
advertiser embedded in two places can time a fetch for a resource and learn
whether you have been somewhere it was already loaded — no script, no cookie,
just the difference between eight milliseconds and eighty. Chrome partitioned
its cache in 2020 and Safari before it. The key here is the top-level
document's *origin*, which is stricter than Chrome's registrable domain and
needs no public suffix list to compute.

What that costs is hit rate on third-party resources. What saves it is that the
blob store is content-addressed: two sites loading the same framework hold two
entries and **one copy of the bytes**. Partitioned for privacy, deduplicated
for size — a test asserts exactly that, because it is the claim the trade rests
on. `syndeo-net --unpartitioned-cache` turns it off, and exists so the two hit
rates can be measured against each other rather than argued about.

**DNS goes over HTTPS by default.** The system resolver sees every hostname you
visit, in plaintext, and hands it to whoever runs it. The default is
`doh:cloudflare`; `--dns doh:google`, `doh:quad9` and `dot:cloudflare` are
there, and `--dns system` goes back to the resolver the machine is configured
with. Two honest costs: it points your lookups at one operator instead of your
ISP, which is a different trust rather than none; and it breaks captive portals
and split-horizon corporate DNS until you pass `--dns system`.

**Nothing is reported anywhere.** No telemetry, no analytics, no crash
reporting, no update ping. There is no code in this tree that sends anything
anywhere except the page you asked for.

**Credentials do not follow a redirect across origins.** `Authorization`,
`Cookie` and `Proxy-Authorization` are dropped when a redirect changes origin.

**A peer is asked only for a body the page already named by hash**, and peer
fetch is off unless you turn it on. What it does and does not buy is in
`syndeo-peer`'s crate documentation: a peer never learns a URL from you, but it
does learn which hashes you want and when, and that is not the same as
anonymous.

## Root secret custody

The seed is never a plaintext file. In order of what an attacker has to get past:

1. **Two independent seals.** Argon2id over the user's passphrase seals the seed,
   calibrated at setup to about a quarter second on the machine it runs on. A
   random wrapping key held by the operating system's credential store — Keychain,
   Secret Service, Credential Manager — seals that. The passphrase layer is on the
   inside on purpose: a compromised Keychain yields a blob that is still
   passphrase-protected. Both are XChaCha20-Poly1305.
2. **Filesystem posture.** `~/.syndeo/.keystore/seed.sealed`, directory 0700, file
   0600, hidden, excluded from Time Machine, and audited for symlink, ownership
   and mode before every open — every open, not once at setup.
3. **Per-signature consent.** The shell renders the exact bytes and the user
   agrees to those bytes. The confirmation is single-use, expires in two minutes,
   and is bound to the origin, so consent for one site can never produce a
   signature for another.
4. **A recovery phrase**, BIP-39, shown once at setup and never written down by
   us. Losing both factors is unrecoverable by design, which is precisely why the
   phrase exists.

### On `sudo`

Deliberately not used, and not recommended. A root-owned key file means the
keystore runs privileged or shells out to a setuid helper, which is a far larger
attack surface than the credential store; and macOS caches sudo credentials for
five minutes by default, so it is weaker than a per-operation check anyway. The
correct equivalent of "requires sudo" for a signing key is per-signature user
presence.

### What presence actually means today

Two things provide it, and they are not the same:

- **Platform presence** — the Keychain item carrying
  `kSecAttrAccessibleWhenUnlockedThisDeviceOnly` and a `SecAccessControl`
  requiring `.userPresence`, so Touch ID is enforced by the Secure Enclave rather
  than by our code. The Security.framework binding for this is written, in
  `syndeo_keystore::enclave`, and it is behind the same `WrappingKeyStore` trait
  as everything else.
- **Shell confirmation** — the user agreeing to a specific payload. This is
  enforced always, by our own code, and tested.

Whether the first one is *in force* depends on how the binary was signed. The
attributes only mean anything in the macOS data protection keychain, which is
only reachable from a binary signed with a keychain access group; without one
`SecItemAdd` returns `errSecMissingEntitlement` and the keystore falls back to
the ordinary keychain and reports presence as unenforced — at which point the
passphrase is mandatory rather than optional. `syndeo-keystore status` says which
of the two you are in, and how to change it. The entitlements file is committed
at `crates/syndeo-keystore/Syndeo.entitlements`; a real signing identity is
required, since ad-hoc signing with that entitlement produces a binary the kernel
kills at launch.

The keystore also forgets the seed on its own: after five idle minutes, when the
machine has slept, or when the screen has locked. A signature after that costs a
passphrase prompt rather than the operation.

The SLIP-0044 coin type in `derive.rs` is **pinned** at 8848 and documented as
SUM's by declaration rather than by allocation. A committed test vector — a
published mnemonic in, an address out — guards the whole derivation path, so a
change is caught by the suite rather than discovered by a user whose funds went
somewhere else. SUM Chain's network id, 1, is recorded beside it and marked as
not the coin type.

## Releasing

`.github/workflows/ci.yml` is what every commit has to survive: rustfmt, clippy
as errors, the test suite and `cargo-deny`, on Linux and macOS. Servo is checked
weekly rather than per-commit, because it is an hour of compilation.

A release is a tag:

```sh
# bump [workspace.package] version, add a CHANGELOG.md section, then
git tag v0.1.0 && git push origin v0.1.0
```

Before handing a release to anyone, check the thing that was published rather
than the thing that was built:

```sh
ci/verify-release.sh 0.1.1
```

It installs from the release with the same one-liner the README gives, into a
throwaway directory, and then asks the installed binaries to demonstrate what
has broken before — a cache hit that is served and then forgotten, a proxy that
turns every Google host into a 502, binaries that cannot find each other. Every
check in it exists because something it covers once shipped broken. It exits
non-zero, so it can gate a release rather than decorate one.

`.github/workflows/release.yml` refuses a tag that disagrees with the workspace
version, builds the three targets, signs and notarizes the macOS binaries when
the signing secrets are present, and publishes the tarballs with a `SHA256SUMS`
covering them. The secrets it reads are named and explained at the top of
[`ci/sign-macos.sh`](ci/sign-macos.sh); without them the build still produces
working tarballs and says in the log that it did not sign them.

## Licensing

`cargo-deny` runs with an allowlist from the first commit, because the failure
that actually bites is discovering a GPL transitive dependency six months in.
MPL-2.0 is pre-authorised for Servo, Stylo and SpiderMonkey at step four: MPL is
file-level copyleft, so modifications to their files get published and the
surrounding code does not, which GPL would not have allowed. OpenSSL is banned
outright; rustls is the only TLS in the tree.

The windowed shell added three entries, all permissive and all noted in
`deny.toml` with why: BSL-1.0 for egui's clipboard support, and OFL-1.1 and
Ubuntu-font-1.0 for the fonts egui embeds — font licences rather than code
licences, whose conditions bite on modifying and renaming the font files.

```sh
cargo deny check licenses bans sources
```

## Known gaps

Everything that is missing or deferred is filed rather than left in a comment.
One thing is open — Windows and ChromeOS support,
[#17](https://github.com/SUM-INNOVATION/syndeo/issues/17) — and the rest of this
is a set of honest limits rather than work waiting to be done:

- `syndeo-ui` shows the headless DOM rather than a rendered page; `syndeo-servo`
  is where pixels are. Putting the renderer inside the shell's window means one
  surface shared between egui and WebRender, which is a piece of work in its own
  right and is not this one.
- Servo does not tell the embedder what a page declared as a subresource's
  integrity, so nothing loaded through the renderer is eligible for peer fetch
  yet. It reaches the cache; it just never asks a peer.

Two things are done but not *demonstrated* on an ordinary developer machine, and
both say so where you would meet them:

- Secure Enclave presence needs a signed build (#1's binding is in the tree;
  `syndeo-keystore status` reports which side of that line you are on).
- The agent's Landlock confinement compiles for Linux and has not been exercised
  on a Linux kernel from here. macOS Seatbelt confinement is tested, including a
  case that builds a fixture and holds TCP, UDP, file writes and `exec` to
  failing.

## Tests

```sh
cargo test --workspace
```

198 of them. Thirty-seven cite the RFC 9111 section they cover.

```sh
cargo test --workspace                  # does not build Servo
cargo build -p syndeo-servo --features renderer
```
