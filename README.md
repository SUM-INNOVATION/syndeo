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
| `syndeo-shell` | the `syndeo` binary: owns the process model and prompts the user |
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
   browser. — *done, h1 and h2; QUIC and HTTP/3 not yet wired (#3)*
4. **Embed Servo**, replace its net crate with this one, run servoshell's UI
   as-is. — *not started (#4)*
5. **Split the process model out properly.** Keystore, then agent. — *done, ahead
   of step four, because the boundaries are cheaper to draw before there is a
   renderer to draw them around*
6. **Replace the shell UI** with our own. — *not started; the shell is a terminal (#5)*

The agent-first alternative to step four — a headless DOM rather than pixels — is
`syndeo-dom`, and it is what the agent reads today.

## Running it

```sh
cargo build --release
export PATH="$PWD/target/release:$PATH"
```

### Read a page

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
  than by our code. Reaching those attributes needs a direct Security.framework
  binding that the `keyring` crate does not expose, so **this is not enforced in
  this build**, and `syndeo_keystore::presence::available()` returns false rather
  than claiming a guarantee the Secure Enclave is not making.
- **Shell confirmation** — the user agreeing to a specific payload. This is
  enforced today, by our own code, and tested.

Because the platform cannot enforce presence here, the passphrase is mandatory
rather than optional. That is the rule for platforms that cannot enforce
presence, applied to this one. When the binding lands, `available()` returns true
and the requirement relaxes on its own; nothing else changes. Tracked in #1.

The SLIP-0044 coin type in `derive.rs` is provisional. It has to be pinned
before anyone holds a balance at an address this derives, because changing it
afterwards strands funds. Tracked in #2.

## Licensing

`cargo-deny` runs with an allowlist from the first commit, because the failure
that actually bites is discovering a GPL transitive dependency six months in.
MPL-2.0 is pre-authorised for Servo, Stylo and SpiderMonkey at step four: MPL is
file-level copyleft, so modifications to their files get published and the
surrounding code does not, which GPL would not have allowed. OpenSSL is banned
outright; rustls is the only TLS in the tree.

```sh
cargo deny check licenses bans sources
```

## Known gaps

Everything that is missing or deferred is filed rather than left in a comment.
The ones worth knowing before you rely on any of this:

| | |
| --- | --- |
| #1 | Secure Enclave presence is not enforced; shell confirmation stands in for it |
| #2 | The SLIP-0044 coin type is provisional |
| #7 | `stale-while-revalidate` serves stale but never refreshes |
| #10 | The cache has no eviction policy and no size bound |
| #12 | Peer discovery is bootstrap-only, so peer fetch is not usable between machines yet |
| #13 | Response bodies are buffered whole and capped at 64 MiB |
| #15 | The cache index has no schema version |

## Tests

```sh
cargo test --workspace
```

124 of them. Thirty-seven cite the RFC 9111 section they cover.
