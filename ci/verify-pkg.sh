#!/usr/bin/env bash
# Check Syndeo's macOS installer package, before and after it is installed.
#
#   ci/verify-pkg.sh inspect <pkg> <version> --tarball <tarball>
#   ci/verify-pkg.sh installed <version> [--commands-say <version>]
#   ci/verify-pkg.sh --self-test [decisions]
#
# inspect looks inside a package without installing it:
# - the name;
# - what the product and the component hold;
# - the product's limits (Apple Silicon, macOS 13, the startup disk only,
#   nothing to customize, no JavaScript);
# - the component's identity, its scripts, byte for byte against the
#   templates, and overwrite-permissions="false";
# - the BOM, path by path with modes, owners and link targets;
# - the payload's files against the release tarball;
# - every command's --version;
# - the signing state.
#
# installed checks what an installation left on this Mac, with the same checks
# the package's own scripts make: the receipt, the private directory, the
# version tree, `current`, the seven command links, and each command's
# --version as started from /usr/local/bin. --commands-say is for a stand-in
# package made of other binaries, which report their own version.
#
# --self-test checks the package's three root scripts, and this script's own
# judgement, against stand-ins in temporary directories, as an ordinary user.
# The scripts are macOS root scripts, written for BSD stat, ls and mv, so
# their behaviour is checked on macOS only. On Linux the version grammar, the
# comparator and the rendering of the scripts are checked. --self-test
# decisions checks only what the scripts say in each real-runner scenario.
#
# Knobs, for inspect:
#   SYNDEO_EXPECT_SIGNED  no (default, and what empty means): the package
#                         carries no signature and Gatekeeper rejects it, and no
#                         executable carries a Developer ID. yes: every
#                         executable is Developer ID Application of
#                         MACOS_TEAM_ID, with the hardened runtime and a secure
#                         timestamp, and only syndeo-keystore has entitlements,
#                         exactly its keychain access group; the package is
#                         Developer ID Installer of that team, chained to Apple
#                         Root CA, its notarization ticket stapled, and
#                         Gatekeeper accepts it as Notarized Developer ID.
#   MACOS_TEAM_ID         with yes, the team every signature has to be of.
#
# Exits non-zero if any check fails.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
templates="$here/macos-pkg"
builder="$here/package-macos-pkg.sh"
NAMES="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit"

# shellcheck source=ci/macos-pkg/version.sh
. "$templates/version.sh"

pass=0; fail=0; failures=""
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; pass=$((pass+1)); }
bad()  {
  printf '  \033[31mFAIL\033[0m  %s — %s\n' "$1" "$2"
  fail=$((fail+1))
  failures="${failures}${1} — ${2}"$'\n'
}
note() { printf '\n  %s\n' "$1"; }
report() { printf '\n  %d passed, %d failed\n\n' "$pass" "$fail"; }

# expected_bom V: every line `lsbom -p fmugl` prints for the package of V.
expected_bom() {
  local v="$1" n
  printf '%s\t40755\t0\t0\t\n' . ./usr ./usr/local ./usr/local/bin ./usr/local/libexec \
    ./usr/local/libexec/syndeo "./usr/local/libexec/syndeo/$v" "./usr/local/libexec/syndeo/$v/tools"
  for n in $NAMES; do
    printf '%s\t120755\t0\t0\t%s\n' "./usr/local/bin/$n" "../libexec/syndeo/current/$n"
    printf '%s\t100755\t0\t0\t\n' "./usr/local/libexec/syndeo/$v/$n"
  done
  printf '%s\t100755\t0\t0\t\n' "./usr/local/libexec/syndeo/$v/uninstall.sh"
  printf '%s\t100644\t0\t0\t\n' "./usr/local/libexec/syndeo/$v/README.md" \
    "./usr/local/libexec/syndeo/$v/LICENSE" "./usr/local/libexec/syndeo/$v/tools/wordcount.wat"
}

# xp FILE XPATH: an XPath value from FILE, or nothing.
xp() { xmllint --xpath "$2" "$1" 2>/dev/null; }

# ------------------------------------------------------------------ inspect

