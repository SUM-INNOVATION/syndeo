#!/usr/bin/env bash
# Syndeo.app on a disposable macOS runner, started the way a person starts it.
#
#   ci/test-app.sh launch <Syndeo.app>
#   ci/test-app.sh refuse <Syndeo.app whose proxy cannot start>
#   ci/test-app.sh mutant <name> <Syndeo.app with that safeguard taken out>
#   ci/test-app.sh routes <Syndeo.app>
#   ci/test-app.sh customer <installed Syndeo.app> [again]
#   ci/test-app.sh realtime <Syndeo.app>
#   ci/test-app.sh looks <Syndeo.app>
#   ci/test-app.sh tabs <Syndeo.app>
#   ci/test-app.sh measure
#
# launch, which gates: open the app with `open`, never by running its
# executable, and require that:
#   - Launch Services has it running as com.sum.syndeo, from this bundle's
#     Contents/MacOS/Syndeo, with an on-screen window;
#   - its one child is the proxy, from this bundle's Contents/Helpers, and no
#     keystore process exists;
#   - its window holds a web view and no error;
#   - its controls are its toolbar's, in the title bar: Back, Forward, the
#     address field and its Reload, each by the identifier it always had;
#     and the address field's route, there, says Through Syndeo for a page
#     that came through the proxy, with its help tag those words first;
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
#     follows navigation as before; and while that page loads, the address
#     field shows its loading progress, which is gone once it has loaded;
#   - it says in its log that passkeys are not available in this copy, which
#     holds no entitlement, and asked nothing of anyone: no sheet, and its
#     one window its own;
#   - asked by another application to open an address, as `open -a` asks:
#     an http and an https address, each with this instance's own query,
#     open in a new tab beside the window, the bar saying Through Syndeo, and
#     the proxy's count rising for each, the https one with Syndeo's
#     authority in no trust settings; an ftp address and a file are each
#     refused in a tab of their own with the words typing them gets, showing
#     nothing of the file and starting no other application; and the log has
#     each one's scheme and nothing else of it;
#   - asked to quit as the Quit menu item would, it exits, and its proxy
#     with it;
#   - with its last window closed, through the window's own close button, the
#     window is let go, the same app and the same proxy keep running, and
#     opening the bundle again brings a window back in that same process, with
#     that same proxy;
#   - opened by another application asking it to open an https address, as
#     a link clicked elsewhere opens a browser that is not running: that page
#     opens, after launch has decided it can browse, with Syndeo's authority
#     in no trust settings, the bar saying Through Syndeo; whether a start
#     page opens beside it is measured, not gated;
#   - opened again, all of that holds again, and quitting again leaves nothing;
#   - it made no keystore changes: no keystore in its home and no keychain
#     item for one;
#   - the user's trust settings are as they were, and the search engine it
#     chose is removed again.
#
# customer, which gates: the part of ci/test-journey.sh's customer journey
# that happens in the app, on the Syndeo.app the package installed, and
# nothing of launch's breadth. Opened by its path, as Finder opens it: running
# as com.sum.syndeo from that bundle, with an on-screen window, its proxy the
# one helper, from the bundle, and no keystore process; the window holds a web
# view and shows the start page; https://example.com, typed with this
# instance's own query, shows its own text and the proxy's count rises (the
# journey requires the trust settings and keychains unchanged around it);
# asked to quit, it exits with its proxy. Then opened again, and all of that
# again. With `again`, after a reinstall: one opening, the same checks. It
# made no keystore changes.
# In the first opening, Reload: a fresh https page, then the bar's Reload,
# pressed as a person presses it. The address and the page have to stay the
# page's. The reload has to consult the origin through Syndeo's proxy:
# - the proxy's revalidations or misses rise, whether the origin then
#   answers 304 or a new page;
# - and nothing is answered from the proxy's cache alone: its hits rise no
#   more than its revalidations, and it serves nothing stale.
# A reload the proxy answered from its cache, even with a miss beside it for
# something else, fails. The proxy's counters are read in a tab of their own,
# then closed, so they bracket the reload and nothing else. No exact count is
# required, since what a page loads and how its server marks it can vary.
# Kept in reload.txt too.
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
# catch it fail, at least one, and no other check does: https, typed and
# asked for by another application, for no-pinning-delegate (while http,
# rendered and counted, still works), a
# page's links in the window for no-navigation-policy, in a new tab for
# any-scheme-in-a-new-tab, an ftp address and a file another application
# asks Syndeo to open refused for asked-any-scheme, typing kept through a
# page's arrival for no-editing-guard, the bar for a name for direct-for-every-name, a link to
# an IPv6 address with a zone for zoned-links-followed, Reload consulting
# the origin for reload-from-cache, Clear Browsing Data forgetting the
# closed tabs and a session kept from a last quit (which it writes first)
# for tabs-kept-after-clear, and a WebSocket through Syndeo for
# websocket-headers-stripped. Any other failure is a failure.
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
#     each range's first and last host address, IPv6 unique local ones, and
#     one carried in IPv6 as ::ffff:10.255.42.1);
#     and, by the proxy's count, local-network addresses that are not this
#     Mac's, the bar saying Local if a page comes back;
#   - after a redirect, the final page's route: a name redirecting to
#     127.0.0.1 is Direct, one redirecting to 10.255.42.1 is Local, and
#     127.0.0.1 redirecting to a public page is Through Syndeo;
#   - an IPv6 address with a zone is refused, typed and followed from a
#     page's link, with the same words, and the origin hears of neither;
#   - an https image from another host, on a page this Mac serves, comes
#     through the proxy with Syndeo's authority in no trust settings.
# It measures, without gating, [::], a public address carried in IPv6, and
# IPv6 addresses this runner has no route to: just outside the ranges, and
# unique local and link-local ones.
# Then: a public page is cached by the proxy, and a cacheable page from this
# Mac, reached through the proxy by a name, is not; and Settings > Clear
# Browsing Data asks first, then empties the proxy's cache (the same proxy
# runs on) and website storage, and WebKit's cache stops answering for a page
# it had kept. It puts lo0 and /etc/hosts back and requires them, and the
# trust settings, exactly as they were.
#
# realtime, which gates WebSockets through Syndeo and WebRTC around it,
# against ci/macos-app/realtime-origin.py, which logs every handshake it is
# sent, with its Upgrade and Connection headers and the process it came from,
# and every STUN datagram, with the process it came from. Each case is a page
# on this Mac's loopback, which WebKit reaches by itself, whose script says
# what happened; kept in realtime.txt with what the origin saw and the
# proxy's count before and after:
#   - the start page says that WebRTC connects directly, not through
#     Syndeo, and that Syndeo is not a VPN;
#   - a WebSocket to 127.0.0.1, which WebKit opens itself, opens, carries a
#     ping back and closes cleanly;
#   - a WebSocket through Syndeo, by a public name for 127.0.0.1, does the
#     same, and reaches the origin from the proxy with Upgrade: websocket,
#     answered 101; and the proxy's count does not move, since a WebSocket
#     never goes near its cache;
#   - WebRTC is there, and goes around Syndeo, as the start page says: STUN
#     at this Mac's LAN address and at 127.0.0.1 reaches this Mac's server,
#     none of it from the proxy, and the proxy's count does not move.
# Measured, not gated, because what they do is theirs: a WebSocket to a
# public echo server by ws:// and wss://, what a public STUN server answered,
# and whether two connections in one page join.
#
# looks, which gates the window's controls and browsing in light and in dark
# appearance, the runner's own, set through System Events for each opening
# and put back as it was at the end, which is gated too; and keeps pictures
# of what it looks like: in each, the start page, a page that came through
# Syndeo, a page on this Mac's loopback (Direct), Syndeo's own page for an
# address it refuses, and Settings, each a looks-*.png in the logs, with how
# light its top and the content below it are (ci/macos-app/luma.py),
# measured and not gated.
#
# tabs, which gates tabs, from no kept session and with macOS's own "keep
# windows" setting as the runner has it, on pages this Mac serves, reached
# through Syndeo by a public name for 127.0.0.1:
#   - History > Reopen Last Closed Tab is ⇧⌘T and View > Show All Tabs ⇧⌘\;
#     View has Show Tab Bar and Show All Tabs, once each; with tabs showing,
#     File has AppKit's
#     Close Other Tabs, once; Window has Duplicate Tab and Pin Tab, and
#     History Reopen All Windows from Last Session and Recently Closed;
#   - the window takes its title from its page, and so does its tab;
#   - with the mouse, as a person clicks: ⌘-click and a middle-click on a
#     link each open it in a tab behind the page, which stays in front, the
#     tabs after it in the order they were opened, with their pages' titles;
#     ⇧⌘-click opens one in front, right after the page; each tab's page came
#     through Syndeo; ⌘-click on a file: link opens no tab, leaves the page
#     and shows nothing of the file;
#   - ⌘1, ⌘2 and ⌘3 show those tabs, and ⌘9 the last;
#   - a tab shows the icon its page names, and that it is loading, until its
#     page has arrived;
#   - Window > Duplicate Tab opens the page again in front, right after it,
#     with its history; Window > Pin Tab puts the tab first, marked; a secondary click on a tab
#     opens Syndeo's menu for it, Unpin Tab for a pinned one, and its
#     Duplicate Tab duplicates that tab;
#   - File > Close Other Tabs closes all but the pinned one, which says no to
#     closing, and this one;
#     History > Recently Closed lists them by title; ⇧⌘T opens the last one
#     closed again, and Recently Closed > Page A that one;
#   - ⇧⌘\ and View > Show All Tabs show AppKit's overview, and each, again,
#     leaves it, as does Escape: gated on AppKit's own state, the tab group's
#     isOverviewVisible, which the app logs before each request and a second
#     after, since on the runner the overview is not a window to
#     Accessibility;
#   - quit: the session is kept, readable by this user alone, with one tab
#     pinned; opened again, the pinned tab is first, beside the start page,
#     and opens its page when shown; History > Reopen All Windows from Last
#     Session opens the rest, in a window of their own;
#   - quit with macOS set to keep windows (NSQuitAlwaysKeepsWindows, for
#     this application only): opened again, every window's tabs are back;
#   - Clear Browsing Data forgets the closed tabs and the session: History >
#     Reopen Last Closed Tab and Reopen All Windows from Last Session are not
#     offered, Recently Closed is empty, and the session file is gone.
# It measures, without gating, memory per tab, as README's Memory section
# measures memory: the same page in one tab and in five, sampled over the
# same interval; kept in tabs-memory.txt. It removes the setting it made and
# the session.
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
no_app() { [ -z "$("$APPCTL" pids "$ID")" ]; }
keystore_item() { security find-generic-password -s com.sum.syndeo.keystore >/dev/null 2>&1 && echo present || echo absent; }

