#!/usr/bin/env bash
# Diagnostic only: not a release step and not one of the pull request's
# checks. It records what Installer does to Syndeo's package on a real Mac,
# before the package's recovery rules are redesigned around it.
#
#   sudo --preserve-env=SYNDEO_ALLOW_SYSTEM_INSTALL_TEST,GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_TEMP,GITHUB_WORKSPACE \
#     bash ci/diagnose-pkg-upgrade.sh --run <probe>
#   sudo --preserve-env=... bash ci/diagnose-pkg-upgrade.sh --cleanup
#
# Three scenarios, each from a runner with no Syndeo on it:
#   upgrade             0.0.9, then 0.0.10;
#   fault-at-entry      0.0.9, then a 0.0.10 whose postinstall fails as it
#                       starts, before it switches `current`; then that same
#                       package again;
#   fault-after-switch  0.0.9, then a 0.0.10 whose postinstall fails just after
#                       it switches `current`; then that same package again.
# In each, a 0.0.9 `syndeo` is started before the upgrade and keeps running
# through it.
#
# At every checkpoint it records:
# - the receipt: identifier, version, location and file list;
# - the version directories;
# - `current` and the seven command links;
# - whether each command starts;
# - where a newly started syndeo finds each sibling;
# - where the running 0.0.9 syndeo finds each sibling;
# - what the PR's verify-pkg.sh makes of it.
# For every install it keeps:
# - installer(8)'s output;
# - its slice of /var/log/install.log;
# - the unified log of installd, installer and the package scripts;
# - a trace, to the millisecond, of the version directories, `current`, the
#   receipt and /usr/local/bin/syndeo, which puts deletion, placement, the
#   switch and the receipt in order.
#
# It judges nothing about the package: an install that fails is recorded, and
# the diagnostic goes on. It stops early only if the runner is not as
# expected, or if a baseline install of 0.0.9 fails, and then it still puts
# the runner back.
#
# The packages are stand-ins, built by the PR's ci/package-macos-pkg.sh from
# tarballs in which:
# - `syndeo` is <probe>, crates/syndeo-shell/examples/locate_probe.rs, which
#   asks the shell's own Supervisor::locate;
# - the other six commands are shell scripts.
# Each faulty package is 0.0.10's package with one block added to its
# postinstall, and nothing else different. The block records what it finds,
# fails the first time it runs, and lets every later run through, so
# installing the same package again is the retry an administrator would make.
#
# --cleanup, whatever happened:
# - stops the probes and the traces;
# - removes what the stand-ins installed, in whatever state they left it;
# - then checks the runner as ci/test-pkg-install.sh --cleanup does.
# It fails if anything differs. Only logs are kept.
set -uo pipefail

diag_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The guards, the records, the snapshots and the final check, shared with the
# install test.
# shellcheck source=ci/test-pkg-install.sh
. "$diag_here/test-pkg-install.sh" --source-only

summary() { printf '%s\n' "$*" | tee -a "$(ST)/logs/summary.txt"; }

# as_runner COMMAND...: as the runner user, with a clean environment whose
# PATH starts with /usr/local/bin, as a user's would.
as_runner() {
  sudo -u "$RUNNER" /usr/bin/env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/Users/runner "$@"
}

# ------------------------------------------------------------------ packages

# diag_standin V: a stand-in package for V, with <probe> as `syndeo`.
diag_standin() {
  local v="$1" dir name n repo
  dir="$(ST)/standins/$v"
  name="syndeo-$v-aarch64-apple-darwin"
  mkdir -p "$dir/src/$name/tools"
  cp "$PROBE" "$dir/src/$name/syndeo"
  chmod 755 "$dir/src/$name/syndeo"
  for n in $NAMES; do
    [ "$n" = syndeo ] && continue
    printf '#!/bin/sh\necho "%s %s"\n' "$n" "$v" >"$dir/src/$name/$n"
    chmod 755 "$dir/src/$name/$n"
  done
  cp "$here/../README.md" "$here/../LICENSE" "$dir/src/$name/"
  cp "$here/../crates/syndeo-agent/tools/wordcount.wat" "$dir/src/$name/tools/"
  tar -C "$dir/src" --format=ustar -czf "$dir/$name.tar.gz" "$name" || return 1
  repo="$(cd "$here/.." && pwd)"
  SOURCE_DATE_EPOCH="$(git -c safe.directory="$repo" -C "$repo" log -1 --format=%ct)" \
    "$here/package-macos-pkg.sh" "$v" "$dir/$name.tar.gz" "$dir" >"$(ST)/logs/build-$v" 2>&1 || return 1
  printf '%s' "$dir/$name.pkg"
}

