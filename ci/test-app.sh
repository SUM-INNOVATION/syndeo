#!/usr/bin/env bash
# Syndeo.app on a disposable macOS runner, started the way a person starts it.
#
#   ci/test-app.sh launch <Syndeo.app>
#   ci/test-app.sh refuse <Syndeo.app whose proxy cannot start>
#   ci/test-app.sh measure
#
# launch, which gates: open the app with `open`, never by running its
# executable, and require that:
#   - Launch Services has it running as com.sum.syndeo, from this bundle's
#     Contents/MacOS/Syndeo, with an on-screen window;
#   - its one child is the proxy, from this bundle's Contents/Helpers, and no
#     keystore process exists;
#   - its window holds a web view and no error;
#   - in it, typed the way a person types: an address opens; the proxy's own
#     page, which only the proxy answers, has counted that request; Back and
#     Forward work, by the bar's button and by ⌘]; words and a file: address
#     are refused with Syndeo's own page, not sent anywhere; File > New Tab
#     opens a tab with the start page, beside the first in one window, and ⌘W
#     closes it, letting its window go and leaving the first as it was;
#   - asked to quit as the Quit menu item would, it exits, and its proxy
#     with it;
#   - with its last window closed, through the window's own close button, the
#     window is let go, the same app and the same proxy keep running, and
#     opening the bundle again brings a window back in that same process, with
#     that same proxy;
#   - opened again, all of that holds again, and quitting again leaves nothing;
#   - it made no keystore changes: no keystore in its home and no keychain
#     item for one;
#   - the user's trust settings are as they were.
#
# refuse, which gates: a bundle whose proxy cannot start opens all the same,
# its window says it cannot browse and holds no web view, and no helper runs.
#
# launch also measures, without gating: whether an https page opens while the
# proxy's authority is not trusted, and whether a page on this runner's own
# loopback opens, and a link on it to a new tab opens one.
#
# measure, which only reports: what this runner allows a test to do —
# Accessibility, Apple Events to System Events, and whether a user-domain
# trust change raises an interactive authorization dialog — so the
# onboarding test is designed against what was seen, not assumed. The probe
# certificate it trusts is a throwaway, removed afterwards, and the trust
# settings have to be exactly as they were or this fails.
#
# Both change the user's state, so both refuse unless
# SYNDEO_ALLOW_APP_TEST=yes, which only CI's disposable runners set.
set -uo pipefail

ID=com.sum.syndeo
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/syndeo-app-test"
fails=0

say() { printf 'test-app: %s\n' "$*"; }
ok() { printf '  PASS  %s\n' "$1"; }
bad() {
  printf '  FAIL  %s — %s\n' "$1" "$2"
  fails=$((fails + 1))
}
must() { [ "$1" = yes ] && ok "$2" || bad "$2" "${3:-}"; }

[ "$(uname -s)" = Darwin ] || { say "macOS only"; exit 2; }
[ "${SYNDEO_ALLOW_APP_TEST:-}" = yes ] || {
  say "refusing: this opens an application and changes the user's state; only on a disposable runner, with SYNDEO_ALLOW_APP_TEST=yes"
  exit 2
}
mkdir -p "$LOGS"
APPCTL="$LOGS/appctl"
swiftc -O -o "$APPCTL" "$here/macos-app/appctl.swift" >"$LOGS/swiftc.log" 2>&1 || { say "could not build appctl"; cat "$LOGS/swiftc.log"; exit 2; }

# The path of the image pid runs, as the kernel has it.
image_of() {
  python3 -I -c '
import ctypes, sys
proc = ctypes.CDLL("/usr/lib/libproc.dylib")
buf = ctypes.create_string_buffer(4096)
n = proc.proc_pidpath(int(sys.argv[1]), buf, 4096)
print(buf.value.decode() if n > 0 else "")' "$1"
}
alive() { kill -0 "$1" 2>/dev/null; }
# wait_for SECONDS COMMAND...: until COMMAND succeeds, or the time is up.
wait_for() {
  local until=$((SECONDS + $1))
  shift
  while ! "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$until" ] || return 1
    sleep 0.5
  done
}
gone() { ! alive "$1"; }
the_app_runs() { [ -n "$("$APPCTL" pids "$ID")" ]; }
no_app() { [ -z "$("$APPCTL" pids "$ID")" ]; }
keystore_item() { security find-generic-password -s com.sum.syndeo.keystore >/dev/null 2>&1 && echo present || echo absent; }

