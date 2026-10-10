#!/usr/bin/env bash
# Install Syndeo's macOS package for real on a disposable GitHub-hosted
# runner, take it through every state the package has to handle, and put the
# runner back as it was.
#
#   sudo --preserve-env=SYNDEO_ALLOW_SYSTEM_INSTALL_TEST,GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_TEMP,GITHUB_WORKSPACE \
#     bash ci/test-pkg-install.sh --system-before <pkg> <version>
#   SYNDEO_ALLOW_SYSTEM_INSTALL_TEST=yes bash ci/test-pkg-install.sh --user <version>
#   sudo --preserve-env=... bash ci/test-pkg-install.sh --system-after <pkg> <version>
#   sudo --preserve-env=... bash ci/test-pkg-install.sh --cleanup
#
# The four are separate workflow steps, the last one `if: always()`.
#
# --system-before, as root:
# - installing on another volume is refused;
# - foreign commands are refused, and left exactly as they were;
# - unsafe parent directories are refused;
# - an orphaned private directory is refused;
# - an interrupted first install is resumed, then removed again;
# - with /usr/local/bin owned by runner:admin, mode 0775, stand-in packages:
#   - 0.0.9, then 0.0.10 and 0.0.11 while the commands are started over and
#     over: while Installer runs they fail only with ENOENT or EINVAL, every
#     one that starts is one whole version, and none fails afterwards. One
#     version is left, and /usr/local/bin's owner and mode are untouched;
#   - an older package refused;
#   - 0.0.12, whose postinstall fails at its entry, and 0.0.13, whose
#     postinstall fails just after its switch: each leaves the state measured
#     on a runner, in which verify-pkg installed fails, the uninstaller
#     refuses, an older and a newer package are refused, all changing
#     nothing, and the same package again completes the upgrade;
#   - 0.0.15, made of the real binaries;
# - the real package is installed over that, while a 0.0.15 syndeo doctor,
#   held after it found its directory, waits: afterwards every sibling it
#   looks for is refused as removed during an upgrade, and it starts nothing.
# It leaves the real package installed.
#
# --user, as runner: the commands from PATH, doctor and its siblings, the
# example tools seeded on first run, the agent listing them, a fetch and a
# cache hit, a supervisor's children, and syndeo-webkit's proxy. That last one
# uses a throwaway keychain holding the proxy's authority without trust
# settings, and the user's keychain search list is restored by a trap.
#
# --system-after, as root:
# - a same-version reinstall, then reinstalls refused over every kind of
#   altered tree;
# - a path the receipt lists, missing: verify-pkg installed fails and the
#   uninstaller refuses, changing nothing; the same package repairs it;
# - a downgrade refused;
# - disagreement between current, the receipt and the commands refused, and
#   a second version beside the installed one;
# - the uninstaller refusing, all or nothing, with content planted in each
#   place;
# - a receipt that cannot be forgotten;
# - and finally a clean removal.
#
# --cleanup, as root, whatever happened: it undoes everything the steps
# recorded, removes only planted paths that still match what was planted,
# and then checks every affected path. That covers existence, owner, group,
# mode, ACL, flags and extended attributes, the keychain search lists, the
# trust settings, the receipts and the homes. It fails if anything differs.
#
# The checks on the environment below prevent accidents; they are not
# security. Every mode refuses unless all of them hold, and the system modes
# also need root and a runner with no trace of Syndeo when they start.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NAMES="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit"
ID=com.sum.syndeo.pkg
BIN=/usr/local/bin
LIBEXEC=/usr/local/libexec
D=/usr/local/libexec/syndeo
ALT=/Volumes/SyndeoAltTest
RUNNER=runner

say() { printf '%s\n' "$*"; }
pass() { printf '  \033[32mPASS\033[0m  %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; exit 1; }
# must DESCRIPTION COMMAND...: the test stops at the first thing that is wrong.
must() {
  local what="$1"; shift
  if "$@"; then pass "$what"; else fail "$what"; fi
}
refuse() { printf 'test-pkg-install: refusing: %s\n' "$1" >&2; exit 2; }

guard_runner() {
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
  [ -d /Users/runner ] || refuse "there is no /Users/runner"
  [ "$(uname -sm)" = "Darwin arm64" ] || refuse "this is not Darwin arm64"
}

guard_root() {
  guard_runner
  [ "$(id -u)" = 0 ] || refuse "not root"
}

ST() { printf '%s' "$RUNNER_TEMP/syndeo-pkg-test"; }
US() { printf '%s' "$RUNNER_TEMP/syndeo-pkg-user"; }

present() { [ -e "$1" ] || [ -L "$1" ]; }

# ------------------------------------------------------------------ records
#
# Everything the test changes is written down before it is changed, so that
# --cleanup can undo it whatever happened in between.

# meta PATH: what a planted path is, to tell later whether it is still that.
meta() {
  local p="$1"
  if [ -L "$p" ]; then
    printf 'link|%s|%s' "$(stat -f '%u|%g' "$p")" "$(readlink "$p")"
  elif [ -d "$p" ]; then
    printf 'dir|%s' "$(stat -f '%u|%g|%Lp|%i' "$p")"
  elif [ -f "$p" ]; then
    printf 'file|%s|%s' "$(stat -f '%u|%g|%Lp|%i|%z' "$p")" "$(shasum -a 256 "$p" | cut -d' ' -f1)"
  else
    printf 'absent'
  fi
}

# plant KIND PATH [CONTENT|TARGET] [OWNER] [MODE]: create a path the package
# must find, and write down exactly what it is.
plant() {
  local kind="$1" p="$2" arg="${3:-}" owner="${4:-root:wheel}" mode="${5:-}"
  present "$p" && fail "planting $p: it already exists"
  case "$kind" in
    file) printf '%s' "$arg" >"$p"; chown "$owner" "$p"; chmod "${mode:-644}" "$p" ;;
    link) ln -s "$arg" "$p"; chown -h "$owner" "$p" ;;
    dir) mkdir "$p"; chown "$owner" "$p"; chmod "${mode:-755}" "$p" ;;
  esac
  printf '%s\t%s\n' "$p" "$(meta "$p")" >>"$(ST)/planted"
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
  if [ -d "$p" ] && [ ! -L "$p" ]; then rmdir "$p"; else rm -f "$p"; fi
  forget_plant "$p"
}

# consumed PATH: a planted path the package has since taken over; it is not
# the test's to remove any more.
forget_plant() {
  awk -F'\t' -v p="$1" '$1 != p' "$(ST)/planted" >"$(ST)/planted.new"
  mv -f "$(ST)/planted.new" "$(ST)/planted"
}

# alter KIND PATH VALUE: change a path's mode, owner, ACL, flags or link,
# recording how to undo it; restore_last undoes the newest alteration.
alter() {
  local kind="$1" p="$2" value="$3" undo
  case "$kind" in
    mode) undo="$(stat -f '%Mp%Lp' "$p")"; chmod "$value" "$p" ;;
    owner) undo="$(stat -f '%Su:%Sg' "$p")"; chown -h "$value" "$p" ;;
    acl) undo="$value"; chmod +a "$value" "$p" ;;
    flags) undo=nouchg; chflags uchg "$p" ;;
    link) undo="$(readlink "$p")"; rm -f "$p"; ln -s "$value" "$p"; chown -h root:wheel "$p" ;;
    aside) undo="$value"; mv "$p" "$value" ;;
  esac
  printf '%s\t%s\t%s\n' "$kind" "$p" "$undo" >>"$(ST)/altered"
}

