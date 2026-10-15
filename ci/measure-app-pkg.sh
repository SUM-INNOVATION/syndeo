#!/usr/bin/env bash
# The logs are the runner's: sudo runs each command, never the redirect into
# a log, on purpose.
# shellcheck disable=SC2024
# Measure, on a disposable macOS runner, what Syndeo.app's installer package
# will rest on, before it is designed: what Installer, LaunchServices and
# Gatekeeper actually do with an application package, and exactly what the
# published v0.1.x packages leave. It changes nothing shipped and gates
# nothing but putting the runner back.
#
#   ci/measure-app-pkg.sh build <binaries-dir> <out-dir>
#   ci/measure-app-pkg.sh run <out-dir>
#   ci/measure-app-pkg.sh cleanup
#
# build, as the runner: four probe Syndeo.app bundles, 0.0.1 to 0.0.4, each
# assembled by ci/package-macos-app.sh from the built binaries and signed ad
# hoc (the helper, then the bundle). 0.0.1 alone holds one more file,
# Contents/Resources/measure-stale.txt, inside its seal. Each goes into a
# component package with the identifier the design proposes,
# com.sum.syndeo.app, installed at /Applications with BundleIsRelocatable
# false, inside a product archive with the real package's limits. 0.0.4's
# postinstall fails on purpose. pkgbuild's own component defaults are kept.
#
# run, as the runner, with sudo for what needs root:
#   1. the runner as found: receipts, /Applications, /usr/local, the user's
#      Syndeo paths, LaunchServices' records for com.sum.syndeo, trust
#      settings;
#   2. the published v0.1.6 and v0.1.7 packages, downloaded and checked
#      against their releases' SHA256SUMS: 0.1.6 installed, upgraded to
#      0.1.7, removed with 0.1.7's uninstaller, 0.1.7 installed fresh and
#      removed again, with the exact receipt and filesystem state after each;
#   3. the probe packages: 0.0.1 installed from a copy quarantined as a
#      browser leaves a download (spctl's verdicts on it recorded), then
#      opened through LaunchServices; 0.0.2 over it; 0.0.3 over 0.0.2 while
#      0.0.2 runs; 0.0.4, whose postinstall fails, over 0.0.3; 0.0.3 again
#      over what that left; 0.0.1 over 0.0.3 (an older bundle); the app moved
#      to the Trash as Finder would, and 0.0.3 installed again. After each:
#      the receipt, the bundle's files with owners, modes, flags and extended
#      attributes, its signature, spctl's verdict, its version, and
#      LaunchServices' records;
#   4. everything Installer logged meanwhile, and the unified log of
#      installd, Installer, LaunchServices and syspolicyd.
#
# cleanup, whatever happened: quits Syndeo, removes every bundle, receipt,
# v0.1.x path and user path the run created (and nothing it did not
# create), unregisters the bundles from LaunchServices, and then requires
# the runner to be exactly as step 1 found it. The only thing that fails.
#
# Every mode refuses unless SYNDEO_ALLOW_SYSTEM_INSTALL_TEST=yes and
# GITHUB_ACTIONS=true: it installs system-wide.
set -uo pipefail

ID=com.sum.syndeo
PKG_ID=com.sum.syndeo.app
OLD_ID=com.sum.syndeo.pkg
NAMES='syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit'
VERSIONS='0.0.1 0.0.2 0.0.3 0.0.4'
OLD_RELEASES='0.1.6 0.1.7'
REPO_URL=https://github.com/SUM-INNOVATION/syndeo/releases/download
APP=/Applications/Syndeo.app
TRASH="$HOME/.Trash"
TRASHED="$TRASH/Syndeo.app"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOGS="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/syndeo-app-pkg-measure"
# The user's paths a launched Syndeo.app may create.
USER_PATHS=(
  "$HOME/Library/Application Support/Syndeo"
  "$HOME/Library/Preferences/$ID.plist"
  "$HOME/Library/Caches/$ID"
  "$HOME/Library/WebKit/$ID"
  "$HOME/Library/HTTPStorages/$ID"
  "$HOME/Library/HTTPStorages/$ID.binarycookies"
  "$HOME/Library/Saved Application State/$ID.savedState"
  "$HOME/Downloads/Syndeo-0.0.1.pkg"
  "$TRASHED"
)

