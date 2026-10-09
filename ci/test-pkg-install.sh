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
#   - 0.0.9, then 0.0.10, the numeric upgrade. The old tree is kept and the
#     owner and mode of /usr/local/bin are left alone;
#   - 0.0.9's uninstaller refuses;
#   - 0.0.11's postinstall fails, leaving an interrupted state;
# - the real package is installed over that.
# It leaves the real package installed.
#
# --user, as runner: the commands from PATH, doctor and its siblings, the
# example tools seeded on first run, the agent listing them, a fetch and a
# cache hit, a supervisor's children, and syndeo-webkit's proxy. That last one
# uses a throwaway keychain holding the proxy's authority without trust
# settings, and the user's keychain search list is restored by a trap.
#
# --system-after, as root:
# - the switch under load, with the real binaries and doctor: only ENOENT or
#   EINVAL while switching, siblings always from one version, nothing failing
#   afterwards;
# - a same-version reinstall, then reinstalls refused over every kind of
#   altered tree;
# - a downgrade refused;
# - disagreement between current, the receipt and the commands refused;
# - --old-versions;
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

# ------------------------------------------------------------------ installing

install_count=0

# install LABEL PKG [TARGET]: installer(8), keeping its output and the
# package's lines from /var/log/install.log.
install() {
  local label="$1" pkg="$2" target="${3:-/}" before status
  install_count=$((install_count + 1))
  local log
  log="$(ST)/logs/$(printf '%02d' "$install_count")-$label"
  before="$(wc -l </var/log/install.log | tr -d ' ')"
  installer -pkg "$pkg" -target "$target" >"$log.installer" 2>&1
  status=$?
  last_log="$log"
  last_before="$before"
  log_lines
  return "$status"
}

log_lines() {
  tail -n +"$((last_before + 1))" /var/log/install.log | grep -E 'syndeo (preinstall|postinstall)|installer\[' >"$last_log.install-log" || true
}

# said TEXT: the package said TEXT during the last install. installd may
# write its lines a moment after installer returns, so it is read again for a
# few seconds before giving up.
said() {
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    grep -qF -- "$1" "$last_log.install-log" && return 0
    sleep 0.5
    log_lines
  done
  return 1
}

# refused LABEL PKG TEXT: the install fails, the package says TEXT, and
# nothing of Syndeo's changed.
refused() {
  local label="$1" pkg="$2" text="$3" before
  before="$(package_state)"
  if install "$label" "$pkg"; then
    fail "$label: the install succeeded"
  fi
  said "$text" || { sed 's/^/      | /' "$last_log.install-log"; fail "$label: refused, but not saying \"$text\""; }
  [ "$(package_state)" = "$before" ] || fail "$label: refused, but something changed"
  pass "$label: refused, saying \"$text\", nothing changed"
}

installed() {
  local label="$1" pkg="$2" text="$3" version="$4"
  install "$label" "$pkg" || { sed 's/^/      | /' "$last_log.installer" "$last_log.install-log"; fail "$label: the install failed"; }
  said "$text" || { sed 's/^/      | /' "$last_log.install-log"; fail "$label: installed, but the preinstall did not say \"$text\""; }
  bash "$here/verify-pkg.sh" installed "$version" >"$last_log.verify" 2>&1 ||
    { sed 's/^/      | /' "$last_log.verify"; fail "$label: the installation is not exactly $version"; }
  pass "$label: installed ($text), and verify-pkg installed $version passes"
}