CANNOT_BROWSE="Syndeo can't open websites right now."

# started APP ROUND: open APP and check what is running: Launch Services has
# it as $ID, from APP, with an on-screen window, and its one child is the
# proxy from APP's Contents/Helpers; no keystore process exists. Sets PID and
# HELPER; returns non-zero if there is no app to check further.
started() {
  local app="$1" round="$2" children image windows
  PID='' HELPER=''
  open --stdout "$LOGS/app-$round.stdout" --stderr "$LOGS/app-$round.stderr" "$app" || { bad "open ($round)" "open failed"; return 1; }
  if ! wait_for 30 the_app_runs; then
    bad "running ($round)" "no running application is $ID 30 s after open"
    return 1
  fi
  PID="$("$APPCTL" pids "$ID" | head -n 1)"
  must "$([ "$("$APPCTL" pids "$ID" | wc -l | tr -d ' ')" = 1 ] && echo yes)" "one running application is $ID ($round, pid $PID)" "$("$APPCTL" pids "$ID" | tr '\n' ' ')"
  must "$([ "$("$APPCTL" bundle "$PID")" = "$ID" ] && echo yes)" "Launch Services has pid $PID as $ID ($round)" "$("$APPCTL" bundle "$PID")"
  image="$(image_of "$PID")"
  must "$([ "$image" = "$app/Contents/MacOS/Syndeo" ] && echo yes)" "it runs $app/Contents/MacOS/Syndeo ($round)" "$image"
  if wait_for 30 "$APPCTL" windows "$PID"; then
    windows="$("$APPCTL" windows "$PID" | tr '\n' ';')"
    ok "it has an on-screen window: $windows ($round)"
  else
    bad "window ($round)" "no on-screen window 30 s after open"
  fi
  must "$(pgrep -x syndeo-keystore >/dev/null && echo no || echo yes)" "no keystore process exists ($round)" "$(pgrep -lx syndeo-keystore)"
  return 0
}

# helper_of ROUND: require pid PID's one child to be APP's proxy; sets HELPER.
helper_of() {
  local app="$1" round="$2" children image
  wait_for 15 pgrep -P "$PID" || true
  children="$(pgrep -P "$PID" | tr '\n' ' ')"
  # shellcheck disable=SC2086 # pids, split on purpose
  set -- $children
  must "$([ "$#" = 1 ] && echo yes)" "exactly one helper runs under it ($round)" "children: ${children:-none}"
  HELPER="${1:-}"
  if [ -n "$HELPER" ]; then
    image="$(image_of "$HELPER")"
    must "$([ "$image" = "$app/Contents/Helpers/syndeo-proxy" ] && echo yes)" "that helper is $app/Contents/Helpers/syndeo-proxy ($round)" "$image"
  fi
}

# browses ROUND: PID's window holds a web view (an AXWebArea) and no error.
has_web_area() { "$APPCTL" ax-tree "$PID" | grep -q '^AXWebArea'; }
browses() {
  if wait_for 20 has_web_area; then ok "its window holds a web view ($1)"; else bad "web view ($1)" "no AXWebArea in its window: $("$APPCTL" ax-tree "$PID" | cut -f1 | sort | uniq -c | tr '\n' ' ')"; fi
  must "$("$APPCTL" ax-tree "$PID" | grep -qF "$CANNOT_BROWSE" && echo no || echo yes)" "and no error ($1)"
}