# fault_variant PKG LABEL entry|after-switch: PKG with the fault block added
# to its postinstall, before its first command or just after the switch.
fault_variant() {
  local pkg="$1" label="$2" where="$3" dir marker
  dir="$(ST)/variants/$label"
  # What the block finds, and whether it has fired, go straight to the logs.
  marker="$(ST)/logs/postinstall-$label"
  mkdir -p "$dir"
  pkgutil --expand "$pkg" "$dir/expanded" || return 1
  cp -p "$dir/expanded/syndeo.pkg/Scripts/postinstall" "$dir/postinstall.original"
  python3 -I - "$dir/expanded/syndeo.pkg/Scripts/postinstall" "$where" "$marker" "$label" <<'PYEOF' || return 1
import os
import sys

path, where, marker, label = sys.argv[1:5]
block = """\
# Diagnostic fault ({label}): records what it finds, then fails the first run only.
{{ /bin/echo "run at $(/bin/date -u '+%H:%M:%S') ({where})"; /bin/ls -la /usr/local/libexec/syndeo; /bin/echo "current -> $(/usr/bin/readlink /usr/local/libexec/syndeo/current)"; }} >>'{marker}.seen' 2>&1 || :
if [ ! -e '{marker}.fired' ]; then
    : >'{marker}.fired'
    /bin/echo "syndeo postinstall: diagnostic fault {where}: failing this first run"
    exit 1
fi
""".format(label=label, where=where, marker=marker)
lines = open(path).read().split("\n")
if where == "entry":
    # Before its first command.
    if lines.count("set -eu") != 1:
        sys.exit("set -eu is not in the postinstall exactly once")
    at = lines.index("set -eu")
else:
    switch = '/bin/mv -h -f "$SYNDEO_DIR/.current.new" "$SYNDEO_DIR/current"'
    if lines.count(switch) != 1:
        sys.exit("the switch is not in the postinstall exactly once")
    at = lines.index(switch) + 1
lines[at:at] = block.rstrip("\n").split("\n")
tmp = path + ".new"
with open(tmp, "w") as f:
    f.write("\n".join(lines))
os.chmod(tmp, 0o755)
os.rename(tmp, path)
PYEOF
  diff "$dir/postinstall.original" "$dir/expanded/syndeo.pkg/Scripts/postinstall" >"$(ST)/logs/variant-$label.diff"
  pkgutil --flatten "$dir/expanded" "$dir/$(basename "$pkg")" || return 1
  # Nothing but the postinstall differs from the package it was made from.
  pkgutil --expand "$dir/$(basename "$pkg")" "$dir/check" || return 1
  pkgutil --expand "$pkg" "$dir/original" || return 1
  local p
  for p in Distribution syndeo.pkg/Bom syndeo.pkg/Payload syndeo.pkg/PackageInfo syndeo.pkg/Scripts/preinstall; do
    cmp -s "$dir/original/$p" "$dir/check/$p" || { echo "variant $label: $p differs" >>"$(ST)/logs/variant-$label.diff"; return 1; }
  done
  printf '%s' "$dir/$(basename "$pkg")"
}

# ------------------------------------------------------------------ running

# probe_start LABEL: start a `syndeo hold` from /usr/local/bin, as runner.
probe_start() {
  PROBE_DIR="$(ST)/probe-$1"
  mkdir -p "$PROBE_DIR"
  chown "$RUNNER" "$PROBE_DIR"
  as_runner "$BIN/syndeo" hold "$PROBE_DIR" >"$(ST)/logs/probe-$1.log" 2>&1 &
  local _
  for _ in $(seq 1 100); do
    grep -q '^holding:' "$(ST)/logs/probe-$1.log" 2>/dev/null && return 0
    sleep 0.1
  done
  fail "the 0.0.9 syndeo did not start: $(cat "$(ST)/logs/probe-$1.log")"
}

# probe_ask LABEL: the running syndeo's sibling lookup, now.
probe_ask() {
  local label="$1" _
  printf '%s\n' "$label" >"$PROBE_DIR/.ask" && mv -f "$PROBE_DIR/.ask" "$PROBE_DIR/ask"
  for _ in $(seq 1 100); do
    if [ -f "$PROBE_DIR/answer.$label" ]; then
      cat "$PROBE_DIR/answer.$label"
      return 0
    fi
    sleep 0.1
  done
  if pgrep -f "syndeo hold $PROBE_DIR" >/dev/null; then
    echo "NO ANSWER within 10 seconds; the process is still running"
  else
    echo "NO ANSWER within 10 seconds; the process is gone"
  fi
}