say() { printf 'measure-app-pkg: %s\n' "$*"; }
section() { printf '\n== %s\n' "$*"; }
measure() { printf '  MEASURE  %s\n' "$*"; }
fail=0
bad() {
  printf '  FAIL  %s\n' "$*"
  fail=1
}

[ "$(uname -s)" = Darwin ] || { say "macOS only"; exit 2; }
if [ "${SYNDEO_ALLOW_SYSTEM_INSTALL_TEST:-}" != yes ] || [ "${GITHUB_ACTIONS:-}" != true ]; then
  say "refusing: this installs system-wide; only on a disposable runner, with SYNDEO_ALLOW_SYSTEM_INSTALL_TEST=yes"
  exit 2
fi
mkdir -p "$LOGS"

# ------------------------------------------------------------------ build

distribution() {
  local v="$1"
  cat <<EOF
<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <title>Syndeo $v (measure)</title>
    <options customize="never" require-scripts="false" hostArchitectures="arm64" rootVolumeOnly="true"/>
    <domains enable_localSystem="true" enable_anywhere="false" enable_currentUserHome="false"/>
    <volume-check>
        <allowed-os-versions>
            <os-version min="13.0"/>
        </allowed-os-versions>
    </volume-check>
    <choices-outline>
        <line choice="$PKG_ID"/>
    </choices-outline>
    <choice id="$PKG_ID" visible="false" title="Syndeo">
        <pkg-ref id="$PKG_ID"/>
    </choice>
    <pkg-ref id="$PKG_ID" version="$v" onConclusion="none">component-$v.pkg</pkg-ref>
    <product id="$ID" version="$v"/>
</installer-gui-script>
EOF
}

build() {
  local bins="$1" out="$2" v d stage plist
  [ -x "$bins/syndeo-app" ] && [ -x "$bins/syndeo-proxy" ] || { say "no syndeo-app and syndeo-proxy in $bins"; exit 2; }
  mkdir -p "$out"
  for v in $VERSIONS; do
    d="$out/app-$v"
    stage="$out/stage-$v"
    rm -rf "$d" "$stage"
    mkdir -p "$d" "$stage"
    bash "$here/package-macos-app.sh" "$v" "$bins" "$d" >/dev/null || { say "assembling $v failed"; exit 1; }
    if [ "$v" = 0.0.1 ]; then
      printf 'only in 0.0.1\n' >"$d/Syndeo.app/Contents/Resources/measure-stale.txt"
      chmod 0644 "$d/Syndeo.app/Contents/Resources/measure-stale.txt"
    fi
    codesign --force --sign - --options runtime "$d/Syndeo.app/Contents/Helpers/syndeo-proxy" &&
      codesign --force --sign - --options runtime "$d/Syndeo.app" &&
      codesign --verify --strict --deep "$d/Syndeo.app" || { say "signing $v failed"; exit 1; }
    ditto "$d/Syndeo.app" "$stage/Syndeo.app"
    plist="$out/component-$v.plist"
    pkgbuild --analyze --root "$stage" "$plist" >/dev/null || { say "pkgbuild --analyze failed"; exit 1; }
    [ "$v" = 0.0.1 ] && cp "$plist" "$out/component-defaults.plist"
    /usr/libexec/PlistBuddy -c 'Set :0:BundleIsRelocatable false' "$plist"
    local scripts=()
    if [ "$v" = 0.0.4 ]; then
      mkdir -p "$out/scripts-$v"
      printf '#!/bin/sh\necho "measure: this postinstall fails on purpose" >&2\nexit 1\n' >"$out/scripts-$v/postinstall"
      chmod 0755 "$out/scripts-$v/postinstall"
      scripts=(--scripts "$out/scripts-$v")
    fi
    pkgbuild --quiet --root "$stage" --component-plist "$plist" --identifier "$PKG_ID" --version "$v" \
      --install-location /Applications ${scripts[@]+"${scripts[@]}"} "$out/component-$v.pkg" || { say "pkgbuild $v failed"; exit 1; }
    distribution "$v" >"$out/Distribution-$v.xml"
    productbuild --quiet --distribution "$out/Distribution-$v.xml" --package-path "$out" "$out/Syndeo-$v.pkg" ||
      { say "productbuild $v failed"; exit 1; }
    say "built $out/Syndeo-$v.pkg"
  done
  say "pkgbuild's component defaults, before BundleIsRelocatable was set false:"
  sed 's/^/    /' "$out/component-defaults.plist"
}

