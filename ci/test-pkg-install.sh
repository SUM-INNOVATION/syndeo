#!/usr/bin/env bash
# The logs are the runner's: sudo runs each command, never the redirect into
# a log, on purpose.
# shellcheck disable=SC2024
# Install Syndeo.app's package for real on a disposable GitHub-hosted runner,
# take it through every state the package has to handle, and put the runner
# back as it was.
#
#   SYNDEO_ALLOW_SYSTEM_INSTALL_TEST=yes bash ci/test-pkg-install.sh run <pkg> <version>
#   SYNDEO_ALLOW_SYSTEM_INSTALL_TEST=yes bash ci/test-pkg-install.sh cleanup
#
# Two workflow steps, the second `if: always()`. Both run as the runner's own
# user, and use sudo for what needs root, as a person's administrator account
# would; Syndeo.app is opened in the runner's own session. SYNDEO_EXPECT_SIGNED
# (no by default) says whether <pkg> is a signed, notarized and stapled
# release, which Gatekeeper has to accept, or a pull request's, which it has
# to reject.
#
# run:
# 1. The command-line tools as install.sh installs them, from the published
#    v0.1.7 tarball, into /usr/local/bin. Then <pkg>, copied to ~/Downloads and
#    quarantined as a browser leaves a download: Gatekeeper's verdict on it,
#    installed, Gatekeeper's verdict on the installed bundle, and install.sh's
#    commands untouched. Opened by its path, as Finder opens it: its bundle
#    identifier, its executable in the bundle, and its helper running from
#    the bundle, byte for byte the one the package carried, and nothing from
#    /usr/local. The package installed again while it runs is refused, as
#    is its uninstaller; quit, nothing of it left running. Reinstalled.
#    Installed on another volume: refused. Removed with its uninstaller,
#    Syndeo's user data kept.
# 2. The published Syndeo-0.2.0.pkg, downloaded and checked against its
#    release's SHA256SUMS, installed: its receipt 0.2.0, and Syndeo.app 0.2.0
#    byte for byte the bundle it carries, stapled ticket and all. A file of
#    the person's planted in ~/Library/Application Support/Syndeo, and a
#    search engine in Syndeo's preferences. <pkg> over it, as every 0.2.0
#    customer upgrades (a pull request's <pkg>, until it raises the version,
#    is 0.2.0 itself, and reinstalls it): its receipt, and its bundle byte
#    for byte the one <pkg> carries, its signature and its uninstaller with
#    it, nothing of 0.2.0's left; the person's file, the preferences and all
#    Syndeo's user data byte for byte as they were, install.sh's commands
#    untouched. Removed with its uninstaller, the user's data kept.
# 3. The published v0.1.6 and v0.1.7 command-line packages, downloaded and
#    checked against their releases' SHA256SUMS. Over 0.1.6, every kind of
#    change to its installation is refused, nothing changed; then <pkg>
#    removes it, exactly, /usr/local/libexec with it, and nothing else. Over
#    0.1.7, install.sh's commands put over its links are refused; repaired with
#    the 0.1.7 package, it is refused while its syndeo-net runs, started
#    through its command link; then its removal is interrupted by a faulty
#    stand-in after the seven links, which leaves what the runner has to
#    refuse from any other package, and the same stand-in again finishes it.
# 4. Stand-ins (0.0.1 to 0.0.3, made from <pkg>'s own bundle): an upgrade; an
#    upgrade whose postinstall fails, what it leaves, the packages refused
#    over it, and its repair; a downgrade refused; a reinstall; a changed
#    bundle refused; <pkg> over all of that.
# 5. Moved to the Trash as Finder moves it: the receipt stays, the trashed
#    copy's uninstaller refuses, saying how to forget it; an older package is
#    refused; <pkg> installs into /Applications again, the trashed copy
#    untouched. With both copies registered with the runner's LaunchServices
#    by the test, opening by the bundle identifier opens the one in
#    /Applications. Moved to the
#    Trash again, the receipt forgotten as the refusal says, an older package
#    installs, and <pkg> upgrades it.
# 6. Removal: a receipt that cannot be forgotten (exit 2, and the command to
#    forget it), then forgotten as it says; a foreign application where
#    Syndeo.app goes, refused; a last install, and its uninstaller, with the
#    user's data kept throughout.
#
# cleanup, whatever happened: it quits Syndeo, undoes every alteration the run
# recorded, removes only what the run planted and only if it is still what
# was planted, removes the bundles, receipts and v0.1.x paths that were not
# there before, and then requires the runner to be exactly as run found it:
# receipts, /Applications, /usr/local, the user's Syndeo paths,
# LaunchServices' records for the bundle, keychains, trust settings and
# Syndeo's defaults. It fails if anything differs. The runner's
# LaunchServices records of /Applications/Syndeo.app and of the trashed copy
# are removed whether or not a bundle is still there, and each checked gone.
#
# The checks on the environment below prevent accidents; they are not
# security.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# validated_ticket: stapler validate, read only, in up to five attempts.
# shellcheck source=ci/stapler-validate.sh
. "$here/stapler-validate.sh"
NAMES="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit"
BUNDLE_ID=com.sum.syndeo
APP_ID=com.sum.syndeo.app
OLD_ID=com.sum.syndeo.pkg
APP=/Applications/Syndeo.app
BIN=/usr/local/bin
LIBEXEC=/usr/local/libexec
D=/usr/local/libexec/syndeo
ALT=/Volumes/SyndeoAltTest
REPO_URL=https://github.com/SUM-INNOVATION/syndeo/releases/download
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister

say() { printf '%s\n' "$*"; }
section() { printf '\n%s\n' "$*"; }
pass() { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; exit 1; }
# must DESCRIPTION COMMAND...: the test stops at the first thing that is wrong.
must() {
  local what="$1"; shift
  if "$@"; then pass "$what"; else fail "$what"; fi
}
refuse() { printf 'test-pkg-install: refusing: %s\n' "$1" >&2; exit 2; }
as_root() { sudo -n "$@"; }