stop_probes() {
  local d
  for d in "$(ST)"/probe-*; do
    [ -d "$d" ] && : >"$d/stop"
  done
  sleep 0.3
  pkill -f "syndeo hold $(ST)/probe-" 2>/dev/null || true
}

stop_traces() {
  : >"$(ST)/trace.stop" 2>/dev/null
  sleep 0.1
  pkill -f "diagnose-pkg-trace.py" 2>/dev/null || true
}

# ------------------------------------------------------------------ recording

# diag_install LABEL PKG: install PKG, with everything around it recorded.
# Sets last_status; never fails.
diag_install() {
  local label="$1" pkg="$2" log tracer before start end
  install_count=$((install_count + 1))
  log="$(ST)/logs/$(printf '%02d' "$install_count")-$label"
  rm -f "$(ST)/trace.stop"
  python3 -I "$diag_here/diagnose-pkg-trace.py" "$(ST)/trace.stop" >"$log.trace" 2>&1 &
  tracer=$!
  sleep 0.3
  before="$(wc -l </var/log/install.log | tr -d ' ')"
  start="$(date '+%Y-%m-%d %H:%M:%S')"
  installer -verbose -pkg "$pkg" -target / >"$log.installer" 2>&1
  last_status=$?
  # installd writes its last lines, and the trace sees the last changes.
  sleep 3
  : >"$(ST)/trace.stop"
  wait "$tracer" 2>/dev/null
  sleep 1
  end="$(date '+%Y-%m-%d %H:%M:%S')"
  printf 'installer exit %s\n' "$last_status" >>"$log.installer"
  tail -n +"$((before + 1))" /var/log/install.log >"$log.install-log"
  log show --start "$start" --end "$end" --style compact --info --debug \
    --predicate 'process == "installd" OR process == "system_installd" OR process == "package_script_service" OR process == "installer"' \
    >"$log.unified-log" 2>&1
  local said
  said="$(grep -hoE 'syndeo (preinstall|postinstall): .*' "$log.install-log" | tr '\n' ';')"
  summary "  install $label: installer exit $last_status; ${said:-the scripts said nothing}"
}

receipt_version() { pkgutil --pkg-info "$ID" --volume / 2>/dev/null | sed -n 's/^version: //p'; }

