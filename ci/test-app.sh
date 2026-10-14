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
#     work, by the bar's button and by History > Forward; a file: address is
#     refused with Syndeo's own page; File >
#     New Tab opens a tab with the start page, beside the first in one window,
#     and File's one enabled ⌘W item, whatever AppKit titles it (Close, or
#     Close Tab once it has seen the tabs), closes that tab and no other,
#     letting its window go and leaving the first as it was;
#   - search: Syndeo > Settings… offers DuckDuckGo, Startpage, Brave Search,
#     Google, Bing and Off, with DuckDuckGo chosen before anyone chooses, and
#     keeps what is chosen; with Off, a word is refused with Syndeo's own page
#     and the proxy's count does not move; with DuckDuckGo, words typed, and
#     the field then left, send nothing, and Return opens DuckDuckGo's fixed
#     https address with them encoded, through the proxy; the log says a
#     search was made with DuckDuckGo and nothing of what was searched for;
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
#   - the user's trust settings are as they were, and the search engine it
#     chose is removed again.
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
# any-scheme-in-a-new-tab, typing kept through a page's arrival for
# no-editing-guard, the bar for a name for direct-for-every-name, and a link
# to an IPv6 address with a zone for zoned-links-followed. Any other failure
# is a failure.
#
# routes, which gates: the bar's route against what WebKit actually does,
# the proxy caching nothing from this Mac, and Clear Browsing Data. From a
# fresh WebKit state (Syndeo's WebKit caches and storage removed first) and
# with a fresh token in every address, so that no cached copy stands in for a
# fetch, it types each address and requires what address::route says:
#   - Direct, the bar saying "Direct — not cached by Syndeo", and WebKit's own
#     networking process, not the proxy, seen by the origin
#     (ci/macos-app/route-origin.py): localhost and localhost., 127.0.0.1,
#     [::1], [::ffff:127.0.0.1], and 127/8's last host address and 127.0.0.2,
#     aliased on lo0 for the time it gates; and 127.0.0.5, which is not
#     assigned, by the proxy's count not moving;
#   - Through Syndeo, the bar saying "Through Syndeo", and the proxy seen:
#     every name, whatever it resolves to — this Mac's .local name, a public
#     name for 127.0.0.1, a single label (in /etc/hosts for that time), a
#     name under .localhost; and, by the proxy's own count, a genuinely
#     remote public address (example.com's, never aliased) and the IPv4
#     addresses just outside each range, the bar saying so on the proxy's
#     own page;
#   - Local, where which way WebKit goes varies and is recorded, not
#     asserted: the bar saying "Local — not cached by Syndeo", and the
#     proxy's cache taking nothing from a page any cache may keep — this
#     Mac's LAN address and every local-network address aliased on lo0
#     (10/8, 172.16/12, 192.168/16, 169.254/16 and 100.64/10 addresses,
#     each range's first and last host address, and IPv6 unique local ones);
#     and, by the proxy's count, local-network addresses that are not this
#     Mac's, the bar saying Local if a page comes back;
#   - after a redirect, the final page's route: a name redirecting to
#     127.0.0.1 is Direct, one redirecting to 10.255.42.1 is Local, and
#     127.0.0.1 redirecting to a public page is Through Syndeo;
#   - an IPv6 address with a zone is refused, typed and followed from a
#     page's link, with the same words, and the origin hears of neither;
#   - an https image from another host, on a page this Mac serves, comes
#     through the proxy with Syndeo's authority in no trust settings.
# It measures, without gating, [::], and IPv6 addresses this runner has no
# route to: just outside the ranges, and unique local and link-local ones.
# Then: a public page is cached by the proxy, and a cacheable page from this
# Mac, reached through the proxy by a name, is not; and Settings > Clear
# Browsing Data asks first, then empties the proxy's cache (the same proxy
# runs on) and website storage, and WebKit's cache stops answering for a page
# it had kept. It puts lo0 and /etc/hosts back and requires them, and the
# trust settings, exactly as they were.
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
Syndeo|Settings…|,
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
# set COUNTED to the requests it has counted so far, and ENTRIES to how many
# entries its cache holds.
COUNTED='' ENTRIES=''
counted() {
  visit "http://syndeo.local/stats" "http://syndeo.local/stats" 'requests [0-9]+' "the proxy's own page opens ($1)"
  COUNTED="$(front | grep -oE 'requests [0-9]+' | head -n 1 | cut -d' ' -f2)"
  ENTRIES="$(front | grep -oE 'entries [0-9]+' | head -n 1 | cut -d' ' -f2)"
}

# settings_open WHEN: Syndeo > Settings…, and the Settings window in front.
settings_open() {
  choose Syndeo "Settings…"
  if wait_for 10 "$APPCTL" value "$PID" syndeo.search; then
    ok "Syndeo > Settings… opens Settings ($1)"
  else
    bad "Settings ($1)" "no Settings window in front: $("$APPCTL" titles "$PID" | tr '\n' '|')"
  fi
}

# settings_close WHEN: close the Settings window as File > Close would,
# leaving a browser window in front.
settings_close() {
  "$APPCTL" activate "$PID" >/dev/null || true
  "$APPCTL" menu-key "$PID" File W >/dev/null || bad "closing Settings ($1)" "File has not exactly one enabled ⌘W item"
  if wait_for 10 "$APPCTL" value "$PID" syndeo.address; then
    ok "closing Settings leaves a browser window in front ($1)"
  else
    bad "closing Settings ($1)" "no address field in front: $("$APPCTL" titles "$PID" | tr '\n' '|')"
  fi
}

