#!/usr/bin/env bash
# Check a published release the way somebody receiving it would.
#
#   ci/verify-release.sh [version]
#
# Installs from the release with the same one-liner the README gives, into a
# throwaway directory, and then asks the installed binaries to demonstrate the
# things that have broken before. Every check here exists because something it
# covers shipped broken once:
#
#   - cache statistics vanished when the network process was killed rather than
#     asked to stop, so a hit was served and never recorded
#   - the proxy forwarded connection headers into HTTP/2 and every Google host
#     answered 502
#   - binaries were installed into separate directories and could not find each
#     other
#
# Exits non-zero if any of them fails, so it can gate a release rather than
# decorate one.
set -uo pipefail

VERSION="${1:-}"
REPO="SUM-INNOVATION/syndeo"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
BIN="$WORK/bin"
HOME_DIR="$WORK/home"

pass=0; fail=0
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s — %s\n' "$1" "$2"; fail=$((fail+1)); }
note() { printf '\n  %s\n' "$1"; }

note "installing from the published release, as a user would"
if ! SYNDEO_INSTALL_DIR="$BIN" SYNDEO_HOME="$HOME_DIR" SYNDEO_VERSION="$VERSION" \
     sh -c "curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sh" >"$WORK/install.log" 2>&1; then
  bad "install.sh completes" "$(tail -3 "$WORK/install.log" | tr '\n' ' ')"
  printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "install.sh completes, verifying the checksum"

note "what it installed"
missing=""
for b in syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui; do
  [ -x "$BIN/$b" ] || missing="$missing $b"
done
[ -z "$missing" ] && ok "every binary is present and executable" || bad "binaries present" "missing:$missing"

reported=$("$BIN/syndeo" --version 2>/dev/null | awk '{print $2}')
if [ -n "$VERSION" ]; then
  [ "$reported" = "$VERSION" ] && ok "reports version $VERSION" || bad "version" "reports ${reported:-nothing}"
else
  [ -n "$reported" ] && ok "reports version $reported" || bad "version" "reports nothing"
fi

note "the process model"
doctor=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" doctor 2>/dev/null)
echo "$doctor" | grep -q 'NOT FOUND' \
  && bad "the shell finds its sibling processes" "$(echo "$doctor" | grep 'NOT FOUND' | tr '\n' ' ')" \
  || ok "the shell finds its sibling processes"
echo "$doctor" | grep -q 'keystore .*initialized' \
  && ok "the keystore starts and answers" \
  || bad "the keystore starts" "$(echo "$doctor" | grep -i keystore | head -1)"

note "fetching, and the cache"
out=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" browse https://www.rust-lang.org/ --twice 2>/dev/null)
echo "$out" | grep -qE '^  200' && ok "fetches a page over the network" || bad "fetch" "no 200 in the output"
echo "$out" | grep -q 'again   cache' && ok "the second fetch is served from cache" || bad "cache hit" "the second fetch was not a hit"

# The regression that nearly shipped: the hit is served and then forgotten,
# because the process holding the cache is killed before it can write.
stats=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" stats 2>/dev/null | head -1)
echo "$stats" | grep -qE 'hits [1-9]' \
  && ok "the hit survives the process that served it" \
  || bad "statistics persist" "$stats"

note "the proxy, on hosts that have broken it before"
SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo-proxy" run >"$WORK/proxy.log" 2>&1 &
proxy_pid=$!
sleep 4
CA="$HOME_DIR/proxy/syndeo-ca.pem"
for host in https://www.google.com/ https://www.youtube.com/ https://github.com/; do
  code=$(curl -s --cacert "$CA" -x http://127.0.0.1:8899 -o /dev/null --max-time 30 -w '%{http_code}' "$host" 2>/dev/null)
  [ "$code" = "200" ] && ok "proxy serves $host" || bad "proxy $host" "HTTP ${code:-no answer}"
done
kill "$proxy_pid" 2>/dev/null

note "privacy defaults"
"$BIN/syndeo-net" --help 2>&1 | grep -q 'doh:cloudflare' \
  && ok "DNS resolves over HTTPS by default" || bad "DoH default" "not the default"
"$BIN/syndeo-net" --help 2>&1 | grep -q 'unpartitioned-cache' \
  && ok "the cache is partitioned, with a documented opt-out" || bad "partitioning" "no opt-out flag"

printf '\n  %d passed, %d failed\n\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