inspect() {
  local pkg="$1" version="$2" tarball="$3"
  local expect="${SYNDEO_EXPECT_SIGNED:-no}" team='' name w full dist info scripts payload tree f n out
  case "$expect" in
    no) ;;
    yes)
      team="${MACOS_TEAM_ID:-}"
      [[ "$team" =~ $TEAM_RE ]] || { echo "SYNDEO_EXPECT_SIGNED=yes needs MACOS_TEAM_ID, ten capital letters and digits" >&2; return 2; }
      ;;
    *) echo "SYNDEO_EXPECT_SIGNED must be yes or no, not '$expect'" >&2; return 2 ;;
  esac
  name="syndeo-$version-aarch64-apple-darwin"
  w="$inspect_work"

  note "the package, by name and shape"
  if syndeo_version_valid "$version" && [ "${pkg##*/}" = "$name.pkg" ]; then
    ok "named $name.pkg"
  else
    bad "name" "${pkg##*/} is not $name.pkg for a valid version"
  fi
  full="$w/full"
  if ! pkgutil --expand-full "$pkg" "$full" >/dev/null 2>&1; then
    bad "expand" "pkgutil could not expand $pkg"
    return 1
  fi
  if [ "$(cd "$full" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "Distribution syndeo.pkg " ]; then
    ok "the product holds a Distribution and one component, syndeo.pkg"
  else
    bad "product" "it holds: $(cd "$full" && ls -A | tr '\n' ' ')"
  fi
  if [ "$(cd "$full/syndeo.pkg" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "Bom PackageInfo Payload Scripts " ]; then
    ok "the component holds a Bom, PackageInfo, Payload and Scripts"
  else
    bad "component" "it holds: $(cd "$full/syndeo.pkg" && ls -A | tr '\n' ' ')"
  fi

  note "the product's limits"
  dist="$full/Distribution"
  local want got problems=""
  for want in \
    "string(/installer-gui-script/options/@customize)=never" \
    "string(/installer-gui-script/options/@require-scripts)=false" \
    "string(/installer-gui-script/options/@hostArchitectures)=arm64" \
    "string(/installer-gui-script/options/@rootVolumeOnly)=true" \
    "string(/installer-gui-script/domains/@enable_localSystem)=true" \
    "string(/installer-gui-script/domains/@enable_anywhere)=false" \
    "string(/installer-gui-script/domains/@enable_currentUserHome)=false" \
    "string(/installer-gui-script/volume-check/allowed-os-versions/os-version/@min)=13.0" \
    "string(/installer-gui-script/product/@id)=com.sum.syndeo" \
    "string(/installer-gui-script/product/@version)=$version" \
    "string(/installer-gui-script/pkg-ref[@version]/@version)=$version" \
    "count(/installer-gui-script/pkg-ref[@id!='com.sum.syndeo.pkg'])=0" \
    "count(//script)=0" \
    "count(//installation-check)=0" \
    "count(//@script)=0"; do
    got="$(xp "$dist" "${want%=*}")"
    [ "$got" = "${want##*=}" ] || problems="$problems ${want%=*} is '$got';"
  done
  if [ -z "$problems" ]; then
    ok "Apple Silicon, macOS 13 or later, the startup disk only, nothing to customize, no JavaScript"
  else
    bad "Distribution" "$problems"
  fi

  note "the component"
  info="$full/syndeo.pkg/PackageInfo"
  problems=""
  for want in \
    "string(/pkg-info/@identifier)=com.sum.syndeo.pkg" \
    "string(/pkg-info/@version)=$version" \
    "string(/pkg-info/@install-location)=/" \
    "string(/pkg-info/@overwrite-permissions)=false" \
    "string(/pkg-info/@auth)=root" \
    "count(/pkg-info/scripts/*)=2" \
    "string(/pkg-info/scripts/preinstall/@file)=./preinstall" \
    "string(/pkg-info/scripts/postinstall/@file)=./postinstall" \
    "count(/pkg-info/bundle)=0" \
    "count(/pkg-info/bundle-version/*)=0"; do
    got="$(xp "$info" "${want%=*}")"
    [ "$got" = "${want##*=}" ] || problems="$problems ${want%=*} is '$got';"
  done
  if [ -z "$problems" ]; then
    ok "com.sum.syndeo.pkg $version, at /, overwrite-permissions=\"false\", a preinstall and a postinstall, no bundles"
  else
    bad "PackageInfo" "$problems"
  fi

  scripts="$full/syndeo.pkg/Scripts"
  mkdir "$w/expected"
  "$builder" --render preinstall "$version" '' 0 0 /usr/sbin/pkgutil "$w/expected/preinstall" >/dev/null
  "$builder" --render postinstall "$version" '' 0 0 /usr/sbin/pkgutil "$w/expected/postinstall" >/dev/null
  "$builder" --render uninstall "$version" '' 0 0 /usr/sbin/pkgutil "$w/expected/uninstall.sh" >/dev/null
  if [ "$(cd "$scripts" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "postinstall preinstall " ]; then
    ok "exactly two scripts, preinstall and postinstall"
  else
    bad "scripts" "found: $(cd "$scripts" && ls -A | tr '\n' ' ')"
  fi
  for f in preinstall postinstall; do
    if cmp -s "$scripts/$f" "$w/expected/$f" && [ "$(stat -f %Lp "$scripts/$f")" = 755 ]; then
      ok "$f is the template with this version, root '', uid 0, gid 0 and /usr/sbin/pkgutil, mode 0755"
    else
      bad "$f" "differs from the template rendered for $version, or is not mode 0755"
    fi
  done

  note "the BOM: every path, mode, owner and link"
  expected_bom "$version" | LC_ALL=C sort >"$w/bom.want"
  lsbom -p fmugl "$full/syndeo.pkg/Bom" | LC_ALL=C sort >"$w/bom.have"
  if cmp -s "$w/bom.want" "$w/bom.have"; then
    ok "exactly the version tree, uninstall.sh and seven links to ../libexec/syndeo/current, all root:wheel; no current, nothing writable beyond its owner, no special bits"
  else
    bad "BOM" "$(diff "$w/bom.want" "$w/bom.have" | grep '^[<>]' | head -8 | tr '\n' ' ')"
  fi

  note "the payload, against the release tarball"
  payload="$full/syndeo.pkg/Payload"
  tree="$payload/usr/local/libexec/syndeo/$version"
  mkdir "$w/tarball"
  if ! tar -xzf "$tarball" -C "$w/tarball" 2>/dev/null; then
    bad "tarball" "could not unpack $tarball"
  else
    problems=""
    for f in $NAMES README.md LICENSE tools/wordcount.wat; do
      cmp -s "$tree/$f" "$w/tarball/$name/$f" || problems="$problems $f"
    done
    if [ -z "$problems" ]; then
      ok "the seven executables, README.md, LICENSE and tools/wordcount.wat are byte-identical to the tarball's"
    else
      bad "payload" "differs from the tarball:$problems"
    fi
  fi
  if cmp -s "$tree/uninstall.sh" "$w/expected/uninstall.sh"; then
    ok "uninstall.sh is the template rendered for $version"
  else
    bad "uninstall.sh" "differs from the template rendered for $version"
  fi
  if [ "$(cd "$payload" && find . -name '*servo*' | wc -l | tr -d ' ')" = 0 ]; then
    ok "nothing of syndeo-servo"
  else
    bad "syndeo-servo" "is in the payload"
  fi

  note "what each command says it is"
  problems=""
  for n in $NAMES; do
    out="$("$tree/$n" --version 2>&1 | head -n 1)"
    [ "$out" = "$n $version" ] || problems="$problems $n says '$out';"
  done
  if [ -z "$problems" ]; then
    ok "all seven report $version"
  else
    bad "versions" "$problems"
  fi

  if [ "$expect" = yes ]; then
    signed_state "$pkg" "$tree" "$team"
    return 0
  fi
  note "signing (expected: none)"
  # Captured first: pkgutil exits non-zero for an unsigned package, which
  # pipefail would otherwise report as the check failing.
  local signature
  signature="$(pkgutil --check-signature "$pkg" 2>&1)"
  if printf '%s\n' "$signature" | grep -q 'Status: no signature'; then
    ok "the package carries no signature"
  else
    bad "package signature" "$(printf '%s\n' "$signature" | sed -n 2p)"
  fi
  if spctl -a -vv -t install "$pkg" >"$w/spctl" 2>&1; then
    bad "Gatekeeper" "accepts the unsigned package: $(tr '\n' ' ' <"$w/spctl")"
  else
    ok "Gatekeeper rejects the package: $(sed -n 's/.*: //p' "$w/spctl" | head -n 1 | tr -d '\n')"
  fi
  problems=""
  local adhoc=""
  for n in $NAMES; do
    out="$(codesign -dv --verbose=2 "$tree/$n" 2>&1)"
    if printf '%s\n' "$out" | grep -q '^Authority=Developer ID'; then
      problems="$problems $n"
    elif printf '%s\n' "$out" | grep -q '^Signature=adhoc'; then
      adhoc="$adhoc $n"
    fi
  done
  if [ -z "$problems" ]; then
    ok "no executable carries a Developer ID; ad-hoc signed:${adhoc:- none}"
  else
    bad "executables" "a Developer ID signature on:$problems"
  fi
}

# A team identifier: ten capital letters and digits, whatever the locale.
TEAM_RE='^[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10}$'

# entitlements_are XML GROUP: XML, what codesign printed as the entitlements,
# is exactly keychain-access-groups = [GROUP], or, with GROUP empty, nothing.
entitlements_are() {
  python3 -I -c '
import plistlib, sys
data, group = sys.argv[1], sys.argv[2]
have = plistlib.loads(data.encode()) if data.strip() else {}
want = {"keychain-access-groups": [group]} if group else {}
sys.exit(0 if have == want else 1)
' "$1" "$2" 2>/dev/null
}

# developer_id KIND VALUE TEAM: VALUE is "Developer ID KIND: <name> (TEAM)".
developer_id() {
  [[ "$2" =~ ^Developer\ ID\ $1:\ .+\ \(([ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10})\)$ ]] && [ "${BASH_REMATCH[1]}" = "$3" ]
}

# signed_state PKG TREE TEAM: what a signed release has to be, checked on the
# package and on the executables in its payload, which inspect has already
# found byte-identical to the tarball's.
signed_state() {
  local pkg="$1" tree="$2" team="$3" n out first last group entitled problems chain
  note "signing (expected: Developer ID of $team, notarized and stapled)"
  for n in $NAMES; do
    problems=""
    codesign --verify --strict "$tree/$n" >/dev/null 2>&1 || problems="$problems does not verify;"
    out="$(codesign -dv --verbose=4 "$tree/$n" 2>&1)"
    first="$(printf '%s\n' "$out" | sed -n 's/^Authority=//p' | head -n 1)"
    last="$(printf '%s\n' "$out" | sed -n 's/^Authority=//p' | tail -n 1)"
    developer_id Application "$first" "$team" || problems="$problems signed by '${first:-nobody}';"
    [ "$last" = "Apple Root CA" ] || problems="$problems not chained to Apple Root CA;"
    printf '%s\n' "$out" | grep -qx "TeamIdentifier=$team" ||
      problems="$problems TeamIdentifier '$(printf '%s\n' "$out" | sed -n 's/^TeamIdentifier=//p')';"
    printf '%s\n' "$out" | grep -qE '^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*runtime[^)]*\)' || problems="$problems no hardened runtime;"
    printf '%s\n' "$out" | grep -q '^Timestamp=' || problems="$problems no secure timestamp;"
    group=''
    entitled="no entitlements"
    if [ "$n" = syndeo-keystore ]; then
      group="$team.com.sum.syndeo.keystore"
      entitled="keychain-access-groups [$group] and nothing else"
    fi
    entitlements_are "$(codesign -d --entitlements - --xml "$tree/$n" 2>/dev/null)" "$group" ||
      problems="$problems entitlements are not: $entitled;"
    if [ -z "$problems" ]; then
      ok "$n: Developer ID Application of $team, hardened runtime, secure timestamp, $entitled"
    else
      bad "$n" "${problems# }"
    fi
  done

  # Captured whole: pkgutil exits non-zero for a package it does not trust.
  out="$(pkgutil --check-signature "$pkg" 2>&1 | sed 's/^[[:space:]]*//')"
  chain="$(printf '%s\n' "$out" | sed -n 's/^[0-9][0-9]*\. //p')"
  first="$(printf '%s\n' "$chain" | head -n 1)"
  last="$(printf '%s\n' "$chain" | tail -n 1)"
  problems=""
  printf '%s\n' "$out" | grep -qx 'Status: signed by a developer certificate issued by Apple for distribution' ||
    problems="$problems $(printf '%s\n' "$out" | grep -m1 '^Status:' || echo 'no status');"
  developer_id Installer "$first" "$team" || problems="$problems signed by '${first:-nobody}';"
  [ "$last" = "Apple Root CA" ] || problems="$problems chained to '${last:-nothing}', not Apple Root CA;"
  printf '%s\n' "$out" | grep -q '^Signed with a trusted timestamp' || problems="$problems no trusted timestamp;"
  if [ -z "$problems" ]; then
    ok "the package: Developer ID Installer of $team, chained to Apple Root CA, with a trusted timestamp"
  else
    bad "package signature" "${problems# }"
  fi
  if xcrun stapler validate "$pkg" >/dev/null 2>&1; then
    ok "its notarization ticket is stapled, and validates"
  else
    bad "notarization ticket" "stapler validate fails"
  fi
  out="$(spctl -a -vv -t install "$pkg" 2>&1)" && problems='' || problems=" rejected;"
  printf '%s\n' "$out" | grep -qxF "$pkg: accepted" || problems="${problems:- not accepted;}"
  printf '%s\n' "$out" | grep -qx 'source=Notarized Developer ID' || problems="$problems $(printf '%s\n' "$out" | grep -m1 '^source=' || echo 'no source');"
  printf '%s\n' "$out" | grep -qxF "origin=$first" || problems="$problems from another origin;"
  if [ -z "$problems" ]; then
    ok "Gatekeeper accepts it: Notarized Developer ID, from the same Installer identity"
  else
    bad "Gatekeeper" "${problems# }"
  fi
}

# ------------------------------------------------------------------ installed

installed() {
  local version="$1" says="${2:-$1}" n out
  # The same checks the package's scripts make, from the same file. The
  # SYNDEO_VERIFY_* knobs exist for the self-test, which installs nothing.
  ROOT="${SYNDEO_VERIFY_ROOT:-}"
  EXPECT_UID="${SYNDEO_VERIFY_UID:-0}"
  EXPECT_GID="${SYNDEO_VERIFY_GID:-0}"
  PKGUTIL="${SYNDEO_VERIFY_PKGUTIL:-/usr/sbin/pkgutil}"
  SCRIPT=verify
  # shellcheck source=ci/macos-pkg/common.sh
  . "$templates/common.sh"

  note "the receipt"
  read_receipt
  if [ "$r_present" != yes ]; then
    bad "receipt" "there is no receipt for $SYNDEO_ID"
    return 0
  fi
  check_receipt_meta
  if [ "$r_version" = "$version" ] && [ "$nfindings" = 0 ]; then
    ok "$SYNDEO_ID $version, for / at /, listing exactly its paths, and no other package claims them"
  else
    bad "receipt" "version '$r_version';$(printf '%s' "$findings" | tr '\n' ' ')"
  fi

  note "the files"
  findings=''; nfindings=0
  check_strict_dir "$SYNDEO_LOCAL"
  check_strict_dir "$SYNDEO_LIBEXEC"
  check_bin_dir "$SYNDEO_BIN"
  scan_private
  scan_bin
  # Every path the receipt lists, there and exactly as packaged.
  [ -n "$r_files" ] && check_receipt_installed "$r_version"
  [ "$versions" = " $version" ] || finding "$SYNDEO_DIR: holds${versions:- no version}, not $version alone"
  [ "$current_v" = "$version" ] || finding "$SYNDEO_DIR/current: points at '$current_v', not $version"
  check_tree "$version" exact
  [ "$bin_present" = 7 ] || finding "$SYNDEO_BIN: has $bin_present of the seven commands"
  for n in $bin_foreign; do
    finding "$SYNDEO_BIN/$n: is not a link the package writes"
  done
  if [ "$nfindings" = 0 ]; then
    ok "$SYNDEO_DIR/$version alone, every path the receipt lists exactly as packaged, current points at it, and all seven commands link through current"
  else
    bad "installation" "$(printf '%s' "$findings" | tr '\n' ' ')"
  fi

  note "the commands, started from $SYNDEO_BIN"
  local problems=""
  for n in $NAMES; do
    out="$(env -i PATH=/usr/bin:/bin HOME="${HOME:-/}" "$SYNDEO_BIN/$n" --version 2>&1 | head -n 1)"
    [ "$out" = "$n $says" ] || problems="$problems $n says '$out';"
  done
  if [ -z "$problems" ]; then
    ok "all seven report $says"
  else
    bad "versions" "$problems"
  fi
}

# ------------------------------------------------------------------ self-test

cases=0; wrong=0

# expect NAME COMMAND...: one self-test case, passing when COMMAND does.
expect() {
  local name="$1"; shift
  cases=$((cases+1))
  if "$@"; then
    printf '  ok    %s\n' "$name"
  else
    printf '  WRONG %s\n' "$name"
    wrong=$((wrong+1))
  fi
}

st_versions() {
  local v r
  printf '\n  the version grammar\n'
  for v in 0.0.0 0.1.5 1.2.3 10.20.30 0.1.18446744073709551616; do
    expect "valid: $v" syndeo_version_valid "$v"
  done
  for v in '' 1.2 1.2.3.4 01.2.3 1.02.3 1.2.03 -1.2.3 +1.2.3 v1.2.3 ' 1.2.3' '1.2.3 ' 1.2.3-rc1 1.2.x 1..3 \
    $'1.2.3\n4.5.6' $'1.2.3\n' '1.2.3/' '1.2.3*'; do
    expect "invalid: $(printf '%q' "$v")" eval '! syndeo_version_valid "$v"'
  done

  printf '\n  the comparator\n'
  while read -r a want b; do
    r="$(syndeo_version_cmp "$a" "$b")"
    expect "$a $want $b" test "$r" = "$want"
  done <<'EOF'
0.1.9 lt 0.1.10
0.1.10 gt 0.1.9
0.2.0 gt 0.1.99
1.0.0 gt 0.999.999
10.0.0 gt 9.9.9
0.1.5 eq 0.1.5
0.0.1 lt 0.0.2
0.1.18446744073709551616 gt 0.1.18446744073709551615
0.1.99999999999999999999 gt 0.1.18446744073709551616
2.0.0 gt 1.999999999999.999999999999
0.10.0 gt 0.9.99
EOF
}

st_render() {
  local d="$1" k
  printf '\n  rendering the scripts\n'
  for k in preinstall postinstall uninstall; do
    "$builder" --render "$k" 0.1.7 '' 0 0 /usr/sbin/pkgutil "$d/$k" >/dev/null 2>&1
    expect "$k renders" test -s "$d/$k"
    expect "$k: no placeholder left" eval "! grep -q '@[A-Z_]*@' '$d/$k'"
    expect "$k: root '', uid 0, gid 0, /usr/sbin/pkgutil" eval "grep -qx \"ROOT=''\" '$d/$k' && grep -qx \"EXPECT_UID='0'\" '$d/$k' && grep -qx \"EXPECT_GID='0'\" '$d/$k' && grep -qx \"PKGUTIL='/usr/sbin/pkgutil'\" '$d/$k'"
    expect "$k: the comparator and the shared checks, once each" eval "[ \"\$(grep -c '^syndeo_version_cmp() {' '$d/$k')\" = 1 ] && [ \"\$(grep -c '^check_tree() {' '$d/$k')\" = 1 ]"
    expect "$k: POSIX sh parses it" sh -n "$d/$k"
  done
  expect "preinstall carries the incoming version" grep -qx "INCOMING='0.1.7'" "$d/preinstall"
  expect "uninstall.sh carries its own version" grep -qx "EMBEDDED='0.1.7'" "$d/uninstall"
  expect "the builder has no test fault to put in a package" eval "! '$builder' --render postinstall 0.0.3 '' 0 0 /usr/sbin/pkgutil '$d/x' --test-fault postinstall-fails >/dev/null 2>&1"
  expect "a root with a quote is refused" eval "! '$builder' --render preinstall 0.1.7 \"/tmp/it's\" 0 0 /usr/sbin/pkgutil '$d/x' >/dev/null 2>&1"
  expect "a relative pkgutil is refused" eval "! '$builder' --render preinstall 0.1.7 '' 0 0 pkgutil '$d/x' >/dev/null 2>&1"
  expect "a malformed version is refused" eval "! '$builder' --render preinstall 0.1 '' 0 0 /usr/sbin/pkgutil '$d/x' >/dev/null 2>&1"
}

# st_said_run DUMP-LINES INSTALL-LOG-LINES TEXT [TRIES [LATE-LINE]]:
# note_said() from ci/test-pkg-install.sh, on an installer dump and an
# install.log made here. Prints its status, what it printed, and what it
# appended to LOG.said, one per line.
st_said_run() {
  local d
  # Its own directory each time: this runs in a command substitution, where
  # no counter would carry over.
  d="$(mktemp -d "$T/said.XXXXXX")"
  printf '%s' "$1" >"$d/7-x.installer"
  printf '%s' "$2" >"$d/install.log"
  if [ -n "${5:-}" ]; then
    ( sleep 1; printf '%s\n' "$5" >>"$d/install.log" ) &
  fi
  bash -c '
    . "$1" --source-only
    INSTALL_LOG="$2"
    SAID_TRIES="$4"
    last_log="$3"
    last_before=0
    where="$(note_said "$5")"
    echo "status $?"
    echo "$where"
    cat "$3.said"
  ' _ "$here/test-pkg-install.sh" "$d/install.log" "$d/7-x" "${4:-2}" "$3"
  wait
}

# What ci/test-pkg-install.sh does with what a package's scripts say, which
# otherwise only a real install exercises. Every installer it, or
# upgrade-stress.py, runs is asked for -dumplog, whose output is kept per
# invocation. note_said() looks there first, for the line however installer
# prefixes it, then in what /var/log/install.log gains, allowing for it to be
# late; and it decides nothing: a line seen nowhere is recorded as such, and
# the run goes on, judged on what is installed.
st_harness() {
  printf '\n  the install test: what a package said, noted\n'
  local runs out ok d
  runs="$(grep -nE '(^|[^-[:alnum:]_./])(/usr/sbin/)?installer +-' "$here/test-pkg-install.sh" | grep -vE '^[0-9]+: *#')"
  ok=no
  if [ -n "$runs" ] && ! printf '%s\n' "$runs" | grep -qv -- ' -dumplog '; then ok=yes; fi
  expect "test-pkg-install.sh runs installer, and always with -dumplog" test "$ok" = yes
  runs="$(grep -n 'a\.installer,' "$here/upgrade-stress.py")"
  ok=no
  if [ -n "$runs" ] && ! printf '%s\n' "$runs" | grep -qv -- '"-dumplog"'; then ok=yes; fi
  expect "upgrade-stress.py runs installer, and always with -dumplog" test "$ok" = yes
  ok=no
  if ! grep -nE '(^|[^_[:alnum:]])said "' "$here/test-pkg-install.sh" | grep -qv 'note_said'; then ok=yes; fi
  expect "test-pkg-install.sh decides nothing on a logged line: only note_said remains" test "$ok" = yes

  local decision='syndeo preinstall: upgrading from 0.0.13 to 0.0.15'
  local prefixed="2026-10-10 00:53:34+00 runner package_script_service[42591]: ./preinstall: $decision"

  out="$(st_said_run "installer: Package name is Syndeo 0.0.15
$prefixed
installer: The upgrade was successful.
" "" "$decision")"
  ok=no
  if [ "$(printf '%s\n' "$out" | sed -n 2p)" = "seen in the -dumplog output" ] &&
    printf '%s\n' "$out" | grep -qxF "seen in the -dumplog output: $decision"; then ok=yes; fi
  expect "note_said() sees a prefixed line in the invocation's -dumplog output, with nothing in install.log" test "$ok" = yes

  out="$(st_said_run "installer: The upgrade was successful.
" "$prefixed
" "$decision")"
  ok=no
  if [ "$(printf '%s\n' "$out" | sed -n 2p)" = "seen in /var/log/install.log, on read 1" ]; then ok=yes; fi
  expect "note_said() falls back to what install.log gained" test "$ok" = yes

  out="$(st_said_run "installer: The upgrade was successful.
" "" "$decision" 10 "$prefixed")"
  ok=no
  case "$(printf '%s\n' "$out" | sed -n 2p)" in
    "seen in /var/log/install.log, on read 1") ;;
    "seen in /var/log/install.log, on read "*) ok=yes ;;
  esac
  expect "note_said() waits a while for a line install.log delivers late" test "$ok" = yes

  out="$(st_said_run "installer: Package name is Syndeo 0.0.15
./preinstall: syndeo preinstall: upgrading from 0.0.12 to 0.0.15
" "" "$decision")"
  ok=no
  if [ "$(printf '%s\n' "$out" | sed -n 1p)" = "status 0" ] && [ "$(printf '%s\n' "$out" | sed -n 2p)" = "not seen" ] &&
    printf '%s\n' "$out" | grep -qxF "not seen: $decision"; then ok=yes; fi
  expect "note_said() records a line seen nowhere, and does not fail" test "$ok" = yes

  d="$T/said-delta"
  mkdir -p "$d"
  printf '%s\n' 'before 1' 'before 2' "$prefixed" 'other line' >"$d/install.log"
  bash -c '
    . "$1" --source-only
    INSTALL_LOG="$2"
    last_log="$3"
    last_before=2
    log_lines
  ' _ "$here/test-pkg-install.sh" "$d/install.log" "$d/7-x"
  ok=no
  if [ "$(cat "$d/7-x.install-log")" = "$(printf '%s\n' "$prefixed" 'other line')" ] &&
    [ "$(cat "$d/7-x.install-log.filtered")" = "$prefixed" ]; then ok=yes; fi
  expect "log_lines keeps all install.log gained, whole, and filters only a copy" test "$ok" = yes
}