# ------------------------------------------------------------------ looking

# lines FILE: how many lines FILE has, 0 if it is not there.
lines() { if [ -f "$1" ]; then wc -l <"$1" | tr -d ' '; else echo 0; fi; }

# The LaunchServices records that mention com.sum.syndeo, whole.
ls_records() {
  "$LSREGISTER" -dump 2>/dev/null | python3 -I -c '
import re, sys
text = sys.stdin.read()
for block in re.split(r"\n-{40,}\n", text):
    if "com.sum.syndeo" in block:
        print(block.strip())
        print("-" * 40)
'
}

# The receipts, the bundles and the v0.1.x payload as they are, into
# $LOGS/state-NAME/, with a one-line summary.
capture() {
  local name="$1" d="$LOGS/state-$1" p tag id
  mkdir -p "$d"
  pkgutil --pkgs 2>/dev/null | grep -E '^com\.sum\.syndeo' >"$d/receipts.txt" || true
  for id in $PKG_ID $OLD_ID; do
    if pkgutil --pkg-info "$id" >"$d/$id.info" 2>&1; then
      pkgutil --files "$id" >"$d/$id.files" 2>&1
      pkgutil --export-plist "$id" >"$d/$id.plist" 2>&1
      while IFS= read -r p; do
        printf '%s\t%s\n' "$p" "$(pkgutil --file-info "/$p" 2>/dev/null | sed -n 's/^pkgid: //p' | tr '\n' ' ')"
      done <"$d/$id.files" >"$d/$id.file-info"
    else
      rm -f "$d/$id.info"
    fi
  done
  for p in "$APP" "$TRASHED"; do
    [ -e "$p" ] || continue
    tag=$([ "$p" = "$APP" ] && echo app || echo trashed)
    find "$p" -exec stat -f '%N|%HT|%Su:%Sg|%Lp|%Sf|%z' {} + >"$d/$tag.stat" 2>&1
    xattr -lr "$p" >"$d/$tag.xattr" 2>&1
    { codesign --verify --strict --deep -vv "$p" 2>&1; echo "exit $?"; } >"$d/$tag.codesign"
    { spctl --assess -vv --type execute "$p" 2>&1; echo "exit $?"; } >"$d/$tag.spctl"
    /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$p/Contents/Info.plist" >"$d/$tag.version" 2>&1
  done
  if [ -e /usr/local/libexec/syndeo ] || [ -L /usr/local/libexec/syndeo ]; then
    find /usr/local/libexec/syndeo -exec stat -f '%N|%HT|%Su:%Sg|%Lp|%Sf|%z|%Y' {} + >"$d/libexec.stat" 2>&1
    find /usr/local/libexec/syndeo -exec xattr {} + >"$d/libexec.xattr" 2>&1 || true
  fi
  for n in $NAMES; do
    p="/usr/local/bin/$n"
    if [ -e "$p" ] || [ -L "$p" ]; then stat -f '%N|%HT|%Su:%Sg|%Lp|%Sf|%Y' "$p"; fi
  done >"$d/bin.stat"
  stat -f '%N|%HT|%Su:%Sg|%Lp|%Sf' /usr/local /usr/local/bin /usr/local/libexec >"$d/parents.stat" 2>&1
  ls_records >"$d/lsregister.txt"
  measure "$name: receipts [$(tr '\n' ' ' <"$d/receipts.txt")]; $PKG_ID $(sed -n 's/^version: //p' "$d/$PKG_ID.info" 2>/dev/null); $OLD_ID $(sed -n 's/^version: //p' "$d/$OLD_ID.info" 2>/dev/null); app $(cat "$d/app.version" 2>/dev/null || echo absent) ($(tail -n 1 "$d/app.codesign" 2>/dev/null), spctl $(tail -n 1 "$d/app.spctl" 2>/dev/null)); trashed $(cat "$d/trashed.version" 2>/dev/null || echo absent); stale file $([ -e "$APP/Contents/Resources/measure-stale.txt" ] && echo present || echo absent); v0.1.x paths $(lines "$d/libexec.stat") + links $(lines "$d/bin.stat"); LaunchServices records $(grep -c '^path:' "$d/lsregister.txt" || true)"
}

