#!/usr/bin/env bash
# Check Syndeo's macOS installer package, before and after it is installed.
#
#   ci/verify-pkg.sh inspect <pkg> <version> --app <Syndeo.app>
#   ci/verify-pkg.sh package-info <PackageInfo> <version>
#   ci/verify-pkg.sh signed-binaries <tarball> <version>
#   ci/verify-pkg.sh signed-app <Syndeo.app> <version> --tarball <tarball>
#   ci/verify-pkg.sh installed <version> [--helper-says <version>]
#   ci/verify-pkg.sh release sums <dir> <version>
#   ci/verify-pkg.sh release check <draft-dir> <version> --against <dir> \
#       [--notes <expected> <draft's>]
#   ci/verify-pkg.sh --self-test [decisions]
#
# The package installs /Applications/Syndeo.app and nothing else (see
# ci/package-macos-pkg.sh).
#
# inspect looks inside a package without installing it:
# - the name;
# - what the product and the component hold;
# - the product's limits (Apple Silicon, macOS 13, the startup disk only,
#   nothing to customize, no JavaScript);
# - the component, as package-info checks it, and its scripts, byte for byte
#   against the templates;
# - the BOM, path by path with modes and owners: Syndeo.app and nothing else;
# - the payload's bundle, byte for byte against <Syndeo.app>, the bundle the
#   package was built from, and exactly Syndeo.app <version> as
#   ci/package-macos-app.sh checks it, its signature verifying;
# - the helper's --version;
# - the signing state.
#
# package-info checks a component's PackageInfo, as pkgbuild wrote it:
# com.sum.syndeo.app <version>, installed at /Applications as root, with
# overwrite-permissions="false", a preinstall and a postinstall, and one
# bundle, Syndeo.app (com.sum.syndeo <version>): not relocatable, not
# version-checked, upgraded in place under its strict identifier.
# ci/package-macos-pkg.sh checks every package it builds with it.
#
# signed-binaries checks the seven executables of a release tarball, with
# SYNDEO_EXPECT_SIGNED=yes and MACOS_TEAM_ID, and then runs each of them:
# --version has to exit 0 and say exactly "<name> <version>", and
# syndeo-keystore status, in a home of its own, has to report platform
# presence unenforced and the passphrase required. release.yml runs it on the
# signed tarball right after signing and again before the binaries are sent
# to be notarized, so a binary macOS kills at launch (as it killed the
# keystore signed with a keychain access group) fails before any
# notarization submission is spent on it.
#
# signed-app checks the signed Syndeo.app the same way, before it is
# notarized: exactly Syndeo.app <version>, signed; the bundle and its helper
# Developer ID Application of MACOS_TEAM_ID, hardened runtime, secure
# timestamp, no entitlements; the helper byte for byte the tarball's
# syndeo-proxy, and run. The application itself opens a window and is not run
# here: ci/test-pkg-install.sh opens it, installed.
#
# installed checks what an installation left on this Mac, with the checks
# the package's own scripts make, from the same libraries: the receipt for
# com.sum.syndeo.app <version>, listing exactly the bundle's paths; the
# bundle exactly as packaged, its signature verifying; no receipt for the
# discontinued v0.1.x package, com.sum.syndeo.pkg, and nothing of its layout
# in /usr/local; and the helper's --version. --helper-says is for a stand-in
# made of another version's binaries, whose helper reports that version.
#
# release is what release.yml checks a release's assets with. sums requires
# <dir> to hold exactly the three tarballs and the package of <version> and
# nothing else, nothing missing, duplicated or of another version, and writes
# SHA256SUMS over them in a fixed order. check requires a draft's assets, as
# downloaded back, to be exactly those five, each byte-identical to what was
# built, with a SHA256SUMS that verifies, and, given --notes, the draft's
# notes to be the expected ones.
#
# --self-test checks the package's three root scripts, and this script's own
# judgement, against stand-ins in temporary directories, as an ordinary user.
# The scripts are macOS root scripts, written for BSD stat, ls and lsof, so
# their behaviour is checked on macOS only. On Linux the version grammar, the
# comparator, the rendering of the scripts and the release's assets are
# checked. --self-test decisions checks only what the scripts say in each
# real-runner scenario.
#
# Knobs, for inspect:
#   SYNDEO_EXPECT_SIGNED  no (default, and what empty means): the package
#                         carries no signature and Gatekeeper rejects it, and
#                         the bundle and its helper are signed ad hoc, with no
#                         Developer ID. yes: the bundle and its helper are
#                         Developer ID Application of MACOS_TEAM_ID, with the
#                         hardened runtime and a secure timestamp, no
#                         entitlements, and the bundle's notarization ticket
#                         stapled; the package is Developer ID Installer of
#                         that team, chained to Apple Root CA, its ticket
#                         stapled; and Gatekeeper accepts both as Notarized
#                         Developer ID.
#   MACOS_TEAM_ID         with yes, the team every signature has to be of.
#
# For signed-binaries and signed-app, SYNDEO_VERIFY_LAUNCH_SECONDS (60 by
# default) is how long each run may take; the self-test shortens it.
#
# Exits non-zero if any check fails.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
templates="$here/macos-pkg"
builder="$here/package-macos-pkg.sh"
assembler="$here/package-macos-app.sh"
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

# xp FILE XPATH: an XPath value from FILE, or nothing.
xp() { xmllint --xpath "$2" "$1" 2>/dev/null; }

# bundle_state APP: stapled if APP holds a notarization ticket, signed
# otherwise.
bundle_state() { if [ -e "$1/Contents/CodeResources" ]; then echo stapled; else echo signed; fi; }

# expected_bom STATE: every line `lsbom -p fmugl` prints for a package of
# Syndeo.app, signed or stapled: the payload's root, /Applications itself,
# and the bundle, all root:wheel, no link.
expected_bom() {
  local p t m
  printf '.\t40755\t0\t0\t\n./Syndeo.app\t40755\t0\t0\t\n'
  while read -r p t m; do
    if [ "$t" = d ]; then
      printf './Syndeo.app/%s\t40%s\t0\t0\t\n' "$p" "$m"
    else
      printf './Syndeo.app/%s\t100%s\t0\t0\t\n' "$p" "$m"
    fi
  done < <(bash "$assembler" --manifest "$1")
}

# ------------------------------------------------------------------ package-info

# package_info FILE VERSION: the component's PackageInfo is what the design
# rests on.
package_info() {
  local info="$1" version="$2" want got problems=""
  for want in \
    "string(/pkg-info/@identifier)=com.sum.syndeo.app" \
    "string(/pkg-info/@version)=$version" \
    "string(/pkg-info/@install-location)=/Applications" \
    "string(/pkg-info/@overwrite-permissions)=false" \
    "string(/pkg-info/@auth)=root" \
    "count(/pkg-info/scripts/*)=2" \
    "string(/pkg-info/scripts/preinstall/@file)=./preinstall" \
    "string(/pkg-info/scripts/postinstall/@file)=./postinstall" \
    "count(/pkg-info/bundle)=1" \
    "string(/pkg-info/bundle/@path)=./Syndeo.app" \
    "string(/pkg-info/bundle/@id)=com.sum.syndeo" \
    "string(/pkg-info/bundle/@CFBundleShortVersionString)=$version" \
    "count(/pkg-info/bundle/*)=0" \
    "count(/pkg-info/bundle-version/*)=0" \
    "count(/pkg-info/relocate/*)=0" \
    "count(/pkg-info/update-bundle/*)=0" \
    "count(/pkg-info/atomic-update-bundle/*)=0" \
    "count(/pkg-info/upgrade-bundle/bundle)=1" \
    "string(/pkg-info/upgrade-bundle/bundle/@id)=com.sum.syndeo" \
    "count(/pkg-info/strict-identifier/bundle)=1" \
    "string(/pkg-info/strict-identifier/bundle/@id)=com.sum.syndeo"; do
    got="$(xp "$info" "${want%=*}")"
    [ "$got" = "${want##*=}" ] || problems="$problems ${want%=*} is '$got';"
  done
  if [ -z "$problems" ]; then
    ok "com.sum.syndeo.app $version, at /Applications, as root, overwrite-permissions=\"false\", a preinstall and a postinstall; Syndeo.app (com.sum.syndeo $version) not relocatable, not version-checked, upgraded in place under its strict identifier"
  else
    bad "PackageInfo" "$problems"
  fi
}

# ------------------------------------------------------------------ inspect

inspect() {
  local pkg="$1" version="$2" app="$3"
  local expect="${SYNDEO_EXPECT_SIGNED:-no}" team='' name w full dist scripts payload f out state
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
  if [ "$(cd "$full" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "Distribution Syndeo.pkg " ]; then
    ok "the product holds a Distribution and one component, Syndeo.pkg"
  else
    bad "product" "it holds: $(cd "$full" && ls -A | tr '\n' ' ')"
  fi
  if [ "$(cd "$full/Syndeo.pkg" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "Bom PackageInfo Payload Scripts " ]; then
    ok "the component holds a Bom, PackageInfo, Payload and Scripts"
  else
    bad "component" "it holds: $(cd "$full/Syndeo.pkg" && ls -A | tr '\n' ' ')"
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
    "count(/installer-gui-script/pkg-ref[@id!='com.sum.syndeo.app'])=0" \
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
  package_info "$full/Syndeo.pkg/PackageInfo" "$version"
  scripts="$full/Syndeo.pkg/Scripts"
  mkdir "$w/expected"
  "$builder" --render preinstall "$version" '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign "$w/expected/preinstall" >/dev/null
  "$builder" --render postinstall "$version" '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign "$w/expected/postinstall" >/dev/null
  if [ "$(cd "$scripts" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "postinstall preinstall " ]; then
    ok "exactly two scripts, preinstall and postinstall"
  else
    bad "scripts" "found: $(cd "$scripts" && ls -A | tr '\n' ' ')"
  fi
  for f in preinstall postinstall; do
    if cmp -s "$scripts/$f" "$w/expected/$f" && [ "$(stat -f %Lp "$scripts/$f")" = 755 ]; then
      ok "$f is the template with this version, root '', uid 0, gid 0, /usr/sbin/pkgutil and /usr/bin/codesign, mode 0755"
    else
      bad "$f" "differs from the template rendered for $version, or is not mode 0755"
    fi
  done

  note "the payload: Syndeo.app, and nothing else"
  payload="$full/Syndeo.pkg/Payload"
  state="$(bundle_state "$payload/Syndeo.app")"
  expected_bom "$state" | LC_ALL=C sort >"$w/bom.want"
  lsbom -p fmugl "$full/Syndeo.pkg/Bom" | LC_ALL=C sort >"$w/bom.have"
  if cmp -s "$w/bom.want" "$w/bom.have"; then
    ok "the BOM: exactly Syndeo.app ($state), all root:wheel, directories and executables 0755, everything else 0644, no link"
  else
    bad "BOM" "$(diff "$w/bom.want" "$w/bom.have" | grep '^[<>]' | head -8 | tr '\n' ' ')"
  fi
  if [ "$(cd "$payload" && ls -A | tr '\n' ' ')" = "Syndeo.app " ]; then
    ok "the payload holds Syndeo.app alone: nothing for /usr/local, no command, no link"
  else
    bad "payload" "it holds: $(cd "$payload" && ls -A | tr '\n' ' ')"
  fi
  if out="$(bash "$assembler" --check "$payload/Syndeo.app" "$version" "$state" 2>&1)"; then
    ok "it is exactly Syndeo.app $version, $state, its uninstaller the template rendered for $version"
  else
    bad "bundle" "$(printf '%s' "$out" | tr '\n' ' ')"
  fi
  if [ "$(bundle_tree "$payload/Syndeo.app")" = "$(bundle_tree "$app")" ] && same_bytes "$payload/Syndeo.app" "$app"; then
    ok "byte for byte the bundle it was built from, $app"
  else
    bad "provenance" "the payload's Syndeo.app differs from $app"
  fi
  if codesign --verify --strict --deep "$payload/Syndeo.app" >/dev/null 2>&1; then
    ok "its code signature verifies, strictly, with everything nested in it"
  else
    bad "bundle signature" "codesign --verify --strict --deep fails on the payload's Syndeo.app"
  fi
  out="$(bounded "${SYNDEO_VERIFY_LAUNCH_SECONDS:-60}" "$payload/Syndeo.app/Contents/Helpers/syndeo-proxy" --version)"
  if [ "$out" = "syndeo-proxy $version" ]; then
    ok "its helper says syndeo-proxy $version"
  else
    bad "helper" "--version said '$(printf '%s' "$out" | head -c 120 | tr '\n' '|')', not 'syndeo-proxy $version'"
  fi

  if [ "$expect" = yes ]; then
    signed_state "$pkg" "$payload/Syndeo.app" "$team"
    return 0
  fi
  note "signing (expected: none on the package; the bundle ad hoc)"
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
  for f in "$payload/Syndeo.app" "$payload/Syndeo.app/Contents/Helpers/syndeo-proxy"; do
    out="$(codesign -dv --verbose=2 "$f" 2>&1)"
    if printf '%s\n' "$out" | grep -q '^Authority=Developer ID'; then
      problems="$problems ${f##*/} carries a Developer ID;"
    elif ! printf '%s\n' "$out" | grep -q '^Signature=adhoc'; then
      problems="$problems ${f##*/} is not signed ad hoc;"
    fi
  done
  if [ -z "$problems" ]; then
    ok "the bundle and its helper are signed ad hoc, with no Developer ID"
  else
    bad "bundle signing" "${problems# }"
  fi
}

# bundle_tree APP: every path in APP, with its type and mode, sorted.
bundle_tree() {
  (cd "$1" 2>/dev/null && find . -exec stat -f '%N %HT %Lp' {} + | LC_ALL=C sort)
}

# same_bytes A B: every regular file in A is byte-identical to the same path
# in B. With bundle_tree equal, that is the same bundle.
same_bytes() {
  local f
  while IFS= read -r f; do
    cmp -s "$1/$f" "$2/$f" || return 1
  done < <(cd "$1" && find . -type f | LC_ALL=C sort)
}

# A team identifier: ten capital letters and digits, whatever the locale.
TEAM_RE='^[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10}$'

# entitlements_are XML GROUP: XML, what codesign printed as the entitlements,
# is exactly keychain-access-groups = [GROUP], or, with GROUP empty, nothing.
# Every shipped executable is checked with GROUP empty: a restricted
# entitlement, such as the keychain access group the keystore once claimed,
# gets a standalone executable killed at launch.
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

# code_problems PATH TEAM [--deep]: what keeps PATH from being signed as a
# release has to be: Developer ID Application of TEAM, chained to Apple Root
# CA, with the hardened runtime and a secure timestamp, and no entitlement.
# Prints nothing if it is.
code_problems() {
  local path="$1" team="$2" deep="${3:-}" out first last problems=""
  # shellcheck disable=SC2086
  codesign --verify --strict $deep "$path" >/dev/null 2>&1 || problems="$problems does not verify;"
  out="$(codesign -dv --verbose=4 "$path" 2>&1)"
  first="$(printf '%s\n' "$out" | sed -n 's/^Authority=//p' | head -n 1)"
  last="$(printf '%s\n' "$out" | sed -n 's/^Authority=//p' | tail -n 1)"
  developer_id Application "$first" "$team" || problems="$problems signed by '${first:-nobody}';"
  [ "$last" = "Apple Root CA" ] || problems="$problems not chained to Apple Root CA;"
  printf '%s\n' "$out" | grep -qx "TeamIdentifier=$team" ||
    problems="$problems TeamIdentifier '$(printf '%s\n' "$out" | sed -n 's/^TeamIdentifier=//p')';"
  printf '%s\n' "$out" | grep -qE '^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*runtime[^)]*\)' || problems="$problems no hardened runtime;"
  printf '%s\n' "$out" | grep -q '^Timestamp=' || problems="$problems no secure timestamp;"
  entitlements_are "$(codesign -d --entitlements - --xml "$path" 2>/dev/null)" "" ||
    problems="$problems has entitlements, and no shipped executable may;"
  printf '%s' "${problems# }"
}

# signed_executables TREE TEAM: each of the seven executables in TREE is
# signed as a release has to be.
signed_executables() {
  local tree="$1" team="$2" n problems
  for n in $NAMES; do
    problems="$(code_problems "$tree/$n" "$team")"
    if [ -z "$problems" ]; then
      ok "$n: Developer ID Application of $team, hardened runtime, secure timestamp, no entitlements"
    else
      bad "$n" "$problems"
    fi
  done
}

# signed_bundle APP TEAM: Syndeo.app and its helper are signed as a release
# has to be, the bundle as com.sum.syndeo and verifying with everything
# nested in it.
signed_bundle() {
  local app="$1" team="$2" problems id
  problems="$(code_problems "$app" "$team" --deep)"
  id="$(codesign -dv --verbose=4 "$app" 2>&1 | sed -n 's/^Identifier=//p')"
  [ "$id" = com.sum.syndeo ] || problems="${problems:+$problems }signed as '$id', not com.sum.syndeo;"
  if [ -z "$problems" ]; then
    ok "Syndeo.app: com.sum.syndeo, Developer ID Application of $team, hardened runtime, secure timestamp, no entitlements, verifying deeply"
  else
    bad "Syndeo.app" "$problems"
  fi
  problems="$(code_problems "$app/Contents/Helpers/syndeo-proxy" "$team")"
  if [ -z "$problems" ]; then
    ok "its helper, syndeo-proxy: Developer ID Application of $team, hardened runtime, secure timestamp, no entitlements"
  else
    bad "syndeo-proxy, the helper" "$problems"
  fi
}

# accepted PATH TYPE ORIGIN: Gatekeeper accepts PATH, assessed as TYPE
# (install or execute), as Notarized Developer ID from ORIGIN. Prints what
# is wrong, or nothing.
accepted() {
  local path="$1" type="$2" origin="$3" out problems
  out="$(spctl -a -vv -t "$type" "$path" 2>&1)" && problems='' || problems=" rejected;"
  printf '%s\n' "$out" | grep -qxF "$path: accepted" || problems="${problems:- not accepted;}"
  printf '%s\n' "$out" | grep -qx 'source=Notarized Developer ID' || problems="$problems $(printf '%s\n' "$out" | grep -m1 '^source=' || echo 'no source');"
  printf '%s\n' "$out" | grep -qxF "origin=$origin" || problems="$problems from another origin;"
  printf '%s' "${problems# }"
}

# signed_state PKG APP TEAM: what a signed release has to be, checked on the
# package and on the bundle in its payload, which inspect has already found
# byte-identical to the one it was built from.
signed_state() {
  local pkg="$1" app="$2" team="$3" out first last problems chain origin
  note "signing (expected: Developer ID of $team, notarized and stapled)"
  signed_bundle "$app" "$team"
  if xcrun stapler validate "$app" >/dev/null 2>&1; then
    ok "Syndeo.app's notarization ticket is stapled, and validates"
  else
    bad "Syndeo.app's notarization ticket" "stapler validate fails"
  fi
  origin="$(codesign -dv --verbose=4 "$app" 2>&1 | sed -n 's/^Authority=//p' | head -n 1)"
  problems="$(accepted "$app" execute "$origin")"
  if [ -z "$problems" ]; then
    ok "Gatekeeper accepts Syndeo.app to run: Notarized Developer ID, from the same Application identity"
  else
    bad "Gatekeeper, Syndeo.app" "$problems"
  fi

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
  problems="$(accepted "$pkg" install "$first")"
  if [ -z "$problems" ]; then
    ok "Gatekeeper accepts it: Notarized Developer ID, from the same Installer identity"
  else
    bad "Gatekeeper" "$problems"
  fi
}

# ------------------------------------------------------------------ release

# release_assets V: the four payload assets of V, in SHA256SUMS's order: the
# three tarballs, then the package.
release_assets() {
  printf '%s\n' "syndeo-$1-aarch64-apple-darwin.tar.gz" "syndeo-$1-aarch64-unknown-linux-gnu.tar.gz" \
    "syndeo-$1-x86_64-unknown-linux-gnu.tar.gz" "syndeo-$1-aarch64-apple-darwin.pkg"
}

# exactly DIR NAMES: DIR holds the regular, non-empty files NAMES (one per
# line), and nothing else.
exactly() {
  local dir="$1" want="$2" have f problems=""
  have="$(cd "$dir" && ls -A | LC_ALL=C sort)"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    printf '%s\n' "$want" | grep -qxF -- "$f" || problems="$problems unexpected $f;"
  done <<<"$have"
  while IFS= read -r f; do
    if [ ! -e "$dir/$f" ]; then
      problems="$problems missing $f;"
    elif [ -L "$dir/$f" ] || [ ! -f "$dir/$f" ] || [ ! -s "$dir/$f" ]; then
      problems="$problems $f is not a non-empty regular file;"
    fi
  done <<<"$want"
  if [ -z "$problems" ]; then
    ok "exactly:$(printf '%s\n' "$want" | sed 's/^/ /' | tr -d '\n')"
  else
    bad "assets" "${problems# }"
  fi
}

release_sums() {
  local dir="$1" v="$2" want
  syndeo_version_valid "$v" || { echo "release: '$v' is not a version" >&2; return 2; }
  want="$(release_assets "$v")"
  note "the release's assets, before SHA256SUMS"
  exactly "$dir" "$want"
  [ "$fail" -eq 0 ] || return 1
  (cd "$dir" && printf '%s\n' "$want" | while IFS= read -r f; do shasum -a 256 -- "$f"; done) >"$dir/SHA256SUMS.new" &&
    mv "$dir/SHA256SUMS.new" "$dir/SHA256SUMS"
  ok "SHA256SUMS written over the four, in order"
  sed 's/^/      /' "$dir/SHA256SUMS"
}

release_check() {
  local draft="$1" v="$2" built="$3" want_notes="${4:-}" have_notes="${5:-}" want f listed
  syndeo_version_valid "$v" || { echo "release: '$v' is not a version" >&2; return 2; }
  want="$(release_assets "$v")"
  note "the draft's assets, downloaded back"
  exactly "$draft" "$(printf '%s\nSHA256SUMS\n' "$want" | LC_ALL=C sort)"
  for f in $want SHA256SUMS; do
    [ -f "$draft/$f" ] || continue
    if cmp -s "$draft/$f" "$built/$f"; then
      ok "$f: $(wc -c <"$draft/$f" | tr -d ' ') bytes, identical to what was built"
    else
      bad "$f" "differs from what was built ($(wc -c <"$draft/$f" | tr -d ' ') bytes, built $(wc -c <"$built/$f" 2>/dev/null | tr -d ' ') bytes)"
    fi
  done
  if [ -f "$draft/SHA256SUMS" ]; then
    listed="$(sed 's/^[0-9a-f]\{64\}  //' "$draft/SHA256SUMS")"
    if [ "$listed" = "$want" ] && [ "$(grep -cE '^[0-9a-f]{64}  ' "$draft/SHA256SUMS")" = 4 ] &&
      (cd "$draft" && shasum -a 256 -c SHA256SUMS >/dev/null 2>&1); then
      ok "SHA256SUMS lists the four, in order, and every one verifies"
    else
      bad "SHA256SUMS" "does not list exactly the four, in order, or one does not verify"
    fi
  fi
  if [ -n "$want_notes" ]; then
    note "the draft's notes"
    if [ "$(cat "$want_notes")" = "$(cat "$have_notes" 2>/dev/null)" ]; then
      ok "exactly the notes ci/release-notes.sh wrote"
    else
      bad "notes" "differ from the expected: $(diff <(cat "$want_notes") <(cat "$have_notes" 2>/dev/null) | grep '^[<>]' | head -3 | tr '\n' ' ')"
    fi
  fi
}

# bounded SECONDS COMMAND...: COMMAND, with stdin closed, killed by SIGALRM if
# it runs longer than SECONDS; its stdout and stderr together, and its exit
# status, are what the caller captures.
bounded() {
  perl -e 'alarm shift @ARGV; exec @ARGV or die "exec: $!\n"' "$@" </dev/null 2>&1
}

# launch_problem STATUS OUT WANT SECONDS: what was wrong with a run that did
# not exit 0 saying exactly WANT.
launch_problem() {
  case "$1" in
    0) printf "said '%s', not %s" "$(printf '%s' "$2" | head -c 120 | tr '\n' '|')" "$3" ;;
    142) printf 'did not finish in %s seconds' "$4" ;;
    1[3-9][0-9] | 2[0-5][0-9]) printf 'killed by signal %s at launch' "$(($1 - 128))" ;;
    *) printf "exited %s, saying '%s'" "$1" "$(printf '%s' "$2" | head -c 120 | tr '\n' '|')" ;;
  esac
}

