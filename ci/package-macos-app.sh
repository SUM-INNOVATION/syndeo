#!/usr/bin/env bash
# Assemble Syndeo.app, the macOS desktop application, from built binaries.
#
#   ci/package-macos-app.sh <version> <binaries-dir> <out-dir>
#   ci/package-macos-app.sh --check <Syndeo.app> <version>
#   ci/package-macos-app.sh --self-test
#
# The bundle holds exactly these, and nothing else:
#
#   Syndeo.app/Contents/Info.plist              ci/macos-app/Info.plist.in, for <version>
#   Syndeo.app/Contents/PkgInfo                 APPL????
#   Syndeo.app/Contents/MacOS/Syndeo            <binaries-dir>/syndeo-app
#   Syndeo.app/Contents/Helpers/syndeo-proxy    <binaries-dir>/syndeo-proxy
#   Syndeo.app/Contents/Resources/Syndeo.icns   ci/macos-app/Syndeo.icns
#
# The application's one helper is the proxy, which runs the network layer
# in-process. The app finds it in Contents/Helpers and nowhere else (see
# syndeo_shell::supervisor::bundle_helpers). The command-line tools are not
# in the bundle: they ship in the per-user tarball, as before.
#
# Directories are 0755, the executables 0755 and everything else 0644, with
# no extended attributes and no links. The bundle is unsigned; signing it is
# a separate step, the helper first and then the bundle.
#
# --check requires a bundle to be exactly that: the paths and their modes,
# the Info.plist's identifier, executable, versions and minimum system, the
# PkgInfo, and an icon.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ci/macos-pkg/version.sh
. "$here/macos-pkg/version.sh"

say() { printf 'package-macos-app: %s\n' "$*"; }
die() {
  printf 'package-macos-app: %s\n' "$*" >&2
  exit 1
}

# The bundle's paths, relative to Syndeo.app, with each one's type and mode.
manifest() {
  cat <<'EOF'
Contents d 755
Contents/Helpers d 755
Contents/Helpers/syndeo-proxy f 755
Contents/Info.plist f 644
Contents/MacOS d 755
Contents/MacOS/Syndeo f 755
Contents/PkgInfo f 644
Contents/Resources d 755
Contents/Resources/Syndeo.icns f 644
EOF
}

# What APP actually holds, in manifest's form.
holds() {
  (
    cd "$1"
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

assemble() {
  local version="$1" bins="$2" out="$3" app staging
  syndeo_version_valid "$version" || die "'$version' is not a version"
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
  find "$staging" -type d -exec chmod 755 {} +
  find "$staging" -type f -exec chmod 644 {} +
  chmod 755 "$staging/Contents/MacOS/Syndeo" "$staging/Contents/Helpers/syndeo-proxy"
  xattr -cr "$staging" 2>/dev/null || true
  check "$staging" "$version" >/dev/null
  mv "$staging" "$app"
  trap - EXIT
  say "assembled $app ($version)"
}

# check APP VERSION: APP is exactly the bundle for VERSION. Prints each
# failure; returns non-zero if there was any.
check() {
  local app="$1" version="$2" plist bad=0 key want have
  plist="$app/Contents/Info.plist"
  if [ "$(holds "$app")" != "$(manifest)" ]; then
    printf '  FAIL  manifest — %s\n' "$(diff <(manifest) <(holds "$app") | grep '^[<>]' | tr '\n' ';')"
    bad=1
  fi
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
  if [ "$(cat "$app/Contents/PkgInfo" 2>/dev/null)" != 'APPL????' ]; then
    printf '  FAIL  PkgInfo — not APPL????\n'
    bad=1
  fi
  if ! file -b "$app/Contents/Resources/Syndeo.icns" 2>/dev/null | grep -q '^Mac OS X icon'; then
    printf '  FAIL  icon — Syndeo.icns is not an icon\n'
    bad=1
  fi
  if [ "$bad" = 0 ]; then
    say "$app is exactly Syndeo.app $version"
  fi
  return "$bad"
}

self_test() {
  local cases=0 wrong=0
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
  fresh() {
    rm -rf "$T/out-dir" && mkdir -p "$T/out-dir"
    bash "$0" 0.2.0 "$T/bins" "$T/out-dir" >/dev/null
  }

  printf '\n  package-macos-app.sh: the bundle, exactly\n'
  fresh
  expect "0:assembled from the two binaries: exactly the bundle for 0.2.0" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
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
  ln -s Syndeo "$T/out-dir/Syndeo.app/Contents/MacOS/link"
  expect "1:a link inside the bundle: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0
  fresh
  printf 'garbage' >"$T/out-dir/Syndeo.app/Contents/Info.plist"
  expect "1:an Info.plist that is not one: caught" bash "$0" --check "$T/out-dir/Syndeo.app" 0.2.0

  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test)
    [ "$(uname -s)" = Darwin ] || { printf '\n  on %s: plutil and stat -f are macOS tools; not run\n\n' "$(uname -s)"; exit 0; }
    self_test
    ;;
  --check)
    [ "$#" = 3 ] || die "usage: $0 --check <Syndeo.app> <version>"
    check "$2" "$3"
    ;;
  *)
    [ "$#" = 3 ] || die "usage: $0 <version> <binaries-dir> <out-dir> | --check <Syndeo.app> <version> | --self-test"
    assemble "$1" "$2" "$3"
    ;;
esac
