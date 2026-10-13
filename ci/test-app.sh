#!/usr/bin/env bash
# Syndeo.app on a disposable macOS runner, started the way a person starts it.
#
#   ci/test-app.sh launch <Syndeo.app>
#   ci/test-app.sh refuse <Syndeo.app whose proxy cannot start>
#   ci/test-app.sh mutant <name> <Syndeo.app with that safeguard taken out>
#   ci/test-app.sh routes <Syndeo.app>
#   ci/test-app.sh measure
#
# launch, which gates: open the app with `open`, never by running its
# executable, and require that:
#   - Launch Services has it running as com.sum.syndeo, from this bundle's
#     Contents/MacOS/Syndeo, with an on-screen window;
#   - its one child is the proxy, from this bundle's Contents/Helpers, and no
#     keystore process exists;
#   - its window holds a web view and no error;
#   - its menus have the usual shortcuts, read from the menu items;
#   - in it, as a person uses it — File > Open Location…, then the address
#     typed as keystrokes and Return: an address opens and shows the page's
#     own text, and the count on the proxy's own page, which only the proxy
#     answers, rises for it (example.com, with a query of this instance's
#     own, so that no copy WebKit cached for another instance stands in for
#     it); Back and Forward
#     work, by the bar's button and by History > Forward; words and a file:
#     address are refused with Syndeo's own page, not sent anywhere; File >
#     New Tab opens a tab with the start page, beside the first in one window,
#     and File's one enabled ⌘W item, whatever AppKit titles it (Close, or
#     Close Tab once it has seen the tabs), closes that tab and no other,
#     letting its window go and leaving the first as it was;
#   - nothing of a visited page's path or query is in the app's log or its
#     proxy's, for a page that opened and one whose host does not exist, nor
#     the query of any example.com page opened;
#   - https://example.com opens, and the proxy's count shows it fetched it,
#     though Syndeo's authority is in no trust settings: the window's own
#     delegate anchors it, and the trust settings are checked unchanged at
#     the end;
#   - on a page this test serves, each link and script to a scheme a window
#     may not show (file, mailto, ftp, blob, data), in the window or in a new
#     tab, leaves the page as it was, opens no tab, shows nothing of what it
#     points at and starts no other application, judged against what ran
#     just before that attempt (an application it did start is quit, and has
#     to be gone before the next attempt); and its link to a web page
#     in a new tab opens one;
#   - an address typed while a page loads is not overwritten when it arrives,
#     leaving the field shows the page's address again, and the field then
#     follows navigation as before;
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
# launch also measures, without gating: a page on this runner's own loopback,
# and whether the proxy's count moved for it; and how the proxy logged the
# fetch that failed.
#
# mutant, which gates: the app given is one of ci/macos-app/mutants.py's,
# with one safeguard taken out, and this passes only if the checks meant to
# catch it fail, at least one, and no other check does: https for
# no-pinning-delegate (while http, rendered and counted, still works), a
# page's links in the window for no-navigation-policy, in a new tab for
# any-scheme-in-a-new-tab, and typing kept through a page's arrival for
# no-editing-guard. Any other failure is a failure.
#
# routes, which only reports: what Syndeo.app's web views reach directly,
# through its proxy, or from WebKit's own cache. From a fresh WebKit state
# (Syndeo's WebKit caches and storage removed first) and with a fresh token in
# every address, so that no cached copy stands in for a fetch, it opens:
# loopback by IPv4, IPv6 and localhost; this Mac's .local name and LAN
# address; a public name for 127.0.0.1; redirects from loopback to the web and
# from a public name to loopback; pages with an image from the other kind of
# address; and a public page and a loopback page, each twice. For each it
# reports whether the page was shown, which process the origin
# (ci/macos-app/route-origin.py) saw each request from, WebKit's own or the
# proxy, and how the proxy's own count moved.
#
# measure, which only reports: what this runner allows a test to do —
# Accessibility, Apple Events to System Events, and whether a user-domain
# trust change raises an interactive authorization dialog — so the
# onboarding test is designed against what was seen, not assumed. The probe
# certificate it trusts is a throwaway, removed afterwards, and the trust
# settings have to be exactly as they were or this fails.
#
# Every mode changes the user's state, so each refuses unless
# SYNDEO_ALLOW_APP_TEST=yes, which only CI's disposable runners set.
set -uo pipefail

ID=com.sum.syndeo
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/syndeo-app-test"
fails=0