guard() {
  [ "${SYNDEO_ALLOW_SYSTEM_INSTALL_TEST:-}" = yes ] || refuse "SYNDEO_ALLOW_SYSTEM_INSTALL_TEST is not yes"
  [ "${GITHUB_ACTIONS:-}" = true ] || refuse "GITHUB_ACTIONS is not true"
  [ "${RUNNER_ENVIRONMENT:-}" = github-hosted ] || refuse "RUNNER_ENVIRONMENT is not github-hosted"
  case "${RUNNER_TEMP:-}" in
    /Users/runner/work/_temp | /Users/runner/work/_temp/*) ;;
    *) refuse "RUNNER_TEMP is not under /Users/runner/work/_temp" ;;
  esac
  case "${GITHUB_WORKSPACE:-}" in
    /Users/runner/work/?*) ;;
    *) refuse "GITHUB_WORKSPACE is not under /Users/runner/work/" ;;
  esac
  [ "$(uname -sm)" = "Darwin arm64" ] || refuse "this is not Darwin arm64"
  [ "$(id -un)" = runner ] && [ "$HOME" = /Users/runner ] || refuse "not the runner's own user"
  sudo -n true 2>/dev/null || refuse "sudo needs a password here"
  case "${SYNDEO_EXPECT_SIGNED:-no}" in yes | no) ;; *) refuse "SYNDEO_EXPECT_SIGNED is neither yes nor no" ;; esac
}

ST() { printf '%s' "$RUNNER_TEMP/syndeo-pkg-test"; }

present() { [ -e "$1" ] || [ -L "$1" ]; }

# The user's paths a launched Syndeo.app may create, and the download.
user_paths() {
  printf '%s\n' \
    "$HOME/Library/Application Support/Syndeo" \
    "$HOME/Library/Preferences/$BUNDLE_ID.plist" \
    "$HOME/Library/Caches/$BUNDLE_ID" \
    "$HOME/Library/WebKit/$BUNDLE_ID" \
    "$HOME/Library/HTTPStorages/$BUNDLE_ID" \
    "$HOME/Library/HTTPStorages/$BUNDLE_ID.binarycookies" \
    "$HOME/Library/Saved Application State/$BUNDLE_ID.savedState" \
    "$HOME/.Trash/Syndeo.app"
}

# ------------------------------------------------------------------ records
#
# Everything the test changes is written down before it is changed, so that
# cleanup can undo it whatever happened in between.

# meta PATH: what a planted path is, to tell later whether it is still that.
meta() {
  local p="$1"
  if [ -L "$p" ]; then
    printf 'link|%s|%s' "$(stat -f '%u|%g' "$p")" "$(readlink "$p")"
  elif [ -d "$p" ]; then
    printf 'dir|%s' "$(stat -f '%u|%g|%Lp|%i' "$p")"
  elif [ -f "$p" ]; then
    printf 'file|%s|%s' "$(stat -f '%u|%g|%Lp|%i|%z' "$p")" "$(as_root shasum -a 256 "$p" | cut -d' ' -f1)"
  else
    printf 'absent'
  fi
}

record_plant() { printf '%s\t%s\n' "$1" "$(meta "$1")" >>"$(ST)/planted"; }

# plant KIND PATH [CONTENT|TARGET] [OWNER] [MODE]: create a path the package
# must find, as root, and write down exactly what it is.
plant() {
  local kind="$1" p="$2" arg="${3:-}" owner="${4:-root:wheel}" mode="${5:-}"
  present "$p" && fail "planting $p: it already exists"
  case "$kind" in
    file) printf '%s' "$arg" | as_root tee "$p" >/dev/null && as_root chown "$owner" "$p" && as_root chmod "${mode:-644}" "$p" ;;
    link) as_root ln -s "$arg" "$p" && as_root chown -h "$owner" "$p" ;;
    dir) as_root mkdir "$p" && as_root chown "$owner" "$p" && as_root chmod "${mode:-755}" "$p" ;;
  esac || fail "planting $p"
  record_plant "$p"
}

# planted_meta PATH: what PATH was when planted, matched on the whole path.
planted_meta() {
  awk -F'\t' -v p="$1" '$1 == p { m = $2 } END { if (m != "") print m }' "$(ST)/planted"
}

# unplant PATH: remove a planted path, only if it is still what was planted.
unplant() {
  local p="$1" want
  want="$(planted_meta "$p")"
  [ -n "$want" ] || fail "unplanting $p: it was never planted"
  [ "$(meta "$p")" = "$want" ] || fail "unplanting $p: it is no longer what was planted"
  if [ -d "$p" ] && [ ! -L "$p" ]; then as_root rmdir "$p"; else as_root rm -f "$p"; fi
  forget_plant "$p"
}

# forget_plant PATH: a planted path the package or the test has since taken
# over; it is not cleanup's to remove any more.
forget_plant() {
  awk -F'\t' -v p="$1" '$1 != p' "$(ST)/planted" >"$(ST)/planted.new"
  mv -f "$(ST)/planted.new" "$(ST)/planted"
}

# still_planted PATH: PATH is exactly what was planted, the same file with
# the same bytes.
still_planted() { [ -n "$(planted_meta "$1")" ] && [ "$(meta "$1")" = "$(planted_meta "$1")" ]; }

# plant_default KEY VALUE: a string in Syndeo's preferences, as the person's
# own, through cfprefsd as Syndeo writes them, and written down first, as
# plant writes down a path. unplant_default KEY VALUE removes it, only if it
# is still VALUE; cleanup does the same with what is left.
plant_default() {
  defaults read "$BUNDLE_ID" "$1" >/dev/null 2>&1 && fail "planting the preference $1: it is set already"
  printf '%s\t%s\n' "$1" "$2" >>"$(ST)/planted-defaults"
  defaults write "$BUNDLE_ID" "$1" -string "$2" || fail "planting the preference $1"
}

unplant_default() {
  [ "$(defaults read "$BUNDLE_ID" "$1" 2>/dev/null)" = "$2" ] || fail "unplanting the preference $1: it is no longer what was planted"
  defaults delete "$BUNDLE_ID" "$1" || fail "unplanting the preference $1"
  awk -F'\t' -v k="$1" '$1 != k' "$(ST)/planted-defaults" >"$(ST)/planted-defaults.new"
  mv -f "$(ST)/planted-defaults.new" "$(ST)/planted-defaults"
}

# alter KIND PATH VALUE: change a path's mode, or move it aside to VALUE,
# recording how to undo it; restore_last undoes the newest alteration.
alter() {
  local kind="$1" p="$2" value="$3" undo
  case "$kind" in
    mode) undo="$(stat -f '%Mp%Lp' "$p")"; as_root chmod "$value" "$p" ;;
    aside) undo="$value"; as_root mv "$p" "$value" ;;
  esac || fail "altering $p"
  printf '%s\t%s\t%s\n' "$kind" "$p" "$undo" >>"$(ST)/altered"
}

undo_line() {
  local kind="$1" p="$2" undo="$3"
  case "$kind" in
    mode) as_root chmod "$undo" "$p" ;;
    aside) as_root mv "$undo" "$p" ;;
  esac
}

restore_last() {
  local line kind p undo
  line="$(tail -n 1 "$(ST)/altered")"
  [ -n "$line" ] || fail "nothing to restore"
  kind="${line%%	*}"; line="${line#*	}"; p="${line%%	*}"; undo="${line#*	}"
  undo_line "$kind" "$p" "$undo" || fail "could not undo $kind on $p"
  sed '$d' "$(ST)/altered" >"$(ST)/altered.new" && mv -f "$(ST)/altered.new" "$(ST)/altered"
}

# ------------------------------------------------------------------ looking

snap_path() {
  local p="$1"
  if present "$p"; then
    printf '%s\t%s\t%s\t%s\n' "$p" "$(stat -f '%HT|%Su|%Sg|%Lp|%Mp|%Sf' "$p")" \
      "$(ls -lde "$p" | sed 1d | tr '\n' ';')" "$(xattr -l "$p" 2>/dev/null | shasum | cut -c1-16)"
  else
    printf '%s\tabsent\n' "$p"
  fi
}

# ls_filter: of lsregister -dump on stdin, the records that mention
# com.sum.syndeo, whole.
ls_filter() {
  python3 -I -c '
import re, sys
text = sys.stdin.read()
for block in re.split(r"\n-{40,}\n", text):
    if "com.sum.syndeo" in block:
        print(block.strip())
        print("-" * 40)
'
}

# The LaunchServices records that mention com.sum.syndeo, whole.
ls_records() { "$LSREGISTER" -dump 2>/dev/null | ls_filter; }

# ls_paths_of: the paths of the records on stdin, one per line.
ls_paths_of() { sed -n 's/^path: *//p' | sed 's/ (0x[0-9a-f]*)$//'; }
# ls_paths: the paths of those records, one per line.
ls_paths() { ls_records | ls_paths_of; }
# ls_has PATH: the runner's LaunchServices has a record of PATH.
ls_has() { ls_paths | grep -qxF -- "$1"; }
# ls_unregister PATH: remove the runner's record of PATH. lsregister is the
# test's alone: the package's scripts never touch LaunchServices.
ls_unregister() { "$LSREGISTER" -u "$1" >/dev/null 2>&1 || true; }

# snapshot DIR: what cleanup has to restore, as comparable text.
snapshot() {
  local d="$1" p
  mkdir -p "$d"
  pkgutil --pkgs 2>/dev/null | LC_ALL=C sort >"$d/receipts.txt"
  { ls -1A /Applications | LC_ALL=C sort; snap_path /Applications; } >"$d/applications.txt"
  snapshot_usr_local >"$d/usr-local.txt"
  while IFS= read -r p; do
    if present "$p"; then echo "present $p"; else echo "absent $p"; fi
  done < <(user_paths) >"$d/user-paths.txt"
  ls_records | sed -n 's/^path: *//p' | grep -E "^($APP|$HOME/.Trash/Syndeo.app)( |\(|$)" | LC_ALL=C sort -u >"$d/lsregister.txt"
  {
    security list-keychains -d user
    security list-keychains -d system
    printf 'user trust %s\n' "$(security dump-trust-settings 2>&1 | shasum | cut -c1-16)"
    printf 'admin trust %s\n' "$(security dump-trust-settings -d 2>&1 | shasum | cut -c1-16)"
  } >"$d/security.txt"
  if defaults read "$BUNDLE_ID" >/dev/null 2>&1; then echo present; else echo absent; fi >"$d/defaults.txt"
}

SNAPSHOTS="receipts applications usr-local user-paths lsregister security defaults"

receipt_version() { pkgutil --pkg-info "$1" 2>/dev/null | sed -n 's/^version: //p'; }
receipt_is() { [ "$(receipt_version "$1")" = "$2" ]; }
no_receipt() { ! pkgutil --pkg-info "$1" >/dev/null 2>&1; }
app_version() { plutil -extract CFBundleShortVersionString raw -o - "$APP/Contents/Info.plist" 2>/dev/null; }
app_is() { [ -d "$APP" ] && [ "$(app_version)" = "$1" ]; }

# legacy_paths V: the v0.1.x paths of V, every one.
legacy_paths() {
  local n
  for n in $NAMES; do echo "$BIN/$n"; done
  echo "$D/current"
  for n in $NAMES README.md LICENSE tools/wordcount.wat tools uninstall.sh; do echo "$D/$1/$n"; done
  echo "$D/$1"
  echo "$D"
}

# legacy_left V: how many of them are there.
legacy_left() {
  local p n=0
  while IFS= read -r p; do present "$p" && n=$((n + 1)); done < <(legacy_paths "$1")
  echo "$n"
}

# legacy_hold V NAME: the v0.1.x package V's NAME, started as a person would,
# through its command link in /usr/local/bin, with a home of its own and a
# pipe nobody writes to for its standard input, so that it waits. Sets HELD;
# legacy_unhold stops it.
legacy_hold() {
  local v="$1" n="$2" fifo
  fifo="$(ST)/legacy-stdin"
  [ -p "$fifo" ] || mkfifo "$fifo" || fail "making a pipe for $n"
  exec 7<>"$fifo"
  mkdir -p "$(ST)/legacy-home"
  HOME="$(ST)/legacy-home" SYNDEO_HOME="$(ST)/legacy-home" "$BIN/$n" --home "$(ST)/legacy-home" \
    <"$fifo" >"$(ST)/logs/legacy-$n.log" 2>&1 &
  HELD=$!
  echo "$HELD" >>"$(ST)/pids"
  wait_for 20 sh -c "[ -n \"\$(/usr/sbin/lsof -a -d txt -t -- '$D/$v/$n' 2>/dev/null)\" ]" ||
    { sed 's/^/      | /' "$(ST)/logs/legacy-$n.log"; fail "the v0.1.x $n did not stay running"; }
}

legacy_unhold() {
  kill "$HELD" 2>/dev/null
  wait "$HELD" 2>/dev/null
  exec 7>&-
  return 0
}

# legacy_exact V: the v0.1.x package V installed exactly as its package left
# it, judged by the same library the application package's scripts embed.
legacy_exact() {
  [ -f "$(ST)/libraries.sh" ] || bash "$here/package-macos-pkg.sh" --library "$(ST)/libraries.sh" || return 1
  bash -c '
    ROOT="" EXPECT_UID=0 EXPECT_GID=0 PKGUTIL=/usr/sbin/pkgutil CODESIGN=/usr/bin/codesign SCRIPT=test
    . "$1"
    legacy_plan no
    [ "$nfindings" = 0 ] && [ "$legacy" = whole ] && [ "$legacy_v" = "$2" ] || { printf "%s\n" "$findings" >&2; exit 1; }
  ' _ "$(ST)/libraries.sh" "$1"
}

# package_state: everything of Syndeo's, to compare before and after a step
# that should change nothing.
package_state() {
  local id n
  for id in $APP_ID $OLD_ID; do
    pkgutil --pkg-info "$id" 2>&1 | grep -v '^install-time'
    pkgutil --files "$id" 2>&1
  done
  if present "$APP"; then
    as_root find "$APP" -exec stat -f '%N|%HT|%u|%g|%Lp|%Mp|%Sf|%i|%z' {} + | LC_ALL=C sort
    as_root find "$APP" -type f -exec shasum -a 256 {} + | LC_ALL=C sort
  fi
  for n in $NAMES; do
    snap_path "$BIN/$n"
    [ -L "$BIN/$n" ] && readlink "$BIN/$n"
    [ -f "$BIN/$n" ] && shasum -a 256 "$BIN/$n"
  done
  if present "$D"; then
    find "$D" -exec stat -f '%N|%HT|%u|%g|%Lp|%Mp|%Sf|%i|%z|%Y' {} + | LC_ALL=C sort
    find "$D" -type f -exec shasum -a 256 {} + | LC_ALL=C sort
  fi
  snap_path /usr/local
  snap_path "$BIN"
  snap_path "$LIBEXEC"
  snap_path /Applications
}

# pristine: nothing of Syndeo's on the runner.
pristine() {
  local n
  no_receipt "$APP_ID" && no_receipt "$OLD_ID" || return 1
  present "$APP" && return 1
  present "$D" && return 1
  for n in $NAMES; do present "$BIN/$n" && return 1; done
  return 0
}

# ------------------------------------------------------------------ installing

# The system's installer log, and how many half-second reads note_said()
# gives it: it is written asynchronously, a line can arrive late, and one may
# not arrive at all. The self-test points both at its own.
INSTALL_LOG=/var/log/install.log
SAID_TRIES=10

# install LABEL PKG [TARGET]: installer(8), as root, with -dumplog, so that
# its own detailed log comes back with this invocation, on stderr.
# Everything it printed is kept in LOG.installer, whole, one file per
# invocation, and what /var/log/install.log gained meanwhile in
# LOG.install-log, as supplementary evidence.
install() {
  local label="$1" pkg="$2" target="${3:-/}" before status n log
  n="$(cat "$(ST)/installs" 2>/dev/null || echo 0)"
  n=$((n + 1))
  echo "$n" >"$(ST)/installs"
  log="$(ST)/logs/$(printf '%02d' "$n")-$label"
  [ -e "$log.installer" ] && fail "$log.installer exists; not writing over it"
  before="$(wc -l <"$INSTALL_LOG" | tr -d ' ')"
  sudo -n /usr/sbin/installer -dumplog -pkg "$pkg" -target "$target" >"$log.installer" 2>&1
  status=$?
  last_log="$log"
  last_before="$before"
  log_lines
  return "$status"
}

# log_lines: everything /var/log/install.log has gained since the last
# install began, whole, and only then a view of the lines about Syndeo and
# installer.
log_lines() {
  tail -n +"$((last_before + 1))" "$INSTALL_LOG" >"$last_log.install-log"
  grep -E 'syndeo (preinstall|postinstall)|installer\[' "$last_log.install-log" >"$last_log.install-log.filtered" || true
}

# note_said TEXT: whether the last install's scripts are seen to have said
# TEXT, for the record and nothing else. Looked for in what installer printed
# for that invocation (-dumplog, which on macOS 15 carries installer's own
# lines but not the scripts'), then in what /var/log/install.log gained, read
# again for a few seconds. Installer normally records a script's lines there,
# but does not guarantee it, so no test is decided on this: the installs are
# judged by their status and by what is installed before and after, and every
# line a script says here is asserted directly by ci/verify-pkg.sh's
# st_decisions. Appends where it was seen to LOG.said, and prints it.
note_said() {
  local try where='not seen'
  if grep -qF -- "$1" "$last_log.installer"; then
    where='seen in the -dumplog output'
  else
    for try in $(seq 1 "$SAID_TRIES"); do
      log_lines
      if grep -qF -- "$1" "$last_log.install-log"; then
        where="seen in /var/log/install.log, on read $try"
        break
      fi
      sleep 0.5
    done
  fi
  printf '%s: %s\n' "$where" "$1" >>"$last_log.said"
  printf '%s' "$where"
}

# before_is DECISION: the runner is in the state the preinstall decides
# DECISION on, as ci/macos-pkg/preinstall.in's matrix reads it.
before_is() {
  local app="${1%%, and *}" legacy='' x v
  case "$1" in *", and "*) legacy="${1#*, and }" ;; esac
  case "$app" in
    "installing "*"; Syndeo.app "*", which the receipt names, is no longer in /Applications")
      x="${app#*; Syndeo.app }"; x="${x%%,*}"
      receipt_is "$APP_ID" "$x" && ! present "$APP" || return 1 ;;
    "installing "*) no_receipt "$APP_ID" && ! present "$APP" || return 1 ;;
    "resuming an interrupted installation of "*) no_receipt "$APP_ID" && app_is "${app##* }" || return 1 ;;
    "upgrading from "*)
      x="${app#upgrading from }"; x="${x%% *}"
      receipt_is "$APP_ID" "$x" && app_is "$x" || return 1 ;;
    "reinstalling "*) receipt_is "$APP_ID" "${app##* }" && app_is "${app##* }" || return 1 ;;
    "completing the failed upgrade from "*)
      x="${app#completing the failed upgrade from }"; v="${x##* }"; x="${x%% *}"
      receipt_is "$APP_ID" "$x" && app_is "$v" || return 1 ;;
    *) return 1 ;;
  esac
  case "$legacy" in
    '') no_receipt "$OLD_ID" ;;
    "removing the v0.1.x command-line package "*)
      x="${legacy##* }"
      receipt_is "$OLD_ID" "$x" && legacy_exact "$x" ;;
    "finishing the removal of the v0.1.x command-line package "*)
      x="${legacy##* }"
      receipt_is "$OLD_ID" "$x" && [ "$(legacy_left "$x")" -lt 22 ] ;;
    *) return 1 ;;
  esac
}