# stopped ROUND: ask the app to quit, as the Quit menu item would, and
# require it and its helper gone.
stopped() {
  local round="$1"
  "$APPCTL" quit "$ID" || bad "quit ($round)" "nothing was asked to quit"
  if [ -n "$PID" ]; then
    if wait_for 20 gone "$PID"; then ok "asked to quit, it exited ($round)"; else bad "quit ($round)" "pid $PID still running 20 s later"; fi
  fi
  if [ -n "$HELPER" ]; then
    if wait_for 10 gone "$HELPER"; then ok "its helper exited with it ($round)"; else bad "helper ($round)" "pid $HELPER still running 10 s after the app"; fi
  fi
  if wait_for 10 no_app; then ok "nothing runs as $ID ($round)"; else bad "after quit ($round)" "$("$APPCTL" pids "$ID" | tr '\n' ' ')"; fi
}

# The front window, as Accessibility has it; its address field; its page.
front() { "$APPCTL" front-tree "$PID"; }
address() { "$APPCTL" value "$PID" syndeo.address; }
page_says() { front | grep -qE "$1"; }
# at ADDRESS PAGE: there is an address field, it says ADDRESS, and the page says PAGE.
at() {
  local said
  said="$(address)" || return 1
  [ "$said" = "$1" ] && page_says "$2"
}
page_text() { front | grep -E '^AX(StaticText|Heading)' | cut -f2,3 | tr '\t\n' '  ' | head -c 300; }

# arrive ADDRESS PAGE WHAT: within 30 s the front window's address field says
# ADDRESS and its page says PAGE, an extended regular expression.
arrive() {
  if wait_for 30 at "$1" "$2"; then ok "$3"; else bad "$3" "address field: '$(address)'; page: $(page_text)"; fi
}

# visit TEXT ADDRESS PAGE WHAT: type TEXT into the address field and press
# Return, as a person would, and require arrive ADDRESS PAGE.
visit() {
  if "$APPCTL" type "$PID" "$1" >/dev/null; then arrive "$2" "$3" "$4"; else bad "$4" "pid $PID could not be brought to the front to type into"; fi
}

# The free port of a loopback origin this test starts, and its pid.
ORIGIN_PORT='' ORIGIN_PID=''
origin() {
  local dir="$LOGS/origin"
  mkdir -p "$dir"
  printf '%s\n' '<!doctype html><title>Loopback origin</title><h1>Loopback origin</h1>' \
    '<p><a href="http://example.com/" target="_blank">Example in a new tab</a></p>' >"$dir/index.html"
  python3 -u -m http.server --bind 127.0.0.1 --directory "$dir" 0 >"$LOGS/origin.log" 2>&1 &
  ORIGIN_PID=$!
  wait_for 10 grep -q 'port [0-9]' "$LOGS/origin.log" || return 1
  ORIGIN_PORT="$(sed -n 's/.* port \([0-9][0-9]*\).*/\1/p' "$LOGS/origin.log" | head -n 1)"
}

