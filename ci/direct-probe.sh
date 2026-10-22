#!/usr/bin/env bash
# EXPERIMENT (#58), removed before review: what excluding a domain from
# WebKit's proxy configuration does, on this runner's macOS.
#
#   ci/direct-probe.sh <syndeo-proxy>
#
# 1. Which hosts an excluded domain covers: itself, a subdomain, a name that
#    merely ends the same way; for "d", ".d" and "*.d".
# 2. Replacing the store's configuration between loads in one web view.
# 3. github.com and www.apple.com, loaded until every host macOS refuses
#    through the proxy is excluded: which hosts, and whether the page then
#    loads whole.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-/tmp}/direct-probe"
say() { printf 'direct-probe: %s\n' "$*"; }
[ "${SYNDEO_ALLOW_APP_TEST:-}" = yes ] || { say "refusing outside CI"; exit 2; }
PROXY="$1"

rm -rf "$LOGS"
mkdir -p "$LOGS/site"
sw_vers >"$LOGS/sw_vers.txt"
APP="$LOGS/directprobe.app"
mkdir -p "$APP/Contents/MacOS"
sed -e 's/pageload/directprobe/g' "$here/macos-app/pageload.plist" >"$APP/Contents/Info.plist"
{ swiftc -O -o "$APP/Contents/MacOS/directprobe" "$here/macos-app/directprobe.swift" &&
  codesign --force --sign - --options runtime "$APP"; } >"$LOGS/swiftc.log" 2>&1 ||
  { say "could not build directprobe"; cat "$LOGS/swiftc.log"; exit 2; }
PROBE="$APP/Contents/MacOS/directprobe"

# A local origin, under names of its own in /etc/hosts.
printf '<title>local</title><p>a local page</p>\n' >"$LOGS/site/index.html"
python3 -m http.server 0 --bind 127.0.0.1 --directory "$LOGS/site" >"$LOGS/origin.log" 2>&1 &
ORIGIN_PID=$!
PORT=''
for _ in $(seq 1 50); do
  PORT="$(lsof -nP -a -p "$ORIGIN_PID" -iTCP -sTCP:LISTEN 2>/dev/null | awk 'NR>1 {sub(/.*:/, "", $9); print $9; exit}')"
  [ -n "$PORT" ] && break
  sleep 0.2
done
cp /etc/hosts "$LOGS/hosts.before"
NAMES="probe-a.test x.probe-a.test xprobe-a.test probe-b.test probe-c.test"
for n in $NAMES; do printf '127.0.0.1 %s\n' "$n"; done | sudo tee -a /etc/hosts >/dev/null
PROXY_PID=''
stop_proxy() {
  [ -n "$PROXY_PID" ] || return 0
  kill "$PROXY_PID" 2>/dev/null
  wait "$PROXY_PID" 2>/dev/null
  PROXY_PID=''
}
finish() {
  stop_proxy
  kill "$ORIGIN_PID" 2>/dev/null
  sudo cp "$LOGS/hosts.before" /etc/hosts
}
trap finish EXIT
say "origin on port $PORT"

dir="$LOGS/run"
mkdir -p "$dir"
SYNDEO_HOME="$dir" SYNDEO_LOG=syndeo_proxy=debug,syndeo_net=info NO_COLOR=1 \
  "$PROXY" run --listen 127.0.0.1:0 --announce --dns system \
  >"$dir/stdout" 2>"$dir/stderr" &
PROXY_PID=$!
line=''
for _ in $(seq 1 100); do
  line="$(head -n 1 "$dir/stdout" 2>/dev/null)"
  case "$line" in "SYNDEO-PROXY-READY 1 "*) break ;; esac
  sleep 0.2
done
at="${line#SYNDEO-PROXY-READY 1 }"
CA="$dir/proxy/syndeo-ca.pem"
say "proxy at $at"

# tunnels_since LINE HOST: tunnels the proxy opened to HOST after line LINE of its log.
tunnels_since() { tail -n +"$(($1 + 1))" "$dir/stderr" | grep -c "tunnel opened host=$2\$"; }
mark() { wc -l <"$dir/stderr" | tr -d ' '; }
# refused_since LINE: hosts whose client fell back to a ClientHello without
# signature_algorithms after line LINE: macOS's refusal, as measured.
refused_since() { tail -n +"$(($1 + 1))" "$dir/stderr" | grep 'SignatureAlgorithmsExtensionRequired' | sed -E 's/.*host=([^ ]+) .*/\1/' | sort -u; }
probe() { perl -e 'alarm 150; exec @ARGV' "$PROBE" "$@" 2>>"$dir/probe.stderr" | tee -a "$dir/probe.txt"; }

echo
say "1. which hosts an excluded domain covers"
for ex in probe-a.test .probe-a.test '*.probe-a.test'; do
  for host in probe-a.test x.probe-a.test xprobe-a.test; do
    m="$(mark)"
    said="$(probe load "$at" "$CA" "$ex" "http://$host:$PORT/" | grep '^LOAD' | awk '{print $3}')"
    t="$(tunnels_since "$m" "$host")"
    printf '  excluded %-16s load %-16s %-4s %s\n' "$ex" "$host" "${said:-?}" "$([ "$t" -gt 0 ] && echo "through the proxy ($t tunnels)" || echo direct)"
  done
done

echo
say "2. the store's configuration replaced between loads, in one web view"
m="$(mark)"
probe update "$at" "$CA" "http://probe-b.test:$PORT/?one" probe-b.test "http://probe-b.test:$PORT/?two" "http://probe-c.test:$PORT/?three" | sed 's/^/  /'
printf '  tunnels: probe-b.test %s, probe-c.test %s\n' "$(tunnels_since "$m" probe-b.test)" "$(tunnels_since "$m" probe-c.test)"
printf '  the origin saw: %s\n' "$(grep -oE 'GET /\?(one|two|three)' "$LOGS/origin.log" | tr '\n' ' ')"

echo
say "3. loaded until every host macOS refuses through the proxy is excluded"
for url in https://github.com/ https://www.apple.com/; do
  host="${url#https://}"
  host="${host%/}"
  excluded=''
  for round in 1 2 3 4 5; do
    m="$(mark)"
    out="$(probe load "$at" "$CA" "${excluded:--}" "$url")"
    result="$(printf '%s\n' "$out" | grep '^LOAD' | tail -1 | cut -c1-400)"
    refused="$(refused_since "$m")"
    pinned="$(printf '%s\n' "$out" | grep 'pinned refused.*pinning requirements' | awk '{print $2}' | sort -u)"
    printf '  %s round %s: %s\n' "$host" "$round" "$result"
    printf '    refused through the proxy: %s; pinned by macOS: %s\n' "$(echo $refused)" "$(echo $pinned)"
    new=''
    case "$result" in *" FAIL "*) case ",$excluded," in *",$host,"*) ;; *) new="$host" ;; esac ;; esac
    for h in $refused $pinned; do case ",$excluded,$new," in *",$h,"*) ;; *) new="$new $h" ;; esac; done
    new="$(echo $new)"
    [ -n "$new" ] || break
    excluded="$(printf '%s\n' ${excluded//,/ } $new | sort -u | paste -sd, -)"
    printf '    now excluded: %s\n' "$excluded"
  done
done