# chosen ENGINE: the stored search engine is ENGINE.
chosen() { [ "$(defaults read "$ID" SearchEngine 2>/dev/null)" = "$1" ]; }

# searching LOG: Settings offers the six choices, DuckDuckGo chosen before
# anyone chose; with Off, a word is refused and nothing is sent; with
# DuckDuckGo, words typed are sent nowhere, and leaving the field sends
# nothing either, until Return, which opens DuckDuckGo's fixed https address
# with them encoded, through the proxy; and nothing of what was searched for
# is in a log.
searching() {
  local log="$1" before items query='syndeo search & test' expected='https://duckduckgo.com/?q=syndeo+search+%26+test'
  must "$(defaults read "$ID" SearchEngine >/dev/null 2>&1 && echo no || echo yes)" "no search engine has been chosen yet" "$(defaults read "$ID" SearchEngine 2>&1)"
  settings_open "search"
  must "$([ "$("$APPCTL" value "$PID" syndeo.search)" = DuckDuckGo ] && echo yes)" \
    "Settings shows DuckDuckGo as the search engine, before anyone chose one" "it shows '$("$APPCTL" value "$PID" syndeo.search)'"
  items="$("$APPCTL" popup-items "$PID" syndeo.search | tr '\n' '|')"
  must "$([ "$items" = 'DuckDuckGo|Startpage|Brave Search|Google|Bing|Off — Syndeo does not search|' ] && echo yes)" \
    "it offers DuckDuckGo, Startpage, Brave Search, Google, Bing and Off" "$items"
  "$APPCTL" pick "$PID" syndeo.search "Off — Syndeo does not search" || bad "choosing Off" "no such item to choose"
  if wait_for 5 chosen off; then ok "choosing Off is kept"; else bad "choosing Off" "the setting is '$(defaults read "$ID" SearchEngine 2>&1)'"; fi
  settings_close "search off"
  counted "before a word, with search off"
  before="$COUNTED"
  visit "bread" "bread" 'does not search' "with search off, a word is refused by Syndeo's own page, which says it was not sent anywhere"
  counted "after a word, with search off"
  must "$([ -n "$before" ] && [ "$COUNTED" = "$before" ] && echo yes)" "and nothing was sent: the proxy's count stayed at ${before:-?}" "now ${COUNTED:-?}"

  settings_open "search on"
  "$APPCTL" pick "$PID" syndeo.search "DuckDuckGo" || bad "choosing DuckDuckGo" "no such item to choose"
  if wait_for 5 chosen duckduckgo; then ok "choosing DuckDuckGo is kept"; else bad "choosing DuckDuckGo" "the setting is '$(defaults read "$ID" SearchEngine 2>&1)'"; fi
  settings_close "search on"
  counted "before typing words"
  before="$COUNTED"
  choose File "Open Location…"
  "$APPCTL" type-only "$PID" "$query" >/dev/null || bad "typing words" "pid $PID could not be brought to the front"
  sleep 3
  "$APPCTL" tab "$PID" >/dev/null || bad "Tab" "pid $PID could not be brought to the front"
  arrive "http://syndeo.local/stats" 'requests [0-9]+' "words typed, then the field left without Return: the page stays, and the field shows its address again"
  counted "after typing words without Return"
  must "$([ -n "$before" ] && [ "$COUNTED" = "$before" ] && echo yes)" \
    "nothing was sent while the words were typed or when the field was left: the proxy's count stayed at ${before:-?}" "now ${COUNTED:-?}"
  visit "$query" "$expected" '^AXWebArea' "words, on Return, are a search at DuckDuckGo's fixed https address, encoded: $expected"
  sleep 3
  counted "after a search"
  risen "$before" "and the search went through the proxy"
  must "$(grep 'a search, on Return' "$log" | grep -qE 'engine="?duckduckgo' && echo yes)" "the app logged a search with DuckDuckGo, and which engine only" \
    "$(grep 'a search' "$log" | tail -n 1)"
  must "$(grep -qiE 'syndeo( |\+|%20)search' "$log" && echo no || echo yes)" "nothing of what was searched for is in the app's log or its proxy's" \
    "$(grep -ciE 'syndeo( |\+|%20)search' "$log") lines"
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
  searching "$log"
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
  defaults delete "$ID" SearchEngine >/dev/null 2>&1 || true
  must "$(defaults read "$ID" SearchEngine >/dev/null 2>&1 && echo no || echo yes)" "the search engine this test chose is removed again"
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
    direct-for-every-name) intended='^the bar for a name' ;;
    zoned-links-followed) intended='^a link to an IPv6 address with a zone' ;;
    *) say "no mutant $name"; exit 2 ;;
  esac
  if started "$app" "mutant-$name"; then
    helper_of "$app" "mutant-$name"
    browses "mutant-$name"
    log="$LOGS/app-mutant-$name.stderr"
    case "$name" in
      no-pinning-delegate) plain_http; pinned; unlogged "$log" ;;
      no-editing-guard) editing ;;
      direct-for-every-name | zoned-links-followed) routes_lite ;;
      *) policy "$log" ;;
    esac
  fi
  origin_stop
  route_origin_stop
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
  : >"$LOGS/routes-origin.jsonl"
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

# The origin's log lines so far, to read only what comes after.
origin_lines() { wc -l <"$LOGS/routes-origin.jsonl" | tr -d ' '; }

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