# evidence: what the last install's scripts and installer are known to have
# said, from both, for a failure's message.
evidence() {
  grep -hE 'syndeo (preinstall|postinstall)|installer' "$last_log.installer" "$last_log.install-log" 2>/dev/null | sed 's/^/      | /'
}

# refused LABEL PKG TEXT: the install fails, and nothing of Syndeo's changed.
# TEXT, the preinstall's reason, is asserted by ci/verify-pkg.sh's
# st_decisions; here it is only noted.
refused() {
  local label="$1" pkg="$2" text="$3" before seen
  before="$(package_state)"
  if install "$label" "$pkg"; then
    fail "$label: the install succeeded"
  fi
  seen="$(note_said "$text")"
  [ "$(package_state)" = "$before" ] || { evidence; fail "$label: refused, but something changed"; }
  pass "$label: refused, nothing changed (\"$text\": $seen)"
}

# installed LABEL PKG DECISION VERSION: from the state the preinstall decides
# DECISION on, the install succeeds and leaves exactly VERSION, as
# verify-pkg.sh installed checks it. DECISION itself is asserted by
# ci/verify-pkg.sh's st_decisions; here it is only noted.
installed() {
  local label="$1" pkg="$2" text="$3" version="$4" seen
  before_is "$text" || fail "$label: before installing, the runner is not in the state that \"$text\" is decided on"
  install "$label" "$pkg" || { sed 's/^/      | /' "$last_log.installer" "$last_log.install-log"; fail "$label: the install failed"; }
  seen="$(note_said "$text")"
  # The stand-ins' helper is the real one, which says the real version.
  bash "$here/verify-pkg.sh" installed "$version" --helper-says "$REAL_VERSION" >"$last_log.verify" 2>&1 ||
    { sed 's/^/      | /' "$last_log.verify"; fail "$label: the installation is not exactly $version"; }
  pass "$label: installed from the state \"$text\" is decided on, and verify-pkg installed $version passes ($seen)"
}

# failed_install LABEL PKG: a faulty stand-in, whose postinstall fails its
# first run.
failed_install() {
  local label="$1" pkg="$2"
  if install "$label" "$pkg"; then fail "$label: the faulty package installed"; fi
  note_said "syndeo postinstall: test fault" >/dev/null
  pass "$label: the install failed, as its fault makes it"
}

# published_install LABEL PKG: one of the published packages, v0.1.x or
# 0.2.0, installed; what its scripts decide is their own, as published.
published_install() {
  install "$1" "$2" || { sed 's/^/      | /' "$last_log.installer"; fail "$1: the published package did not install"; }
  pass "$1: installed"
}

