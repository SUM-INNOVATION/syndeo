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
#   - the headline renderer, syndeo-webkit, was in the tarball and never
#     installed, while syndeo-servo was in the tarball and should not have been
#
# Knobs, both optional:
#
#   SYNDEO_EXPECT_SIGNED  yes | no (default: no). On macOS, whether the binaries
#                         must carry a Developer ID signature and pass
#                         Gatekeeper. The check fails when the release is not
#                         what this says, in either direction, so a release that
#                         is signed when it was meant not to be — or only half
#                         signed — is caught as surely as the opposite.
#   SYNDEO_INSTALLER      the install.sh to run: a URL or a local path (default:
#                         main's, from GitHub, as the README gives it). A path is
#                         how a change to the installer is checked before it
#                         reaches main.
#
# Exits non-zero if any of them fails, so it can gate a release rather than
# decorate one.
set -uo pipefail

VERSION="${1:-}"
REPO="SUM-INNOVATION/syndeo"
EXPECT_SIGNED="${SYNDEO_EXPECT_SIGNED:-no}"
INSTALLER="${SYNDEO_INSTALLER:-https://raw.githubusercontent.com/${REPO}/main/install.sh}"
CORE="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui"
# The version syndeo-webkit is installed from, on macOS. Kept the same as
# install.sh's WEBKIT_SINCE.
WEBKIT_SINCE="0.1.3"

case "$EXPECT_SIGNED" in
  yes | no) ;;
  *) echo "SYNDEO_EXPECT_SIGNED must be yes or no, not '$EXPECT_SIGNED'" >&2; exit 2 ;;
esac

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
BIN="$WORK/bin"
HOME_DIR="$WORK/home"

pass=0; fail=0
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s — %s\n' "$1" "$2"; fail=$((fail+1)); }
note() { printf '\n  %s\n' "$1"; }

# The same comparison install.sh makes: major.minor.patch as numbers, anything
# after `-` or `+` ignored, and anything malformed never at least anything.
version_at_least() {
  local have="${1%%[-+]*}" want="$2" rest
  case "$have" in
    *[!0-9.]* | .* | *. | *..*) return 1 ;;
  esac
  case "$have" in
    *.*.*.*) return 1 ;;
    *.*.*) ;;
    *) return 1 ;;
  esac
  local have_major="${have%%.*}"; rest="${have#*.}"
  local have_minor="${rest%%.*}" have_patch="${rest#*.}"
  local want_major="${want%%.*}"; rest="${want#*.}"
  local want_minor="${rest%%.*}" want_patch="${rest#*.}"
  if [ "$have_major" -ne "$want_major" ]; then [ "$have_major" -gt "$want_major" ]; return; fi
  if [ "$have_minor" -ne "$want_minor" ]; then [ "$have_minor" -gt "$want_minor" ]; return; fi
  [ "$have_patch" -ge "$want_patch" ]
}

note "installing from the published release, as a user would"
case "$INSTALLER" in
  http://* | https://*) install_command="curl -fsSL '$INSTALLER' | sh" ;;
  *) install_command="sh '$INSTALLER'" ;;
esac
if ! SYNDEO_INSTALL_DIR="$BIN" SYNDEO_HOME="$HOME_DIR" SYNDEO_VERSION="$VERSION" \
     sh -c "$install_command" >"$WORK/install.log" 2>&1; then
  bad "install.sh completes" "$(tail -3 "$WORK/install.log" | tr '\n' ' ')"
  printf '\n  %d passed, %d failed\n' "$pass" "$fail"; exit 1
fi
ok "install.sh completes, verifying the checksum"

# What the installer says it installed, which is what the rules below are for.
installed=$(sed -n 's/^Syndeo \([^ ]*\) for .*/\1/p' "$WORK/install.log" | head -1)
os="$(uname -s)"
expect_webkit=no
if [ "$os" = Darwin ] && version_at_least "$installed" "$WEBKIT_SINCE"; then
  expect_webkit=yes
fi

note "what it installed (${installed:-unknown version}, $os)"
missing=""
for b in $CORE; do
  [ -x "$BIN/$b" ] || missing="$missing $b"
done
if [ -z "$missing" ]; then
  ok "every core binary is present and executable"
else
  bad "binaries present" "missing:$missing"
fi

if [ -e "$BIN/syndeo-servo" ]; then
  bad "syndeo-servo is not installed" "it is at $BIN/syndeo-servo"
else
  ok "syndeo-servo is not installed"
fi

if [ "$expect_webkit" = yes ]; then
  if [ -x "$BIN/syndeo-webkit" ] && [ -x "$BIN/syndeo-proxy" ]; then
    ok "syndeo-webkit is installed beside syndeo-proxy"
  else
    bad "syndeo-webkit beside syndeo-proxy" "not both executable in $BIN"
  fi
  if "$BIN/syndeo-webkit" --help >/dev/null 2>&1; then
    ok "syndeo-webkit --help answers"
  else
    bad "syndeo-webkit --help" "exited non-zero"
  fi