undo_line() {
  local kind="$1" p="$2" undo="$3"
  case "$kind" in
    mode) chmod "$undo" "$p" ;;
    owner) chown -h "$undo" "$p" ;;
    acl) chmod -a "$undo" "$p" ;;
    flags) chflags nouchg "$p" ;;
    link) rm -f "$p"; ln -s "$undo" "$p"; chown -h root:wheel "$p" ;;
    aside) mv "$undo" "$p" ;;
  esac
}

# forget_last: the newest alteration is no longer the test's to undo.
forget_last() {
  sed '$d' "$(ST)/altered" >"$(ST)/altered.new" && mv -f "$(ST)/altered.new" "$(ST)/altered"
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

# parents: every directory the package writes into, and everything else
# /usr/local/bin holds.
parents() {
  local e n
  snap_path /usr/local
  snap_path "$BIN"
  snap_path "$LIBEXEC"
  if [ -d "$BIN" ]; then
    for e in "$BIN"/* "$BIN"/.[!.]* "$BIN"/..?*; do
      present "$e" || continue
      n="${e##*/}"
      case " $NAMES " in *" $n "*) continue ;; esac
      printf '%s\t%s\n' "$e" "$(stat -f '%HT|%u|%g|%Lp|%i|%z|%m' "$e")"
    done
  fi
  if [ -d "$LIBEXEC" ]; then
    for e in "$LIBEXEC"/* "$LIBEXEC"/.[!.]* "$LIBEXEC"/..?*; do
      present "$e" || continue
      [ "${e##*/}" = syndeo ] && continue
      printf '%s\t%s\n' "$e" "$(stat -f '%HT|%u|%g|%Lp|%i' "$e")"
    done
  fi
}

security_state() {
  sudo -u "$RUNNER" security list-keychains -d user
  security list-keychains -d system
  printf 'user trust %s\n' "$(sudo -u "$RUNNER" security dump-trust-settings 2>&1 | shasum | cut -c1-16)"
  printf 'admin trust %s\n' "$(security dump-trust-settings -d 2>&1 | shasum | cut -c1-16)"
  printf 'system trust %s\n' "$(security dump-trust-settings -s 2>&1 | shasum | cut -c1-16)"
}