say() { printf 'test-app: %s\n' "$*"; }
ok() { printf '  PASS  %s\n' "$1"; }
FAILED=''
bad() {
  printf '  FAIL  %s — %s\n' "$1" "$2"
  fails=$((fails + 1))
  FAILED="$FAILED$1"$'\n'
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
# This instance's query for example.com (see started).
FRESH=''

# What example.com's page itself says. Its <title> is "Example Domain", and
# the window takes its title from the page through its controller, so that is
# no proof the page was shown; this sentence is in the page's body.
EXAMPLE_BODY='This domain is for use in documentation examples'

# started APP ROUND: open APP and check what is running: Launch Services has
# it as $ID, from APP, with an on-screen window, and its one child is the
# proxy from APP's Contents/Helpers; no keystore process exists. Sets PID and
# HELPER; returns non-zero if there is no app to check further.
started() {
  local app="$1" round="$2" children image windows
  PID='' HELPER=''
  # This instance's own query for example.com: random, and nothing else.
  # WebKit's cache persists from one instance to the next, so a page another
  # instance opened could otherwise be shown without this one fetching it.
  FRESH="fresh$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
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
controls() { printf 'Back %s, Forward %s' "$("$APPCTL" enabled "$PID" syndeo.back)" "$("$APPCTL" enabled "$PID" syndeo.forward)"; }

# arrive ADDRESS PAGE WHAT: within 30 s the front window's address field says
# ADDRESS and its page says PAGE, an extended regular expression.
arrive() {
  if wait_for 30 at "$1" "$2"; then ok "$3"; else bad "$3" "address field: '$(address)'; page: $(page_text); $(controls)"; fi
}

# choose MENU ITEM: bring the app to the front and press MENU > ITEM.
choose() {
  "$APPCTL" activate "$PID" >/dev/null || true
  "$APPCTL" menu "$PID" "$1" "$2" || bad "$1 > $2" "no such menu item to press; $1 has: $("$APPCTL" menu-items "$PID" "$1" | tr '\n' '|')"
}

# close_tab WHEN: press File's one enabled item whose shortcut is exactly
# Command-W, whatever AppKit titles it at that moment: Close, or Close Tab
# once it has seen the window's tabs.
close_tab() {
  local title
  "$APPCTL" activate "$PID" >/dev/null || true
  if title="$("$APPCTL" menu-key "$PID" File W)"; then
    ok "File has one enabled ⌘W item, '$title', and it was pressed ($1)"
  else
    bad "File's ⌘W item ($1)" "$title; File has: $("$APPCTL" menu-items "$PID" File | tr '\n' '|')"
  fi
}

# The app's log, its proxy's lines included, without terminal colours.
plain_log() { perl -pe 's/\e\[[0-9;]*m//g' "$1"; }

# visit TEXT ADDRESS PAGE WHAT: File > Open Location…, then TEXT typed and
# Return, as a person would; then require arrive ADDRESS PAGE.
visit() {
  choose File "Open Location…"
  if "$APPCTL" type "$PID" "$1" >/dev/null; then arrive "$2" "$3" "$4"; else bad "$4" "pid $PID could not be brought to the front to type into"; fi
}

# shortcuts: each menu item a person reaches by keyboard has its shortcut,
# Command and one key, as AppKit has it.
shortcuts() {
  local menu item key have
  while IFS='|' read -r menu item key; do
    have="$("$APPCTL" shortcut "$PID" "$menu" "$item")"
    must "$([ "$(printf '%s' "$have" | tr '[:lower:]' '[:upper:]')" = "$key 0" ] && echo yes)" "$menu > $item is ⌘$key" "${have:-no such item}"
  done <<'EOF'
Syndeo|Quit Syndeo|Q
File|New Tab|T
File|New Window|N
File|Open Location…|L
File|Close|W
View|Reload Page|R
View|Stop Loading|.
History|Back|[
History|Forward|]
EOF
}

# The free port of a loopback origin this test starts, and its pid. A plain
# TCPServer: http.server's own looks this host's name up when it binds, which
# on the runner took longer than the test waits. Its /slow.html answers 8 s
# after it is asked for, with SYNDEO-SLOW-PAGE. It serves policy.html: a
# page whose links and scripts go to schemes a window's main frame may not
# show, in the window and in a new tab, and one link to a web page in a new
# tab. What they point at says SYNDEO-...-PAGE, or is /etc/passwd.
ORIGIN_PORT='' ORIGIN_PID=''
origin() {
  local dir="$LOGS/origin"
  [ -n "$ORIGIN_PORT" ] && return 0
  mkdir -p "$dir"
  cat >"$dir/policy.html" <<'EOF'
<!doctype html><meta charset="utf-8"><title>Policy test</title>
<h1>Policy test</h1>
<p><a href="file:///etc/passwd">file link</a></p>
<p><a href="mailto:someone@example.com">mail link</a></p>
<p><a href="ftp://example.com/">ftp link</a></p>
<p><button onclick="location.href = URL.createObjectURL(new Blob(['<h1>SYNDEO-BLOB-PAGE</h1>'], {type: 'text/html'}))">blob navigation</button></p>
<p><button onclick="location.href = 'data:text/html,<h1>SYNDEO-DATA-PAGE</h1>'">data navigation</button></p>
<p><a href="file:///etc/passwd" target="_blank">file in a new tab</a></p>
<p><a href="mailto:someone@example.com" target="_blank">mail in a new tab</a></p>
<p><button onclick="window.open(URL.createObjectURL(new Blob(['<h1>SYNDEO-BLOB-PAGE</h1>'], {type: 'text/html'})))">blob in a new tab</button></p>
<p><button onclick="window.open('data:text/html,<h1>SYNDEO-DATA-PAGE</h1>')">data in a new tab</button></p>
<p><a href="http://example.com/" target="_blank">Example in a new tab</a></p>
EOF
  python3 -I -u -c '
import http.server, os, socketserver, sys, time
os.chdir(sys.argv[1])
class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if not self.path.startswith("/slow.html"):
            return super().do_GET()
        time.sleep(8)
        body = b"<!doctype html><title>Slow page</title><p>SYNDEO-SLOW-PAGE</p>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
socketserver.ThreadingTCPServer.daemon_threads = True
server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
print("port", server.server_address[1], flush=True)
server.serve_forever()' "$dir" >"$LOGS/origin.log" 2>&1 &
  ORIGIN_PID=$!
  wait_for 10 grep -q '^port [0-9]' "$LOGS/origin.log" || { bad "loopback origin" "it did not start: $(head -c 200 "$LOGS/origin.log")"; return 1; }
  ORIGIN_PORT="$(sed -n 's/^port \([0-9][0-9]*\)$/\1/p' "$LOGS/origin.log" | head -n 1)"
}
# origin_stop: stop the loopback origin, which has to be running until then
# and stop for the SIGTERM sent, and nothing else. Reaped here, so the shell
# does not report the stop it was asked for; any other end, or a traceback in
# its log, is a failure.
origin_stop() {
  local status
  [ -n "$ORIGIN_PID" ] || return 0
  if kill -0 "$ORIGIN_PID" 2>/dev/null; then
    kill "$ORIGIN_PID"
    wait "$ORIGIN_PID" 2>/dev/null
    status=$?
    [ "$status" = 143 ] || bad "loopback origin" "it ended with status $status, not from the SIGTERM sent: $(tail -n 3 "$LOGS/origin.log" | tr '\n' ' ')"
  else
    wait "$ORIGIN_PID" 2>/dev/null
    status=$?
    bad "loopback origin" "it had already ended, status $status, before it was stopped: $(tail -n 3 "$LOGS/origin.log" | tr '\n' ' ')"
  fi
  if grep -q 'Traceback' "$LOGS/origin.log"; then
    bad "loopback origin" "it raised: $(grep -A3 'Traceback' "$LOGS/origin.log" | tail -n 1)"
  fi
  ORIGIN_PORT='' ORIGIN_PID=''
}

# counted WHEN: open the proxy's own page, which only the proxy answers, and
# set COUNTED to the requests it has counted so far.
COUNTED=''
counted() {
  visit "http://syndeo.local/stats" "http://syndeo.local/stats" 'requests [0-9]+' "the proxy's own page opens ($1)"
  COUNTED="$(front | grep -oE 'requests [0-9]+' | head -n 1 | cut -d' ' -f2)"
}

# pinned: https through the proxy, whose authority is in no trust settings
# (they are checked unchanged at the end), opens only because the window's
# own delegate answers the challenge with that authority as the anchor. The
# proxy's count shows the window reached it through the proxy.
pinned() {
  local before
  counted "before https"
  before="$COUNTED"
  visit "https://example.com/?$FRESH" "https://example.com/?$FRESH" "$EXAMPLE_BODY" \
    "https://example.com opens, with this instance's own query, and shows its own text, with Syndeo's authority in no trust settings"
  counted "after https"
  risen "$before" "and the proxy fetched it over https"
}

# risen BEFORE WHAT: COUNTED, the proxy's count read since, is above BEFORE.
risen() {
  must "$([ -n "$1" ] && [ -n "$COUNTED" ] && [ "$COUNTED" -gt "$1" ] && echo yes)" \
    "$2: the proxy's count went from ${1:-?} to ${COUNTED:-?}"
}

# plain_http: http://example.com, typed with this instance's own query, opens
# and shows its own text, and the proxy's count shows the window reached it
# through the proxy.
plain_http() {
  local before
  counted "before http"
  before="$COUNTED"
  visit "http://example.com/?$FRESH" "http://example.com/?$FRESH" "$EXAMPLE_BODY" \
    "http://example.com, typed with this instance's own query, opens on Return and shows its own text"
  counted "after http"
  risen "$before" "and the proxy fetched it over http"
}

# unlogged LOG: this instance's query, on every example.com page it opened,
# is in no log line of the app's or its proxy's.
unlogged() {
  must "$([ -n "$FRESH" ] && ! grep -q "$FRESH" "$1" && echo yes)" \
    "the query of each example.com page opened is in no log, the app's or its proxy's" "$(grep -c "$FRESH" "$1") lines"
}

# private LOG: visit a page with a path and a query, and one whose host does
# not exist, and require nothing of either path or query in LOG, the app's
# log, which holds its proxy's lines too.
private() {
  local log="$1"
  visit "http://example.com/syndeo-private-path?syndeo-private-query=1" \
    "http://example.com/syndeo-private-path?syndeo-private-query=1" '^AXWebArea' "a page with a path and a query opens"
  visit "http://nonexistent.invalid/syndeo-private-path?syndeo-private-query=2" \
    "http://nonexistent.invalid/syndeo-private-path?syndeo-private-query=2" '^AXWebArea' "a page whose host does not exist is tried"
  must "$(grep -q 'proxy up' "$log" && echo yes)" "the proxy's own log lines are in the app's log"
  must "$(grep -q 'syndeo-private' "$log" && echo no || echo yes)" \
    "no path or query of either is in the app's log or its proxy's" "$(plain_log "$log" | grep 'syndeo-private' | head -n 2 | tr '\n' ' ')"
  printf '  MEASURE  the failed fetch, as the proxy logged it: %s\n' "$(plain_log "$log" | grep 'fetch failed' | tail -n 1 | cut -c1-220)"
}

# policy LOG: on policy.html, each link and script to another scheme has to
# leave the page as it was, open no tab, show nothing of what it points at,
# and start no other application; and the link to a web page in a new tab
# opens one, made as every tab is.
policy() {
  local log="$1" page label where opened apps started_now problems id how
  origin || return 1
  page="http://127.0.0.1:$ORIGIN_PORT/policy.html"
  visit "$page" "$page" 'Policy test' "a page with links and scripts to other schemes opens"
  while IFS='|' read -r label where; do
    at "$page" 'Policy test' || visit "$page" "$page" 'Policy test' "the page with links and scripts, again"
    opened="$(grep -c 'a window opened' "$log")"
    # What runs, taken immediately before this attempt and for it alone.
    apps="$("$APPCTL" apps)"
    if ! "$APPCTL" press-web "$PID" "$label"; then
      bad "policy: $label is refused $where" "no such link or button on the page: $(page_text | head -c 160)"
      continue
    fi
    sleep 3
    problems=''
    [ "$(grep -c 'a window opened' "$log")" = "$opened" ] || problems="$problems a tab opened;"
    at "$page" 'Policy test' || problems="$problems the window left the page (address '$(address)', page: $(page_text | head -c 120));"
    if "$APPCTL" ax-tree "$PID" | grep -qE 'root:|SYNDEO-(BLOB|DATA)-PAGE'; then problems="$problems what it points at is shown;"; fi
    started_now="$(comm -13 <(printf '%s\n' "$apps") <("$APPCTL" apps))"
    [ -z "$started_now" ] || problems="$problems another application started: $(printf '%s' "$started_now" | tr '\n' ' ');"
    if [ -z "$problems" ]; then ok "policy: $label is refused $where"; else bad "policy: $label is refused $where" "$problems"; fi
    # Any application it started, and only those, is quit, and has to be
    # gone before the next attempt; one that will not go is a failure.
    for id in $started_now; do
      if how="$("$APPCTL" stop "$id")"; then
        printf '        %s, started by %s, was stopped (%s)\n' "$id" "$label" "$how"
      else
        bad "cleanup after $label" "$id, which it started, is $how"
      fi
    done
    # A tab it opened is closed again, so the next attempt starts as this one
    # did: its opening has already been counted against it.
    if [ "$(grep -c 'a window opened' "$log")" != "$opened" ]; then
      "$APPCTL" activate "$PID" >/dev/null || true
      "$APPCTL" menu-key "$PID" File W >/dev/null 2>&1 || true
      sleep 1
    fi
  done <<'EOF'
file link|in the window
mail link|in the window
ftp link|in the window
blob navigation|in the window
data navigation|in the window
file in a new tab|and opens no tab
mail in a new tab|and opens no tab
blob in a new tab|and opens no tab
data in a new tab|and opens no tab
EOF
  at "$page" 'Policy test' || visit "$page" "$page" 'Policy test' "the page with links and scripts, again"
  opened="$(grep -c 'a window opened' "$log")"
  "$APPCTL" press-web "$PID" "Example in a new tab" || bad "a link to a web page in a new tab opens one" "no such link on the page"
  if wait_for 15 sh -c '[ "$(grep -c "a window opened" "$1")" -gt "$2" ]' _ "$log" "$opened"; then
    ok "a link to a web page in a new tab opens one"
  else
    bad "a link to a web page in a new tab opens one" "the app reports no new window: $(grep 'a window' "$log" | tail -n 2 | tr '\n' ' ')"
  fi
  arrive "http://example.com/" "$EXAMPLE_BODY" "and the new tab shows that page"
  close_tab "the tab a link opened"
  arrive "$page" 'Policy test' "closing it leaves the page with the link as it was"
}

# editing: what someone types in the address field while a page loads stays
# theirs. From a page this test serves, /slow.html is opened, and while it
# loads (it answers 8 s later) another address is typed and not given. When
# the page arrives, the field still says what was typed; leaving it with Tab
# shows the page's address again; and the field follows navigation as before,
# Back showing the page before.
editing() {
  local first slow typed="http://example.com/typed-while-loading" said
  origin || return 1
  first="http://127.0.0.1:$ORIGIN_PORT/policy.html"
  slow="http://127.0.0.1:$ORIGIN_PORT/slow.html"
  visit "$first" "$first" 'Policy test' "a page to start from opens"
  choose File "Open Location…"
  "$APPCTL" type "$PID" "$slow" >/dev/null || bad "typing $slow" "pid $PID could not be brought to the front"
  choose File "Open Location…"
  "$APPCTL" type-only "$PID" "$typed" >/dev/null || bad "typing while it loads" "pid $PID could not be brought to the front"
  if wait_for 30 page_says 'SYNDEO-SLOW-PAGE'; then
    ok "the slow page arrives while the field holds an address typed and not given"
  else
    bad "the slow page arrives" "not within 30 s: $(page_text)"
  fi
  # Every report of its arrival (commit, finish) made by now.
  sleep 2
  said="$("$APPCTL" value "$PID" syndeo.address)"
  must "$([ "$said" = "$typed" ] && echo yes)" \
    "typing in the address field while a page loads: the page's arrival leaves what was typed" "the field says '$said'"
  "$APPCTL" tab "$PID" >/dev/null || bad "Tab" "pid $PID could not be brought to the front"
  arrive "$slow" 'SYNDEO-SLOW-PAGE' "leaving the field with Tab shows the page's address again"
  "$APPCTL" press "$PID" syndeo.back || bad "Back" "no Back button to press in the front window"
  arrive "$first" 'Policy test' "and the field follows navigation again: Back shows the page before"
}

# Browse as a person would, in the first window and a tab beside it.
browsing() {
  local app="$1" round=browse log on_screen have before
  say "browse: addresses, Back and Forward, refusals, a tab, privacy, https, a page's own links"
  started "$app" "$round" || return
  helper_of "$app" "$round"
  browses "$round"
  log="$LOGS/app-$round.stderr"
  shortcuts
  arrive "" 'Welcome to Syndeo' "a new window shows the start page, with an empty address field ($round)"
  plain_http
  "$APPCTL" press "$PID" syndeo.back || bad "Back" "no Back button to press in the front window"
  arrive "http://example.com/?$FRESH" "$EXAMPLE_BODY" "the bar's Back button goes back"
  choose History Forward
  arrive "http://syndeo.local/stats" 'requests [1-9]' "History > Forward goes forward"
  visit "bread" "bread" 'does not search' "a word is refused by Syndeo's own page, which says it was not sent anywhere"
  visit "file:///etc/passwd" "file:///etc/passwd" 'not file: ones' "a file: address is refused"
  must "$(page_says 'root:' && echo no || echo yes)" "and nothing of that file is shown"

  choose File "New Tab"
  if wait_for 15 grep -q 'a window opened open=2' "$log"; then ok "File > New Tab opens a second tab"; else bad "New Tab" "the app does not report two windows: $(grep 'a window' "$log" | tail -n 3 | tr '\n' ' ')"; fi
  arrive "" 'Welcome to Syndeo' "the new tab shows the start page, with an empty address field"
  on_screen="$("$APPCTL" windows "$PID" | wc -l | tr -d ' ')"
  must "$([ "$on_screen" = 1 ] && echo yes)" "the two are tabs of one window: one window on screen" "$on_screen on screen"
  "$APPCTL" titles "$PID" >"$LOGS/browse-tabs.txt" 2>&1
  "$APPCTL" front-tree "$PID" >"$LOGS/browse-two-tabs-tree.txt" 2>&1
  "$APPCTL" menu-items "$PID" File >"$LOGS/browse-two-tabs-file-menu.txt" 2>&1
  close_tab "with two tabs"
  if wait_for 15 grep -q 'a window closed open=1' "$log"; then ok "it closes the tab in front, one window and no other, and the app lets it go"; else bad "Close" "the app does not report one window: $(grep 'a window' "$log" | tail -n 3 | tr '\n' ' ')"; fi
  arrive "file:///etc/passwd" 'not file: ones' "the first tab is in front again, as it was left"
  on_screen="$("$APPCTL" windows "$PID" | wc -l | tr -d ' ')"
  must "$([ "$on_screen" = 1 ] && echo yes)" "and its window is still the one on screen" "$on_screen on screen"
  must "$(alive "$PID" && [ "$(pgrep -P "$PID" | tr -d '\n')" = "$HELPER" ] && echo yes)" "the app and its proxy run on (pids $PID, $HELPER)"

  private "$log"
  pinned
  unlogged "$log"

  # Measured, not gated: a page on this runner's loopback, and the proxy's
  # count around it.
  if origin; then
    counted "before a loopback page"
    before="$COUNTED"
    visit "http://127.0.0.1:$ORIGIN_PORT/policy.html" "http://127.0.0.1:$ORIGIN_PORT/policy.html" 'Policy test' "a page on this runner's loopback opens"
    counted "after a loopback page"
    printf '  MEASURE  http://127.0.0.1:%s/: the origin saw %s request(s); the proxy'"'"'s count went from %s to %s\n' \
      "$ORIGIN_PORT" "$(grep -c 'GET /policy.html ' "$LOGS/origin.log")" "${before:-?}" "${COUNTED:-?}"
  fi
  policy "$log"
  editing
  origin_stop
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

# mutant NAME APP: APP is Syndeo.app with NAME's safeguard taken out. Run what
# should catch it, and pass only if the checks meant to catch it failed, at
# least one of them, and nothing else did: every other check, from launch
# through quitting, is the mutant's isolation, and an unrelated failure is a
# failure here too. For no-pinning-delegate that includes http through the
# proxy, rendered and counted, so it is the pinning that failed.
mutant() {
  local name="${1:-}" app intended log caught unrelated
  [ -n "$name" ] && [ -d "${2:-}" ] || { say "usage: $0 mutant <name> <Syndeo.app>"; exit 2; }
  app="$(cd "$2" && pwd -P)"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  printf '\nSyndeo.app with a safeguard taken out (%s): %s\n' "$name" "$app"
  case "$name" in
    no-pinning-delegate) intended='^(https://example\.com opens|and the proxy fetched it over https:)' ;;
    no-navigation-policy) intended='^policy: .* is refused in the window$' ;;
    any-scheme-in-a-new-tab) intended='^policy: .* is refused and opens no tab$' ;;
    no-editing-guard) intended="^typing in the address field while a page loads" ;;
    *) say "no mutant $name"; exit 2 ;;
  esac
  if started "$app" "mutant-$name"; then
    helper_of "$app" "mutant-$name"
    browses "mutant-$name"
    log="$LOGS/app-mutant-$name.stderr"
    case "$name" in
      no-pinning-delegate) plain_http; pinned; unlogged "$log" ;;
      no-editing-guard) editing ;;
      *) policy "$log" ;;
    esac
  fi
  origin_stop
  stopped "mutant-$name"
  printf '\n'
  caught="$(printf '%s' "$FAILED" | grep -E "$intended")"
  unrelated="$(printf '%s' "$FAILED" | grep -vE "$intended")"
  if [ -n "$unrelated" ]; then
    printf 'MUTANT WRONG  %s: a check it was not meant to fail failed: %s\n' "$name" "$(printf '%s' "$unrelated" | tr '\n' ';')"
    return 1
  fi
  if [ -z "$caught" ]; then
    printf 'MUTANT SURVIVED  %s: no check matching /%s/ failed\n' "$name" "$intended"
    return 1
  fi
  printf 'MUTANT CAUGHT  %s, by: %s\n' "$name" "$(printf '%s' "$caught" | tr '\n' ';')"
  return 0
}

