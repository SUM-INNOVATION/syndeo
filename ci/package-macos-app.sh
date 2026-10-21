#!/usr/bin/env bash
# Assemble Syndeo.app, the macOS desktop application, from built binaries.
#
#   ci/package-macos-app.sh <version> <binaries-dir> <out-dir> [--profile <file>]
#   ci/package-macos-app.sh --check <Syndeo.app> <version> [unsigned|signed|stapled] [release]
#   ci/package-macos-app.sh --manifest [unsigned|signed|stapled] [profiled]
#   ci/package-macos-app.sh --profiled <version>
#   ci/package-macos-app.sh --self-test
#
# The bundle holds exactly these, and nothing else (ci/macos-app/bundle.manifest
# lists them, with their modes, for this script and for the package's root
# scripts alike):
#
#   Syndeo.app/Contents/Info.plist                ci/macos-app/Info.plist.in, for <version>
#   Syndeo.app/Contents/PkgInfo                   APPL????
#   Syndeo.app/Contents/MacOS/Syndeo              <binaries-dir>/syndeo-app
#   Syndeo.app/Contents/Helpers/syndeo-proxy      <binaries-dir>/syndeo-proxy
#   Syndeo.app/Contents/Resources/Syndeo.icns     ci/macos-app/Syndeo.icns
#   Syndeo.app/Contents/Resources/uninstall.sh    ci/macos-pkg/uninstall.sh.in, for <version>
#
# With --profile, a release's: the provisioning profile that authorizes
# Syndeo.app's one entitlement, Apple's browser passkey entitlement (see
# ci/macos-app/profile.py), as
#
#   Syndeo.app/Contents/embedded.provisionprofile <file>
#
# A release's bundle carries it from the version in ci/macos-app/profile-since
# on, and a pull request's never does: no pull request has the profile. A
# release's uninstaller knows that version, so that it can tell an older
# release's bundle, from before the profile, from a newer one's; a pull
# request's knows none, and expects a profile in no bundle.
#
# uninstall.sh removes the bundle the installer package installed, and that
# package's receipt (see ci/package-macos-pkg.sh). It is inside the bundle so
# that it is signed and sealed with it, and goes wherever the bundle goes.
#
# The application's one helper is the proxy, which runs the network layer
# in-process. The app finds it in Contents/Helpers and nowhere else (see
# syndeo_shell::supervisor::bundle_helpers). The command-line tools are not
# in the bundle: they ship in the per-user tarball, as before.
#
# Directories are 0755, the executables 0755 and everything else 0644, with
# no extended attributes and no links. The bundle is unsigned; signing it is
# a separate step (ci/sign-macos.sh --app), the helper first and then the
# bundle.
#
# --check requires a bundle to be exactly that: the paths and their modes,
# the Info.plist's identifier, executable, versions and minimum system, its
# App Transport Security and URL types exactly as below, the PkgInfo, an icon, and the
# uninstaller exactly as rendered for <version>. signed also requires the
# code signature's two paths, Contents/_CodeSignature/ and its CodeResources;
# stapled, those and the notarization ticket stapler adds,
# Contents/CodeResources. It looks at the paths, not at whether the signature
# verifies: codesign does that. --manifest prints what --check requires, one
# path per line with its type (d or f) and mode.
#
# App Transport Security: web pages may load over plain http, as they do in
# any browser, and nothing else is relaxed. NSAllowsArbitraryLoadsInWebContent
# applies to web views only; the application's own connections keep ATS.
#
# URL types: http and https, and no other scheme, as a web browser's. That is
# what lets someone choose Syndeo as their default browser, lets another
# application ask it to open a web address, and is one of Apple's conditions
# for its browser passkey entitlement. Another scheme here would hand Syndeo
# addresses it refuses anyway.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ci/macos-pkg/version.sh
. "$here/macos-pkg/version.sh"

say() { printf 'package-macos-app: %s\n' "$*"; }
die() {
  printf 'package-macos-app: %s\n' "$*" >&2
  exit 1
}

# The first version a release's Syndeo.app carries a provisioning profile in.
profile_since() { cat "$here/macos-app/profile-since"; }