homes() { ls -d /Users/*/.syndeo /var/root/.syndeo 2>/dev/null || true; }

receipts() { pkgutil --pkgs | grep -i syndeo || true; }

# package_state: everything of Syndeo's, to compare before and after a step
# that should change nothing.
package_state() {
  local n p
  pkgutil --pkg-info "$ID" 2>&1 | grep -v '^install-time'
  pkgutil --files "$ID" 2>&1
  for n in $NAMES; do
    p="$BIN/$n"
    if present "$p"; then
      printf '%s\t%s\t%s\n' "$p" "$(stat -f '%HT|%u|%g|%Lp|%Sf|%i' "$p")" "$(readlink "$p" 2>/dev/null)"
    else
      printf '%s\tabsent\n' "$p"
    fi
  done
  if present "$D"; then
    find "$D" -exec stat -f '%N|%HT|%u|%g|%Lp|%Mp|%Sf|%i|%z|%Y' {} + | LC_ALL=C sort
    find "$D" -type f -exec shasum -a 256 {} + | LC_ALL=C sort
    for e in "$D" "$D"/*; do
      present "$e" && ls -lde "$e" | sed 1d
    done
  fi
  parents
}

pristine() {
  local n
  pkgutil --pkg-info "$ID" >/dev/null 2>&1 && return 1
  present "$D" && return 1
  for n in $NAMES; do present "$BIN/$n" && return 1; done
  return 0
}

current_is() { [ "$(readlink "$D/current" 2>/dev/null)" = "$1" ]; }

# commands_say V: every command, started from /usr/local/bin with a clean
# environment, says it is V.
commands_say() {
  local n
  for n in $NAMES; do
    [ "$(env -i PATH=/usr/bin:/bin "$BIN/$n" --version 2>&1 | head -n 1)" = "$n $1" ] || return 1
  done
}
receipt_is() { [ "$(pkgutil --pkg-info "$ID" 2>/dev/null | sed -n 's/^version: //p')" = "$1" ]; }

# commands_fail_closed: every command, started from /usr/local/bin, fails to
# start, and only because what its link leads to is not there.
commands_fail_closed() {
  local n out
  for n in $NAMES; do
    if out="$(env -i PATH=/usr/bin:/bin "$BIN/$n" --version 2>&1)"; then return 1; fi
    case "$out" in *"$BIN/$n: No such file or directory"*) ;; *) return 1 ;; esac
  done
}

# alone V: the private directory holds V and current, and nothing else.
alone() { [ "$(ls -A "$D" | tr '\n' ' ')" = "$1 current " ]; }

# ------------------------------------------------------------------ installing

# The system's installer log, and how many half-second reads note_said()
# gives it: it is written asynchronously, a line can arrive late, and one may
# not arrive at all. The self-test points both at its own.
INSTALL_LOG=/var/log/install.log
SAID_TRIES=10

# install LABEL PKG [TARGET]: installer(8), with -dumplog, so that its own
# detailed log comes back with this invocation, on stderr. Everything it
# printed is kept in LOG.installer, whole, one file per invocation: the
# numbering carries on across the steps and an existing file is never
# written over. What /var/log/install.log gained meanwhile is kept whole too,
# in LOG.install-log, as supplementary evidence.
install() {
  local label="$1" pkg="$2" target="${3:-/}" before status n log
  n="$(cat "$(ST)/installs" 2>/dev/null || echo 0)"
  n=$((n + 1))
  echo "$n" >"$(ST)/installs"
  log="$(ST)/logs/$(printf '%02d' "$n")-$label"
  [ -e "$log.installer" ] && fail "$log.installer exists; not writing over it"
  before="$(wc -l <"$INSTALL_LOG" | tr -d ' ')"
  /usr/sbin/installer -dumplog -pkg "$pkg" -target "$target" >"$log.installer" 2>&1
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

# before_is DECISION: the package's state is the one the preinstall decides
# DECISION on, as ci/macos-pkg/preinstall.in's matrix reads it.
before_is() {
  local x v
  case "$1" in
    "installing "*) pristine ;;
    "resuming an interrupted installation of "*)
      v="${1##* }"
      ! pkgutil --pkg-info "$ID" >/dev/null 2>&1 && [ "$(ls -A "$D" 2>/dev/null)" = "$v" ]
      ;;
    "upgrading from "*)
      x="${1#upgrading from }"
      x="${x%% *}"
      receipt_is "$x" && alone "$x" && current_is "$x"
      ;;
    "reinstalling "* | "repairing "*)
      v="${1##* }"
      receipt_is "$v" && alone "$v" && current_is "$v"
      ;;
    "completing the failed upgrade from "*)
      x="${1#completing the failed upgrade from }"
      v="${x##* }"
      x="${x%% *}"
      receipt_is "$x" && alone "$v" && { current_is "$x" || current_is "$v"; }
      ;;
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
# DECISION on, the install succeeds and leaves exactly VERSION. DECISION
# itself is asserted by ci/verify-pkg.sh's st_decisions; here it is only
# noted.
installed() {
  local label="$1" pkg="$2" text="$3" version="$4" seen
  before_is "$text" || fail "$label: before installing, the package is not in the state that \"$text\" is decided on"
  install "$label" "$pkg" || { sed 's/^/      | /' "$last_log.installer" "$last_log.install-log"; fail "$label: the install failed"; }
  seen="$(note_said "$text")"
  bash "$here/verify-pkg.sh" installed "$version" >"$last_log.verify" 2>&1 ||
    { sed 's/^/      | /' "$last_log.verify"; fail "$label: the installation is not exactly $version"; }
  pass "$label: installed from the state \"$text\" is decided on, and verify-pkg installed $version passes ($seen)"
}

# standin VERSION: a package of shell stand-ins, from the real builder.
standin() {
  local v="$1" dir name n
  dir="$(ST)/standins/$v"
  name="syndeo-$v-aarch64-apple-darwin"
  mkdir -p "$dir/src/$name/tools"
  for n in $NAMES; do
    printf '#!/bin/sh\necho "%s %s"\n' "$n" "$v" >"$dir/src/$name/$n"
    chmod 755 "$dir/src/$name/$n"
  done
  cp "$here/../README.md" "$here/../LICENSE" "$dir/src/$name/"
  cp "$here/../crates/syndeo-agent/tools/wordcount.wat" "$dir/src/$name/tools/"
  tar -C "$dir/src" --format=ustar -czf "$dir/$name.tar.gz" "$name"
  build_standin "$v" "$dir"
}

# build_standin V DIR: the package for DIR/syndeo-V-aarch64-apple-darwin.tar.gz.
build_standin() {
  local v="$1" dir="$2" repo
  # The checkout is the runner's and this is root: git would refuse it.
  repo="$(cd "$here/.." && pwd)"
  SOURCE_DATE_EPOCH="$(git -c safe.directory="$repo" -C "$repo" log -1 --format=%ct)" \
    "$here/package-macos-pkg.sh" "$v" "$dir/syndeo-$v-aarch64-apple-darwin.tar.gz" "$dir" >/dev/null ||
    fail "building the stand-in $v"
  printf '%s' "$dir/syndeo-$v-aarch64-apple-darwin.pkg"
}

# real_standin V PKG VERSION: a stand-in package for V whose seven commands
# are the real binaries in PKG, the package of VERSION.
real_standin() {
  local v="$1" pkg="$2" real="$3" dir name n from
  dir="$(ST)/standins/$v"
  name="syndeo-$v-aarch64-apple-darwin"
  expand_payload "$pkg" "$(ST)/real-payload"
  from="$(ST)/real-payload/syndeo.pkg/Payload/usr/local/libexec/syndeo/$real"
  mkdir -p "$dir/src/$name/tools"
  for n in $NAMES; do
    cp "$from/$n" "$dir/src/$name/$n" || fail "copying the real $n"
    chmod 755 "$dir/src/$name/$n"
  done
  cp "$here/../README.md" "$here/../LICENSE" "$dir/src/$name/"
  cp "$here/../crates/syndeo-agent/tools/wordcount.wat" "$dir/src/$name/tools/"
  tar -C "$dir/src" --format=ustar -czf "$dir/$name.tar.gz" "$name"
  build_standin "$v" "$dir"
}

# fault_pkg PKG entry|after-switch: PKG, a stand-in, with a test fault in its
# postinstall that fails the first run only (ci/pkg-test-fault.sh).
fault_pkg() {
  local pkg="$1" where="$2" dir
  dir="$(ST)/faults/$where"
  mkdir -p "$dir"
  bash "$here/pkg-test-fault.sh" "$pkg" "$dir/$(basename "$pkg")" "$where" "$(ST)/logs/postinstall-fault-$where" >&2 ||
    fail "making the $where fault"
  printf '%s' "$dir/$(basename "$pkg")"
}

# failed_upgrade LABEL PKG X W AT OLDER NEWER: install PKG, a faulty W, over
# X. It fails and leaves what was measured: X's receipt, X gone, W in place,
# current at AT. Everything that must refuse that state does, changing
# nothing, OLDER and NEWER packages included; then PKG again completes it.
failed_upgrade() {
  local label="$1" pkg="$2" x="$3" w="$4" at="$5" older="$6" newer="$7" before
  must "$label: before, $x alone, with its receipt and current" eval "receipt_is $x && alone $x && current_is $x"
  if install "$label" "$pkg"; then fail "$label: the faulty package installed"; fi
  note_said "syndeo postinstall: test fault" >/dev/null
  must "$label: Installer left the receipt at $x, $x gone, $w in place, current -> $at" eval "receipt_is $x && alone $w && current_is $at"
  if [ "$at" = "$x" ]; then
    must "  ... every command fails to start, only because it is not there" commands_fail_closed
  else
    must "  ... every command runs $w" commands_say "$w"
  fi
  must "  ... verify-pkg installed fails: the receipt is $x's, and the paths it lists are missing" eval \
    "! bash '$here/verify-pkg.sh' installed $w >'$(ST)/logs/$label.verify' 2>&1 && grep -q 'is missing, and the receipt for $x lists it' '$(ST)/logs/$label.verify'"
  before="$(package_state)"
  must "  ... $w's uninstaller refuses, saying to finish the upgrade" eval \
    "! /bin/sh '$D/$w/uninstall.sh' >'$(ST)/logs/$label.uninstall' 2>&1 && grep -q 'the upgrade from $x to $w did not finish' '$(ST)/logs/$label.uninstall'"
  must "  ... changing nothing" test "$(package_state)" = "$before"
  refused "$label-older-package" "$older" "an upgrade from $x to $w did not finish: install the Syndeo $w package again"
  refused "$label-newer-package" "$newer" "an upgrade from $x to $w did not finish: install the Syndeo $w package again"
  installed "$label-the-same-package-again" "$pkg" "completing the failed upgrade from $x to $w" "$w"
  must "  ... $w alone, with its receipt, current, the links and every command" eval "alone $w && receipt_is $w && current_is $w && commands_say $w"
}

# tree V [subset]: plant a version tree owned and moded as the package lays
# one down. What the files hold does not matter to the scripts, which judge
# names, types, owners and modes, and Installer replaces them when it resumes.
tree() {
  local v="$1" n
  plant dir "$D/$v"
  plant dir "$D/$v/tools"
  for n in $NAMES uninstall.sh; do
    if [ "${2:-}" = subset ] && [ "$n" != syndeo ] && [ "$n" != syndeo-net ]; then continue; fi
    plant file "$D/$v/$n" "#!/bin/sh
" root:wheel 755
  done
  plant file "$D/$v/README.md" "placeholder
" root:wheel 644
  if [ "${2:-}" != subset ]; then
    plant file "$D/$v/LICENSE" "placeholder
" root:wheel 644
    plant file "$D/$v/tools/wordcount.wat" "(module)
" root:wheel 644
  fi
}

untree() {
  local v="$1" n
  for n in $NAMES uninstall.sh README.md LICENSE tools/wordcount.wat; do
    if [ -n "$(planted_meta "$D/$v/$n")" ]; then unplant "$D/$v/$n"; fi
  done
  unplant "$D/$v/tools"
  unplant "$D/$v"
}

expand_payload() {
  local pkg="$1" out="$2"
  rm -rf "$out"
  pkgutil --expand-full "$pkg" "$out" >/dev/null || fail "expanding $pkg"
}

# ------------------------------------------------------------------ cleanup

# cleanup: undo everything recorded, whatever happened, then check the runner
# is as it was. Safe to run any number of times.
cleanup() {
  local st problems="" line p kind undo v
  st="$(ST)"
  [ -d "$st" ] || { say "cleanup: nothing was recorded"; return 0; }
  say ""
  say "cleanup"
  # Processes the steps started, if any are left.
  for p in "$(US)/pids" "$st/pids"; do
    [ -f "$p" ] || continue
    while read -r pid; do kill "$pid" 2>/dev/null; done <"$p"
  done
  # The user's keychain search list and the throwaway keychain.
  if [ -f "$(US)/keychain-list" ]; then
    # shellcheck disable=SC2046
    sudo -u "$RUNNER" security list-keychains -d user -s $(tr -d '"' <"$(US)/keychain-list") ||
      problems="$problems; could not restore the keychain search list"
  fi
  if [ -f "$(US)/keychain" ] && [ -e "$(cat "$(US)/keychain")" ]; then
    sudo -u "$RUNNER" security delete-keychain "$(cat "$(US)/keychain")" ||
      problems="$problems; could not delete the throwaway keychain"
  fi
  # The alternate volume.
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
  # Planted paths, newest first: those inside the package's tree first, so
  # its uninstaller finds only what the package left, then the rest. Each is
  # removed only if it is still exactly what was planted.
  local inside
  for inside in yes no; do
    [ -s "$st/planted" ] || break
    tail -r "$st/planted" | while IFS='	' read -r p want; do
      case "$p" in "$D" | "$D"/*) [ "$inside" = yes ] || continue ;; *) [ "$inside" = no ] || continue ;; esac
      if [ "$(meta "$p")" = "$want" ]; then
        if [ -d "$p" ] && [ ! -L "$p" ]; then rmdir "$p" 2>/dev/null || continue; else rm -f "$p"; fi
        forget_plant "$p"
      fi
    done
  done
  # The package, by its own uninstaller, once it is checked to be the one the
  # repository renders.
  if pkgutil --pkg-info "$ID" >/dev/null 2>&1; then
    v="$(pkgutil --pkg-info "$ID" | sed -n 's/^version: //p')"
    if [ -d "$D/$v" ] && ! current_is "$v"; then
      rm -f "$D/current"; ln -s "$v" "$D/current"; chown -h root:wheel "$D/current"
    fi
    "$here/package-macos-pkg.sh" --render uninstall "$v" '' 0 0 /usr/sbin/pkgutil "$st/uninstall.expected" >/dev/null
    if cmp -s "$st/uninstall.expected" "$D/$v/uninstall.sh" && /bin/sh "$D/$v/uninstall.sh"; then
      :
    else
      # A failed upgrade, or a step that stopped halfway: the uninstaller
      # refuses those by design, or is not the repository's to run.
      say "cleanup: the package's own uninstaller did not remove it; removing what the package put down"
      remove_leftovers
    fi
  elif present "$D"; then
    say "cleanup: $D is there with no receipt; removing what the package put down"
    remove_leftovers
  fi
  # A planted path that no longer exists has nothing left to restore: the
  # package took it over, and its uninstaller removed it.
  if [ -s "$st/planted" ]; then
    cut -f1 "$st/planted" | while IFS= read -r p; do
      present "$p" || forget_plant "$p"
    done
  fi
  # Directories that did not exist before and were made while testing.
  if [ -f "$st/initial.parents" ]; then
    for p in "$LIBEXEC" "$BIN"; do
      if grep -qx "$p	absent" "$st/initial.parents" && [ -d "$p" ] && [ -z "$(ls -A "$p")" ]; then
        rmdir "$p"
      fi
    done
    # And then everything, compared.
    if [ -s "$st/planted" ]; then
      problems="$problems; planted paths no longer as planted: $(cut -f1 "$st/planted" | tr '\n' ' ')"
    fi
    [ "$(parents)" = "$(cat "$st/initial.parents")" ] ||
      { diff "$st/initial.parents" <(parents) | sed 's/^/      | /'; problems="$problems; a parent directory differs"; }
    [ "$(security_state)" = "$(cat "$st/initial.security")" ] ||
      { diff "$st/initial.security" <(security_state) | sed 's/^/      | /'; problems="$problems; keychains or trust settings differ"; }
    [ "$(homes)" = "$(cat "$st/initial.homes")" ] || problems="$problems; a home's .syndeo differs"
    [ "$(receipts)" = "$(cat "$st/initial.receipts")" ] || problems="$problems; the receipts differ"
  fi
  pristine || problems="$problems; Syndeo is still installed"
  if [ -n "$problems" ]; then
    say "cleanup: FAILED$problems"
    return 1
  fi
  say "cleanup: the runner is as it was: no receipt, no payload, no keychain or trust change, every parent the same"
  return 0
}

# remove_leftovers: what the package put down, in whatever state a stopped
# step left it, when its uninstaller cannot remove it: the seven links if they
# are the package's, `current` and `.current.new`, version directories holding
# only the package's names, the private directory, and the receipt. Anything
# else is left, for the checks that follow to report.
remove_leftovers() {
  local n p v
  for n in $NAMES; do
    p="$BIN/$n"
    if [ -L "$p" ] && [ "$(readlink "$p")" = "../libexec/syndeo/current/$n" ]; then rm -f "${p:?}"; fi
  done
  if [ -d "$D" ] && [ ! -L "$D" ]; then
    for p in "$D/current" "$D/.current.new"; do
      if [ -L "$p" ]; then rm -f "${p:?}"; fi
    done
    for v in "$D"/*; do
      [ -d "$v" ] && [ ! -L "$v" ] || continue
      case "${v##*/}" in '' | *[!0-9.]*) continue ;; esac
      for n in $NAMES uninstall.sh README.md LICENSE tools/wordcount.wat; do
        if [ -f "$v/$n" ] && [ ! -L "$v/$n" ]; then rm -f "${v:?}/${n:?}"; fi
      done
      if [ -d "$v/tools" ] && [ ! -L "$v/tools" ]; then rmdir "${v:?}/tools" 2>/dev/null; fi
      rmdir "${v:?}" 2>/dev/null
    done
    rmdir "${D:?}" 2>/dev/null
  fi
  if pkgutil --pkg-info "$ID" >/dev/null 2>&1; then
    pkgutil --forget "$ID" --volume / >/dev/null 2>&1
  fi
  return 0
}

