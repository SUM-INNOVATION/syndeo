#!/usr/bin/env bash
# How long pages take to show in WebKit by itself, and through Syndeo's
# proxy, on a disposable macOS runner.
#
#   ci/test-pageload.sh <syndeo-proxy> <runs> <url>...
#
# For each url, each run, in turn, loads it in a new web view on an emptied
# data store (ci/macos-app/pageload.swift), once cold and once warm:
#   - direct: WebKit by itself;
#   - syndeo: through <syndeo-proxy> as Syndeo.app runs it, started for the
#     run with a home and a cache of its own, so nothing it kept from another
#     run answers;
#   - syndeo-dns-system: the same, resolving with the system's resolver
#     rather than the default DNS over HTTPS.
# Interleaved, so the network's own drift falls on all three alike. For each,
# the median first contentful paint and load, cold and warm; the protocols
# WebKit used, by nextHopProtocol; and through the proxy, how many tunnels
# WebKit opened to each host. Every load's record is kept in pageload/ in the
# logs, a directory to a url.
#
# The times are measured, never gated. What is gated is what WebKit is shown,
# for each url not written measure-only:<url>: every load has to finish, and
# through the proxy WebKit has to speak HTTP/2 to it on every cold load. A
# measure-only url is reported the same way, and gates nothing.
# It opens windows, so like ci/test-app.sh it refuses unless
# SYNDEO_ALLOW_APP_TEST=yes, which only CI's disposable runners set.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/syndeo-app-test/pageload"
say() { printf 'test-pageload: %s\n' "$*"; }

[ "$(uname -s)" = Darwin ] || { say "macOS only"; exit 2; }
[ "${SYNDEO_ALLOW_APP_TEST:-}" = yes ] || {
  say "refusing: this opens windows; only on a disposable runner, with SYNDEO_ALLOW_APP_TEST=yes"
  exit 2
}
[ $# -ge 3 ] || { say "usage: $0 <syndeo-proxy> <runs> <url>..."; exit 2; }
PROXY="$1" RUNS="$2"
shift 2
[ -x "$PROXY" ] || { say "no proxy at $PROXY"; exit 2; }

rm -rf "$LOGS"
mkdir -p "$LOGS"
# An application bundle, signed as CI signs Syndeo.app: ad hoc, with the
# hardened runtime; its Info.plist is pageload.plist.
BUNDLE="$LOGS/pageload.app"
mkdir -p "$BUNDLE/Contents/MacOS"
cp "$here/macos-app/pageload.plist" "$BUNDLE/Contents/Info.plist"
PAGELOAD="$BUNDLE/Contents/MacOS/pageload"
{ swiftc -O -o "$PAGELOAD" "$here/macos-app/pageload.swift" &&
  codesign --force --sign - --options runtime "$BUNDLE"; } >"$LOGS/swiftc.log" 2>&1 ||
  { say "could not build pageload"; cat "$LOGS/swiftc.log"; exit 2; }

PROXY_PID=''
stop_proxy() {
  [ -n "$PROXY_PID" ] || return 0
  kill "$PROXY_PID" 2>/dev/null
  wait "$PROXY_PID" 2>/dev/null
  PROXY_PID=''
}
trap stop_proxy EXIT

# start_proxy DIR [ARG...]: the proxy as Syndeo.app runs it, but without a
# credential, with a home and a cache in DIR, its tunnels logged; sets
# PROXY_AT to where it listens.
start_proxy() {
  local dir="$1" line='' until
  shift
  mkdir -p "$dir"
  SYNDEO_HOME="$dir" SYNDEO_LOG=syndeo_proxy=debug,syndeo_net=info NO_COLOR=1 \
    "$PROXY" run --listen 127.0.0.1:0 --announce --trace-requests=false "$@" \
    >"$dir/stdout" 2>"$dir/stderr" &
  PROXY_PID=$!
  until=$((SECONDS + 20))
  while [ "$SECONDS" -lt "$until" ]; do
    line="$(head -n 1 "$dir/stdout" 2>/dev/null)"
    case "$line" in "SYNDEO-PROXY-READY 1 "*) break ;; esac
    sleep 0.2
  done
  case "$line" in
    "SYNDEO-PROXY-READY 1 "*) PROXY_AT="${line#SYNDEO-PROXY-READY 1 }" ;;
    *) say "the proxy did not start: $(tail -n 3 "$dir/stderr" | tr '\n' ' ')"; return 1 ;;
  esac
}