CANNOT_BROWSE="Syndeo can't open websites right now."
# This instance's query for example.com (see started).
FRESH=''

# What example.com's page itself says. Its <title> is "Example Domain", and
# the window takes its title from the page through its controller, so that is
# no proof the page was shown; this sentence is in the page's body.
EXAMPLE_BODY='This domain is for use in documentation examples'

# started APP ROUND [ADDRESS]: open APP, asking it to open ADDRESS if one is
# given, as another application would, and check what is running: Launch Services has
# it as $ID, from APP, with an on-screen window, and its one child is the
# proxy from APP's Contents/Helpers; no keystore process exists. Sets PID and
# HELPER; returns non-zero if there is no app to check further.
#
# The count and the pid come from one answer of Launch Services per poll,
# never from two: an application in one answer and missing from the next
# would otherwise pass as exactly one, with no pid, and every check after
# would ask about nothing.
started() {
  local app="$1" round="$2" children image windows pids='' until
  PID='' HELPER=''
  # This instance's own query for example.com: random, and nothing else.
  # WebKit's cache persists from one instance to the next, so a page another
  # instance opened could otherwise be shown without this one fetching it.
  FRESH="fresh$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
  if [ -n "${3:-}" ]; then
    open --stdout "$LOGS/app-$round.stdout" --stderr "$LOGS/app-$round.stderr" -a "$app" "$3" || { bad "open ($round)" "open failed"; return 1; }
  else
    open --stdout "$LOGS/app-$round.stdout" --stderr "$LOGS/app-$round.stderr" "$app" || { bad "open ($round)" "open failed"; return 1; }
  fi
  until=$((SECONDS + 30))
  while :; do
    pids="$("$APPCTL" pids "$ID")"
    if [ -n "$pids" ] && [ "$(printf '%s\n' "$pids" | wc -l | tr -d ' ')" = 1 ]; then
      PID="$pids"
      break
    fi
    [ "$SECONDS" -lt "$until" ] || break
    sleep 0.5
  done
  if [ -z "$PID" ]; then
    bad "running ($round)" "not exactly one running application is $ID 30 s after open; Launch Services' last answer: [$(printf '%s' "$pids" | tr '\n' ' ')]"
    return 1
  fi
  ok "one running application is $ID ($round, pid $PID)"
  # Asked of Launch Services for that pid, which it may answer only once the
  # application has checked in: CI 39872069798 listed the pid 0.1 s after
  # open, and asked 0.04 s later, before the application had logged anything,
  # it had no identifier for it yet; 0.5 s later it had com.sum.syndeo.
  if wait_for 10 sh -c '[ "$("$1" bundle "$2")" = "$3" ]' _ "$APPCTL" "$PID" "$ID"; then
    ok "Launch Services has pid $PID as $ID ($round)"
  else
    bad "Launch Services has pid $PID as $ID ($round)" "it has '$("$APPCTL" bundle "$PID")' 10 s after listing the pid"
  fi
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

# chrome WHEN: the front window's controls are its toolbar's, in the title
# bar: Back, Forward, the address field and its Reload, each found by the
# identifier it always had, and Reload saying Reload.
chrome() {
  local id where
  for id in syndeo.back syndeo.forward syndeo.address syndeo.reload; do
    where="$("$APPCTL" in-toolbar "$PID" "$id" 2>&1)"
    must "$([ "$where" = "in the toolbar" ] && echo yes)" "$id is in the window's toolbar ($1)" "${where:-no such control in the front window}"
  done
  must "$("$APPCTL" describe "$PID" syndeo.reload | grep -q 'Reload' && echo yes)" "the address field's Reload says Reload ($1)" \
    "it says '$("$APPCTL" describe "$PID" syndeo.reload)'"
}

# route_words WHEN: the front window's page came through Syndeo, and the
# address field's route, in the toolbar, says so: Through Syndeo, with its
# help tag those words first, then what each route means.
route_words() {
  local help
  must "$([ "$(bar)" = "$SYNDEO_LABEL" ] && echo yes)" "the address field's route says '$SYNDEO_LABEL' ($1)" "it says '$(bar)'"
  must "$("$APPCTL" in-toolbar "$PID" syndeo.route >/dev/null 2>&1 && echo yes)" "and it is in the address field, in the toolbar ($1)"
  help="$("$APPCTL" describe "$PID" syndeo.route | cut -f2)"
  must "$([[ "$help" == "$SYNDEO_LABEL. How this page was reached."* ]] && echo yes)" \
    "and its help tag is its words, then what each route means ($1)" "help: '${help:-none}'"
}

# look NAME: a picture of PID's front window, the one Accessibility has in
# front, which the checks before it used, kept as looks-NAME.png in the
# logs, and how light its top and the content below it are; measured, never
# gated, since what a runner lets a picture show is the runner's.
look() {
  local id file="$LOGS/looks-$1.png"
  id="$("$APPCTL" front-window-id "$PID")"
  rm -f "$file"
  if [ -n "$id" ] && screencapture -x -o -l "$id" "$file" 2>/dev/null && [ -s "$file" ]; then
    printf '  MEASURE  %s: looks-%s.png, %s\n' "$1" "$1" "$(python3 -I "$here/macos-app/luma.py" "$file" 2>&1 | head -c 160)"
  else
    printf '  MEASURE  %s: no picture of the window could be taken (window %s)\n' "$1" "${id:-not found}"
  fi
}

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
  if wait_for 5 "$APPCTL" value "$PID" syndeo.progress; then
    ok "while a page loads, the address field shows its loading progress"
  else
    bad "loading progress" "no progress in the address field while $slow loads"
  fi
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
  must "$("$APPCTL" value "$PID" syndeo.progress >/dev/null 2>&1 && echo no || echo yes)" "once that page has loaded, its progress is gone"
  "$APPCTL" tab "$PID" >/dev/null || bad "Tab" "pid $PID could not be brought to the front"
  arrive "$slow" 'SYNDEO-SLOW-PAGE' "leaving the field with Tab shows the page's address again"
  "$APPCTL" press "$PID" syndeo.back || bad "Back" "no Back button to press in the front window"
  arrive "$first" 'Policy test' "and the field follows navigation again: Back shows the page before"
}

# What the log says of passkeys in a copy without Apple's browser passkey
# entitlement, which no copy CI builds holds (see the passkeys module).
PASSKEYS_NONE="passkeys: not available in this copy of Syndeo, which does not hold com.apple.developer.web-browser.public-key-credential; a page's passkey requests fail, and nothing was asked of anyone"

# passkeys_unavailable LOG ROUND: the app says so, once, and asked nothing of
# anyone: no sheet in its window, and its one window its own.
passkeys_unavailable() {
  local said
  wait_for 10 grep -qF "$PASSKEYS_NONE" "$1" || true
  said="$(plain_log "$1" | grep -c 'passkeys: ')"
  must "$(plain_log "$1" | grep -qF "$PASSKEYS_NONE" && [ "$said" = 1 ] && echo yes)" \
    "it says passkeys are not available in this copy, which holds no entitlement, and that it asked nothing ($2)" \
    "$said passkeys lines: $(plain_log "$1" | grep 'passkeys: ' | head -n 2 | cut -c1-200 | tr '\n' ' ')"
  must "$("$APPCTL" ax-tree "$PID" | grep -q '^AXSheet' && echo no || echo yes)" "and nothing is asked of anyone: no sheet in its window ($2)"
  must "$([ "$("$APPCTL" titles "$PID" | grep -c .)" = 1 ] && echo yes)" "and its one window is its own ($2)" "$("$APPCTL" titles "$PID" | tr '\n' '|')"
}

# opened_since LOG N: the app reports more than N windows opened.
opened_since() { [ "$(grep -c 'a window opened' "$1")" -gt "$2" ]; }

# asked_web APP LOG SCHEME: another application asks Syndeo to open
# SCHEME://example.com with this instance's own query, as `open -a` asks. It
# opens in a new tab beside the window, shows the page, the bar says Through
# Syndeo, and the proxy's count rises for it; for https, with Syndeo's
# authority in no trust settings. The checks are named "an SCHEME address
# another application asks Syndeo to open", which, for https, is what the
# no-pinning-delegate mutant has to fail.
asked_web() {
  local app="$1" log="$2" scheme="$3" page what opened before on_screen
  page="$scheme://example.com/?$FRESH-asked-$scheme"
  what="an $scheme address another application asks Syndeo to open"
  counted "before $what"
  before="$COUNTED"
  opened="$(grep -c 'a window opened' "$log")"
  open -a "$app" "$page" || bad "$what opens in a new tab" "open -a failed"
  if wait_for 15 opened_since "$log" "$opened"; then ok "$what opens in a new tab"; else bad "$what opens in a new tab" "the app reports no new window: $(grep 'a window' "$log" | tail -n 2 | tr '\n' ' ')"; fi
  arrive "$page" "$EXAMPLE_BODY" "$what shows its page, with Syndeo's authority in no trust settings"
  must "$([ "$(bar)" = "$SYNDEO_LABEL" ] && echo yes)" "$what: the bar says $SYNDEO_LABEL" "the bar says $(bar)"
  on_screen="$("$APPCTL" windows "$PID" | wc -l | tr -d ' ')"
  must "$([ "$on_screen" = 1 ] && echo yes)" "$what: a tab beside the window, one window on screen" "$on_screen on screen"
  counted "after $what"
  risen "$before" "$what: the proxy fetched it"
  close_tab "the tab $what opened"
}