# on_exit: a system step that fails cleans up after itself; one that
# succeeds leaves its state for the next step, and --cleanup comes last.
on_exit() {
  local status=$?
  trap - EXIT INT TERM HUP
  if [ "$status" -ne 0 ]; then
    say "the step failed (status $status); cleaning up"
    cleanup || say "cleanup failed too"
  fi
  exit "$status"
}

# ------------------------------------------------------------------ before

system_before() {
  local pkg="$1" version="$2" st
  guard_root
  st="$(ST)"
  [ -e "$st" ] && refuse "$st exists: this runner has been used for this test before"
  pristine || refuse "Syndeo is already on this runner"
  mount | grep -q " on $ALT " && refuse "$ALT is already mounted"
  mkdir -p "$st/logs"
  : >"$st/planted"
  : >"$st/altered"
  trap on_exit EXIT
  trap 'exit 130' INT TERM HUP

  say "Installing $pkg ($version) on this runner. Package identifier $ID. It will write:"
  pkgutil --payload-files "$pkg" | sed 's|^\.|  |'
  say "and it will plant and alter, then restore, entries in /usr/local, $BIN, $LIBEXEC and $D,"
  say "and a disk image mounted at $ALT."

  parents >"$st/initial.parents"
  security_state >"$st/initial.security"
  homes >"$st/initial.homes"
  receipts >"$st/initial.receipts"
  cp "$st/initial.parents" "$st/logs/initial.parents"

  # /usr/local and /usr/local/libexec have to be strict for the package to
  # install at all; a runner image that has them otherwise is normalized here
  # and put back by cleanup.
  local p
  for p in /usr/local "$LIBEXEC"; do
    present "$p" || continue
    [ -n "$(ls -lde "$p" | sed 1d)" ] && refuse "$p has an ACL; not changing that"
    [ "$(stat -f %u "$p")" = 0 ] || alter owner "$p" root:wheel
    case "$(stat -f %Lp "$p")" in *[2367]? | *[2367]) alter mode "$p" 755 ;; esac
  done
  # A parent the runner lacks is made plainly, as Installer would make it,
  # and cleanup removes it again if it is empty at the end.
  for p in "$BIN" "$LIBEXEC"; do
    if ! present "$p"; then
      mkdir "$p" && chown root:wheel "$p" && chmod 755 "$p" || fail "making $p"
    fi
  done

  say ""
  say "another volume"
  hdiutil create -size 64m -fs APFS -volname SyndeoAltTest "$st/alt.dmg" >/dev/null || fail "creating a disk image"
  hdiutil attach "$st/alt.dmg" -nobrowse -mountpoint "$ALT" >/dev/null || fail "attaching it"
  if install another-volume "$pkg" "$ALT"; then fail "installing on $ALT succeeded"; fi
  must "installing on another volume is refused, and writes nothing there or on /" \
    eval "! present '$ALT/usr/local/libexec/syndeo' && ! pkgutil --pkgs --volume '$ALT' 2>/dev/null | grep -q syndeo && pristine"
  hdiutil detach "$ALT" >/dev/null || fail "detaching $ALT"
  rm -f "$st/alt.dmg"

  say ""
  say "commands that are not the package's"
  plant file "$st/foreign-target" "a foreign program
