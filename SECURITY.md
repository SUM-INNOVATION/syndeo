# Reporting a vulnerability

Use [private vulnerability
reporting](https://github.com/SUM-INNOVATION/syndeo/security/advisories/new).
It goes to the maintainers and to nobody else. Please do not open a public
issue for anything that would let someone reach a key, a seed, or a signature
they were not shown.

You should expect an acknowledgement within three working days and an
assessment within ten.

## What is in scope

This is pre-1.0 software and the whole of it is in scope, but these are the
things worth the most attention, because they are the claims the design rests
on:

1. **Anything that lets a renderer or the agent open a socket**, or reach the
   network other than through `syndeo-net` — or, for `syndeo-webkit`, other
   than through the `syndeo-proxy` it is configured to use (see below for what
   that configuration does not cover).
2. **Anything that lets the agent reach the keystore**, directly or by
   obtaining a session secret or a confirmation it was not issued.
3. **Anything that produces a signature over bytes a user did not see** — a
   confirmation replayed, reused across origins, used after it expired, or
   valid for a payload other than the one displayed.
4. **Anything that reveals the root seed**, including through the sealed file's
   filesystem posture, the wrapping key in the credential store, a process
   dump, or memory that should have been zeroized.
5. **Anything that makes the cache serve bytes that fail their declared
   integrity hash**, or serve one origin's response to another — or one
   client's cookie to another, or a body to a peer that no page declared.
6. **Anything a page can do to the terminal, to parsing or to memory**: a
   control or format character from a page reaching the terminal through
   `syndeo browse` or the agent, a page that costs the parser more than its
   fixed work budget, or a response collected whole past its 64 MiB ceiling.

## What is already known, and is not a finding

These are documented limits rather than undiscovered ones. Reporting them is
welcome as a second opinion; they are not treated as new.

- **An unsigned macOS build does not enforce Secure Enclave presence**, and
  macOS releases through v0.1.6 are ad-hoc signed, without Developer ID or
  notarization. The data protection keychain needs a signed binary with a keychain
  access group. Without one the keystore falls back to the ordinary keychain,
  reports presence as unenforced, and makes the passphrase mandatory instead.
  `syndeo-keystore status` says which side of that line a build is on. A
  browser-downloaded archive is quarantined and Gatekeeper rejects it; the
  one-line installer is not quarantined.
- **The macOS installer package runs two scripts as root.** v0.1.6 is the
  first release to include the package, and the v0.1.6 package is unsigned.
  - Its preinstall refuses to install over anything it cannot account for.
    The reason is normally recorded in `/var/log/install.log`, but is not
    guaranteed to appear there.
  - One version is installed at a time. An upgrade leaves the commands
    unavailable for a moment, and a Syndeo left running from the replaced
    version stops at the next program it needs.
  - Gatekeeper is expected to reject the unsigned package when it comes from
    a browser download. The documented route downloads it with `curl` and
    checks it against `SHA256SUMS`.
  - Installing it from Finder is untested.
- **A peer learns which hashes you want, and when.** It never learns a URL, and
  that is not the same as anonymous. `syndeo-peer`'s crate documentation is
  explicit about it.
- **The Landlock confinement is compiled but not yet exercised on a running
  Linux kernel.** Where a kernel applies only part of the ruleset, the agent
  reports itself partially confined and names what that kernel cannot
  restrict. The macOS Seatbelt path is tested, including a fixture that holds
  TCP, UDP, file writes and `exec` to failing.
- **A signature does not say what it is for.** The purpose a request states is
  shown and bound into the shell's confirmation, but the signature is a plain
  ed25519 signature over the payload, so a verifier cannot tell a login from a
  transaction.
- **The keystore forgets the seed on screen lock only on macOS, and only in a
  session that reports it.** Over ssh or as a daemon, macOS does not report the
  screen lock; Linux never does. Sleep and the idle timeout apply everywhere.
  `syndeo doctor` says which hold for the running keystore.
- **`syndeo-servo` is experimental, for development only, and unsafe for
  untrusted sites.** It does not enforce cross-origin reads, so a page can read
  other origins' responses, including services on the machine and its network;
  it sends form POST bodies empty; and it buffers every response completely,
  with no size cap. It prints those reasons each time it starts, and gives
  them on `--help`. It is behind an off-by-default `renderer` feature and is
  not in the release archives from v0.1.3 on. The v0.1.1 and v0.1.2 archives
  did include it; anyone who unpacked it from one of those by hand should not
  point it at sites they do not trust. Building it yourself also brings
  in Servo's dependency tree, which carries an RSA timing side channel
  (RUSTSEC-2023-0071) and unmaintained crates. What ships is built with default
  features, and that dependency tree has to pass the advisory check on every
  commit; the renderer's is reported weekly, separately, and never gates.
- **`syndeo-webkit` is kept behind `syndeo-proxy` by configuration, not by a
  sandbox.** Its web view is configured to send its traffic through the proxy,
  with the proxy's certificate pinned. Anything WebKit does not send through
  that setting is not covered by it. WebKit was seen to send requests for
  localhost and loopback addresses directly, never to the proxy, so a page in
  it can reach local services without the proxy seeing the request. WebRTC is
  another candidate and has not been measured. WebSockets do not work through
  the proxy.
- **What can reach the local proxy.** The proxy `syndeo-webkit` starts listens
  on a loopback port the system picks and answers only requests carrying a
  credential generated for that launch, which is handed to it and to WebKit
  and to nothing else; a request without it gets 407 and reaches no origin.
  It keeps out a process that can reach the port and nothing more; it is no
  defence against code already able to inspect the browser process itself. A
  `syndeo-proxy run` started by hand asks for no
  credential: anything on the machine that can reach its port, 127.0.0.1:8899
  by default, can fetch through it, read its statistics, and have its requests
  answered from its cache, for as long as it runs. Neither limits how many
  connections it accepts. Request bodies are limited, to 64 MiB by default.
- **Proxy traffic is not partitioned by site, and says it is a proxy.** The
  proxy — and so `syndeo-webkit` — has no top-level site to partition its
  cache by, and every request it forwards carries `Via: 1.1 syndeo`.
- **Losing both the passphrase and the recovery phrase is unrecoverable.** That
  is what the recovery phrase is for.

## Fixed, and worth knowing about

Fixed in 0.1.5:

- **A tool could run past its fuel, and could corrupt the runtime's
  garbage-collected heap.** Every release up to 0.1.4 ran the agent's tools on
  Wasmtime 48.0.2. With fuel on, as the agent has it, that version lost count
  of the fuel a function spent when it was reached through `call_ref` or
  threw to a `try_table` that caught it, so a tool could hold the agent for as
  long as it liked (RUSTSEC-2026-0315). A GC reference held across a call in a
  `try_table` might not be kept alive (RUSTSEC-2026-0326). Wasmtime 48.0.4
  fixes both. Only tools run on Wasmtime, and either needs a hostile tool in
  your tools directory.

Fixed in 0.1.4:

- **A `CONNECT` tunnel could reach an origin other than the one it named.**
  Inside a tunnel the proxy fetched whatever absolute URL a request gave. The
  tunnel's authority is now the only target, and a `CONNECT` without a port is
  refused.
- **The browser's proxy answered anything on the machine.** It listened on a
  fixed port with no credential, and `syndeo-webkit` took whatever answered on
  that port for its proxy. It now learns the address from the proxy itself and
  requires a per-launch credential.
- **Cookies were stored with cached responses** and handed to whoever asked
  next — another client of the proxy, or the same user after the site had
  cleared them. The cache now never stores or replays `Set-Cookie`.
- **A peer could fetch any cached body** by its content address or by a digest
  the store computed, including private responses, which told it where you had
  been. Only bodies checked against a hash a page declared are served now.
- **Declared integrity was not checked before a body was stored and shared**,
  and cached bodies were never checked against it. Every path checks it now,
  and a declaration with no usable hash is refused rather than ignored.
- **Text from a page reached the terminal unfiltered**, escape sequences
  included, through `syndeo browse` and the agent.
- **The agent could ask for your identity at any site without anyone being
  asked**, which could link identities that are derived separately so that
  they cannot be linked. The shell now checks the origin and asks you first.
- **A response could be as large as an origin liked.** `syndeo browse`, the
  agent, the window and Servo collected every response whole, with no
  ceiling, so an origin could send until memory ran out. A whole-body fetch
  now refuses a body past 64 MiB and closes the connection, which lets the
  origin go.
- **Parsing a page had no bound**, so a page could hold `syndeo browse` or the
  agent for as long as it liked. Every page is now parsed within a fixed
  budget.
- **Passphrases were left in memory** after use, in the environment of a
  scripted run and in the shell's and keystore's handling of them. They are
  taken out of the environment before any thread exists, and wiped after use
  everywhere Syndeo holds them; copies made inside libraries it uses (the
  terminal password reader, the JSON decoder's scratch buffer, the window's
  text field) are not.
- **`syndeo doctor` described protection the session did not have.** 0.1.3
  printed a fixed list, screen lock included, on every platform. It now
  reports what the running keystore says.

Fixed in 0.1.3:

- **0.1.2's `syndeo-webkit` let a redirect's destination run as the site that
  redirected to it.** Its proxy followed redirects itself and handed back the
  destination's page as the answer to the first URL. Fixed in 0.1.3. The
  installer never installed that renderer; anyone who unpacked the 0.1.2
  archive by hand and ran it should upgrade.

## Supported versions

The latest release. There is no back-porting before 1.0.
