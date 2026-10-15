# Syndeo

A browser built cache-first, with the network, the keys and the agent in
separate processes.

The cache is the product. Everything else is arranged so the cache can be
swapped, shared, or fed from a peer without anything above it noticing.

## Install

For one user, on macOS on Apple Silicon, or Linux on x86_64 or arm64:

```sh
curl -fsSL https://raw.githubusercontent.com/SUM-INNOVATION/syndeo/main/install.sh | sh
```

It downloads the release built for your machine, checks it against the published
`SHA256SUMS`, and puts the binaries in `~/.local/bin`. No `sudo`, and nothing
written outside your home directory. `SYNDEO_INSTALL_DIR` moves them;
`SYNDEO_VERSION` pins a version. If that directory is not on your `PATH`, it
says which file to add it to for your shell. On Linux it also checks that the
keystore can start, and says what to install if it cannot (see below). On
macOS there is also an installer package, for the whole Mac; see
[below](#for-the-whole-mac-the-installer-package). From the release after
v0.1.7 it installs Syndeo.app, the desktop application, and no longer the
command-line tools.

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
- **`libdbus-1.so.3` on Linux, for the keystore** — `libdbus-1-3` on Debian and
  Ubuntu, `dbus-libs` on Fedora. A desktop install has it; a minimal server or
  container image may not, and then the keystore cannot start at all. The
  installer checks and says so, and `syndeo` reports a keystore that exits
  before it is ready, rather than waiting for it.
- **A Secret Service implementation on Linux** — gnome-keyring or KWallet —
  before `syndeo-keystore init` will work, because the wrapping key is never a
  file we wrote. Everything that is not the keystore runs headless.
- **Windows and ChromeOS**: not yet, and tracked at
  [#17](https://github.com/SUM-INNOVATION/syndeo/issues/17).

**From v0.1.7, the macOS binaries are signed with a Developer ID and
notarized, and the installer package is signed, notarized and stapled.**
Releases through v0.1.6 are ad-hoc signed, without Developer ID or
notarization: installed with the one-liner above, those binaries are not
quarantined and run, while an archive of them downloaded in a browser is
quarantined, and Gatekeeper rejects its executables — use the one-liner for
them instead.

**No macOS build enforces Secure Enclave presence (Touch ID), signed or not.**
The keystore keeps its wrapping key in the ordinary login keychain, reports
platform presence as unenforced, and requires the passphrase. The data
protection keychain that would gate the key needs a keychain access group
entitlement that only a provisioning profile can authorise, and a standalone
executable cannot carry one: signed with it anyway, macOS kills it at launch.
`syndeo-keystore status` reports this.

### Verifying a download yourself

```sh
curl -fsSLO https://github.com/SUM-INNOVATION/syndeo/releases/latest/download/SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
```

### For the whole Mac: the installer package

The package installs **Syndeo.app**, the desktop application, in
`/Applications`, and nothing else. The v0.1.6 and v0.1.7 packages were
different: they installed the command-line tools under `/usr/local`. Those
now come only from the tarball and `install.sh`, above. With `<version>` the
release's version:

```sh
curl -fsSLO https://github.com/SUM-INNOVATION/syndeo/releases/download/v<version>/Syndeo-<version>.pkg
curl -fsSLO https://github.com/SUM-INNOVATION/syndeo/releases/download/v<version>/SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
sudo installer -pkg Syndeo-<version>.pkg -target /
```

- **What it installs.** `/Applications/Syndeo.app`, root-owned:
  - its helper, `syndeo-proxy`, inside it, in `Contents/Helpers`;
  - its uninstaller, in `Contents/Resources/uninstall.sh`;
  - the package's receipt, `com.sum.syndeo.app`.

  Nothing goes in `/usr/local`: no command and no link. It needs an
  administrator, Apple Silicon and macOS 13 or later, and installs only on
  the startup disk, always in `/Applications`.
- **Its two scripts.**
  - The preinstall writes nothing. It refuses:
    - while Syndeo or its helper runs from `/Applications`, or any program of
      the v0.1.x package runs from its own tree (below);
    - a `Syndeo.app` there that no Syndeo package installed;
    - one changed since it was signed, or not what its receipt says;
    - a package older than the one installed.
  - The postinstall checks that the bundle is exactly this version, its
    signature intact. Then it removes the v0.1.x command-line package, if it
    is there (below).

  Installer itself says only that the installation failed. The scripts say
  why, one `syndeo preinstall: refusing: …` line per reason. Installer
  normally records those lines in `/var/log/install.log`, but they are not
  guaranteed to appear there.
- **Over the v0.1.6 or v0.1.7 package.** Its receipt is `com.sum.syndeo.pkg`.
  The package removes it only if it is exactly as its own package left it:
  - its seven links in `/usr/local/bin`, then `/usr/local/libexec/syndeo`,
    each path looked at again just before it goes;
  - `/usr/local/libexec` itself, only if it is an empty directory exactly as
    that package created it (root:wheel, 0755, no access control list, flags
    or extended attributes) and no other package claims it. Otherwise it
    stays, harmlessly;
  - its receipt, last.

  Quit anything of it first: the package refuses while any of its programs
  runs from `/usr/local/libexec/syndeo`, however it was started.
  `/usr/local/bin` itself and everything else in it stay. Without that
  receipt, nothing in `/usr/local` is looked at, so the commands `install.sh`
  puts there, or anywhere, are never touched. In any other state the package
  refuses, and touches none of it: a file changed or missing, an extra one, or
  a command `install.sh` put over one of its links. Repair it by installing
  its own package again, or remove it with
  `sudo /bin/sh /usr/local/libexec/syndeo/<old version>/uninstall.sh`, then
  install this one. If the removal is interrupted, installing the same
  package again finishes it.
- **Upgrading.** Quit Syndeo first: Installer would replace it while it runs,
  so the package refuses to.
  - If an upgrade fails, Installer undoes nothing: the new bundle stays, under
    the old receipt. Install the same package again to finish it. Until then
    every other package, older or newer, is refused, and so is the
    uninstaller.
- **Downgrading is refused.** Remove Syndeo.app with its uninstaller first,
  which also forgets the package's receipt. After Syndeo.app is moved to the
  Trash, its receipt remains, and an older package is still refused until
  that receipt is forgotten:

  ```sh
  sudo /usr/sbin/pkgutil --forget com.sum.syndeo.app --volume /
  ```
- **Removing it**, either way:
  - With the uninstaller, which removes the application and the package's
    receipt:

    ```sh
    sudo /bin/sh /Applications/Syndeo.app/Contents/Resources/uninstall.sh
    ```

    It changes nothing unless all of it is exactly as the package installed
    it, its signature included. `--help` says what it does. If it removed the
    application but could not forget the receipt, it exits 2 and prints the
    command that finishes the job:

    ```sh
    sudo /usr/sbin/pkgutil --forget com.sum.syndeo.app --volume /
    ```
  - By moving Syndeo.app to the Trash. The receipt stays behind, harmlessly.
    Installing the same version or a newer one puts the application back in
    `/Applications`; an older one is refused until the receipt is forgotten
    (above).

  Neither touches your settings or data: `~/Library/Application Support/Syndeo`,
  preferences, cookies, caches and search settings are kept, and so are
  keychain items and trust settings.
- **Signing.**
  - In a signed release, Syndeo.app and its helper are signed with a
    Developer ID Application identity, with the hardened runtime and no
    entitlements, and notarized with the command-line binaries, the ticket
    stapled. The package is signed with a Developer ID Installer identity,
    notarized, and its ticket stapled. Gatekeeper has to accept both.
  - An unsigned build's application is signed ad hoc, and its package not at
    all. Downloaded in a browser, Gatekeeper rejects both.
- **What was tested.** Every pull request builds the binaries and Syndeo.app
  as a release does, builds the package from the app, inspects it, and
  installs it for real on a disposable GitHub-hosted macOS 15 runner. The
  test covers:
  - a download quarantined as Safari leaves one, and Gatekeeper's verdict on
    it, then installed;
  - the application opened through LaunchServices, its helper running from
    inside the bundle, byte for byte the one the package carried;
  - the published v0.1.6 and v0.1.7 packages removed, every kind of change to
    them refused, and an interrupted removal finished;
  - the commands `install.sh` put in `/usr/local/bin`, left alone;
  - upgrades, a failed upgrade and its repair, a downgrade refused, a
    reinstall, and refusals while the app runs and while a v0.1.x program
    runs;
  - the Trash: an older package refused, the same one reinstalled, and a
    deliberate downgrade once the receipt is forgotten;
  - the uninstaller, and a receipt that cannot be forgotten, with the user's
    data kept throughout;
  - the runner put back afterwards.

  A signed release runs the same test on its signed package, where
  Gatekeeper has to accept it.
- **Checked by hand, for every signed release:** installing by opening the
  package in Finder, which runs Installer's window and asks for an
  administrator's password, then opening Syndeo.app from `/Applications`. CI
  cannot drive either, so it is a required manual check before the release's
  tag (see [Releasing](#releasing)), not something a pull request waits on.
- **What was not tested:** the refusal of Intel Macs and of macOS before 13,
  which rests on the package's metadata alone, with no such runners to try
  it on.

## The three boundaries

These are load-bearing. Get them right at the start and everything else is
refactorable; get them wrong and no amount of later work recovers it.

1. **`syndeo-servo` opens no socket; `syndeo-webkit` is configured to use the
   proxy.** Servo sends a `NetRequest` and receives bytes: sockets, DNS,
   certificates and the cache all live behind that one call, in `syndeo-net`,
   and the renderer itself opens no socket. `syndeo-webkit` holds to something
   weaker: its web view is configured to send its HTTP and HTTPS traffic
   through `syndeo-proxy`, in front of `syndeo-net`. That is a configuration,
   not a sandbox, and it has exceptions. WebKit was seen to send requests for
   localhost and loopback addresses directly, never to the proxy. And it does
   not cover transports WebKit sends outside its proxy setting — WebRTC is the
   obvious one, and has not been measured.
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
| `syndeo-webkit` | WebKit embedded, configured to send its traffic through syndeo-proxy (localhost and loopback excepted), with the proxy's certificate pinned — the one that plays video (macOS) |
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

`syndeo-servo` is not in that build, or in the release archives from v0.1.3
on; v0.1.1 and v0.1.2 did include it. See below for what it costs to build.

## Running it

### A page, rendered

> **Experimental, for development only — unsafe for untrusted sites.**
> `syndeo-servo` does not enforce cross-origin reads, so a page can read other
> origins' responses, including services on your machine and your network; it
> sends form POST bodies empty; and it buffers every response completely, with
> no size cap. It prints those three reasons every time it starts, and `--help`
> gives them too, along with which releases included it: it is left out of the
> release archives from v0.1.3 on, though v0.1.1 and v0.1.2 included it. If you
> unpacked it from one of those, all of this applies. To browse, use
> `syndeo-webkit` (macOS) or `syndeo-ui`.

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
interception is the supported way in front of them. It can take a body in
pieces, but this embedding does not use that yet: each response is fetched
whole from the network process and handed to Servo in one piece, with no size
cap, which is one of the reasons for the warning above.

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

### A page that plays

```sh
syndeo-proxy ca --trust                  # once: it asks first, then macOS asks for your password
syndeo-webkit https://www.youtube.com/watch?v=wXtngLBkK4Q
```

macOS only. This is the one that plays video: WebKit for the engine, so Media
Source Extensions and adaptive streaming work, with its HTTP and HTTPS loads —
the DASH segments included — going through `syndeo-proxy` into our own cache,
except loads of localhost and loopback addresses, which WebKit was seen to
make directly. It starts that proxy itself, on a port the system picks, and
stops it again however it exits. That proxy answers only this browser: every
request to it has to carry a credential made for this launch, which WebKit is
given and nothing else on the machine is. The installer puts it beside the
proxy from 0.1.3 on.

Plain `http://` pages load through it from 0.1.4 on. In 0.1.3 every one of
them failed: WebKit sends them down a `CONNECT` tunnel as plain HTTP, and the
proxy expected TLS inside every tunnel.

If you unpacked the 0.1.2 archive by hand and ran its `syndeo-webkit`, upgrade:
that build's proxy followed redirects itself, so a page could run as the site
that redirected to it. The installer never installed that renderer.

Tabs: `⌘T` opens, `⌘W` closes, `⌘[` and `⌘]` cycle, `⌘1`–`⌘9` jump. Hidden
rather than destroyed, so a tab keeps its scroll position, its heap and its
playing video, and all of them share one process.

Boundary one holds differently here, and more weakly, which is worth stating
rather than glossing. With Servo the renderer opens no socket at all, because
Servo asks the embedder about every load. WebKit will not do that —
`WKURLSchemeHandler` refuses `http` and `https` — so instead the web view is
configured to send its traffic through syndeo-proxy, with the proxy's
certificate pinned. That is a configuration WebKit honours for the loads it
makes, not a sandbox Syndeo controls and not something the kernel enforces.
Anything WebKit does not send through its proxy setting is not covered by it:
requests for localhost and loopback addresses, which WebKit was seen to send
directly, are one such case, and WebRTC's transports may be another; see
*Known gaps*.

The trust step is not decoration. Caching HTTPS means terminating it, and
WebKit validates subresources in its *networking* process, which never consults
an anchor pinned inside ours — so without it a page loads its document and
silently drops everything else. The authority is generated on your machine,
goes in your login keychain for your user only, is trusted for TLS and nothing
else, needs no `sudo`, and comes out again with `syndeo-proxy ca --untrust`.
Anyone who takes its private key can impersonate any site to you, which is why
`--trust` asks before it acts and tells you where that key is.

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

Everything a page supplies is printed without its control and format
characters, so a page cannot move the cursor, clear the screen, set the
clipboard or reorder text in your terminal. `--json` is left as it is: JSON
escapes those characters itself.

Parsing a page is bounded. Some markup costs html5ever far more than its size —
a one-megabyte page nested two hundred thousand deep takes html5ever alone 48
seconds in a release build — so
every page is parsed against a fixed budget of work, and one that would cost
more is parsed only as far as the budget goes. `browse` says when a page was cut
short, and how much of it was parsed; `--json` gives the same as `cut_short`,
which is `null` for a page parsed whole.

Three limits are involved, and none stands in for another:

- **The parser's work budget** bounds what parsing a body that has arrived may
  cost.
- **The whole-body ceiling**, 64 MiB, bounds how much of a response `browse`,
  the agent, the window and Servo collect to parse at all. The network process
  streams an ordinary response on to them as it arrives, whatever its length
  (the exceptions are under `max_body_bytes`, next); a response that declares
  a larger length is refused before its body is read, one that does not is
  refused at the piece that crosses the ceiling, and the connection is closed
  so the origin is let go. The proxy streams, and is not bound by it.
- **The network process's `max_body_bytes`**, also 64 MiB by default, is a
  different limit on different responses. An ordinary GET or HEAD from the
  origin streams through however large it is, and past this limit is simply
  not cached. A response the network process has to hold whole before it can
  use it is buffered instead, and refused past this limit: one whose caller
  declared its integrity, which is checked before any of it is passed on; the
  response to any method but GET, HEAD, OPTIONS and TRACE; a revalidation
  answered with a full body; and a background refresh.

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

A request body larger than `--max-request-body` (64 MiB by default) is refused
with 413 before it reaches the origin. A `syndeo-proxy run` like this one asks
its clients for no credential, so anything on the machine that can reach
127.0.0.1:8899 can use it while it runs; only the proxy `syndeo-webkit` starts
for itself requires one.

The proxy has a cache of its own, at `<SYNDEO_HOME>/proxy/cache` (`--cache`
moves it), apart from the one `syndeo browse` and the other command-line tools
use at `<SYNDEO_HOME>/cache`. So its hit rates and statistics are the proxy's,
not the command line's, and the two can run at once. Upgrading to 0.1.3 starts
the proxy's cache empty. Proxy traffic is not partitioned by site: see *What it
does not tell anyone*.

The proxy caches only what came from the public internet. A response whose
connection was to a loopback, private, link-local, shared (carrier-grade NAT)
or other address that is not publicly reachable is passed on and never
stored, even when a public name resolved there: it is the address the proxy
actually connected to that counts, not the name in the URL.

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

The purpose you give is shown and bound into the shell's confirmation, but it
is not part of what is signed: the signature is a plain ed25519 signature over
the payload, so anything verifying it cannot tell a login from a transaction.
Changing that changes every signature, and is left for a later release.

### The agent

```sh
syndeo agent "read https://www.rust-lang.org/"
syndeo agent "crawl https://www.rust-lang.org/"
syndeo agent "sign https://wallet.test anything"     # goes in front of a human
```

The agent can also ask for your identity at a site — the public key and
address that site sees. The shell checks the origin, then asks you before the
keystore is asked anything; a run with nobody to ask declines.

### Peer fetch

```sh
syndeo browse https://example.test/ --peer on
syndeo browse https://example.test/ --peer /ip4/10.0.0.5/tcp/4001
```

A peer is asked only for a body the page already named by hash. No declared
integrity, no peer request.

What a node offers to peers is as narrow: only bodies it stored itself and
has checked against a hash some page declared. A peer can ask for one by that
hash, or by its content address; any other body is answered as one the node
does not have. A body it got from a peer is used for the request and not kept,
so it is not offered on.

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

**The cache is partitioned by the top-level site** — for everything that goes
through `syndeo-net` directly: `syndeo browse`, `syndeo-ui`, the agent and the
renderers it serves. A cache keyed on the URL
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

`syndeo-proxy`, and so `syndeo-webkit`, is not partitioned. A browser behind a
proxy does not say which page a request came from, so the proxy has no
top-level site to partition by, and its cache is shared across every site
browsed through it. The timing attack described above works against it.

**The proxy says it is a proxy.** Requests it forwards carry `Via: 1.1 syndeo`
on the end of whatever chain they arrived with, as HTTP requires of a proxy and
as the proxy needs in order to recognise a request that has come back to it.
So a site can tell that a request came through Syndeo's proxy.

**DNS goes over HTTPS by default.** The system resolver sees every hostname you
visit, in plaintext, and hands it to whoever runs it. The default is
`doh:cloudflare`; `--dns doh:google`, `doh:quad9` and `dot:cloudflare` are
there, and `--dns system` goes back to the resolver the machine is configured
with. Two honest costs: it points your lookups at one operator instead of your
ISP, which is a different trust rather than none; and it breaks captive portals
and split-horizon corporate DNS until you pass `--dns system`.

**Nothing is reported anywhere.** No telemetry, no analytics, no crash
reporting, no update ping. Nothing in this tree sends anything anywhere except
to the page you asked for, the DNS resolver named above, and — only when you
turn it on — peers.

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

The first one is not in force in any build Syndeo ships, signed or not. The
attributes only mean anything in the macOS data protection keychain, which a
process reaches only with a `keychain-access-groups` entitlement. That
entitlement is restricted: macOS honours it only where a provisioning profile
authorises it, and kills at launch a process that claims it without one, Developer
ID signed or not (Apple's TN3125). A standalone executable such as
`syndeo-keystore` cannot embed a profile. So Syndeo's binaries are signed
without entitlements, `SecItemAdd` returns `errSecMissingEntitlement`, and the
keystore keeps the wrapping key in the ordinary keychain and reports presence
as unenforced — at which point the passphrase is mandatory rather than
optional. `syndeo-keystore status` says so. Turning platform presence on would
take wrapping the keystore in an app-like bundle with a Developer ID
provisioning profile for its own App ID; that is separate work, not started.

The keystore also forgets the seed on its own, and how depends on the platform
and the session:

- **When the machine has slept**, on every platform: the keystore notices wall
  time jumping past its monotonic clock.
- **After five idle minutes**, the default idle timeout.
- **When the screen locks, on macOS only, and only in a session that reports
  it** — a graphical login. Over ssh, or run as a daemon, macOS does not report
  the screen lock, and locking it forgets nothing. Linux does not report it at
  all.

`syndeo doctor` asks the running keystore which of these hold for it, now, and
says only those. A signature after the seed is forgotten costs a passphrase
prompt rather than the operation.

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
SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID=<team> ci/verify-release.sh 0.1.7
```

`SYNDEO_EXPECT_SIGNED=yes` is for a signed release, v0.1.7 and later, and
`MACOS_TEAM_ID` is the ten-character team it is signed by, which
`pkgutil --check-signature` shows for the release's package. Each binary is
then held to what a notarized command-line tool is:
- Developer ID Application of that team, chained to Apple Root CA;
- a strict verification;
- the hardened runtime and a secure timestamp;
- no entitlements;
- notarized, as `codesign --check-notarization -R=notarized` finds;
- it runs, and its `--version` says exactly this release.

Gatekeeper's `spctl -a -t execute` is not asked about the binaries. That
assessment is for apps, and rejects every bare command-line tool, however
it is signed. Gatekeeper still judges the package (`ci/verify-pkg.sh`). For
an earlier release leave both out, and the binaries are expected to carry no
Developer ID.

It installs from the release with the same one-liner the README gives, into a
throwaway directory, and then asks the installed binaries to demonstrate what
has broken before — a cache hit that is served and then forgotten, a proxy that
turns every Google host into a 502, binaries that cannot find each other, a
`doctor` that claims protection the session does not have. Every check in it
exists because something it covers once shipped broken. It exits non-zero, so
it can gate a release rather than decorate one. `ci/verify-release.sh
--self-test` checks its judgement against stand-ins, and `ci/test-install.sh`
runs the installer against a release built on the spot; CI runs both, on Linux
and macOS.

`.github/workflows/release.yml` refuses a tag that disagrees with the workspace
version, and builds the three targets, Syndeo.app and the macOS installer
package.
- **Only the signing jobs see the signing configuration.** The
  `release-macos` environment holds `SYNDEO_EXPECT_SIGNED` and any signing
  secrets. Four jobs declare it, and each is given only its own share:
  - `signing-config` decides;
  - `build-macos` gets the Application certificate;
  - `notarize-binaries` gets the notary key;
  - `package` gets the Installer certificate and the notary key.

  No other job sees any of it. The environment is that boundary, not an
  approval gate: it has no required reviewers and no wait timer, and a
  release run waits for no one.
- **Signing is all or nothing.**
  [`ci/check-signing-config.sh`](ci/check-signing-config.sh) reads the
  environment's `SYNDEO_EXPECT_SIGNED` and the ten secrets it names, and
  refuses any half-configured state.
  - Signed, the macOS release goes through three jobs, in order:
    1. `build-macos` signs the binaries with `ci/sign-macos.sh` (Developer ID
       Application, the hardened runtime and a secure timestamp, and no
       entitlements) and makes the tarball. It assembles Syndeo.app from the
       same binaries, its helper the tarball's own `syndeo-proxy`, and signs
       the bundle the same way. It then checks the signatures and runs every
       binary (`ci/verify-pkg.sh signed-binaries` and `signed-app`): each
       `--version` has to exit 0 with exactly its name and version, and
       `syndeo-keystore status` has to report presence unenforced and the
       passphrase required. macOS kills a binary it refuses as it starts, and
       that fails here, before Apple is sent anything.
    2. `notarize-binaries` checks and runs exactly those binaries and that
       bundle again, then sends them to Apple once, together, with
       `ci/notarize.py`, and waits for the verdict.
    3. `package` staples that same bundle, checked by its SHA-256, and
       requires it unchanged but for its ticket, and accepted by Gatekeeper.
       It builds the installer package from it, signs it with
       `ci/sign-macos-pkg.sh`, sends it to Apple once, waits, and only then
       staples it. Every stapler
       command is shown with its output and exit status. Validating the
       stapled ticket is retried, up to five times over about four minutes,
       and has to pass; if it never does, the job fails and keeps the package
       and what the tools said as an internal artifact, for a week.

    Each wait has its own job, and its own deadline inside that job's time
    limit. A deadline reached means Apple has not decided, which is not a
    rejection: nothing is resubmitted, and the submission ID is kept to ask
    about later.
  - Unsigned, nothing is signed or sent to Apple.
  - v0.1.6 was built with `SYNDEO_EXPECT_SIGNED=no` and no signing secrets,
    so its binaries have no Developer ID and its package is unsigned. v0.1.7
    is the first release built with `SYNDEO_EXPECT_SIGNED=yes`.
- **The package** is built from Syndeo.app, inspected against what was
  decided and against that bundle (`ci/verify-pkg.sh inspect`), and installed
  and removed on a fresh runner.
- **Publishing.** Exactly the three tarballs and the package, with one
  `SHA256SUMS` over them, go into a draft release. The draft's assets are
  downloaded back and checked against what was built, and its notes against
  what they should say. Only then is it published.
- **A `workflow_dispatch` run is a dry run.** It does all of this, but
  rehearses the publication on a draft it deletes, and publishes nothing.
- **Finder, by hand, before a signed release is tagged.** CI installs the
  package with `installer`; it cannot drive Installer's window or its
  password prompt. So for every signed release:
  1. Download the signed dry run's `Syndeo-<version>.pkg` in a browser, on a
     Mac with no Syndeo on it.
  2. Open it in Finder, and install it through Installer's window.
  3. Open Syndeo.app from `/Applications`, and browse a page.

  Gatekeeper has to accept both without a warning. A tag waits for this,
  and its result is recorded with the release's evidence.
- **Checks on every pull request.** `ci/check-workflows.py` checks that no
  pull request can see a secret, and that release.yml keeps them to that one
  job.

`ci/verify-release.sh` checks the signing state of the installed binaries
against `SYNDEO_EXPECT_SIGNED`, `no` by default, and fails a release that is
not what it says.

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
- WebSockets do not go through `syndeo-proxy`. It removes the `Upgrade` header
  a WebSocket handshake depends on, as it does every hop-by-hop header, and has
  no path for an upgraded connection. That is from reading the code; it has not
  been measured in `syndeo-webkit`.
- WebRTC in `syndeo-webkit` is not covered by the proxy configuration, and
  whether WebKit sends its WebRTC traffic anywhere else has not been measured.
- WebKit sends some requests directly, not through the proxy. Measured on CI,
  not inferred:
  - `localhost` and loopback addresses (127/8, ::1) always went directly;
  - addresses assigned to the Mac itself went directly, except its own
    network address, which went directly in one run and through the proxy
    in another;
  - private, link-local and shared (carrier-grade NAT) addresses that were
    not the Mac's own went through the proxy;
  - every name, whatever it resolves to, and every public address went
    through the proxy.

  A request that goes directly reaches its service without the proxy, and is
  neither cached nor counted by it, and the proxy caches nothing from a
  local-network address either. Syndeo.app's bar says Direct for loopback,
  Local for a local-network address, which may go either way, and Through
  Syndeo for the rest.
- The proxy and `syndeo-webkit` share an unpartitioned cache.
- A plain `syndeo-proxy run` asks for no credential; only the proxy
  `syndeo-webkit` starts does. Neither limits how many connections it accepts.
- Signatures do not say what they are for. The purpose is bound into the
  shell's confirmation, not into the signature.
- A page that would cost the parser more than its fixed budget is read only as
  far as the budget goes, and says so.

Two things are done but not *demonstrated* on an ordinary developer machine, and
both say so where you would meet them:

- Secure Enclave presence needs a signed build (#1's binding is in the tree;
  `syndeo-keystore status` reports which side of that line you are on).
- The agent's Landlock confinement compiles for Linux and has not been exercised
  on a Linux kernel from here. Where a kernel applies only part of the ruleset,
  the agent says it is partially confined and names what that kernel cannot
  restrict — below Landlock ABI 4, for instance, TCP. macOS Seatbelt
  confinement is tested, including a case that builds a fixture and holds TCP,
  UDP, file writes and `exec` to failing.

## Tests

```sh
cargo test --workspace
```

About 475 on macOS and 455 on Linux, where the Seatbelt, keychain and WebKit
tests do not run. Two macOS tests drive the real WebKit data store and a
throwaway keychain, and run only where `SYNDEO_WEBKIT_UI_TEST=1` and
`SYNDEO_KEYCHAIN_TEST=1` are set, as CI's macOS runner sets them. The RFC 9111 conformance suite,
`crates/syndeo-cache/tests/rfc9111.rs`, files its 51 cases under the section of
the RFC each one covers.

```sh
cargo test --workspace                  # does not build Servo
cargo build -p syndeo-servo --features renderer
```