# manifest [STATE] [PROFILED]: the bundle's paths, relative to Syndeo.app,
# with each one's type and mode, sorted, for an unsigned (the default),
# signed or stapled bundle, with its provisioning profile when PROFILED is
# yes.
manifest() {
  {
    cat "$here/macos-app/bundle.manifest"
    if [ "${2:-no}" = yes ]; then printf '%s\n' 'Contents/embedded.provisionprofile f 644'; fi
    case "${1:-unsigned}" in
      signed | stapled) printf '%s\n' 'Contents/_CodeSignature d 755' 'Contents/_CodeSignature/CodeResources f 644' ;;
    esac
    if [ "${1:-unsigned}" = stapled ]; then printf '%s\n' 'Contents/CodeResources f 644'; fi
  } | LC_ALL=C sort
}

# What APP actually holds, in manifest's form.
holds() {
  (
    # Inside a command substitution bash does not apply set -e, so a
    # directory that cannot be entered has to stop this here, or find
    # would list wherever it was run from instead.
    cd "$1" || exit 1
    find . -mindepth 1 -print | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r p; do
      if [ -L "$p" ]; then
        printf '%s l\n' "$p"
      elif [ -d "$p" ]; then
        printf '%s d %s\n' "$p" "$(stat -f '%Lp' "$p")"
      elif [ -f "$p" ]; then
        printf '%s f %s\n' "$p" "$(stat -f '%Lp' "$p")"
      else
        printf '%s ?\n' "$p"
      fi
    done
  )
}

# profiled VERSION RELEASE: yes if Syndeo.app VERSION holds a provisioning
# profile: a release's (RELEASE yes) from profile-since on; no otherwise.
profiled() {
  if [ "$2" = yes ] && [ "$(syndeo_version_cmp "$1" "$(profile_since)")" != lt ]; then echo yes; else echo no; fi
}

# uninstaller VERSION RELEASE OUT: the uninstaller for Syndeo.app VERSION, a
# release's (RELEASE yes) or a pull request's.
uninstaller() {
  local since=''
  [ "$2" = no ] || since="$(profile_since)"
  bash "$here/package-macos-pkg.sh" --render uninstall "$1" '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign "$3" "$since"
}

assemble() {
  local version="$1" bins="$2" out="$3" profile="${4:-}" release=no app staging
  syndeo_version_valid "$version" || die "'$version' is not a version"
  if [ -n "$profile" ]; then
    [ -f "$profile" ] && [ ! -L "$profile" ] && [ -s "$profile" ] || die "no provisioning profile at $profile"
    syndeo_version_valid "$(profile_since)" || die "ci/macos-app/profile-since does not hold a version"
    [ "$(profiled "$version" yes)" = yes ] ||
      die "$version is before $(profile_since), the first version a release's Syndeo.app carries a provisioning profile in"
    release=yes
  fi
  for b in syndeo-app syndeo-proxy; do
    [ -f "$bins/$b" ] && [ ! -L "$bins/$b" ] || die "no $bins/$b"
  done
  [ -d "$out" ] || die "no directory $out"
  app="$out/Syndeo.app"
  [ ! -e "$app" ] || die "$app already exists"
  # Built beside, then moved into place whole.
  staging="$(mktemp -d "$out/.Syndeo.app.XXXXXX")"
  trap 'rm -rf "$staging"' EXIT
  mkdir -p "$staging/Contents/MacOS" "$staging/Contents/Helpers" "$staging/Contents/Resources"
  sed "s/@VERSION@/$version/g" "$here/macos-app/Info.plist.in" >"$staging/Contents/Info.plist"
  printf 'APPL????' >"$staging/Contents/PkgInfo"
  cp "$bins/syndeo-app" "$staging/Contents/MacOS/Syndeo"
  cp "$bins/syndeo-proxy" "$staging/Contents/Helpers/syndeo-proxy"
  cp "$here/macos-app/Syndeo.icns" "$staging/Contents/Resources/Syndeo.icns"
  if [ "$release" = yes ]; then
    cp "$profile" "$staging/Contents/embedded.provisionprofile"
  fi
  uninstaller "$version" "$release" "$staging/Contents/Resources/uninstall.sh"
  find "$staging" -type d -exec chmod 755 {} +
  find "$staging" -type f -exec chmod 644 {} +
  chmod 755 "$staging/Contents/MacOS/Syndeo" "$staging/Contents/Helpers/syndeo-proxy" \
    "$staging/Contents/Resources/uninstall.sh"
  xattr -cr "$staging" 2>/dev/null || true
  check "$staging" "$version" unsigned "$release" >/dev/null
  mv "$staging" "$app"
  trap - EXIT
  if [ "$release" = yes ]; then say "assembled $app ($version), with its provisioning profile"; else say "assembled $app ($version)"; fi
}