# --- macOS: the root scripts, under a temporary root ------------------------

# st_new: a fresh case. C is its directory; R the root, with a space in it,
# holding usr/local; DB the stand-in receipts; U and G the expected owner.
st_new() {
  C="$T/case-$((++st_n))"
  R="$C/root with space"
  DB="$C/receipts"
  D="$R/usr/local/libexec/syndeo"
  mkdir -p "$R/usr/local" "$DB"
  chmod 755 "$R" "$R/usr" "$R/usr/local"
  U="$(id -u)"
  G="$(stat -f %g "$R")"
  cat >"$C/pkgutil" <<'EOF'
#!/bin/sh
db="$(dirname "$0")/receipts"
case "$1" in
  --pkg-info)
    [ -f "$db/$2.info" ] || { echo "No receipt for '$2' found at '/'." >&2; exit 1; }
    cat "$db/$2.info" ;;
  --files)
    [ -f "$db/$2.files" ] || exit 1
    cat "$db/$2.files" ;;
  --file-info)
    printf 'volume: /\npath: %s\n' "$2"
    [ -f "$db/claims" ] || exit 0
    while IFS='	' read -r p id; do
      [ "$p" = "$2" ] && printf '\npkgid: %s\n' "$id"
    done <"$db/claims"
    exit 0 ;;
  --forget)
    [ -f "$db/forget-fails" ] && { echo "pkgutil: could not forget" >&2; exit 1; }
    [ -f "$db/$2.info" ] || exit 1
    rm -f "$db/$2.info" "$db/$2.files" ;;
  *) exit 64 ;;
esac
EOF
  chmod 755 "$C/pkgutil"
}

st_render_for() { "$builder" --render "$1" "$2" "$R" "$U" "$G" "$C/pkgutil" "$3" >/dev/null; }

st_dirs() {
  mkdir -p "$D"
  chmod 755 "$R/usr/local/libexec" "$D"
}

# st_tree V [subset]: a version tree as the package lays it down.
st_tree() {
  local v="$1" t n
  st_dirs
  t="$D/$v"
  mkdir -p "$t/tools"
  chmod 755 "$t" "$t/tools"
  for n in $NAMES; do
    if [ "${2:-}" = subset ] && [ "$n" = syndeo-webkit ]; then continue; fi
    printf '#!/bin/sh\necho "%s %s"\n' "$n" "$v" >"$t/$n"
    chmod 755 "$t/$n"
  done
  echo readme >"$t/README.md"
  chmod 644 "$t/README.md"
  if [ "${2:-}" != subset ]; then
    echo license >"$t/LICENSE"
    echo '(module)' >"$t/tools/wordcount.wat"
    chmod 644 "$t/LICENSE" "$t/tools/wordcount.wat"
  fi
  st_render_for uninstall "$v" "$t/uninstall.sh"
}

st_links() {
  local n
  mkdir -p "$R/usr/local/bin"
  chmod 755 "$R/usr/local/bin"
  for n in $NAMES; do
    ln -s "../libexec/syndeo/current/$n" "$R/usr/local/bin/$n"
  done
}

st_current() { ln -s "$1" "$D/current"; }

