#!/usr/bin/env bash
# Check the keystore's entitlements parse strictly, as committed and as
# ci/sign-macos.sh renders them for a team, before any release signs with
# them.
#
#   ci/check-entitlements.sh [--team TEAMID]
#   ci/check-entitlements.sh --self-test
#
# codesign reads entitlements with a strict XML parser, and refuses the
# whole signing step if they are not well-formed. plutil -lint is more
# lenient: it accepted an XML comment holding "--", which XML forbids, and the
# signed 0.1.6 dry run (38196649860) then failed in ci/sign-macos.sh when
# codesign reached syndeo-keystore. So both files are checked with:
# - Python's expat, everywhere;
# - xmllint --noout, where it is installed;
# - on macOS, codesign itself, signing a scratch copy of /usr/bin/true ad hoc
#   with them, which needs no identity and no secret;
# and their value has to be exactly the one keychain access group: the
# Xcode placeholder as committed, the team's prefix once rendered.
#
# --team picks the team to render for; by default a stand-in, AB12CD34EF.
# Nothing here reads a secret.
set -euo pipefail
LC_ALL=C
export LC_ALL

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE="$here/../crates/syndeo-keystore/Syndeo.entitlements"
GROUP='com.sum.syndeo.keystore'

die() {
  printf 'check-entitlements: %s\n' "$*" >&2
  exit 1
}

# render SOURCE TEAM OUT: what ci/sign-macos.sh signs with. The self-test
# requires ci/sign-macos.sh to render with this same expression.
render() {
  sed "s/\$(AppIdentifierPrefix)/${2}./" "$1" >"$3"
}

# check FILE GROUP-PREFIX LABEL: FILE parses strictly, and holds exactly
# keychain-access-groups = [GROUP-PREFIX + GROUP]. Prints what passed;
# returns 1, saying why, on the first failure.
check() {
  local file="$1" prefix="$2" label="$3" out
  if ! out="$(python3 -I - "$file" "$prefix$GROUP" <<'PY' 2>&1
import plistlib, sys, xml.parsers.expat
path, group = sys.argv[1:3]
data = open(path, "rb").read()
try:
    xml.parsers.expat.ParserCreate().Parse(data, True)
except xml.parsers.expat.ExpatError as e:
    sys.exit("not well-formed XML (expat): %s" % e)
value = plistlib.loads(data)
if value != {"keychain-access-groups": [group]}:
    sys.exit("holds %r, not exactly keychain-access-groups = [%r]" % (value, group))
PY
  )"; then
    printf '  %s: %s\n' "$label" "$out" >&2
    return 1
  fi
  local ran="expat"
  if command -v xmllint >/dev/null 2>&1; then
    if ! out="$(xmllint --noout "$file" 2>&1)"; then
      printf '  %s: xmllint: %s\n' "$label" "$(printf '%s' "$out" | head -n 1)" >&2
      return 1
    fi
    ran="$ran, xmllint"
  fi
  if [ "$(uname -s)" = Darwin ]; then
    if ! out="$(plutil -lint "$file" 2>&1)"; then
      printf '  %s: plutil: %s\n' "$label" "$out" >&2
      return 1
    fi
    local probe
    probe="$(mktemp -d)"
    cp /usr/bin/true "$probe/probe"
    if ! out="$(codesign --force --sign - --entitlements "$file" "$probe/probe" 2>&1)"; then
      rm -rf "$probe"
      printf '  %s: codesign: %s\n' "$label" "$(printf '%s' "$out" | grep -v '^$' | head -n 1)" >&2
      return 1
    fi
    rm -rf "$probe"
    ran="$ran, plutil, codesign"
  fi
  printf '  ok    %s (%s)\n' "$label" "$ran"
}

# check_both SOURCE TEAM: the committed file, and its rendering for TEAM.
check_both() {
  local src="$1" team="$2" work status=0
  [[ "$team" =~ ^[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10}$ ]] || die "the team has to be ten capital letters and digits"
  work="$(mktemp -d)"
  check "$src" '$(AppIdentifierPrefix)' "as committed" || status=1
  render "$src" "$team" "$work/rendered.entitlements"
  check "$work/rendered.entitlements" "$team." "rendered for $team" || status=1
  rm -rf "$work"
  return "$status"
}