# failed_upgrade LABEL PKG X W OLDER NEWER: install PKG, a faulty W, over X.
# It fails and leaves what was measured: X's receipt, W's bundle. Everything
# that must refuse that state does, changing nothing, OLDER and NEWER packages
# included; then PKG again completes it.
failed_upgrade() {
  local label="$1" pkg="$2" x="$3" w="$4" older="$5" newer="$6" before
  must "$label: before, Syndeo.app $x with its receipt" eval "receipt_is $APP_ID $x && app_is $x"
  failed_install "$label" "$pkg"
  must "  ... Installer left the receipt at $x, and Syndeo.app $w in its place" eval "receipt_is $APP_ID $x && app_is $w"
  must "  ... verify-pkg installed fails: the receipt is $x's" eval \
    "! bash '$here/verify-pkg.sh' installed $w >'$(ST)/logs/$label.verify' 2>&1 && grep -q \"version '$x'\" '$(ST)/logs/$label.verify'"
  before="$(package_state)"
  must "  ... $w's uninstaller refuses, saying to finish the upgrade" eval \
    "! sudo -n /bin/sh '$APP/Contents/Resources/uninstall.sh' >'$(ST)/logs/$label.uninstall' 2>&1 && grep -q 'the upgrade from $x to $w did not finish' '$(ST)/logs/$label.uninstall'"
  must "  ... changing nothing" test "$(package_state)" = "$before"
  refused "$label-older-package" "$older" "an upgrade from $x to $w did not finish: install the Syndeo $w package again"
  refused "$label-newer-package" "$newer" "an upgrade from $x to $w did not finish: install the Syndeo $w package again"
  installed "$label-the-same-package-again" "$pkg" "completing the failed upgrade from $x to $w" "$w"
}

# ------------------------------------------------------------------ packages

# payload_app PKG DIR: the bundle in PKG's payload, expanded into DIR.
payload_app() {
  rm -rf "$2"
  pkgutil --expand-full "$1" "$2" >/dev/null || fail "expanding $1"
  [ -d "$2/Syndeo.pkg/Payload/Syndeo.app" ] || fail "no Syndeo.app in $1's payload"
}

# bundle_bytes DIR: every path in the bundle DIR, relative to it, with its
# type, and every file's SHA-256, as text.
bundle_bytes() {
  (
    cd "$1" || exit 1
    as_root find . -exec stat -f '%N|%HT' {} + | LC_ALL=C sort
    as_root find . -type f -exec shasum -a 256 {} + | LC_ALL=C sort
  )
}

# same_bundle BUNDLE PAYLOAD: BUNDLE holds exactly the paths of PAYLOAD, a
# package's bundle, each file byte for byte, its code signature and stapled
# ticket among them, and nothing else; says how they differ if not.
same_bundle() {
  local a b
  a="$(bundle_bytes "$1")" && b="$(bundle_bytes "$2")" || return 1
  [ "$a" = "$b" ] && return 0
  diff <(printf '%s\n' "$b") <(printf '%s\n' "$a") | sed 's/^/      | /'
  return 1
}

# cdhash BUNDLE: its code signature's CDHash, as codesign reads it.
cdhash() { codesign -dvvv "$1" 2>&1 | sed -n 's/^CDHash=//p'; }

# standin V: a package of Syndeo.app V, made from the real package's own
# bundle: its versions changed, its uninstaller rendered for V, the stapled
# ticket dropped, the helper and then the bundle signed again ad hoc, and
# built by the real builder.
standin() {
  local v="$1" dir app
  dir="$(ST)/standins/$v"
  app="$dir/app/Syndeo.app"
  mkdir -p "$dir/app"
  ditto "$(ST)/real-payload/Syndeo.pkg/Payload/Syndeo.app" "$app" || return 1
  rm -f "$app/Contents/CodeResources"
  plutil -replace CFBundleShortVersionString -string "$v" "$app/Contents/Info.plist" &&
    plutil -replace CFBundleVersion -string "$v" "$app/Contents/Info.plist" || return 1
  bash "$here/package-macos-pkg.sh" --render uninstall "$v" '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign \
    "$app/Contents/Resources/uninstall.sh" || return 1
  codesign --force --sign - --options runtime "$app/Contents/Helpers/syndeo-proxy" >/dev/null 2>&1 &&
    codesign --force --sign - --options runtime "$app" >/dev/null 2>&1 || return 1
  SOURCE_DATE_EPOCH="$(date +%s)" bash "$here/package-macos-pkg.sh" "$v" "$app" "$dir" >"$dir/build.log" 2>&1 ||
    { sed 's/^/      | /' "$dir/build.log" >&2; return 1; }
  printf '%s' "$dir/Syndeo-$v.pkg"
}

# fault_pkg PKG entry|mid-removal: PKG, a stand-in, with a test fault in its
# postinstall that fails the first run only (ci/pkg-test-fault.sh).
fault_pkg() {
  local pkg="$1" where="$2" dir
  dir="$(ST)/faults/$where"
  mkdir -p "$dir"
  bash "$here/pkg-test-fault.sh" "$pkg" "$dir/$(basename "$pkg")" "$where" "$(ST)/logs/postinstall-fault-$where" >&2 ||
    fail "making the $where fault"
  printf '%s' "$dir/$(basename "$pkg")"
}

# old_release V [NAME]: the package NAME published with vV, by default its
# v0.1.x command-line package, downloaded with that release's SHA256SUMS,
# and checked against it. Sets OLD_PKG.
old_release() {
  local v="$1" name="${2:-syndeo-$1-aarch64-apple-darwin.pkg}" dir
  dir="$(ST)/releases/$v"
  mkdir -p "$dir"
  curl -fsSL --retry 3 -o "$dir/$name" "$REPO_URL/v$v/$name" || fail "downloading $name"
  curl -fsSL --retry 3 -o "$dir/SHA256SUMS" "$REPO_URL/v$v/SHA256SUMS" || fail "downloading v$v's SHA256SUMS"
  (cd "$dir" && shasum -a 256 -c SHA256SUMS --ignore-missing) >"$dir/sums.log" 2>&1 ||
    fail "$name does not match its release's SHA256SUMS: $(cat "$dir/sums.log")"
  [ "$(grep -c "  $name\$" "$dir/SHA256SUMS")" = 1 ] || fail "v$v's SHA256SUMS does not list $name once"
  pass "v$v: $name, $(shasum -a 256 "$dir/$name" | cut -c1-16)..., matches its release's SHA256SUMS"
  OLD_PKG="$dir/$name"
}

# ------------------------------------------------------------------ install.sh

# cli_install DIR: the command-line tools as install.sh installs them, from
# the published v0.1.7 tarball, into DIR, and each command written down as
# the test's own.
cli_install() {
  local dir="$1" n
  SYNDEO_VERSION=0.1.7 SYNDEO_INSTALL_DIR="$dir" SYNDEO_HOME="$(ST)/cli-home" \
    sh "$here/../install.sh" >"$(ST)/logs/install-sh-$(date +%s).log" 2>&1 || fail "install.sh into $dir"
  for n in $NAMES; do
    [ -f "$dir/$n" ] && [ ! -L "$dir/$n" ] || fail "install.sh did not put a regular file at $dir/$n"
    forget_plant "$dir/$n"
    record_plant "$dir/$n"
  done
}

# cli_untouched DIR: every command install.sh put in DIR is still exactly
# what it was.
cli_untouched() {
  local n
  for n in $NAMES; do
    [ "$(meta "$1/$n")" = "$(planted_meta "$1/$n")" ] || return 1
  done
}

cli_remove() {
  local n
  for n in $NAMES; do unplant "$1/$n"; done
}

# ------------------------------------------------------------------ the app

image_of() {
  python3 -I -c '
import ctypes, sys
proc = ctypes.CDLL("/usr/lib/libproc.dylib")
buf = ctypes.create_string_buffer(4096)
n = proc.proc_pidpath(int(sys.argv[1]), buf, 4096)
print(buf.value.decode() if n > 0 else "(gone)")' "$1"
}

appctl() {
  local bin
  bin="$(ST)/appctl"
  if [ ! -x "$bin" ]; then
    swiftc -O -o "$bin" "$here/macos-app/appctl.swift" >"$(ST)/logs/swiftc.log" 2>&1 || return 1
  fi
  "$bin" "$@"
}

wait_for() {
  local until=$((SECONDS + $1))
  shift
  while ! "$@" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$until" ] || return 1
    sleep 0.5
  done
}

# one_app: one answer of LaunchServices for $BUNDLE_ID, kept in APP_PIDS;
# true when that answer names exactly one application, with a pid. The
# count and the pid come from the one answer, never from two: an
# application in one answer and missing from the next would otherwise pass
# as running, with no pid.
APP_PIDS=''
one_app() {
  APP_PIDS="$(appctl pids "$BUNDLE_ID")"
  [ -n "$APP_PIDS" ] && [ "$(printf '%s\n' "$APP_PIDS" | wc -l | tr -d ' ')" = 1 ]
}
none_running() {
  [ -z "$(appctl pids "$BUNDLE_ID")" ] && ! pgrep -f '/Syndeo.app/Contents/MacOS/Syndeo' >/dev/null &&
    ! pgrep -f '/Syndeo.app/Contents/Helpers/syndeo-proxy' >/dev/null
}

# descendants PID: PID and every process under it.
descendants() {
  local p c
  echo "$1"
  for c in $(pgrep -P "$1"); do descendants "$c"; done
  return 0
}