" root:wheel 755
  plant file "$BIN/syndeo" "foreign
" root:wheel 755
  refused foreign-file "$pkg" "syndeo preinstall: refusing: $BIN/syndeo: exists, and no Syndeo package is installed"
  unplant "$BIN/syndeo"
  plant link "$BIN/syndeo-net" "$st/foreign-target"
  refused foreign-link "$pkg" "refusing: $BIN/syndeo-net: exists"
  unplant "$BIN/syndeo-net"
  must "  ... and its target is byte-identical" test "$(meta "$st/foreign-target")" = "$(planted_meta "$st/foreign-target")"
  plant link "$BIN/syndeo-proxy" ../libexec/other/syndeo-proxy
  refused foreign-dangling-link "$pkg" "refusing: $BIN/syndeo-proxy: exists"
  unplant "$BIN/syndeo-proxy"
  plant dir "$BIN/syndeo-ui"
  refused foreign-directory "$pkg" "refusing: $BIN/syndeo-ui: exists"
  unplant "$BIN/syndeo-ui"
  plant link "$BIN/syndeo-agent" ../libexec/syndeo/current/syndeo-agent
  refused package-shaped-link-without-receipt "$pkg" "refusing: $BIN/syndeo-agent: exists, and no Syndeo package is installed"
  unplant "$BIN/syndeo-agent"
  unplant "$st/foreign-target"

  say ""
  say "the directories around it"
  alter mode "$BIN" "$(printf '%o' $((0$(stat -f %Lp "$BIN") | 02)))"
  refused bin-world-writable "$pkg" "$BIN: can be written by everyone"
  restore_last
  alter acl "$BIN" "everyone allow add_file"
  refused bin-acl "$pkg" "$BIN: has an access control list"
  restore_last
  must "  ... and the ACL is gone again" test -z "$(ls -lde "$BIN" | sed 1d)"
  alter mode "$LIBEXEC" 775
  refused libexec-group-writable "$pkg" "$LIBEXEC: can be written by its group or by everyone"
  restore_last
  plant dir "$st/elsewhere"
  plant link "$D" "$st/elsewhere"
  refused private-directory-is-a-link "$pkg" "$D: is a Symbolic Link, not a directory"
  unplant "$D"
  unplant "$st/elsewhere"

  say ""
  say "a private directory with no receipt"
  plant dir "$D"
  tree 0.0.7
  plant link "$D/current" 0.0.7
  refused orphan "$pkg" "$D: exists, and no Syndeo package is installed"
  unplant "$D/current"
  untree 0.0.7
  unplant "$D"

  say ""
  say "an interrupted first install, resumed"
  plant dir "$D"
  tree "$version" subset
  installed resume "$pkg" "resuming an interrupted installation of $version" "$version"
  # The package owns those paths now.
  forget_plant "$D"
  local n
  for n in "$version" "$version/tools" "$version/syndeo" "$version/syndeo-net" "$version/uninstall.sh" "$version/README.md"; do
    forget_plant "$D/$n"
  done
  /bin/sh "$D/$version/uninstall.sh" >"$st/logs/resume-uninstall" 2>&1 ||
    { sed 's/^/      | /' "$st/logs/resume-uninstall"; fail "removing the resumed install"; }
  must "  ... and its uninstaller removes it entirely" pristine

  say ""
  say "stand-ins: upgrades with the commands in use, two failed upgrades, and their repair"
  local s9 s10 s11 s12 s13 s14 f12 f13
  s9="$(standin 0.0.9)" || fail "building the stand-in 0.0.9"
  s10="$(standin 0.0.10)" || fail "building the stand-in 0.0.10"
  s11="$(standin 0.0.11)" || fail "building the stand-in 0.0.11"
  s12="$(standin 0.0.12)" || fail "building the stand-in 0.0.12"
  s13="$(standin 0.0.13)" || fail "building the stand-in 0.0.13"
  s14="$(standin 0.0.14)" || fail "building the stand-in 0.0.14"
  f12="$(fault_pkg "$s12" entry)" || fail "building 0.0.12 with a fault at its postinstall's entry"
  f13="$(fault_pkg "$s13" after-switch)" || fail "building 0.0.13 with a fault just after its switch"
  alter owner "$BIN" "$RUNNER:admin"
  alter mode "$BIN" 775
  local bin_before out status
  bin_before="$(snap_path "$BIN")"
  installed standin-0.0.9 "$s9" "installing 0.0.9" 0.0.9
  must "  ... $BIN is still $RUNNER:admin 0775: overwrite-permissions=\"false\" held" test "$(snap_path "$BIN")" = "$bin_before"

  say ""
  say "upgrades, with the commands started over and over"
  out="$(python3 -I "$here/upgrade-stress.py" --private "$D" --command "$BIN/syndeo" --from 0.0.9 \
    --install 0.0.10 "$s10" --install 0.0.11 "$s11" --expect-output 'syndeo {version}' \
    --expect-file $'#!/bin/sh\necho "syndeo {version}"\n' --log "$st/logs/upgrade-stress" 2>&1)"
  status=$?
  printf '%s\n' "$out" | tee "$st/logs/upgrade-stress" | sed 's/^/      | /'
  must "0.0.9 to 0.0.10 to 0.0.11: while Installer ran, failures only ENOENT or EINVAL and every start one whole version; none afterwards" test "$status" = 0
  bash "$here/verify-pkg.sh" installed 0.0.11 >"$st/logs/after-upgrade-stress" 2>&1 ||
    { sed 's/^/      | /' "$st/logs/after-upgrade-stress"; fail "after the upgrades, the installation is not exactly 0.0.11"; }
  must "  ... 0.0.11 alone, and verify-pkg installed 0.0.11 passes" alone 0.0.11
  must "  ... $BIN is still $RUNNER:admin 0775" test "$(snap_path "$BIN")" = "$bin_before"
  refused older-package "$s10" "Syndeo 0.0.11 is installed, and this package is the older 0.0.10"

  say ""
  say "an upgrade whose postinstall fails at its entry, before the switch"
  failed_upgrade fault-at-entry "$f12" 0.0.11 0.0.12 0.0.11 "$s11" "$s13"

  say ""
  say "an upgrade whose postinstall fails just after its switch"
  failed_upgrade fault-after-switch "$f13" 0.0.12 0.0.13 0.0.13 "$s12" "$s14"

  say ""
  say "a running syndeo across an upgrade, with the real binaries"
  local r15 late
  r15="$(real_standin 0.0.15 "$pkg" "$version")" || fail "building 0.0.15 from the real binaries"
  before_is "upgrading from 0.0.13 to 0.0.15" || fail "before 0.0.15, the package is not 0.0.13 alone"
  install real-binaries-0.0.15 "$r15" || { sed 's/^/      | /' "$last_log.installer" "$last_log.install-log"; fail "installing 0.0.15"; }
  note_said "upgrading from 0.0.13 to 0.0.15" >/dev/null
  # Its commands are the real ones, which report the real version.
  bash "$here/verify-pkg.sh" installed 0.0.15 --commands-say "$version" >"$last_log.verify" 2>&1 ||
    { sed 's/^/      | /' "$last_log.verify"; fail "the installation is not exactly 0.0.15"; }
  must "0.0.15, the real binaries: alone, its receipt, tree, links and current exact (verify-pkg installed)" eval "alone 0.0.15 && receipt_is 0.0.15 && current_is 0.0.15"
  python3 -I "$here/late-lookup.py" --command "$BIN/syndeo" --user "$RUNNER" --home "$st/late-home" \
    --private "$D" --old 0.0.15 --new "$version" --held "$st/late.held" --go "$st/late.go" >"$st/logs/late-lookup" 2>&1 &
  late=$!
  echo "$late" >>"$st/pids"
  for _ in $(seq 1 600); do
    [ -f "$st/late.held" ] && break
    kill -0 "$late" 2>/dev/null || break
    sleep 0.1
  done
  [ -f "$st/late.held" ] || { sed 's/^/      | /' "$st/logs/late-lookup"; fail "the 0.0.15 syndeo doctor was not held"; }
  pass "a 0.0.15 syndeo doctor runs, held after it found its directory and before it looked for any sibling"

  say ""
  say "the real package, over all of that"
  installed real "$pkg" "upgrading from 0.0.15 to $version" "$version"
  : >"$st/late.go"
  wait "$late"
  status=$?
  reaped "$st/pids" "$late"
  sed 's/^/      | /' "$st/logs/late-lookup"
  must "the 0.0.15 doctor, after the upgrade: every sibling it looked for refused as removed during an upgrade, and nothing started" test "$status" = 0
  must "  ... $BIN is still $RUNNER:admin 0775" test "$(snap_path "$BIN")" = "$bin_before"
  must "  ... no home has a .syndeo it did not have" test "$(homes)" = "$(cat "$st/initial.homes")"
  must "  ... keychains and trust settings are as they were" test "$(security_state)" = "$(cat "$st/initial.security")"
  # The stand-ins' paths, for --system-after. Beside $st/standins/, the
  # directory they are built in.
  printf '%s\n' "$s9" "$s10" >"$st/standins.list"
}