# The routing origin (ci/macos-app/route-origin.py): its port, whether it
# also answers on ::1, and its pid.
ROUTE_PORT='' ROUTE_V6='' ROUTE_PID=''
route_origin() {
  python3 -I -u "$here/macos-app/route-origin.py" "$LOGS/routes-origin.jsonl" >"$LOGS/routes-origin.out" 2>&1 &
  ROUTE_PID=$!
  if ! wait_for 10 grep -q '^port [0-9]' "$LOGS/routes-origin.out"; then
    bad "routing origin" "it did not start: $(head -c 200 "$LOGS/routes-origin.out")"
    return 1
  fi
  ROUTE_PORT="$(sed -n 's/^port \([0-9][0-9]*\) v6 .*/\1/p' "$LOGS/routes-origin.out")"
  ROUTE_V6="$(sed -n 's/^port [0-9]* v6 \(.*\)$/\1/p' "$LOGS/routes-origin.out")"
}
route_origin_stop() {
  local status
  [ -n "$ROUTE_PID" ] || return 0
  if kill -0 "$ROUTE_PID" 2>/dev/null; then
    kill "$ROUTE_PID"
    wait "$ROUTE_PID" 2>/dev/null
    status=$?
    [ "$status" = 143 ] || bad "routing origin" "it ended with status $status, not from the SIGTERM sent"
  else
    wait "$ROUTE_PID" 2>/dev/null
    bad "routing origin" "it had already ended, status $?, before it was stopped: $(tail -n 3 "$LOGS/routes-origin.out" | tr '\n' ' ')"
  fi
  if grep -q 'Traceback' "$LOGS/routes-origin.out"; then
    bad "routing origin" "it raised: $(grep -A3 'Traceback' "$LOGS/routes-origin.out" | tail -n 1)"
  fi
  ROUTE_PID=''
}

