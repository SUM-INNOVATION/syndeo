#!/usr/bin/env bash
# EXPERIMENT (#58), removed before review: hosts through Syndeo's proxy on
# this runner's macOS, by WebKit and by a bare Network framework connection.
# Logs in $RUNNER_TEMP/hsts-probe.
#
#   ci/hsts-probe.sh <syndeo-proxy>
#
# 1. Preloaded and other hosts, loaded once each (webkit, nw).
# 2. Hosts loaded twice in one data store: two that send their own HSTS
#    header (www.mozilla.org, crates.io), one that does not (www.rust-lang.org).
# 3. Preloaded hosts in an ephemeral data store.
# 4. The same in a bundle whose ATS allows arbitrary loads everywhere, not
#    only in web content: measured, never proposed.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-/tmp}/hsts-probe"
say() { printf 'hsts-probe: %s\n' "$*"; }
[ "${SYNDEO_ALLOW_APP_TEST:-}" = yes ] || { say "refusing outside CI"; exit 2; }
PROXY="$1"

rm -rf "$LOGS"
mkdir -p "$LOGS"
sw_vers >"$LOGS/sw_vers.txt"

# bundle NAME PLIST-SED: tlsprobe in an application bundle of its own.
bundle() {
  local app="$LOGS/$1.app"
  mkdir -p "$app/Contents/MacOS"
  sed -e 's/pageload/tlsprobe/g' -e "$2" "$here/macos-app/pageload.plist" >"$app/Contents/Info.plist"
  { swiftc -O -o "$app/Contents/MacOS/tlsprobe" "$here/macos-app/tlsprobe.swift" &&
    codesign --force --sign - --options runtime "$app"; } >"$LOGS/swiftc-$1.log" 2>&1 ||
    { say "could not build $1"; cat "$LOGS/swiftc-$1.log"; exit 2; }
}
bundle tlsprobe 's/x/x/'
bundle tlsprobe-ats 's|<key>NSAllowsArbitraryLoadsInWebContent</key>|<key>NSAllowsArbitraryLoads</key><true/><key>NSAllowsArbitraryLoadsInWebContent</key>|'

PROXY_PID=''
stop_proxy() {
  [ -n "$PROXY_PID" ] || return 0
  kill "$PROXY_PID" 2>/dev/null
  wait "$PROXY_PID" 2>/dev/null
  PROXY_PID=''
}
trap stop_proxy EXIT

dir="$LOGS/run"
mkdir -p "$dir"
SYNDEO_HOME="$dir" SYNDEO_LOG=syndeo_proxy=debug,syndeo_net=info NO_COLOR=1 \
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
say "proxy at $at"
start="$(date '+%Y-%m-%d %H:%M:%S')"

# probe BUNDLE MODE HOST
probe() {
  perl -e 'alarm 60; exec @ARGV' "$LOGS/$1.app/Contents/MacOS/tlsprobe" "$2" "$3" "$at" "$dir/proxy/syndeo-ca.pem" \
    >>"$dir/probe-$1.txt" 2>>"$dir/probe-$1.stderr"
  grep -E "^(FIRST|RESULT) $2 $3 " "$dir/probe-$1.txt" | tail -2 | cut -c1-300 | sed "s/^/  [$1] /"
}

for host in github.com paypal.com en.wikipedia.org go.dev www.dropbox.com \
  example.com www.rust-lang.org www.mozilla.org crates.io www.apple.com; do
  probe tlsprobe nw "$host"
  probe tlsprobe webkit "$host"
done
for host in www.mozilla.org crates.io www.rust-lang.org; do
  probe tlsprobe webkit-twice "$host"
done
for host in github.com paypal.com; do
  probe tlsprobe webkit-ephemeral "$host"
done
for host in github.com paypal.com; do
  probe tlsprobe-ats webkit "$host"
done
probe tlsprobe-ats webkit-twice www.mozilla.org
stop_proxy

sudo log show --start "$start" --style compact --info --debug \
  --predicate 'subsystem == "com.apple.CFNetwork" OR subsystem == "com.apple.securityd" OR process == "tlsprobe"' 2>&1 |
  cat >"$dir/unified.log"
say "unified log: $(wc -l <"$dir/unified.log") lines; CFNetwork's trust lines:"
grep -E 'strict TLS|TLS Trust result|HSTS|ATS exception' "$dir/unified.log" |
  sed -E 's/^[0-9-]+ [0-9:.]+ +[A-Za-z]+ +//; s/\[[0-9]+:[0-9a-f]+\]//; s/Connection [0-9]+/Connection N/; s/Task <[^>]+>\.<[0-9]+>/Task T/' |
  sort | uniq -c | sort -rn | head -20
say "the proxy's TLS lines:"
grep -E 'tls handshake|incompatible' "$dir/stderr" | sed -E 's/^[^ ]+ +//' | cut -c1-200 | sort | uniq -c | sort -rn | head -10

echo
say "summary"
for b in tlsprobe tlsprobe-ats; do
  grep -hE '^(FIRST|RESULT)' "$dir/probe-$b.txt" | awk -v b="$b" '{print "  " b, $1, $2, $3, $4, $5}'
done