# check APP VERSION [STATE] [RELEASE]: APP is exactly the bundle for
# VERSION, unsigned (the default), signed or stapled; a release's (RELEASE
# yes), its uninstaller knowing profile-since and its provisioning profile
# there from that version on, or a pull request's (no, the default), with
# neither. Prints each failure; returns non-zero if there was any.
check() {
  local app="$1" version="$2" state="${3:-unsigned}" release="${4:-no}" profiled plist bad=0 key want have rendered
  plist="$app/Contents/Info.plist"
  case "$state" in unsigned | signed | stapled) ;; *) die "no state called $state" ;; esac
  case "$release" in yes | no) ;; *) die "release is yes or no, not $release" ;; esac
  syndeo_version_valid "$version" || die "'$version' is not a version"
  profiled="$(profiled "$version" "$release")"
  if [ ! -d "$app/Contents" ] || [ -L "$app" ]; then
    printf '  FAIL  bundle — %s is not a bundle\n' "$app"
    return 1
  fi
  if [ "$(holds "$app")" != "$(manifest "$state" "$profiled")" ]; then
    printf '  FAIL  manifest (%s%s) — %s\n' "$state" "$([ "$profiled" = yes ] && echo ', profiled')" \
      "$(diff <(manifest "$state" "$profiled") <(holds "$app") | grep '^[<>]' | tr '\n' ';')"
    bad=1
  fi
  rendered="$(mktemp)"
  if ! uninstaller "$version" "$release" "$rendered" 2>/dev/null ||
    ! cmp -s "$rendered" "$app/Contents/Resources/uninstall.sh"; then
    printf '  FAIL  uninstall.sh — not exactly the uninstaller rendered for %s%s\n' "$version" "$([ "$release" = yes ] && echo ', a release')"
    bad=1
  fi
  rm -f "$rendered"
  if ! plutil -lint -s "$plist" >/dev/null 2>&1; then
    printf '  FAIL  Info.plist — not a valid property list\n'
    return 1
  fi
  while IFS='=' read -r key want; do
    have="$(plutil -extract "$key" raw -o - "$plist" 2>/dev/null || echo '<missing>')"
    if [ "$have" != "$want" ]; then
      printf '  FAIL  Info.plist %s — %s, not %s\n' "$key" "$have" "$want"
      bad=1
    fi
  done <<EOF
CFBundleIdentifier=com.sum.syndeo
CFBundleExecutable=Syndeo
CFBundleName=Syndeo
CFBundleDisplayName=Syndeo
CFBundlePackageType=APPL
CFBundleIconFile=Syndeo
CFBundleShortVersionString=$version
CFBundleVersion=$version
LSMinimumSystemVersion=13.0
NSPrincipalClass=NSApplication
EOF
  have="$(plutil -extract NSAppTransportSecurity json -o - "$plist" 2>/dev/null || echo '<missing>')"
  if [ "$have" != '{"NSAllowsArbitraryLoadsInWebContent":true}' ]; then
    printf '  FAIL  Info.plist NSAppTransportSecurity — %s, not web content only\n' "$have"
    bad=1
  fi
  have="$(plutil -extract CFBundleURLTypes json -o - "$plist" 2>/dev/null || echo '<missing>')"
  if [ "$have" != '[{"CFBundleTypeRole":"Viewer","CFBundleURLName":"Web site URL","CFBundleURLSchemes":["http","https"]}]' ]; then
    printf '  FAIL  Info.plist CFBundleURLTypes — %s, not http and https alone, as a web browser\n' "$have"
    bad=1
  fi
  if [ "$(cat "$app/Contents/PkgInfo" 2>/dev/null)" != 'APPL????' ]; then
    printf '  FAIL  PkgInfo — not APPL????\n'
    bad=1
  fi
  if ! file -b "$app/Contents/Resources/Syndeo.icns" 2>/dev/null | grep -q '^Mac OS X icon'; then
    printf '  FAIL  icon — Syndeo.icns is not an icon\n'
    bad=1
  fi
  if [ "$bad" = 0 ]; then
    say "$app is exactly Syndeo.app $version, $state$([ "$profiled" = yes ] && echo ', with its provisioning profile')"
  fi
  return "$bad"
}