# launched TREE VERSION: each of the seven executables in TREE, run. Signed
# alike and checked alike is not enough: macOS can still refuse to run a
# signed binary, and does so by killing it as it starts.
launched() {
  local tree="$1" v="$2" n out status home seconds="${SYNDEO_VERIFY_LAUNCH_SECONDS:-60}"
  note "each signed executable, run (each within $seconds seconds)"
  for n in $NAMES; do
    out="$(bounded "$seconds" "$tree/$n" --version)" && status=0 || status=$?
    if [ "$status" = 0 ] && [ "$out" = "$n $v" ]; then
      ok "$n --version: exits 0 and says '$n $v'"
    else
      bad "$n" "--version: $(launch_problem "$status" "$out" "'$n $v'" "$seconds")"
    fi
  done
  # The keystore, asked what it enforces, in a home that holds nothing: the
  # ordinary keychain, presence unenforced, the passphrase required.
  home="$(mktemp -d "$inspect_work/keystore-home.XXXXXX")"
  out="$(SYNDEO_HOME="$home" HOME="$home" bounded "$seconds" "$tree/syndeo-keystore" status)" && status=0 || status=$?
  if [ "$status" = 0 ] && printf '%s\n' "$out" | grep -qx 'presence enforced    false' &&
    printf '%s\n' "$out" | grep -qx 'passphrase required  true'; then
    ok "syndeo-keystore status: platform presence unenforced, the passphrase required"
  elif [ "$status" = 0 ]; then
    bad "syndeo-keystore status" "does not report platform presence unenforced and the passphrase required: $(printf '%s' "$out" | grep -E '^(presence enforced|passphrase required)' | tr '\n' ';')"
  else
    bad "syndeo-keystore status" "$(launch_problem "$status" "$out" "a status" "$seconds")"
  fi
}

# signed_binaries TARBALL VERSION: the release tarball's executables, signed
# and ready to be notarized, and each of them run. Requires
# SYNDEO_EXPECT_SIGNED=yes and MACOS_TEAM_ID.
signed_binaries() {
  local tarball="$1" v="$2" team="${MACOS_TEAM_ID:-}" name
  [ "${SYNDEO_EXPECT_SIGNED:-}" = yes ] || { echo "signed-binaries needs SYNDEO_EXPECT_SIGNED=yes" >&2; return 2; }
  [[ "$team" =~ $TEAM_RE ]] || { echo "signed-binaries needs MACOS_TEAM_ID, ten capital letters and digits" >&2; return 2; }
  syndeo_version_valid "$v" || { echo "signed-binaries: '$v' is not a version" >&2; return 2; }
  name="syndeo-$v-aarch64-apple-darwin"
  note "the signed binaries of $name.tar.gz (expected: Developer ID Application of $team)"
  if [ "${tarball##*/}" != "$name.tar.gz" ]; then
    bad "name" "${tarball##*/} is not $name.tar.gz"
    return 0
  fi
  mkdir "$inspect_work/binaries"
  if ! tar -xzf "$tarball" -C "$inspect_work/binaries" 2>/dev/null; then
    bad "tarball" "could not unpack $tarball"
    return 0
  fi
  signed_executables "$inspect_work/binaries/$name" "$team"
  launched "$inspect_work/binaries/$name" "$v"
}

# ------------------------------------------------------------------ signed-app

# signed_app APP VERSION TARBALL: the signed Syndeo.app, before it is
# notarized: exactly the bundle, the bundle and its helper signed as a
# release has to be, and the helper the tarball's own syndeo-proxy, run.
# Requires SYNDEO_EXPECT_SIGNED=yes and MACOS_TEAM_ID.
signed_app() {
  local app="$1" v="$2" tarball="$3" team="${MACOS_TEAM_ID:-}" name out status seconds="${SYNDEO_VERIFY_LAUNCH_SECONDS:-60}"
  [ "${SYNDEO_EXPECT_SIGNED:-}" = yes ] || { echo "signed-app needs SYNDEO_EXPECT_SIGNED=yes" >&2; return 2; }
  [[ "$team" =~ $TEAM_RE ]] || { echo "signed-app needs MACOS_TEAM_ID, ten capital letters and digits" >&2; return 2; }
  syndeo_version_valid "$v" || { echo "signed-app: '$v' is not a version" >&2; return 2; }
  name="syndeo-$v-aarch64-apple-darwin"
  note "the signed Syndeo.app $v (expected: Developer ID Application of $team)"
  if out="$(bash "$assembler" --check "$app" "$v" signed 2>&1)"; then
    ok "exactly Syndeo.app $v, signed, its uninstaller the template rendered for $v"
  else
    bad "bundle" "$(printf '%s' "$out" | tr '\n' ' ')"
  fi
  signed_bundle "$app" "$team"
  mkdir "$inspect_work/binaries"
  if [ "${tarball##*/}" != "$name.tar.gz" ] || ! tar -xzf "$tarball" -C "$inspect_work/binaries" 2>/dev/null; then
    bad "tarball" "${tarball##*/} is not $name.tar.gz, or could not be unpacked"
  elif cmp -s "$app/Contents/Helpers/syndeo-proxy" "$inspect_work/binaries/$name/syndeo-proxy"; then
    ok "its helper is byte for byte the tarball's syndeo-proxy"
  else
    bad "helper" "differs from the tarball's syndeo-proxy"
  fi
  out="$(bounded "$seconds" "$app/Contents/Helpers/syndeo-proxy" --version)" && status=0 || status=$?
  if [ "$status" = 0 ] && [ "$out" = "syndeo-proxy $v" ]; then
    ok "its helper, run: --version exits 0 and says 'syndeo-proxy $v'"
  else
    bad "helper" "--version: $(launch_problem "$status" "$out" "'syndeo-proxy $v'" "$seconds")"
  fi
}

# ------------------------------------------------------------------ installed