# install LABEL PKG: install PKG as root, keeping Installer's whole account.
install_pkg() {
  local label="$1" pkg="$2" status
  sudo /usr/sbin/installer -verboseR -dumplog -pkg "$pkg" -target / >"$LOGS/installer-$label.log" 2>&1
  status=$?
  measure "installer, $label ($(basename "$pkg")): exit $status; $(grep -E 'installer: (The install|Installation)' "$LOGS/installer-$label.log" | tail -n 1)"
  return 0
}

image_of() {
  python3 -I -c '
import ctypes, sys
proc = ctypes.CDLL("/usr/lib/libproc.dylib")
buf = ctypes.create_string_buffer(4096)
n = proc.proc_pidpath(int(sys.argv[1]), buf, 4096)
print(buf.value.decode() if n > 0 else "(gone)")' "$1"
}

APPCTL="$LOGS/appctl"
appctl() {
  if [ ! -x "$APPCTL" ]; then
    swiftc -O -o "$APPCTL" "$here/macos-app/appctl.swift" >"$LOGS/swiftc.log" 2>&1 || return 1
  fi
  "$APPCTL" "$@"
}

wait_for() {
  local until=$((SECONDS + $1))
  shift
  while ! "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$until" ] || return 1
    sleep 0.5
  done
}

running() { [ -n "$(appctl pids "$ID")" ]; }

# opened LABEL: open the app through LaunchServices, by bundle identifier, as
# a person or Finder would, and say what started.
opened() {
  local label="$1" pid helper
  open -b "$ID" >"$LOGS/open-$label.log" 2>&1
  if ! wait_for 30 running; then
    measure "opened by bundle id ($label): nothing running 30 s later: $(cat "$LOGS/open-$label.log")"
    return 1
  fi
  pid="$(appctl pids "$ID" | head -n 1)"
  wait_for 30 appctl windows "$pid" || true
  wait_for 15 pgrep -P "$pid" || true
  helper="$(pgrep -P "$pid" | head -n 1)"
  measure "opened by bundle id ($label): pid $pid runs $(image_of "$pid"), version $(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null); helper ${helper:-none}${helper:+ runs $(image_of "$helper")}; windows: $(appctl windows "$pid" | tr '\n' ';')"
  RUNNING_PID="$pid" RUNNING_HELPER="$helper"
}

quit_app() {
  appctl quit "$ID" >/dev/null 2>&1 || true
  wait_for 20 sh -c '[ -z "$(pgrep -f "/Contents/MacOS/Syndeo$")" ]' || appctl stop "$ID" >/dev/null 2>&1 || true
}