# A fresh token, for one address: random, and nothing else.
token() { printf 'r%s' "$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"; }

# seen FROM TOKEN...: what the origin was asked, after its first FROM log
# lines, for these tokens: each path, how many times, and from which process.
seen() {
  python3 -I - "$LOGS/routes-origin.jsonl" "$@" <<'PY'
import collections, json, sys
log, start, tokens = sys.argv[1], int(sys.argv[2]), set(sys.argv[3:])
counts = collections.Counter()
with open(log, encoding="utf-8") as f:
    for line in f.readlines()[start:]:
        entry = json.loads(line)
        if entry["t"] in tokens:
            counts[(entry["path"], entry["process"])] += 1
print("; ".join(f"{n}x {path} from {process}" for (path, process), n in sorted(counts.items())) or "nothing")
PY
}

# route LABEL URL MARKER TOKEN...: open URL, typed as a person would, and
# report whether MARKER was shown, what the origin saw for TOKEN..., and the
# proxy's count before and after.
route() {
  local label="$1" url="$2" marker="$3" before after shown from saw
  shift 3
  counted "before: $label"
  before="$COUNTED"
  from="$(wc -l <"$LOGS/routes-origin.jsonl" | tr -d ' ')"
  choose File "Open Location…"
  "$APPCTL" type "$PID" "$url" >/dev/null || true
  if wait_for 20 page_says "$marker"; then shown=yes; else shown=no; fi
  # Its images, or a redirect's second request, by now too.
  sleep 2
  saw="$(seen "$from" "$@")"
  counted "after: $label"
  after="$COUNTED"
  printf '  MEASURE  %s: %s\n           shown: %s; the origin saw: %s; the proxy'"'"'s count: %s -> %s\n' \
    "$label" "$url" "$shown" "$saw" "${before:-?}" "${after:-?}"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$label" "$url" "$shown" "$saw" "${before:-?}" "${after:-?}" >>"$LOGS/routes.tsv"
}