# Browse as a person would, in the first window and a tab beside it.
browsing() {
  local app="$1" round=browse log outcome on_screen
  say "browse: addresses, Back and Forward, refusals, a tab"
  started "$app" "$round" || return
  helper_of "$app" "$round"
  browses "$round"
  log="$LOGS/app-$round.stderr"
  arrive "" 'Welcome to Syndeo' "a new window shows the start page, with an empty address field ($round)"
  visit "http://example.com" "http://example.com/" 'Example Domain' "an address typed into the field opens on Return"
  visit "http://syndeo.local/stats" "http://syndeo.local/stats" 'requests [1-9]' \
    "the proxy's own page, which only the proxy answers, opens and has counted that request"
  "$APPCTL" press "$PID" syndeo.back || bad "Back" "no Back button to press in the front window"
  arrive "http://example.com/" 'Example Domain' "the bar's Back button goes back"
  "$APPCTL" keys "$PID" 'cmd+]' >/dev/null || bad "Forward" "pid $PID could not be brought to the front"
  arrive "http://syndeo.local/stats" 'requests [1-9]' "History > Forward (⌘]) goes forward"
  visit "bread" "bread" 'does not search' "a word is refused by Syndeo's own page, which says it was not sent anywhere"
  visit "file:///etc/passwd" "file:///etc/passwd" 'not file: ones' "a file: address is refused"
  must "$(page_says 'root:' && echo no || echo yes)" "and nothing of that file is shown"

  "$APPCTL" activate "$PID" >/dev/null || true
  "$APPCTL" menu "$PID" File "New Tab" || bad "New Tab" "no File > New Tab to press"
  if wait_for 15 grep -q 'a window opened open=2' "$log"; then ok "File > New Tab opens a second tab"; else bad "New Tab" "the app does not report two windows: $(grep 'a window' "$log" | tail -n 3 | tr '\n' ' ')"; fi
  arrive "" 'Welcome to Syndeo' "the new tab shows the start page, with an empty address field"
  on_screen="$("$APPCTL" windows "$PID" | wc -l | tr -d ' ')"
  must "$([ "$on_screen" = 1 ] && echo yes)" "the two are tabs of one window: one window on screen" "$on_screen on screen"
  "$APPCTL" titles "$PID" >"$LOGS/browse-tabs.txt" 2>&1
  "$APPCTL" front-tree "$PID" >"$LOGS/browse-two-tabs-tree.txt" 2>&1
  "$APPCTL" keys "$PID" cmd+w >/dev/null || bad "Close" "pid $PID could not be brought to the front"
  if wait_for 15 grep -q 'a window closed open=1' "$log"; then ok "File > Close (⌘W) closes the tab, and the app lets its window go"; else bad "Close" "the app does not report one window: $(grep 'a window' "$log" | tail -n 3 | tr '\n' ' ')"; fi
  arrive "file:///etc/passwd" 'not file: ones' "the first tab is in front again, as it was left"
  must "$(alive "$PID" && [ "$(pgrep -P "$PID" | tr -d '\n')" = "$HELPER" ] && echo yes)" "the app and its proxy run on (pids $PID, $HELPER)"

  # Measured, not gated.
  "$APPCTL" type "$PID" "https://example.com" >/dev/null || true
  if wait_for 30 page_says "Example Domain|couldn't open"; then
    if page_says 'Example Domain'; then outcome="it opened"; else outcome="Syndeo's failure page: $(page_text)"; fi
  else
    outcome="neither the page nor a failure page after 30 s: $(page_text)"
  fi
  printf '  MEASURE  https://example.com, the authority not trusted: %s\n' "$outcome"
  if origin; then
    "$APPCTL" type "$PID" "http://127.0.0.1:$ORIGIN_PORT/" >/dev/null || true
    if wait_for 30 page_says "Loopback origin|couldn't open"; then
      if page_says 'Loopback origin'; then outcome="it opened"; else outcome="Syndeo's failure page: $(page_text)"; fi
    else
      outcome="neither the page nor a failure page after 30 s: $(page_text)"
    fi
    printf '  MEASURE  a page on this runner'"'"'s loopback, http://127.0.0.1:%s/: %s; the origin saw: %s\n' \
      "$ORIGIN_PORT" "$outcome" "$(grep -c 'GET / ' "$LOGS/origin.log") request(s) for /"
    if page_says 'Loopback origin' && "$APPCTL" press-link "$PID" "Example in a new tab"; then
      if wait_for 15 grep -q 'a window opened open=2' "$log"; then
        wait_for 30 page_says 'Example Domain' || true
        printf '  MEASURE  a target=_blank link opened a tab; in front: %s, %s\n' "$(address)" "$(page_text | head -c 80)"
      else
        printf '  MEASURE  a target=_blank link opened no tab in 15 s\n'
      fi
    fi
    kill "$ORIGIN_PID" 2>/dev/null
  else
    printf '  MEASURE  could not start a loopback origin: %s\n' "$(head -c 200 "$LOGS/origin.log")"
    kill "$ORIGIN_PID" 2>/dev/null
  fi
  stopped "$round"
}

# One launch: open, check everything, quit, and require nothing left.
one_launch() {
  local app="$1" round="$2"
  say "launch $round"
  started "$app" "$round" || return
  helper_of "$app" "$round"
  browses "$round"
  stopped "$round"
}