# ------------------------------------------------------------------ snapshot

# snapshot DIR: what cleanup has to restore, as comparable text.
snapshot() {
  local d="$1" p
  mkdir -p "$d"
  pkgutil --pkgs 2>/dev/null | LC_ALL=C sort >"$d/receipts.txt"
  ls -1A /Applications | LC_ALL=C sort >"$d/applications.txt"
  find /usr/local -maxdepth 2 -exec stat -f '%N|%HT|%Su:%Sg|%Lp|%Sf' {} + 2>/dev/null | LC_ALL=C sort >"$d/usr-local.txt"
  for p in "${USER_PATHS[@]}"; do
    if [ -e "$p" ] || [ -L "$p" ]; then echo "present $p"; else echo "absent $p"; fi
  done >"$d/user-paths.txt"
  # Only the bundles this run installs or moves: LaunchServices may register
  # the probe bundles in the build directory on its own, at any time.
  ls_records | sed -n 's/^path: *//p' | grep -E "^($APP|$TRASHED)( |\(|$)" | LC_ALL=C sort -u >"$d/lsregister.txt"
  security dump-trust-settings >"$d/trust.txt" 2>&1 || true
  if defaults read "$ID" >/dev/null 2>&1; then echo present; else echo absent; fi >"$d/defaults.txt"
}

# ------------------------------------------------------------------ run

old_releases() {
  local v name dir pkg
  section "the published v0.1.x packages"
  for v in $OLD_RELEASES; do
    name="syndeo-$v-aarch64-apple-darwin.pkg"
    dir="$LOGS/release-$v"
    mkdir -p "$dir"
    if ! curl -fsSL --retry 3 -o "$dir/$name" "$REPO_URL/v$v/$name" ||
      ! curl -fsSL --retry 3 -o "$dir/SHA256SUMS" "$REPO_URL/v$v/SHA256SUMS"; then
      measure "v$v: could not download its package or SHA256SUMS"
      return 1
    fi
    if (cd "$dir" && shasum -a 256 -c SHA256SUMS --ignore-missing) >"$dir/sums.log" 2>&1; then
      measure "v$v: $name matches its release's SHA256SUMS: $(shasum -a 256 "$dir/$name" | cut -d' ' -f1)"
    else
      measure "v$v: $name does NOT match its release's SHA256SUMS: $(cat "$dir/sums.log")"
      return 1
    fi
    pkgutil --check-signature "$dir/$name" >"$dir/signature.txt" 2>&1 || true
    { spctl --assess -vv --type install "$dir/$name" 2>&1; echo "exit $?"; } >"$dir/spctl.txt"
    measure "v$v: signature: $(sed -n 's/^ *Status: //p' "$dir/signature.txt" | head -n 1); spctl: $(tr '\n' ' ' <"$dir/spctl.txt")"
  done
  pkg() { echo "$LOGS/release-$1/syndeo-$1-aarch64-apple-darwin.pkg"; }
  install_pkg v0.1.6 "$(pkg 0.1.6)"
  capture v0.1.6-installed
  install_pkg v0.1.7-over-v0.1.6 "$(pkg 0.1.7)"
  capture v0.1.7-over-v0.1.6
  sudo /bin/sh /usr/local/libexec/syndeo/0.1.7/uninstall.sh >"$LOGS/uninstall-v0.1.7-1.log" 2>&1
  measure "v0.1.7's uninstaller after the upgrade: exit $? ($(tail -n 1 "$LOGS/uninstall-v0.1.7-1.log"))"
  capture v0.1.7-uninstalled-after-upgrade
  install_pkg v0.1.7-fresh "$(pkg 0.1.7)"
  capture v0.1.7-installed-fresh
  sudo /bin/sh /usr/local/libexec/syndeo/0.1.7/uninstall.sh >"$LOGS/uninstall-v0.1.7-2.log" 2>&1
  measure "v0.1.7's uninstaller after a fresh install: exit $? ($(tail -n 1 "$LOGS/uninstall-v0.1.7-2.log"))"
  capture v0.1.7-uninstalled
}