# way FROM TOKEN: which way the requests for TOKEN after the origin's first
# FROM lines came: "direct" if every one came from WebKit's own networking
# process, "syndeo" if every one came from the proxy, "none" if there were
# none, and what there was otherwise.
way() {
  python3 -I - "$LOGS/routes-origin.jsonl" "$1" "$2" <<'PY'
import json, sys
log, start, token = sys.argv[1], int(sys.argv[2]), sys.argv[3]
with open(log, encoding="utf-8") as f:
    found = {json.loads(line)["process"] for line in f.readlines()[start:] if json.loads(line)["t"] == token}
names = {"com.apple.WebKit.Networking": "direct", "syndeo-proxy": "syndeo"}
print("none" if not found else names.get(next(iter(found)), "other") if len(found) == 1 else "both")
PY
}

# times FROM TOKEN: how many requests for TOKEN the origin saw after its
# first FROM lines.
times() {
  python3 -I - "$LOGS/routes-origin.jsonl" "$1" "$2" <<'PY'
import json, sys
log, start, token = sys.argv[1], int(sys.argv[2]), sys.argv[3]
with open(log, encoding="utf-8") as f:
    print(sum(1 for line in f.readlines()[start:] if json.loads(line)["t"] == token))
PY
}

# resolution NAME: what this Mac resolves NAME to, or "an address" when NAME
# is an address itself.
resolution() {
  local found
  case "$1" in
    *[!0-9.]*) ;;
    *) echo "an address"; return ;;
  esac
  case "$1" in *:*) echo "an address"; return ;; esac
  found="$(dscacheutil -q host -a name "$1" 2>/dev/null | awk '/_address:/ {print $2}' | tr '\n' ' ')"
  echo "${found:-nothing}"
}

# What the bar says the front window's page came by: see address::Route.
DIRECT_LABEL='Direct — not cached by Syndeo'
LOCAL_LABEL='Local — not cached by Syndeo'
SYNDEO_LABEL='Through Syndeo'
label_for() { case "$1" in direct) printf '%s' "$DIRECT_LABEL" ;; local) printf '%s' "$LOCAL_LABEL" ;; syndeo) printf '%s' "$SYNDEO_LABEL" ;; *) printf '(nothing)' ;; esac; }
# bar: what the bar says, or "(nothing)" when it says nothing.
bar() {
  local said
  said="$("$APPCTL" value "$PID" syndeo.route 2>/dev/null)"
  printf '%s' "${said:-(nothing)}"
}
# The bar says nothing: hidden, or empty.
bar_empty() { [ "$(bar)" = '(nothing)' ]; }
# The bar says something.
bar_said() { [ "$(bar)" != '(nothing)' ]; }

# blank: about:blank, typed, so that the bar says nothing until the next page.
blank() {
  choose File "Open Location…"
  "$APPCTL" type "$PID" "about:blank" >/dev/null || true
  wait_for 10 bar_empty || bad "about:blank" "the bar still says \"$(bar)\""
}

# record LABEL TYPED EXPECTED MEASURED BAR DETAIL: a line of the gate's table.
record() {
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$@" >>"$LOGS/routes.tsv"
  printf '  ROUTE  %s\n         typed: %s; expected: %s; WebKit went: %s; the bar: %s; %s\n' "$@"
}

# by_origin LABEL TYPED EXPECT TOKEN [measure]: a page the routing origin
# serves, typed, whose route is one of the two that do not vary. Which way
# WebKit went is the process the origin saw the request from — its own
# networking process, or the proxy — and that, and what the bar then says,
# must both be EXPECT (direct or syndeo). With "measure", the two are
# recorded and compared, and nothing fails.
by_origin() {
  local label="$1" typed="$2" expect="$3" t="$4" mode="${5:-gate}" from shown went said
  from="$(origin_lines)"
  choose File "Open Location…"
  "$APPCTL" type "$PID" "$typed" >/dev/null || true
  if wait_for 20 page_says "ROUTE-PAGE-$t"; then shown=yes; else shown=no; fi
  # Its arrival reported, and the bar set from it.
  sleep 1
  went="$(way "$from" "$t")"
  said="$(bar)"
  record "$label" "$typed" "$expect" "$went" "$said" "shown: $shown; the origin saw: $(seen "$from" "$t")"
  if [ "$mode" = measure ]; then
    printf '  MEASURE  %s: WebKit went %s, the bar says "%s" (%s)\n' "$label" "$went" "$said" \
      "$([ "$said" = "$(label_for "$went")" ] && echo agreeing || echo NOT agreeing)"
    return 0
  fi
  must "$([ "$shown" = yes ] && [ "$went" = "$expect" ] && echo yes)" "the route for $label: $expect" "shown: $shown; WebKit went: $went"
  must "$([ "$said" = "$(label_for "$expect")" ] && echo yes)" "the bar for $label says \"$(label_for "$expect")\"" "it says \"$said\""
}