# The Dock's way back: close the last window without quitting, require the
# same app and proxy to stay, then open the bundle again, as a Dock click or
# Finder would, and require a window back in that same process, with that
# same proxy.
reopened() {
  local app="$1" round=reopen pid helper
  say "close the last window, then open again"
  started "$app" "$round" || return
  helper_of "$app" "$round"
  pid="$PID" helper="$HELPER"
  "$APPCTL" ax-close "$pid" || bad "close ($round)" "no window had a close button to press"
  if wait_for 15 sh -c '! "$1" windows "$2"' _ "$APPCTL" "$pid"; then ok "its last window closed ($round)"; else bad "close ($round)" "a window is still on screen"; fi
  if wait_for 10 grep -q 'a window closed open=0' "$LOGS/app-$round.stderr"; then ok "and the app let that window go ($round)"; else bad "close ($round)" "the app does not report no windows: $(grep 'a window' "$LOGS/app-$round.stderr" | tail -n 3 | tr '\n' ' ')"; fi
  sleep 2
  must "$(alive "$pid" && [ "$("$APPCTL" pids "$ID")" = "$pid" ] && echo yes)" "with no window, the same app runs on (pid $pid)" "$("$APPCTL" pids "$ID" | tr '\n' ' ')"
  must "$([ -n "$helper" ] && alive "$helper" && echo yes)" "and so does its proxy (pid $helper)"
  open "$app" || bad "open again ($round)" "open failed"
  if wait_for 30 "$APPCTL" windows "$pid"; then ok "opened again, a window is back in pid $pid ($round)"; else bad "reopen ($round)" "no window 30 s after open"; fi
  must "$([ "$("$APPCTL" pids "$ID" | tr -d '\n')" = "$pid" ] && echo yes)" "it is still the one application, pid $pid ($round)" "$("$APPCTL" pids "$ID" | tr '\n' ' ')"
  must "$([ "$(pgrep -P "$pid" | tr -d '\n')" = "$helper" ] && echo yes)" "with the same proxy, pid $helper ($round)" "children: $(pgrep -P "$pid" | tr '\n' ' ')"
  must "$([ "$(grep -c 'a window opened open=1' "$LOGS/app-$round.stderr")" = 2 ] && echo yes)" "a new window, the second this process opened ($round)" "$(grep 'a window' "$LOGS/app-$round.stderr" | tr '\n' ' ')"
  browses "$round"
  stopped "$round"
}