probes() {
  local out="$1" q
  section "the application package, quarantined as a download"
  cp "$out/Syndeo-0.0.1.pkg" "$HOME/Downloads/Syndeo-0.0.1.pkg"
  q="0083;$(printf '%x' "$(date +%s)");Safari;$(uuidgen)"
  xattr -w com.apple.quarantine "$q" "$HOME/Downloads/Syndeo-0.0.1.pkg"
  measure "the download's extended attributes: $(xattr -l "$HOME/Downloads/Syndeo-0.0.1.pkg" | tr '\n' ' ')"
  pkgutil --check-signature "$HOME/Downloads/Syndeo-0.0.1.pkg" >"$LOGS/probe-signature.txt" 2>&1 || true
  measure "its signature: $(head -n 2 "$LOGS/probe-signature.txt" | tr '\n' ' ')"
  for t in install open; do
    if [ "$t" = open ]; then
      spctl --assess -vv --type open --context context:primary-signature "$HOME/Downloads/Syndeo-0.0.1.pkg" >"$LOGS/probe-spctl-$t.txt" 2>&1
    else
      spctl --assess -vv --type install "$HOME/Downloads/Syndeo-0.0.1.pkg" >"$LOGS/probe-spctl-$t.txt" 2>&1
    fi
    measure "spctl --type $t on the quarantined download: exit $? ($(tr '\n' ' ' <"$LOGS/probe-spctl-$t.txt"))"
  done
  if command -v syspolicy_check >/dev/null 2>&1; then
    syspolicy_check distribution "$out/app-0.0.1/Syndeo.app" >"$LOGS/probe-syspolicy-check.txt" 2>&1
    measure "syspolicy_check distribution on the ad-hoc app: exit $? ($(head -c 400 "$LOGS/probe-syspolicy-check.txt" | tr '\n' ' '))"
  fi
  install_pkg 0.0.1-quarantined "$HOME/Downloads/Syndeo-0.0.1.pkg"
  capture 0.0.1-installed
  opened 0.0.1 || true
  measure "the bundle's extended attributes after it was opened: $(xattr -lr "$APP" 2>/dev/null | grep -c . | tr -d ' ') lines ($(xattr -lr "$APP" 2>/dev/null | head -n 4 | tr '\n' ' '))"
  quit_app
  capture 0.0.1-after-open

  section "an upgrade under the same identifier"
  install_pkg 0.0.2-over-0.0.1 "$out/Syndeo-0.0.2.pkg"
  capture 0.0.2-over-0.0.1
  opened 0.0.2 || true

  section "an upgrade while the app runs"
  install_pkg 0.0.3-over-running-0.0.2 "$out/Syndeo-0.0.3.pkg"
  measure "after it: pid ${RUNNING_PID:-?} $(kill -0 "${RUNNING_PID:-0}" 2>/dev/null && echo "still runs $(image_of "$RUNNING_PID")" || echo gone); helper ${RUNNING_HELPER:-?} $(kill -0 "${RUNNING_HELPER:-0}" 2>/dev/null && echo "still runs $(image_of "$RUNNING_HELPER")" || echo gone); windows: $(appctl windows "${RUNNING_PID:-0}" 2>/dev/null | tr '\n' ';')"
  capture 0.0.3-over-running-0.0.2
  quit_app
  opened 0.0.3-after-upgrade || true
  quit_app

  section "a failed upgrade, and what follows it"
  install_pkg 0.0.4-failing-over-0.0.3 "$out/Syndeo-0.0.4.pkg"
  capture 0.0.4-failed-over-0.0.3
  install_pkg 0.0.3-after-the-failure "$out/Syndeo-0.0.3.pkg"
  capture 0.0.3-after-the-failure
  install_pkg 0.0.1-over-0.0.3-older "$out/Syndeo-0.0.1.pkg"
  capture 0.0.1-over-0.0.3-older
  install_pkg 0.0.3-same-again "$out/Syndeo-0.0.3.pkg"
  capture 0.0.3-reinstalled

  section "moved to the Trash, as Finder would, then installed again"
  mkdir -p "$TRASH"
  sudo mv "$APP" "$TRASHED"
  "$LSREGISTER" -f "$TRASHED" >/dev/null 2>&1 || true
  capture 0.0.3-in-the-trash
  install_pkg 0.0.3-after-the-trash "$out/Syndeo-0.0.3.pkg"
  capture 0.0.3-after-the-trash
  opened 0.0.3-after-the-trash || true
  quit_app
}