# by_local LABEL ORIGIN: a local-network address, typed, whose route may
# vary: ORIGIN/cached?t=<token>, a page the routing origin serves that any
# cache may keep. Which way WebKit
# went is recorded and not asserted. The bar must say "Local — not cached by
# Syndeo", and the proxy's cache must hold no more entries afterwards,
# whichever way it went. ENTRIES is the proxy's entries just before, and is
# left as they are after.
by_local() {
  local label="$1" origin="$2" t from shown went said before
  before="$ENTRIES"
  t="$(token)"
  from="$(origin_lines)"
  choose File "Open Location…"
  "$APPCTL" type "$PID" "$origin/cached?t=$t" >/dev/null || true
  if wait_for 20 page_says "ROUTE-CACHED-$t"; then shown=yes; else shown=no; fi
  sleep 1
  went="$(way "$from" "$t")"
  said="$(bar)"
  counted "after: $label"
  record "$label" "$origin/cached?t=$t" local "$went" "$said" "shown: $shown; the origin saw: $(seen "$from" "$t"); the proxy's entries: ${before:-?} -> ${ENTRIES:-?}"
  must "$([ "$shown" = yes ] && [ "$said" = "$LOCAL_LABEL" ] && echo yes)" "the bar for $label says \"$LOCAL_LABEL\"" "shown: $shown; it says \"$said\""
  must "$([ -n "$before" ] && [ "$ENTRIES" = "$before" ] && echo yes)" \
    "and the proxy did not cache it, though it may be cached: its entries stayed at ${before:-?} (WebKit went $went)" "now ${ENTRIES:-?}"
}

# conns ADDRESS: the processes holding a TCP connection to ADDRESS, by name,
# one a line.
conns() {
  local at="$1"
  case "$at" in *:*) at="[$at]" ;; esac
  lsof +c 0 -nP -iTCP@"$at" -Fc 2>/dev/null | sed -n 's/^c//p' | sort -u
}

# settled: the page in front came from the address, or failed to.
settled() { bar_said || page_says "couldn't open"; }

# by_count LABEL TYPED EXPECT ADDRESS: an address nothing here serves, typed,
# from about:blank, with COUNTED and ENTRIES the proxy's count and entries
# just before. Which way WebKit went is whether the proxy's count rose for
# it; the processes seen holding a connection to ADDRESS in the first 6 s
# are recorded too. Then, by EXPECT:
#   - syndeo: it must have gone through the proxy, and the bar must say
#     "Through Syndeo" on the proxy's own page, once the proxy gives up on
#     the address;
#   - direct: it must not have reached the proxy, no page comes from it, and
#     the bar must say nothing; a load WebKit is still trying is stopped;
#   - local: which way it went is recorded, not asserted; the proxy's cache
#     must hold no more entries, and if a page came back, the bar must say
#     "Local — not cached by Syndeo"; a load still being tried after 25 s is
#     stopped;
#   - measure: recorded, and nothing asserted.
# Leaves COUNTED and ENTRIES the proxy's count and entries after.
by_count() {
  local label="$1" typed="$2" expect="$3" address="$4" before entries seen_by='' said went now
  before="$COUNTED" entries="$ENTRIES"
  blank
  choose File "Open Location…"
  "$APPCTL" type "$PID" "$typed" >/dev/null || true
  for _ in $(seq 1 12); do
    now="$(conns "$address" | tr '\n' ' ')"
    [ -z "$now" ] || seen_by="$seen_by $now"
    sleep 0.5
  done
  seen_by="$(printf '%s\n' $seen_by | sort -u | tr '\n' ' ')"
  case "$expect" in
    syndeo) wait_for 30 bar_said || true ;;
    local | measure) wait_for 25 settled || true ;;
  esac
  if ! settled; then
    "$APPCTL" activate "$PID" >/dev/null || true
    "$APPCTL" menu "$PID" View "Stop Loading" >/dev/null 2>&1 || true
  fi
  said="$(bar)"
  counted "after: $label"
  went=unknown
  if [ -n "$before" ] && [ -n "$COUNTED" ]; then
    if [ "$COUNTED" -gt "$before" ]; then went=syndeo; else went="not the proxy"; fi
  fi
  record "$label" "$typed" "$expect" "$went" "$said" "the proxy's count: ${before:-?} -> ${COUNTED:-?}; its entries: ${entries:-?} -> ${ENTRIES:-?}; holding a connection to $address: ${seen_by:-nothing seen}"
  case "$expect" in
    syndeo)
      must "$([ "$went" = syndeo ] && echo yes)" "the route for $label: syndeo" "WebKit went: $went; connections: ${seen_by:-none seen}"
      must "$([ "$said" = "$SYNDEO_LABEL" ] && echo yes)" "the bar for $label says \"$SYNDEO_LABEL\"" "it says \"$said\""
      ;;
    direct)
      must "$([ "$went" = "not the proxy" ] && echo yes)" "the route for $label: direct, not the proxy" "WebKit went: $went; connections: ${seen_by:-none seen}"
      must "$([ "$said" = '(nothing)' ] && echo yes)" "the bar for $label says nothing: no page came from it" "it says \"$said\""
      ;;
    local)
      must "$([ -n "$entries" ] && [ "$ENTRIES" = "$entries" ] && echo yes)" \
        "the proxy's cache is unchanged by $label (WebKit went $went)" "its entries: ${entries:-?} -> ${ENTRIES:-?}"
      if [ "$said" != '(nothing)' ]; then
        must "$([ "$said" = "$LOCAL_LABEL" ] && echo yes)" "the bar for $label says \"$LOCAL_LABEL\"" "it says \"$said\""
      fi
      ;;
    measure)
      printf '  MEASURE  %s: the proxy'"'"'s count %s -> %s, connections: %s, the bar: "%s"\n' "$label" "${before:-?}" "${COUNTED:-?}" "${seen_by:-none seen}" "$said"
      ;;
  esac
}