# ------------------------------------------------------------------ user

user_finish() {
  local status=$?
  trap - EXIT INT TERM HUP
  local us problems=""
  us="$(US)"
  # agent_tools's descriptor and FIFO, if it stopped between opening and
  # closing them.
  exec 9<&- 2>/dev/null
  if [ -p "$us/agent-stdin.fifo" ]; then rm -f "${us:?}/agent-stdin.fifo"; fi
  if [ -f "$us/pids" ]; then
    while read -r p; do kill "$p" 2>/dev/null; done <"$us/pids"
  fi
  if [ -f "$us/keychain-list" ]; then
    # shellcheck disable=SC2046
    security list-keychains -d user -s $(tr -d '"' <"$us/keychain-list") || problems="could not restore the search list"
    [ "$(security list-keychains -d user)" = "$(cat "$us/keychain-list")" ] || problems="$problems; the search list differs"
  fi
  if [ -f "$us/keychain" ] && [ -e "$(cat "$us/keychain")" ]; then
    security delete-keychain "$(cat "$us/keychain")" || problems="$problems; could not delete the keychain"
  fi
  if [ -n "$problems" ]; then
    say "user cleanup: $problems"
    [ "$status" -eq 0 ] && status=1
  else
    say "user cleanup: keychain search list restored, throwaway keychain deleted, processes stopped"
  fi
  exit "$status"
}

# agent_tools TOOLS: `syndeo-agent --task tools` on TOOLS, from /usr/local/bin.
# Sets agent_out to what it said and agent_status to how it exited.
#
# The agent takes end-of-file on its stdin to mean that whoever started it is
# gone (syndeo_ipc::exit_when_parent_does), and exits at once, saying
# nothing; a CI step's stdin is already at end-of-file. It is given what the
# shell gives it: a pipe whose other end stays open while it runs. Here that is
# a private FIFO, opened read and write on descriptor 9, so the open never
# waits and no timer or helper process holds it. The descriptor is closed and
# the FIFO removed however the agent did; user_finish does both again.
agent_tools() {
  local fifo
  fifo="$(US)/agent-stdin.fifo"
  agent_out=''
  agent_status=1
  mkfifo -m 600 "$fifo" || return 1
  exec 9<>"$fifo"
  agent_out="$(clean_env syndeo-agent --net-socket "$(US)/net.sock" --shell-socket "$(US)/shell.sock" \
    --task tools --tools "$1" 2>&1 <&9 9<&-)"
  agent_status=$?
  exec 9<&-
  rm -f "${fifo:?}"
}

# reaped FILE PID: PID has been waited for, so it is no longer this test's to
# kill; a pid left in FILE could name someone else's process by the time a
# cleanup reads it.
reaped() {
  awk -v pid="$2" '$0 != pid' "$1" >"$1.new" && mv -f "$1.new" "$1"
}

# clean_env ...: a command from /usr/local/bin with nothing of the step's
# environment but HOME, PATH and, if set, SYNDEO_HOME.
clean_env() {
  env -i HOME="$HOME" PATH="$BIN:/usr/bin:/bin:/usr/sbin:/sbin" ${SYNDEO_HOME:+SYNDEO_HOME="$SYNDEO_HOME"} "$@"
}

# spawn LOG COMMAND...: clean_env in the background, as its own process, so
# that $! is the program's pid and its children are its own.
spawn() {
  local log="$1"; shift
  ( exec env -i HOME="$HOME" PATH="$BIN:/usr/bin:/bin:/usr/sbin:/sbin" ${SYNDEO_HOME:+SYNDEO_HOME="$SYNDEO_HOME"} "$@" ) >"$log" 2>&1 &
  spawned=$!
  echo "$spawned" >>"$(US)/pids"
}

# same_file A B: one file, by device and inode. The kernel may name
# /usr/local as /System/Volumes/Data/usr/local.
same_file() { [ "$(stat -f '%d:%i' "$1" 2>/dev/null)" = "$(stat -f '%d:%i' "$2" 2>/dev/null)" ] && [ -n "$(stat -f %i "$1" 2>/dev/null)" ]; }

# runs_from PID PATH: the process's executable, as started, is PATH.
runs_from() { same_file "$(ps -o args= -p "$1" | cut -d' ' -f1)" "$2"; }

wait_child() {
  local parent="$1" name="$2" c
  for _ in $(seq 1 100); do
    for c in $(pgrep -P "$parent" 2>/dev/null); do
      case "$(ps -o args= -p "$c")" in *"/$name "* | *"/$name") printf '%s' "$c"; return 0 ;; esac
    done
    sleep 0.2
  done
  return 1
}