run() {
  local out="$1" start
  for v in $VERSIONS; do [ -f "$out/Syndeo-$v.pkg" ] || { say "no $out/Syndeo-$v.pkg; run build first"; exit 2; }; done
  start="$(date '+%Y-%m-%d %H:%M:%S')"
  echo "$start" >"$LOGS/started"
  wc -l </var/log/install.log | tr -d ' ' >"$LOGS/install-log-lines" 2>/dev/null || echo 0 >"$LOGS/install-log-lines"
  section "the runner as found"
  snapshot "$LOGS/before"
  measure "receipts: $(grep -c . "$LOGS/before/receipts.txt"), of which Syndeo's: $(grep -c '^com\.sum\.syndeo' "$LOGS/before/receipts.txt"); /Applications/Syndeo.app $([ -e "$APP" ] && echo present || echo absent); /usr/local: $(cut -d'|' -f1-3 "$LOGS/before/usr-local.txt" | tr '\n' ' ')"
  if grep -q '^com\.sum\.syndeo' "$LOGS/before/receipts.txt" || [ -e "$APP" ] || [ -e /usr/local/libexec/syndeo ]; then
    bad "the runner already has Syndeo on it; refusing to measure over it"
    return 1
  fi
  old_releases || bad "the v0.1.x measurements did not finish"
  probes "$out" || bad "the application package measurements did not finish"
  diagnostics
}

diagnostics() {
  local from start
  section "diagnostics"
  from="$(cat "$LOGS/install-log-lines" 2>/dev/null || echo 0)"
  tail -n "+$((from + 1))" /var/log/install.log >"$LOGS/install.log" 2>/dev/null || sudo tail -n "+$((from + 1))" /var/log/install.log >"$LOGS/install.log"
  start="$(cat "$LOGS/started" 2>/dev/null)"
  log show --style syslog --info --debug ${start:+--start "$start"} \
    --predicate 'process == "installd" OR process == "Installer" OR process == "installer" OR process == "package_script_service" OR process == "lsd" OR subsystem BEGINSWITH "com.apple.launchservices" OR process == "syspolicyd"' \
    >"$LOGS/unified.log" 2>&1 || true
  measure "kept: install.log ($(wc -l <"$LOGS/install.log" | tr -d ' ') lines), the unified log ($(wc -l <"$LOGS/unified.log" | tr -d ' ') lines), every installer -dumplog, and each state's receipts, files, signatures and LaunchServices records"
}

# ------------------------------------------------------------------ cleanup

was_absent() { grep -qxF "absent $1" "$LOGS/before/user-paths.txt" 2>/dev/null; }

is_syndeo_bundle() {
  [ -d "$1" ] && [ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist" 2>/dev/null)" = "$ID" ]
}