elif [ -e "$BIN/syndeo-webkit" ]; then
  bad "syndeo-webkit is not installed" "this version and platform do not include it"
else
  ok "syndeo-webkit is not installed, as this version and platform expect"
fi

reported=$("$BIN/syndeo" --version 2>/dev/null | awk '{print $2}')
if [ -n "$VERSION" ]; then
  if [ "$reported" = "$VERSION" ]; then
    ok "reports version $VERSION"
  else
    bad "version" "reports ${reported:-nothing}"
  fi
elif [ -n "$reported" ]; then
  ok "reports version $reported"
else
  bad "version" "reports nothing"
fi

if [ "$os" = Darwin ]; then
  note "signing (expected: $([ "$EXPECT_SIGNED" = yes ] && echo "Developer ID signed and notarized" || echo "not Developer ID signed"))"
  signed=""; unsigned=""
  for b in $CORE syndeo-webkit; do
    [ -e "$BIN/$b" ] || continue
    if codesign -dv --verbose=2 "$BIN/$b" 2>&1 | grep -q '^Authority=Developer ID Application'; then
      signed="$signed $b"
    else
      unsigned="$unsigned $b"
    fi
  done
  if [ "$EXPECT_SIGNED" = yes ]; then
    if [ -z "$unsigned" ]; then
      ok "every binary carries a Developer ID signature"
    else
      bad "Developer ID signature" "missing on:$unsigned"
    fi
    # Every installed binary, not only the signed ones: an unsigned one is
    # exactly what Gatekeeper should be seen to refuse.
    rejected=""
    for b in $signed $unsigned; do
      spctl -a -t execute "$BIN/$b" >/dev/null 2>&1 || rejected="$rejected $b"
    done
    if [ -z "$rejected" ]; then
      ok "Gatekeeper accepts every binary"
    else
      bad "Gatekeeper" "rejects:$rejected"
    fi
  elif [ -z "$signed" ]; then
    ok "no binary carries a Developer ID signature, as expected for an unsigned release"
  else
    bad "unsigned release" "Developer ID signature found on:$signed (set SYNDEO_EXPECT_SIGNED=yes if that is intended)"
  fi
fi

note "the process model"
doctor=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" doctor 2>/dev/null)
if echo "$doctor" | grep -q 'NOT FOUND'; then
  bad "the shell finds its sibling processes" "$(echo "$doctor" | grep 'NOT FOUND' | tr '\n' ' ')"
else
  ok "the shell finds its sibling processes"
fi
if echo "$doctor" | grep -q 'keystore .*initialized'; then
  ok "the keystore starts and answers"
else
  bad "the keystore starts" "$(echo "$doctor" | grep -i keystore | head -1)"
fi

note "fetching, and the cache"
out=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" browse https://www.rust-lang.org/ --twice 2>/dev/null)
if echo "$out" | grep -qE '^  200'; then
  ok "fetches a page over the network"
else
  bad "fetch" "no 200 in the output"
fi
if echo "$out" | grep -q 'again   cache'; then
  ok "the second fetch is served from cache"
else
  bad "cache hit" "the second fetch was not a hit"
fi

# The regression that nearly shipped: the hit is served and then forgotten,
# because the process holding the cache is killed before it can write.
stats=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" stats 2>/dev/null | head -1)
if echo "$stats" | grep -qE 'hits [1-9]'; then
  ok "the hit survives the process that served it"
else
  bad "statistics persist" "$stats"
fi

note "the proxy, on hosts that have broken it before"
SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo-proxy" run >"$WORK/proxy.log" 2>&1 &
proxy_pid=$!
sleep 4
CA="$HOME_DIR/proxy/syndeo-ca.pem"
for host in https://www.google.com/ https://www.youtube.com/ https://github.com/; do
  code=$(curl -s --cacert "$CA" -x http://127.0.0.1:8899 -o /dev/null --max-time 30 -w '%{http_code}' "$host" 2>/dev/null)
  if [ "$code" = "200" ]; then
    ok "proxy serves $host"
  else
    bad "proxy $host" "HTTP ${code:-no answer}"
  fi
done
kill "$proxy_pid" 2>/dev/null
# Reaped here, so the shell does not report the kill it was asked for.
wait "$proxy_pid" 2>/dev/null

note "privacy defaults"
if "$BIN/syndeo-net" --help 2>&1 | grep -q 'doh:cloudflare'; then
  ok "DNS resolves over HTTPS by default"
else
  bad "DoH default" "not the default"
fi
if "$BIN/syndeo-net" --help 2>&1 | grep -q 'unpartitioned-cache'; then
  ok "the cache is partitioned, with a documented opt-out"
else
  bad "partitioning" "no opt-out flag"
fi

printf '\n  %d passed, %d failed\n\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