# Syndeo.app is opened two ways, kept apart. launched opens it by its path,
# as Finder does: how a person opens it, and how this test does. opened_by_id
# opens it by its bundle identifier, which needs the runner's own
# LaunchServices to have a record of the bundle; it is used where that is
# the point, which copy the identifier opens. Installer's log says it
# registered the bundle "for uid 0", and the first run of this test found no
# record of it in the runner's LaunchServices 2 seconds after the install,
# so open -b failed (CI 38817241706), though in the measurement it had
# found it.
# When another user's LaunchServices learns of a new bundle is not assumed
# here: opened_by_id registers it itself, as the runner's user, and
# ls_diagnostics keeps what Installer and LaunchServices said each time. (In
# CI 38829502689 the runner's record appeared on its own, some 3 to 12
# seconds after Installer's line: evidence, not relied on.)

# ls_diagnostics LABEL: kept in the logs, and said, never required: the last
# line in which Installer's log says it registered the bundle, and the
# runner's LaunchServices records of the bundle identifier. LaunchServices is
# asked once, and what is said is read from the answer that is kept, never
# from a second query: two can answer differently, as in CI 38829502689. A
# query that fails is kept and said as failed, and nothing it printed is
# taken for its answer. A file already there is not written over.
ls_diagnostics() {
  local f registered records status paths summary
  f="$(ST)/logs/launchservices-$1"
  [ -e "$f" ] && fail "$f exists; not writing over it"
  registered="$(grep -h 'Registered bundle file:///Applications/Syndeo.app/' "$INSTALL_LOG" 2>/dev/null | tail -n 1 | sed 's/.*PackageKit: //')"
  records="$("$LSREGISTER" -dump 2>"$f.query" | ls_filter)" && status=0 || status=$?
  {
    printf 'Installer, last: %s\n' "${registered:-(no such line)}"
    if [ "$status" = 0 ]; then
      printf 'the records of %s in the LaunchServices of %s:\n' "$BUNDLE_ID" "$(id -un)"
      printf '%s\n' "${records:-(none)}"
    else
      printf 'the LaunchServices of %s: the query failed (lsregister -dump, filtered: status %s), and its answer is not kept; lsregister'"'"'s errors:\n' "$(id -un)" "$status"
      cat "$f.query"
    fi
  } >"$f"
  rm -f "$f.query"
  if [ "$status" != 0 ]; then
    summary="not known: the query failed (status $status)"
  else
    paths="$(printf '%s\n' "$records" | ls_paths_of)"
    if grep -qxF -- "$APP" <<<"$paths"; then summary="a record of $APP"; else summary="no record of $APP"; fi
  fi
  say "        recorded ($1): Installer's log, last: ${registered:-no such line}; the runner's LaunchServices: $summary"
}

# launched LABEL: open Syndeo.app by its path, as Finder does, then
# came_up. Sets RUNNING_PID.
launched() {
  local label="$1" log
  log="$(ST)/logs/open-$label.log"
  ls_diagnostics "before-$label"
  open "$APP" >"$log" 2>&1 || fail "$label: open $APP: $(cat "$log")"
  came_up "$label"
  ls_diagnostics "after-$label"
}

# opened_by_id LABEL: register /Applications/Syndeo.app with the runner's
# LaunchServices, as the runner's user, wait for its record, open Syndeo.app
# by its bundle identifier, then came_up. Sets RUNNING_PID.
opened_by_id() {
  local label="$1" log
  log="$(ST)/logs/open-$label.log"
  "$LSREGISTER" -f "$APP" >"$(ST)/logs/lsregister-$label.log" 2>&1 ||
    fail "$label: lsregister -f $APP: $(cat "$(ST)/logs/lsregister-$label.log")"
  must "$label: $APP, registered by the test as the runner's user, has a record in its LaunchServices" wait_for 30 ls_has "$APP"
  ls_diagnostics "$label"
  open -b "$BUNDLE_ID" >"$log" 2>&1 || fail "$label: open -b $BUNDLE_ID: $(cat "$log")"
  came_up "$label"
}

# came_up LABEL: what open started, polled for: within 30 seconds exactly
# one process with the bundle identifier com.sum.syndeo, its pid from the
# same answer (one_app), running the bundle's executable, with a window; its
# helper, and every process under it, runs from /Applications/Syndeo.app,
# and nothing from /usr/local. Sets RUNNING_PID.
came_up() {
  local label="$1" pid helper p img
  wait_for 30 one_app ||
    fail "$label: not exactly one application is $BUNDLE_ID 30 seconds after open; LaunchServices' last answer: [$(printf '%s' "$APP_PIDS" | tr '\n' ' ')]"
  pid="$APP_PIDS"
  must "$label: Syndeo runs, with the bundle identifier $BUNDLE_ID" test "$(appctl bundle "$pid")" = "$BUNDLE_ID"
  must "  ... its executable $APP/Contents/MacOS/Syndeo" test "$(image_of "$pid")" = "$APP/Contents/MacOS/Syndeo"
  must "  ... a window shows" wait_for 30 appctl windows "$pid"
  wait_for 30 sh -c "pgrep -P $pid >/dev/null" || fail "$label: no helper 30 seconds after open"
  helper=''
  for p in $(descendants "$pid"); do
    img="$(image_of "$p")"
    printf '%s %s\n' "$p" "$img" >>"$(ST)/logs/open-$label.processes"
    case "$img" in
      "$APP/Contents/MacOS/Syndeo") ;;
      "$APP/Contents/Helpers/syndeo-proxy") helper="$p" ;;
      *) fail "$label: process $p, under Syndeo, runs $img, not from the bundle" ;;
    esac
  done
  must "  ... its helper runs $APP/Contents/Helpers/syndeo-proxy, and every process under it runs from the bundle, none from /usr/local" test -n "$helper"
  must "  ... the helper is byte for byte the one the package carried" \
    cmp -s "$APP/Contents/Helpers/syndeo-proxy" "$(ST)/real-payload/Syndeo.pkg/Payload/Syndeo.app/Contents/Helpers/syndeo-proxy"
  RUNNING_PID="$pid"
}

quit_app() {
  appctl quit "$BUNDLE_ID" >/dev/null 2>&1 || true
  wait_for 20 none_running || appctl stop "$BUNDLE_ID" >/dev/null 2>&1 || true
  wait_for 10 none_running
}

# user_data: Syndeo's user data, every path and every file's bytes, as text.
user_data() {
  local p
  while IFS= read -r p; do
    case "$p" in "$HOME/.Trash/"*) continue ;; esac
    present "$p" || continue
    find "$p" -exec stat -f '%N|%HT|%Lp|%z' {} + | LC_ALL=C sort
    find "$p" -type f -exec shasum -a 256 {} + | LC_ALL=C sort
  done < <(user_paths)
}

# settled_user_data: user_data, once it has stopped changing: WebKit's
# processes may still write to Syndeo's caches for a moment after it quits.
# Two reads two seconds apart that agree, within a minute.
settled_user_data() {
  local a b
  b="$(user_data)"
  for _ in $(seq 1 30); do
    a="$b"
    sleep 2
    b="$(user_data)"
    [ "$a" = "$b" ] && break
  done
  printf '%s\n' "$b"
}

# user_data_kept BEFORE: the user's data are byte for byte BEFORE; says how
# they differ if not.
user_data_kept() {
  [ "$(user_data)" = "$1" ] && return 0
  diff <(printf '%s\n' "$1") <(user_data) | sed 's/^/      | /'
  return 1
}

# prefs: Syndeo's preferences, the whole domain, as cfprefsd exports it.
prefs() { defaults export "$BUNDLE_ID" - 2>/dev/null; }

# prefs_kept BEFORE: Syndeo's preferences are byte for byte BEFORE; says how
# they differ if not.
prefs_kept() {
  [ "$(prefs)" = "$1" ] && return 0
  diff <(printf '%s\n' "$1") <(prefs) | sed 's/^/      | /'
  return 1
}

# gatekeeper SLUG WHAT PATH TYPE: Gatekeeper's verdict on PATH, kept in
# logs/spctl-SLUG, and what it has to be: accepted as Notarized Developer ID
# for a signed release, rejected otherwise.
gatekeeper() {
  local slug="$1" what="$2" path="$3" type="$4" out status
  out="$(spctl --assess -vv --type "$type" "$path" 2>&1)" && status=0 || status=$?
  printf '%s\nexit %s\n' "$out" "$status" >"$(ST)/logs/spctl-$slug"
  if [ "${SYNDEO_EXPECT_SIGNED:-no}" = yes ]; then
    must "Gatekeeper accepts $what as Notarized Developer ID" eval \
      "[ $status = 0 ] && grep -qx 'source=Notarized Developer ID' '$(ST)/logs/spctl-$slug'"
  else
    must "Gatekeeper rejects $what, unsigned with a Developer ID as a pull request's is ($(printf '%s' "$out" | head -n 1 | sed 's/^[^:]*: //'))" test "$status" != 0
  fi
}

# ------------------------------------------------------------------ run