# The libraries set and read these; shellcheck cannot follow a generated one.
# shellcheck disable=SC2034,SC2154
installed() {
  local version="$1" says="${2:-$1}" n p out lib
  # The same checks the package's scripts make, from the same libraries. The
  # SYNDEO_VERIFY_* knobs exist for the self-test, which installs nothing.
  ROOT="${SYNDEO_VERIFY_ROOT:-}"
  EXPECT_UID="${SYNDEO_VERIFY_UID:-0}"
  EXPECT_GID="${SYNDEO_VERIFY_GID:-0}"
  PKGUTIL="${SYNDEO_VERIFY_PKGUTIL:-/usr/sbin/pkgutil}"
  CODESIGN="${SYNDEO_VERIFY_CODESIGN:-/usr/bin/codesign}"
  SCRIPT=verify
  lib="$inspect_work/libraries.sh"
  "$builder" --library "$lib" || { bad "libraries" "could not be written"; return 0; }
  # shellcheck source=/dev/null
  . "$lib"

  note "the receipt"
  read_app_receipt
  if [ "$ar_present" != yes ]; then
    bad "receipt" "there is no receipt for $APP_RECEIPT"
    return 0
  fi
  check_app_receipt_meta
  if [ "$ar_version" = "$version" ] && [ "$nfindings" = 0 ]; then
    ok "$APP_RECEIPT $version, for /, in Applications, listing exactly Syndeo.app's paths"
  else
    bad "receipt" "version '$ar_version';$(printf '%s' "$findings" | tr '\n' ' ')"
  fi

  note "the bundle"
  findings=''; nfindings=0
  check_app_bundle "$version"
  if [ "$nfindings" = 0 ] && [ -n "$ar_ticket" ] && [ "$app_ticket" != "$ar_ticket" ]; then
    finding "$APP: its stapled ticket and its receipt disagree"
  fi
  if [ "$nfindings" = 0 ]; then
    ok "$APP is exactly Syndeo.app $version as packaged, every path the receipt lists, its signature verifying as $APP_BUNDLE_ID"
  else
    bad "bundle" "$(printf '%s' "$findings" | tr '\n' ' ')"
  fi

  note "the discontinued v0.1.x command-line package"
  read_receipt
  if [ "$r_present" = no ]; then
    ok "no receipt for $SYNDEO_ID"
  else
    bad "v0.1.x receipt" "$SYNDEO_ID $r_version is still recorded"
  fi
  p=''
  is_present "$SYNDEO_DIR" && p="$p $SYNDEO_DIR"
  for n in $SYNDEO_NAMES; do
    if [ -L "$SYNDEO_BIN/$n" ] && [ "$(readlink "$SYNDEO_BIN/$n")" = "../libexec/syndeo/current/$n" ]; then
      p="$p $SYNDEO_BIN/$n"
    fi
  done
  if [ -z "$p" ]; then
    ok "nothing of its layout in /usr/local: no $SYNDEO_DIR, and no link into it in $SYNDEO_BIN"
  else
    bad "v0.1.x layout" "still there:$p"
  fi

  note "the helper, run"
  out="$(bounded "${SYNDEO_VERIFY_LAUNCH_SECONDS:-60}" "$APP/Contents/Helpers/syndeo-proxy" --version)"
  if [ "$out" = "syndeo-proxy $says" ]; then
    ok "$APP/Contents/Helpers/syndeo-proxy says syndeo-proxy $says"
  else
    bad "helper" "said '$(printf '%s' "$out" | head -c 120 | tr '\n' '|')', not 'syndeo-proxy $says'"
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
    "$builder" --render "$k" 0.1.7 '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign "$d/$k" >/dev/null 2>&1
    expect "$k renders" test -s "$d/$k"
    expect "$k: no placeholder left" eval "! grep -q '@[A-Z_]*@' '$d/$k'"
    expect "$k: root '', uid 0, gid 0, /usr/sbin/pkgutil, /usr/bin/codesign" eval "grep -qx \"ROOT=''\" '$d/$k' && grep -qx \"EXPECT_UID='0'\" '$d/$k' && grep -qx \"EXPECT_GID='0'\" '$d/$k' && grep -qx \"PKGUTIL='/usr/sbin/pkgutil'\" '$d/$k' && grep -qx \"CODESIGN='/usr/bin/codesign'\" '$d/$k'"
    expect "$k: the comparator, the v0.1.x checks and the app's, once each" eval "[ \"\$(grep -c '^syndeo_version_cmp() {' '$d/$k')\" = 1 ] && [ \"\$(grep -c '^check_tree() {' '$d/$k')\" = 1 ] && [ \"\$(grep -c '^check_app_bundle() {' '$d/$k')\" = 1 ]"
    expect "$k: the bundle's manifest embedded, whole" test "$(awk '/^app_manifest\(\) \{/ { on = 1 } on && /^Contents[^ ]* [df] [0-9][0-9][0-9]$/ { n++ } on && /^}/ { on = 0 } END { print n + 0 }' "$d/$k")" = 12
    expect "$k: POSIX sh parses it" sh -n "$d/$k"
  done
  expect "preinstall carries the incoming version" grep -qx "INCOMING='0.1.7'" "$d/preinstall"
  expect "uninstall.sh carries its own version" grep -qx "EMBEDDED='0.1.7'" "$d/uninstall"
  expect "the builder has no test fault to put in a package" eval "! '$builder' --render postinstall 0.0.3 '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign '$d/x' --test-fault postinstall-fails >/dev/null 2>&1"
  expect "a root with a quote is refused" eval "! '$builder' --render preinstall 0.1.7 \"/tmp/it's\" 0 0 /usr/sbin/pkgutil /usr/bin/codesign '$d/x' >/dev/null 2>&1"
  expect "a relative pkgutil is refused" eval "! '$builder' --render preinstall 0.1.7 '' 0 0 pkgutil /usr/bin/codesign '$d/x' >/dev/null 2>&1"
  expect "a relative codesign is refused" eval "! '$builder' --render preinstall 0.1.7 '' 0 0 /usr/sbin/pkgutil codesign '$d/x' >/dev/null 2>&1"
  expect "a malformed version is refused" eval "! '$builder' --render preinstall 0.1 '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign '$d/x' >/dev/null 2>&1"
  expect "--library: the three libraries with the manifest, nothing left to fill in" eval "'$builder' --library '$d/lib' && ! grep -q '@[A-Z_]*@' '$d/lib' && grep -qx 'Contents/Resources/uninstall.sh f 755' '$d/lib' && sh -n '$d/lib'"
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
# otherwise only a real install exercises. Every installer it runs is asked
# for -dumplog, whose output is kept per invocation. note_said() looks there
# first, for the line however installer prefixes it, then in what
# /var/log/install.log gains, allowing for it to be late; and it decides
# nothing: a line seen nowhere is recorded as such, and the run goes on,
# judged on what is installed.
st_harness() {
  printf '\n  the install test: what a package said, noted\n'
  local runs out ok d
  runs="$(grep -nE '(^|[^-[:alnum:]_./])(/usr/sbin/)?installer +-' "$here/test-pkg-install.sh" | grep -vE '^[0-9]+: *#')"
  ok=no
  if [ -n "$runs" ] && ! printf '%s\n' "$runs" | grep -qv -- ' -dumplog '; then ok=yes; fi
  expect "test-pkg-install.sh runs installer, and always with -dumplog" test "$ok" = yes
  ok=no
  if ! grep -nE '(^|[^_[:alnum:]])said "' "$here/test-pkg-install.sh" | grep -qv 'note_said'; then ok=yes; fi
  expect "test-pkg-install.sh decides nothing on a logged line: only note_said remains" test "$ok" = yes

  local decision='syndeo preinstall: upgrading from 0.0.1 to 0.0.2'
  local prefixed="2026-10-10 00:53:34+00 runner package_script_service[42591]: ./preinstall: $decision"

  out="$(st_said_run "installer: Package name is Syndeo 0.0.2
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

  out="$(st_said_run "installer: Package name is Syndeo 0.0.2
./preinstall: syndeo preinstall: upgrading from 0.0.9 to 0.0.2
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
# holding usr/local and Applications; DB the stand-in receipts; A the
# bundle's path; D the v0.1.x private directory; U and G the expected owner.
st_new() {
  C="$T/case-$((++st_n))"
  R="$C/root with space"
  DB="$C/receipts"
  D="$R/usr/local/libexec/syndeo"
  A="$R/Applications/Syndeo.app"
  mkdir -p "$R/usr/local" "$R/Applications" "$DB"
  chmod 755 "$R" "$R/usr" "$R/usr/local" "$R/Applications"
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
    [ -f "$db/file-info-fails" ] && exit 1
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
  # A stand-in for codesign, for bundles only. A bundle is signed when its
  # Contents/_CodeSignature/CodeResources holds an identifier and a digest of
  # every path in it, but the two a signature and a ticket write, and of every
  # file's bytes: any change to a file, and any path added or removed, breaks
  # it.
  cat >"$C/codesign" <<'EOF'
#!/bin/sh
seal() {
  ( cd "$1" && /usr/bin/find . ! -path ./Contents/_CodeSignature/CodeResources ! -path ./Contents/CodeResources |
    LC_ALL=C /usr/bin/sort | while IFS= read -r f; do
      if [ -f "$f" ] && [ ! -L "$f" ]; then
        printf '%s %s\n' "$f" "$(/usr/bin/shasum -a 256 <"$f" | /usr/bin/cut -c1-64)"
      else
        printf '%s %s\n' "$f" "$(/usr/bin/stat -f %HT "$f")"
      fi
    done ) | /usr/bin/shasum -a 256 | /usr/bin/cut -c1-64
}
for last in "$@"; do :; done
sig="$last/Contents/_CodeSignature/CodeResources"
case "$1" in
  --verify)
    [ -f "$sig" ] && [ "$(/usr/bin/sed -n 2p "$sig")" = "seal $(seal "$last")" ] ;;
  --display)
    [ -f "$sig" ] || { echo "$last: code object is not signed at all" >&2; exit 1; }
    echo "Executable=$last/Contents/MacOS/Syndeo" >&2
    echo "Identifier=$(/usr/bin/sed -n 's/^identifier //p' "$sig")" >&2 ;;
  --stand-in-sign)
    /bin/mkdir -p "$last/Contents/_CodeSignature"
    : >"$sig"
    /bin/chmod 755 "$last/Contents/_CodeSignature"
    /bin/chmod 644 "$sig"
    printf 'identifier %s\nseal %s\n' "$2" "$(seal "$last")" >"$sig" ;;
  *) exit 64 ;;
esac
EOF
  chmod 755 "$C/pkgutil" "$C/codesign"
}

st_render_for() { "$builder" --render "$1" "$2" "$R" "$U" "$G" "$C/pkgutil" "$C/codesign" "$3" >/dev/null; }

# st_app V [TICKET [IDENTIFIER]]: Syndeo.app V in the case's Applications,
# laid out as the package installs it, its uninstaller rendered for this
# case, and signed by the stand-in as IDENTIFIER (com.sum.syndeo); TICKET yes
# staples a ticket.
st_app() {
  local v="$1" ticket="${2:-no}" id="${3:-com.sum.syndeo}"
  mkdir -p "$A/Contents/MacOS" "$A/Contents/Helpers" "$A/Contents/Resources"
  sed "s/@VERSION@/$v/g" "$here/macos-app/Info.plist.in" >"$A/Contents/Info.plist"
  printf 'APPL????' >"$A/Contents/PkgInfo"
  printf '#!/bin/sh\necho "Syndeo %s"\n' "$v" >"$A/Contents/MacOS/Syndeo"
  printf '#!/bin/sh\necho "syndeo-proxy %s"\n' "$v" >"$A/Contents/Helpers/syndeo-proxy"
  printf 'icon' >"$A/Contents/Resources/Syndeo.icns"
  st_render_for uninstall "$v" "$A/Contents/Resources/uninstall.sh"
  find "$A" -type d -exec chmod 755 {} +
  find "$A" -type f -exec chmod 644 {} +
  chmod 755 "$A/Contents/MacOS/Syndeo" "$A/Contents/Helpers/syndeo-proxy" "$A/Contents/Resources/uninstall.sh"
  st_sign "$id"
  if [ "$ticket" = yes ]; then
    printf 'ticket' >"$A/Contents/CodeResources"
    chmod 644 "$A/Contents/CodeResources"
  fi
}

# st_sign [IDENTIFIER]: sign the bundle again, as it now is.
st_sign() { "$C/codesign" --stand-in-sign "${1:-com.sum.syndeo}" "$A"; }

# st_app_receipt V [TICKET [LOCATION]]: the receipt for Syndeo.app V, listing
# the bundle's paths, the ticket with TICKET yes. LOCATION is its location
# lines, none if empty; by default the one pkgutil prints for it.
st_app_receipt() {
  local v="$1" ticket="${2:-no}" loc="${3-location: Applications}" p t m state=signed
  [ "$ticket" = yes ] && state=stapled
  {
    printf 'package-id: com.sum.syndeo.app\nversion: %s\nvolume: /\n' "$v"
    [ -z "$loc" ] || printf '%s\n' "$loc"
    printf 'install-time: 1\n'
  } >"$DB/com.sum.syndeo.app.info"
  {
    echo Syndeo.app
    bash "$assembler" --manifest "$state" | while read -r p t m; do echo "Syndeo.app/$p"; done
  } >"$DB/com.sum.syndeo.app.files"
}

# st_app_installed V [TICKET]: what a completed installation of V leaves.
st_app_installed() { st_app "$1" "${2:-no}"; st_app_receipt "$1" "${2:-no}"; }

st_dirs() {
  mkdir -p "$D"
  chmod 755 "$R/usr/local/libexec" "$D"
}

# st_tree V [subset]: a v0.1.x version tree as its package lays it down.
st_tree() {
  local v="$1" t n
  st_dirs
  t="$D/$v"
  mkdir -p "$t/tools"
  chmod 755 "$t" "$t/tools"
  for n in $NAMES uninstall.sh; do
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
}

st_bin() {
  mkdir -p "$R/usr/local/bin"
  chmod 755 "$R/usr/local/bin"
}

st_links() {
  local n
  st_bin
  for n in $NAMES; do
    ln -s "../libexec/syndeo/current/$n" "$R/usr/local/bin/$n"
  done
}

st_current() { ln -s "$1" "$D/current"; }

# st_receipt V [LOCATION]: the v0.1.x receipt for V. LOCATION is its location
# lines, none if empty; by default the one pkgutil prints for that package,
# whose location relative to the volume is empty.
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

# st_legacy V [LOCATION]: what a completed v0.1.x installation of V leaves.
st_legacy() { st_tree "$1"; st_links; st_current "$1"; st_receipt "$@"; }

# st_legacy_steps V: the v0.1.x paths in the order the postinstall removes
# them, written out here rather than taken from the library.
st_legacy_steps() {
  local n
  for n in $NAMES; do echo "$R/usr/local/bin/$n"; done
  echo "$D/current"
  for n in $NAMES README.md LICENSE tools/wordcount.wat tools uninstall.sh; do echo "$D/$1/$n"; done
  echo "$D/$1"
  echo "$D"
}

# st_legacy_cut V K: the first K of those removed, as the postinstall would
# have removed them before it stopped.
st_legacy_cut() {
  local p
  while IFS= read -r p; do
    if [ -d "$p" ] && [ ! -L "$p" ]; then rmdir "$p"; else rm -f "$p"; fi
  done < <(st_legacy_steps "$1" | head -n "$2")
}

# st_legacy_left V: how many of the v0.1.x paths are still there.
st_legacy_left() {
  local p n=0
  while IFS= read -r p; do
    if [ -e "$p" ] || [ -L "$p" ]; then n=$((n + 1)); fi
  done < <(st_legacy_steps "$1")
  echo "$n"
}

# st_hold app|helper: the bundle's application or helper replaced by a
# program that waits, the bundle signed again, and the program started from
# there. held is its process ID; st_unhold stops it.
st_hold() {
  local f
  case "$1" in
    app) f="$A/Contents/MacOS/Syndeo" ;;
    helper) f="$A/Contents/Helpers/syndeo-proxy" ;;
  esac
  cp "$T/hold" "$f"
  chmod 755 "$f"
  st_sign
  "$f" 120 &
  held=$!
  echo "$held" >>"$T/held"
  for _ in $(seq 1 50); do
    [ -n "$(/usr/sbin/lsof -t -- "$f" 2>/dev/null)" ] && return 0
    sleep 0.1
  done
  return 1
}