# asked_refused APP LOG: another application asks Syndeo to open an ftp
# address, and a file, as `open -a` asks: /private/etc/passwd, by its real
# path, since Launch Services hands an application a file by that, and
# /etc/passwd arrives as file:///private/etc/passwd (CI 39664605039). Each is
# refused in a tab of its own
# with the words typing it gets, shows nothing of what it points at, and
# starts no other application; the log names each by its scheme alone. The
# checks are named "an ftp address ... is refused" and "a file ... is
# refused", which is what the asked-any-scheme mutant has to fail.
asked_refused() {
  local app="$1" log="$2" what address words shown opened apps started_now id how
  while IFS='|' read -r what address shown words; do
    opened="$(grep -c 'a window opened' "$log")"
    apps="$("$APPCTL" apps)"
    open -a "$app" "$address" || bad "$what is refused in a tab of its own" "open -a failed"
    if wait_for 15 opened_since "$log" "$opened"; then ok "$what is refused in a tab of its own"; else bad "$what is refused in a tab of its own" "the app reports no new window"; fi
    arrive "$shown" "$words" "$what is refused with the words typing it gets"
    must "$(page_says 'root:' && echo no || echo yes)" "$what is refused, and nothing of what it points at is shown"
    started_now="$(comm -13 <(printf '%s\n' "$apps") <("$APPCTL" apps))"
    must "$([ -z "$started_now" ] && echo yes)" "$what is refused, and starts no other application" "$(printf '%s' "$started_now" | tr '\n' ' ')"
    for id in $started_now; do
      how="$("$APPCTL" stop "$id")" || bad "cleanup after $what" "$id, which it started, is $how"
    done
    if opened_since "$log" "$opened"; then close_tab "the tab $what opened"; fi
  done <<'ASKED'
an ftp address another application asks Syndeo to open|ftp://example.com/|ftp://example.com/|not ftp: ones
a file another application asks Syndeo to open|/private/etc/passwd|file:///private/etc/passwd|not file: ones
ASKED
  must "$(plain_log "$log" | grep -q 'refused an address another application asked to open scheme=ftp' &&
    plain_log "$log" | grep -q 'refused an address another application asked to open scheme=file' && echo yes)" \
    "the log names each address another application asked for by its scheme alone" \
    "$(plain_log "$log" | grep 'another application asked' | cut -c1-160 | tr '\n' ' ')"
}

# asked_launch APP: open APP by asking it to open an https address, as a link
# clicked elsewhere opens a browser that is not running. The page opens,
# after launch has decided it can browse, with Syndeo's authority in no trust
# settings, the bar saying Through Syndeo. Whether the address reaches it
# before launch finishes, and so whether a start page opens beside it, is
# AppKit's to decide: measured, not gated.
asked_launch() {
  local app="$1" round=asked page log
  page="https://example.com/?$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')-asked-launch"
  say "opened to open an address"
  started "$app" "$round" "$page" || return
  helper_of "$app" "$round"
  browses "$round"
  log="$LOGS/app-$round.stderr"
  arrive "$page" "$EXAMPLE_BODY" "opened to open an https address, it shows that page, with Syndeo's authority in no trust settings"
  must "$([ "$(bar)" = "$SYNDEO_LABEL" ] && echo yes)" "and the bar says $SYNDEO_LABEL ($round)" "the bar says $(bar)"
  must "$([ "$(plain_log "$log" | grep -c 'opening an address another application asked to open scheme=https')" = 1 ] && echo yes)" \
    "the log says it opened the one address it was asked for, by its scheme alone ($round)"
  printf '  MEASURE  opened to open an address: %s window(s) opened; %s\n' \
    "$(grep -c 'a window opened' "$log")" \
    "$(plain_log "$log" | awk '/opening an address another application asked/ && !asked {asked = NR} /a window opened open=1/ && !first {first = NR} END {print (asked && first && asked < first) ? "the address came before launch finished, so no start page" : "the address came after launch had opened the start page"}')"
  stopped "$round"
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
  chrome "$round"
  plain_http
  route_words "$round"
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
  passkeys_unavailable "$log" "$round"
  asked_web "$app" "$log" http
  asked_web "$app" "$log" https
  asked_refused "$app" "$log"
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
  asked_launch "$app"
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

# stats_now WHEN: the proxy's own page, opened as counted opens it, and its
# counters on one line, "requests N hits N stale N misses N revalidations N
# 304 N entries N", a ? for any not found. Sets STATS (and, through
# counted, COUNTED and ENTRIES).
STATS=''
stats_now() {
  local tree k v
  counted "$1"
  tree="$(front)"
  STATS=''
  for k in requests hits stale misses revalidations '304:' entries; do
    v="$(printf '%s\n' "$tree" | grep -oE "$k [0-9]+" | head -n 1 | awk '{print $NF}')"
    STATS="$STATS${STATS:+ }${k%:} ${v:-?}"
  done
}

# stat_of NAME STATS: one counter's value in a STATS line, or ?.
stat_of() { printf '%s\n' "$2" | awk -v k="$1" '{ for (i = 1; i < NF; i += 2) if ($i == k) { print $(i + 1); exit } } END { if (NR == 0) print "?" }'; }

# rose NAME BEFORE AFTER: counter NAME is higher in AFTER than in BEFORE.
rose() {
  local was is
  was="$(stat_of "$1" "$2")" is="$(stat_of "$1" "$3")"
  [ -n "$was" ] && [ -n "$is" ] && [ "$was" != '?' ] && [ "$is" != '?' ] && [ "$is" -gt "$was" ]
}

# cached_alone BEFORE AFTER: how many more answers came from the proxy's
# cache without asking the origin: hits beyond revalidations, which count as
# hits too, and stale serves. Prints ? for a counter it cannot read.
cached_alone() {
  local h0 h1 r0 r1 s0 s1 v
  h0="$(stat_of hits "$1")" h1="$(stat_of hits "$2")"
  r0="$(stat_of revalidations "$1")" r1="$(stat_of revalidations "$2")"
  s0="$(stat_of stale "$1")" s1="$(stat_of stale "$2")"
  for v in "$h0" "$h1" "$r0" "$r1" "$s0" "$s1"; do
    [[ "$v" =~ ^[0-9]+$ ]] || { echo '?'; return; }
  done
  echo $(( (h1 - h0) - (r1 - r0) + (s1 - s0) ))
}

# change BEFORE AFTER: each counter's change between two STATS lines.
change() {
  awk -v a="$1" -v b="$2" 'BEGIN {
    n = split(a, x, " "); split(b, y, " "); s = ""
    for (i = 1; i < n; i += 2) {
      d = (x[i + 1] ~ /^[0-9]+$/ && y[i + 1] ~ /^[0-9]+$/) ? sprintf("%+d", y[i + 1] - x[i + 1]) : "?"
      s = s (s == "" ? "" : " ") x[i] " " d
    }
    print s
  }'
}

# stats_aside ADDRESS PAGE WHEN: the proxy's counters, read in a tab of their
# own, which is then closed, leaving the front tab's page, at ADDRESS and
# saying PAGE, as it was. Sets STATS.
stats_aside() {
  choose File "New Tab"
  arrive "" 'Welcome to Syndeo' "a tab of its own for the proxy's counters ($3)"
  stats_now "$3"
  close_tab "$3"
  arrive "$1" "$2" "the page's tab in front again, as it was ($3)"
}

# reload_gate: see the top of this script.
reload_gate() {
  local a b at pressed=no stop_seen=no reloaded=no consulted=no alone out="$LOGS/reload.txt"
  FRESH="fresh$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
  at="https://example.com/?$FRESH"
  visit "$at" "$at" "$EXAMPLE_BODY" "a fresh https page opens, to reload"
  stats_aside "$at" "$EXAMPLE_BODY" "before Reload"
  a="$STATS"
  if "$APPCTL" press "$PID" syndeo.reload; then
    pressed=yes
    if wait_for 5 sh -c '"$1" describe "$2" syndeo.reload | grep -q "Stop"' _ "$APPCTL" "$PID"; then stop_seen=yes; fi
    if wait_for 30 sh -c '"$1" describe "$2" syndeo.reload | grep -q "Reload"' _ "$APPCTL" "$PID"; then reloaded=yes; fi
  fi
  must "$pressed" "the bar's Reload is pressed" "no Reload button to press in the front window"
  arrive "$at" "$EXAMPLE_BODY" "after Reload, the address is still the page's, and the page shows its own text"
  stats_aside "$at" "$EXAMPLE_BODY" "after Reload"
  b="$STATS"
  alone="$(cached_alone "$a" "$b")"
  if { rose revalidations "$a" "$b" || rose misses "$a" "$b"; } && [ "$alone" = 0 ]; then consulted=yes; fi
  {
    printf 'a fresh https page, then the bar'"'"'s Reload\n  before: %s\n  after:  %s\n  change: %s\n' "$a" "$b" "$(change "$a" "$b")"
    printf 'Reload pressed: %s; the bar showed Stop then: %s; it read Reload again within 30 s: %s\n' "$pressed" "$stop_seen" "$reloaded"
    printf 'answers from the cache alone (hits beyond revalidations, and stale): %s\n' "$alone"
    printf 'the origin consulted through the proxy (revalidations or misses rose, and nothing from the cache alone): %s\n' "$consulted"
  } >"$out"
  sed 's/^/  RELOAD  /' "$out"
  must "$consulted" "the reload consulted the origin through Syndeo's proxy: its revalidations or misses rose, and nothing came from its cache alone ($(change "$a" "$b"))" \
    "a reload the proxy answered, in part or whole, from its own cache alone"
}

