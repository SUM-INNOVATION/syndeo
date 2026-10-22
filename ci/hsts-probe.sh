#!/usr/bin/env bash
# EXPERIMENT (#58), removed before review: each host through Syndeo's proxy,
# by WebKit, URLSession and a bare Network framework connection, with each
# kind of leaf, on this runner's macOS. Logs in $RUNNER_TEMP/hsts-probe.
#
#   ci/hsts-probe.sh <syndeo-proxy> <host>...
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-/tmp}/hsts-probe"
say() { printf 'hsts-probe: %s\n' "$*"; }
[ "${SYNDEO_ALLOW_APP_TEST:-}" = yes ] || { say "refusing outside CI"; exit 2; }
PROXY="$1"
shift

rm -rf "$LOGS"
mkdir -p "$LOGS"
sw_vers >"$LOGS/sw_vers.txt"
BUNDLE="$LOGS/tlsprobe.app"
mkdir -p "$BUNDLE/Contents/MacOS"
sed -e 's/pageload/tlsprobe/g' "$here/macos-app/pageload.plist" >"$BUNDLE/Contents/Info.plist"
PROBE="$BUNDLE/Contents/MacOS/tlsprobe"
{ swiftc -O -o "$PROBE" "$here/macos-app/tlsprobe.swift" &&
  codesign --force --sign - --options runtime "$BUNDLE"; } >"$LOGS/swiftc.log" 2>&1 ||
  { say "could not build tlsprobe"; cat "$LOGS/swiftc.log"; exit 2; }

PROXY_PID=''
stop_proxy() {
  [ -n "$PROXY_PID" ] || return 0
  kill "$PROXY_PID" 2>/dev/null
  wait "$PROXY_PID" 2>/dev/null
  PROXY_PID=''
}
trap stop_proxy EXIT

for leaf in default 397; do
  dir="$LOGS/$leaf"
  mkdir -p "$dir"
  if [ "$leaf" = default ]; then unset SYNDEO_EXPERIMENT_LEAF; else export SYNDEO_EXPERIMENT_LEAF="$leaf"; fi
  SYNDEO_HOME="$dir" SYNDEO_LOG=syndeo_proxy=debug,syndeo_net=info,rustls=debug NO_COLOR=1 \
    "$PROXY" run --listen 127.0.0.1:0 --announce --trace-requests=false \
    >"$dir/stdout" 2>"$dir/stderr" &
  PROXY_PID=$!
  line=''
  for _ in $(seq 1 100); do
    line="$(head -n 1 "$dir/stdout" 2>/dev/null)"
    case "$line" in "SYNDEO-PROXY-READY 1 "*) break ;; esac
    sleep 0.2
  done
  at="${line#SYNDEO-PROXY-READY 1 }"
  say "leaf $leaf: proxy at $at"
  start="$(date '+%Y-%m-%d %H:%M:%S')"
  for host in "$@"; do
    for mode in nw session webkit; do
      perl -e 'alarm 45; exec @ARGV' "$PROBE" "$mode" "$host" "$at" "$dir/proxy/syndeo-ca.pem" \
        >>"$dir/probe.txt" 2>>"$dir/probe.stderr"
      grep "^RESULT $mode $host " "$dir/probe.txt" | tail -1 | cut -c1-400 | sed "s/^/  [$leaf] /"
    done
  done
  stop_proxy
  # What macOS logged meanwhile.
  sudo log show --start "$start" --style compact --info --debug \
    --predicate 'subsystem == "com.apple.network" OR subsystem == "com.apple.securityd" OR subsystem == "com.apple.CFNetwork" OR subsystem BEGINSWITH "com.apple.WebKit" OR process == "tlsprobe" OR process == "trustd"' 2>&1 |
    cat >"$dir/unified.log"
  say "leaf $leaf: unified log $(wc -l <"$dir/unified.log") lines; interesting lines:"
  grep -iE 'hsts|preload|boringssl.*(alert|fail|error)|trust.*(fail|error|result)|SecTrust|ct (required|policy)|transparency|validity|pinning|-98[0-9][0-9]|-1200' "$dir/unified.log" |
    grep -vE 'Unknown domain' | cut -c1-400 | sort | uniq -c | sort -rn | head -60
  say "leaf $leaf: proxy's TLS lines:"
  grep -E 'tls handshake|minting a leaf|alert|incompatible' "$dir/stderr" | sed -E 's/^[^ ]+ +//' | cut -c1-300 | sort | uniq -c | sort -rn | head -40
done

echo
say "summary"
for leaf in default 397; do
  grep '^RESULT' "$LOGS/$leaf/probe.txt" | awk -v leaf="$leaf" '{print "  " leaf, $2, $3, $4, $5}'
done