st_unhold() {
  kill "$held" 2>/dev/null
  wait "$held" 2>/dev/null
  return 0
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
  local p='refusing:'
  printf '\n  preinstall: the app, with no receipt\n'
  st_new; pre_case "nothing installed" 0 "installing 0.1.7" 0.1.7
  st_new; rm -rf "${R:?}/usr/local"; pre_case "no /usr/local at all" 0 "installing 0.1.7" 0.1.7
  st_new; rmdir "$R/Applications"; pre_case "refused: no /Applications" 1 "$R/Applications: is a path that cannot be read, not a directory" 0.1.7
  st_new; rmdir "$R/Applications"; mkdir "$C/apps"; ln -s "$C/apps" "$R/Applications"
  pre_case "refused: /Applications is a link" 1 "$R/Applications: is a Symbolic Link, not a directory" 0.1.7
  st_new; st_app 0.1.7; pre_case "the bundle of this version, no receipt: an interrupted installation" 0 "resuming an interrupted installation of 0.1.7" 0.1.7
  st_new; st_app 0.1.7 yes; pre_case "the same, stapled" 0 "resuming an interrupted installation of 0.1.7" 0.1.7
  st_new; st_app 0.0.2; pre_case "refused: another version of the bundle, no receipt" 1 "$p $A: is Syndeo.app 0.0.2, with no receipt. If the Syndeo 0.0.2 package was interrupted installing it, install that package again; otherwise move it to the Trash, then install this package" 0.1.7
  st_new; st_app 0.1.7 no com.example.other; plutil -replace CFBundleIdentifier -string com.example.other "$A/Contents/Info.plist"; st_sign com.example.other
  pre_case "refused: another application at the path" 1 "$p $A: is there, and no Syndeo.app package installed it. Move it to the Trash" 0.1.7
  st_new; mkdir -p "$C/elsewhere"; A="$C/elsewhere/Syndeo.app"; st_app 0.1.7; ln -s "$A" "$R/Applications/Syndeo.app"; A="$R/Applications/Syndeo.app"
  pre_case "refused: a link to a bundle elsewhere, never followed" 1 "$A: is not a Directory owned by root:wheel" 0.1.7
  st_new; st_app 0.1.7; echo x >"$A/Contents/Resources/extra"; st_sign
  pre_case "refused: the bundle of this version with a file more" 1 "$A/Contents/Resources/extra: is not part of Syndeo.app" 0.1.7

  printf '\n  preinstall: the app, with its receipt\n'
  st_new; st_app_installed 0.0.2; pre_case "upgrade 0.0.2 to 0.1.7" 0 "upgrading from 0.0.2 to 0.1.7" 0.1.7
  st_new; st_app_installed 0.0.9; pre_case "upgrade 0.0.9 to 0.0.10, numerically" 0 "upgrading from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_app_installed 0.1.7; pre_case "the same version again" 0 "reinstalling 0.1.7" 0.1.7
  st_new; st_app_installed 0.1.7 yes; pre_case "the same version again, stapled" 0 "reinstalling 0.1.7" 0.1.7
  st_new; st_app_installed 0.1.7; pre_case "refused: a downgrade" 1 "$p Syndeo.app 0.1.7 is installed, and this package is the older 0.0.3. Remove 0.1.7 first: run sudo /bin/sh $A/Contents/Resources/uninstall.sh, or move Syndeo.app to the Trash" 0.0.3
  st_new; st_app_receipt 0.0.2; pre_case "the bundle moved to the Trash: installed again" 0 "installing 0.1.7; Syndeo.app 0.0.2, which the receipt names, is no longer in $R/Applications" 0.1.7
  st_new; st_app_receipt 0.1.7; pre_case "the bundle moved to the Trash: an older one may be installed" 0 "installing 0.0.3; Syndeo.app 0.1.7, which the receipt names, is no longer in $R/Applications" 0.0.3
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  pre_case "a failed upgrade: the same package completes it" 0 "completing the failed upgrade from 0.0.2 to 0.0.3" 0.0.3
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  pre_case "a failed upgrade: the older package refused" 1 "$p an upgrade from 0.0.2 to 0.0.3 did not finish: install the Syndeo 0.0.3 package again to complete it, then this one" 0.0.2
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  pre_case "a failed upgrade: a newer package refused" 1 "$p an upgrade from 0.0.2 to 0.0.3 did not finish: install the Syndeo 0.0.3 package again" 0.1.7
  st_new; st_app 0.0.3; echo x >"$A/Contents/Resources/extra"; st_sign; st_app_receipt 0.0.2
  pre_case "refused: a failed upgrade's bundle, changed" 1 "$A/Contents/Resources/extra: is not part of Syndeo.app" 0.0.3
  st_new; st_app 0.0.1; st_app_receipt 0.0.2
  pre_case "refused: an older bundle than its receipt" 1 "its CFBundleShortVersionString is '0.0.1', not 0.0.2" 0.1.7
  st_new; st_app_installed 0.0.2; echo x >"$A/Contents/Resources/extra"; st_sign
  pre_case "refused: a file more in the bundle" 1 "$A/Contents/Resources/extra: is not part of Syndeo.app" 0.1.7
  st_new; st_app_installed 0.0.2; echo x >"$A/Contents/Resources/extra"; st_sign
  pre_case "  ... and it says to move it to the Trash" 1 "$p $A: is not Syndeo.app 0.0.2 as its package installed it. Move it to the Trash, then install this package" 0.1.7
  st_new; st_app_installed 0.0.2; echo changed >>"$A/Contents/Info.plist"
  pre_case "refused: a file changed since it was signed" 1 "$A: its code signature does not verify: it has been changed since it was signed" 0.1.7
  st_new; st_app_installed 0.0.2; rm "$A/Contents/Resources/Syndeo.icns"; st_sign
  pre_case "refused: a file missing" 1 "$A/Contents/Resources/Syndeo.icns: is missing" 0.1.7
  st_new; st_app_installed 0.0.2; chmod 775 "$A/Contents/MacOS/Syndeo"
  pre_case "refused: a mode changed" 1 "$A/Contents/MacOS/Syndeo: is not a Regular File owned by root:wheel with mode 755" 0.1.7
  st_new; st_app_installed 0.0.2; chmod +a "everyone allow read" "$A/Contents/Info.plist"
  pre_case "refused: an access control list on a file" 1 "$A/Contents/Info.plist: is not a Regular File owned by root:wheel with mode 644" 0.1.7
  st_new; st_app_installed 0.0.2; chflags uchg "$A/Contents/PkgInfo"
  pre_case "refused: a file flagged immutable" 1 "$A/Contents/PkgInfo: is not a Regular File owned by root:wheel with mode 644" 0.1.7
  chflags nouchg "$A/Contents/PkgInfo"
  st_new; st_app_installed 0.0.2; rm "$A/Contents/Resources/Syndeo.icns"; ln -s ../Info.plist "$A/Contents/Resources/Syndeo.icns"; st_sign
  pre_case "refused: a link inside the bundle" 1 "$A/Contents/Resources/Syndeo.icns: is not a Regular File" 0.1.7
  st_new; st_app_installed 0.0.2; st_sign com.example.other
  pre_case "refused: signed as another identifier" 1 "$A: is signed as 'com.example.other', not com.sum.syndeo" 0.1.7
  st_new; st_app_installed 0.0.2; plutil -replace CFBundleIdentifier -string com.example.other "$A/Contents/Info.plist"; st_sign
  pre_case "refused: another bundle identifier" 1 "$A: is the bundle 'com.example.other', not com.sum.syndeo" 0.1.7
  st_new; st_app 0.0.2 yes; st_app_receipt 0.0.2 no
  pre_case "refused: a ticket the receipt does not list" 1 "$A: holds a stapled ticket its receipt does not list" 0.1.7
  st_new; st_app 0.0.2 no; st_app_receipt 0.0.2 yes
  pre_case "refused: no ticket where the receipt lists one" 1 "$A: lacks the stapled ticket its receipt lists" 0.1.7
  st_new; st_app_installed 0.0.2 yes; pre_case "a stapled bundle and its receipt: upgraded" 0 "upgrading from 0.0.2 to 0.1.7" 0.1.7
  st_new; st_app_installed 0.0.2; echo Syndeo.app/Contents/extra >>"$DB/com.sum.syndeo.app.files"
  pre_case "refused: the receipt lists other paths" 1 "the receipt for com.sum.syndeo.app 0.0.2 does not list exactly Syndeo.app's paths" 0.1.7
  st_new; st_app_installed 0.0.2; sed -i '' 's/^volume: \//volume: \/Volumes\/Other/' "$DB/com.sum.syndeo.app.info"
  pre_case "refused: the receipt is for another volume" 1 "the receipt for com.sum.syndeo.app is for volume '/Volumes/Other', not /" 0.1.7
  st_new; st_app_installed 0.0.2; sed -i '' 's/^version: .*/version: 0.2/' "$DB/com.sum.syndeo.app.info"
  pre_case "refused: the receipt's version is malformed" 1 "has the version '0.2', which is not a version" 0.1.7

  printf '\n  preinstall: where, and while Syndeo runs\n'
  st_new; pre_case "another target volume" 1 "the target volume is '/Volumes/Other'" 0.1.7 /Volumes/Other
  if [ -x "$T/hold" ]; then
    st_new; st_app_installed 0.0.2
    if st_hold app; then
      pre_case "refused: Syndeo running from /Applications" 1 "$p Syndeo is running from $A (process $held): quit it first" 0.1.7
    else
      expect "preinstall: Syndeo running (the held program did not start)" false
    fi
    st_unhold
    st_new; st_app_installed 0.0.2
    if st_hold helper; then
      pre_case "refused: its helper running from /Applications, by the file it runs" 1 "$p Syndeo is running from $A (process $held): quit it first" 0.1.7
    else
      expect "preinstall: the helper running (the held program did not start)" false
    fi
    st_unhold
    # A scan, as Spotlight or a malware check makes one just after an
    # install: the file open, and not running.
    st_new; st_app_installed 0.0.2
    # It holds the file open on its standard input, and never reads it.
    # shellcheck disable=SC2217
    sleep 120 <"$A/Contents/MacOS/Syndeo" &
    held=$!
    echo "$held" >>"$T/held"
    pre_case "a process that only reads Syndeo's executable: not running, upgraded" 0 "upgrading from 0.0.2 to 0.1.7" 0.1.7
    st_unhold
  else
    expect "preinstall: Syndeo running (needs cc, to build a program to hold)" false
  fi

  printf '\n  preinstall: the v0.1.x command-line package\n'
  st_new; st_tree 0.1.7; st_links; st_current 0.1.7
  pre_case "no receipt for it: whatever is in /usr/local is not looked at" 0 "syndeo preinstall: installing 0.1.7" 0.1.7
  st_new; st_bin; for n in $NAMES; do printf 'install.sh\n' >"$R/usr/local/bin/$n"; chmod 755 "$R/usr/local/bin/$n"; done
  pre_case "commands install.sh put in /usr/local/bin, no receipt: not looked at" 0 "syndeo preinstall: installing 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7
  pre_case "an exact v0.1.7: removed" 0 "syndeo preinstall: installing 0.1.7, and removing the v0.1.x command-line package 0.1.7" 0.1.7
  st_new; st_legacy 0.1.6; st_app_installed 0.0.2
  pre_case "an exact v0.1.6, with an upgrade" 0 "syndeo preinstall: upgrading from 0.0.2 to 0.1.7, and removing the v0.1.x command-line package 0.1.6" 0.1.7
  st_new; st_legacy 0.1.7 'location: /'
  pre_case "its receipt's location /" 0 "and removing the v0.1.x command-line package 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7; rm "$R/usr/local/bin/syndeo"; printf 'install.sh\n' >"$R/usr/local/bin/syndeo"; chmod 755 "$R/usr/local/bin/syndeo"
  pre_case "refused: a command install.sh put over one of its links" 1 "$p $R/usr/local/bin/syndeo: is not what the v0.1.x 0.1.7 package put there" 0.1.7
  st_new; st_legacy 0.1.7; rm "$R/usr/local/bin/syndeo"; printf 'install.sh\n' >"$R/usr/local/bin/syndeo"; chmod 755 "$R/usr/local/bin/syndeo"
  pre_case "  ... and it says how to repair or remove it" 1 "$p nothing of the v0.1.x command-line package 0.1.7 is removed unless it is exactly as its package left it: repair it by installing the Syndeo 0.1.7 package again, or remove it with sudo /bin/sh $D/0.1.7/uninstall.sh; then install this package" 0.1.7
  st_new; st_legacy 0.1.7; echo x >"$D/0.1.7/notes.txt"
  pre_case "refused: a file more in its tree" 1 "$D/0.1.7/notes.txt: is not part of Syndeo 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7; chmod 775 "$D/0.1.7/syndeo-net"
  pre_case "refused: a mode changed" 1 "$D/0.1.7/syndeo-net: is not a Regular File owned by root:wheel with mode 755" 0.1.7
  st_new; st_legacy 0.1.7; ln -s 0.1.7 "$D/.current.new"
  pre_case "refused: .current.new left by a switch" 1 "$D/.current.new: is there, from a switch that did not finish" 0.1.7
  st_new; st_legacy 0.1.7; st_tree 0.1.6
  pre_case "refused: a second version" 1 "$D: holds 0.1.6 0.1.7; only 0.1.7 may be there" 0.1.7
  st_new; st_legacy 0.1.7; rm "$D/current"; st_current 0.1.6
  pre_case "refused: current at another version" 1 "$D/current: points at 0.1.6, not 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7; rm "$D/0.1.7/README.md"
  pre_case "refused: a file missing" 1 "$D/0.1.7/README.md: is missing, and the receipt for 0.1.7 lists it" 0.1.7
  st_new; st_legacy 0.1.7; rm "$R/usr/local/bin/syndeo-agent"
  pre_case "refused: a command missing" 1 "$R/usr/local/bin/syndeo-agent: is missing, and the receipt for 0.1.7 lists it" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 1
  pre_case "refused: its first link gone, the bundle not this version" 1 "$p the v0.1.x 0.1.7 command-line package is only partly there: $R/usr/local/bin/syndeo is gone, and everything removed before it" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 7; st_app 0.1.7
  pre_case "its removal interrupted after the links, the bundle of this version: finished" 0 "syndeo preinstall: resuming an interrupted installation of 0.1.7, and finishing the removal of the v0.1.x command-line package 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 7; st_app_installed 0.1.7
  pre_case "  ... the same with this version's receipt: finished" 0 "syndeo preinstall: reinstalling 0.1.7, and finishing the removal of the v0.1.x command-line package 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 7; st_app 0.1.7; st_app_receipt 0.0.2
  pre_case "  ... the same after a failed upgrade to this version: finished" 0 "syndeo preinstall: completing the failed upgrade from 0.0.2 to 0.1.7, and finishing the removal of the v0.1.x command-line package 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 7; st_app_installed 0.0.2
  pre_case "refused: interrupted, and the bundle of another version" 1 "is only partly there" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 22; st_app 0.1.7
  pre_case "every path gone but the receipt, the bundle of this version: finished" 0 "and finishing the removal of the v0.1.x command-line package 0.1.7" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 22
  pre_case "refused: every path gone but the receipt, no bundle" 1 "is only partly there: $D is gone" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 2; rm "$R/usr/local/bin/syndeo-agent"; st_app 0.1.7
  pre_case "refused: a gap in what is left" 1 "$R/usr/local/bin/syndeo-agent: is missing, and the receipt for 0.1.7 lists it" 0.1.7
  st_new; st_legacy 0.1.7; st_legacy_cut 0.1.7 1; ln -s ../libexec/syndeo/current/syndeo "$R/usr/local/bin/syndeo"; rm "$R/usr/local/bin/syndeo-net"; st_app 0.1.7
  pre_case "refused: a link back where the removal had been" 1 "$R/usr/local/bin/syndeo-net: is missing" 0.1.7
  st_new; st_legacy 0.2.0
  pre_case "refused: a receipt for a version that is not 0.1.x" 1 "the receipt for com.sum.syndeo.pkg is for 0.2.0, which is not a v0.1.x command-line package" 0.1.7
  st_new; st_legacy 0.1.7; echo usr/local/extra >>"$DB/com.sum.syndeo.pkg.files"
  pre_case "refused: its receipt lists other paths" 1 "does not list exactly the package's paths" 0.1.7
  st_new; st_legacy 0.1.7; printf '%s\tcom.example.other\n' "$R/usr/local/bin/syndeo" >"$DB/claims"
  pre_case "refused: another package claims a command" 1 "is claimed by another package, com.example.other" 0.1.7
  st_new; st_legacy 0.1.7; chmod 777 "$R/usr/local/bin"
  pre_case "refused: /usr/local/bin writable by everyone" 1 "$R/usr/local/bin: can be written by everyone (mode 777)" 0.1.7
  st_new; st_legacy 0.1.7; chmod 775 "$R/usr/local/libexec"
  pre_case "refused: /usr/local/libexec group-writable" 1 "$R/usr/local/libexec: can be written by its group or by everyone (mode 775)" 0.1.7
  st_new; st_legacy 0.1.7; rm -rf "${D:?}/0.1.7"; st_tree 0.1.8; rm "$D/current"; st_current 0.1.7
  pre_case "refused: a failed v0.1.x upgrade" 1 "$D: holds 0.1.8; only 0.1.7 may be there" 0.1.7
  st_new; st_legacy 0.1.7; st_app_installed 0.0.2; echo x >"$A/Contents/Resources/extra"; st_sign; echo y >"$D/0.1.7/notes.txt"
  pre_case "the bundle and the v0.1.x tree both wrong: both named at once" 1 "$D/0.1.7/notes.txt: is not part of Syndeo 0.1.7" 0.1.7
  expect "  ... the bundle's too" grep -qF "$A/Contents/Resources/extra: is not part of Syndeo.app" "$C/out"
}