self_test() {
  local cases=0 wrong=0
  # A case whose preparation fails is a wrong case, not the end of the run:
  # the self-test keeps going and counts it.
  set +e
  # Not local: the EXIT trap runs after this function has returned.
  T="$(mktemp -d)"
  trap 'rm -rf "$T"' EXIT
  mkdir -p "$T/bins"
  printf '#!/bin/sh\n' >"$T/bins/syndeo-app"
  printf '#!/bin/sh\n' >"$T/bins/syndeo-proxy"
  expect() {
    cases=$((cases + 1))
    if "${@:2}" >"$T/out" 2>&1; then got=0; else got=1; fi
    if [ "$got" = "${1%%:*}" ]; then
      printf '  ok    %s\n' "${1#*:}"
    else
      printf '  WRONG %s\n' "${1#*:}"
      sed 's/^/          | /' "$T/out"
      wrong=$((wrong + 1))
    fi
  }
  # fresh: a new bundle for 0.2.0 in $T/out-dir. If assembly fails there is
  # none, and the case after it is wrong.
  fresh() {
    rm -rf "$T/out-dir" && mkdir -p "$T/out-dir"
    bash "$0" 0.2.0 "$T/bins" "$T/out-dir" >/dev/null 2>&1
  }

  printf '\n  package-macos-app.sh: the bundle, exactly\n'
  fresh
  expect "0:assembled from the two binaries: exactly the bundle for 0.2.0, its uninstaller included" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  expect "1:another version is not this bundle" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.1
  expect "1:assembling over an existing bundle: refused" bash "$0" 0.2.0 "$T/bins" "$T/out-dir"
  expect "1:a version that is not one: refused" bash "$0" v0.2 "$T/bins" "$T/out-dir/x"
  mv "$T/bins/syndeo-proxy" "$T/bins/held"
  rm -rf "$T/out2" && mkdir "$T/out2"
  expect "1:without the proxy: refused, and nothing left behind" bash "$0" 0.2.0 "$T/bins" "$T/out2"
  mv "$T/bins/held" "$T/bins/syndeo-proxy"
  cases=$((cases + 1))
  if [ -z "$(ls -A "$T/out2")" ]; then printf '  ok    %s\n' "a refused assembly leaves nothing in the output"; else
    printf '  WRONG %s\n' "a refused assembly leaves nothing in the output"; wrong=$((wrong + 1)); fi

  fresh
  : >"$T/out-dir/Syndeo.app/Contents/Resources/extra"
  expect "1:a file more than the manifest: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  rm "$T/out-dir/Syndeo.app/Contents/Helpers/syndeo-proxy"
  expect "1:the helper missing: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  mv "$T/out-dir/Syndeo.app/Contents/Helpers/syndeo-proxy" "$T/out-dir/Syndeo.app/Contents/MacOS/syndeo-proxy"
  rmdir "$T/out-dir/Syndeo.app/Contents/Helpers"
  expect "1:the helper beside the app's executable instead of in Helpers: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  chmod 644 "$T/out-dir/Syndeo.app/Contents/MacOS/Syndeo"
  expect "1:the executable not executable: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -replace CFBundleIdentifier -string com.example.other "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:another bundle identifier: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -replace LSMinimumSystemVersion -string 12.0 "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:another minimum system: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -insert NSAppTransportSecurity.NSAllowsArbitraryLoads -bool YES "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:App Transport Security turned off for everything, not only web content: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -remove NSAppTransportSecurity "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:web pages held to App Transport Security, so no http page opens: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -remove CFBundleURLTypes "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:no URL types, so it is no web browser to macOS: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -insert CFBundleURLTypes.0.CFBundleURLSchemes.2 -string file "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:a scheme besides http and https (file): caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -remove CFBundleURLTypes.0.CFBundleURLSchemes.1 "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:http without https: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -insert CFBundleURLTypes.1 -json '{"CFBundleURLName":"Other","CFBundleURLSchemes":["x-other"]}' "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:a second URL type, for another scheme: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  plutil -replace CFBundleURLTypes.0.CFBundleTypeRole -string Editor "$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:the URL type's role not Viewer: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  ln -s Syndeo "$T/out-dir/Syndeo.app/Contents/MacOS/link"
  expect "1:a link inside the bundle: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  printf '# changed\n' >>"$T/out-dir/Syndeo.app/Contents/Resources/uninstall.sh"
  expect "1:an uninstaller other than the one rendered for 0.2.0: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  rm "$T/out-dir/Syndeo.app/Contents/Resources/uninstall.sh"
  expect "1:no uninstaller: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  expect "1:an unsigned bundle is not a signed one" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0 signed
  mkdir -m 755 "$T/out-dir/Syndeo.app/Contents/_CodeSignature"
  printf 'seal' >"$T/out-dir/Syndeo.app/Contents/_CodeSignature/CodeResources"
  chmod 644 "$T/out-dir/Syndeo.app/Contents/_CodeSignature/CodeResources"
  expect "0:signed: the signature's two paths as well" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0 signed
  expect "1:a signed bundle is not the unsigned one" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  expect "1:a signed bundle without a ticket is not a stapled one" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0 stapled
  printf 'ticket' >"$T/out-dir/Syndeo.app/Contents/CodeResources"
  chmod 644 "$T/out-dir/Syndeo.app/Contents/CodeResources"
  expect "0:stapled: the ticket as well" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0 stapled
  expect "1:a stapled bundle is not merely a signed one" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0 signed
  expect "1:a state that is not one: refused" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0 notarized
  printf '\n  package-macos-app.sh: a release'"'"'s bundle, with its provisioning profile from %s on\n' "$(profile_since)"
  local since below=0.0.1 above=999.0.0
  since="$(profile_since)"
  printf 'a stand-in profile' >"$T/profile"
  rel() { rm -rf "$T/rel" && mkdir -p "$T/rel" && bash "$0" "$1" "$T/bins" "$T/rel" --profile "$T/profile" >/dev/null 2>&1; }
  expect "0:--profiled: a release's bundle of $since holds the profile" test "$(bash "$0" --profiled "$since")" = yes
  expect "0:--profiled: one of $above does" test "$(bash "$0" --profiled "$above")" = yes
  expect "0:--profiled: one of $below, from before, does not" test "$(bash "$0" --profiled "$below")" = no
  rel "$since"
  expect "0:assembled for $since with the profile: exactly a release's bundle" bash "$0" --check "$T/rel/Syndeo.app" "$since" unsigned release
  cases=$((cases + 1))
  if cmp -s "$T/profile" "$T/rel/Syndeo.app/Contents/embedded.provisionprofile" &&
    [ "$(stat -f '%Lp' "$T/rel/Syndeo.app/Contents/embedded.provisionprofile")" = 644 ]; then
    printf '  ok    %s\n' "  ... the profile in it byte for byte, mode 0644"
  else
    printf '  WRONG %s\n' "  ... the profile in it byte for byte, mode 0644"
    wrong=$((wrong + 1))
  fi
  expect "1:  ... and not a pull request's bundle" bash "$0" --check "$T/rel/Syndeo.app" "$since"
  mkdir -m 755 "$T/rel/Syndeo.app/Contents/_CodeSignature"
  printf 'seal' >"$T/rel/Syndeo.app/Contents/_CodeSignature/CodeResources"
  chmod 644 "$T/rel/Syndeo.app/Contents/_CodeSignature/CodeResources"
  expect "0:  ... signed, still exactly a release's bundle" bash "$0" --check "$T/rel/Syndeo.app" "$since" signed release
  rm "$T/rel/Syndeo.app/Contents/embedded.provisionprofile"
  expect "1:a release's bundle of $since without its profile: caught" bash "$0" --check "$T/rel/Syndeo.app" "$since" signed release
  rm -rf "$T/pr" && mkdir -p "$T/pr" && bash "$0" "$since" "$T/bins" "$T/pr" >/dev/null 2>&1
  expect "1:a pull request's bundle of $since is not a release's: no profile, and its uninstaller knows no version" bash "$0" --check "$T/pr/Syndeo.app" "$since" unsigned release
  cp "$T/profile" "$T/pr/Syndeo.app/Contents/embedded.provisionprofile"
  expect "1:a pull request's bundle with a profile put in it: caught" bash "$0" --check "$T/pr/Syndeo.app" "$since"
  rm -rf "$T/old" && mkdir -p "$T/old" && bash "$0" "$below" "$T/bins" "$T/old" >/dev/null 2>&1
  bash "$here/package-macos-pkg.sh" --render uninstall "$below" '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign \
    "$T/old/Syndeo.app/Contents/Resources/uninstall.sh" "$since"
  expect "0:a release's bundle of $below, from before the profile: no profile, its uninstaller knowing $since" bash "$0" --check "$T/old/Syndeo.app" "$below" unsigned release
  cp "$T/profile" "$T/old/Syndeo.app/Contents/embedded.provisionprofile"
  expect "1:  ... and with a profile, which no bundle of $below had: caught" bash "$0" --check "$T/old/Syndeo.app" "$below" unsigned release
  rm -rf "$T/early" && mkdir -p "$T/early"
  expect "1:assembling $below with a profile, before $since: refused" bash "$0" "$below" "$T/bins" "$T/early" --profile "$T/profile"
  expect "1:assembling with a profile that is not there: refused" bash "$0" "$since" "$T/bins" "$T/early" --profile "$T/no-such-profile"
  cases=$((cases + 1))
  if [ -z "$(ls -A "$T/early")" ]; then printf '  ok    %s\n' "  ... and nothing left behind"; else
    printf '  WRONG %s\n' "  ... and nothing left behind"; wrong=$((wrong + 1)); fi

  printf '\n  package-macos-app.sh: what is not a bundle\n'
  fresh
  printf 'garbage' >"$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:an Info.plist that is not one: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  rm -rf "$T/out-dir/Syndeo.app"
  expect "1:no bundle at all: refused at once, nothing else looked at" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0

  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test)
    [ "$(uname -s)" = Darwin ] || { printf '\n  on %s: plutil and stat -f are macOS tools; not run\n\n' "$(uname -s)"; exit 0; }
    self_test
    ;;
  --manifest)
    [ "$#" -le 3 ] || die "usage: $0 --manifest [unsigned|signed|stapled] [profiled]"
    case "${2:-unsigned}" in unsigned | signed | stapled) ;; *) die "no state called $2" ;; esac
    case "${3:-}" in '' | profiled) ;; *) die "usage: $0 --manifest [unsigned|signed|stapled] [profiled]" ;; esac
    manifest "${2:-unsigned}" "$([ "${3:-}" = profiled ] && echo yes || echo no)"
    ;;
  --profiled)
    [ "$#" = 2 ] && syndeo_version_valid "$2" || die "usage: $0 --profiled <version>"
    profiled "$2" yes
    ;;
  --check)
    [ "$#" -ge 3 ] && [ "$#" -le 5 ] || die "usage: $0 --check <Syndeo.app> <version> [unsigned|signed|stapled] [release]"
    case "${5:-}" in '' | release) ;; *) die "usage: $0 --check <Syndeo.app> <version> [unsigned|signed|stapled] [release]" ;; esac
    check "$2" "$3" "${4:-unsigned}" "$([ "${5:-}" = release ] && echo yes || echo no)"
    ;;
  *)
    if [ "$#" = 5 ] && [ "$4" = --profile ]; then
      assemble "$1" "$2" "$3" "$5"
    else
      [ "$#" = 3 ] || die "usage: $0 <version> <binaries-dir> <out-dir> [--profile <file>] | --check <Syndeo.app> <version> [unsigned|signed|stapled] [release] | --self-test"
      assemble "$1" "$2" "$3"
    fi
    ;;
esac