user() {
  local version="$1" us h out n child pid ready
  guard_runner
  [ "$(id -u)" != 0 ] && [ "$(id -un)" = "$RUNNER" ] || refuse "run --user as $RUNNER, not root"
  receipt_is "$version" || refuse "Syndeo $version is not installed"
  us="$(US)"
  [ -e "$us" ] && refuse "$us exists already"
  mkdir -p "$us"
  : >"$us/pids"
  trap user_finish EXIT
  trap 'exit 130' INT TERM HUP
  h="$us/home with space"

  say ""
  say "the commands, from PATH"
  for n in $NAMES; do
    must "$n --version says $version" test "$(clean_env "$n" --version | head -n 1)" = "$n $version"
  done

  say ""
  say "doctor, and the first run's example tools"
  must "no home yet" eval "! present '$h'"
  out="$(SYNDEO_HOME="$h" clean_env syndeo doctor 2>&1)"
  printf '%s\n' "$out" >"$us/doctor"
  for n in syndeo-net syndeo-keystore syndeo-agent; do
    must "doctor finds $n in $D/$version" same_file "$(printf '%s\n' "$out" | awk -v n="$n" '$1 == n { print $2 }')" "$D/$version/$n"
  done
  must "the keystore starts and answers" eval "printf '%s\n' \"\$out\" | grep -qE '^keystore .*initialized'"
  must "the first run seeded $h/tools/wordcount.wat: $RUNNER's, 0644, a regular file, the installed bytes" eval \
    "[ -d '$h/tools' ] && [ ! -L '$h/tools/wordcount.wat' ] && [ -f '$h/tools/wordcount.wat' ] && [ \"\$(stat -f '%Su %Lp' '$h/tools/wordcount.wat')\" = '$RUNNER 644' ] && cmp -s '$h/tools/wordcount.wat' '$D/$version/tools/wordcount.wat'"
  agent_tools "$h/tools"
  printf '%s\n' "$agent_out" >"$us/agent.log"
  ready=no
  if [ "$agent_status" = 0 ] && grep -qE 'wordcount +ready' "$us/agent.log"; then ready=yes; fi
  must "the agent lists wordcount as ready" test "$ready" = yes
  must "  ... and its stdin FIFO is closed and gone" eval "! present '$us/agent-stdin.fifo' && ! { : >&9; } 2>/dev/null"
  printf '(module)\n' >"$h/tools/wordcount.wat"
  SYNDEO_HOME="$h" clean_env syndeo doctor >/dev/null 2>&1
  must "an edited tool is not overwritten" test "$(cat "$h/tools/wordcount.wat")" = "(module)"
  mkdir -p "$us/empty/tools"
  SYNDEO_HOME="$us/empty" clean_env syndeo doctor >/dev/null 2>&1
  must "an existing empty tools directory is left empty" test -z "$(ls -A "$us/empty/tools")"
  mkdir -p "$us/linked" "$us/elsewhere"
  ln -s "$us/elsewhere" "$us/linked/tools"
  SYNDEO_HOME="$us/linked" clean_env syndeo doctor >/dev/null 2>&1
  must "a symlinked tools directory is not followed" test -z "$(ls -A "$us/elsewhere")"

  say ""
  say "fetching through the installed process model"
  out="$(SYNDEO_HOME="$h" clean_env syndeo browse https://www.rust-lang.org/ --twice 2>/dev/null)"
  must "a fetch, then a cache hit" eval "printf '%s\n' \"\$out\" | grep -qE '^  200' && printf '%s\n' \"\$out\" | grep -q 'again   cache'"
  SYNDEO_HOME="$h" spawn "$us/peer.log" syndeo peer serve --serve-only --listen /ip4/127.0.0.1/tcp/0
  pid=$spawned
  child="$(wait_child "$pid" syndeo-net)" || { sed 's/^/      | /' "$us/peer.log"; fail "syndeo peer serve started no syndeo-net"; }
  must "a long-running syndeo started from $BIN runs syndeo-net from $D/$version" runs_from "$child" "$D/$version/syndeo-net"
  kill "$pid"; wait "$pid" 2>/dev/null
  reaped "$us/pids" "$pid"

  say ""
  say "syndeo-webkit's proxy, with a throwaway keychain"
  SYNDEO_HOME="$h" clean_env syndeo-proxy ca >/dev/null 2>&1
  [ -f "$h/proxy/syndeo-ca.pem" ] || fail "syndeo-proxy ca made no authority"
  security list-keychains -d user >"$us/keychain-list"
  local kc="$us/webkit-test.keychain-db"
  echo "$kc" >"$us/keychain"
  security create-keychain -p "$(openssl rand -hex 16)" "$kc" || fail "creating the throwaway keychain"
  security import "$h/proxy/syndeo-ca.pem" -k "$kc" -t cert >/dev/null || fail "importing the authority, without trust"
  # shellcheck disable=SC2046
  security list-keychains -d user -s "$kc" $(tr -d '"' <"$us/keychain-list") || fail "adding the keychain to the search list"
  SYNDEO_HOME="$h" spawn "$us/webkit.log" syndeo-webkit --home "$h" https://www.rust-lang.org/
  pid=$spawned
  child="$(wait_child "$pid" syndeo-proxy)" || { sed 's/^/      | /' "$us/webkit.log"; fail "syndeo-webkit started no syndeo-proxy"; }
  must "syndeo-webkit started from $BIN runs syndeo-proxy from $D/$version" runs_from "$child" "$D/$version/syndeo-proxy"
  kill "$pid"; wait "$pid" 2>/dev/null
  reaped "$us/pids" "$pid"
  for _ in $(seq 1 50); do kill -0 "$child" 2>/dev/null || break; sleep 0.2; done
  must "  ... and the proxy goes when the browser does" eval "! kill -0 $child 2>/dev/null"
}

# ------------------------------------------------------------------ after