# st_receipt V [LOCATION]: the receipt for V. LOCATION is its location lines,
# none if empty; by default the one pkgutil prints for this package, whose
# location relative to the volume is empty.
st_receipt() {
  local v="$1" loc="${2-location: }" n
  {
    printf 'package-id: com.sum.syndeo.pkg\nversion: %s\nvolume: /\n' "$v"
    [ -z "$loc" ] || printf '%s\n' "$loc"
    printf 'install-time: 1\n'
  } >"$DB/com.sum.syndeo.pkg.info"
  {
    printf '%s\n' usr usr/local usr/local/bin usr/local/libexec usr/local/libexec/syndeo \
      "usr/local/libexec/syndeo/$v" "usr/local/libexec/syndeo/$v/tools" \
      "usr/local/libexec/syndeo/$v/tools/wordcount.wat" "usr/local/libexec/syndeo/$v/README.md" \
      "usr/local/libexec/syndeo/$v/LICENSE" "usr/local/libexec/syndeo/$v/uninstall.sh"
    for n in $NAMES; do
      printf '%s\n' "usr/local/libexec/syndeo/$v/$n" "usr/local/bin/$n"
    done
  } >"$DB/com.sum.syndeo.pkg.files"
}

# st_installed V [LOCATION]: what a completed installation of V leaves.
st_installed() { st_tree "$1"; st_links; st_current "$1"; st_receipt "$@"; }

# st_failed X W [AT]: what an upgrade from X to W whose postinstall failed
# leaves, as measured: X's receipt, X's tree gone, W's tree and the links in
# place, and current still at X (failed before the switch) or at AT.
st_failed() {
  st_installed "$1"
  rm -rf "${D:?}/$1"
  st_tree "$2"
  rm "$D/current"
  st_current "${3:-$1}"
}

st_snap() {
  { find "$R" "$DB" -print0 | xargs -0 stat -f '%N|%HT|%u|%g|%Lp|%Mp|%Sf|%i|%z|%Y'; } | LC_ALL=C sort
}

# st_run SCRIPT [args...]: run a rendered script, recording its status,
# output, and whether anything under the root or the receipts changed.
st_run() {
  local before after
  before="$(st_snap)"
  /bin/sh "$@" >"$C/out" 2>&1
  status=$?
  after="$(st_snap)"
  [ "$before" = "$after" ] && changed=no || changed=yes
}

# pre_case NAME EXIT TEXT VERSION [TARGET]: the preinstall for VERSION exits
# EXIT, says TEXT, and changes nothing.
pre_case() {
  local name="$1" want="$2" text="$3" v="$4" target="${5:-/}"
  st_render_for preinstall "$v" "$C/preinstall"
  st_run "$C/preinstall" "$C/fake.pkg" / "$target" /
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qF -- "$text" "$C/out" || verdict=no
  [ "$changed" = no ] || verdict=no
  expect "preinstall: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_preinstall() {
  printf '\n  preinstall: no receipt\n'
  st_new; pre_case "nothing installed" 0 "installing 0.0.9" 0.0.9
  st_new; rm -rf "${R:?}/usr/local"; pre_case "no /usr/local at all" 0 "installing 0.0.9" 0.0.9
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; echo x >"$R/usr/local/bin/syndeo"
  pre_case "refused: a plain file at a command" 1 "/usr/local/bin/syndeo: exists, and no Syndeo package is installed" 0.0.9
  st_new; st_links; pre_case "refused: the package's own link shapes" 1 "exists, and no Syndeo package is installed" 0.0.9
  st_new; st_tree 0.0.9 subset; pre_case "an interrupted first install of this version" 0 "resuming an interrupted installation of 0.0.9" 0.0.9
  st_new; st_tree 0.0.9; st_current 0.0.9; st_links; pre_case "interrupted after the switch, links in place" 0 "resuming an interrupted installation of 0.0.9" 0.0.9
  st_new; st_tree 0.0.9 subset; st_current 0.0.9; pre_case "refused: current set, but the tree incomplete" 1 "is missing" 0.0.9
  st_new; st_tree 0.0.8; pre_case "refused: another version" 1 "exists, and no Syndeo package is installed" 0.0.9
  st_new; st_tree 0.0.9 subset; echo x >"$D/notes"; pre_case "refused: something else in the private directory" 1 "$D/notes: is not part of Syndeo" 0.0.9
  st_new; st_tree 0.0.9 subset; st_links; rm "$R/usr/local/bin/syndeo-ui"; ln -s /tmp/elsewhere "$R/usr/local/bin/syndeo-ui"
  pre_case "refused: a foreign link beside an interrupted install" 1 "syndeo-ui: is not a link this package writes" 0.0.9

  printf '\n  preinstall: one version installed\n'
  st_new; st_installed 0.0.9; pre_case "upgrade 0.0.9 to 0.0.10, numerically" 0 "upgrading from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; pre_case "the same version again" 0 "reinstalling 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; rm "$D/0.0.10/README.md" "$D/0.0.10/tools/wordcount.wat"
  pre_case "the same version, files an interruption left missing: repaired" 0 "repairing 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; rm "$R/usr/local/bin/syndeo-agent"
  pre_case "the same version, a command link missing: repaired" 0 "repairing 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; pre_case "refused: a downgrade, 0.0.10 to 0.0.9" 1 "Syndeo 0.0.10 is installed, and this package is the older 0.0.9" 0.0.9
  st_new; st_installed 0.0.9; rm "$D/0.0.9/README.md"
  pre_case "refused: an upgrade from an installation missing a path the receipt lists" 1 "$D/0.0.9/README.md: is missing, and the receipt for 0.0.9 lists it" 0.0.10
  st_new; st_installed 0.0.9; rm "$R/usr/local/bin/syndeo-agent"
  pre_case "refused: an upgrade from an installation missing a command" 1 "/usr/local/bin/syndeo-agent: is missing, and the receipt for 0.0.9 lists it" 0.0.10
  st_new; st_installed 0.0.9; st_tree 0.0.10
  pre_case "refused: a second version beside the installed one" 1 "with 0.0.9 installed, it may hold nothing else" 0.0.10
  st_new; st_installed 0.0.9; st_tree 0.0.10; rm "$D/current"; st_current 0.0.10
  pre_case "refused: current at a second version, the installed one still there" 1 "points at '0.0.10', not the installed 0.0.9" 0.0.10
  st_new; st_installed 0.0.9; rm "$D/current"; pre_case "refused: current is missing" 1 "$D/current: points at '', not the installed 0.0.9" 0.0.10
  st_new; st_installed 0.0.9; rm "$D/current"; ln -s elsewhere "$D/current"
  pre_case "refused: current does not name a version" 1 "points at 'elsewhere', which is not a version" 0.0.10
  st_new; st_installed 0.0.9; ln -s 0.0.8 "$D/.current.new"
  pre_case "refused: .current.new names another version" 1 ".current.new: points at 0.0.8, not 0.0.9" 0.0.10
  st_new; st_installed 0.0.9; rm "$R/usr/local/bin/syndeo-net"; echo x >"$R/usr/local/bin/syndeo-net"
  pre_case "refused: a command replaced by a file" 1 "syndeo-net: is not a link this package writes" 0.0.10
  st_new; st_installed 0.0.9; echo usr/local/extra >>"$DB/com.sum.syndeo.pkg.files"
  pre_case "refused: the receipt lists other paths" 1 "does not list exactly the package's paths" 0.0.10
  st_new; st_installed 0.0.9; sed -i '' 's/^volume: \//volume: \/Volumes\/Other/' "$DB/com.sum.syndeo.pkg.info"
  pre_case "refused: the receipt is for another volume" 1 "the receipt is for volume '/Volumes/Other', not /" 0.0.10
  st_new; st_installed 0.0.9; sed -i '' 's/^version: .*/version: 0.9/' "$DB/com.sum.syndeo.pkg.info"
  pre_case "refused: the receipt's version is malformed" 1 "is not a version" 0.0.10
  st_new; st_installed 0.0.9; printf '%s\tcom.example.other\n' "$R/usr/local/bin/syndeo" >"$DB/claims"
  pre_case "refused: another package claims a command" 1 "is claimed by another package, com.example.other" 0.0.10
  st_new; st_installed 0.0.10; chmod 775 "$D/0.0.10/syndeo"; pre_case "the same version refused: an executable's mode changed" 1 "$D/0.0.10/syndeo: is not a Regular File" 0.0.10
  st_new; st_installed 0.0.10; echo x >"$D/0.0.10/extra"; pre_case "the same version refused: an extra file" 1 "$D/0.0.10/extra: is not part of Syndeo 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; rm "$D/0.0.10/README.md"; echo x >"$D/0.0.10/extra"
  pre_case "the same version refused: a missing file does not excuse an extra one" 1 "$D/0.0.10/extra: is not part of Syndeo 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; rm "$D/0.0.10/LICENSE"; ln -s README.md "$D/0.0.10/LICENSE"
  pre_case "the same version refused: a symlink inside the tree" 1 "$D/0.0.10/LICENSE: is not a Regular File" 0.0.10
  st_new; st_installed 0.0.10; chmod +a "everyone allow read" "$D/0.0.10/README.md"
  pre_case "the same version refused: an ACL on a file" 1 "$D/0.0.10/README.md: is not a Regular File" 0.0.10
  st_new; st_installed 0.0.10; chflags uchg "$D/0.0.10/README.md"
  pre_case "the same version refused: a file flagged immutable" 1 "$D/0.0.10/README.md: is not a Regular File" 0.0.10
  chflags nouchg "$D/0.0.10/README.md"

  printf '\n  preinstall: after a failed upgrade\n'
  st_new; st_failed 0.0.9 0.0.10
  pre_case "failed at the postinstall's entry: the same package completes it" 0 "completing the failed upgrade from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_failed 0.0.9 0.0.10 0.0.10
  pre_case "failed after the switch: the same package completes it" 0 "completing the failed upgrade from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; ln -s 0.0.10 "$D/.current.new"
  pre_case "failed between the new link and the switch: completed" 0 "completing the failed upgrade from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; rm "$D/0.0.10/LICENSE" "$R/usr/local/bin/syndeo-ui"
  pre_case "failed, a file and a command link missing: completed" 0 "completing the failed upgrade from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_failed 0.0.9 0.0.10
  pre_case "refused: the old version's package" 1 "an upgrade from 0.0.9 to 0.0.10 did not finish: install the Syndeo 0.0.10 package again to complete it" 0.0.9
  st_new; st_failed 0.0.9 0.0.10
  pre_case "refused: an older package" 1 "did not finish: install the Syndeo 0.0.10 package again" 0.0.8
  st_new; st_failed 0.0.9 0.0.10 0.0.10
  pre_case "refused: a newer package" 1 "did not finish: install the Syndeo 0.0.10 package again" 0.0.11
  st_new; st_failed 0.0.10 0.0.9
  pre_case "refused: what is left is not newer" 1 "the installed 0.0.10 is gone, and 0.0.9 is not newer" 0.0.9
  st_new; st_failed 0.0.9 0.0.10; st_tree 0.0.11
  pre_case "refused: two versions left" 1 "rather than exactly one newer version" 0.0.10
  st_new; st_installed 0.0.9; rm -rf "${D:?}/0.0.9"
  pre_case "refused: no version left at all" 1 "it holds nothing rather than exactly one newer version" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; rm "$D/current"; st_current 0.0.8
  pre_case "refused: current names a third version" 1 "points at 0.0.8, neither 0.0.9 nor 0.0.10" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; rm "$D/current"
  pre_case "refused: current is missing" 1 "$D/current: is missing" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; ln -s 0.0.9 "$D/.current.new"
  pre_case "refused: .current.new names the old version" 1 ".current.new: points at 0.0.9, not 0.0.10" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; echo x >"$D/0.0.10/extra"
  pre_case "refused: an extra file in what is left" 1 "$D/0.0.10/extra: is not part of Syndeo 0.0.10" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; chmod 775 "$D/0.0.10/syndeo-net"
  pre_case "refused: a file's mode changed in what is left" 1 "$D/0.0.10/syndeo-net: is not a Regular File" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; rm "$D/0.0.10/LICENSE"; ln -s README.md "$D/0.0.10/LICENSE"
  pre_case "refused: a symlink in what is left" 1 "$D/0.0.10/LICENSE: is not a Regular File" 0.0.10
  st_new; st_failed 0.0.9 0.0.10; rm "$R/usr/local/bin/syndeo-ui"; ln -s /tmp/elsewhere "$R/usr/local/bin/syndeo-ui"
  pre_case "refused: a foreign command" 1 "syndeo-ui: is not a link this package writes" 0.0.10

  printf '\n  preinstall: where, and the paths around it\n'
  st_new; pre_case "another target volume" 1 "the target volume is '/Volumes/Other'" 0.0.9 /Volumes/Other
  st_new; mkdir -p "$R/usr/local/bin"; chmod 777 "$R/usr/local/bin"; pre_case "/usr/local/bin writable by everyone" 1 "/usr/local/bin: can be written by everyone" 0.0.9
  st_new; mkdir -p "$R/usr/local/bin"; chmod 775 "$R/usr/local/bin"
  if chgrp 80 "$R/usr/local/bin" 2>/dev/null; then
    pre_case "/usr/local/bin group-writable by admin is accepted" 0 "installing 0.0.9" 0.0.9
  else
    expect "/usr/local/bin group-writable by admin (needs membership of admin to set up)" false
  fi
  st_new; mkdir -p "$R/usr/local/bin"; chmod 775 "$R/usr/local/bin"; chgrp 20 "$R/usr/local/bin"
  pre_case "/usr/local/bin group-writable by staff" 1 "neither wheel nor admin" 0.0.9
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; chmod +a "everyone allow add_file" "$R/usr/local/bin"
  pre_case "/usr/local/bin with an ACL" 1 "/usr/local/bin: has an access control list" 0.0.9
  st_new; mkdir -p "$R/usr/local/libexec"; chmod 775 "$R/usr/local/libexec"; pre_case "/usr/local/libexec group-writable" 1 "/usr/local/libexec: can be written by its group or by everyone" 0.0.9
  st_new; mkdir -p "$R/usr/local/libexec" "$C/elsewhere"; chmod 755 "$R/usr/local/libexec"; ln -s "$C/elsewhere" "$D"
  pre_case "the private directory is a symlink" 1 "$D: is a Symbolic Link, not a directory" 0.0.9
  st_new; st_dirs; pre_case "an empty private directory, no receipt" 1 "exists, and no Syndeo package is installed" 0.0.9
  st_new; U=0; pre_case "/usr/local not owned by root" 1 "/usr/local: is owned by uid $(id -u), not root" 0.0.9
}