# customer_round APP ROUND [reload]: one opening, as customer describes it.
customer_round() {
  local app="$1" round="$2"
  started "$app" "$round" || return
  helper_of "$app" "$round"
  browses "$round"
  arrive "" 'Welcome to Syndeo' "the window shows the start page, with an empty address field ($round)"
  pinned
  if [ "${3:-}" = reload ]; then reload_gate; fi
  stopped "$round"
}

# customer APP [again]: see the top of this script.
customer() {
  local app keystore_before home
  [ -d "${1:-}" ] || { say "usage: $0 customer <Syndeo.app> [again]"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  home="$HOME/Library/Application Support/Syndeo"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  keystore_before="$(keystore_item)"
  if [ "${2:-}" = again ]; then
    printf '\nSyndeo.app, installed again, opened once more: %s\n' "$app"
    customer_round "$app" customer-again
  else
    printf '\nSyndeo.app, opened as a customer opens it: %s\n' "$app"
    customer_round "$app" customer-first reload
    printf '\nquit, and opened again\n'
    customer_round "$app" customer-reopened
  fi
  printf '\nwhat it left\n'
  must "$([ ! -e "$home/.keystore" ] && echo yes)" "no keystore in its home ($home)" "$(ls -la "$home/.keystore" 2>&1 | head -3)"
  must "$([ "$keystore_before" = absent ] && [ "$(keystore_item)" = absent ] && echo yes)" "no keychain item for a keystore, before or after" "before: $keystore_before, after: $(keystore_item)"
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
  local name="${1:-}" app intended log caught unrelated session="$HOME/Library/Application Support/Syndeo/session.txt"
  [ -n "$name" ] && [ -d "${2:-}" ] || { say "usage: $0 mutant <name> <Syndeo.app>"; exit 2; }
  app="$(cd "$2" && pwd -P)"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  printf '\nSyndeo.app with a safeguard taken out (%s): %s\n' "$name" "$app"
  case "$name" in
    no-pinning-delegate) intended='^(https://example\.com opens|and the proxy fetched it over https:|an https address another application asks Syndeo to open( shows|: the bar|: the proxy))' ;;
    no-navigation-policy) intended='^policy: .* is refused in the window$' ;;
    any-scheme-in-a-new-tab) intended='^policy: .* is refused and opens no tab$' ;;
    asked-any-scheme) intended='^(an? (ftp address|file) another application asks Syndeo to open is refused with the words typing it gets|the log names each address another application asked for by its scheme alone)$' ;;
    no-editing-guard) intended="^typing in the address field while a page loads" ;;
    direct-for-every-name) intended='^the bar for a name' ;;
    zoned-links-followed) intended='^a link to an IPv6 address with a zone' ;;
    reload-from-cache) intended="^the reload consulted the origin through Syndeo's proxy" ;;
    tabs-kept-after-clear) intended='^Clear Browsing Data forgets' ;;
    websocket-headers-stripped) intended='^a WebSocket through Syndeo' ;;
    *) say "no mutant $name"; exit 2 ;;
  esac
  if [ "$name" = tabs-kept-after-clear ]; then
    # A session kept from a last quit, as the app writes it, to be cleared.
    mkdir -p "$(dirname "$session")"
    printf 'syndeo-session 1\nwindow\t0\ntab\t-\thttps://example.com/\tExample Domain\n' >"$session"
  fi
  if started "$app" "mutant-$name"; then
    helper_of "$app" "mutant-$name"
    browses "mutant-$name"
    log="$LOGS/app-mutant-$name.stderr"
    case "$name" in
      no-pinning-delegate) plain_http; pinned; asked_web "$app" "$log" https; unlogged "$log" ;;
      asked-any-scheme) asked_refused "$app" "$log" ;;
      no-editing-guard) editing ;;
      direct-for-every-name | zoned-links-followed) routes_lite ;;
      reload-from-cache) reload_gate ;;
      tabs-kept-after-clear) tabs_cleared "$session" ;;
      websocket-headers-stripped) realtime_origin && rt_through_syndeo ;;
      *) policy "$log" ;;
    esac
  fi
  origin_stop
  route_origin_stop
  realtime_origin_stop
  stopped "mutant-$name"
  # What it kept, which a clear would have forgotten.
  [ "$name" = tabs-kept-after-clear ] && rm -f "$session"
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
      if added 10.255.42.1; then by_local "[::ffff:10.255.42.1], a local address carried in IPv6" "http://[::ffff:10.255.42.1]:$P"; fi
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
      # Measured, not gated: a public address carried in IPv6, whose route
      # has not been measured, and IPv6 addresses this runner has no route to.
      if [ -n "$public" ]; then by_count "[::ffff:$public], a public address carried in IPv6" "http://[::ffff:$public]/" measure "$public"; fi
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
# The realtime origin (ci/macos-app/realtime-origin.py): its port and pid.
RT_PORT='' RT_PID=''
realtime_origin() {
  : >"$LOGS/realtime-origin.jsonl"
  python3 -I -u "$here/macos-app/realtime-origin.py" "$LOGS/realtime-origin.jsonl" >"$LOGS/realtime-origin.out" 2>&1 &
  RT_PID=$!
  if ! wait_for 10 grep -q '^port [0-9]' "$LOGS/realtime-origin.out"; then
    bad "realtime origin" "it did not start: $(head -c 200 "$LOGS/realtime-origin.out")"
    return 1
  fi
  RT_PORT="$(sed -n 's/^port \([0-9][0-9]*\)$/\1/p' "$LOGS/realtime-origin.out")"
}
realtime_origin_stop() {
  local status
  [ -n "$RT_PID" ] || return 0
  if kill -0 "$RT_PID" 2>/dev/null; then
    kill "$RT_PID"
    wait "$RT_PID" 2>/dev/null
    status=$?
    [ "$status" = 143 ] || bad "realtime origin" "it ended with status $status, not from the SIGTERM sent"
  else
    wait "$RT_PID" 2>/dev/null
    bad "realtime origin" "it had already ended, status $?, before it was stopped: $(tail -n 3 "$LOGS/realtime-origin.out" | tr '\n' ' ')"
  fi
  if grep -q 'Traceback' "$LOGS/realtime-origin.out"; then
    bad "realtime origin" "it raised: $(grep -A3 'Traceback' "$LOGS/realtime-origin.out" | tail -n 1)"
  fi
  RT_PID=''
}

# The realtime origin's log lines so far, to read only what comes after.
rt_lines() { wc -l <"$LOGS/realtime-origin.jsonl" | tr -d ' '; }

# rt_said TOKEN KIND: what the front page says on its RT-KIND-TOKEN lines.
rt_said() {
  front | grep -E '^AX(StaticText|Heading)' | cut -f2,3 | tr '\t' ' ' | grep -oE "RT-$2-$1 .*" | sed 's/ *$//'
}

# rt_origin FROM TOKEN: what the realtime origin saw for TOKEN after its
# first FROM log lines: each page, each handshake with its headers and
# answer, and what it echoed, with the process each came from.
rt_origin() {
  python3 -I - "$LOGS/realtime-origin.jsonl" "$1" "$2" <<'PY'
import json, sys
log, start, token = sys.argv[1], int(sys.argv[2]), sys.argv[3]
said = []
with open(log, encoding="utf-8") as f:
    for line in f.readlines()[start:]:
        e = json.loads(line)
        if e.get("udp") or e.get("t") != token:
            continue
        if "echoed" in e:
            said.append(f"echoed {e['echoed']} message(s), ended by {e.get('ended', 'what was not kept')}")
        elif e["path"] == "/echo":
            said.append(f"a handshake from {e['process']}, Host {e['host']}, Upgrade {e['upgrade']!r}, "
                        f"Connection {e['connection']!r}, key {e['key']}, Via {e['via']!r}: answered {e['answered']}")
        else:
            said.append(f"{e['path']} from {e['process']}")
print("; ".join(said) or "nothing")
PY
}

# rt_udp FROM: the datagrams the realtime origin had after its first FROM
# log lines, by where from, what they were and the process that sent them.
rt_udp() {
  python3 -I - "$LOGS/realtime-origin.jsonl" "$1" <<'PY'
import collections, json, sys
log, start = sys.argv[1], int(sys.argv[2])
counts = collections.Counter()
with open(log, encoding="utf-8") as f:
    for line in f.readlines()[start:]:
        e = json.loads(line)
        if e.get("udp"):
            counts[(e["from"], e["stun"], e["process"])] += 1
print("; ".join(f"{n}x {kind} from {addr}, by {process}" for (addr, kind, process), n in sorted(counts.items())) or "none")
PY
}

# rt_record WHAT LINE...: one measurement, said and kept in realtime.txt.
rt_record() {
  local what="$1" l
  shift
  printf '  MEASURE  %s\n' "$what" | tee -a "$LOGS/realtime.txt"
  for l in "$@"; do printf '             %s\n' "$l" | tee -a "$LOGS/realtime.txt"; done
}

# rt_websocket WHERE: the WebSocket page for WHERE (see realtime-origin.py),
# opened from this Mac's loopback origin, which WebKit reaches by itself, so
# that only the WebSocket can move the proxy's count. Sets RT_SAID, what the
# page said; RT_SEEN, what the origin saw; and RT_BEFORE and RT_AFTER, the
# proxy's count around it.
RT_SAID='' RT_SEEN='' RT_BEFORE='' RT_AFTER=''
rt_websocket() {
  local where="$1" t from base="http://127.0.0.1:$RT_PORT"
  t="$(token)"
  counted "before the WebSocket ($where)"
  RT_BEFORE="$COUNTED"
  from="$(rt_lines)"
  visit "$base/ws?t=$t&to=$where" "$base/ws?t=$t&to=$where" "RT-WS-$t" "the WebSocket page opens ($where)"
  wait_for 30 page_says "RT-DONE-$t" || true
  RT_SAID="$(rt_said "$t" WS)"
  RT_SEEN="$(rt_origin "$from" "$t")"
  counted "after the WebSocket ($where)"
  RT_AFTER="$COUNTED"
}