# standin VERSION [--test-fault F]: a package of shell stand-ins, from the real builder.
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
  shift
  # The checkout is the runner's and this is root: git would refuse it.
  local repo
  repo="$(cd "$here/.." && pwd)"
  SOURCE_DATE_EPOCH="$(git -c safe.directory="$repo" -C "$repo" log -1 --format=%ct)" \
    "$here/package-macos-pkg.sh" "$v" "$dir/$name.tar.gz" "$dir" "$@" >/dev/null ||
    fail "building the stand-in $v"
  printf '%s' "$dir/$name.pkg"
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
  # Processes the user step started, if any are left.
  if [ -f "$(US)/pids" ]; then
    while read -r p; do kill "$p" 2>/dev/null; done <"$(US)/pids"
  fi
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
  for f in "/var/db/receipts/$ID.plist" "/var/db/receipts/$ID.bom"; do
    [ -e "$f" ] && chflags nouchg "$f"
  done
  if pkgutil --pkg-info "$ID" >/dev/null 2>&1; then
    v="$(pkgutil --pkg-info "$ID" | sed -n 's/^version: //p')"
    if [ -d "$D/$v" ] && ! current_is "$v"; then
      rm -f "$D/current"; ln -s "$v" "$D/current"; chown -h root:wheel "$D/current"
    fi
    "$here/package-macos-pkg.sh" --render uninstall "$v" '' 0 0 /usr/sbin/pkgutil "$st/uninstall.expected" >/dev/null
    if cmp -s "$st/uninstall.expected" "$D/$v/uninstall.sh"; then
      /bin/sh "$D/$v/uninstall.sh" || problems="$problems; the package's uninstaller did not finish"
    else
      problems="$problems; $D/$v/uninstall.sh is not the repository's, so it was not run"
    fi
  elif present "$D"; then
    problems="$problems; $D is there with no receipt"
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
  say "a disk image mounted at $ALT, and the receipt files under /var/db/receipts."

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
  say "stand-ins: an upgrade, an old uninstaller, an interrupted upgrade"
  local s9 s10 s11
  s9="$(standin 0.0.9)" || fail "building the stand-in 0.0.9"
  s10="$(standin 0.0.10)" || fail "building the stand-in 0.0.10"
  s11="$(standin 0.0.11 --test-fault postinstall-fails)" || fail "building the stand-in 0.0.11"
  alter owner "$BIN" "$RUNNER:admin"
  alter mode "$BIN" 775
  local bin_before
  bin_before="$(snap_path "$BIN")"
  installed standin-0.0.9 "$s9" "installing 0.0.9" 0.0.9
  must "  ... $BIN is still $RUNNER:admin 0775: overwrite-permissions=\"false\" held" test "$(snap_path "$BIN")" = "$bin_before"
  installed standin-0.0.10 "$s10" "upgrading from 0.0.9 to 0.0.10" 0.0.10
  must "  ... the upgrade kept 0.0.9's tree" test -x "$D/0.0.9/syndeo"
  must "  ... and the receipt lists only 0.0.10's paths" eval "! pkgutil --files $ID | grep -q 0.0.9"
  must "  ... $BIN is still $RUNNER:admin 0775" test "$(snap_path "$BIN")" = "$bin_before"
  local before
  before="$(package_state)"
  must "0.0.9's uninstaller refuses after the upgrade" eval "! /bin/sh '$D/0.0.9/uninstall.sh' >'$st/logs/old-uninstaller' 2>&1 && grep -q \"this is 0.0.9's uninstaller\" '$st/logs/old-uninstaller'"
  must "  ... and with --old-versions" eval "! /bin/sh '$D/0.0.9/uninstall.sh' --old-versions >>'$st/logs/old-uninstaller' 2>&1"
  must "  ... changing nothing" test "$(package_state)" = "$before"
  if install standin-0.0.11-fault "$s11"; then fail "the fault build installed"; fi
  must "an interrupted upgrade (0.0.11's postinstall stops before the switch): receipt and current still 0.0.10" eval "receipt_is 0.0.10 && current_is 0.0.10 && [ -x '$D/0.0.11/syndeo' ]"
  must "  ... and every command still runs 0.0.10" commands_say 0.0.10
  plant link "$D/.current.new" 0.0.10

  say ""
  say "the real package, over all of that"
  installed real "$pkg" "upgrading from 0.0.10 to $version" "$version"
  forget_plant "$D/.current.new"
  must "  ... the stale .current.new is gone, and 0.0.9, 0.0.10 and 0.0.11 are kept" eval "! present '$D/.current.new' && [ -d '$D/0.0.9' ] && [ -d '$D/0.0.10' ] && [ -d '$D/0.0.11' ]"
  must "  ... $BIN is still $RUNNER:admin 0775" test "$(snap_path "$BIN")" = "$bin_before"
  must "  ... no home has a .syndeo it did not have" test "$(homes)" = "$(cat "$st/initial.homes")"
  must "  ... keychains and trust settings are as they were" test "$(security_state)" = "$(cat "$st/initial.security")"
  printf '%s\n' "$s9" "$s10" >"$st/standins"
}

# ------------------------------------------------------------------ user