# The addresses routes adds to this runner for the time it gates, and puts
# back: on lo0, the four measured before, and each direct range's first and
# last host addresses; and a single-label name in /etc/hosts.
ALIASES4="10.255.42.1 172.31.42.1 169.254.42.1 100.64.42.1 127.0.0.2 127.255.255.254 10.0.0.1 10.255.255.254 172.16.0.1 172.31.255.254 192.168.0.1 192.168.255.254 169.254.0.1 169.254.255.254 100.64.0.1 100.127.255.254"
ALIASES6="fd42:5a7e:6e5d::1 fc00::1 fdff:ffff:ffff:ffff:ffff:ffff:ffff:fffe"
SINGLE="syndeoprobe"
ADDED=''

# lo0's addresses, one a line, sorted.
lo0_addresses() { ifconfig lo0 | awk '$1 == "inet" || $1 == "inet6" {print $1, $2}' | sort; }

add_addresses() {
  local a
  lo0_addresses >"$LOGS/lo0-before.txt"
  cp /etc/hosts "$LOGS/hosts-before"
  for a in $ALIASES4; do
    if sudo ifconfig lo0 alias "$a" netmask 255.255.255.255; then ADDED="$ADDED $a"; else bad "alias $a" "could not alias it on lo0"; fi
  done
  for a in $ALIASES6; do
    if sudo ifconfig lo0 inet6 "$a" prefixlen 128 alias; then ADDED="$ADDED $a"; else bad "alias $a" "could not alias it on lo0"; fi
  done
  if printf '127.0.0.1\t%s\n' "$SINGLE" | sudo tee -a /etc/hosts >/dev/null; then ADDED="$ADDED $SINGLE"; fi
  sudo dscacheutil -flushcache
  sudo killall -HUP mDNSResponder 2>/dev/null || true
  printf '  added for the time it gates:%s\n' "$ADDED"
}

# put_back: remove what add_addresses added, and require lo0 and /etc/hosts
# to be exactly as they were and the single-label name to resolve to nothing.
# Says so either way; returns non-zero if anything is not back.
put_back() {
  local a back=yes
  [ -f "$LOGS/hosts-before" ] || return 0
  for a in $ALIASES4; do
    if lo0_addresses | grep -qx "inet $a"; then sudo ifconfig lo0 -alias "$a" || true; fi
  done
  for a in $ALIASES6; do
    if lo0_addresses | grep -q "^inet6 $a\$"; then sudo ifconfig lo0 inet6 "$a" -alias || true; fi
  done
  if ! cmp -s /etc/hosts "$LOGS/hosts-before"; then sudo cp "$LOGS/hosts-before" /etc/hosts || true; fi
  sudo dscacheutil -flushcache
  sudo killall -HUP mDNSResponder 2>/dev/null || true
  lo0_addresses >"$LOGS/lo0-after.txt"
  if cmp -s "$LOGS/lo0-before.txt" "$LOGS/lo0-after.txt"; then ok "lo0's addresses are exactly as they were"; else bad "put back" "lo0: $(diff "$LOGS/lo0-before.txt" "$LOGS/lo0-after.txt" | grep '^[<>]' | tr '\n' ' ')"; back=no; fi
  if cmp -s /etc/hosts "$LOGS/hosts-before"; then ok "/etc/hosts is byte for byte as it was"; else bad "put back" "/etc/hosts differs"; back=no; fi
  if [ "$(resolution "$SINGLE")" = nothing ]; then ok "$SINGLE resolves to nothing again"; else bad "put back" "$SINGLE still resolves to $(resolution "$SINGLE")"; back=no; fi
  rm -f "$LOGS/hosts-before"
  [ "$back" = yes ]
}