# rt_carried SAID: the page saw its WebSocket open, its ping echoed, and a
# clean close with 1000 (the origin gives no reason with its close).
rt_carried() {
  case "$1" in *"; open; message echo; close 1000 clean") return 0 ;; esac
  return 1
}

# rt_answered SEEN FROM: the origin saw a handshake from FROM, with Upgrade
# websocket, answered it 101, and echoed its one message.
#
# Each of these is a function, never a case inside "$(...)": the bash these
# run under on the runner, macOS's 3.2, cannot parse a case pattern's ")"
# there, and says so only when the line runs.
rt_answered() {
  case "$1" in *"a handshake from $2,"*"Upgrade 'websocket'"*"answered 101; echoed 1 message(s)"*) return 0 ;; esac
  return 1
}

# rt_rtc_there SAID: the WebRTC page found RTCPeerConnection a function.
rt_rtc_there() {
  case "$1" in *"RTCPeerConnection: a function"*) return 0 ;; esac
  return 1
}

# rt_rtc_around SEEN: this Mac's STUN server had at least one Binding
# request, and none of them from the proxy.
rt_rtc_around() {
  [ "$(printf '%s\n' "$1" | tr ';' '\n' | grep -c 'binding from')" -ge 1 ] || return 1
  case "$1" in *syndeo-proxy*) return 1 ;; esac
  return 0
}

# rt_through_syndeo: a WebSocket to this Mac by a public name for 127.0.0.1,
# which goes through Syndeo, has to be carried: open, echoed and closed
# cleanly in the page; at the origin, one handshake from the proxy with
# Upgrade websocket, answered 101, its message echoed; and the proxy's
# cache not asked, so its count does not move. The checks are named "a
# WebSocket through Syndeo", which is what the websocket-headers-stripped
# mutant has to fail and nothing else.
rt_through_syndeo() {
  rt_websocket name
  must "$(rt_carried "$RT_SAID" && echo yes)" "a WebSocket through Syndeo opens, carries its ping back and closes cleanly in the page" "${RT_SAID:-the page said nothing}"
  must "$(rt_answered "$RT_SEEN" syndeo-proxy && echo yes)" \
    "a WebSocket through Syndeo reaches its origin from the proxy, with Upgrade websocket, is answered 101 and carries its message" "the origin saw: $RT_SEEN"
  must "$([ -n "$RT_BEFORE" ] && [ "$RT_BEFORE" = "$RT_AFTER" ] && echo yes)" \
    "a WebSocket through Syndeo is kept from the proxy's cache: its count did not move" "$RT_BEFORE before, $RT_AFTER after"
  printf '  WEBSOCKET  through Syndeo: the page: %s; the origin: %s; the proxy count: %s before, %s after\n' \
    "${RT_SAID:-nothing}" "$RT_SEEN" "$RT_BEFORE" "$RT_AFTER" >>"$LOGS/realtime.txt"
}

# realtime APP: see the top of this script.
realtime() {
  local app P iface lan t from before said seen
  [ -d "${1:-}" ] || { say "usage: $0 realtime <Syndeo.app>"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  printf '\nWebSockets through Syndeo, and WebRTC around it\n'
  : >"$LOGS/realtime.txt"
  realtime_origin || return 1
  P="$RT_PORT"
  iface="$(/sbin/route -n get default 2>/dev/null | awk '/interface:/ {print $2}')"
  lan="$([ -n "$iface" ] && ipconfig getifaddr "$iface" 2>/dev/null)"
  if ! started "$app" realtime; then
    realtime_origin_stop
    return 1
  fi
  helper_of "$app" realtime
  browses realtime
  must "$(page_says 'WebRTC media and peer connections connect directly and are not sent through or cached by Syndeo\.' &&
    page_says 'Syndeo is not a VPN\.' && echo yes)" \
    "the start page says WebRTC connects directly, not through Syndeo, and that Syndeo is not a VPN" "$(page_text)"

  rt_websocket loop
  must "$(rt_carried "$RT_SAID" && rt_answered "$RT_SEEN" com.apple.WebKit.Networking && echo yes)" \
    "a WebSocket to this Mac's loopback address, which WebKit opens itself, opens, carries its ping back and closes cleanly" \
    "the page: ${RT_SAID:-nothing}; the origin: $RT_SEEN"
  rt_through_syndeo

  # Public servers: measured, not gated, since what they do is theirs.
  rt_websocket echo
  rt_record "a WebSocket, ws://, to a public echo server (echo.websocket.org)" "the page: ${RT_SAID:-nothing}" "the proxy's count: $RT_BEFORE before, $RT_AFTER after"
  rt_websocket echos
  rt_record "a WebSocket, wss://, to the same public echo server" "the page: ${RT_SAID:-nothing}" "the proxy's count: $RT_BEFORE before, $RT_AFTER after"

  t="$(token)"
  counted "before WebRTC"
  before="$COUNTED"
  from="$(rt_lines)"
  visit "http://127.0.0.1:$P/rtc?t=$t&lan=$lan" "http://127.0.0.1:$P/rtc?t=$t&lan=$lan" "RT-RTC-$t" "the WebRTC page opens"
  wait_for 40 page_says "RT-DONE-$t" || true
  said="$(rt_said "$t" RTC | tr '\n' '|' | sed 's/|$//; s/|/ | /g')"
  seen="$(rt_udp "$from")"
  counted "after WebRTC"
  # What the start page says about WebRTC, kept true: it is there, and it
  # goes around Syndeo — its STUN reaches this Mac's server, and none of it
  # from the proxy, and the proxy's count does not move.
  must "$(rt_rtc_there "$said" && echo yes)" "WebRTC is there: RTCPeerConnection is a function" "${said:-the page said nothing}"
  must "$(rt_rtc_around "$seen" && echo yes)" \
    "WebRTC goes around Syndeo: this Mac's STUN server had Binding requests, none from syndeo-proxy" "the STUN server had: $seen"
  must "$([ -n "$before" ] && [ "$before" = "$COUNTED" ] && echo yes)" "WebRTC goes around Syndeo: the proxy's count did not move" "$before before, $COUNTED after"
  rt_record "WebRTC, from a page on this Mac's loopback; STUN at ${lan:-no LAN address}:$P, 127.0.0.1:$P and stun.l.google.com:19302" \
    "the page: ${said:-nothing}" "this Mac's STUN server: $seen" "the proxy's count: $before before, $COUNTED after"

  stopped realtime
  realtime_origin_stop
  printf '\n%d failed\n' "$fails"
  [ "$fails" = 0 ]
}

# The runner's appearance, dark or light, as System Events has it; and
# setting it. Each gives up after 20 s.
appearance_now() {
  perl -e 'alarm 20; exec @ARGV' osascript -e 'tell application "System Events" to tell appearance preferences to get dark mode' 2>/dev/null |
    sed 's/^true$/dark/; s/^false$/light/'
}
appearance_set() {
  perl -e 'alarm 20; exec @ARGV' osascript -e "tell application \"System Events\" to tell appearance preferences to set dark mode to $([ "$1" = dark ] && echo true || echo false)" >/dev/null 2>&1
}

# looks APP: see the top of this script.
looks() {
  local app appearance round was page
  [ -d "${1:-}" ] || { say "usage: $0 looks <Syndeo.app>"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  was="$(appearance_now)"
  printf '\nthe runner'"'"'s appearance before: %s\n' "${was:-unknown}"
  for appearance in light dark; do
    round="looks-$appearance"
    printf '\nSyndeo.app, in %s appearance: %s\n' "$appearance" "$app"
    appearance_set "$appearance"
    must "$([ "$(appearance_now)" = "$appearance" ] && echo yes)" "the runner is in $appearance appearance" "System Events says '$(appearance_now)'"
    started "$app" "$round" || continue
    helper_of "$app" "$round"
    browses "$round"
    arrive "" 'Welcome to Syndeo' "the window shows the start page ($appearance)"
    chrome "$appearance"
    look "$appearance-start"
    plain_http
    route_words "$appearance"
    look "$appearance-page"
    if origin; then
      page="http://127.0.0.1:$ORIGIN_PORT/policy.html"
      visit "$page" "$page" 'Policy test' "a page on this Mac's loopback opens ($appearance)"
      must "$([ "$(bar)" = "$DIRECT_LABEL" ] && echo yes)" "and the address field's route says '$DIRECT_LABEL' ($appearance)" "it says '$(bar)'"
      look "$appearance-direct"
    fi
    visit "file:///etc/passwd" "file:///etc/passwd" 'not file: ones' "Syndeo's own page for an address it refuses ($appearance)"
    look "$appearance-refused"
    settings_open "$appearance"
    look "$appearance-settings"
    settings_close "$appearance"
    stopped "$round"
  done
  origin_stop
  [ -z "$was" ] || appearance_set "$was"
  must "$([ -n "$was" ] && [ "$(appearance_now)" = "$was" ] && echo yes)" "the runner's appearance is put back as it was: ${was:-unknown}" "System Events says '$(appearance_now)'"
  printf '\n%d failed\n' "$fails"
  [ "$fails" = 0 ]
}

# The pages tabs opens, served by the loopback origin and reached through
# Syndeo by a public name for 127.0.0.1: a page of links, three pages to
# open from it, Page A naming an icon, a PNG this writes, and the origin's
# slow page. Sets TP, the address they are under.
TP=''
tab_pages() {
  local dir="$LOGS/origin"
  origin || return 1
  TP="http://127.0.0.1.nip.io:$ORIGIN_PORT"
  cat >"$dir/tabs.html" <<'EOF'
<!doctype html><meta charset="utf-8"><title>Tabs test</title>
<h1>SYNDEO-TABS-PAGE</h1>
<p><a href="/page-a.html">Page A link</a></p>
<p><a href="/page-b.html">Page B link</a></p>
<p><a href="/page-c.html">Page C link</a></p>
<p><a href="/slow.html">Slow link</a></p>
<p><a href="file:///etc/passwd">File link</a></p>
EOF
  printf '<!doctype html><meta charset="utf-8"><title>Page A</title><link rel="icon" href="/icon.png"><h1>SYNDEO-PAGE-A</h1>\n' >"$dir/page-a.html"
  printf '<!doctype html><meta charset="utf-8"><title>Page B</title><h1>SYNDEO-PAGE-B</h1>\n' >"$dir/page-b.html"
  printf '<!doctype html><meta charset="utf-8"><title>Page C</title><h1>SYNDEO-PAGE-C</h1>\n' >"$dir/page-c.html"
  python3 -I -c '
import struct, sys, zlib
side = 16
rows = b"".join(b"\x00" + b"\x2a\x6f\xdb\xff" * side for _ in range(side))
def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xffffffff)
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", side, side, 8, 6, 0, 0, 0)) \
    + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")