# routes APP: see the top of this script. Reports; never fails for what it
# finds.
routes() {
  local app d iface lan local_name resolved t s P
  [ -d "${1:-}" ] || { say "usage: $0 routes <Syndeo.app>"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  printf '\nwhat Syndeo.app'"'"'s web views reach directly, through its proxy, or from cache (measured; not a gate)\n'
  # A fresh WebKit state: nothing an earlier instance cached or stored.
  for d in "$HOME/Library/Caches/$ID" "$HOME/Library/WebKit/$ID" "$HOME/Library/HTTPStorages/$ID" "$HOME/Library/HTTPStorages/$ID.binarycookies"; do
    if [ -e "$d" ]; then rm -rf "$d" && printf '  cleared %s\n' "$d"; fi
  done
  : >"$LOGS/routes.tsv"
  : >"$LOGS/routes-origin.jsonl"
  route_origin || return 0
  P="$ROUTE_PORT"
  iface="$(/sbin/route -n get default 2>/dev/null | awk '/interface:/ {print $2}')"
  lan="$([ -n "$iface" ] && ipconfig getifaddr "$iface" 2>/dev/null)"
  local_name="$(scutil --get LocalHostName 2>/dev/null)"
  local_name="${local_name:+$local_name.local}"
  resolved="$([ -n "$local_name" ] && dscacheutil -q host -a name "$local_name" 2>/dev/null | awk '/_address:/ {print $2}' | tr '\n' ' ')"
  printf '  this runner: the default route on %s, address %s; %s resolves to %s; the origin on port %s, IPv6 %s\n' \
    "${iface:-?}" "${lan:-none}" "${local_name:-no .local name}" "${resolved:-nothing}" "$P" "$ROUTE_V6"
  started "$app" routes || { route_origin_stop; return 0; }
  helper_of "$app" routes
  browses routes

  t="$(token)"; route "IPv4 loopback" "http://127.0.0.1:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"
  if [ "$ROUTE_V6" = yes ]; then t="$(token)"; route "IPv6 loopback" "http://[::1]:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"; fi
  t="$(token)"; route "localhost" "http://localhost:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"
  if [ -n "$local_name" ]; then t="$(token)"; route "this Mac's .local name" "http://$local_name:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"; fi
  if [ -n "$lan" ]; then t="$(token)"; route "this Mac's LAN address" "http://$lan:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"; fi
  t="$(token)"; route "a public name for 127.0.0.1" "http://127.0.0.1.nip.io:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"
  t="$(token)"; s="$(token)"
  route "a redirect from loopback to a public page" "http://127.0.0.1:$P/redirect?t=$t&to=http://example.com/?$s" "$EXAMPLE_BODY" "$t"
  t="$(token)"; s="$(token)"
  route "a redirect from a public name to loopback" "http://127.0.0.1.nip.io:$P/redirect?t=$t&to=http://127.0.0.1:$P/page?t=$s" "ROUTE-PAGE-$s" "$t" "$s"
  t="$(token)"; s="$(token)"
  route "a loopback page with an image from a public name" "http://127.0.0.1:$P/page?t=$t&sub=http://127.0.0.1.nip.io:$P/img?t=$s" "ROUTE-PAGE-$t" "$t" "$s"
  t="$(token)"; s="$(token)"
  route "a public-name page with a loopback image" "http://127.0.0.1.nip.io:$P/page?t=$t&sub=http://127.0.0.1:$P/img?t=$s" "ROUTE-PAGE-$t" "$t" "$s"
  if [ -n "$lan" ]; then
    t="$(token)"; s="$(token)"
    route "a public-name page with an image from the LAN address" "http://127.0.0.1.nip.io:$P/page?t=$t&sub=http://$lan:$P/img?t=$s" "ROUTE-PAGE-$t" "$t" "$s"
  fi
  if [ -n "$local_name" ]; then
    t="$(token)"; s="$(token)"
    route "a public-name page with an image from the .local name" "http://127.0.0.1.nip.io:$P/page?t=$t&sub=http://$local_name:$P/img?t=$s" "ROUTE-PAGE-$t" "$t" "$s"
  fi
  t="$(token)"
  route "a public page, the first time" "http://example.com/?$t" "$EXAMPLE_BODY" "$t"
  route "the same public page, again" "http://example.com/?$t" "$EXAMPLE_BODY" "$t"
  t="$(token)"
  route "a loopback page, the first time" "http://127.0.0.1:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"
  route "the same loopback page, again" "http://127.0.0.1:$P/page?t=$t" "ROUTE-PAGE-$t" "$t"

  stopped routes
  route_origin_stop
  printf '\n  %d of its own checks failed (reported, not gated)\n' "$fails"
  return 0
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
  mutant) shift; mutant "$@" ;;
  routes) shift; routes "$@" ;;
  measure) measure ;;
  *) say "usage: $0 launch <Syndeo.app> | refuse <Syndeo.app> | mutant <name> <Syndeo.app> | routes <Syndeo.app> | measure"; exit 2 ;;
esac
