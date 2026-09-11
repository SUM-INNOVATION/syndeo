# Syndeo

A browser built cache-first, with the network, the keys and the agent in
separate processes.

The cache is the product. Everything else is arranged so the cache can be
swapped, shared, or fed from a peer without anything above it noticing.

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

## Running it

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

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
What is worth knowing before you rely on any of this:

Everything filed is closed. What remains is a set of honest limits rather than
open work:

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

197 of them. Thirty-seven cite the RFC 9111 section they cover.

```sh
cargo test --workspace                  # does not build Servo
cargo build -p syndeo-servo --features renderer
```