run() {
  local pkg="$1" version="$2" st n q s1 s2 s3 f1 f3 trashed before
  guard
  st="$(ST)"
  [ -e "$st" ] && refuse "$st exists: this runner has been used for this test before"
  pristine || refuse "Syndeo is already on this runner"
  present "$HOME/.Trash/Syndeo.app" && refuse "there is a Syndeo.app in the Trash already"
  mount | grep -q " on $ALT " && refuse "$ALT is already mounted"
  [ "$(syndeo_cmp "$version" 0.0.3)" = gt ] || refuse "$version is not newer than the stand-ins, 0.0.1 to 0.0.3"
  [ "$(syndeo_cmp "$version" 0.2.0)" != lt ] || refuse "$version is older than the published 0.2.0, which it has to upgrade"
  REAL_VERSION="$version"
  mkdir -p "$st/logs"
  : >"$st/planted"
  : >"$st/planted-defaults"
  : >"$st/altered"
  snapshot "$st/before"
  cp -R "$st/before" "$st/logs/before"

  say "Installing $pkg ($version) on this runner, again and again. It writes only:"
  pkgutil --payload-files "$pkg" | sed 's|^\.|  /Applications|'
  say "The test also installs the published v0.1.6, v0.1.7 and 0.2.0 packages, install.sh's commands in $BIN,"
  say "stand-in packages 0.0.1 to 0.0.3, and a disk image mounted at $ALT; cleanup removes all of it."
  payload_app "$pkg" "$st/real-payload"
  s1="$(standin 0.0.1)" || fail "building the stand-in 0.0.1"
  s2="$(standin 0.0.2)" || fail "building the stand-in 0.0.2"
  s3="$(standin 0.0.3)" || fail "building the stand-in 0.0.3"
  f1="$(fault_pkg "$s1" mid-removal)" || fail "building 0.0.1 with a fault in its removal of v0.1.x"
  f3="$(fault_pkg "$s3" entry)" || fail "building 0.0.3 with a fault at its postinstall's entry"

  section "1. the first install, as a person makes it"
  cli_install "$BIN"
  pass "install.sh put the v0.1.7 command-line tools in $BIN, as regular files"
  q="$HOME/Downloads/$(basename "$pkg")"
  present "$q" && fail "$q is there already"
  cp "$pkg" "$q" || fail "copying the package to ~/Downloads"
  xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");Safari;$(uuidgen)" "$q"
  record_plant "$q"
  xattr -l "$q" >"$st/logs/download-xattr" 2>&1
  pkgutil --check-signature "$q" >"$st/logs/download-signature" 2>&1 || true
  gatekeeper download "the download, quarantined as Safari leaves it" "$q" install
  installed quarantined-download "$q" "installing $version" "$version"
  must "  ... install.sh's commands in $BIN untouched" cli_untouched "$BIN"
  must "  ... /Applications kept its owner, mode and ACL" test "$(snap_path /Applications)" = "$(tail -n 1 "$st/before/applications.txt")"
  must "  ... the installed bundle carries no quarantine" eval "! as_root xattr -r '$APP' 2>/dev/null | grep -q com.apple.quarantine"
  gatekeeper installed-app "the installed Syndeo.app" "$APP" execute
  if [ "${SYNDEO_EXPECT_SIGNED:-no}" = yes ]; then
    must "  ... its notarization ticket is stapled, and validates" validated_ticket "$APP" "$st/logs/stapler-validate-installed-app"
  fi
  launched first
  before="$(package_state)"
  refused while-it-runs "$pkg" "Syndeo is running from $APP (process $RUNNING_PID): quit it first"
  must "  ... Syndeo still runs, the same process" test "$(image_of "$RUNNING_PID")" = "$APP/Contents/MacOS/Syndeo"
  must "  ... and its uninstaller refuses too, changing nothing" eval \
    "! sudo -n /bin/sh '$APP/Contents/Resources/uninstall.sh' >'$st/logs/uninstall-while-it-runs' 2>&1 && grep -q 'Syndeo is running from $APP' '$st/logs/uninstall-while-it-runs' && [ \"\$(package_state)\" = \"\$before\" ]"
  quit_app
  must "quit: nothing of Syndeo left running, no helper" none_running
  user_data >"$st/logs/user-data-after-first-open"
  installed reinstall "$pkg" "reinstalling $version" "$version"

  section "another volume"
  hdiutil create -size 64m -fs APFS -volname SyndeoAltTest "$st/alt.dmg" >/dev/null || fail "creating a disk image"
  hdiutil attach "$st/alt.dmg" -nobrowse -mountpoint "$ALT" >/dev/null || fail "attaching it"
  if install another-volume "$pkg" "$ALT"; then fail "installing on $ALT succeeded"; fi
  must "installing on another volume is refused, and writes nothing there" eval \
    "! present '$ALT/Applications/Syndeo.app' && ! pkgutil --pkgs --volume '$ALT' 2>/dev/null | grep -q syndeo"
  hdiutil detach "$ALT" >/dev/null || fail "detaching $ALT"
  rm -f "$st/alt.dmg"

  section "removed with its uninstaller"
  uninstalled "$version"

  section "2. the published Syndeo-0.2.0.pkg, upgraded, as every 0.2.0 customer's is"
  old_release 0.2.0 Syndeo-0.2.0.pkg
  local p020="$OLD_PKG" asd="$HOME/Library/Application Support/Syndeo" kept from data saved was now
  payload_app "$p020" "$st/published-0.2.0"
  published_install published-0.2.0 "$p020"
  note_said "installing 0.2.0" >/dev/null
  must "  ... its receipt, $APP_ID 0.2.0, and Syndeo.app 0.2.0" eval "receipt_is $APP_ID 0.2.0 && app_is 0.2.0"
  was="$(cdhash "$APP")"
  must "  ... byte for byte the bundle the published package carries, its stapled ticket among it (CDHash $was)" eval \
    "[ -f '$APP/Contents/CodeResources' ] && same_bundle '$APP' '$st/published-0.2.0/Syndeo.pkg/Payload/Syndeo.app'"
  # What a person who has used 0.2.0 has: a file of theirs where Syndeo
  # keeps its data, and a search engine chosen in its Settings.
  present "$asd" || plant dir "$asd" "$(id -un):$(id -gn)" 700
  kept="$asd/kept-across-the-upgrade-from-0.2.0"
  plant file "$kept" "a person's file, which no upgrade may touch
" "$(id -un):$(id -gn)" 600
  plant_default SearchEngine startpage
  must "a file of the person's planted in $asd" still_planted "$kept"
  must "  ... and SearchEngine startpage in Syndeo's preferences, $BUNDLE_ID" test "$(defaults read "$BUNDLE_ID" SearchEngine 2>/dev/null)" = startpage
  data="$(settled_user_data)"
  saved="$(prefs)"
  # A release's <pkg> upgrades 0.2.0. A pull request's is 0.2.0 itself until
  # it raises the version, and the preinstall decides on reinstalling it.
  if [ "$(syndeo_cmp "$version" 0.2.0)" = gt ]; then from="upgrading from 0.2.0 to $version"; else from="reinstalling 0.2.0"; fi
  installed upgrade-from-0.2.0 "$pkg" "$from" "$version"
  note_said "Syndeo.app $version is installed in /Applications" >/dev/null
  now="$(cdhash "$st/real-payload/Syndeo.pkg/Payload/Syndeo.app")"
  must "  ... its receipt, $APP_ID $version" receipt_is "$APP_ID" "$version"
  must "  ... Syndeo.app byte for byte the bundle $(basename "$pkg") carries: nothing of 0.2.0's left" \
    same_bundle "$APP" "$st/real-payload/Syndeo.pkg/Payload/Syndeo.app"
  must "  ... its code signature the package's (CDHash $now), not 0.2.0's ($was)" eval \
    "[ -n '$now' ] && [ '$now' != '$was' ] && [ \"\$(cdhash '$APP')\" = '$now' ]"
  must "  ... its uninstaller $version's" grep -qxF "EMBEDDED='$version'" "$APP/Contents/Resources/uninstall.sh"
  must "  ... the person's file byte for byte, the very file planted" still_planted "$kept"
  must "  ... Syndeo's preferences byte for byte as they were" prefs_kept "$saved"
  must "  ... and all of Syndeo's user data byte for byte as it was" user_data_kept "$data"
  must "  ... install.sh's commands in $BIN untouched" cli_untouched "$BIN"
  uninstalled "$version"
  unplant "$kept"
  if [ -n "$(planted_meta "$asd")" ]; then unplant "$asd"; fi
  unplant_default SearchEngine startpage
  must "the person's file and preference, the test's own, removed again" eval \
    "! present '$kept' && ! defaults read $BUNDLE_ID SearchEngine >/dev/null 2>&1"

  section "3. the published v0.1.x command-line packages"
  cli_remove "$BIN"
  old_release 0.1.6
  local p016="$OLD_PKG"
  old_release 0.1.7
  local p017="$OLD_PKG"
  published_install v0.1.6 "$p016"
  must "v0.1.6, exactly as its package leaves it" legacy_exact 0.1.6
  plant file "$D/0.1.6/notes.txt" "notes
"
  refused legacy-extra-file "$pkg" "$D/0.1.6/notes.txt: is not part of Syndeo 0.1.6"
  unplant "$D/0.1.6/notes.txt"
  alter mode "$D/0.1.6/syndeo-net" 775
  refused legacy-mode-changed "$pkg" "$D/0.1.6/syndeo-net: is not a Regular File owned by root:wheel with mode 755"
  restore_last
  plant link "$D/.current.new" 0.1.6
  refused legacy-current-new "$pkg" "$D/.current.new: is there, from a switch that did not finish"
  unplant "$D/.current.new"
  plant dir "$D/0.0.99"
  refused legacy-second-version "$pkg" "$D: holds 0.0.99 0.1.6; only 0.1.6 may be there"
  unplant "$D/0.0.99"
  alter aside "$D/0.1.6/README.md" "$st/aside-README.md"
  refused legacy-file-missing "$pkg" "$D/0.1.6/README.md: is missing, and the receipt for 0.1.6 lists it"
  restore_last
  alter aside "$BIN/syndeo" "$st/aside-syndeo"
  refused legacy-first-link-missing "$pkg" "the v0.1.x 0.1.6 command-line package is only partly there"
  restore_last
  alter aside "$BIN/syndeo-agent" "$st/aside-syndeo-agent"
  refused legacy-link-missing "$pkg" "$BIN/syndeo-agent: is missing, and the receipt for 0.1.6 lists it"
  restore_last
  must "v0.1.6, after all of that, still exactly as its package left it" legacy_exact 0.1.6
  installed migrate-v0.1.6 "$pkg" "installing $version, and removing the v0.1.x command-line package 0.1.6" "$version"
  note_said "removed $LIBEXEC, which the v0.1.x package created, empty" >/dev/null
  must "  ... nothing of v0.1.6 left, its receipt forgotten" eval "[ \"\$(legacy_left 0.1.6)\" = 0 ] && no_receipt $OLD_ID"
  must "  ... /usr/local exactly as the runner had it before any package: $LIBEXEC, which v0.1.6 created, removed, and $BIN untouched" \
    usr_local_as_before
  uninstalled "$version"

  section "v0.1.7, with install.sh's commands put over its links"
  published_install v0.1.7 "$p017"
  must "v0.1.7, exactly as its package leaves it" legacy_exact 0.1.7
  cli_install "$BIN"
  refused legacy-install-sh-over-links "$pkg" "$BIN/syndeo: is not what the v0.1.x 0.1.7 package put there"
  must "  ... install.sh's commands untouched" cli_untouched "$BIN"
  cli_remove "$BIN"
  published_install v0.1.7-repaired "$p017"
  must "v0.1.7, repaired by its own package, exactly as it leaves it" legacy_exact 0.1.7
  legacy_hold 0.1.7 syndeo-net
  refused legacy-running "$pkg" "the v0.1.x command-line package 0.1.7 is running from $D/0.1.7 (process $HELD): quit it first"
  must "  ... its syndeo-net still runs, from its tree" eval "[ \"\$(image_of $HELD)\" = '$D/0.1.7/syndeo-net' ]"
  legacy_unhold

  section "its removal interrupted, and finished by the same package"
  before_is "installing 0.0.1, and removing the v0.1.x command-line package 0.1.7" || fail "before the interrupted removal, the runner is not v0.1.7 alone"
  failed_install migration-interrupted "$f1"
  must "  ... it stopped after the seven links: the rest of v0.1.7 and its receipt left, Syndeo.app 0.0.1 with no receipt" eval \
    "[ \"\$(legacy_left 0.1.7)\" = 15 ] && receipt_is $OLD_ID 0.1.7 && no_receipt $APP_ID && app_is 0.0.1"
  before="$(package_state)"
  must "  ... 0.0.1's uninstaller refuses, with no receipt to go by, changing nothing" eval \
    "! sudo -n /bin/sh '$APP/Contents/Resources/uninstall.sh' >'$st/logs/uninstall-interrupted' 2>&1 && grep -q 'no Syndeo.app package is installed' '$st/logs/uninstall-interrupted' && [ \"\$(package_state)\" = \"\$before\" ]"
  refused migration-interrupted-other-package "$pkg" "$APP: is Syndeo.app 0.0.1, with no receipt"
  note_said "the v0.1.x 0.1.7 command-line package is only partly there" >/dev/null
  installed migration-resumed "$f1" "resuming an interrupted installation of 0.0.1, and finishing the removal of the v0.1.x command-line package 0.1.7" 0.0.1
  must "  ... nothing of v0.1.7 left, its receipt forgotten, /usr/local as the runner had it" eval \
    "[ \"\$(legacy_left 0.1.7)\" = 0 ] && no_receipt $OLD_ID && usr_local_as_before"

  section "4. upgrades, a failed one, a downgrade, a reinstall"
  installed upgrade-0.0.2 "$s2" "upgrading from 0.0.1 to 0.0.2" 0.0.2
  failed_upgrade fault "$f3" 0.0.2 0.0.3 "$s2" "$pkg"
  refused downgrade "$s2" "Syndeo.app 0.0.3 is installed, and this package is the older 0.0.2"
  installed reinstall-0.0.3 "$s3" "reinstalling 0.0.3" 0.0.3
  plant file "$APP/Contents/Resources/extra" "extra
"
  refused modified-bundle "$s3" "$APP: is not Syndeo.app 0.0.3 as its package installed it. Move it to the Trash, then install this package"
  unplant "$APP/Contents/Resources/extra"
  installed upgrade-real "$pkg" "upgrading from 0.0.3 to $version" "$version"

  section "5. moved to the Trash, as Finder moves it"
  trashed="$HOME/.Trash/Syndeo.app"
  mkdir -p "$HOME/.Trash"
  as_root mv "$APP" "$trashed" || fail "moving Syndeo.app to the Trash"
  "$LSREGISTER" -f "$trashed" >/dev/null 2>&1 || true
  must "the receipt stays behind" receipt_is "$APP_ID" "$version"
  before="$(package_state)"
  must "  ... the trashed copy's uninstaller refuses, saying how to forget the receipt, changing nothing" eval \
    "! sudo -n /bin/sh '$trashed/Contents/Resources/uninstall.sh' >'$st/logs/uninstall-trashed' 2>&1 && grep -qF '    sudo /usr/sbin/pkgutil --forget $APP_ID --volume /' '$st/logs/uninstall-trashed' && [ \"\$(package_state)\" = \"\$before\" ]"
  as_root find "$trashed" -exec stat -f '%N|%HT|%u|%g|%Lp|%i|%z' {} + | LC_ALL=C sort >"$st/trashed.before"
  refused after-the-trash-older "$s3" "Syndeo.app $version is no longer in /Applications, but its package's receipt remains, and this package is the older 0.0.3"
  installed after-the-trash "$pkg" "installing $version; Syndeo.app $version, which the receipt names, is no longer in /Applications" "$version"
  must "  ... the trashed copy untouched" eval "[ \"\$(as_root find '$trashed' -exec stat -f '%N|%HT|%u|%g|%Lp|%i|%z' {} + | LC_ALL=C sort)\" = \"\$(cat '$st/trashed.before')\" ]"
  "$LSREGISTER" -f "$trashed" >"$st/logs/lsregister-trashed.log" 2>&1 ||
    fail "lsregister -f $trashed: $(cat "$st/logs/lsregister-trashed.log")"
  must "the trashed copy, registered by the test as the runner's user, has a record in its LaunchServices" wait_for 30 ls_has "$trashed"
  opened_by_id after-the-trash
  must "  ... with both copies registered, the bundle identifier opened the one in /Applications, not the trashed one" eval \
    "[ \"\$(image_of $RUNNING_PID)\" = '$APP/Contents/MacOS/Syndeo' ] && ! pgrep -f '$trashed/Contents/' >/dev/null"
  quit_app
  must "quit again: nothing left running" none_running
  ls_unregister "$trashed"
  as_root rm -rf "$trashed"
  must "the trashed copy, the test's own, emptied from the Trash" test ! -e "$trashed"

  section "a deliberate downgrade, after the Trash"
  as_root mv "$APP" "$trashed" || fail "moving Syndeo.app to the Trash again"
  "$LSREGISTER" -f "$trashed" >/dev/null 2>&1 || true
  as_root /usr/sbin/pkgutil --forget "$APP_ID" --volume / >/dev/null || fail "forgetting the receipt, as the refusal says"
  must "the receipt forgotten, as the refusal says to" no_receipt "$APP_ID"
  installed downgrade-after-forgetting "$s3" "installing 0.0.3" 0.0.3
  installed upgrade-after-downgrade "$pkg" "upgrading from 0.0.3 to $version" "$version"
  ls_unregister "$trashed"
  as_root rm -rf "$trashed"
  must "the trashed copy emptied from the Trash" test ! -e "$trashed"

  section "6. removal"
  forget_fails "$version"
  plant dir "$APP"
  plant dir "$APP/Contents"
  plant file "$APP/Contents/Info.plist" '<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.example.other</string><key>CFBundleShortVersionString</key><string>1.0.0</string></dict></plist>
'
  refused foreign-app "$pkg" "$APP: is there, and no Syndeo.app package installed it. Move it to the Trash, then install this package"
  unplant "$APP/Contents/Info.plist"
  unplant "$APP/Contents"
  unplant "$APP"
  installed final "$pkg" "installing $version" "$version"
  uninstalled "$version"
  must "nothing of Syndeo's left on the runner, and /usr/local as it had it" eval "pristine && usr_local_as_before"
}

