#!/usr/bin/env bash
# EXPERIMENT (#58), removed before review: hosts through Syndeo's proxy on
# this runner's macOS, by WebKit and by a bare Network framework connection.
# Logs in $RUNNER_TEMP/hsts-probe.
#
#   ci/hsts-probe.sh <syndeo-proxy>
#
# 1. A baseline: a preloaded host and another, by WebKit as Syndeo.app runs.
# 2. A site's own HSTS header across a relaunch: loaded in one process, then
#    in a second that keeps the data store.
# 3. WebKit's allowsHSTSWithUntrustedRootCertificate (SPI) on the store:
#    preloaded hosts and others; then with an authority that is not the
#    proxy's, which the window's pinning has to refuse.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-/tmp}/hsts-probe"
say() { printf 'hsts-probe: %s\n' "$*"; }
[ "${SYNDEO_ALLOW_APP_TEST:-}" = yes ] || { say "refusing outside CI"; exit 2; }
PROXY="$1"

rm -rf "$LOGS"
mkdir -p "$LOGS"
sw_vers >"$LOGS/sw_vers.txt"

# bundle NAME: tlsprobe in an application bundle of its own.
bundle() {
  local app="$LOGS/$1.app"
  mkdir -p "$app/Contents/MacOS"
  sed -e 's/pageload/tlsprobe/g' "$here/macos-app/pageload.plist" >"$app/Contents/Info.plist"
  { swiftc -O -o "$app/Contents/MacOS/tlsprobe" "$here/macos-app/tlsprobe.swift" &&
    codesign --force --sign - --options runtime "$app"; } >"$LOGS/swiftc-$1.log" 2>&1 ||
    { say "could not build $1"; cat "$LOGS/swiftc-$1.log"; exit 2; }
}
bundle tlsprobe

PROXY_PID=''
stop_proxy() {
  [ -n "$PROXY_PID" ] || return 0
  kill "$PROXY_PID" 2>/dev/null
  wait "$PROXY_PID" 2>/dev/null
  PROXY_PID=''
}
trap stop_proxy EXIT

# start_proxy DIR: sets $at.
start_proxy() {
  mkdir -p "$1"
  SYNDEO_HOME="$1" SYNDEO_LOG=syndeo_proxy=debug,syndeo_net=info NO_COLOR=1 \
    "$PROXY" run --listen 127.0.0.1:0 --announce --trace-requests=false \
    >"$1/stdout" 2>"$1/stderr" &
  PROXY_PID=$!
  line=''
  for _ in $(seq 1 100); do
    line="$(head -n 1 "$1/stdout" 2>/dev/null)"
    case "$line" in "SYNDEO-PROXY-READY 1 "*) break ;; esac
    sleep 0.2
  done
  at="${line#SYNDEO-PROXY-READY 1 }"
}

# Another authority, from a proxy home of its own: not the one in use.
start_proxy "$LOGS/other"
stop_proxy
OTHER_CA="$LOGS/other/proxy/syndeo-ca.pem"

dir="$LOGS/run"
start_proxy "$dir"
say "proxy at $at"
start="$(date '+%Y-%m-%d %H:%M:%S')"

# probe MODE HOST [CA]
probe() {
  perl -e 'alarm 60; exec @ARGV' "$LOGS/tlsprobe.app/Contents/MacOS/tlsprobe" "$1" "$2" "$at" "${3:-$dir/proxy/syndeo-ca.pem}" \
    >>"$dir/probe.txt" 2>>"$dir/probe.stderr"
  grep -E "^(SPI|RESULT) $1 $2 |^SPI" "$dir/probe.txt" | tail -1 | cut -c1-300 | sed "s/^/  [${3:+other CA }$1] /"
}

probe webkit github.com
probe webkit example.com
for host in crates.io www.mozilla.org; do
  probe webkit "$host"
  probe webkit-keep "$host"
done
for host in github.com paypal.com en.wikipedia.org go.dev www.dropbox.com www.apple.com example.com crates.io; do
  probe webkit-spi "$host"
done
probe webkit-spi-persistent github.com
probe webkit-spi github.com "$OTHER_CA"
probe webkit-spi example.com "$OTHER_CA"
stop_proxy

sudo log show --start "$start" --style compact --info --debug \
  --predicate 'subsystem == "com.apple.CFNetwork" OR subsystem == "com.apple.securityd" OR process == "tlsprobe"' 2>&1 |
  cat >"$dir/unified.log"
say "CFNetwork's trust lines:"
grep -E 'strict TLS|TLS Trust result' "$dir/unified.log" |
  sed -E 's/^[0-9-]+ [0-9:.]+ +[A-Za-z]+ +//; s/\[[0-9]+:[0-9a-f]+\]//; s/Connection [0-9]+/Connection N/' |
  sort | uniq -c | sort -rn | head -10

echo
say "summary"
grep -hE '^RESULT' "$dir/probe.txt" | awk '{print "  " $2, $3, $4, $5}'