# load OUT URL VARIANT RUN [PROXY-ARG...]: one cold and one warm load of URL,
# appended to OUT/VARIANT.jsonl.
load() {
  local out="$1" url="$2" variant="$3" run="$4" dir
  shift 4
  if [ "$variant" = direct ]; then
    perl -e 'alarm 180; exec @ARGV' "$PAGELOAD" "$url" 1 >>"$out/$variant.jsonl" 2>>"$out/$variant.stderr"
    return
  fi
  dir="$out/$variant-$run"
  start_proxy "$dir" "$@" || { printf '{"run":%s,"visit":"cold","ok":false,"error":"no proxy"}\n' "$run" >>"$out/$variant.jsonl"; return; }
  perl -e 'alarm 180; exec @ARGV' "$PAGELOAD" "$url" 1 "$PROXY_AT" "$dir/proxy/syndeo-ca.pem" >>"$out/$variant.jsonl" 2>>"$out/$variant.stderr"
  stop_proxy
}

n=0
for arg in "$@"; do
  n=$((n + 1))
  case "$arg" in
    measure-only:*) url="${arg#measure-only:}" gated=no ;;
    *) url="$arg" gated=yes ;;
  esac
  out="$LOGS/$n"
  mkdir -p "$out"
  printf '%s\n%s\n' "$url" "$gated" >"$out/url"
  say "$url, $RUNS runs of each, cold and warm$([ "$gated" = yes ] || echo '; measured only')"
  for run in $(seq 1 "$RUNS"); do
    load "$out" "$url" direct "$run"
    load "$out" "$url" syndeo "$run"
    load "$out" "$url" syndeo-dns-system "$run" --dns system
  done
done

python3 -I - "$LOGS" <<'EOF'
import glob, json, os, re, statistics, sys
logs = sys.argv[1]
failed = 0
for named in sorted(glob.glob(os.path.join(logs, "*", "url")), key=lambda p: int(p.split(os.sep)[-2])):
    url, gated = open(named).read().split()
    gated = gated == "yes"
    d = os.path.dirname(named)
    print("\n%s: ms from the navigation's start, medians%s" % (url, "" if gated else "; measured only, nothing gated"))
    print("  %-18s %-5s %4s %7s %7s %7s  %s" % ("variant", "visit", "n", "fcp", "load", "wall", "protocols"))
    for variant in ("direct", "syndeo", "syndeo-dns-system"):
        path = os.path.join(d, variant + ".jsonl")
        lines = [json.loads(l) for l in open(path)] if os.path.exists(path) else []
        for visit in ("cold", "warm"):
            rows = [l for l in lines if l.get("visit") == visit]
            good = [l for l in rows if l.get("ok")]
            if gated:
                failed += len(rows) - len(good)
            med = lambda k: (str(round(statistics.median([l[k] for l in good if l.get(k) is not None])))
                             if any(l.get(k) is not None for l in good) else "-")
            protocols = {}
            for l in good:
                for p, n in l.get("protocols", {}).items():
                    protocols[p] = protocols.get(p, 0) + n
            print("  %-18s %-5s %4d %7s %7s %7s  %s" % (variant, visit, len(good), med("fcp"), med("load"), med("wall"),
                  ", ".join("%s %d" % kv for kv in sorted(protocols.items()))))
            for error in sorted(set(str(l.get("error")) for l in rows if not l.get("ok"))):
                count = sum(1 for l in rows if not l.get("ok") and str(l.get("error")) == error)
                print("    %s  %d of %d: %s" % ("FAILED" if gated else "failed", count, len(rows), error))
            said = os.path.join(d, variant + ".stderr")
            if len(good) < len(rows) and os.path.exists(said):
                for line in sorted(set(open(said, errors="replace").read().splitlines()))[:3]:
                    print("    stderr  %s" % line[:300])
        if variant == "direct":
            continue
        cold = [l for l in lines if l.get("visit") == "cold" and l.get("ok")]
        if cold and not all(l.get("protocols", {}).get("h2") for l in cold):
            print("    %s  WebKit did not speak HTTP/2 through the proxy on every cold load" % ("FAILED" if gated else "failed"))
            if gated:
                failed += 1
        tunnels, refused = {}, set()
        for stderr in sorted(glob.glob(os.path.join(d, variant + "-*", "stderr"))):
            for line in open(stderr, errors="replace"):
                line = re.sub(r"\x1b\[[0-9;]*m", "", line)
                m = re.search(r"tunnel opened host=(\S+)", line)
                if m:
                    tunnels[m.group(1)] = tunnels.get(m.group(1), 0) + 1
                if "tls handshake failed" in line and any(not l.get("ok") for l in lines):
                    refused.add(re.sub(r"^\S+\s+", "", line).strip())
        for line in sorted(refused)[:3]:
            print("    proxy   %s" % line[:300])
        runs = len(glob.glob(os.path.join(d, variant + "-*")))
        top = sorted(tunnels.items(), key=lambda kv: -kv[1])[:6]
        print("  %-18s tunnels per run, by host: %s" % ("", ", ".join("%s %.1f" % (h, n / max(runs, 1)) for h, n in top) or "none logged"))
print("\n%d failed, of what is gated" % failed)
sys.exit(1 if failed else 0)
EOF