self_test() {
  local T cases=0 wrong=0
  T="$(mktemp -d)"
  # case LABEL WANT FILE: check_both on FILE, for the stand-in team, returns
  # WANT (0 or 1).
  case_() {
    local label="$1" want="$2" file="$3" got=0
    check_both "$file" AB12CD34EF >"$T/out" 2>&1 || got=1
    cases=$((cases + 1))
    if [ "$got" = "$want" ]; then
      printf '  ok    %s\n' "$label"
    else
      printf '  WRONG %s (returned %s)\n' "$label" "$got"
      sed 's/^/          | /' "$T/out"
      wrong=$((wrong + 1))
    fi
  }
  # variant NAME OLD NEW: the committed file with OLD replaced by NEW.
  variant() {
    python3 -I - "$SOURCE" "$T/$1.entitlements" "$2" "$3" <<'PY'
import sys
src, dst, old, new = sys.argv[1:5]
s = open(src).read()
if s.count(old) != 1:
    sys.exit("self-test: %r is not in the entitlements exactly once" % old)
open(dst, "w").write(s.replace(old, new))
PY
  }
  printf '\n  the keystore entitlements, as committed and as rendered\n'
  case_ "the committed file, and its rendering, pass" 0 "$SOURCE"
  variant dashes "ad-hoc signing with this" '`codesign --sign -` with this'
  case_ "\"--\" inside the comment, as 0.1.6 had it: refused" 1 "$T/dashes.entitlements"
  variant dash-end "  -->" "  --->"
  case_ "a comment ending in \"--->\": refused" 1 "$T/dash-end.entitlements"
  variant unclosed "</array>" "<array>"
  case_ "an unclosed element: refused" 1 "$T/unclosed.entitlements"
  variant second-group "<string>\$(AppIdentifierPrefix)com.sum.syndeo.keystore</string>" \
    "<string>\$(AppIdentifierPrefix)com.sum.syndeo.keystore</string><string>\$(AppIdentifierPrefix)com.sum.other</string>"
  case_ "a second access group: refused" 1 "$T/second-group.entitlements"
  variant no-prefix "\$(AppIdentifierPrefix)com.sum.syndeo.keystore" "com.sum.syndeo.keystore"
  case_ "the group without the team placeholder: refused" 1 "$T/no-prefix.entitlements"
  variant extra "<key>keychain-access-groups</key>" "<key>com.apple.security.get-task-allow</key><true/><key>keychain-access-groups</key>"
  case_ "one entitlement more: refused" 1 "$T/extra.entitlements"

  local out status
  out="$(bash "$0" --team ab12cd34ef 2>&1)" && status=0 || status=$?
  cases=$((cases + 1))
  if [ "$status" = 1 ] && printf '%s' "$out" | grep -qF "ten capital letters and digits"; then
    printf '  ok    %s\n' "a malformed team: refused"
  else
    printf '  WRONG %s (exit %s)\n' "a malformed team: refused" "$status"
    wrong=$((wrong + 1))
  fi
  cases=$((cases + 1))
  # The rendering here is the one release signing uses.
  if grep -qF 'sed "s/\$(AppIdentifierPrefix)/${TEAM_ID}./" \' "$here/sign-macos.sh" &&
    grep -qF '"$root/crates/syndeo-keystore/Syndeo.entitlements" > "$entitlements"' "$here/sign-macos.sh"; then
    printf '  ok    %s\n' "ci/sign-macos.sh renders the same file with the same expression"
  else
    printf '  WRONG %s\n' "ci/sign-macos.sh renders the same file with the same expression"
    wrong=$((wrong + 1))
  fi
  rm -rf "$T"
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test ;;
  --team)
    [ "$#" = 2 ] || die "usage: $0 [--team TEAMID] | --self-test"
    check_both "$SOURCE" "$2"
    ;;
  '') check_both "$SOURCE" AB12CD34EF ;;
  *) die "usage: $0 [--team TEAMID] | --self-test" ;;
esac