# post_case NAME EXIT TEXT VERSION
post_case() {
  local name="$1" want="$2" text="$3" v="$4"
  st_render_for postinstall "$v" "$C/postinstall"
  /bin/sh "$C/postinstall" "$C/fake.pkg" / / / >"$C/out" 2>&1
  status=$?
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qF -- "$text" "$C/out" || verdict=no
  expect "postinstall: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_postinstall() {
  printf '\n  postinstall: the switch\n'
  st_new; st_tree 0.0.9; st_links; post_case "first install: current is created" 0 "0.0.9 is current" 0.0.9
  expect "  current points at 0.0.9" test "$(readlink "$D/current")" = 0.0.9
  st_new; st_failed 0.0.9 0.0.10
  post_case "an upgrade, as Installer leaves it: current moves off the removed version" 0 "0.0.10 is current" 0.0.10
  expect "  current points at 0.0.10" test "$(readlink "$D/current")" = 0.0.10
  st_new; st_failed 0.0.9 0.0.10; ln -s 0.0.9 "$D/.current.new"
  post_case "a stale .current.new from an interrupted switch" 0 "0.0.10 is current" 0.0.10
  expect "  .current.new is gone" eval "! [ -e '$D/.current.new' ] && ! [ -L '$D/.current.new' ]"
  st_new; st_failed 0.0.9 0.0.10; rm "$D/0.0.10/README.md"
  post_case "an incomplete tree is not made current" 1 "is missing" 0.0.10
  expect "  current still points at 0.0.9" test "$(readlink "$D/current")" = 0.0.9
  st_new; st_installed 0.0.9; st_tree 0.0.10
  post_case "another version still there: refused before the switch" 1 "$D/0.0.9: is still there; Syndeo 0.0.10 is installed alone" 0.0.10
  expect "  current still points at 0.0.9" test "$(readlink "$D/current")" = 0.0.9
  st_new; st_tree 0.0.9; mkdir "$D/current"; post_case "current is a directory" 1 "$D/current: is not a symlink owned by root:wheel" 0.0.9
  st_new; st_tree 0.0.9
  st_render_for postinstall 0.0.9 "$C/postinstall"
  /bin/sh "$C/postinstall" "$C/fake.pkg" / /Volumes/Other / >"$C/out" 2>&1
  status=$?
  expect "postinstall: refuses another target volume" eval "[ $status = 1 ] && grep -q \"the target volume is '/Volumes/Other'\" '$C/out' && ! [ -L '$D/current' ]"
}

# The switch itself, under load: lookups and starts through `current` while
# it is renamed back and forth between two complete trees. The real runner
# measures a whole upgrade instead, in ci/test-pkg-install.sh.
st_atomic() {
  printf '\n  the switch, under load\n'
  st_new; st_installed 0.0.8; st_tree 0.0.9
  local out
  out="$(python3 -I "$here/switch-stress.py" \
    --private "$D" --command "$R/usr/local/bin/syndeo" --versions 0.0.8 0.0.9 \
    --expect-output 'syndeo {version}' --renames 10000 2>&1)"
  local status=$?
  printf '%s\n' "$out" | sed 's/^/          | /'
  expect "10,000 renames: only ENOENT or EINVAL while switching, every success whole, none after" test "$status" = 0
}

# un_case NAME EXIT TEXT SCRIPT [ARG]: run an uninstaller.
un_case() {
  local name="$1" want="$2" text="$3" script="$4"; shift 4
  st_run "$script" "$@"
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qF -- "$text" "$C/out" || verdict=no
  expect "uninstall: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_uninstall() {
  printf '\n  uninstall: look first, then remove\n'
  st_new; st_installed 0.0.10; ln -s 0.0.10 "$D/.current.new"
  mkdir -p "$R/usr/local/libexec/someone-else"; echo keep >"$R/usr/local/bin/unrelated"
  un_case "everything, a stale .current.new included" 0 "removed Syndeo 0.0.10" "$D/0.0.10/uninstall.sh"
  expect "  nothing of Syndeo left, and the receipt forgotten" eval "! [ -e '$D' ] && ! [ -L '$R/usr/local/bin/syndeo' ] && ! [ -f '$DB/com.sum.syndeo.pkg.info' ]"
  expect "  /usr/local/bin, /usr/local/libexec and what else they hold are kept" eval "[ -d '$R/usr/local/bin' ] && [ -d '$R/usr/local/libexec/someone-else' ] && [ \"\$(cat '$R/usr/local/bin/unrelated')\" = keep ]"

  st_new; st_failed 0.0.9 0.0.10
  un_case "after an upgrade that failed before the switch: refused, saying to finish it" 1 "the upgrade from 0.0.9 to 0.0.10 did not finish. Install the Syndeo 0.0.10 package again" "$D/0.0.10/uninstall.sh"
  [ "$changed" = no ] || { expect "  ... and nothing changed" false; }
  st_new; st_failed 0.0.9 0.0.10 0.0.10
  un_case "after an upgrade that failed after the switch: refused" 1 "the upgrade from 0.0.9 to 0.0.10 did not finish" "$D/0.0.10/uninstall.sh"
  [ "$changed" = no ] || { expect "  ... and nothing changed" false; }
  st_new; st_installed 0.0.10; rm "$D/0.0.10/README.md"
  un_case "a path the receipt lists is missing: refused" 1 "$D/0.0.10/README.md: is missing, and the receipt for 0.0.10 lists it" "$D/0.0.10/uninstall.sh"
  [ "$changed" = no ] || { expect "  ... and nothing changed" false; }
  st_new; st_installed 0.0.10; rm "$R/usr/local/bin/syndeo-agent"
  un_case "a command the receipt lists is missing: refused" 1 "/usr/local/bin/syndeo-agent: is missing, and the receipt for 0.0.10 lists it" "$D/0.0.10/uninstall.sh"
  [ "$changed" = no ] || { expect "  ... and nothing changed" false; }

  local plant
  for plant in tools-extra tree-extra private-file private-dir bad-version older-version foreign-link mode acl pending; do
    st_new; st_installed 0.0.10
    case "$plant" in
      tools-extra) echo x >"$D/0.0.10/tools/extra.wat" ;;
      tree-extra) echo x >"$D/0.0.10/notes.txt" ;;
      private-file) echo x >"$D/notes.txt" ;;
      private-dir) mkdir "$D/backup" ;;
      bad-version) mkdir "$D/0.1" ;;
      older-version) st_tree 0.0.9 ;;
      foreign-link) rm "$R/usr/local/bin/syndeo-ui"; ln -s /Applications/Other.app "$R/usr/local/bin/syndeo-ui" ;;
      mode) chmod 775 "$D/0.0.10/syndeo-net" ;;
      acl) chmod +a "everyone allow list" "$D/0.0.10" ;;
      pending) ln -s elsewhere "$D/.current.new" ;;
    esac
    un_case "all or nothing, with $plant planted: refused, nothing changed" 1 "refusing:" "$D/0.0.10/uninstall.sh"
    [ "$changed" = no ] || { expect "  ... and nothing changed" false; }
  done

  st_new; st_installed 0.0.10
  un_case "--old-versions is gone" 1 "usage:" "$D/0.0.10/uninstall.sh" --old-versions

  st_new; st_installed 0.0.10; touch "$DB/forget-fails"
  un_case "the receipt cannot be forgotten" 2 "sudo /usr/sbin/pkgutil --forget com.sum.syndeo.pkg --volume /" "$D/0.0.10/uninstall.sh"
  expect "  every file is gone, and the receipt is still there" eval "! [ -e '$D' ] && [ -f '$DB/com.sum.syndeo.pkg.info' ]"

  st_new; st_installed 0.0.10; st_tree 0.0.11; rm "$D/current"; st_current 0.0.11
  un_case "current points past the receipt" 1 "points at '0.0.11', not 0.0.10" "$D/0.0.10/uninstall.sh"

  st_new; st_installed 0.0.10; U=0; st_render_for uninstall 0.0.10 "$C/as-root"
  un_case "not run as root" 1 "run it as root, with sudo" "$C/as-root"

  st_new; st_installed 0.0.10
  un_case "an unknown argument" 1 "usage:" "$D/0.0.10/uninstall.sh" --everything

  st_new; st_tree 0.0.10; st_links; st_current 0.0.10
  un_case "no receipt" 1 "no Syndeo package is installed" "$D/0.0.10/uninstall.sh"
}