open(sys.argv[1], "wb").write(png)' "$dir/icon.png"
}

# open_now LOG: how many windows, each tab one, the app last said it has.
open_now() { grep -oE 'a window (opened|closed) open=[0-9]+' "$1" | tail -n 1 | grep -oE '[0-9]+$'; }
# opens LOG N WHAT: within 15 s, the app says it has N.
opens() {
  if wait_for 15 sh -c '[ "$(grep -oE "a window (opened|closed) open=[0-9]+" "$1" | tail -n 1 | grep -oE "[0-9]+\$")" = "$2" ]' _ "$1" "$2"; then
    ok "$3"
  else
    bad "$3" "the app says it has $(open_now "$1") open, not $2"
  fi
}
# The front window's tabs, as tabs prints them, on one line: "* Title|  Title|".
tab_list() { "$APPCTL" tabs "$PID" | tr '\n' '|'; }
route_is() { [ "$("$APPCTL" value "$PID" syndeo.route)" = "$1" ]; }
# press_keys KEY KEYS: KEY pressed holding KEYS, as appctl keys does.
press_keys() { "$APPCTL" keys "$PID" "$1" "$2" || bad "$2+$1" "pid $PID could not be brought to the front"; }
# logged LOG TEXT N: within 15 s, LOG has at least N lines with TEXT.
logged() { wait_for 15 sh -c '[ "$(grep -cF "$2" "$1")" -ge "$3" ]' _ "$1" "$2" "$3"; }

# WebKit's WebContent processes, and memory.
webcontent() { pgrep -f 'com.apple.WebKit.WebContent' | sort -n; }
# rss_of PID...: their resident memory together, in MB.
rss_of() {
  local kb=0 p v
  for p in "$@"; do
    v="$(ps -o rss= -p "$p" 2>/dev/null | tr -d ' ')"
    kb=$((kb + ${v:-0}))
  done
  echo $((kb / 1024))
}
# ours BEFORE: the WebContent processes that were not there before Syndeo opened.
ours() {
  local p
  for p in $(webcontent); do
    case " $1 " in *" $p "*) ;; *) printf '%s ' "$p" ;; esac
  done
}
# memory_now BEFORE OUT: four samples, 3 s apart, of the app's own memory and
# its WebContent processes', each kept in OUT; prints the last, "host H web W
# processes N".
memory_now() {
  local web line i
  for i in 1 2 3 4; do
    web="$(ours "$1")"
    # shellcheck disable=SC2086 # pids, split on purpose
    line="host $(rss_of "$PID") web $(rss_of $web) processes $(printf '%s' "$web" | wc -w | tr -d ' ')"
    printf '    sample %s: %s\n' "$i" "$line" >>"$2"
    [ "$i" = 4 ] || sleep 3
  done
  printf '%s\n' "$line"
}

# tab_memory LOG BEFORE: memory per tab, as README's Memory section measures
# memory: the same page in one tab, then in five, both sampled over the same
# interval; the cost of each tab past the first is reported, not gated.
tab_memory() {
  local log="$1" before="$2" one five n out="$LOGS/tabs-memory.txt"
  : >"$out"
  visit "$TP/page-c.html" "$TP/page-c.html" 'SYNDEO-PAGE-C' "a page to measure memory with opens, in one tab"
  sleep 5
  printf 'Page C (%s), in one tab\n' "$TP/page-c.html" >>"$out"
  one="$(memory_now "$before" "$out")"
  for n in 2 3 4 5; do
    choose File "New Tab"
    arrive "" 'Welcome to Syndeo' "tab $n, to measure memory with, shows the start page"
    visit "$TP/page-c.html" "$TP/page-c.html" 'SYNDEO-PAGE-C' "the same page opens in tab $n"
  done
  sleep 5
  printf 'the same page, in five tabs\n' >>"$out"
  five="$(memory_now "$before" "$out")"
  printf 'each tab past the first: %s\n' "$(awk -v a="$one" -v b="$five" 'BEGIN {
    split(a, x, " "); split(b, y, " ")
    printf "host %+.1f MB, WebContent %+.1f MB, %+.1f WebContent processes", (y[2] - x[2]) / 4, (y[4] - x[4]) / 4, (y[6] - x[6]) / 4 }')" >>"$out"
  sed 's/^/  MEASURE  memory: /' "$out"
  for n in 5 4 3 2; do close_tab "tab $n, measured"; done
  opens "$log" 1 "the tabs measured are closed, and the app lets them go"
}

# tab_shortcuts: the new tab items' shortcuts, as AppKit has them.
tab_shortcuts() {
  local menu item key have
  while IFS='|' read -r menu item key; do
    have="$("$APPCTL" shortcut "$PID" "$menu" "$item")"
    must "$([ "$(printf '%s' "$have" | tr '[:lower:]' '[:upper:]')" = "$key" ] && echo yes)" "$menu > $item is ⇧⌘${key% *}" "${have:-no such item}"
  done <<'EOF'
History|Reopen Last Closed Tab|T 1
View|Show All Tabs|\ 1
EOF
  for menu in File View History Window; do
    "$APPCTL" menu-items "$PID" "$menu" >"$LOGS/tabs-menu-$menu.txt" 2>&1
  done
  must "$([ "$(grep -cx 'Show All Tabs' "$LOGS/tabs-menu-View.txt")" = 1 ] && [ "$(grep -cx 'Show Tab Bar' "$LOGS/tabs-menu-View.txt")" = 1 ] && echo yes)" \
    "View has Show Tab Bar and Show All Tabs, once each" "$(tr '\n' '|' <"$LOGS/tabs-menu-View.txt")"
  must "$(grep -qx 'Duplicate Tab' "$LOGS/tabs-menu-Window.txt" && grep -qx 'Pin Tab' "$LOGS/tabs-menu-Window.txt" && echo yes)" \
    "Window has Duplicate Tab and Pin Tab" "$(tr '\n' '|' <"$LOGS/tabs-menu-Window.txt")"
  must "$(grep -qx 'Reopen All Windows from Last Session' "$LOGS/tabs-menu-History.txt" && grep -qx 'Recently Closed' "$LOGS/tabs-menu-History.txt" && echo yes)" \
    "History has Reopen All Windows from Last Session and Recently Closed" "$(tr '\n' '|' <"$LOGS/tabs-menu-History.txt")"
}

# tabs_cleared SESSION: Clear Browsing Data forgets the tabs closed and the
# session kept on disk at SESSION; both are there before.
tabs_cleared() {
  local session="$1"
  choose File "New Tab"
  arrive "" 'Welcome to Syndeo' "a tab to close before clearing"
  close_tab "a tab closed before clearing"
  must "$([ "$("$APPCTL" menu-enabled "$PID" History "Reopen Last Closed Tab")" = enabled ] && echo yes)" \
    "before clearing, History > Reopen Last Closed Tab is offered"
  must "$([ -f "$session" ] && echo yes)" "before clearing, a session is kept on disk" "$(ls -l "$session" 2>&1)"
  settings_open "clearing tabs"
  if "$APPCTL" press "$PID" syndeo.clear; then ok "Settings > Clear Browsing Data… is pressed (tabs)"; else bad "Clear Browsing Data… (tabs)" "no button to press"; fi
  if wait_for 10 "$APPCTL" press "$PID" syndeo.clear-confirm; then ok "and asks first; Clear is pressed (tabs)"; else bad "Clear Browsing Data (tabs)" "no Clear in a sheet to press"; fi
  if wait_for 40 sh -c '"$1" value "$2" syndeo.clear-status | grep -q "^Browsing data cleared"' _ "$APPCTL" "$PID"; then
    ok "Settings says the browsing data is cleared (tabs)"
  else
    bad "Clear Browsing Data (tabs)" "Settings says: $("$APPCTL" value "$PID" syndeo.clear-status)"
  fi
  settings_close "after clearing tabs"
  must "$([ ! -e "$session" ] && echo yes)" "Clear Browsing Data forgets the session kept on disk" "$(ls -l "$session" 2>&1)"
  must "$([ "$("$APPCTL" menu-enabled "$PID" History "Reopen Last Closed Tab")" = disabled ] && echo yes)" \
    "Clear Browsing Data forgets the closed tabs: History > Reopen Last Closed Tab is not offered"
  must "$([ -z "$("$APPCTL" submenu-items "$PID" History "Recently Closed")" ] && echo yes)" \
    "Clear Browsing Data forgets the closed tabs: History > Recently Closed is empty" \
    "$("$APPCTL" submenu-items "$PID" History "Recently Closed" | tr '\n' '|')"
  must "$([ "$("$APPCTL" menu-enabled "$PID" History "Reopen All Windows from Last Session")" = disabled ] && echo yes)" \
    "Clear Browsing Data forgets the session: History > Reopen All Windows from Last Session is not offered"
}