# checkpoint LABEL: everything about the package's state, now.
checkpoint() {
  local label="$1" f n p rv out status links=0 starts=0 new_own=0 new_other=0 new_none=0 old_own=0 old_other=0 old_none=0 answer fresh
  checkpoint_count=$((checkpoint_count + 1))
  f="$(ST)/logs/checkpoint-$(printf '%02d' "$checkpoint_count")-$label.txt"
  rv="$(receipt_version)"
  fresh="$(as_runner "$BIN/syndeo" locate 2>&1; printf '(exit %s)\n' "$?")"
  answer="$(probe_ask "$label")"
  {
    printf '== checkpoint %s, %s UTC\n' "$label" "$(date -u '+%H:%M:%S')"
    echo "== the receipt (pkgutil --pkg-info, each line as sed -n l shows it)"
    pkgutil --pkg-info "$ID" --volume / 2>&1 | sed -n l
    echo "== its files (pkgutil --files)"
    pkgutil --files "$ID" --volume / 2>&1
    echo "== its files on disk under /var/db/receipts"
    ls -la /var/db/receipts/"$ID".* 2>/dev/null || echo "(none)"
    echo "== $D"
    if present "$D"; then
      ls -la "$D"
      for p in "$D"/*; do
        if [ -d "$p" ] && [ ! -L "$p" ]; then
          echo "-- $p"
          ls -la "$p"
        fi
      done
    else
      echo "absent"
    fi
    echo "== current"
    if [ -L "$D/current" ]; then
      printf 'current -> %s (%s)\n' "$(readlink "$D/current")" "$([ -e "$D/current" ] && echo resolves || echo dangling)"
    else
      echo "current: absent"
    fi
    [ -L "$D/.current.new" ] && printf '.current.new -> %s\n' "$(readlink "$D/.current.new")"
    echo "== the command links"
    for n in $NAMES; do
      p="$BIN/$n"
      [ -e "$p" ] && links=$((links + 1))
      if [ -L "$p" ]; then
        printf '%s -> %s (%s)\n' "$p" "$(readlink "$p")" "$([ -e "$p" ] && echo "resolves to $(/usr/bin/readlink -f "$p")" || echo dangling)"
      elif present "$p"; then
        printf '%s: not a link\n' "$p"
      else
        printf '%s: absent\n' "$p"
      fi
    done
    echo "== each command, started as runner from $BIN"
    for n in $NAMES; do
      out="$(as_runner "$BIN/$n" --version 2>&1)"
      status=$?
      [ "$status" = 0 ] && starts=$((starts + 1))
      printf '%s: exit %s: %s\n' "$n" "$status" "$(printf '%s' "$out" | head -n 2 | tr '\n' ' ')"
    done
    echo "== a newly started syndeo's sibling lookup (syndeo locate, as runner)"
    printf '%s\n' "$fresh"
    echo "== the running 0.0.9 syndeo's sibling lookup (started before the upgrade)"
    printf '%s\n' "$answer"
    echo "== ci/verify-pkg.sh installed <the receipt's version>, the PR's own check, for reference"
    if [ -n "$rv" ]; then
      bash "$here/verify-pkg.sh" installed "$rv" 2>&1 | sed 's/\x1b\[[0-9;]*m//g'
    else
      echo "(no receipt)"
    fi
  } >"$f" 2>&1

  new_own="$(printf '%s\n' "$fresh" | grep -c ' own-version$')"
  new_other="$(printf '%s\n' "$fresh" | grep -c ' OTHER-VERSION$')"
  new_none="$(printf '%s\n' "$fresh" | grep -c ' NOT-FOUND ')"
  old_own="$(printf '%s\n' "$answer" | grep -c ' own-version$')"
  old_other="$(printf '%s\n' "$answer" | grep -c ' OTHER-VERSION$')"
  old_none="$(printf '%s\n' "$answer" | grep -c ' NOT-FOUND ')"
  summary "  checkpoint $label:"
  summary "    receipt: ${rv:-none}$( [ -n "$rv" ] && printf ', location line %s, %s files' "$(pkgutil --pkg-info "$ID" --volume / 2>/dev/null | grep '^location:' | sed -n l | tr '\n' ' ')" "$(pkgutil --files "$ID" --volume / 2>/dev/null | wc -l | tr -d ' ')")"
  summary "    $D: $(if present "$D"; then ls -A "$D" | tr '\n' ' '; else echo absent; fi)$( [ -L "$D/current" ] && printf '; current -> %s (%s)' "$(readlink "$D/current")" "$([ -e "$D/current" ] && echo resolves || echo dangling)")"
  summary "    command links resolving: $links/7; commands starting: $starts/7"
  summary "    a new syndeo's siblings: $new_own own version, $new_other another version, $new_none not found"
  summary "    the running 0.0.9 syndeo's siblings: $old_own own version, $old_other another version, $old_none not found$(printf '%s' "$answer" | grep -q '^NO ANSWER' && echo '; NO ANSWER')"
}

# ------------------------------------------------------------------ resetting

# diag_reset: remove what the stand-ins installed, in whatever state it is
# in: the receipt, the seven links if they are the package's, and in the
# private directory only `current`, `.current.new` and 0.0.x trees holding
# only the package's names. Anything else is left and reported.
diag_reset() {
  local problems="" n p v
  if pkgutil --pkg-info "$ID" --volume / >/dev/null 2>&1; then
    pkgutil --forget "$ID" --volume / >/dev/null 2>&1 || problems="$problems; could not forget the receipt"
  fi
  for n in $NAMES; do
    p="$BIN/$n"
    if [ -L "$p" ] && [ "$(readlink "$p")" = "../libexec/syndeo/current/$n" ]; then
      rm -f "$p"
    elif present "$p"; then
      problems="$problems; $p is not the package's link"
    fi
  done
  if present "$D"; then
    if [ -d "$D" ] && [ ! -L "$D" ]; then
      for p in "$D/current" "$D/.current.new"; do
        [ -L "$p" ] && rm -f "$p"
      done
      for v in "$D"/*; do
        present "$v" || continue
        case "${v##*/}" in
          0.0.[0-9] | 0.0.[1-9][0-9]) ;;
          *) problems="$problems; $v is not a stand-in's"; continue ;;
        esac
        if [ ! -d "$v" ] || [ -L "$v" ]; then
          problems="$problems; $v is not a directory"
          continue
        fi
        for n in $NAMES uninstall.sh README.md LICENSE tools/wordcount.wat; do
          if [ -f "$v/$n" ] && [ ! -L "$v/$n" ]; then rm -f "$v/$n"; fi
        done
        [ -d "$v/tools" ] && [ ! -L "$v/tools" ] && rmdir "$v/tools" 2>/dev/null
        rmdir "$v" 2>/dev/null || problems="$problems; $v still holds: $(ls -A "$v" | tr '\n' ' ')"
      done
      rmdir "$D" 2>/dev/null || problems="$problems; $D still holds: $(ls -A "$D" | tr '\n' ' ')"
    else
      problems="$problems; $D is not a directory"
    fi
  fi
  pristine || problems="$problems; Syndeo is still on the runner"
  if [ -n "$problems" ]; then
    say "reset: FAILED$problems"
    return 1
  fi
  say "reset: no receipt, no $D, no command link"
}