st_installed_mode() {
  printf '\n  this script: installed\n'
  st_new; st_installed 0.0.10
  local out
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.10 2>&1)"
  expect "installed: a complete installation passes" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  rm "$D/current"; st_current 0.0.10; rm "$R/usr/local/bin/syndeo-agent"
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.10 2>&1)"
  expect "installed: a missing command fails" eval "printf '%s' \"\$out\" | grep -q 'has 6 of the seven commands'"
  st_new; st_installed 0.0.10; rm "$D/0.0.10/README.md"
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.10 2>&1)"
  expect "installed: a path the receipt lists is missing fails" eval "printf '%s' \"\$out\" | grep -q 'README.md: is missing, and the receipt for 0.0.10 lists it'"
  st_new; st_installed 0.0.10; st_tree 0.0.9
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.10 2>&1)"
  expect "installed: a second version beside it fails" eval "printf '%s' \"\$out\" | grep -q 'not 0.0.10 alone'"
  st_new; st_failed 0.0.9 0.0.10 0.0.10
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.10 2>&1)"
  expect "installed: after a failed upgrade, it fails on the receipt and its missing paths" eval "printf '%s' \"\$out\" | grep -q \"version '0.0.9'\" && printf '%s' \"\$out\" | grep -q 'is missing, and the receipt for 0.0.9 lists it'"
}

# loc_case NAME accepted|refused LOCATION: a receipt whose location lines are
# LOCATION, judged by every script that reads a receipt: this script's
# installed mode, the preinstall of an upgrade, and the uninstaller.
loc_case() {
  local name="$1" want="$2" loc="$3" out verdict
  st_new; st_installed 0.0.9 "$loc"
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.9 2>&1)"
  verdict=yes
  if [ "$want" = accepted ]; then
    printf '%s' "$out" | grep -q '[1-9][0-9]* passed, 0 failed' || verdict=no
  else
    printf '%s\n' "$out" | grep 'FAIL' | grep -qE 'location fields, not one|location is .*, not the volume.s root' || verdict=no
  fi
  expect "location: $name: installed $want" test "$verdict" = yes
  [ "$verdict" = yes ] || printf '%s\n' "$out" | sed 's/^/          | /'

  st_render_for preinstall 0.0.10 "$C/preinstall"
  st_run "$C/preinstall" "$C/fake.pkg" / / /
  verdict=yes
  [ "$changed" = no ] || verdict=no
  if [ "$want" = accepted ]; then
    [ "$status" = 0 ] && grep -qF "upgrading from 0.0.9 to 0.0.10" "$C/out" || verdict=no
  else
    [ "$status" = 1 ] && grep -qE 'location fields, not one|location is .*, not the volume.s root' "$C/out" || verdict=no
  fi
  expect "location: $name: preinstall $want" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"

  st_run "$D/0.0.9/uninstall.sh"
  verdict=yes
  if [ "$want" = accepted ]; then
    [ "$status" = 0 ] && grep -qF "removed Syndeo 0.0.9" "$C/out" && ! [ -f "$DB/com.sum.syndeo.pkg.info" ] || verdict=no
  else
    [ "$status" = 1 ] && [ "$changed" = no ] && grep -qE 'location fields, not one|location is .*, not the volume.s root' "$C/out" || verdict=no
  fi
  expect "location: $name: uninstall $want" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_location() {
  printf '\n  the receipt'"'"'s location field\n'
  loc_case "empty, as pkgutil prints it for this package" accepted 'location: '
  loc_case "/" accepted 'location: /'
  loc_case "missing" refused ''
  loc_case "duplicated" refused $'location: /\nlocation: /'
  loc_case "duplicated, empty and /" refused $'location: \nlocation: /'
  loc_case "empty, without the separating space" refused 'location:'
  loc_case "/, without the separating space" refused 'location:/'
  loc_case "/, after two spaces" refused 'location:  /'
  loc_case "/, then a space" refused 'location: / '
  loc_case "/, after a tab" refused $'location:\t/'
  loc_case "two spaces" refused 'location:  '
  loc_case "another directory" refused 'location: Applications/Other.app'
  loc_case "an absolute directory" refused 'location: /usr/local'
  loc_case "(null)" refused 'location: (null)'
}

# decision_case NAME KIND VERSION EXIT LINE [TARGET]: the KIND script
# (preinstall or postinstall), rendered for VERSION and run on the state this
# case has set up, exits EXIT and prints LINE, exactly, as one whole line; a
# preinstall changes nothing.
decision_case() {
  local name="$1" kind="$2" v="$3" want="$4" line="$5" target="${6:-/}"
  st_render_for "$kind" "$v" "$C/$kind"
  st_run "$C/$kind" "$C/fake.pkg" / "$target" /
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qxF -- "$line" "$C/out" || verdict=no
  if [ "$kind" = preinstall ] && [ "$changed" != no ]; then verdict=no; fi
  decided="$decided $name"
  expect "decision: $name" test "$verdict" = yes
  if [ "$verdict" != yes ]; then
    printf '          | wanted exit %s and: %s\n' "$want" "$line"
    printf '          | got exit %s:\n' "$status"
    sed 's/^/          | /' "$C/out"
  fi
}

# fault_case NAME X W WHERE AT: a failed upgrade from X to W through the
# test-only fault ci/pkg-test-fault.sh adds to W's postinstall, at WHERE. Its
# first run says exactly that it is failing, exits 1 and leaves current at AT;
# its second goes through and makes W current.
fault_case() {
  local name="$1" x="$2" w="$3" where="$4" at="$5" verdict=yes
  st_new; st_failed "$x" "$w"
  st_render_for postinstall "$w" "$C/postinstall"
  bash "$here/pkg-test-fault.sh" --script "$C/postinstall" "$where" "$C/fault" || verdict=no
  /bin/sh "$C/postinstall" "$C/fake.pkg" / / / >"$C/out" 2>&1
  status=$?
  [ "$status" = 1 ] || verdict=no
  grep -qxF "syndeo postinstall: test fault $where: failing this first run" "$C/out" || verdict=no
  [ "$(readlink "$D/current")" = "$at" ] || verdict=no
  /bin/sh "$C/postinstall" "$C/fake.pkg" / / / >"$C/out2" 2>&1 || verdict=no
  grep -qxF "syndeo postinstall: $w is current. Commands did not start while the previous version was replaced; a Syndeo still running from it has to be quit and started again" "$C/out2" || verdict=no
  [ "$(readlink "$D/current")" = "$w" ] || verdict=no
  decided="$decided $name"
  expect "decision: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out" "$C/out2"
}