launch() {
  local app keystore_before home
  [ -d "${1:-}" ] || { say "usage: $0 launch <Syndeo.app>"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  home="$HOME/Library/Application Support/Syndeo"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  security dump-trust-settings >"$LOGS/trust-before.txt" 2>&1 || true
  keystore_before="$(keystore_item)"
  printf '\nSyndeo.app, opened as a person opens it: %s\n' "$app"
  one_launch "$app" first
  browsing "$app"
  reopened "$app"
  one_launch "$app" again
  printf '\nwhat it left\n'
  must "$([ ! -e "$home/.keystore" ] && echo yes)" "no keystore in its home ($home)" "$(ls -la "$home/.keystore" 2>&1 | head -3)"
  must "$([ "$keystore_before" = absent ] && [ "$(keystore_item)" = absent ] && echo yes)" "no keychain item for a keystore, before or after" "before: $keystore_before, after: $(keystore_item)"
  security dump-trust-settings >"$LOGS/trust-after.txt" 2>&1 || true
  must "$(cmp -s "$LOGS/trust-before.txt" "$LOGS/trust-after.txt" && echo yes)" "the user's trust settings are as they were" "$(diff "$LOGS/trust-before.txt" "$LOGS/trust-after.txt" | head -5)"
  printf '\n%d failed\n' "$fails"
  [ "$fails" = 0 ]
}

# refuse APP: APP's helper cannot start. The app has to open all the same,
# and its window has to say it cannot browse, hold no web view, and leave no
# helper running.
refuse() {
  local app
  [ -d "${1:-}" ] || { say "usage: $0 refuse <Syndeo.app>"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  printf '\nSyndeo.app whose proxy cannot start: %s\n' "$app"
  started "$app" refuse || { printf '\n%d failed\n' "$fails"; return 1; }
  if wait_for 20 sh -c '"$1" ax-tree "$2" | grep -qF "$3"' _ "$APPCTL" "$PID" "$CANNOT_BROWSE"; then
    ok "its window says: $CANNOT_BROWSE"
  else
    bad "error view" "no '$CANNOT_BROWSE' in its window: $("$APPCTL" ax-tree "$PID" | head -5 | tr '\n' ' ')"
  fi
  "$APPCTL" ax-tree "$PID" >"$LOGS/refuse-ax-tree.txt"
  must "$(grep -q '^AXWebArea' "$LOGS/refuse-ax-tree.txt" && echo no || echo yes)" "and holds no web view" "$(grep -c '^AXWebArea' "$LOGS/refuse-ax-tree.txt") web areas"
  if wait_for 10 sh -c '[ -z "$(pgrep -P "$1")" ]' _ "$PID"; then ok "and no helper runs under it"; else bad "helper" "children: $(pgrep -P "$PID" | tr '\n' ' ')"; fi
  HELPER=''
  stopped refuse
  printf '\n%d failed\n' "$fails"
  [ "$fails" = 0 ]
}

# measure: report what this runner allows; fail only if it cannot put the
# trust settings back.
measure() {
  local probe out status started
  printf '\nwhat this runner allows (measured; not a gate)\n'
  printf '  MEASURE  Accessibility for this process: %s\n' "$("$APPCTL" ax)"
  started=$SECONDS
  out="$(perl -e 'alarm 20; exec @ARGV' osascript -e 'tell application "System Events" to count processes' 2>&1)" && status=0 || status=$?
  printf '  MEASURE  Apple Events to System Events: exit %s after %ss: %s\n' "$status" "$((SECONDS - started))" "$(printf '%s' "$out" | head -c 160 | tr '\n' ' ')"

  probe="$LOGS/probe"
  mkdir -p "$probe"
  security dump-trust-settings >"$probe/before.txt" 2>&1 || true
  if ! openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj "/CN=Syndeo CI trust probe $$" \
    -keyout "$probe/key.pem" -out "$probe/cert.pem" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" >"$probe/openssl.log" 2>&1; then
    printf '  MEASURE  trust change: could not make a probe certificate: %s\n' "$(tail -n 1 "$probe/openssl.log")"
    return 0
  fi
  started=$SECONDS
  out="$(perl -e 'alarm 45; exec @ARGV' security add-trusted-cert -r trustRoot -p ssl \
    -k "$HOME/Library/Keychains/login.keychain-db" "$probe/cert.pem" 2>&1)" && status=0 || status=$?
  printf '  MEASURE  user-domain trust of a probe CA (security add-trusted-cert): exit %s after %ss%s: %s\n' \
    "$status" "$((SECONDS - started))" "$([ "$status" = 142 ] && echo ', killed at 45 s: it waited for someone')" \
    "$(printf '%s' "$out" | head -c 200 | tr '\n' ' ')"
  # Put it back: the trust setting, then the certificate.
  perl -e 'alarm 45; exec @ARGV' security remove-trusted-cert "$probe/cert.pem" >"$probe/remove.log" 2>&1 || true
  local sha1
  sha1="$(openssl x509 -in "$probe/cert.pem" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
  security delete-certificate -Z "$sha1" "$HOME/Library/Keychains/login.keychain-db" >"$probe/delete.log" 2>&1 || true
  security dump-trust-settings >"$probe/after.txt" 2>&1 || true
  if cmp -s "$probe/before.txt" "$probe/after.txt" && ! security find-certificate -Z -a "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null | grep -qi "$sha1"; then
    printf '  MEASURE  the probe is removed, and the trust settings are exactly as before\n'
  else
    printf '  FAIL     the trust settings were not put back:\n'
    diff "$probe/before.txt" "$probe/after.txt" | head -10 | sed 's/^/           /'
    return 1
  fi
}

case "${1:-}" in
  launch) shift; launch "$@" ;;
  refuse) shift; refuse "$@" ;;
  measure) measure ;;
  *) say "usage: $0 launch <Syndeo.app> | refuse <Syndeo.app> | measure"; exit 2 ;;
esac