# post_case NAME EXIT TEXT VERSION: the postinstall for VERSION, run once,
# exits EXIT and says TEXT.
post_case() {
  local name="$1" want="$2" text="$3" v="$4"
  st_render_for postinstall "$v" "$C/postinstall"
  st_run "$C/postinstall" "$C/fake.pkg" / / /
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qF -- "$text" "$C/out" || verdict=no
  expect "postinstall: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

# st_legacy_gone: nothing of the v0.1.x package is left, and its receipt is
# forgotten; /usr/local/bin and /usr/local are still there.
st_legacy_gone() {
  [ "$(st_legacy_left 0.1.7)" = 0 ] && [ ! -f "$DB/com.sum.syndeo.pkg.info" ] &&
    [ -d "$R/usr/local/bin" ] && [ -d "$R/usr/local" ]
}

st_postinstall() {
  printf '\n  postinstall: the bundle\n'
  st_new; st_app 0.1.7; post_case "this version, exactly: installed, nothing else done" 0 "syndeo postinstall: Syndeo.app 0.1.7 is installed in $R/Applications" 0.1.7
  expect "  ... and nothing changed" test "$changed" = no
  st_new; st_app 0.1.7; echo x >"$A/Contents/Resources/extra"; st_sign; st_legacy 0.1.7
  post_case "refused: a file more in the bundle, and the v0.1.x package left alone" 1 "$A/Contents/Resources/extra: is not part of Syndeo.app" 0.1.7
  expect "  ... nothing changed" test "$changed" = no
  st_new; st_app 0.1.7; echo changed >>"$A/Contents/Info.plist"
  post_case "refused: a bundle changed since it was signed" 1 "its code signature does not verify" 0.1.7
  st_new; st_app 0.0.2; post_case "refused: a bundle of another version" 1 "its CFBundleShortVersionString is '0.0.2', not 0.1.7" 0.1.7
  st_new; st_app 0.1.7
  st_render_for postinstall 0.1.7 "$C/postinstall"
  st_run "$C/postinstall" "$C/fake.pkg" / /Volumes/Other /
  expect "postinstall: refused on another target volume, nothing changed" eval "[ $status = 1 ] && [ $changed = no ]"

  printf '\n  postinstall: removing the v0.1.x command-line package\n'
  st_new; st_app 0.1.7; st_legacy 0.1.7; echo keep >"$R/usr/local/bin/unrelated"
  post_case "an exact v0.1.7: removed, and Syndeo.app installed" 0 "syndeo postinstall: Syndeo.app 0.1.7 is installed in $R/Applications, and the v0.1.x command-line package 0.1.7 is removed: its commands in $R/usr/local/bin, $D, and its receipt. The command-line tools are still available from the release tarball and install.sh" 0.1.7
  expect "  ... every path of it gone, its receipt forgotten, /usr/local/bin and what else it holds kept" eval "st_legacy_gone && [ \"\$(cat '$R/usr/local/bin/unrelated')\" = keep ]"
  expect "  ... /usr/local/libexec, as the package created it, empty and unclaimed: removed" eval "! [ -e '$R/usr/local/libexec' ] && grep -qxF 'syndeo postinstall: removed $R/usr/local/libexec, which the v0.1.x package created, empty' '$C/out'"
  expect "  ... the bundle untouched" eval "'$C/codesign' --verify --strict --deep '$A'"
  st_new; st_app 0.1.7; st_legacy 0.1.7; printf '%s\tcom.sum.syndeo.pkg\n' "$R/usr/local/libexec" >"$DB/claims"
  post_case "/usr/local/libexec claimed by the v0.1.x receipt alone: removed" 0 "removed $R/usr/local/libexec" 0.1.7
  expect "  ... gone" test ! -e "$R/usr/local/libexec"

  libexec_case() {
    local label="$1" why="$2"
    post_case "/usr/local/libexec $label: left in place" 0 "syndeo postinstall: left $R/usr/local/libexec in place: $why" 0.1.7
    expect "  ... and still there, the v0.1.x package removed" eval "[ -d '$R/usr/local/libexec' ] && st_legacy_gone"
  }
  st_new; st_app 0.1.7; st_legacy 0.1.7; mkdir "$R/usr/local/libexec/someone-else"
  libexec_case "holding something else" "it is not empty"
  st_new; st_app 0.1.7; st_legacy 0.1.7; printf '%s\tcom.example.other\n' "$R/usr/local/libexec" >"$DB/claims"
  libexec_case "claimed by another package" "another package, com.example.other, claims it"
  st_new; st_app 0.1.7; st_legacy 0.1.7; touch "$DB/file-info-fails"
  libexec_case "whose claims cannot be read" "which packages claim it could not be read"
  st_new; st_app 0.1.7; st_legacy 0.1.7; chmod 711 "$R/usr/local/libexec"
  libexec_case "with another mode" "it is $U:$G mode 0711, not root:wheel 0755"
  st_new; st_app 0.1.7; st_legacy 0.1.7; xattr -w com.example.mark yes "$R/usr/local/libexec"
  libexec_case "with an extended attribute" "it has extended attributes"
  st_new; st_app 0.1.7; st_legacy 0.1.7; chflags hidden "$R/usr/local/libexec"
  libexec_case "with a flag" "it has flags (hidden)"
  local other='' g
  for g in $(id -G); do
    if [ "$g" != "$G" ]; then other="$g"; break; fi
  done
  st_new; st_app 0.1.7; st_legacy 0.1.7
  if [ -n "$other" ] && chgrp "$other" "$R/usr/local/libexec" 2>/dev/null; then
    libexec_case "of another group" "it is $U:$other mode 0755, not root:wheel 0755"
  else
    expect "postinstall: /usr/local/libexec of another group (needs a second group to set up)" false
  fi

  st_new; st_app 0.1.7; st_legacy 0.1.7; st_legacy_cut 0.1.7 7
  post_case "its removal interrupted after the links: finished" 0 "the v0.1.x command-line package 0.1.7 is removed" 0.1.7
  expect "  ... every path of it gone, and its receipt" st_legacy_gone
  st_new; st_app 0.1.7; st_legacy 0.1.7; st_legacy_cut 0.1.7 22
  post_case "interrupted with every path gone: its receipt forgotten" 0 "the v0.1.x command-line package 0.1.7 is removed" 0.1.7
  expect "  ... and gone" st_legacy_gone
  st_new; st_app 0.1.7; st_legacy 0.1.7; st_legacy_cut 0.1.7 22; rmdir "$R/usr/local/libexec"
  post_case "interrupted after /usr/local/libexec went: its receipt forgotten" 0 "the v0.1.x command-line package 0.1.7 is removed" 0.1.7
  st_new; st_app 0.1.7; st_legacy 0.1.7; echo x >"$D/0.1.7/notes.txt"
  post_case "refused: a file more in its tree, and nothing of it removed" 1 "$D/0.1.7/notes.txt: is not part of Syndeo 0.1.7" 0.1.7
  expect "  ... nothing changed" test "$changed" = no
  st_new; st_app 0.1.7; st_legacy 0.1.7; touch "$DB/forget-fails"
  post_case "its receipt cannot be forgotten: fails, saying how" 1 "    sudo /usr/sbin/pkgutil --forget com.sum.syndeo.pkg --volume /" 0.1.7
  expect "  ... every path of it gone, the receipt still there" eval "[ \"\$(st_legacy_left 0.1.7)\" = 0 ] && [ -f '$DB/com.sum.syndeo.pkg.info' ]"
  rm -f "$DB/forget-fails"
  post_case "  ... run again: the receipt forgotten" 0 "the v0.1.x command-line package 0.1.7 is removed" 0.1.7
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
  local u
  printf '\n  uninstall: look first, then remove\n'
  st_new; st_app_installed 0.1.7
  mkdir -p "$R/Applications/Other.app" "$R/Users/someone/Library/Application Support/Syndeo" "$R/Users/someone/Library/Preferences"
  echo settings >"$R/Users/someone/Library/Application Support/Syndeo/settings.json"
  echo prefs >"$R/Users/someone/Library/Preferences/com.sum.syndeo.plist"
  cp "$A/Contents/Resources/uninstall.sh" "$C/uninstall.sh"
  u="$A/Contents/Resources/uninstall.sh"
  un_case "everything, from inside the bundle" 0 "syndeo uninstall: removed Syndeo.app 0.1.7. Your settings and data in ~/Library are kept" "$u"
  expect "  ... the bundle gone, the receipt forgotten" eval "! [ -e '$A' ] && ! [ -f '$DB/com.sum.syndeo.app.info' ]"
  expect "  ... /Applications, another application and the user's data kept" eval "[ -d '$R/Applications/Other.app' ] && [ \"\$(cat '$R/Users/someone/Library/Application Support/Syndeo/settings.json')\" = settings ] && [ -f '$R/Users/someone/Library/Preferences/com.sum.syndeo.plist' ]"
  st_new; st_app_installed 0.1.7 yes
  un_case "stapled: the ticket too" 0 "removed Syndeo.app 0.1.7" "$A/Contents/Resources/uninstall.sh"
  expect "  ... the bundle gone, the receipt forgotten" eval "! [ -e '$A' ] && ! [ -f '$DB/com.sum.syndeo.app.info' ]"
  st_new; st_app_installed 0.1.7; st_legacy 0.1.6
  un_case "the v0.1.x package, if there, left alone" 0 "removed Syndeo.app 0.1.7" "$A/Contents/Resources/uninstall.sh"
  expect "  ... its receipt and every path of it still there" eval "[ -f '$DB/com.sum.syndeo.pkg.info' ] && [ \"\$(st_legacy_left 0.1.6)\" = 22 ]"

  st_new; st_app_installed 0.1.7; cp "$A/Contents/Resources/uninstall.sh" "$C/uninstall.sh"
  un_case "--help: both ways to remove it, and what is kept" 0 "Move Syndeo.app to the Trash." "$C/uninstall.sh" --help
  expect "  ... nothing changed" test "$changed" = no
  un_case "an unknown argument" 1 "usage:" "$C/uninstall.sh" --everything
  st_new; st_app_installed 0.1.7; U=0; st_render_for uninstall 0.1.7 "$C/as-root"; U="$(id -u)"
  un_case "not run as root" 1 "refusing: run it as root, with sudo" "$C/as-root"
  expect "  ... nothing changed" test "$changed" = no

  st_new; st_app 0.1.7; cp "$A/Contents/Resources/uninstall.sh" "$C/uninstall.sh"
  un_case "no receipt: refused" 1 "refusing: no Syndeo.app package is installed. If Syndeo.app is in $R/Applications without one, move it to the Trash" "$C/uninstall.sh"
  expect "  ... nothing changed" test "$changed" = no
  st_new; st_app_installed 0.0.2; st_render_for uninstall 0.1.7 "$C/uninstall.sh"
  un_case "another version's receipt: refused" 1 "refusing: this is the uninstaller of Syndeo.app 0.1.7, and the installed package is 0.0.2. Run $A/Contents/Resources/uninstall.sh" "$C/uninstall.sh"
  expect "  ... nothing changed" test "$changed" = no
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  un_case "after a failed upgrade: refused, saying to finish it" 1 "refusing: the upgrade from 0.0.2 to 0.0.3 did not finish. Install the Syndeo 0.0.3 package again to complete it, then run this again" "$A/Contents/Resources/uninstall.sh"
  expect "  ... nothing changed" test "$changed" = no
  st_new; st_app_receipt 0.1.7; st_render_for uninstall 0.1.7 "$C/uninstall.sh"
  un_case "the bundle in the Trash: refused, saying how to forget the receipt" 1 "    sudo /usr/sbin/pkgutil --forget com.sum.syndeo.app --volume /" "$C/uninstall.sh"
  expect "  ... nothing changed" test "$changed" = no

  local plant
  for plant in extra changed missing mode acl link identifier ticket-unlisted ticket-missing; do
    st_new; st_app_installed 0.1.7
    case "$plant" in
      extra) echo x >"$A/Contents/Resources/extra"; st_sign ;;
      changed) echo changed >>"$A/Contents/PkgInfo" ;;
      missing) rm "$A/Contents/Resources/Syndeo.icns"; st_sign ;;
      mode) chmod 775 "$A/Contents/Helpers/syndeo-proxy" ;;
      acl) chmod +a "everyone allow list" "$A/Contents/MacOS" ;;
      link) rm "$A/Contents/PkgInfo"; ln -s Info.plist "$A/Contents/PkgInfo"; st_sign ;;
      identifier) st_sign com.example.other ;;
      ticket-unlisted) printf 'ticket' >"$A/Contents/CodeResources"; chmod 644 "$A/Contents/CodeResources" ;;
      ticket-missing) st_app_receipt 0.1.7 yes ;;
    esac
    un_case "all or nothing, the bundle with $plant: refused, nothing changed" 1 "refusing:" "$A/Contents/Resources/uninstall.sh"
    [ "$changed" = no ] || expect "  ... and nothing changed" false
  done

  if [ -x "$T/hold" ]; then
    st_new; st_app_installed 0.1.7; st_render_for uninstall 0.1.7 "$C/uninstall.sh"
    if st_hold app; then
      un_case "Syndeo running: refused" 1 "refusing: Syndeo is running from $A (process $held): quit it first" "$C/uninstall.sh"
      expect "  ... nothing changed" test "$changed" = no
    else
      expect "uninstall: Syndeo running (the held program did not start)" false
    fi
    st_unhold
  else
    expect "uninstall: Syndeo running (needs cc, to build a program to hold)" false
  fi

  st_new; st_app_installed 0.1.7; touch "$DB/forget-fails"
  un_case "the receipt cannot be forgotten: exit 2, saying how" 2 "    sudo /usr/sbin/pkgutil --forget com.sum.syndeo.app --volume /" "$A/Contents/Resources/uninstall.sh"
  expect "  ... the bundle gone, the receipt still there" eval "! [ -e '$A' ] && [ -f '$DB/com.sum.syndeo.app.info' ]"
}

# inst NAME VERSION...: this script's installed mode, for the case's root.
inst() {
  SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    SYNDEO_VERIFY_CODESIGN="$C/codesign" bash "$here/verify-pkg.sh" installed "$@" 2>&1
}

st_installed_mode() {
  local out
  printf '\n  this script: installed\n'
  st_new; st_app_installed 0.1.7
  out="$(inst 0.1.7)"
  expect "installed: a complete installation passes" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'
  st_new; st_app_installed 0.1.7 yes
  out="$(inst 0.1.7)"
  expect "installed: a stapled one too" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  st_new; st_app_installed 0.1.7; st_bin; printf 'install.sh\n' >"$R/usr/local/bin/syndeo"
  out="$(inst 0.1.7)"
  expect "installed: a command install.sh put in /usr/local/bin is not the package's" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  st_new; st_app_installed 0.0.2
  out="$(inst 0.1.7)"
  expect "installed: another version fails" eval "printf '%s' \"\$out\" | grep -q \"receipt — version '0.0.2'\""
  st_new; st_app_installed 0.0.2
  out="$(inst 0.0.2 --helper-says 0.0.2)"
  expect "installed: --helper-says, the version a stand-in's helper reports: passes" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  out="$(inst 0.0.2 --helper-says 0.1.7)"
  expect "installed: --helper-says another version than the helper's: fails" eval "printf '%s' \"\$out\" | grep -q \"not 'syndeo-proxy 0.1.7'\""
  st_new; st_app_installed 0.1.7; echo x >"$A/Contents/Resources/extra"; st_sign
  out="$(inst 0.1.7)"
  expect "installed: a file more in the bundle fails" eval "printf '%s' \"\$out\" | grep -q 'extra: is not part of Syndeo.app'"
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  out="$(inst 0.0.3)"
  expect "installed: after a failed upgrade, it fails on the receipt" eval "printf '%s' \"\$out\" | grep -q \"receipt — version '0.0.2'\""
  st_new; st_app_installed 0.1.7; st_legacy 0.1.6
  out="$(inst 0.1.7)"
  expect "installed: the v0.1.x receipt and its layout still there fail" eval "printf '%s' \"\$out\" | grep -q 'com.sum.syndeo.pkg 0.1.6 is still recorded' && printf '%s' \"\$out\" | grep -q 'v0.1.x layout — still there: $D'"
  st_new; st_app_installed 0.1.7; st_links
  out="$(inst 0.1.7)"
  expect "installed: a link into the v0.1.x layout fails" eval "printf '%s' \"\$out\" | grep -q 'v0.1.x layout — still there: $R/usr/local/bin/syndeo '"
  st_new
  out="$(inst 0.1.7)"
  expect "installed: nothing installed fails" eval "printf '%s' \"\$out\" | grep -q 'there is no receipt for com.sum.syndeo.app'"
}