user_finish() {
  local status=$?
  trap - EXIT INT TERM HUP
  local us problems=""
  us="$(US)"
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
  local version="$1" us h out n child pid
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
  out="$(clean_env syndeo-agent --net-socket "$us/net.sock" --shell-socket "$us/shell.sock" --task tools --tools "$h/tools" 2>&1)"
  must "the agent lists wordcount as ready" eval "printf '%s\n' \"\$out\" | grep -qE 'wordcount +ready'"
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
  s10="$(sed -n 2p "$st/standins")"

  say ""
  say "the switch under load, with the real binaries"
  # A second complete tree of the same build, to switch to and from.
  local copy=0.0.98 n
  plant dir "$D/$copy"
  plant dir "$D/$copy/tools"
  for n in $NAMES uninstall.sh; do
    cp -p "$D/$version/$n" "$D/$copy/$n"
    printf '%s\t%s\n' "$D/$copy/$n" "$(meta "$D/$copy/$n")" >>"$st/planted"
  done
  for n in README.md LICENSE tools/wordcount.wat; do
    cp -p "$D/$version/$n" "$D/$copy/$n"
    printf '%s\t%s\n' "$D/$copy/$n" "$(meta "$D/$copy/$n")" >>"$st/planted"
  done
  mkdir -p "$st/stress-home"
  chown "$RUNNER" "$st/stress-home"
  expand_payload "$pkg" "$st/payload"
  out="$(python3 -I "$here/switch-stress.py" --private "$D" --command "$BIN/syndeo" --versions "$version" "$copy" \
    --expect-doctor --as-user "$RUNNER" --env "SYNDEO_HOME=$st/stress-home/{thread}" --renames 2000 --min-seconds 20 \
    --postinstall "$version" "$st/payload/syndeo.pkg/Scripts/postinstall" --postinstalls 5 \
    --settle-lookups 200 --settle-launches 5 2>&1)"
  local status=$?
  printf '%s\n' "$out" | tee "$st/logs/switch-stress" | sed 's/^/      | /'
  must "only ENOENT or EINVAL while switching; doctor's siblings always from one version; nothing failing afterwards" test "$status" = 0
  must "  ... current ends at $version" current_is "$version"
  for n in README.md LICENSE tools/wordcount.wat $NAMES uninstall.sh; do
    unplant "$D/$copy/$n"
  done
  unplant "$D/$copy/tools"
  unplant "$D/$copy"
  bash "$here/verify-pkg.sh" installed "$version" >"$st/logs/after-stress" 2>&1 || fail "the installation is not exactly $version after the stress test"

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
  say "downgrades and disagreement"
  refused downgrade "$s10" "this package is the older 0.0.10"
  alter link "$D/current" 0.0.10
  refused current-older-than-the-receipt "$pkg" "points at 0.0.10, older than the installed $version"
  restore_last
  alter link "$BIN/syndeo-ui" /Applications/Foreign.app/Contents/MacOS/foreign
  refused foreign-command-with-a-receipt "$pkg" "$BIN/syndeo-ui: is not a link this package writes"
  restore_last
  bash "$here/verify-pkg.sh" installed "$version" >"$st/logs/after-refusals" 2>&1 || fail "the installation changed during the refusals"

  say ""
  say "--old-versions"
  plant dir "$D/9.9.9"
  plant file "$D/9.9.9/syndeo" "#!/bin/sh
" root:wheel 755
  /bin/sh "$D/$version/uninstall.sh" --old-versions >"$st/logs/old-versions" 2>&1 ||
    { sed 's/^/      | /' "$st/logs/old-versions"; fail "--old-versions"; }
  must "--old-versions removes 0.0.9, 0.0.10 and 0.0.11, and keeps $version, current and the newer 9.9.9" eval \
    "! present '$D/0.0.9' && ! present '$D/0.0.10' && ! present '$D/0.0.11' && [ -d '$D/$version' ] && current_is '$version' && [ -d '$D/9.9.9' ] && receipt_is '$version'"
  unplant "$D/9.9.9/syndeo"
  unplant "$D/9.9.9"

  say ""
  say "the uninstaller, all or nothing"
  local what
  plant dir "$LIBEXEC/someone-else"
  for what in tools-extra tree-extra private-file private-dir bad-version foreign-command mode acl pending; do
    case "$what" in
      tools-extra) plant file "$D/$version/tools/extra.wat" "x" ;;
      tree-extra) plant file "$D/$version/notes.txt" "x" ;;
      private-file) plant file "$D/notes.txt" "x" ;;
      private-dir) plant dir "$D/backup" ;;
      bad-version) plant dir "$D/0.1" ;;
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
      pending) unplant "$D/.current.new" ;;
      *) restore_last ;;
    esac
  done

  say ""
  say "a receipt that cannot be forgotten"
  local f
  for f in "/var/db/receipts/$ID.plist" "/var/db/receipts/$ID.bom"; do
    [ -e "$f" ] || fail "no $f"
    alter flags "$f" uchg
  done
  /bin/sh "$D/$version/uninstall.sh" >"$st/logs/forget-fails" 2>&1
  status=$?
  must "exit 2 and the exact command to finish" eval "[ $status = 2 ] && grep -qF 'sudo /usr/sbin/pkgutil --forget $ID --volume /' '$st/logs/forget-fails'"
  must "  ... every file gone, the receipt still there" eval "! present '$D' && ! present '$BIN/syndeo' && receipt_is '$version'"
  restore_last
  restore_last
  /usr/sbin/pkgutil --forget "$ID" --volume / >/dev/null || fail "the printed command did not finish"
  must "  ... and the printed command finishes it" pristine
  must "$LIBEXEC/someone-else is still there" test -d "$LIBEXEC/someone-else"
  unplant "$LIBEXEC/someone-else"
  must "no home has a .syndeo it did not have" test "$(homes)" = "$(cat "$st/initial.homes")"
}

case "${1:-}" in
  # ci/diagnose-pkg-upgrade.sh borrows the helpers above.
  --source-only) return 0 ;;
  --system-before) [ "$#" = 3 ] || refuse "--system-before <pkg> <version>"; system_before "$2" "$3" ;;
  --user) [ "$#" = 2 ] || refuse "--user <version>"; user "$2" ;;
  --system-after) [ "$#" = 3 ] || refuse "--system-after <pkg> <version>"; system_after "$2" "$3" ;;
  --cleanup) guard_root; cleanup ;;
  *) refuse "usage: $0 --system-before <pkg> <version> | --user <version> | --system-after <pkg> <version> | --cleanup" ;;
esac