# syndeo_cmp A B: lt, eq or gt.
syndeo_cmp() {
  # shellcheck source=ci/macos-pkg/version.sh
  ( . "$here/macos-pkg/version.sh"; syndeo_version_cmp "$1" "$2" )
}

usr_local_as_before() {
  local now
  now="$(snapshot_usr_local)"
  [ "$now" = "$(cat "$(ST)/before/usr-local.txt")" ] || { diff "$(ST)/before/usr-local.txt" <(printf '%s\n' "$now") | sed 's/^/      | /'; return 1; }
}

snapshot_usr_local() {
  find /usr/local -maxdepth 2 -exec stat -f '%N|%HT|%Su:%Sg|%Lp|%Sf' {} + 2>/dev/null | LC_ALL=C sort
  snap_path /usr/local
  snap_path "$BIN"
  snap_path "$LIBEXEC"
}

# uninstalled V: the bundle's own uninstaller removes Syndeo.app V and its
# receipt, and nothing else: the user's data are byte for byte as they were.
uninstalled() {
  local v="$1" log data
  log="$(ST)/logs/uninstall-$(date +%s)"
  data="$(settled_user_data)"
  [ -n "$data" ] || fail "no user data of Syndeo's to keep: it has not been opened"
  # The runner's record of the bundle, while it is still there to name.
  ls_unregister "$APP"
  sudo -n /bin/sh "$APP/Contents/Resources/uninstall.sh" >"$log" 2>&1 ||
    { sed 's/^/      | /' "$log"; fail "the uninstaller failed"; }
  must "the uninstaller removed Syndeo.app $v, and its receipt" eval "! present '$APP' && no_receipt $APP_ID && grep -q 'removed Syndeo.app $v' '$log'"
  must "  ... the user's data kept, byte for byte" user_data_kept "$data"
}