# loc_case NAME accepted|refused LOCATION: an app receipt whose location
# lines are LOCATION, judged by the preinstall of an upgrade, the
# uninstaller and this script's installed mode.
loc_case() {
  local name="$1" want="$2" loc="$3" out verdict
  st_new; st_app 0.0.2; st_app_receipt 0.0.2 no "$loc"
  st_render_for preinstall 0.1.7 "$C/preinstall"
  st_run "$C/preinstall" "$C/fake.pkg" / / /
  verdict=yes
  [ "$changed" = no ] || verdict=no
  if [ "$want" = accepted ]; then
    [ "$status" = 0 ] && grep -qF "upgrading from 0.0.2 to 0.1.7" "$C/out" || verdict=no
  else
    [ "$status" = 1 ] && grep -qF 'does not say the bundle is in Applications' "$C/out" || verdict=no
  fi
  expect "location: $name: preinstall $want" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"

  out="$(inst 0.0.2)"
  verdict=yes
  if [ "$want" = accepted ]; then
    printf '%s' "$out" | grep -q '[1-9][0-9]* passed, 0 failed' || verdict=no
  else
    printf '%s\n' "$out" | grep 'FAIL' | grep -qF 'does not say the bundle is in Applications' || verdict=no
  fi
  expect "location: $name: installed $want" test "$verdict" = yes

  st_run "$A/Contents/Resources/uninstall.sh"
  verdict=yes
  if [ "$want" = accepted ]; then
    [ "$status" = 0 ] && ! [ -e "$A" ] || verdict=no
  else
    [ "$status" = 1 ] && [ "$changed" = no ] && grep -qF 'does not say the bundle is in Applications' "$C/out" || verdict=no
  fi
  expect "location: $name: uninstall $want" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

# legacy_loc_case NAME accepted|refused LOCATION: the same for a v0.1.x
# receipt, judged by the preinstall that would remove it.
legacy_loc_case() {
  local name="$1" want="$2" loc="$3" verdict
  st_new; st_legacy 0.1.7 "$loc"
  st_render_for preinstall 0.1.7 "$C/preinstall"
  st_run "$C/preinstall" "$C/fake.pkg" / / /
  verdict=yes
  [ "$changed" = no ] || verdict=no
  if [ "$want" = accepted ]; then
    [ "$status" = 0 ] && grep -qF "removing the v0.1.x command-line package 0.1.7" "$C/out" || verdict=no
  else
    [ "$status" = 1 ] && grep -qE 'location fields, not one|location is .*, not the volume.s root' "$C/out" || verdict=no
  fi
  expect "v0.1.x location: $name: preinstall $want" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_location() {
  printf '\n  the receipts'"'"' location field\n'
  loc_case "Applications, as pkgutil prints it for this package" accepted 'location: Applications'
  loc_case "missing" refused ''
  loc_case "duplicated" refused $'location: Applications\nlocation: Applications'
  loc_case "without the separating space" refused 'location:Applications'
  loc_case "after two spaces" refused 'location:  Applications'
  loc_case "then a space" refused 'location: Applications '
  loc_case "absolute" refused 'location: /Applications'
  loc_case "the bundle itself" refused 'location: Applications/Syndeo.app'
  loc_case "the volume's root" refused 'location: '
  loc_case "elsewhere" refused 'location: Users/someone/Applications'
  legacy_loc_case "empty, as pkgutil prints it for that package" accepted 'location: '
  legacy_loc_case "/" accepted 'location: /'
  legacy_loc_case "missing" refused ''
  legacy_loc_case "duplicated" refused $'location: /\nlocation: /'
  legacy_loc_case "Applications" refused 'location: Applications'
  legacy_loc_case "/, after a tab" refused $'location:\t/'
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

# fault_case NAME WHERE V: a postinstall for V with the test-only fault
# ci/pkg-test-fault.sh adds at WHERE, run on the state this case has set up.
# Its first run says exactly that it is failing, and exits 1; its second goes
# through.
fault_case() {
  local name="$1" where="$2" v="$3" verdict=yes
  st_render_for postinstall "$v" "$C/postinstall"
  bash "$here/pkg-test-fault.sh" --script "$C/postinstall" "$where" "$C/fault" || verdict=no
  /bin/sh "$C/postinstall" "$C/fake.pkg" / / / >"$C/out" 2>&1
  status=$?
  [ "$status" = 1 ] || verdict=no
  grep -qxF "syndeo postinstall: test fault $where: failing this first run" "$C/out" || verdict=no
  fault_left="$(st_legacy_left 0.1.7)"
  /bin/sh "$C/postinstall" "$C/fake.pkg" / / / >"$C/out2" 2>&1 || verdict=no
  grep -qF "syndeo postinstall: Syndeo.app $v is installed in $R/Applications" "$C/out2" || verdict=no
  decided="$decided $name"
  expect "decision: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out" "$C/out2"
}

# What each package script says in the real-runner test, ci/test-pkg-install.sh,
# asserted here exactly and as the script runs, from the same templates under
# a temporary root. The runner gates on what the installation is before and
# after, not on these lines: Installer normally records them in
# /var/log/install.log, but does not guarantee they appear there. Each case
# is named after the runner scenario it stands for; V, the real package, is
# 0.1.7 here, and the stand-ins are 0.0.1 to 0.0.3.
st_decisions() {
  printf '\n  the decisions the real-runner test relies on, said exactly\n'
  local p='syndeo preinstall: refusing:' label missing n
  decided=''

  # The first install, as a person makes it, with commands install.sh put in
  # /usr/local/bin.
  st_new; st_bin; for n in $NAMES; do printf 'install.sh\n' >"$R/usr/local/bin/$n"; chmod 755 "$R/usr/local/bin/$n"; done
  decision_case "quarantined-download" preinstall 0.1.7 0 "syndeo preinstall: installing 0.1.7"
  if [ -x "$T/hold" ]; then
    st_new; st_app_installed 0.1.7
    if st_hold app; then
      decision_case "while-it-runs" preinstall 0.1.7 1 "$p Syndeo is running from $A (process $held): quit it first"
    fi
    st_unhold
  else
    decided="$decided while-it-runs"
    expect "decision: while-it-runs (needs cc, to build a program to hold)" false
  fi
  st_new; st_app_installed 0.1.7
  decision_case "reinstall" preinstall 0.1.7 0 "syndeo preinstall: reinstalling 0.1.7"

  # The v0.1.x command-line package: 0.1.6, then 0.1.7.
  st_new; st_legacy 0.1.6; echo x >"$D/0.1.6/notes.txt"
  decision_case "legacy-extra-file" preinstall 0.1.7 1 "$p $D/0.1.6/notes.txt: is not part of Syndeo 0.1.6"
  st_new; st_legacy 0.1.6; chmod 775 "$D/0.1.6/syndeo-net"
  decision_case "legacy-mode-changed" preinstall 0.1.7 1 "$p $D/0.1.6/syndeo-net: is not a Regular File owned by root:wheel with mode 755 and nothing else (found Regular File $U:$G 775 special 0 flags -)"
  st_new; st_legacy 0.1.6; ln -s 0.1.6 "$D/.current.new"
  decision_case "legacy-current-new" preinstall 0.1.7 1 "$p $D/.current.new: is there, from a switch that did not finish"
  st_new; st_legacy 0.1.6; st_tree 0.0.99
  decision_case "legacy-second-version" preinstall 0.1.7 1 "$p $D: holds 0.0.99 0.1.6; only 0.1.6 may be there"
  st_new; st_legacy 0.1.6; rm "$D/0.1.6/README.md"
  decision_case "legacy-file-missing" preinstall 0.1.7 1 "$p $D/0.1.6/README.md: is missing, and the receipt for 0.1.6 lists it"
  st_new; st_legacy 0.1.6; st_legacy_cut 0.1.6 1
  decision_case "legacy-first-link-missing" preinstall 0.1.7 1 "$p the v0.1.x 0.1.6 command-line package is only partly there: $R/usr/local/bin/syndeo is gone, and everything removed before it"
  st_new; st_legacy 0.1.6; rm "$R/usr/local/bin/syndeo-agent"
  decision_case "legacy-link-missing" preinstall 0.1.7 1 "$p $R/usr/local/bin/syndeo-agent: is missing, and the receipt for 0.1.6 lists it"
  st_new; st_legacy 0.1.6
  decision_case "migrate-v0.1.6" preinstall 0.1.7 0 "syndeo preinstall: installing 0.1.7, and removing the v0.1.x command-line package 0.1.6"
  st_new; st_app 0.1.7; st_legacy 0.1.6
  decision_case "migrate-v0.1.6-removal" postinstall 0.1.7 0 "syndeo postinstall: removed $R/usr/local/libexec, which the v0.1.x package created, empty"
  st_new; st_legacy 0.1.7; rm "$R/usr/local/bin/syndeo"; printf 'install.sh\n' >"$R/usr/local/bin/syndeo"
  decision_case "legacy-install-sh-over-links" preinstall 0.1.7 1 "$p $R/usr/local/bin/syndeo: is not what the v0.1.x 0.1.7 package put there"
  st_new; st_legacy 0.1.7
  decision_case "migration-interrupted" preinstall 0.0.1 0 "syndeo preinstall: installing 0.0.1, and removing the v0.1.x command-line package 0.1.7"
  st_new; st_app 0.0.1; st_legacy 0.1.7
  fault_case "migration-interrupted-fault" mid-removal 0.0.1
  expect "  ... the first run stopped after the seven links, the rest of v0.1.7 left" test "$fault_left" = 15
  st_new; st_app 0.0.1; st_legacy 0.1.7; st_legacy_cut 0.1.7 7
  decision_case "migration-interrupted-other-package" preinstall 0.1.7 1 "$p $A: is Syndeo.app 0.0.1, with no receipt. If the Syndeo 0.0.1 package was interrupted installing it, install that package again; otherwise move it to the Trash, then install this package"
  st_new; st_app 0.0.1; st_legacy 0.1.7; st_legacy_cut 0.1.7 7
  decision_case "migration-interrupted-other-package-legacy" preinstall 0.1.7 1 "$p the v0.1.x 0.1.7 command-line package is only partly there: $R/usr/local/bin/syndeo-webkit is gone, and everything removed before it"
  st_new; st_app 0.0.1; st_legacy 0.1.7; st_legacy_cut 0.1.7 7
  decision_case "migration-resumed" preinstall 0.0.1 0 "syndeo preinstall: resuming an interrupted installation of 0.0.1, and finishing the removal of the v0.1.x command-line package 0.1.7"

  # Upgrades, a failed one, a downgrade, a reinstall, the Trash.
  st_new; st_app_installed 0.0.1
  decision_case "upgrade-0.0.2" preinstall 0.0.2 0 "syndeo preinstall: upgrading from 0.0.1 to 0.0.2"
  st_new; st_app_installed 0.0.2
  decision_case "fault" preinstall 0.0.3 0 "syndeo preinstall: upgrading from 0.0.2 to 0.0.3"
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  fault_case "fault-postinstall" entry 0.0.3
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  decision_case "fault-older-package" preinstall 0.0.2 1 "$p an upgrade from 0.0.2 to 0.0.3 did not finish: install the Syndeo 0.0.3 package again to complete it, then this one"
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  decision_case "fault-newer-package" preinstall 0.1.7 1 "$p an upgrade from 0.0.2 to 0.0.3 did not finish: install the Syndeo 0.0.3 package again to complete it, then this one"
  st_new; st_app 0.0.3; st_app_receipt 0.0.2
  decision_case "fault-the-same-package-again" preinstall 0.0.3 0 "syndeo preinstall: completing the failed upgrade from 0.0.2 to 0.0.3"
  st_new; st_app_installed 0.0.3
  decision_case "downgrade" preinstall 0.0.2 1 "$p Syndeo.app 0.0.3 is installed, and this package is the older 0.0.2. Remove 0.0.3 first: run sudo /bin/sh $A/Contents/Resources/uninstall.sh, or move Syndeo.app to the Trash"
  st_new; st_app_installed 0.0.3
  decision_case "reinstall-0.0.3" preinstall 0.0.3 0 "syndeo preinstall: reinstalling 0.0.3"
  st_new; st_app_installed 0.0.3; echo x >"$A/Contents/Resources/extra"; st_sign
  decision_case "modified-bundle" preinstall 0.0.3 1 "$p $A: is not Syndeo.app 0.0.3 as its package installed it. Move it to the Trash, then install this package"
  st_new; st_app_installed 0.0.3
  decision_case "upgrade-real" preinstall 0.1.7 0 "syndeo preinstall: upgrading from 0.0.3 to 0.1.7"
  st_new; st_app_receipt 0.1.7
  decision_case "after-the-trash" preinstall 0.1.7 0 "syndeo preinstall: installing 0.1.7; Syndeo.app 0.1.7, which the receipt names, is no longer in $R/Applications"
  st_new; st_app 0.1.7 no com.example.other; plutil -replace CFBundleIdentifier -string com.example.other "$A/Contents/Info.plist"; st_sign com.example.other
  decision_case "foreign-app" preinstall 0.1.7 1 "$p $A: is there, and no Syndeo.app package installed it. Move it to the Trash, then install this package"
  st_new
  decision_case "final" preinstall 0.1.7 0 "syndeo preinstall: installing 0.1.7"

  # Every install the runner makes whose outcome a script decides has its
  # case above, named after it: each literal label of refused, installed and
  # install, and each failed_upgrade with the three it makes. The install on
  # another volume is Installer's own refusal; no script runs.
  missing=''
  for label in $(
    {
      grep -oE '^[[:space:]]*(refused|installed|install|failed_install) [a-z0-9][a-z0-9.-]*' "$here/test-pkg-install.sh" | awk '{print $2}'
      grep -oE '^[[:space:]]*failed_upgrade [a-z0-9][a-z0-9.-]*' "$here/test-pkg-install.sh" | awk '{print $2; print $2 "-older-package"; print $2 "-newer-package"; print $2 "-the-same-package-again"}'
    } | grep -vx 'another-volume' | sort -u
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
    # What it says, unless a knob names it: STANDIN_KILLED (killed as it
    # starts, as macOS kills a binary it refuses), STANDIN_SAYS_OTHER,
    # STANDIN_EXITS (exit 3), STANDIN_HANGS; and the keystore's status, with
    # STANDIN_PRESENCE and STANDIN_PASSPHRASE_REQUIRED.
    cat >"$dir/src/$name/$n" <<EOF
#!/bin/sh
n=$n v=$v
EOF
    cat >>"$dir/src/$name/$n" <<'EOF'
[ "$n" != "${STANDIN_KILLED:-}" ] || kill -9 $$
[ "$n" != "${STANDIN_HANGS:-}" ] || exec sleep 30
if [ "$n" = syndeo-keystore ] && [ "${1:-}" = status ]; then
  printf 'initialized          false\nunsealed             false\n'
  printf 'passphrase required  %s\npresence enforced    %s\n' "${STANDIN_PASSPHRASE_REQUIRED:-true}" "${STANDIN_PRESENCE:-false}"
  exit 0
fi
if [ "$n" = "${STANDIN_SAYS_OTHER:-}" ]; then echo "$n 9.9.9"; else echo "$n $v"; fi
[ "$n" != "${STANDIN_EXITS:-}" ] || exit 3
EOF
    chmod 755 "$dir/src/$name/$n"
  done
  cp "$here/../README.md" "$here/../LICENSE" "$dir/src/$name/"
  cp "$here/../crates/syndeo-agent/tools/wordcount.wat" "$dir/src/$name/tools/"
  tar -C "$dir/src" --format=ustar -czf "$dir/$name.tar.gz" "$name"
}

# st_fake_app DIR V [WORD]: a real Syndeo.app V in DIR/app, assembled by
# ci/package-macos-app.sh from two small compiled programs that say their
# names and V (and WORD, to make another bundle of the same version), and
# signed ad hoc, as a pull request signs it.
st_fake_app() {
  local dir="$1" v="$2" word="${3:-}" n
  mkdir -p "$dir/bins" "$dir/app"
  for n in syndeo-app syndeo-proxy; do
    cc -DNAME="\"$n\"" -DVERSION="\"$v\"" -DWORD="\"$word\"" -o "$dir/bins/$n" "$T/stub.c" || return 1
  done
  bash "$assembler" "$v" "$dir/bins" "$dir/app" >/dev/null || return 1
  codesign --force --sign - --options runtime "$dir/app/Syndeo.app" >/dev/null 2>&1 || return 1
  codesign --verify --strict --deep "$dir/app/Syndeo.app" >/dev/null 2>&1
}

st_inspect() {
  printf '\n  this script: inspect, against packages built from a compiled stand-in\n'
  local d="$T/inspect" out pkg app status
  if ! command -v cc >/dev/null 2>&1; then
    expect "inspect (needs cc, to compile a stand-in Syndeo.app)" false
    return 0
  fi
  mkdir -p "$d/out"
  st_fake_app "$d" 0.0.1 || { expect "inspect: a stand-in Syndeo.app 0.0.1 assembled and signed ad hoc" false; return 0; }
  app="$d/app/Syndeo.app"
  SOURCE_DATE_EPOCH=1 "$builder" 0.0.1 "$app" "$d/out" >/dev/null 2>&1
  pkg="$d/out/syndeo-0.0.1-aarch64-apple-darwin.pkg"
  out="$(bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --app "$app" 2>&1)"
  expect "inspect: a package built from the bundle passes" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'

  st_fake_app "$d/other" 0.0.1 other || expect "inspect: another stand-in assembled" false
  out="$(bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --app "$d/other/app/Syndeo.app" 2>&1)"
  expect "inspect: a payload other than the bundle named fails" eval "printf '%s' \"\$out\" | grep -q 'provenance — the payload.s Syndeo.app differs'"

  rm -rf "$d/unsigned" && mkdir -p "$d/unsigned"
  bash "$assembler" 0.0.1 "$d/bins" "$d/unsigned" >/dev/null
  expect "the builder refuses an unsigned bundle" eval "! SOURCE_DATE_EPOCH=1 '$builder' 0.0.1 '$d/unsigned/Syndeo.app' '$d/refused' >/dev/null 2>&1 && ! [ -e '$d/refused/syndeo-0.0.1-aarch64-apple-darwin.pkg' ]"
  expect "the builder refuses a bundle of another version" eval "! SOURCE_DATE_EPOCH=1 '$builder' 0.0.2 '$app' '$d/refused' >/dev/null 2>&1"
  cp -R "$app" "$d/extra.app" && mkdir -p "$d/extra" && mv "$d/extra.app" "$d/extra/Syndeo.app"
  echo x >"$d/extra/Syndeo.app/Contents/Resources/extra"
  expect "the builder refuses a bundle with a file more" eval "! SOURCE_DATE_EPOCH=1 '$builder' 0.0.1 '$d/extra/Syndeo.app' '$d/refused' >/dev/null 2>&1"

  rm -rf "$d/expanded" && pkgutil --expand "$pkg" "$d/expanded"
  out="$(bash "$here/verify-pkg.sh" package-info "$d/expanded/Syndeo.pkg/PackageInfo" 0.0.1 2>&1)"
  expect "package-info: what pkgbuild wrote for the bundle passes" eval "printf '%s' \"\$out\" | grep -q '1 passed, 0 failed'"
  out="$(bash "$here/verify-pkg.sh" package-info "$d/expanded/Syndeo.pkg/PackageInfo" 0.0.2 2>&1)"
  expect "package-info: another version fails" eval "printf '%s' \"\$out\" | grep -q '0 passed, 1 failed'"

  mkdir -p "$d/faulty"
  local w
  for w in entry mid-removal; do
    rm -f "$d/faulty/syndeo-0.0.1-aarch64-apple-darwin.pkg"
    bash "$here/pkg-test-fault.sh" "$pkg" "$d/faulty/syndeo-0.0.1-aarch64-apple-darwin.pkg" "$w" "$d/faulty/marker" >"$d/faulty/log" 2>&1
    expect "pkg-test-fault.sh: a fault at $w, and nothing else changed" test "$?" = 0
    out="$(bash "$here/verify-pkg.sh" inspect "$d/faulty/syndeo-0.0.1-aarch64-apple-darwin.pkg" 0.0.1 --app "$app" 2>&1)"
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
    out="$(bash "$here/verify-pkg.sh" inspect "$d/edited/syndeo-0.0.1-aarch64-apple-darwin.pkg" 0.0.1 --app "$app" 2>&1)"
    expect "inspect: $label fails" eval "printf '%s' \"\$out\" | grep -q -- '$text'"
  }
  edit_case "overwrite-permissions true" Syndeo.pkg/PackageInfo 'overwrite-permissions="false"' 'overwrite-permissions="true"' "overwrite-permissions)"
  edit_case "a relocatable bundle" Syndeo.pkg/PackageInfo '<relocate/>' '<relocate><bundle id="com.sum.syndeo"/></relocate>' "count(/pkg-info/relocate/\\*)"
  edit_case "a version-checked bundle" Syndeo.pkg/PackageInfo '<bundle-version/>' '<bundle-version><bundle id="com.sum.syndeo"/></bundle-version>' "count(/pkg-info/bundle-version/\\*)"
  edit_case "another install location" Syndeo.pkg/PackageInfo 'install-location="/Applications"' 'install-location="/"' "install-location)"
  edit_case "an Intel-capable product" Distribution 'hostArchitectures="arm64"' 'hostArchitectures="arm64,x86_64"' "hostArchitectures)"
  edit_case "a product installable anywhere" Distribution 'enable_anywhere="false"' 'enable_anywhere="true"' "enable_anywhere)"
  edit_case "JavaScript in the product" Distribution '<title>' '<installation-check script="true()"/><title>' "count(//installation-check)"

  cp "$pkg" "$d/renamed.pkg"
  out="$(bash "$here/verify-pkg.sh" inspect "$d/renamed.pkg" 0.0.1 --app "$app" 2>&1)"
  expect "inspect: a misnamed package fails" eval "printf '%s' \"\$out\" | grep -q 'renamed.pkg is not syndeo-0.0.1-aarch64-apple-darwin.pkg'"
  st_signed "$pkg" "$app"
}

# st_signed PKG APP: inspect's judgement of a signed release, with stand-ins
# for codesign, pkgutil --check-signature, xcrun stapler and spctl that
# report what a signed, notarized and stapled package and bundle would, each
# case changing one thing. Then signed-binaries and signed-app, on stand-in
# tarballs and bundles.
st_signed() {
  local pkg="$1" app="$2" bin="$T/signed-bin" out status
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
    id="$n"
    [ "$n" != Syndeo.app ] || id="${STANDIN_APP_IDENTIFIER:-com.sum.syndeo}"
    {
      echo "Executable=$path"
      echo "Identifier=$id"
      echo "CodeDirectory v=20500 size=1 flags=$flags hashes=1+7 location=embedded"
      echo "Authority=Developer ID Application: Example Org ($team)"
      echo "Authority=Developer ID Certification Authority"
      echo "Authority=Apple Root CA"
      if [ "$n" = "${STANDIN_NO_TIMESTAMP:-}" ]; then echo "Signed Time=10 Oct 2026 at 12:00:00"; else echo "Timestamp=10 Oct 2026 at 12:00:00"; fi
      echo "TeamIdentifier=$team"
    } >&2 ;;
  -d)
    # No entitlements, unless STANDIN_ENTITLED names this executable: then the
    # keychain access group the keystore used to claim, or STANDIN_ENTITLEMENT.
    [ "$n" = "${STANDIN_ENTITLED:-}" ] || exit 0
    printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict>%s</dict></plist>\n' \
      "${STANDIN_ENTITLEMENT:-<key>keychain-access-groups</key><array><string>$team.com.sum.syndeo.keystore</string></array>}" ;;
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
[ "$1 $2" = "stapler validate" ] || exit 1
case "$3" in
  *.app) [ "${STANDIN_APP_STAPLED:-yes}" = yes ] ;;
  *) [ "${STANDIN_STAPLED:-yes}" = yes ] ;;
esac
EOF
  cat >"$bin/spctl" <<'EOF'
#!/bin/bash
path="${@: -1}"
case "$path" in
  *.app) verdict="${STANDIN_APP_SPCTL:-accepted}" source="${STANDIN_APP_SOURCE:-Notarized Developer ID}" origin="${STANDIN_APP_ORIGIN:-Developer ID Application: Example Org (AB12CD34EF)}" ;;
  *) verdict="${STANDIN_SPCTL:-accepted}" source="${STANDIN_SOURCE:-Notarized Developer ID}" origin="${STANDIN_ORIGIN:-Developer ID Installer: Example Org (AB12CD34EF)}" ;;