added() { case " $ADDED " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# The text Syndeo shows for an IPv6 address with a zone (address::ZONED).
ZONED_TEXT="link-local addresses that name a network interface"

# zoned P: an IPv6 address with a zone (lo0's own link-local address, on
# which the routing origin answers) is refused typed, and refused followed
# from a page's link, with the same words, and the origin hears of neither.
zoned() {
  local P="$1" t s from
  t="$(token)"
  from="$(origin_lines)"
  visit "http://[fe80::1%25lo0]:$P/page?t=$t" "http://[fe80::1%25lo0]:$P/page?t=$t" "$ZONED_TEXT" \
    "an IPv6 address with a zone, typed, is refused with Syndeo's own page"
  must "$(bar_empty && echo yes)" "and the bar says nothing of a route" "it says \"$(bar)\""
  must "$([ "$(times "$from" "$t")" = 0 ] && echo yes)" "and the origin did not hear of it" "the origin saw: $(seen "$from" "$t")"
  t="$(token)"; s="$(token)"
  from="$(origin_lines)"
  choose File "Open Location…"
  "$APPCTL" type "$PID" "http://127.0.0.1:$P/links?t=$t&href=http://[fe80::1%2525lo0]:$P/page?t=$s" >/dev/null || true
  if wait_for 20 page_says "ROUTE-LINKS-$t"; then ok "a page with a link to an IPv6 address with a zone opens"; else bad "a page with a link to an IPv6 address with a zone opens" "page: $(page_text)"; fi
  "$APPCTL" press-web "$PID" "go" || bad "a link to an IPv6 address with a zone is followed" "no link to press: $(page_text)"
  if wait_for 10 page_says "$ZONED_TEXT"; then
    ok "a link to an IPv6 address with a zone is refused with the same words as typing it"
  else
    bad "a link to an IPv6 address with a zone is refused with the same words as typing it" "address field: '$(address)'; page: $(page_text)"
  fi
  sleep 1
  must "$([ "$(times "$from" "$s")" = 0 ] && echo yes)" \
    "a link to an IPv6 address with a zone reaches nothing: the origin did not hear of it" "the origin saw: $(seen "$from" "$t" "$s")"
}

# entries: the proxy's cache entries, from its own page, which counted read.
ENTRIES=''

# cached LABEL HOST P: a page this test serves that any cache may keep, by a
# name for this Mac that only the proxy reaches: the proxy fetches it, and
# its cache has no more entries afterwards. Sets CACHED to the token.
CACHED=''
cached() {
  local label="$1" host="$2" P="$3" t from before
  counted "before: $label"
  before="$ENTRIES"
  t="$(token)"; CACHED="$t"
  from="$(origin_lines)"
  visit "http://$host:$P/cached?t=$t" "http://$host:$P/cached?t=$t" "ROUTE-CACHED-$t" "$label opens"
  sleep 1
  must "$([ "$(way "$from" "$t")" = syndeo ] && echo yes)" "and it came through the proxy" "the origin saw: $(seen "$from" "$t")"
  counted "after: $label"
  must "$([ -n "$before" ] && [ "$ENTRIES" = "$before" ] && echo yes)" \
    "and the proxy did not cache it: its cache's entries stayed at ${before:-?}" "now ${ENTRIES:-?}"
}

# routes APP: see the top of this script.
routes() {
  local app d iface lan local_name public t s a from P status=0 before repeat_hits after_hits proxy_pid
  [ -d "${1:-}" ] || { say "usage: $0 routes <Syndeo.app>"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  printf '\nthe route bar, against what WebKit does; the proxy caching nothing from this Mac; Clear Browsing Data\n'
  security dump-trust-settings >"$LOGS/routes-trust-before.txt" 2>&1 || true
  # A fresh WebKit state: nothing an earlier instance cached or stored.
  for d in "$HOME/Library/Caches/$ID" "$HOME/Library/WebKit/$ID" "$HOME/Library/HTTPStorages/$ID" "$HOME/Library/HTTPStorages/$ID.binarycookies"; do
    if [ -e "$d" ]; then rm -rf "$d" && printf '  cleared %s\n' "$d"; fi
  done
  : >"$LOGS/routes.tsv"
  trap 'put_back >/dev/null 2>&1' EXIT
  add_addresses
  if route_origin; then
    P="$ROUTE_PORT"
    iface="$(/sbin/route -n get default 2>/dev/null | awk '/interface:/ {print $2}')"
    lan="$([ -n "$iface" ] && ipconfig getifaddr "$iface" 2>/dev/null)"
    local_name="$(scutil --get LocalHostName 2>/dev/null)"
    local_name="${local_name:+$local_name.local}"
    public="$(dig +short A example.com 2>/dev/null | grep -E '^[0-9]+(\.[0-9]+){3}$' | head -n 1)"
    printf '  this runner: the default route on %s, address %s; %s; example.com resolves to %s; the origin on port %s, IPv6 %s\n' \
      "${iface:-?}" "${lan:-none}" "${local_name:-no .local name}" "${public:-nothing}" "$P" "$ROUTE_V6"
    if started "$app" routes; then
      helper_of "$app" routes
      browses routes
      proxy_pid="$HELPER"

      # Direct, by what the origin saw: localhost, with and without its final
      # dot, loopback by IPv4, IPv6 and IPv4 in IPv6, and 127/8's edges,
      # aliased on lo0 for the time it gates.
      t="$(token)"; by_origin "127.0.0.1" "http://127.0.0.1:$P/page?t=$t" direct "$t"
      t="$(token)"; by_origin "[::1]" "http://[::1]:$P/page?t=$t" direct "$t"
      t="$(token)"; by_origin "[::ffff:127.0.0.1], IPv4 loopback in IPv6" "http://[::ffff:127.0.0.1]:$P/page?t=$t" direct "$t"
      t="$(token)"; by_origin "localhost" "http://localhost:$P/page?t=$t" direct "$t"
      t="$(token)"; by_origin "localhost., with its final dot" "http://localhost.:$P/page?t=$t" direct "$t"
      for a in 127.0.0.2 127.255.255.254; do
        added "$a" || continue
        t="$(token)"; by_origin "$a, on lo0" "http://$a:$P/page?t=$t" direct "$t"
      done

      # Through Syndeo, by what the origin saw: every name, whatever it
      # resolves to.
      if [ -n "$local_name" ]; then t="$(token)"; by_origin "a name: this Mac's .local name" "http://$local_name:$P/page?t=$t" syndeo "$t"; fi
      t="$(token)"; by_origin "a name for 127.0.0.1 (127.0.0.1.nip.io)" "http://127.0.0.1.nip.io:$P/page?t=$t" syndeo "$t"
      if added "$SINGLE"; then t="$(token)"; by_origin "a name of one label, $SINGLE (in /etc/hosts)" "http://$SINGLE:$P/page?t=$t" syndeo "$t"; fi
      t="$(token)"; by_origin "a name under .localhost" "http://syndeo-probe.localhost:$P/page?t=$t" syndeo "$t"

      # After a redirect, the bar is the final page's: a name that redirects
      # to 127.0.0.1 ends direct.
      t="$(token)"; s="$(token)"
      by_origin "a name redirecting to 127.0.0.1, the page it ends on" "http://127.0.0.1.nip.io:$P/redirect?t=$t&to=http://127.0.0.1:$P/page?t=$s" direct "$s"

      # Measured, not gated: the unspecified IPv6 address just below ::1,
      # which WebKit may refuse outright.
      t="$(token)"; by_origin "[::], the edge below ::1" "http://[::]:$P/page?t=$t" syndeo "$t" measure

      # Local, whichever way WebKit goes: the bar says so, and the proxy's
      # cache takes nothing from a page any cache may keep. This Mac's LAN
      # address, every local-network address aliased on lo0 (each range's
      # first and last host address among them), and a name redirecting to
      # one, which ends local.
      counted "before the local addresses"
      if [ -n "$lan" ]; then by_local "this Mac's LAN address, $lan" "http://$lan:$P"; fi
      for a in $ALIASES4; do
        case "$a" in 127.*) continue ;; esac
        added "$a" || continue
        by_local "$a, on lo0" "http://$a:$P"
      done
      for a in $ALIASES6; do
        added "$a" || continue
        by_local "[$a], on lo0" "http://[$a]:$P"
      done
      # The redirect's own target is the page by_local asks for: what
      # follows "to=" is that page's address.
      if added 10.255.42.1; then
        by_local "a name redirecting to 10.255.42.1, the page it ends on" "http://127.0.0.1.nip.io:$P/redirect?t=$(token)&to=http://10.255.42.1:$P"
      fi

      # Addresses nothing here serves, by the proxy's count, from about:blank.
      counted "before the addresses nothing here serves"
      # Through Syndeo: a genuinely remote public address, then the IPv4
      # addresses just outside each range.
      if [ -n "$public" ]; then by_count "a public address, $public (example.com's)" "http://$public/" syndeo "$public"; fi
      for a in 126.255.255.254 128.0.0.1 9.255.255.254 11.0.0.1 172.15.255.254 172.32.0.1 192.167.255.254 192.169.0.1 169.253.255.254 169.255.0.1 100.63.255.254 100.128.0.1; do
        by_count "$a, just outside a range" "http://$a:$P/" syndeo "$a"
      done
      # Direct: loopback that is not this Mac's assigned address.
      by_count "127.0.0.5, loopback, not assigned" "http://127.0.0.5:$P/" direct 127.0.0.5
      # Local: local-network addresses that are not this Mac's own. The
      # proxy's cache takes nothing, whichever way WebKit goes.
      for a in 10.200.200.200 172.20.200.200 192.168.200.200 169.254.200.200 100.100.100.100; do
        by_count "$a, local, not this Mac's" "http://$a:$P/" local "$a"
      done
      # Measured, not gated: IPv6 addresses this runner has no route to.
      for a in fbff:ffff:ffff:ffff:ffff:ffff:ffff:ffff fe00::1 fe7f:ffff:ffff:ffff:ffff:ffff:ffff:ffff fec0::1 ::2 fd00:5a7e::5 fe80::5; do
        by_count "[$a], unreachable IPv6" "http://[$a]:$P/" measure "$a"
      done

      # A redirect from 127.0.0.1 to a public page ends through Syndeo.
      t="$(token)"
      before="$COUNTED"
      visit "http://127.0.0.1:$P/redirect?t=$t&to=http://example.com/?$t" "http://example.com/?$t" "$EXAMPLE_BODY" \
        "127.0.0.1 redirecting to a public page ends on that page"
      a="$(bar)"
      counted "after the redirect to a public page"
      record "127.0.0.1 redirecting to example.com, the page it ends on" "http://127.0.0.1:$P/redirect?..." syndeo \
        "$([ -n "$before" ] && [ "${COUNTED:-0}" -gt "$before" ] && echo syndeo || echo direct)" "$a" "the proxy's count: ${before:-?} -> ${COUNTED:-?}"
      must "$([ "$a" = "$SYNDEO_LABEL" ] && echo yes)" "the bar for the page a redirect to a public page ends on says \"$SYNDEO_LABEL\"" "it says \"$a\""

      zoned "$P"

      # No trust settings needed for a page's parts either: an https image
      # from another host, on a page this Mac serves, comes through the
      # proxy, though Syndeo's authority is in no trust settings. The proxy
      # counts a request only once TLS inside its tunnel has succeeded, so its
      # count rising says WebKit accepted the proxy's certificate for it.
      counted "before a page with an https image from another host"
      before="$COUNTED"
      t="$(token)"; s="$(token)"
      choose File "Open Location…"
      "$APPCTL" type "$PID" "http://127.0.0.1:$P/page?t=$t&sub=https://example.com/?$s" >/dev/null || true
      if wait_for 20 page_says "ROUTE-PAGE-$t"; then ok "a page from this Mac with an https image from example.com opens"; else bad "a page with an https image opens" "page: $(page_text)"; fi
      sleep 3
      counted "after a page with an https image from another host"
      must "$([ -n "$before" ] && [ -n "$COUNTED" ] && [ "$COUNTED" -gt "$before" ] && echo yes)" \
        "its https image from another host came through the proxy, with Syndeo's authority in no trust settings: the proxy's count went from ${before:-?} to ${COUNTED:-?}"

      # The proxy caches what came from the public internet, and nothing
      # from this Mac, reached by name.
      counted "before a public page"
      before="$ENTRIES"
      visit "http://example.com/?$FRESH" "http://example.com/?$FRESH" "$EXAMPLE_BODY" "a public page opens"
      counted "after a public page"
      must "$([ -n "$before" ] && [ -n "$ENTRIES" ] && [ "$ENTRIES" -gt "$before" ] && echo yes)" \
        "and the proxy cached it: its cache's entries went from ${before:-?} to ${ENTRIES:-?}"
      cached "a cacheable page from this Mac, by a name under .localhost" syndeo-probe.localhost "$P"
      cached "a cacheable page from this Mac, by a public name for 127.0.0.1" 127.0.0.1.nip.io "$P"

      # Clear Browsing Data. Before it: a page in WebKit's cache, opened
      # again (measured: whether WebKit's own cache answered), and a value
      # in website storage.
      from="$(origin_lines)"
      visit "http://127.0.0.1.nip.io:$P/cached?t=$CACHED" "http://127.0.0.1.nip.io:$P/cached?t=$CACHED" "ROUTE-CACHED-$CACHED" "the cacheable page, opened again"
      repeat_hits="$(times "$from" "$CACHED")"
      printf '  MEASURE  opened again, the origin was asked %s time(s): %s\n' "$repeat_hits" \
        "$([ "$repeat_hits" = 0 ] && echo "WebKit's own cache answered" || echo "WebKit asked again")"
      t="$(token)"; s="$(token)"
      visit "http://127.0.0.1:$P/storage?t=$t" "http://127.0.0.1:$P/storage?t=$t" "ROUTE-STORAGE-new:$t" "a page keeps a value in website storage"
      visit "http://127.0.0.1:$P/storage?t=$s" "http://127.0.0.1:$P/storage?t=$s" "ROUTE-STORAGE-$t" "and finds it there on its next visit"

      settings_open "before clearing"
      if "$APPCTL" press "$PID" syndeo.clear; then ok "Settings > Clear Browsing Data… is pressed"; else bad "Clear Browsing Data…" "no button to press"; fi
      if wait_for 10 "$APPCTL" press "$PID" syndeo.clear-confirm; then ok "and asks first; Clear is pressed"; else bad "Clear Browsing Data" "no Clear in a sheet to press: $(front | head -c 300)"; fi
      if wait_for 40 sh -c '"$1" value "$2" syndeo.clear-status | grep -q "^Browsing data cleared"' _ "$APPCTL" "$PID"; then
        ok "Settings says the browsing data is cleared"
      else
        bad "Clear Browsing Data" "Settings says: $("$APPCTL" value "$PID" syndeo.clear-status)"
      fi
      must "$(grep -q 'browsing data cleared' "$LOGS/app-routes.stderr" && echo yes)" "and the app logged that both stores were cleared" \
        "$(plain_log "$LOGS/app-routes.stderr" | grep -i 'clear' | tail -n 2 | tr '\n' ' ')"
      settings_close "after clearing"

      counted "after clearing"
      must "$([ "$ENTRIES" = 0 ] && echo yes)" "after clearing, the proxy's cache has no entries" "${ENTRIES:-?}"
      must "$([ "$(pgrep -P "$PID" | tr -d '\n')" = "$proxy_pid" ] && echo yes)" "and it is the same proxy, pid $proxy_pid, still running"
      t="$(token)"
      visit "http://127.0.0.1:$P/storage?t=$t" "http://127.0.0.1:$P/storage?t=$t" "ROUTE-STORAGE-new:$t" "after clearing, website storage is empty"
      from="$(origin_lines)"
      visit "http://127.0.0.1.nip.io:$P/cached?t=$CACHED" "http://127.0.0.1.nip.io:$P/cached?t=$CACHED" "ROUTE-CACHED-$CACHED" "the cacheable page, after clearing"
      after_hits="$(times "$from" "$CACHED")"
      if [ "$repeat_hits" = 0 ]; then
        must "$([ "$after_hits" -ge 1 ] && echo yes)" "after clearing, WebKit's cache no longer answers for it: the origin was asked again" "asked $after_hits time(s)"
      else
        printf '  MEASURE  after clearing, the origin was asked %s time(s) (WebKit had asked again before clearing too)\n' "$after_hits"
      fi
      visit "http://example.com/?$FRESH" "http://example.com/?$FRESH" "$EXAMPLE_BODY" "browsing goes on after clearing"

      stopped routes
    fi
    route_origin_stop
  fi
  printf '\nputting this runner back\n'
  put_back || status=1
  trap - EXIT
  security dump-trust-settings >"$LOGS/routes-trust-after.txt" 2>&1 || true
  if cmp -s "$LOGS/routes-trust-before.txt" "$LOGS/routes-trust-after.txt"; then ok "the user's trust settings are as they were"; else bad "trust settings" "they changed"; fi
  printf '\n%d failed\n' "$fails"
  [ "$fails" = 0 ] && [ "$status" = 0 ]
}

# routes_lite APP: the part of routes that needs nothing changed on this
# runner, for the mutants it is meant to catch: 127.0.0.1 and a name for it,
# and an IPv6 address with a zone, typed and as a link.
routes_lite() {
  local t
  route_origin || return 1
  t="$(token)"; by_origin "127.0.0.1" "http://127.0.0.1:$ROUTE_PORT/page?t=$t" direct "$t"
  t="$(token)"; by_origin "a name for 127.0.0.1 (127.0.0.1.nip.io)" "http://127.0.0.1.nip.io:$ROUTE_PORT/page?t=$t" syndeo "$t"
  t="$(token)"; by_origin "a name under .localhost" "http://syndeo-probe.localhost:$ROUTE_PORT/page?t=$t" syndeo "$t"
  zoned "$ROUTE_PORT"
  route_origin_stop
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
