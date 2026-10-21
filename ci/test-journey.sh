#!/usr/bin/env bash
# The logs are the runner's: sudo runs each command, never the redirect into
# a log, on purpose.
# shellcheck disable=SC2024
# One customer's journey with Syndeo.app's package, on a pristine
# GitHub-hosted runner: a new customer downloads it, installs it, uses
# Syndeo, quits and opens it again, installs it again, and removes it.
#
#   SYNDEO_ALLOW_SYSTEM_INSTALL_TEST=yes SYNDEO_ALLOW_APP_TEST=yes \
#     bash ci/test-journey.sh run <pkg> <version> <sha256>
#   SYNDEO_ALLOW_SYSTEM_INSTALL_TEST=yes SYNDEO_ALLOW_APP_TEST=yes \
#     bash ci/test-journey.sh cleanup
#
# Two workflow steps, the second `if: always()`. <pkg> is the exact package
# an upstream job made, required by its SHA-256 before anything else:
# - in a pull request, macos-package's pr-package-adhoc-unsigned-<commit>;
# - in a release, the package job's, the one publish would publish;
# - after a release, the published asset.
# SYNDEO_EXPECT_SIGNED (no by default) says whether Gatekeeper has to accept
# it, signed, notarized and stapled, or reject it, ad hoc.
#
# It repeats nothing of ci/test-pkg-install.sh's states (the v0.1.x
# migration, refusals, faults, downgrades, the Trash) or of the breadth of
# ci/test-app.sh launch. It is one customer, end to end:
#   0. the runner is pristine, required, not assumed: nothing of Syndeo on
#      it; the trust settings, keychains and their certificates recorded;
#   1. the package, byte for byte the one given, in ~/Downloads, quarantined
#      as a browser leaves a download;
#   2. Gatekeeper's verdict on it;
#   3. installed with installer(8), as root. Installer's window and its
#      password prompt cannot be driven here; they are the manual Finder
#      check of every signed release;
#   4. Gatekeeper's verdict on the installed Syndeo.app, and, signed, its
#      stapled ticket;
#   5. ci/test-app.sh customer: opened by its path, as Finder opens it, it
#      browses https pinned, with Syndeo's authority in no trust settings,
#      quits with nothing left, and opens again; a reload measured;
#      then the trust settings, keychains and certificates unchanged;
#   6. installed again over itself, the user's data kept, and opened once
#      more (ci/test-app.sh customer again);
#   7. removed with its uninstaller, the user's data kept;
#   8. the trust settings, keychains and certificates exactly as step 0
#      found them, and no keystore.
# cleanup, whatever happened, puts the runner back as step 0 found it, with
# ci/test-pkg-install.sh's own cleanup, and requires it.
#
# The checks on the environment prevent accidents; they are not security.
set -uo pipefail

journey_here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Installing, Gatekeeper, the snapshots and cleanup are the package test's
# own; only its directory differs.
# shellcheck source=ci/test-pkg-install.sh
. "$journey_here/test-pkg-install.sh" --source-only
ST() { printf '%s' "$RUNNER_TEMP/syndeo-journey"; }

journey_guard() {
  guard
  [ "${SYNDEO_ALLOW_APP_TEST:-}" = yes ] || refuse "SYNDEO_ALLOW_APP_TEST is not yes"
}

# keys_and_trust: the trust settings, the user's and the admin's, whole; the
# keychain search lists; and every certificate in the login and System
# keychains, by its hashes, sorted.
keys_and_trust() {
  printf 'user trust settings:\n'
  security dump-trust-settings 2>&1
  printf 'admin trust settings:\n'
  security dump-trust-settings -d 2>&1
  printf 'keychains, user:\n'
  security list-keychains -d user 2>&1
  printf 'keychains, system:\n'
  security list-keychains -d system 2>&1
  printf 'certificates, login:\n'
  security find-certificate -a -Z "$HOME/Library/Keychains/login.keychain-db" 2>/dev/null | grep -E '^SHA-(1|256) hash: ' | LC_ALL=C sort
  printf 'certificates, System:\n'
  security find-certificate -a -Z /Library/Keychains/System.keychain 2>/dev/null | grep -E '^SHA-(1|256) hash: ' | LC_ALL=C sort
}

# keys_and_trust_kept WHEN: as step 0 recorded them, or how they differ.
keys_and_trust_kept() {
  keys_and_trust >"$(ST)/logs/keys-and-trust-$1.txt"
  cmp -s "$(ST)/keys-and-trust-before.txt" "$(ST)/logs/keys-and-trust-$1.txt" && return 0
  diff "$(ST)/keys-and-trust-before.txt" "$(ST)/logs/keys-and-trust-$1.txt" | sed 's/^/      | /'
  return 1
}

keystore_item() { security find-generic-password -s com.sum.syndeo.keystore >/dev/null 2>&1; }

# not_pristine: what of Syndeo is on this runner, one thing a line; nothing
# if it is pristine.
not_pristine() {
  local p
  pristine || echo "a Syndeo package, its receipt, or the v0.1.x commands"
  while IFS= read -r p; do present "$p" && echo "$p"; done < <(user_paths)
  defaults read "$BUNDLE_ID" >/dev/null 2>&1 && echo "the defaults of $BUNDLE_ID"
  ls_has "$APP" && echo "a LaunchServices record of $APP"
  keystore_item && echo "a keychain item for a keystore"
  none_running || echo "Syndeo running"
  return 0
}