# ax_windows: how many windows Accessibility has for the app.
ax_windows() { "$APPCTL" ax-tree "$PID" | grep -c '^AXWindow'; }

# The two ways a person asks for the tab overview.
overview_by_keys() { press_keys '\' cmd+shift; }
overview_by_menu() { choose View "Show All Tabs"; }

# overview_shows N ACTION WANT WHAT: run ACTION, the Nth request for the tab
# overview, and require AppKit's state around it, as the app logs it, to be
# WANT: "was=X shown=Y", whether it was shown when asked for, and whether it
# is a second later.
overview_shows() {
  local said
  "$2"
  wait_for 10 sh -c '[ "$(grep -c "the tab overview, a second later" "$1")" -ge "$2" ]' _ "$log" "$1" || true
  said="$([ "$(grep -c 'the tab overview, a second later' "$log")" -ge "$1" ] &&
    printf 'was=%s shown=%s' "$(grep 'the tab overview asked for' "$log" | tail -n 1 | sed -n 's/.*was=\([a-z]*\).*/\1/p')" \
      "$(grep 'the tab overview, a second later' "$log" | tail -n 1 | sed -n 's/.*shown=\([a-z]*\).*/\1/p')")"
  must "$([ "$said" = "$3" ] && echo yes)" "$4: AppKit's tab group, $3" "the app said ${said:-nothing for request $1}"
}

# show_tab N ADDRESS PAGE WHAT: ⌘N, and the tab it shows at ADDRESS saying
# PAGE. If it does not, that tab is pressed in the tab bar instead, so the
# checks after this one still start where they should.
show_tab() {
  press_keys "$1" cmd
  if wait_for 15 at "$2" "$3"; then
    ok "$4"
  else
    bad "$4" "address field: '$(address)'; tabs: $(tab_list)"
    if [ "$1" = 9 ]; then "$APPCTL" tab-press "$PID" "$("$APPCTL" tabs "$PID" | wc -l | tr -d ' ')"; else "$APPCTL" tab-press "$PID" "$1"; fi
    wait_for 15 at "$2" "$3" || true
  fi
}

# tabbing LOG: links opened in tabs by ⌘ and the middle button, ⌘ digits,
# a refused address, icons and loading in the tab bar, Duplicate, Pin, the
# tab's own menu, Close Other Tabs, Reopen Last Closed Tab and Recently
# Closed, and Show All Tabs.
tabbing() {
  local log="$1" list last_closed items before
  visit "$TP/tabs.html" "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "a page with links opens, by a public name"
  must "$(route_is 'Through Syndeo' && echo yes)" "and came through Syndeo" "the bar says '$("$APPCTL" value "$PID" syndeo.route)'"
  if wait_for 10 sh -c '[ "$("$1" titles "$2" | head -n 1)" = "Tabs test" ]' _ "$APPCTL" "$PID"; then
    ok "the window takes its title from the page"
  else
    bad "the window takes its title from the page" "titles: $("$APPCTL" titles "$PID" | tr '\n' '|')"
  fi

  "$APPCTL" click-web "$PID" "Page A link" cmd left || bad "⌘-click" "no Page A link to click"
  opens "$log" 2 "⌘-click on a link opens it in a new tab"
  sleep 2
  arrive "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "behind: the page clicked on stays in front, as it was"
  if wait_for 15 sh -c '[ "$("$1" tabs "$2" | tr "\n" "|")" = "* Tabs test|  Page A|" ]' _ "$APPCTL" "$PID"; then
    ok "and the tab behind it is after it, with its page's title, Page A"
  else
    bad "⌘-click's tab" "tabs: $(tab_list)"
  fi

  "$APPCTL" click-web "$PID" "Page B link" none middle || bad "middle-click" "no Page B link to click"
  opens "$log" 3 "a middle-click on a link opens it in a new tab"
  sleep 2
  arrive "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "behind, too"
  if wait_for 15 sh -c '[ "$("$1" tabs "$2" | tr "\n" "|")" = "* Tabs test|  Page A|  Page B|" ]' _ "$APPCTL" "$PID"; then
    ok "tabs opened behind a page are in the order they were opened, after it"
  else
    bad "tabs opened behind" "tabs: $(tab_list)"
  fi

  "$APPCTL" click-web "$PID" "Page C link" cmd+shift left || bad "⇧⌘-click" "no Page C link to click"
  opens "$log" 4 "⇧⌘-click on a link opens it in a new tab"
  arrive "$TP/page-c.html" 'SYNDEO-PAGE-C' "in front"
  must "$(route_is 'Through Syndeo' && echo yes)" "and the tab came through Syndeo" "the bar says '$("$APPCTL" value "$PID" syndeo.route)'"
  if wait_for 10 sh -c '[ "$("$1" tabs "$2" | tr "\n" "|")" = "  Tabs test|* Page C|  Page A|  Page B|" ]' _ "$APPCTL" "$PID"; then
    ok "right after the page it was opened from"
  else
    bad "a tab opened in front goes right after its page" "tabs: $(tab_list)"
  fi
  # The menus as AppKit has them with tabs showing, which is when it adds
  # its own items.
  for items in File View Window History; do
    "$APPCTL" menu-dump "$PID" "$items" >"$LOGS/tabs-menu-$items-with-tabs.txt" 2>&1
  done
  must "$([ "$(grep -c "^Close Other Tabs$(printf '\t')" "$LOGS/tabs-menu-File-with-tabs.txt")" = 1 ] && echo yes)" \
    "with tabs showing, File has AppKit's Close Other Tabs, once" "$(tr '\t\n' ' |' <"$LOGS/tabs-menu-File-with-tabs.txt")"
  must "$([ "$(grep -c "^Show All Tabs$(printf '\t')" "$LOGS/tabs-menu-View-with-tabs.txt")" = 1 ] && echo yes)" \
    "and View one Show All Tabs" "$(tr '\t\n' ' |' <"$LOGS/tabs-menu-View-with-tabs.txt")"

  show_tab 1 "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "⌘1 shows the first tab"
  "$APPCTL" click-web "$PID" "File link" cmd left || bad "⌘-click on a file: link" "no File link to click"
  sleep 3
  must "$([ "$(open_now "$log")" = 4 ] && echo yes)" "⌘-click on a file: link opens no tab" "the app says $(open_now "$log") open"
  must "$(at "$TP/tabs.html" 'SYNDEO-TABS-PAGE' && echo yes)" "and leaves the page as it was" "address '$(address)'"
  must "$("$APPCTL" ax-tree "$PID" | grep -q 'root:' && echo no || echo yes)" "and shows nothing of that file"

  show_tab 9 "$TP/page-b.html" 'SYNDEO-PAGE-B' "⌘9 shows the last tab, the middle-click's"
  must "$(route_is 'Through Syndeo' && echo yes)" "which came through Syndeo" "the bar says '$("$APPCTL" value "$PID" syndeo.route)'"
  show_tab 2 "$TP/page-c.html" 'SYNDEO-PAGE-C' "⌘2 shows the second tab"
  show_tab 3 "$TP/page-a.html" 'SYNDEO-PAGE-A' "⌘3 shows the third, the ⌘-click's"
  must "$(route_is 'Through Syndeo' && echo yes)" "which came through Syndeo" "the bar says '$("$APPCTL" value "$PID" syndeo.route)'"
  if wait_for 15 "$APPCTL" ids "$PID" syndeo.tab-icon; then
    ok "its tab shows the icon Page A names: $("$APPCTL" ids "$PID" syndeo.tab-icon | head -n 1 | tr '\t' ' ')"
  else
    bad "its tab shows the icon Page A names" "no syndeo.tab-icon; the app: $(grep "a page's icon" "$log" | tail -n 2 | tr '\n' ' ')"
  fi
  "$APPCTL" front-tree "$PID" >"$LOGS/tabs-bar-tree.txt" 2>&1

  show_tab 1 "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "⌘1 shows the first tab again"
  "$APPCTL" click-web "$PID" "Slow link" cmd+shift left || bad "⇧⌘-click on Slow link" "no Slow link to click"
  opens "$log" 5 "⇧⌘-click on a slow page opens a tab in front"
  if wait_for 6 "$APPCTL" ids "$PID" syndeo.tab-loading; then ok "its tab shows it loading"; else bad "its tab shows it loading" "no syndeo.tab-loading"; fi
  "$APPCTL" id-tree "$PID" >"$LOGS/tabs-loading-ids.txt" 2>&1
  arrive "$TP/slow.html" 'SYNDEO-SLOW-PAGE' "the slow page arrives"
  if wait_for 10 sh -c '! "$1" ids "$2" syndeo.tab-loading' _ "$APPCTL" "$PID"; then ok "and its tab stops showing it loading"; else bad "its tab stops showing it loading" "still loading"; fi

  show_tab 1 "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "⌘1 shows the first tab, to duplicate"
  choose Window "Duplicate Tab"
  opens "$log" 6 "Window > Duplicate Tab opens a tab"
  arrive "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "with the same page, in front"
  must "$(logged "$log" 'a tab duplicated' 1 && echo yes)" "the app says a tab was duplicated"
  must "$([ "$("$APPCTL" enabled "$PID" syndeo.back)" = enabled ] && echo yes)" "with its history: Back is offered" "$(controls)"
  list="$(tab_list)"
  must "$([ "$list" = "  Tabs test|* Tabs test|  Slow page|  Page C|  Page A|  Page B|" ] && echo yes)" "right after the tab it duplicates" "tabs: $list"

  choose Window "Pin Tab"
  must "$(logged "$log" 'a tab pinned or unpinned pinned=true' 1 && echo yes)" "Window > Pin Tab pins it"
  if wait_for 10 sh -c '[ "$("$1" tabs "$2" | tr "\n" "|")" = "* Tabs test|  Tabs test|  Slow page|  Page C|  Page A|  Page B|" ]' _ "$APPCTL" "$PID"; then
    ok "and puts it first"
  else
    bad "a pinned tab goes first" "tabs: $(tab_list)"
  fi
  if wait_for 5 "$APPCTL" ids "$PID" syndeo.tab-pinned; then ok "marked pinned in the tab bar"; else bad "a pinned tab is marked" "no syndeo.tab-pinned"; fi

  items="$("$APPCTL" tab-menu "$PID" 1 | tr '\n' '|')"
  must "$(printf '%s' "|$items" | grep -qF '|Unpin Tab|' && echo yes)" "a pinned tab's own menu offers Unpin Tab" "$items"
  items="$("$APPCTL" tab-menu "$PID" 2 | tr '\n' '|')"
  printf '%s\n' "$items" >"$LOGS/tabs-tab-menu.txt"
  must "$(for want in 'Duplicate Tab' 'Pin Tab' 'Close Tab' 'Close Other Tabs' 'Move Tab to New Window' 'Show All Tabs'; do
      printf '%s' "|$items" | grep -qF "|$want|" || exit 1; done && echo yes)" \
    "a secondary click on a tab opens its own menu: Duplicate, Pin, Close, Close Other Tabs, Move to New Window, Show All" "$items"
  "$APPCTL" tab-menu "$PID" 2 "Duplicate Tab" >/dev/null || bad "the tab's menu > Duplicate Tab" "it could not be pressed"
  opens "$log" 7 "its Duplicate Tab duplicates that tab"
  arrive "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "in front"

  choose File "Close Other Tabs"
  opens "$log" 2 "File > Close Other Tabs closes every other tab but the pinned one"
  list="$(tab_list)"
  must "$([ "$list" = "  Tabs test|* Tabs test|" ] && echo yes)" "leaving the pinned tab first, and this one" "tabs: $list"
  must "$(logged "$log" 'a pinned tab kept open' 1 && echo yes)" "the pinned tab said no to closing"
  items="$("$APPCTL" submenu-items "$PID" History "Recently Closed" | tr '\n' '|')"
  printf '%s\n' "$items" >"$LOGS/tabs-recently-closed.txt"
  must "$(for want in 'Page A' 'Page B' 'Page C' 'Slow page'; do printf '%s' "|$items" | grep -qF "|$want|" || exit 1; done && echo yes)" \
    "History > Recently Closed lists the tabs closed, by title" "$items"

  last_closed="$(printf '%s' "$items" | cut -d'|' -f1)"
  press_keys t cmd+shift
  opens "$log" 3 "⇧⌘T opens a closed tab again"
  if wait_for 30 sh -c '"$1" tabs "$2" | grep -qxF "* $3"' _ "$APPCTL" "$PID" "$last_closed"; then
    ok "the last one closed, $last_closed, in front"
  else
    bad "⇧⌘T opens the last tab closed" "it was '$last_closed'; tabs: $(tab_list)"
  fi
  if [ "$last_closed" != 'Page A' ]; then
    "$APPCTL" submenu "$PID" History "Recently Closed" "Page A" || bad "History > Recently Closed > Page A" "no such item"
    opens "$log" 4 "History > Recently Closed > Page A opens it again"
    arrive "$TP/page-a.html" 'SYNDEO-PAGE-A' "in front, with its page"
  fi

  # AppKit's overview is not a window to Accessibility on the runner (CI
  # 39738260562: none appeared while AppKit said it was shown), so what is
  # gated is AppKit's own state, NSWindowTabGroup.isOverviewVisible, as the
  # app logs it before each request and a second after.
  overview_shows 1 overview_by_keys "was=false shown=true" "⇧⌘\\ shows AppKit's overview of the tabs"
  "$APPCTL" ax-tree "$PID" >"$LOGS/tabs-overview-tree.txt" 2>&1
  printf '  MEASURE  with the overview shown, Accessibility has %s window(s)\n' "$(ax_windows)"
  overview_shows 2 overview_by_menu "was=true shown=false" "View > Show All Tabs, with it shown, leaves it"
  overview_shows 3 overview_by_menu "was=false shown=true" "View > Show All Tabs shows it"
  press_keys escape none
  # AppKit's overview closes with an animation; asked for again while it
  # was still closing, it stayed closed (CI 39858637347).
  sleep 3
  overview_shows 4 overview_by_keys "was=false shown=true" "Escape leaves it: asked for again, AppKit had it not shown, and shows it"
  overview_shows 5 overview_by_menu "was=true shown=false" "View > Show All Tabs again leaves it"
  if wait_for 10 "$APPCTL" value "$PID" syndeo.address; then ok "and the window is as it was, its address field in front"; else bad "leaving the overview" "no address field in front"; fi
}