diag_restore() {
  local status=0
  stop_probes
  stop_traces
  if [ -d "$(ST)" ]; then
    diag_reset || status=1
  fi
  cleanup || status=1
  return "$status"
}

diag_exit() {
  local status=$?
  trap - EXIT INT TERM HUP
  if [ "$status" -ne 0 ]; then
    say "the diagnostic stopped early (status $status)"
  fi
  diag_restore || status=1
  exit "$status"
}

# ------------------------------------------------------------------ the run

# scenario NAME PKG once|again
scenario() {
  local name="$1" pkg="$2" again="$3"
  summary ""
  summary "scenario: $name"
  diag_install "$name-install-0.0.9" "$S9"
  [ "$last_status" = 0 ] || fail "$name: the baseline install of 0.0.9 failed"
  probe_start "$name"
  checkpoint "$name-0.0.9-installed"
  diag_install "$name-upgrade" "$pkg"
  checkpoint "$name-after-the-upgrade"
  if [ "$again" = again ]; then
    diag_install "$name-the-same-package-again" "$pkg"
    checkpoint "$name-after-the-same-package-again"
  fi
  stop_probes
  diag_reset >>"$(ST)/logs/summary.txt" 2>&1 || fail "$name: the runner could not be reset"
  summary "  reset to no Syndeo"
}

diag_run() {
  PROBE="$1"
  guard_root
  local st p
  st="$(ST)"
  [ -e "$st" ] && refuse "$st exists: this runner has been used for this before"
  pristine || refuse "Syndeo is already on this runner"
  [ -f "$PROBE" ] && [ -x "$PROBE" ] || refuse "no probe at $PROBE"
  mkdir -p "$st/logs"
  : >"$st/planted"
  : >"$st/altered"
  checkpoint_count=0
  trap diag_exit EXIT
  trap 'exit 130' INT TERM HUP

  summary "Diagnostic installs of stand-in packages, versions 0.0.9 and 0.0.10, identifier $ID, on this runner;"
  summary "each is removed again, and the runner is checked against how it was. No release package is built."
  parents >"$st/initial.parents"
  security_state >"$st/initial.security"
  homes >"$st/initial.homes"
  receipts >"$st/initial.receipts"
  cp "$st/initial.parents" "$st/logs/initial.parents"
  # As the install test does: strict parents, made plainly if absent, and
  # /usr/local/bin as the runner image has it, runner:admin 0775.
  for p in /usr/local "$LIBEXEC"; do
    present "$p" || continue
    [ -n "$(ls -lde "$p" | sed 1d)" ] && refuse "$p has an ACL; not changing that"
    [ "$(stat -f %u "$p")" = 0 ] || alter owner "$p" root:wheel
    case "$(stat -f %Lp "$p")" in *[2367]? | *[2367]) alter mode "$p" 755 ;; esac
  done
  for p in "$BIN" "$LIBEXEC"; do
    if ! present "$p"; then
      mkdir "$p" && chown root:wheel "$p" && chmod 755 "$p" || fail "making $p"
    fi
  done
  alter owner "$BIN" "$RUNNER:admin"
  alter mode "$BIN" 775

  S9="$(diag_standin 0.0.9)" || fail "building the stand-in 0.0.9"
  local s10 entry after
  s10="$(diag_standin 0.0.10)" || fail "building the stand-in 0.0.10"
  entry="$(fault_variant "$s10" fault-at-entry entry)" || fail "building the fault-at-entry package"
  after="$(fault_variant "$s10" fault-after-switch after-switch)" || fail "building the fault-after-switch package"
  summary "built: $S9, $s10, $entry, $after"

  scenario upgrade "$s10" once
  scenario fault-at-entry "$entry" again
  scenario fault-after-switch "$after" again

  summary ""
  summary "every scenario ran"
}

case "${1:-}" in
  --run) [ "$#" = 2 ] || refuse "--run <probe>"; diag_run "$2" ;;
  --cleanup) guard_root; diag_restore ;;
  *) refuse "usage: $0 --run <probe> | --cleanup" ;;
esac