esac
if [ "$verdict" = accepted ]; then
  printf '%s: accepted\nsource=%s\norigin=%s\n' "$path" "$source" "$origin" >&2
else
  printf '%s: rejected\n' "$path" >&2; exit 3
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
      bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --app "$app" 2>&1)"
    if [ "$fails" = 0 ]; then
      expect "signed inspect: $label" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
    else
      expect "signed inspect: $label" eval "printf '%s' \"\$out\" | grep -q ' passed, $fails failed' && printf '%s' \"\$out\" | grep 'FAIL' | grep -qF -- \"\$text\""
    fi
    printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'
  }
  printf '\n  this script: inspect, of a signed release, against stand-ins for the signing tools\n'
  signed_case "Developer ID throughout, no entitlements, notarized and stapled: passes" 0 ""
  signed_case "the bundle of another team, and so Gatekeeper's origin" 2 "Syndeo.app — signed by 'Developer ID Application: Example Org (ZZ99ZZ99ZZ)'; TeamIdentifier 'ZZ99ZZ99ZZ';" STANDIN_OTHER_TEAM=Syndeo.app
  signed_case "the helper of another team" 1 "syndeo-proxy, the helper — signed by 'Developer ID Application: Example Org (ZZ99ZZ99ZZ)'" STANDIN_OTHER_TEAM=syndeo-proxy
  signed_case "a bundle that does not verify" 2 "Syndeo.app — does not verify;" STANDIN_UNVERIFIED=Syndeo.app
  signed_case "the bundle without the hardened runtime" 1 "Syndeo.app — no hardened runtime;" STANDIN_NO_RUNTIME=Syndeo.app
  signed_case "the helper without a secure timestamp" 1 "syndeo-proxy, the helper — no secure timestamp;" STANDIN_NO_TIMESTAMP=syndeo-proxy
  signed_case "the bundle with an entitlement" 1 "Syndeo.app — has entitlements, and no shipped executable may;" STANDIN_ENTITLED=Syndeo.app "STANDIN_ENTITLEMENT=<key>com.apple.security.cs.disable-library-validation</key><true/>"
  signed_case "the helper with the keychain access group" 1 "syndeo-proxy, the helper — has entitlements, and no shipped executable may;" STANDIN_ENTITLED=syndeo-proxy
  signed_case "the bundle signed as another identifier" 1 "Syndeo.app — signed as 'com.example.other', not com.sum.syndeo;" STANDIN_APP_IDENTIFIER=com.example.other
  signed_case "the bundle's ticket not stapled" 1 "Syndeo.app's notarization ticket — stapler validate fails" STANDIN_APP_STAPLED=no
  signed_case "Gatekeeper rejects the bundle" 1 "Gatekeeper, Syndeo.app — rejected;" STANDIN_APP_SPCTL=rejected
  signed_case "Gatekeeper: the bundle Developer ID, not notarized" 1 "Gatekeeper, Syndeo.app — source=Developer ID;" "STANDIN_APP_SOURCE=Developer ID"
  signed_case "the package signed by another team" 1 "package signature — signed by 'Developer ID Installer: Other Org (ZZ99ZZ99ZZ)';" "STANDIN_PKG_SIGNER=Developer ID Installer: Other Org (ZZ99ZZ99ZZ)" "STANDIN_ORIGIN=Developer ID Installer: Other Org (ZZ99ZZ99ZZ)"
  signed_case "the package signed with the Application identity" 1 "package signature — signed by 'Developer ID Application:" "STANDIN_PKG_SIGNER=Developer ID Application: Example Org (AB12CD34EF)" "STANDIN_ORIGIN=Developer ID Application: Example Org (AB12CD34EF)"
  signed_case "the package not chained to Apple Root CA" 1 "chained to 'Someone Root', not Apple Root CA;" "STANDIN_PKG_ROOT=Someone Root"
  signed_case "the package trusted, but not for distribution" 1 "package signature — Status: signed by a certificate trusted by macOS;" "STANDIN_PKG_STATUS=signed by a certificate trusted by macOS"
  signed_case "no stapled ticket on the package" 1 "notarization ticket — stapler validate fails" STANDIN_STAPLED=no
  signed_case "Gatekeeper: the package Developer ID, not notarized" 1 "Gatekeeper — source=Developer ID;" "STANDIN_SOURCE=Developer ID"
  signed_case "Gatekeeper: the package from another origin" 1 "Gatekeeper — from another origin;" "STANDIN_ORIGIN=Developer ID Installer: Example Org (ZZ99ZZ99ZZ)"
  signed_case "Gatekeeper rejects the package" 1 "Gatekeeper — rejected;" STANDIN_SPCTL=rejected

  # binaries_case LABEL FAILS TEXT [NAME=VALUE...]: signed-binaries on the
  # stand-in tarball, as for signed_case.
  local d="$T/binaries" tgz
  mkdir -p "$d"
  st_tarball "$d" 0.0.1
  tgz="$d/syndeo-0.0.1-aarch64-apple-darwin.tar.gz"
  binaries_case() {
    local label="$1" fails="$2" text="$3"
    shift 3
    out="$(env "$@" PATH="$bin:$PATH" SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID=AB12CD34EF \
      bash "$here/verify-pkg.sh" signed-binaries "$tgz" 0.0.1 2>&1)"
    if [ "$fails" = 0 ]; then
      # Seven signatures, seven runs, and the keystore's status.
      expect "signed-binaries: $label" eval "printf '%s' \"\$out\" | grep -q '15 passed, 0 failed'"
    else
      expect "signed-binaries: $label" eval "printf '%s' \"\$out\" | grep -q ' passed, $fails failed' && printf '%s' \"\$out\" | grep 'FAIL' | grep -qF -- \"\$text\""
    fi
    printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'
  }
  printf '\n  this script: signed-binaries, the tarball before notarization\n'
  binaries_case "all seven Developer ID of the team, none entitled: passes" 0 ""
  binaries_case "one executable of another team" 1 "syndeo-net — signed by 'Developer ID Application: Example Org (ZZ99ZZ99ZZ)'" STANDIN_OTHER_TEAM=syndeo-net
  binaries_case "one executable that does not verify" 1 "syndeo-ui — does not verify;" STANDIN_UNVERIFIED=syndeo-ui
  binaries_case "one executable without the hardened runtime" 1 "syndeo-agent — no hardened runtime;" STANDIN_NO_RUNTIME=syndeo-agent
  binaries_case "one executable without a secure timestamp" 1 "syndeo-proxy — no secure timestamp;" STANDIN_NO_TIMESTAMP=syndeo-proxy
  binaries_case "the keystore with the keychain access group it used to claim" 1 "syndeo-keystore — has entitlements, and no shipped executable may;" STANDIN_ENTITLED=syndeo-keystore
  binaries_case "the access group on another executable" 1 "syndeo-ui — has entitlements, and no shipped executable may;" STANDIN_ENTITLED=syndeo-ui
  binaries_case "any entitlement on any executable" 1 "syndeo-webkit — has entitlements, and no shipped executable may;" STANDIN_ENTITLED=syndeo-webkit "STANDIN_ENTITLEMENT=<key>com.apple.security.cs.disable-library-validation</key><true/>"
  binaries_case "the keystore killed at launch, as macOS killed it entitled: fails, its version and its status" 2 "syndeo-keystore — --version: killed by signal 9 at launch" STANDIN_KILLED=syndeo-keystore
  binaries_case "another executable killed at launch" 1 "syndeo-proxy — --version: killed by signal 9 at launch" STANDIN_KILLED=syndeo-proxy
  binaries_case "an executable that says another version" 1 "syndeo-net — --version: said 'syndeo-net 9.9.9', not 'syndeo-net 0.0.1'" STANDIN_SAYS_OTHER=syndeo-net
  binaries_case "an executable that says its version and exits non-zero" 1 "syndeo-agent — --version: exited 3, saying 'syndeo-agent 0.0.1'" STANDIN_EXITS=syndeo-agent
  binaries_case "an executable that does not finish" 1 "syndeo-ui — --version: did not finish in 2 seconds" STANDIN_HANGS=syndeo-ui SYNDEO_VERIFY_LAUNCH_SECONDS=2
  binaries_case "the keystore claiming platform presence" 1 "syndeo-keystore status — does not report platform presence unenforced and the passphrase required" STANDIN_PRESENCE=true
  binaries_case "the keystore not requiring the passphrase" 1 "syndeo-keystore status — does not report platform presence unenforced and the passphrase required" STANDIN_PASSPHRASE_REQUIRED=false
  out="$(env -u MACOS_TEAM_ID SYNDEO_EXPECT_SIGNED=yes bash "$here/verify-pkg.sh" signed-binaries "$tgz" 0.0.1 2>&1)" && status=0 || status=$?
  expect "signed-binaries: without MACOS_TEAM_ID, refused" eval "[ $status = 2 ] && printf '%s' \"\$out\" | grep -q 'needs MACOS_TEAM_ID'"
  out="$(SYNDEO_EXPECT_SIGNED=no MACOS_TEAM_ID=AB12CD34EF bash "$here/verify-pkg.sh" signed-binaries "$tgz" 0.0.1 2>&1)" && status=0 || status=$?
  expect "signed-binaries: SYNDEO_EXPECT_SIGNED=no, refused" eval "[ $status = 2 ] && printf '%s' \"\$out\" | grep -q 'needs SYNDEO_EXPECT_SIGNED=yes'"
  out="$(PATH="$bin:$PATH" SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID=AB12CD34EF bash "$here/verify-pkg.sh" signed-binaries "$tgz" 0.0.2 2>&1)" && status=0 || status=$?
  expect "signed-binaries: a tarball of another version, refused" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'is not syndeo-0.0.2-aarch64-apple-darwin.tar.gz'"

  # The stand-in tarball's own syndeo-proxy, as the helper of a bundle whose
  # signature's paths the stand-in codesign answers for.
  local sa="$d/signed-app"
  mkdir -p "$sa/bins" "$sa/out"
  cp "$d/src/syndeo-0.0.1-aarch64-apple-darwin/syndeo-proxy" "$sa/bins/syndeo-proxy"
  printf '#!/bin/sh\n' >"$sa/bins/syndeo-app"
  bash "$assembler" 0.0.1 "$sa/bins" "$sa/out" >/dev/null
  mkdir -m 755 "$sa/out/Syndeo.app/Contents/_CodeSignature"
  printf 'seal' >"$sa/out/Syndeo.app/Contents/_CodeSignature/CodeResources"
  chmod 644 "$sa/out/Syndeo.app/Contents/_CodeSignature/CodeResources"
  app_case() {
    local label="$1" fails="$2" text="$3" a="$4"
    shift 4
    out="$(env "$@" PATH="$bin:$PATH" SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID=AB12CD34EF \
      bash "$here/verify-pkg.sh" signed-app "$a" 0.0.1 --tarball "$tgz" 2>&1)"
    if [ "$fails" = 0 ]; then
      expect "signed-app: $label" eval "printf '%s' \"\$out\" | grep -q '5 passed, 0 failed'"
    else
      expect "signed-app: $label" eval "printf '%s' \"\$out\" | grep -q ' passed, $fails failed' && printf '%s' \"\$out\" | grep 'FAIL' | grep -qF -- \"\$text\""
    fi
    printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'
  }
  printf '\n  this script: signed-app, the bundle before notarization\n'
  app_case "the bundle and its helper Developer ID of the team, the helper the tarball's: passes" 0 "" "$sa/out/Syndeo.app"
  app_case "the bundle of another team" 1 "Syndeo.app — signed by 'Developer ID Application: Example Org (ZZ99ZZ99ZZ)'" "$sa/out/Syndeo.app" STANDIN_OTHER_TEAM=Syndeo.app
  app_case "the helper without the hardened runtime" 1 "syndeo-proxy, the helper — no hardened runtime;" "$sa/out/Syndeo.app" STANDIN_NO_RUNTIME=syndeo-proxy
  app_case "the helper killed at launch" 1 "helper — --version: killed by signal 9 at launch" "$sa/out/Syndeo.app" STANDIN_KILLED=syndeo-proxy
  cp -R "$sa/out" "$sa/other"
  printf '#!/bin/sh\necho "syndeo-proxy 0.0.1"\n' >"$sa/other/Syndeo.app/Contents/Helpers/syndeo-proxy"
  app_case "a helper that is not the tarball's" 1 "helper — differs from the tarball's syndeo-proxy" "$sa/other/Syndeo.app"
  rm -rf "$sa/other/Syndeo.app/Contents/_CodeSignature"
  cp "$sa/out/Syndeo.app/Contents/Helpers/syndeo-proxy" "$sa/other/Syndeo.app/Contents/Helpers/syndeo-proxy"
  app_case "a bundle without its signature's paths" 1 "bundle — " "$sa/other/Syndeo.app"
  out="$(env -u MACOS_TEAM_ID SYNDEO_EXPECT_SIGNED=yes bash "$here/verify-pkg.sh" signed-app "$sa/out/Syndeo.app" 0.0.1 --tarball "$tgz" 2>&1)" && status=0 || status=$?
  expect "signed-app: without MACOS_TEAM_ID, refused" eval "[ $status = 2 ] && printf '%s' \"\$out\" | grep -q 'needs MACOS_TEAM_ID'"

  out="$(PATH="$bin:$PATH" SYNDEO_EXPECT_SIGNED=no bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --app "$app" 2>&1)"
  expect "inspect: SYNDEO_EXPECT_SIGNED=no, of a package whose bundle is signed: fails" eval "printf '%s' \"\$out\" | grep -q 'carries a Developer ID'"
  out="$(env -u MACOS_TEAM_ID SYNDEO_EXPECT_SIGNED=yes bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --app "$app" 2>&1)" && status=0 || status=$?
  expect "inspect: SYNDEO_EXPECT_SIGNED=yes without MACOS_TEAM_ID is refused" eval "[ $status = 2 ] && printf '%s' \"\$out\" | grep -q 'needs MACOS_TEAM_ID'"
  out="$(SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID=ab12cd34ef bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --app "$app" 2>&1)" && status=0 || status=$?
  expect "inspect: SYNDEO_EXPECT_SIGNED=yes with a malformed MACOS_TEAM_ID is refused" eval "[ $status = 2 ] && printf '%s' \"\$out\" | grep -q 'needs MACOS_TEAM_ID'"
}