# tabs APP: see the top of this script.
tabs_test() {
  local app home session log before on_screen
  [ -d "${1:-}" ] || { say "usage: $0 tabs <Syndeo.app>"; exit 2; }
  app="$(cd "$1" && pwd -P)"
  home="$HOME/Library/Application Support/Syndeo"
  session="$home/session.txt"
  no_app || { say "refusing: $ID is already running"; exit 2; }
  printf '\nSyndeo.app, its tabs: %s\n' "$app"
  # From no session, and macOS's own setting: whatever an earlier test left.
  rm -f "$session"
  defaults delete "$ID" NSQuitAlwaysKeepsWindows >/dev/null 2>&1 || true
  tab_pages || { printf '\n%d failed\n' "$fails"; return 1; }
  before="$(webcontent | tr '\n' ' ')"
  if started "$app" tabs; then
    helper_of "$app" tabs
    browses tabs
    log="$LOGS/app-tabs.stderr"
    arrive "" 'Welcome to Syndeo' "a new window shows the start page (tabs)"
    tab_shortcuts
    tab_memory "$log" "$before"
    tabbing "$log"
    must "$([ "$(pgrep -P "$PID" | tr -d '\n')" = "$HELPER" ] && echo yes)" "with all those tabs opened and closed, the proxy is still the one helper" \
      "children: $(pgrep -P "$PID" | tr '\n' ' ')"
  fi
  stopped tabs

  printf '\nquit, with a pinned tab: kept, and opened again\n'
  must "$([ -f "$session" ] && [ "$(stat -f %Lp "$session")" = 600 ] && echo yes)" "quitting kept the session, readable by this user alone" "$(ls -l "$session" 2>&1)"
  must "$([ "$(grep -c "$(printf '^tab\tpinned\t')" "$session" 2>/dev/null)" = 1 ] && echo yes)" "with one tab pinned"
  if started "$app" tabs-pinned; then
    helper_of "$app" tabs-pinned
    log="$LOGS/app-tabs-pinned.stderr"
    arrive "" 'Welcome to Syndeo' "opened again, the window shows the start page"
    if wait_for 15 sh -c '"$1" tabs "$2" | head -n 1 | grep -qx "  Tabs test"' _ "$APPCTL" "$PID"; then
      ok "with the pinned tab before it, as it was"
    else
      bad "the pinned tab, opened again" "tabs: $(tab_list)"
    fi
    must "$(wait_for 5 "$APPCTL" ids "$PID" syndeo.tab-pinned && echo yes)" "marked pinned"
    press_keys 1 cmd
    arrive "$TP/tabs.html" 'SYNDEO-TABS-PAGE' "the pinned tab opens its page when it is shown"
    choose History "Reopen All Windows from Last Session"
    must "$(logged "$log" 'tabs from the last session opened windows=1' 1 && echo yes)" "History > Reopen All Windows from Last Session opens the rest"
    arrive "$TP/page-a.html" 'SYNDEO-PAGE-A' "in a window of their own, with the tab that was in front, in front"
    "$APPCTL" tabs "$PID" >"$LOGS/tabs-session-window.txt" 2>&1
    must "$(grep -qx '  Tabs test' "$LOGS/tabs-session-window.txt" && grep -qx '\* Page A' "$LOGS/tabs-session-window.txt" && echo yes)" \
      "and the other tabs it had, by their titles, not yet opened" "tabs: $(tab_list)"
  fi
  stopped tabs-pinned

  printf '\nquit, with macOS set to keep windows: every tab opened again\n'
  defaults write "$ID" NSQuitAlwaysKeepsWindows -bool true
  if started "$app" tabs-kept; then
    helper_of "$app" tabs-kept
    log="$LOGS/app-tabs-kept.stderr"
    must "$(logged "$log" 'tabs from the last session opened windows=2' 1 && echo yes)" "opened again, every window's tabs open again" \
      "$(grep 'session' "$log" | tail -n 2 | tr '\n' ' ')"
    arrive "$TP/page-a.html" 'SYNDEO-PAGE-A' "the tab that was in front is in front, with its page"
    on_screen="$("$APPCTL" windows "$PID" | wc -l | tr -d ' ')"
    must "$([ "$on_screen" = 2 ] && echo yes)" "in two windows, as they were" "$on_screen on screen"
    tabs_cleared "$session"
  fi
  stopped tabs-kept
  defaults delete "$ID" NSQuitAlwaysKeepsWindows >/dev/null 2>&1 || true
  rm -f "$session"
  origin_stop
  must "$(defaults read "$ID" NSQuitAlwaysKeepsWindows >/dev/null 2>&1 && echo no || echo yes)" "the setting this test made is removed again"
  printf '\n%d failed\n' "$fails"
  [ "$fails" = 0 ]
}

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
  customer) shift; customer "$@" ;;
  realtime) shift; realtime "$@" ;;
  looks) shift; looks "$@" ;;
  tabs) shift; tabs_test "$@" ;;
  measure) measure ;;
  *) say "usage: $0 launch <Syndeo.app> | refuse <Syndeo.app> | mutant <name> <Syndeo.app> | routes <Syndeo.app> | customer <Syndeo.app> [again] | realtime <Syndeo.app> | looks <Syndeo.app> | tabs <Syndeo.app> | measure"; exit 2 ;;
esac
