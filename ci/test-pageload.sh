#!/usr/bin/env bash
# How long a page takes to show in WebKit by itself, and through Syndeo's
# proxy, on a disposable macOS runner.
#
#   ci/test-pageload.sh <syndeo-proxy> <url> [runs]
#
# Each run, in turn, loads <url> in a new web view with a new, empty data
# store (ci/macos-app/pageload.swift), once cold and once warm:
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
# logs.
#
# Measured, and gated only on what WebKit is shown: every load has to finish.
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
[ $# -ge 2 ] || { say "usage: $0 <syndeo-proxy> <url> [runs]"; exit 2; }
PROXY="$1" URL="$2" RUNS="${3:-5}"
[ -x "$PROXY" ] || { say "no proxy at $PROXY"; exit 2; }

mkdir -p "$LOGS"
PAGELOAD="$LOGS/pageload"
swiftc -O -o "$PAGELOAD" "$here/macos-app/pageload.swift" >"$LOGS/swiftc.log" 2>&1 || { say "could not build pageload"; cat "$LOGS/swiftc.log"; exit 2; }

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

# load VARIANT RUN [PROXY-ARG...]: one cold and one warm load, appended to
# VARIANT.jsonl.
load() {
  local variant="$1" run="$2" dir
  shift 2
  if [ "$variant" = direct ]; then
    perl -e 'alarm 180; exec @ARGV' "$PAGELOAD" "$URL" 1 >>"$LOGS/$variant.jsonl" 2>>"$LOGS/$variant.stderr"
    return
  fi
  dir="$LOGS/$variant-$run"
  start_proxy "$dir" "$@" || { printf '{"run":%s,"visit":"cold","ok":false,"error":"no proxy"}\n' "$run" >>"$LOGS/$variant.jsonl"; return; }
  perl -e 'alarm 180; exec @ARGV' "$PAGELOAD" "$URL" 1 "$PROXY_AT" "$dir/proxy/syndeo-ca.pem" >>"$LOGS/$variant.jsonl" 2>>"$LOGS/$variant.stderr"
  stop_proxy
}

say "$URL, $RUNS runs of each, cold and warm"
for run in $(seq 1 "$RUNS"); do
  load direct "$run"
  load syndeo "$run"
  load syndeo-dns-system "$run" --dns system
done

python3 -I - "$LOGS" <<'EOF'
import glob, json, os, re, statistics, sys
logs = sys.argv[1]
failed = 0
print("\npage load, in ms from the navigation's start (medians; measured, not gated)")
print("  %-18s %-5s %4s %7s %7s %7s  %s" % ("variant", "visit", "n", "fcp", "load", "wall", "protocols"))
for variant in ("direct", "syndeo", "syndeo-dns-system"):
    path = os.path.join(logs, variant + ".jsonl")
    lines = [json.loads(l) for l in open(path)] if os.path.exists(path) else []
    for visit in ("cold", "warm"):
        rows = [l for l in lines if l.get("visit") == visit]
        good = [l for l in rows if l.get("ok")]
        failed += len(rows) - len(good)
        med = lambda k: (str(round(statistics.median([l[k] for l in good if l.get(k) is not None])))
                         if any(l.get(k) is not None for l in good) else "-")
        protocols = {}
        for l in good:
            for p, n in l.get("protocols", {}).items():
                protocols[p] = protocols.get(p, 0) + n
        print("  %-18s %-5s %4d %7s %7s %7s  %s" % (variant, visit, len(good), med("fcp"), med("load"), med("wall"),
              ", ".join("%s %d" % kv for kv in sorted(protocols.items()))))
        for l in rows:
            if not l.get("ok"):
                print("    FAILED  run %s: %s" % (l.get("run"), l.get("error")))
    if variant != "direct":
        tunnels = {}
        for stderr in glob.glob(os.path.join(logs, variant + "-*", "stderr")):
            for line in open(stderr, errors="replace"):
                m = re.search(r"tunnel opened host=(\S+)", re.sub(r"\x1b\[[0-9;]*m", "", line))
                if m:
                    tunnels[m.group(1)] = tunnels.get(m.group(1), 0) + 1
        runs = len(glob.glob(os.path.join(logs, variant + "-*")))
        top = sorted(tunnels.items(), key=lambda kv: -kv[1])[:6]
        print("  %-18s tunnels per run, by host: %s" % ("", ", ".join("%s %.1f" % (h, n / max(runs, 1)) for h, n in top) or "none logged"))
print("\n%d loads failed" % failed)
sys.exit(1 if failed else 0)
EOF
