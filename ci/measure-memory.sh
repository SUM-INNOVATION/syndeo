#!/usr/bin/env bash
# Compare the browser's memory against Safari's, on the same page, honestly.
#
#   ci/measure-memory.sh <url> [--autoplay]
#
# Written because measuring this by hand produced two wrong published numbers
# in one afternoon, both in the same direction, and both from the same two
# mistakes:
#
#   1. Attribution. WebKit's WebContent processes are XPC services parented to
#      launchd, so "the new process" is whichever appeared in the window — and
#      if Safari is open it spawns its own. Ours are told apart by not holding a
#      file under Safari's container.
#
#   2. Picking the wrong process. A page with an iframe gets two WebContent
#      processes: the main frame, and a small one for the embed. Reading the
#      small one gave "Safari uses 50 MB on YouTube" when its main frame was
#      using 969 MB. Every process is printed here, so the mistake is visible
#      rather than averaged away.
#
# Memory also climbs for the first half-minute on a heavy page, so both sides
# are sampled over the same interval rather than snapshotted at whatever moment
# the script happened to reach.
set -uo pipefail

URL="${1:?usage: measure-memory.sh <url> [--autoplay]}"
shift || true
EXTRA=("$@")
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${SYNDEO_BIN:-$HERE/target/release}"
SAMPLES=6
EVERY=8

wc_pids() { pgrep -f 'com.apple.WebKit.WebContent' | sort -n; }
rss_mb()  { local kb; kb=$(ps -o rss= -p "$1" 2>/dev/null | tr -d ' '); [ -n "$kb" ] && echo $((kb / 1024)) || echo 0; }
is_safari() { lsof -nP -p "$1" 2>/dev/null | grep -q 'com.apple.Safari'; }

echo "page: $URL"
echo

echo "syndeo-webkit"
before=$(wc_pids | tr '\n' ' ')
"$BIN/syndeo-webkit" --proxy 127.0.0.1:8899 \
  --proxy-ca "${SYNDEO_HOME:-$HOME/.syndeo}/proxy/syndeo-ca.pem" "${EXTRA[@]}" "$URL" \
  >/dev/null 2>&1 &
host=$!
sleep 12
ours=()
for p in $(wc_pids); do
  echo " $before " | grep -q " $p " && continue
  is_safari "$p" && continue
  ours+=("$p")
done
for i in $(seq 1 $SAMPLES); do
  line=""; total=$(rss_mb "$host")
  for p in "${ours[@]:-}"; do [ -n "$p" ] || continue; m=$(rss_mb "$p"); line+="  webcontent $p: ${m} MB"; total=$((total + m)); done
  printf '  t+%-3ss  host: %s MB%s  =  %s MB\n' "$((12 + (i-1)*EVERY))" "$(rss_mb "$host")" "$line" "$total"
  sleep $EVERY
done
kill "$host" 2>/dev/null
echo

echo "Safari, same page, same interval"
before=$(wc_pids | tr '\n' ' ')
osascript -e "tell application \"Safari\" to make new document with properties {URL:\"$URL\"}" >/dev/null 2>&1
sleep 12
theirs=()
for p in $(wc_pids); do echo " $before " | grep -q " $p " || theirs+=("$p"); done
for i in $(seq 1 $SAMPLES); do
  line=""; total=0
  for p in "${theirs[@]:-}"; do [ -n "$p" ] || continue; m=$(rss_mb "$p"); line+="  webcontent $p: ${m} MB"; total=$((total + m)); done
  printf '  t+%-3ss %s  =  %s MB (excludes Safari'"'"'s browser process, shared with its other tabs)\n' \
    "$((12 + (i-1)*EVERY))" "$line" "$total"
  sleep $EVERY
done
osascript -e 'tell application "Safari"
 repeat with w in windows
  try
   if (URL of current tab of w) is equal to "'"$URL"'" then close w
  end try
 end repeat
end tell' >/dev/null 2>&1
echo "  (window closed)"