cleanup() {
  local p id n t
  section "putting the runner back"
  if [ ! -d "$LOGS/before" ]; then
    say "no snapshot of the runner as found; nothing to compare against, and nothing removed"
    return 0
  fi
  quit_app
  pkill -f '/Contents/Helpers/syndeo-proxy' 2>/dev/null || true
  for p in "$APP" "$TRASHED"; do
    if is_syndeo_bundle "$p"; then
      "$LSREGISTER" -u "$p" >/dev/null 2>&1 || true
      sudo rm -rf "$p"
      say "removed $p"
    fi
  done
  for id in $PKG_ID $OLD_ID; do
    if pkgutil --pkg-info "$id" >/dev/null 2>&1 && ! grep -qxF "$id" "$LOGS/before/receipts.txt"; then
      if [ "$id" = "$OLD_ID" ] && [ -L /usr/local/libexec/syndeo/current ]; then
        sudo /bin/sh "/usr/local/libexec/syndeo/$(readlink /usr/local/libexec/syndeo/current)/uninstall.sh" >/dev/null 2>&1 || true
      fi
      if pkgutil --pkg-info "$id" >/dev/null 2>&1; then
        sudo pkgutil --forget "$id" >/dev/null 2>&1 || true
      fi
      say "forgot $id"
    fi
  done
  # What the v0.1.x packages put in /usr/local, if anything is still there:
  # only links that lead into the private directory, and that directory.
  if ! grep -q '^/usr/local/libexec/syndeo|' "$LOGS/before/usr-local.txt"; then
    # (each link only if it leads where the package's own do)
    for n in $NAMES; do
      t="$(readlink "/usr/local/bin/$n" 2>/dev/null || true)"
      if [ "$t" = "../libexec/syndeo/current/$n" ]; then sudo rm -f "/usr/local/bin/$n"; fi
    done
    if [ -e /usr/local/libexec/syndeo ] || [ -L /usr/local/libexec/syndeo ]; then sudo rm -rf /usr/local/libexec/syndeo; say "removed /usr/local/libexec/syndeo"; fi
  fi
  # The v0.1.x packages create /usr/local/libexec and /usr/local/bin when
  # they are missing, and their uninstaller leaves them: removed if they were
  # not there before, and only if empty.
  for t in /usr/local/libexec /usr/local/bin /usr/local; do
    if ! grep -q "^$t|" "$LOGS/before/usr-local.txt" && [ -d "$t" ] && [ -z "$(ls -A "$t")" ]; then
      sudo rmdir "$t" && say "removed $t, empty, which was not there before"
    fi
  done
  for p in "${USER_PATHS[@]}"; do
    if { [ -e "$p" ] || [ -L "$p" ]; } && was_absent "$p"; then
      rm -rf "$p" 2>/dev/null || sudo rm -rf "$p"
      say "removed $p"
    fi
  done
  if grep -qx absent "$LOGS/before/defaults.txt" 2>/dev/null; then defaults delete "$ID" >/dev/null 2>&1 || true; fi
  snapshot "$LOGS/after"
  for f in receipts applications usr-local user-paths lsregister trust defaults; do
    if cmp -s "$LOGS/before/$f.txt" "$LOGS/after/$f.txt"; then
      printf '  PASS  %s exactly as found\n' "$f"
    else
      bad "$f differs from the runner as found: $(diff "$LOGS/before/$f.txt" "$LOGS/after/$f.txt" | grep '^[<>]' | head -n 6 | tr '\n' ' ')"
    fi
  done
  return "$fail"
}

case "${1:-}" in
  build) shift; [ "$#" = 2 ] || { say "usage: $0 build <binaries-dir> <out-dir>"; exit 2; }; build "$@" ;;
  run) shift; [ "$#" = 1 ] || { say "usage: $0 run <out-dir>"; exit 2; }; run "$@"; printf '\n  measured; %s\n' "$([ "$fail" = 0 ] && echo 'every step finished' || echo 'some step did not finish (reported, not a gate)')"; exit 0 ;;
  cleanup) cleanup; exit $? ;;
  *) say "usage: $0 build <binaries-dir> <out-dir> | run <out-dir> | cleanup"; exit 2 ;;
esac