# forget_fails V: the uninstaller, as a stand-in pkgutil makes forgetting the
# receipt fail and nothing else, exits 2, says the exact command, and has
# removed the bundle; the command then does what it says.
forget_fails() {
  local v="$1" fake out status data
  fake="$(ST)/fake-pkgutil"
  data="$(settled_user_data)"
  as_root mkdir -p "$fake"
  as_root tee "$fake/pkgutil" >/dev/null <<EOF
#!/bin/sh
if [ "\$*" = "--forget $APP_ID --volume /" ]; then
  echo "pkgutil (stand-in): could not forget" >&2
  exit 1
fi
exec /usr/sbin/pkgutil "\$@"
EOF
  as_root chown -R root:wheel "$fake" && as_root chmod 755 "$fake" "$fake/pkgutil" || fail "making the stand-in pkgutil"
  bash "$here/package-macos-pkg.sh" --render uninstall "$v" '' 0 0 "$fake/pkgutil" /usr/bin/codesign "$(ST)/uninstall-forget-fails" ||
    fail "rendering the uninstaller with the stand-in"
  ls_unregister "$APP"
  out="$(sudo -n /bin/sh "$(ST)/uninstall-forget-fails" 2>&1)" && status=0 || status=$?
  printf '%s\n' "$out" >"$(ST)/logs/uninstall-forget-fails"
  must "the receipt cannot be forgotten: the uninstaller exits 2" test "$status" = 2
  must "  ... saying the exact command" eval "printf '%s\n' \"\$out\" | grep -qxF 'syndeo uninstall:     sudo /usr/sbin/pkgutil --forget $APP_ID --volume /'"
  must "  ... the bundle removed, the receipt still there" eval "! present '$APP' && receipt_is $APP_ID $v"
  must "  ... the user's data kept, byte for byte" user_data_kept "$data"
  as_root /usr/sbin/pkgutil --forget "$APP_ID" --volume / >/dev/null || fail "the command it gave"
  must "  ... and the command it gave forgets it" no_receipt "$APP_ID"
  as_root rm -rf "$fake"
}

# ------------------------------------------------------------------ cleanup

was_absent() { grep -qxF "absent $1" "$(ST)/before/user-paths.txt" 2>/dev/null; }

is_syndeo_bundle() {
  [ -d "$1" ] && [ "$(plutil -extract CFBundleIdentifier raw -o - "$1/Contents/Info.plist" 2>/dev/null)" = "$BUNDLE_ID" ]
}

# cleanup: undo everything recorded, whatever happened, then check the runner
# is as it was. Safe to run any number of times.
cleanup() {
  local st problems="" kind p undo want id n t f k
  guard
  st="$(ST)"
  [ -d "$st/before" ] || { say "cleanup: nothing was recorded"; return 0; }
  section "cleanup"
  if [ -f "$st/pids" ]; then
    while read -r p; do kill "$p" 2>/dev/null; done <"$st/pids"
  fi
  quit_app
  pkill -f '/Syndeo.app/Contents/Helpers/syndeo-proxy' 2>/dev/null || true
  if mount | grep -q " on $ALT "; then
    hdiutil detach "$ALT" -force >/dev/null || problems="$problems; could not detach $ALT"
  fi
  rm -f "$st/alt.dmg"
  # Alterations, newest first.
  if [ -s "$st/altered" ]; then
    tail -r "$st/altered" | while IFS='	' read -r kind p undo; do
      undo_line "$kind" "$p" "$undo" || echo "could not undo $kind on $p"
    done
    : >"$st/altered"
  fi
  # Planted paths, newest first, each only if it is still what was planted.
  if [ -s "$st/planted" ]; then
    tail -r "$st/planted" | while IFS='	' read -r p want; do
      if [ "$(meta "$p")" = "$want" ]; then
        if [ -d "$p" ] && [ ! -L "$p" ]; then as_root rmdir "$p" 2>/dev/null || continue; else as_root rm -f "$p"; fi
        forget_plant "$p"
      elif ! present "$p"; then
        forget_plant "$p"
      fi
    done
  fi
  # Preferences planted, each only if it is still what was planted.
  if [ -s "$st/planted-defaults" ]; then
    while IFS='	' read -r k want; do
      if [ "$(defaults read "$BUNDLE_ID" "$k" 2>/dev/null)" = "$want" ]; then
        defaults delete "$BUNDLE_ID" "$k" >/dev/null 2>&1 || problems="$problems; could not remove the preference $k"
      elif defaults read "$BUNDLE_ID" "$k" >/dev/null 2>&1; then
        problems="$problems; the preference $k is no longer as planted"
      fi
    done <"$st/planted-defaults"
    : >"$st/planted-defaults"
  fi
  # The bundles, in /Applications and in the Trash, and their receipt; the
  # runner's LaunchServices records of both paths, whether or not a bundle
  # is still there.
  for p in "$APP" "$HOME/.Trash/Syndeo.app"; do
    if is_syndeo_bundle "$p" || ls_has "$p"; then ls_unregister "$p"; fi
    if is_syndeo_bundle "$p"; then
      as_root rm -rf "$p"
      say "cleanup: removed $p"
    fi
  done
  # The v0.1.x paths, only those exactly as its package names them, and the
  # receipts.
  for n in $NAMES; do
    p="$BIN/$n"
    if [ -L "$p" ] && [ "$(readlink "$p")" = "../libexec/syndeo/current/$n" ]; then as_root rm -f "$p"; fi
  done
  if [ -d "$D" ] && [ ! -L "$D" ]; then
    for p in "$D/current" "$D/.current.new"; do
      if [ -L "$p" ]; then as_root rm -f "$p"; fi
    done
    for t in "$D"/*; do
      [ -d "$t" ] && [ ! -L "$t" ] || continue
      case "${t##*/}" in '' | *[!0-9.]*) continue ;; esac
      for f in $NAMES uninstall.sh README.md LICENSE tools/wordcount.wat; do
        if [ -f "$t/$f" ] && [ ! -L "$t/$f" ]; then as_root rm -f "$t/$f"; fi
      done
      as_root rmdir "$t/tools" 2>/dev/null
      as_root rmdir "$t" 2>/dev/null
    done
    as_root rmdir "$D" 2>/dev/null
  fi
  for id in $APP_ID $OLD_ID; do
    if pkgutil --pkg-info "$id" >/dev/null 2>&1 && ! grep -qxF "$id" "$st/before/receipts.txt"; then
      as_root /usr/sbin/pkgutil --forget "$id" --volume / >/dev/null 2>&1 || problems="$problems; could not forget $id"
    fi
  done
  # Directories that did not exist before and were made while testing.
  for t in "$LIBEXEC" "$BIN" /usr/local; do
    if grep -qxF "$t	absent" "$st/before/usr-local.txt" && [ -d "$t" ] && [ -z "$(ls -A "$t")" ]; then
      as_root rmdir "$t"
    fi
  done
  # What the application made for the runner's user.
  while IFS= read -r p; do
    if present "$p" && was_absent "$p"; then rm -rf "$p" 2>/dev/null || as_root rm -rf "$p"; fi
  done < <(user_paths)
  if grep -qx absent "$st/before/defaults.txt"; then defaults delete "$BUNDLE_ID" >/dev/null 2>&1 || true; fi
  as_root rm -rf "$st/fake-pkgutil" "$st/cli-home"
  # And then everything, compared.
  if [ -s "$st/planted" ]; then
    problems="$problems; planted paths no longer as planted: $(cut -f1 "$st/planted" | tr '\n' ' ')"
  fi
  snapshot "$st/after"
  cp -R "$st/after" "$st/logs/after"
  for f in $SNAPSHOTS; do
    if cmp -s "$st/before/$f.txt" "$st/after/$f.txt"; then
      pass "$f: exactly as the runner had it"
    else
      diff "$st/before/$f.txt" "$st/after/$f.txt" | sed 's/^/      | /'
      problems="$problems; $f differs"
    fi
  done
  # The records, each by name, unless the runner had it before.
  for p in "$APP" "$HOME/.Trash/Syndeo.app"; do
    sed 's/ (0x[0-9a-f]*)$//' "$st/before/lsregister.txt" | grep -qxF -- "$p" && continue
    if ls_has "$p"; then
      problems="$problems; LaunchServices still has a record of $p"
    else
      pass "LaunchServices: no record of $p"
    fi
  done
  pristine || problems="$problems; Syndeo is still installed"
  if [ -n "$problems" ]; then
    say "cleanup: FAILED$problems"
    return 1
  fi
  say "cleanup: the runner is as it was: no receipt, no bundle, nothing in /usr/local, no user data, no LaunchServices record, keychains and trust as they were"
  return 0
}

case "${1:-}" in
  # For the self-test, which uses note_said and log_lines.
  --source-only) return 0 2>/dev/null || exit 0 ;;
  run) [ "$#" = 3 ] || refuse "usage: $0 run <pkg> <version>"; run "$2" "$3"; say ""; say "every step passed" ;;
  cleanup) cleanup ;;
  *) refuse "usage: $0 run <pkg> <version> | cleanup" ;;
esac