# What each package script says in the real-runner test, ci/test-pkg-install.sh,
# asserted here exactly and as the script runs, from the same templates under
# a temporary root. The runner gates on what the installation is before and
# after, not on these lines: Installer normally records them in
# /var/log/install.log, but does not guarantee they appear there. Each case
# is named after the runner scenario it stands for.
st_decisions() {
  printf '\n  the decisions the real-runner test relies on, said exactly\n'
  local p='syndeo preinstall: refusing:' other g label missing
  decided=''

  # --system-before
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; echo foreign >"$R/usr/local/bin/syndeo"
  decision_case "foreign-file" preinstall 0.1.5 1 "$p $R/usr/local/bin/syndeo: exists, and no Syndeo package is installed"
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; echo foreign >"$C/foreign-target"; ln -s "$C/foreign-target" "$R/usr/local/bin/syndeo-net"
  decision_case "foreign-link" preinstall 0.1.5 1 "$p $R/usr/local/bin/syndeo-net: exists, and no Syndeo package is installed"
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; ln -s ../libexec/other/syndeo-proxy "$R/usr/local/bin/syndeo-proxy"
  decision_case "foreign-dangling-link" preinstall 0.1.5 1 "$p $R/usr/local/bin/syndeo-proxy: exists, and no Syndeo package is installed"
  st_new; mkdir -p "$R/usr/local/bin/syndeo-ui"; chmod 755 "$R/usr/local/bin"
  decision_case "foreign-directory" preinstall 0.1.5 1 "$p $R/usr/local/bin/syndeo-ui: exists, and no Syndeo package is installed"
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; ln -s ../libexec/syndeo/current/syndeo-agent "$R/usr/local/bin/syndeo-agent"
  decision_case "package-shaped-link-without-receipt" preinstall 0.1.5 1 "$p $R/usr/local/bin/syndeo-agent: exists, and no Syndeo package is installed"
  st_new; mkdir -p "$R/usr/local/bin"; chmod 777 "$R/usr/local/bin"
  decision_case "bin-world-writable" preinstall 0.1.5 1 "$p $R/usr/local/bin: can be written by everyone (mode 777)"
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; chmod +a "everyone allow add_file" "$R/usr/local/bin"
  decision_case "bin-acl" preinstall 0.1.5 1 "$p $R/usr/local/bin: has an access control list"
  st_new; mkdir -p "$R/usr/local/libexec"; chmod 775 "$R/usr/local/libexec"
  decision_case "libexec-group-writable" preinstall 0.1.5 1 "$p $R/usr/local/libexec: can be written by its group or by everyone (mode 775)"
  st_new; mkdir -p "$R/usr/local/libexec" "$C/elsewhere"; chmod 755 "$R/usr/local/libexec"; ln -s "$C/elsewhere" "$D"
  decision_case "private-directory-is-a-link" preinstall 0.1.5 1 "$p $D: is a Symbolic Link, not a directory"
  st_new; st_tree 0.0.7; st_current 0.0.7
  decision_case "orphan" preinstall 0.1.5 1 "$p $D: exists, and no Syndeo package is installed; it holds 0.0.7, current -> 0.0.7"
  st_new; st_tree 0.1.5 subset
  decision_case "resume" preinstall 0.1.5 0 "syndeo preinstall: resuming an interrupted installation of 0.1.5"
  st_new
  decision_case "standin-0.0.9" preinstall 0.0.9 0 "syndeo preinstall: installing 0.0.9"
  st_new; st_installed 0.0.11
  decision_case "older-package" preinstall 0.0.10 1 "$p Syndeo 0.0.11 is installed, and this package is the older 0.0.10. Uninstall 0.0.11 first with $D/0.0.11/uninstall.sh"
  fault_case "fault-at-entry" 0.0.11 0.0.12 entry 0.0.11
  st_new; st_failed 0.0.11 0.0.12
  decision_case "fault-at-entry-older-package" preinstall 0.0.11 1 "$p an upgrade from 0.0.11 to 0.0.12 did not finish: install the Syndeo 0.0.12 package again to complete it, then this one"
  st_new; st_failed 0.0.11 0.0.12
  decision_case "fault-at-entry-newer-package" preinstall 0.0.13 1 "$p an upgrade from 0.0.11 to 0.0.12 did not finish: install the Syndeo 0.0.12 package again to complete it, then this one"
  st_new; st_failed 0.0.11 0.0.12
  decision_case "fault-at-entry-the-same-package-again" preinstall 0.0.12 0 "syndeo preinstall: completing the failed upgrade from 0.0.11 to 0.0.12"
  fault_case "fault-after-switch" 0.0.12 0.0.13 after-switch 0.0.13
  st_new; st_failed 0.0.12 0.0.13 0.0.13
  decision_case "fault-after-switch-older-package" preinstall 0.0.12 1 "$p an upgrade from 0.0.12 to 0.0.13 did not finish: install the Syndeo 0.0.13 package again to complete it, then this one"
  st_new; st_failed 0.0.12 0.0.13 0.0.13
  decision_case "fault-after-switch-newer-package" preinstall 0.0.14 1 "$p an upgrade from 0.0.12 to 0.0.13 did not finish: install the Syndeo 0.0.13 package again to complete it, then this one"
  st_new; st_failed 0.0.12 0.0.13 0.0.13
  decision_case "fault-after-switch-the-same-package-again" preinstall 0.0.13 0 "syndeo preinstall: completing the failed upgrade from 0.0.12 to 0.0.13"
  st_new; st_installed 0.0.13
  decision_case "real-binaries-0.0.15" preinstall 0.0.15 0 "syndeo preinstall: upgrading from 0.0.13 to 0.0.15"
  st_new; st_installed 0.0.15
  decision_case "real" preinstall 0.1.5 0 "syndeo preinstall: upgrading from 0.0.15 to 0.1.5"

  # --system-after
  st_new; st_installed 0.1.5
  decision_case "reinstall" preinstall 0.1.5 0 "syndeo preinstall: reinstalling 0.1.5"
  st_new; st_installed 0.1.5; chmod 775 "$D/0.1.5/syndeo-net"
  decision_case "reinstall-over-a-changed-mode" preinstall 0.1.5 1 "$p $D/0.1.5/syndeo-net: is not a Regular File owned by root:wheel with mode 755 and nothing else (found Regular File $U:$G 775 special 0 flags -)"
  # The runner gives README.md to another owner; a test that is not root
  # changes its group instead, which the same check refuses with the same
  # message.
  st_new; st_installed 0.1.5
  other=''
  for g in $(id -G); do
    if [ "$g" != "$G" ]; then other="$g"; break; fi
  done
  if [ -n "$other" ] && chgrp "$other" "$D/0.1.5/README.md" 2>/dev/null; then
    decision_case "reinstall-over-a-changed-owner" preinstall 0.1.5 1 "$p $D/0.1.5/README.md: is not a Regular File owned by root:wheel with mode 644 and nothing else (found Regular File $U:$other 644 special 0 flags -)"
  else
    expect "decision: reinstall-over-a-changed-owner (needs a second group to set up)" false
  fi
  st_new; st_installed 0.1.5; echo extra >"$D/0.1.5/notes.txt"
  decision_case "reinstall-over-an-extra-file" preinstall 0.1.5 1 "$p $D/0.1.5/notes.txt: is not part of Syndeo 0.1.5"
  st_new; st_installed 0.1.5; ln -s ../README.md "$D/0.1.5/tools/linked.wat"
  decision_case "reinstall-over-an-internal-symlink" preinstall 0.1.5 1 "$p $D/0.1.5/tools/linked.wat: is not part of Syndeo 0.1.5"
  st_new; st_installed 0.1.5; chmod +a "everyone allow read" "$D/0.1.5/README.md"
  decision_case "reinstall-over-an-acl" preinstall 0.1.5 1 "$p $D/0.1.5/README.md: is not a Regular File owned by root:wheel with mode 644 and nothing else (found Regular File $U:$G 644 special 0 flags -)"
  st_new; st_installed 0.1.5; chflags uchg "$D/0.1.5/README.md"
  decision_case "reinstall-over-an-immutable-file" preinstall 0.1.5 1 "$p $D/0.1.5/README.md: is not a Regular File owned by root:wheel with mode 644 and nothing else (found Regular File $U:$G 644 special 0 flags uchg)"
  chflags nouchg "$D/0.1.5/README.md"
  st_new; st_installed 0.1.5; rm "$D/0.1.5/README.md"
  decision_case "repair" preinstall 0.1.5 0 "syndeo preinstall: repairing 0.1.5"
  st_new; st_installed 0.1.5
  decision_case "downgrade" preinstall 0.0.10 1 "$p Syndeo 0.1.5 is installed, and this package is the older 0.0.10. Uninstall 0.1.5 first with $D/0.1.5/uninstall.sh"
  st_new; st_installed 0.1.5; rm "$D/current"; ln -s 0.0.10 "$D/current"
  decision_case "current-at-another-version" preinstall 0.1.5 1 "$p $D/current: points at '0.0.10', not the installed 0.1.5"
  st_new; st_installed 0.1.5; rm "$R/usr/local/bin/syndeo-ui"; ln -s /Applications/Foreign.app/Contents/MacOS/foreign "$R/usr/local/bin/syndeo-ui"
  decision_case "foreign-command-with-a-receipt" preinstall 0.1.5 1 "$p $R/usr/local/bin/syndeo-ui: is not a link this package writes"
  st_new; st_installed 0.1.5; mkdir "$D/0.0.99"; chmod 755 "$D/0.0.99"
  decision_case "a-second-version" preinstall 0.1.5 1 "$p $D: holds 0.0.99 0.1.5; with 0.1.5 installed, it may hold nothing else"

  # Every install the runner makes whose outcome a script decides has its
  # case above, named after it: each literal label of refused, installed and
  # install, and each failed_upgrade with the three it makes. The install on
  # another volume is Installer's own refusal; no script runs.
  missing=''
  for label in $(
    {
      grep -oE '^[[:space:]]*(refused|installed|install) [a-z0-9][a-z0-9.-]*' "$here/test-pkg-install.sh" | awk '{print $2}'
      grep -oE '^[[:space:]]*failed_upgrade [a-z0-9][a-z0-9.-]*' "$here/test-pkg-install.sh" | awk '{print $2; print $2 "-older-package"; print $2 "-newer-package"; print $2 "-the-same-package-again"}'
    } | sort -u
  ); do
    case " $decided " in *" $label "*) ;; *) missing="$missing $label" ;; esac
  done
  expect "every runner install a script decides has its case here${missing:+ (missing:$missing)}" test -z "$missing"
}

# st_tarball DIR V: a release tarball of stand-ins, as ci/package.sh lays one out.
st_tarball() {
  local dir="$1" v="$2" name n
  name="syndeo-$v-aarch64-apple-darwin"
  mkdir -p "$dir/src/$name/tools"
  for n in $NAMES; do
    printf '#!/bin/sh\necho "%s %s"\n' "$n" "$v" >"$dir/src/$name/$n"
    chmod 755 "$dir/src/$name/$n"
  done
  cp "$here/../README.md" "$here/../LICENSE" "$dir/src/$name/"
  cp "$here/../crates/syndeo-agent/tools/wordcount.wat" "$dir/src/$name/tools/"
  tar -C "$dir/src" --format=ustar -czf "$dir/$name.tar.gz" "$name"
}

st_inspect() {
  printf '\n  this script: inspect, against stand-in packages\n'
  local d="$T/inspect" out
  mkdir -p "$d/out"
  st_tarball "$d" 0.0.1
  "$builder" 0.0.1 "$d/syndeo-0.0.1-aarch64-apple-darwin.tar.gz" "$d/out" >/dev/null
  local pkg="$d/out/syndeo-0.0.1-aarch64-apple-darwin.pkg" tgz="$d/syndeo-0.0.1-aarch64-apple-darwin.tar.gz"
  out="$(bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$tgz" 2>&1)"
  expect "inspect: a package built from the tarball passes" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'

  mkdir -p "$d/other"
  st_tarball "$d/other" 0.0.1
  echo changed >>"$d/other/src/syndeo-0.0.1-aarch64-apple-darwin/README.md"
  tar -C "$d/other/src" --format=ustar -czf "$d/other/syndeo-0.0.1-aarch64-apple-darwin.tar.gz" syndeo-0.0.1-aarch64-apple-darwin
  out="$(bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$d/other/syndeo-0.0.1-aarch64-apple-darwin.tar.gz" 2>&1)"
  expect "inspect: a payload that differs from the tarball fails" eval "printf '%s' \"\$out\" | grep -q 'differs from the tarball: README.md'"

  mkdir -p "$d/fault" "$d/faulty"
  st_tarball "$d/fault" 0.0.2
  "$builder" 0.0.2 "$d/fault/syndeo-0.0.2-aarch64-apple-darwin.tar.gz" "$d/fault" >/dev/null
  local w
  for w in entry after-switch; do
    rm -f "$d/faulty/syndeo-0.0.2-aarch64-apple-darwin.pkg"
    bash "$here/pkg-test-fault.sh" "$d/fault/syndeo-0.0.2-aarch64-apple-darwin.pkg" "$d/faulty/syndeo-0.0.2-aarch64-apple-darwin.pkg" "$w" "$d/faulty/marker" >"$d/faulty/log" 2>&1
    expect "pkg-test-fault.sh: a fault at $w, and nothing else changed" test "$?" = 0
    out="$(bash "$here/verify-pkg.sh" inspect "$d/faulty/syndeo-0.0.2-aarch64-apple-darwin.pkg" 0.0.2 --tarball "$d/fault/syndeo-0.0.2-aarch64-apple-darwin.tar.gz" 2>&1)"
    expect "inspect: a package with a test fault at $w fails" eval "printf '%s' \"\$out\" | grep -q 'postinstall — differs from the template'"
  done
  expect "pkg-test-fault.sh: refuses anything but a 0.0.x stand-in" eval "cp '$pkg' '$d/faulty/syndeo-0.1.7-aarch64-apple-darwin.pkg' && ! bash '$here/pkg-test-fault.sh' '$d/faulty/syndeo-0.1.7-aarch64-apple-darwin.pkg' '$d/faulty/y.pkg' entry '$d/faulty/m' >/dev/null 2>&1 && ! [ -e '$d/faulty/y.pkg' ]"

  # Edited packages: expanded, one thing changed, flattened again.
  edit_case() {
    local label="$1" file="$2" from="$3" to="$4" text="$5"
    rm -rf "$d/edit" "$d/edited"
    pkgutil --expand "$pkg" "$d/edit"
    sed -i '' "s|$from|$to|" "$d/edit/$file"
    mkdir -p "$d/edited"
    pkgutil --flatten "$d/edit" "$d/edited/syndeo-0.0.1-aarch64-apple-darwin.pkg"
    out="$(bash "$here/verify-pkg.sh" inspect "$d/edited/syndeo-0.0.1-aarch64-apple-darwin.pkg" 0.0.1 --tarball "$tgz" 2>&1)"
    expect "inspect: $label fails" eval "printf '%s' \"\$out\" | grep -q -- '$text'"
  }
  edit_case "overwrite-permissions true" syndeo.pkg/PackageInfo 'overwrite-permissions="false"' 'overwrite-permissions="true"' "overwrite-permissions)"
  edit_case "an Intel-capable product" Distribution 'hostArchitectures="arm64"' 'hostArchitectures="arm64,x86_64"' "hostArchitectures)"
  edit_case "a product installable anywhere" Distribution 'enable_anywhere="false"' 'enable_anywhere="true"' "enable_anywhere)"
  edit_case "JavaScript in the product" Distribution '<title>' '<installation-check script="true()"/><title>' "count(//installation-check)"

  cp "$pkg" "$d/renamed.pkg"
  out="$(bash "$here/verify-pkg.sh" inspect "$d/renamed.pkg" 0.0.1 --tarball "$tgz" 2>&1)"
  expect "inspect: a misnamed package fails" eval "printf '%s' \"\$out\" | grep -q 'renamed.pkg is not syndeo-0.0.1-aarch64-apple-darwin.pkg'"
  st_signed "$pkg" "$tgz"
}

