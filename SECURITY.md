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
   network other than through `syndeo-net`.
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

- **An unsigned macOS build does not enforce Secure Enclave presence.** The
  data protection keychain needs a signed binary with a keychain access group.
  Without one the keystore falls back to the ordinary keychain, reports
  presence as unenforced, and makes the passphrase mandatory instead.
  `syndeo-keystore status` says which side of that line a build is on.
- **A peer learns which hashes you want, and when.** It never learns a URL, and
  that is not the same as anonymous. `syndeo-peer`'s crate documentation is
  explicit about it.
- **The Landlock confinement is compiled but not yet exercised on a running
  Linux kernel.** The macOS Seatbelt path is tested, including a fixture that
  holds TCP, UDP, file writes and `exec` to failing.
- **The renderer's dependency tree carries advisories, and is not in any
  release.** `syndeo-servo` is behind an off-by-default `renderer` feature and
  is in no tarball, because it is about 1,200 crates. Building it yourself
  brings in Servo's own dependencies, and today that includes an RSA timing
  side channel (RUSTSEC-2023-0071) and six unmaintained crates. CI reports that
  tree weekly rather than gating merges on it; what ships is checked on every
  commit and is clean. If you build the renderer, you are taking on Servo's
  dependency graph as well as ours.
- **Losing both the passphrase and the recovery phrase is unrecoverable.** That
  is what the recovery phrase is for.

## Supported versions

The latest release. There is no back-porting before 1.0.