# in_app [again]: ci/test-app.sh customer on the installed bundle.
in_app() {
  bash "$journey_here/test-app.sh" customer "$APP" "$@"
}

run() {
  local pkg="$1" version="$2" want="$3" st q found data log
  journey_guard
  st="$(ST)"
  [ -e "$st" ] && refuse "$st exists: this runner has been used for this journey before"
  [ -f "$pkg" ] || refuse "$pkg is not a file"
  printf '%s' "$want" | grep -qxE '[0-9a-f]{64}' || refuse "'$want' is not a SHA-256"
  mkdir -p "$st/logs"
  : >"$st/planted"
  : >"$st/altered"

  section "0. a pristine runner"
  found="$(not_pristine)"
  [ -z "$found" ] || refuse "this runner is not pristine: $(printf '%s' "$found" | tr '\n' ';')"
  snapshot "$st/before"
  cp -R "$st/before" "$st/logs/before"
  keys_and_trust >"$st/keys-and-trust-before.txt"
  cp "$st/keys-and-trust-before.txt" "$st/logs/"
  pass "nothing of Syndeo here: no package, receipt, bundle, user data, defaults, LaunchServices record, keystore item or process"
  say "        recorded: the trust settings (user and admin), the keychain search lists, and the login and System keychains' certificates"

  section "1. the package, as a customer downloads it"
  must "$(basename "$pkg"), $(wc -c <"$pkg" | tr -d ' ') bytes, is byte for byte the one given: SHA-256 $want" \
    eval "printf '%s  %s\n' '$want' '$pkg' | shasum -a 256 -c - >/dev/null"
  q="$HOME/Downloads/$(basename "$pkg")"
  present "$q" && fail "$q is there already"
  cp "$pkg" "$q" || fail "copying the package to ~/Downloads"
  xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");Safari;$(uuidgen)" "$q"
  record_plant "$q"
  must "  ... in ~/Downloads, quarantined as a browser leaves a download" eval "xattr -p com.apple.quarantine '$q' >/dev/null 2>&1"
  pkgutil --check-signature "$q" >"$st/logs/download-signature" 2>&1 || true

  section "2. Gatekeeper, on the download"
  gatekeeper download "the download" "$q" install
  if [ "${SYNDEO_EXPECT_SIGNED:-no}" = yes ]; then
    must "  ... its notarization ticket is stapled, and validates" validated_ticket "$q" "$st/logs/stapler-validate-download"
  fi

  section "3. installed"
  install installed "$q" || { sed 's/^/      | /' "$last_log.installer"; fail "the install failed"; }
  must "installed: Syndeo.app $version in /Applications, its receipt $version" eval "app_is '$version' && receipt_is $APP_ID '$version'"

  section "4. Gatekeeper, on the installed Syndeo.app"
  gatekeeper installed-app "the installed Syndeo.app" "$APP" execute
  if [ "${SYNDEO_EXPECT_SIGNED:-no}" = yes ]; then
    must "  ... its notarization ticket is stapled, and validates" validated_ticket "$APP" "$st/logs/stapler-validate-installed-app"
  fi

  section "5. opened, used, quit, and opened again"
  must "in Syndeo.app: opened by its path, it browses https pinned, quits, and opens again (ci/test-app.sh customer)" in_app
  must "  ... and nothing of it runs" none_running
  must "  ... the trust settings, keychains and certificates as they were: Syndeo's authority added to none" keys_and_trust_kept after-use

  section "6. installed again"
  data="$(settled_user_data)"
  [ -n "$data" ] || fail "no user data of Syndeo's to keep: it has not been opened"
  install reinstalled "$q" || { sed 's/^/      | /' "$last_log.installer"; fail "installing it again failed"; }
  must "installed again: Syndeo.app $version, its receipt $version" eval "app_is '$version' && receipt_is $APP_ID '$version'"
  must "  ... the user's data kept, byte for byte" user_data_kept "$data"
  must "  ... opened once more, it browses, and quits (ci/test-app.sh customer again)" in_app again
  must "  ... and nothing of it runs" none_running

  section "7. removed"
  data="$(settled_user_data)"
  log="$st/logs/uninstall"
  # The runner's record of the bundle, while it is still there to name.
  ls_unregister "$APP"
  sudo -n /bin/sh "$APP/Contents/Resources/uninstall.sh" >"$log" 2>&1 ||
    { sed 's/^/      | /' "$log"; fail "the uninstaller failed"; }
  must "the uninstaller removed Syndeo.app $version, and its receipt" eval "! present '$APP' && no_receipt $APP_ID && grep -q 'removed Syndeo.app $version' '$log'"
  must "  ... nothing of it runs" none_running
  must "  ... the user's data kept, byte for byte, as the uninstaller says it keeps it" user_data_kept "$data"

  section "8. what it left alone"
  must "the trust settings, keychains and certificates exactly as the pristine runner had them" keys_and_trust_kept end
  must "no keychain item for a keystore" eval "! keystore_item"
  must "no keystore in Syndeo's home" eval "! present '$HOME/Library/Application Support/Syndeo/.keystore'"
}

journey_cleanup() {
  journey_guard
  cleanup
}

case "${1:-}" in
  run) [ "$#" = 4 ] || refuse "usage: $0 run <pkg> <version> <sha256>"; run "$2" "$3" "$4"; say ""; say "every step of the journey passed" ;;
  cleanup) journey_cleanup ;;
  *) refuse "usage: $0 run <pkg> <version> <sha256> | cleanup" ;;
esac