system_after() {
  local pkg="$1" version="$2" st before out
  guard_root
  st="$(ST)"
  [ -d "$st" ] || refuse "--system-before has not run"
  receipt_is "$version" || refuse "Syndeo $version is not installed"
  trap on_exit EXIT
  trap 'exit 130' INT TERM HUP
  local s10
  s10="$(sed -n 2p "$st/standins.list")"


  say ""
  say "the same version again"
  local hashes
  hashes="$(find "$D/$version" -type f -exec shasum -a 256 {} + | sort)"
  installed reinstall "$pkg" "reinstalling $version" "$version"
  must "  ... every file is the same bytes" test "$(find "$D/$version" -type f -exec shasum -a 256 {} + | sort)" = "$hashes"
  alter mode "$D/$version/syndeo-net" 775
  refused reinstall-over-a-changed-mode "$pkg" "$D/$version/syndeo-net: is not a Regular File"
  restore_last
  alter owner "$D/$version/README.md" "$RUNNER:wheel"
  refused reinstall-over-a-changed-owner "$pkg" "$D/$version/README.md: is not a Regular File"
  restore_last
  plant file "$D/$version/notes.txt" "extra
"
  refused reinstall-over-an-extra-file "$pkg" "$D/$version/notes.txt: is not part of Syndeo $version"
  unplant "$D/$version/notes.txt"
  plant link "$D/$version/tools/linked.wat" ../README.md
  refused reinstall-over-an-internal-symlink "$pkg" "$D/$version/tools/linked.wat: is not part of Syndeo $version"
  unplant "$D/$version/tools/linked.wat"
  alter acl "$D/$version/README.md" "everyone allow read"
  refused reinstall-over-an-acl "$pkg" "$D/$version/README.md: is not a Regular File"
  restore_last
  alter flags "$D/$version/README.md" uchg
  refused reinstall-over-an-immutable-file "$pkg" "$D/$version/README.md: is not a Regular File"
  restore_last

  say ""
  say "a path the receipt lists, missing"
  mkdir -p "$st/aside"
  alter aside "$D/$version/README.md" "$st/aside/README.md"
  must "verify-pkg installed fails, naming it" eval \
    "! bash '$here/verify-pkg.sh' installed '$version' >'$st/logs/missing-path.verify' 2>&1 && grep -q '$D/$version/README.md: is missing, and the receipt for $version lists it' '$st/logs/missing-path.verify'"
  before="$(package_state)"
  must "the uninstaller refuses, naming it" eval \
    "! /bin/sh '$D/$version/uninstall.sh' >'$st/logs/missing-path.uninstall' 2>&1 && grep -q '$D/$version/README.md: is missing, and the receipt for $version lists it' '$st/logs/missing-path.uninstall'"
  must "  ... changing nothing" test "$(package_state)" = "$before"
  installed repair "$pkg" "repairing $version" "$version"
  # The package put it back; the copy set aside is the test's to delete.
  forget_last
  rm -f "$st/aside/README.md"
  rmdir "$st/aside"

  say ""
  say "downgrades and disagreement"
  refused downgrade "$s10" "this package is the older 0.0.10"
  alter link "$D/current" 0.0.10
  refused current-at-another-version "$pkg" "points at '0.0.10', not the installed $version"
  restore_last
  alter link "$BIN/syndeo-ui" /Applications/Foreign.app/Contents/MacOS/foreign
  refused foreign-command-with-a-receipt "$pkg" "$BIN/syndeo-ui: is not a link this package writes"
  restore_last
  bash "$here/verify-pkg.sh" installed "$version" >"$st/logs/after-refusals" 2>&1 || fail "the installation changed during the refusals"

  plant dir "$D/0.0.99"
  refused a-second-version "$pkg" "with $version installed, it may hold nothing else"
  unplant "$D/0.0.99"

  say ""
  say "the uninstaller, all or nothing"
  local what
  plant dir "$LIBEXEC/someone-else"
  for what in tools-extra tree-extra private-file private-dir bad-version second-version foreign-command mode acl pending; do
    case "$what" in
      tools-extra) plant file "$D/$version/tools/extra.wat" "x" ;;
      tree-extra) plant file "$D/$version/notes.txt" "x" ;;
      private-file) plant file "$D/notes.txt" "x" ;;
      private-dir) plant dir "$D/backup" ;;
      bad-version) plant dir "$D/0.1" ;;
      second-version) plant dir "$D/0.0.99" ;;
      foreign-command) alter link "$BIN/syndeo-ui" /Applications/Foreign.app/Contents/MacOS/foreign ;;
      mode) alter mode "$D/$version/syndeo-agent" 775 ;;
      acl) alter acl "$D/$version" "everyone allow list" ;;
      pending) plant link "$D/.current.new" elsewhere ;;
    esac
    before="$(package_state)"
    if /bin/sh "$D/$version/uninstall.sh" >"$st/logs/uninstall-$what" 2>&1; then
      fail "the uninstaller ran with $what planted"
    fi
    grep -q 'refusing:' "$st/logs/uninstall-$what" || fail "the uninstaller failed with $what planted, without refusing"
    must "with $what: refused, nothing changed, the receipt kept" eval "[ \"\$(package_state)\" = \"\$before\" ] && receipt_is '$version'"
    case "$what" in
      tools-extra) unplant "$D/$version/tools/extra.wat" ;;
      tree-extra) unplant "$D/$version/notes.txt" ;;
      private-file) unplant "$D/notes.txt" ;;
      private-dir) unplant "$D/backup" ;;
      bad-version) unplant "$D/0.1" ;;
      second-version) unplant "$D/0.0.99" ;;
      pending) unplant "$D/.current.new" ;;
      *) restore_last ;;
    esac
  done

  say ""
  say "a receipt that cannot be forgotten"
  # The fault is injected into a scratch copy of this version's uninstaller,
  # never into what is installed: the copy is the same template rendered with
  # PKGUTIL naming a test-only stand-in, which fails exactly the uninstaller's
  # forget and hands every other call, unchanged, to the real pkgutil. Neither
  # is part of any package.
  local fake="$st/pkgutil-forget-fails" scratch="$st/uninstall-forget-fails.sh" n gone
  cat >"$fake" <<'FAKE'
#!/bin/sh
# Test only, written by ci/test-pkg-install.sh: fails the one call that
# forgets Syndeo's receipt, and is the real pkgutil for every other.
if [ "$#" -eq 4 ] && [ "$1" = --forget ] && [ "$2" = com.sum.syndeo.pkg ] && [ "$3" = --volume ] && [ "$4" = / ]; then
    echo "pkgutil-forget-fails (test only): not forgetting com.sum.syndeo.pkg" >&2
    exit 1
fi
exec /usr/sbin/pkgutil "$@"
FAKE
  chown root:wheel "$fake" && chmod 755 "$fake" || fail "setting up $fake"
  must "the test's stand-in pkgutil is root:wheel, writable by root alone" test "$(stat -f '%Su:%Sg %Lp' "$fake")" = "root:wheel 755"
  "$here/package-macos-pkg.sh" --render uninstall "$version" '' 0 0 "$fake" "$scratch" >/dev/null || fail "rendering the scratch uninstaller"
  must "the scratch uninstaller differs from the installed one only in its PKGUTIL" \
    test "$(diff "$D/$version/uninstall.sh" "$scratch" | grep '^[<>]')" = "< PKGUTIL='/usr/sbin/pkgutil'
> PKGUTIL='$fake'"
  /bin/sh "$scratch" >"$st/logs/forget-fails" 2>&1
  status=$?
  must "exit 2, and the exact command to finish" eval \
    "[ $status = 2 ] && grep -qxF 'syndeo uninstall:     sudo /usr/sbin/pkgutil --forget $ID --volume /' '$st/logs/forget-fails'"
  gone=yes
  present "$D" && gone=no
  for n in $NAMES; do present "$BIN/$n" && gone=no; done
  must "  ... every payload path gone, and the receipt still there" eval "[ $gone = yes ] && receipt_is '$version'"
  # The command it printed, exactly as printed.
  sudo /usr/sbin/pkgutil --forget "$ID" --volume / >/dev/null || fail "the printed command did not finish"
  must "  ... and the printed command finishes it" pristine
  rm -f "${fake:?}" "${scratch:?}"
  must "$LIBEXEC/someone-else is still there" test -d "$LIBEXEC/someone-else"
  unplant "$LIBEXEC/someone-else"
  must "no home has a .syndeo it did not have" test "$(homes)" = "$(cat "$st/initial.homes")"
}

case "${1:-}" in
  # verify-pkg.sh's self-test borrows the functions above.
  --source-only) return 0 ;;
  --system-before) [ "$#" = 3 ] || refuse "--system-before <pkg> <version>"; system_before "$2" "$3" ;;
  --user) [ "$#" = 2 ] || refuse "--user <version>"; user "$2" ;;
  --system-after) [ "$#" = 3 ] || refuse "--system-after <pkg> <version>"; system_after "$2" "$3" ;;
  --cleanup) guard_root; cleanup ;;
  *) refuse "usage: $0 --system-before <pkg> <version> | --user <version> | --system-after <pkg> <version> | --cleanup" ;;
esac