# st_signed PKG TARBALL: inspect's judgement of a signed release, with
# stand-ins for codesign, pkgutil --check-signature, xcrun stapler and spctl
# that report what a signed, notarized and stapled package would, each case
# changing one thing.
st_signed() {
  local pkg="$1" tgz="$2" bin="$T/signed-bin" out status
  mkdir -p "$bin"
  cat >"$bin/codesign" <<'EOF'
#!/bin/bash
path="${@: -1}"; n="${path##*/}"; team=AB12CD34EF
[ "$n" != "${STANDIN_OTHER_TEAM:-}" ] || team=ZZ99ZZ99ZZ
case "$1" in
  --verify) [ "$n" != "${STANDIN_UNVERIFIED:-}" ] ;;
  -dv)
    flags='0x10000(runtime)'
    [ "$n" != "${STANDIN_NO_RUNTIME:-}" ] || flags='0x0(none)'
    {
      echo "Executable=$path"
      echo "CodeDirectory v=20500 size=1 flags=$flags hashes=1+7 location=embedded"
      echo "Authority=Developer ID Application: Example Org ($team)"
      echo "Authority=Developer ID Certification Authority"
      echo "Authority=Apple Root CA"
      if [ "$n" = "${STANDIN_NO_TIMESTAMP:-}" ]; then echo "Signed Time=10 Oct 2026 at 12:00:00"; else echo "Timestamp=10 Oct 2026 at 12:00:00"; fi
      echo "TeamIdentifier=$team"
    } >&2 ;;
  -d)
    group=''
    [ "$n" != syndeo-keystore ] || group="${STANDIN_KEYSTORE_GROUP:-$team.com.sum.syndeo.keystore}"
    [ "$n" != "${STANDIN_ENTITLED:-}" ] || group="$team.com.sum.syndeo.keystore"
    [ -n "$group" ] || exit 0
    printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>keychain-access-groups</key><array><string>%s</string></array>%s</dict></plist>\n' \
      "$group" "${STANDIN_KEYSTORE_EXTRA:-}" ;;
esac
EOF
  cat >"$bin/pkgutil" <<'EOF'
#!/bin/bash
[ "$1" = --check-signature ] || exec /usr/sbin/pkgutil "$@"
printf 'Package "%s":\n' "${2##*/}"
printf '   Status: %s\n' "${STANDIN_PKG_STATUS:-signed by a developer certificate issued by Apple for distribution}"
printf '   Notarization: trusted by the Apple notary service\n'
printf '   Signed with a trusted timestamp on: 2026-10-10 12:00:00 +0000\n'
printf '   Certificate Chain:\n'
printf '    1. %s\n       Expires: 2031-10-10 12:00:00 +0000\n' "${STANDIN_PKG_SIGNER:-Developer ID Installer: Example Org (AB12CD34EF)}"
printf '    2. Developer ID Certification Authority\n'
printf '    3. %s\n' "${STANDIN_PKG_ROOT:-Apple Root CA}"
EOF
  cat >"$bin/xcrun" <<'EOF'
#!/bin/bash
[ "$1 $2" = "stapler validate" ] && [ "${STANDIN_STAPLED:-yes}" = yes ]
EOF
  cat >"$bin/spctl" <<'EOF'
#!/bin/bash
if [ "${STANDIN_SPCTL:-accepted}" = accepted ]; then
  printf '%s: accepted\nsource=%s\norigin=%s\n' "${@: -1}" "${STANDIN_SOURCE:-Notarized Developer ID}" "${STANDIN_ORIGIN:-Developer ID Installer: Example Org (AB12CD34EF)}" >&2
else
  printf '%s: rejected\n' "${@: -1}" >&2; exit 3
fi
EOF
  chmod 755 "$bin"/*

  # signed_case LABEL FAILS TEXT [NAME=VALUE...]: inspect, expecting a signed
  # release of AB12CD34EF, fails exactly FAILS checks, one of them saying TEXT
  # (with FAILS 0, it passes).
  signed_case() {
    local label="$1" fails="$2" text="$3"
    shift 3
    out="$(env "$@" PATH="$bin:$PATH" SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID=AB12CD34EF \
      bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$tgz" 2>&1)"
    if [ "$fails" = 0 ]; then
      expect "signed inspect: $label" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
    else
      expect "signed inspect: $label" eval "printf '%s' \"\$out\" | grep -q ' passed, $fails failed' && printf '%s' \"\$out\" | grep 'FAIL' | grep -qF -- \"\$text\""
    fi
    printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'
  }
  printf '\n  this script: inspect, of a signed release, against stand-ins for the signing tools\n'
  signed_case "Developer ID throughout, the keystore alone entitled, notarized and stapled: passes" 0 ""
  signed_case "one executable of another team" 1 "syndeo-net — signed by 'Developer ID Application: Example Org (ZZ99ZZ99ZZ)'; TeamIdentifier 'ZZ99ZZ99ZZ';" STANDIN_OTHER_TEAM=syndeo-net
  signed_case "one executable that does not verify" 1 "syndeo-ui — does not verify;" STANDIN_UNVERIFIED=syndeo-ui
  signed_case "one executable without the hardened runtime" 1 "syndeo-agent — no hardened runtime;" STANDIN_NO_RUNTIME=syndeo-agent
  signed_case "one executable without a secure timestamp" 1 "syndeo-proxy — no secure timestamp;" STANDIN_NO_TIMESTAMP=syndeo-proxy
  signed_case "the keystore's access group of another team" 1 "syndeo-keystore — entitlements are not: keychain-access-groups [AB12CD34EF.com.sum.syndeo.keystore] and nothing else;" STANDIN_KEYSTORE_GROUP=ZZ99ZZ99ZZ.com.sum.syndeo.keystore
  signed_case "the keystore with one entitlement more" 1 "syndeo-keystore — entitlements are not: keychain-access-groups [AB12CD34EF.com.sum.syndeo.keystore] and nothing else;" "STANDIN_KEYSTORE_EXTRA=<key>com.apple.security.get-task-allow</key><true/>"
  signed_case "the access group on another executable" 1 "syndeo-agent — entitlements are not: no entitlements;" STANDIN_ENTITLED=syndeo-agent
  signed_case "the package signed by another team" 1 "package signature — signed by 'Developer ID Installer: Other Org (ZZ99ZZ99ZZ)';" "STANDIN_PKG_SIGNER=Developer ID Installer: Other Org (ZZ99ZZ99ZZ)" "STANDIN_ORIGIN=Developer ID Installer: Other Org (ZZ99ZZ99ZZ)"
  signed_case "the package signed with the Application identity" 1 "package signature — signed by 'Developer ID Application:" "STANDIN_PKG_SIGNER=Developer ID Application: Example Org (AB12CD34EF)" "STANDIN_ORIGIN=Developer ID Application: Example Org (AB12CD34EF)"
  signed_case "the package not chained to Apple Root CA" 1 "chained to 'Someone Root', not Apple Root CA;" "STANDIN_PKG_ROOT=Someone Root"
  signed_case "the package trusted, but not for distribution" 1 "package signature — Status: signed by a certificate trusted by macOS;" "STANDIN_PKG_STATUS=signed by a certificate trusted by macOS"
  signed_case "no stapled ticket" 1 "notarization ticket — stapler validate fails" STANDIN_STAPLED=no
  signed_case "Gatekeeper: Developer ID, not notarized" 1 "Gatekeeper — source=Developer ID;" "STANDIN_SOURCE=Developer ID"
  signed_case "Gatekeeper: from another origin" 1 "Gatekeeper — from another origin;" "STANDIN_ORIGIN=Developer ID Installer: Example Org (ZZ99ZZ99ZZ)"
  signed_case "Gatekeeper rejects it" 1 "Gatekeeper — rejected;" STANDIN_SPCTL=rejected
  out="$(PATH="$bin:$PATH" SYNDEO_EXPECT_SIGNED=no bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$tgz" 2>&1)"
  expect "inspect: SYNDEO_EXPECT_SIGNED=no, of a package that is signed: fails" eval "printf '%s' \"\$out\" | grep -q 'a Developer ID signature on: syndeo'"
  out="$(env -u MACOS_TEAM_ID SYNDEO_EXPECT_SIGNED=yes bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$tgz" 2>&1)" && status=0 || status=$?
  expect "inspect: SYNDEO_EXPECT_SIGNED=yes without MACOS_TEAM_ID is refused" eval "[ $status = 2 ] && printf '%s' \"\$out\" | grep -q 'needs MACOS_TEAM_ID'"
  out="$(SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID=ab12cd34ef bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$tgz" 2>&1)" && status=0 || status=$?
  expect "inspect: SYNDEO_EXPECT_SIGNED=yes with a malformed MACOS_TEAM_ID is refused" eval "[ $status = 2 ] && printf '%s' \"\$out\" | grep -q 'needs MACOS_TEAM_ID'"
}

self_test() {
  T="$(mktemp -d)"
  # Not local: the EXIT trap runs after this function has returned.
  trap 'chflags -R nouchg "$T" 2>/dev/null; rm -rf "$T"' EXIT
  st_n=0
  st_versions
  mkdir -p "$T/render"
  st_render "$T/render"
  st_harness
  if [ "$(uname -s)" = Darwin ]; then
    st_preinstall
    st_postinstall
    st_atomic
    st_uninstall
    st_installed_mode
    st_location
    st_decisions
    st_inspect
  else
    printf '\n  on %s: the root scripts, the package build and inspect are macOS-only (BSD stat, ls -e, mv -h, pkgbuild) and were not run\n' "$(uname -s)"
  fi
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

# self_test_decisions: only st_decisions, for runs that change one message.
self_test_decisions() {
  T="$(mktemp -d)"
  trap 'chflags -R nouchg "$T" 2>/dev/null; rm -rf "$T"' EXIT
  st_n=0
  [ "$(uname -s)" = Darwin ] || { echo "the decisions are checked on macOS only" >&2; return 2; }
  st_decisions
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test)
    case "${2:-}" in
      '') self_test ;;
      decisions) self_test_decisions ;;
      *) echo "usage: $0 --self-test [decisions]" >&2; exit 2 ;;
    esac
    exit
    ;;
  inspect)
    [ "$#" = 5 ] && [ "$4" = --tarball ] || { echo "usage: $0 inspect <pkg> <version> --tarball <tarball>" >&2; exit 2; }
    inspect_work="$(mktemp -d)"
    trap 'rm -rf "$inspect_work"' EXIT
    inspect "$2" "$3" "$5"
    status=$?
    [ "$status" = 2 ] && exit 2
    report
    [ "$fail" -eq 0 ]
    ;;
  installed)
    # --commands-say: for a stand-in version made of other binaries, what
    # those report instead.
    if [ "$#" = 4 ] && [ "$3" = --commands-say ]; then
      installed "$2" "$4"
    else
      [ "$#" = 2 ] || { echo "usage: $0 installed <version> [--commands-say <version>]" >&2; exit 2; }
      installed "$2"
    fi
    report
    [ "$fail" -eq 0 ]
    ;;
  *) echo "usage: $0 inspect <pkg> <version> --tarball <tarball> | installed <version> [--commands-say <version>] | --self-test [decisions]" >&2; exit 2 ;;
esac
