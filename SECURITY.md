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
   integrity hash**, or serve one origin's response to another.

## What is already known, and is not a finding

These are documented limits rather than undiscovered ones. Reporting them is
welcome as a second opinion; they are not treated as new.

- **An unsigned macOS build does not enforce Secure Enclave presence**, and
  the v0.1.3 macOS release is unsigned: no Developer ID signature, not
  notarized. The data protection keychain needs a signed binary with a keychain
  access group. Without one the keystore falls back to the ordinary keychain,
  reports presence as unenforced, and makes the passphrase mandatory instead.
  `syndeo-keystore status` says which side of that line a build is on. A
  browser-downloaded archive is quarantined and Gatekeeper rejects it; the
  one-line installer is not quarantined.
- **A peer learns which hashes you want, and when.** It never learns a URL, and
  that is not the same as anonymous. `syndeo-peer`'s crate documentation is
  explicit about it.
- **The Landlock confinement is compiled but not yet exercised on a running
  Linux kernel.** The macOS Seatbelt path is tested, including a fixture that
  holds TCP, UDP, file writes and `exec` to failing.
- **`syndeo-servo` is experimental, for development only, and unsafe for
  untrusted sites.** It does not enforce cross-origin reads, so a page can read
  other origins' responses, including services on the machine and its network;
  it sends form POST bodies empty; and it buffers every response completely,
  with no size cap. It says so on `--help` and each time it starts. It is behind
  an off-by-default `renderer` feature and is in no release: the v0.1.1 and
  v0.1.2 tarballs carried it, v0.1.3's do not. Building it yourself also brings
  in Servo's dependency tree, which carries an RSA timing side channel
  (RUSTSEC-2023-0071) and unmaintained crates. What ships is built with default
  features, and that dependency tree has to pass the advisory check on every
  commit; the renderer's is reported weekly, separately, and never gates.
- **`syndeo-webkit` is kept behind `syndeo-proxy` by configuration, not by a
  sandbox.** Its web view is configured to send its traffic through the proxy,
  with the proxy's certificate pinned. Anything WebKit does not send through
  that setting is not covered by it; WebRTC is the obvious candidate and has
  not been measured. WebSockets do not work through the proxy.
- **Proxy traffic is not partitioned by site, and says it is a proxy.** The
  proxy — and so `syndeo-webkit` — has no top-level site to partition its
  cache by, and every request it forwards carries `Via: 1.1 syndeo`.
- **Losing both the passphrase and the recovery phrase is unrecoverable.** That
  is what the recovery phrase is for.

## Fixed, and worth knowing about

- **0.1.2's `syndeo-webkit` let a redirect's destination run as the site that
  redirected to it.** Its proxy followed redirects itself and handed back the
  destination's page as the answer to the first URL. Fixed in 0.1.3. The
  installer never installed that renderer; anyone who unpacked the 0.1.2
  archive by hand and ran it should upgrade.

## Supported versions

The latest release. There is no back-porting before 1.0.