# st_tools: what the macOS cases build once: a program that waits, for a
# Syndeo that runs, and a program that says its name and version, for a
# stand-in bundle that is real.
st_tools() {
  cat >"$T/stub.c" <<'EOF'
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
  if (argc > 1 && strcmp(argv[1], "--version") == 0) {
    printf("%s %s\n", NAME, VERSION);
    return 0;
  }
  (void)WORD;
  pause();
  return 0;
}
EOF
  cat >"$T/hold.c" <<'EOF'
#include <stdlib.h>
#include <unistd.h>
int main(int argc, char **argv) {
  sleep(argc > 1 ? (unsigned)atoi(argv[1]) : 60);
  return 0;
}
EOF
  if command -v cc >/dev/null 2>&1; then
    cc -o "$T/hold" "$T/hold.c" 2>/dev/null || rm -f "$T/hold"
  fi
}

self_test() {
  T="$(mktemp -d)"
  : >"$T/held"
  # Not local: the EXIT trap runs after this function has returned.
  trap 'while read -r p; do kill "$p" 2>/dev/null; done <"$T/held"; chflags -R nouchg,nohidden "$T" 2>/dev/null; rm -rf "$T"' EXIT
  st_n=0
  st_versions
  mkdir -p "$T/render"
  st_render "$T/render"
  st_harness
  st_release
  if [ "$(uname -s)" = Darwin ]; then
    st_tools
    st_preinstall
    st_postinstall
    st_uninstall
    st_installed_mode
    st_location
    st_decisions
    st_inspect
  else
    printf '\n  on %s: the root scripts, the package build and inspect are macOS-only (BSD stat, ls -e, lsof, pkgbuild) and were not run\n' "$(uname -s)"
  fi
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

# st_release: the release's assets and notes, as release.yml checks them,
# against stand-in assets.
st_release() {
  printf '\n  this script: the assets and notes of a release\n'
  local d="$T/release" v=0.9.1 f out status
  # rel ARGS...: this script's release mode; sets out and status.
  rel() { out="$(bash "$here/verify-pkg.sh" release "$@" 2>&1)" && status=0 || status=$?; }
  fresh() {
    rm -rf "$d"
    mkdir -p "$d/dist"
    for f in $(release_assets "$v"); do printf '%s\n' "$f" >"$d/dist/$f"; done
  }

  fresh
  rel sums "$d/dist" "$v"
  expect "sums: the four assets of the version: SHA256SUMS written, in order" eval \
    "[ $status = 0 ] && [ \"\$(sed 's/^[0-9a-f]*  //' '$d/dist/SHA256SUMS')\" = \"\$(release_assets $v)\" ] && (cd '$d/dist' && shasum -a 256 -c SHA256SUMS >/dev/null)"
  fresh; rm "$d/dist/syndeo-$v-aarch64-unknown-linux-gnu.tar.gz"
  rel sums "$d/dist" "$v"
  expect "sums: a tarball missing: refused, nothing written" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'missing syndeo-$v-aarch64-unknown-linux-gnu.tar.gz' && [ ! -e '$d/dist/SHA256SUMS' ]"
  fresh; mv "$d/dist/syndeo-$v-x86_64-unknown-linux-gnu.tar.gz" "$d/dist/syndeo-0.9.0-x86_64-unknown-linux-gnu.tar.gz"
  rel sums "$d/dist" "$v"
  expect "sums: one of another version: refused" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'unexpected syndeo-0.9.0-x86_64-unknown-linux-gnu.tar.gz' && printf '%s' \"\$out\" | grep -q 'missing syndeo-$v-x86_64'"
  fresh; echo extra >"$d/dist/syndeo-$v-x86_64-apple-darwin.tar.gz"
  rel sums "$d/dist" "$v"
  expect "sums: an extra asset: refused" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'unexpected syndeo-$v-x86_64-apple-darwin.tar.gz'"
  fresh; echo old >"$d/dist/SHA256SUMS"
  rel sums "$d/dist" "$v"
  expect "sums: a SHA256SUMS already there: refused" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'unexpected SHA256SUMS'"
  fresh; : >"$d/dist/syndeo-$v-aarch64-apple-darwin.pkg"
  rel sums "$d/dist" "$v"
  expect "sums: an empty package: refused" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'pkg is not a non-empty regular file'"
  fresh; rm "$d/dist/syndeo-$v-aarch64-apple-darwin.pkg"; mkdir "$d/dist/syndeo-$v-aarch64-apple-darwin.pkg"
  rel sums "$d/dist" "$v"
  expect "sums: a directory for the package: refused" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'pkg is not a non-empty regular file'"
  fresh
  rel sums "$d/dist" 0.9
  expect "sums: a malformed version: refused" test "$status" = 2

  # draft V: what was built, with SHA256SUMS, and the draft downloaded from it.
  draft() {
    fresh
    bash "$here/verify-pkg.sh" release sums "$d/dist" "$v" >/dev/null 2>&1
    cp -R "$d/dist" "$d/draft"
    bash "$here/release-notes.sh" "$v" no >"$d/notes.md"
    cp "$d/notes.md" "$d/draft-notes.md"
  }
  draft
  rel check "$d/draft" "$v" --against "$d/dist" --notes "$d/notes.md" "$d/draft-notes.md"
  expect "check: the draft as built, with its notes: passes" test "$status" = 0
  draft; printf '\n\n' >>"$d/draft-notes.md"
  rel check "$d/draft" "$v" --against "$d/dist" --notes "$d/notes.md" "$d/draft-notes.md"
  expect "check: notes that differ only in trailing blank lines: pass" test "$status" = 0
  draft; rm "$d/draft/syndeo-$v-aarch64-apple-darwin.pkg"
  rel check "$d/draft" "$v" --against "$d/dist"
  expect "check: the package missing from the draft: fails" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'missing syndeo-$v-aarch64-apple-darwin.pkg'"
  draft; echo stale >"$d/draft/syndeo-0.9.0-aarch64-apple-darwin.pkg"
  rel check "$d/draft" "$v" --against "$d/dist"
  expect "check: a stale asset in the draft: fails" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'unexpected syndeo-0.9.0-aarch64-apple-darwin.pkg'"
  draft; printf 'syndeo-%s-aarch64-apple-darwin.tar.gx\n' "$v" >"$d/draft/syndeo-$v-aarch64-apple-darwin.tar.gz"
  rel check "$d/draft" "$v" --against "$d/dist"
  expect "check: an asset changed in one byte, same size: fails" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'syndeo-$v-aarch64-apple-darwin.tar.gz — differs from what was built'"
  draft
  for f in "$d/dist/SHA256SUMS" "$d/draft/SHA256SUMS"; do
    printf '%s\n' "$(sed -n 1p "$f")" "$(sed -n 3p "$f")" "$(sed -n 2p "$f")" "$(sed -n 4p "$f")" >"$f.new" && mv "$f.new" "$f"
  done
  rel check "$d/draft" "$v" --against "$d/dist"
  expect "check: SHA256SUMS out of order, though it verifies: fails" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'SHA256SUMS — does not list exactly the four'"
  draft
  for f in "$d/dist/SHA256SUMS" "$d/draft/SHA256SUMS"; do sed -i.bak '1s/^./0/;1s/^0/f/' "$f" && rm "$f.bak"; done
  rel check "$d/draft" "$v" --against "$d/dist"
  expect "check: a SHA256SUMS that does not verify: fails" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'SHA256SUMS — does not list exactly the four'"
  draft; bash "$here/release-notes.sh" "$v" yes >"$d/draft-notes.md"
  rel check "$d/draft" "$v" --against "$d/dist" --notes "$d/notes.md" "$d/draft-notes.md"
  expect "check: notes claiming signing for an unsigned build: fail" eval "[ $status = 1 ] && printf '%s' \"\$out\" | grep -q 'notes — differ from the expected'"

  out="$(bash "$here/release-notes.sh" "$v" no)"
  expect "notes, unsigned: both routes, the package's exact name, Syndeo.app alone, its uninstaller, and the signing state as it is" eval "printf '%s' \"\$out\" | grep -qF 'install.sh | sh' &&
    printf '%s' \"\$out\" | grep -qF 'sudo installer -pkg syndeo-$v-aarch64-apple-darwin.pkg -target /' &&
    printf '%s' \"\$out\" | grep -qF 'shasum -a 256 -c SHA256SUMS --ignore-missing' &&
    printf '%s' \"\$out\" | grep -qF 'sudo /bin/sh /Applications/Syndeo.app/Contents/Resources/uninstall.sh' &&
    printf '%s' \"\$out\" | grep -qF 'and nothing else: the command-line tools come' &&
    printf '%s' \"\$out\" | grep -qF 'ad-hoc signed, with no Developer ID, and not notarized' &&
    printf '%s' \"\$out\" | grep -qF 'The installer package is unsigned' && ! printf '%s' \"\$out\" | grep -qF 'stapled'"
  out="$(bash "$here/release-notes.sh" "$v" yes)"
  expect "notes, signed: Developer ID and notarized, the package's ticket stapled, and no unsigned claim" eval "printf '%s' \"\$out\" | grep -qF 'signed with a Developer ID and notarized' &&
    printf '%s' \"\$out\" | grep -qF 'ticket stapled' && ! printf '%s' \"\$out\" | grep -qF 'unsigned' && ! printf '%s' \"\$out\" | grep -qF 'ad-hoc'"
  out="$(bash "$here/release-notes.sh" 0.1.5 no)"
  expect "notes: the CHANGELOG's section for the version, when it has one" eval "printf '%s' \"\$out\" | grep -qF 'Wasmtime 48.0.2 → 48.0.4'"
  expect "notes: a malformed version, or signed neither yes nor no, is refused" eval "! bash '$here/release-notes.sh' 0.1 no >/dev/null 2>&1 && ! bash '$here/release-notes.sh' 0.1.5 maybe >/dev/null 2>&1"
}

# self_test_decisions: only st_decisions, for runs that change one message.
self_test_decisions() {
  T="$(mktemp -d)"
  : >"$T/held"
  trap 'while read -r p; do kill "$p" 2>/dev/null; done <"$T/held"; chflags -R nouchg,nohidden "$T" 2>/dev/null; rm -rf "$T"' EXIT
  st_n=0
  [ "$(uname -s)" = Darwin ] || { echo "the decisions are checked on macOS only" >&2; return 2; }
  st_tools
  st_decisions
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

usage() {
  echo "usage: $0 inspect <pkg> <version> --app <Syndeo.app> | package-info <PackageInfo> <version> | signed-binaries <tarball> <version> | signed-app <Syndeo.app> <version> --tarball <tarball> | installed <version> | release sums|check ... | --self-test [decisions]" >&2
  exit 2
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
    [ "$#" = 5 ] && [ "$4" = --app ] || { echo "usage: $0 inspect <pkg> <version> --app <Syndeo.app>" >&2; exit 2; }
    inspect_work="$(mktemp -d)"
    trap 'rm -rf "$inspect_work"' EXIT
    inspect "$2" "$3" "$5"
    status=$?
    [ "$status" = 2 ] && exit 2
    report
    [ "$fail" -eq 0 ]
    ;;
  package-info)
    [ "$#" = 3 ] || { echo "usage: $0 package-info <PackageInfo> <version>" >&2; exit 2; }
    package_info "$2" "$3"
    report
    [ "$fail" -eq 0 ]
    ;;
  release)
    case "${2:-}:$#" in
      sums:4) release_sums "$3" "$4" ;;
      check:6 | check:9)
        [ "$5" = --against ] && { [ "$#" = 6 ] || [ "${7:-}" = --notes ]; } ||
          { echo "usage: $0 release check <draft-dir> <version> --against <dir> [--notes <expected> <draft's>]" >&2; exit 2; }
        release_check "$3" "$4" "$6" "${8:-}" "${9:-}"
        ;;
      *) echo "usage: $0 release sums <dir> <version> | release check <draft-dir> <version> --against <dir> [--notes <expected> <draft's>]" >&2; exit 2 ;;
    esac
    status=$?
    [ "$status" = 2 ] && exit 2
    report
    [ "$fail" -eq 0 ]
    ;;
  signed-binaries)
    [ "$#" = 3 ] || { echo "usage: $0 signed-binaries <tarball> <version>" >&2; exit 2; }
    inspect_work="$(mktemp -d)"
    trap 'rm -rf "$inspect_work"' EXIT
    signed_binaries "$2" "$3"
    status=$?
    [ "$status" = 2 ] && exit 2
    report
    [ "$fail" -eq 0 ]
    ;;
  signed-app)
    [ "$#" = 5 ] && [ "$4" = --tarball ] || { echo "usage: $0 signed-app <Syndeo.app> <version> --tarball <tarball>" >&2; exit 2; }
    inspect_work="$(mktemp -d)"
    trap 'rm -rf "$inspect_work"' EXIT
    signed_app "$2" "$3" "$5"
    status=$?
    [ "$status" = 2 ] && exit 2
    report
    [ "$fail" -eq 0 ]
    ;;
  installed)
    if [ "$#" = 4 ] && [ "$3" = --helper-says ]; then
      says="$4"
    else
      [ "$#" = 2 ] || { echo "usage: $0 installed <version> [--helper-says <version>]" >&2; exit 2; }
      says="$2"
    fi
    inspect_work="$(mktemp -d)"
    trap 'rm -rf "$inspect_work"' EXIT
    installed "$2" "$says"
    report
    [ "$fail" -eq 0 ]
    ;;
  *) usage ;;
esac
