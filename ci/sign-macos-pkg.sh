#!/usr/bin/env bash
# Sign the macOS installer package, and, once Apple has accepted it, staple
# it; and staple Syndeo.app, which Apple accepted with the binaries, before
# the package is built from it. Notarization itself is ci/notarize.py's.
#
#   ci/sign-macos-pkg.sh staple-app <Syndeo.app.zip> <out-dir> <diagnostics-dir>
#   ci/sign-macos-pkg.sh sign <unsigned.pkg> <signed.pkg> <state-dir>
#   ci/sign-macos-pkg.sh finish <signed.pkg> <state-dir> <notarization-record> <diagnostics-dir>
#   ci/sign-macos-pkg.sh --self-test
#
# Only for a release that ci/check-signing-config.sh calls signed
# (SYNDEO_EXPECT_SIGNED=yes); both modes refuse anything else.
#
# sign, stopping at the first failure:
#  1. expand the unsigned package (pkgutil --expand-full), and keep its
#     manifest in <state-dir>, which must not exist yet;
#  2. productsign it with the Developer ID Installer identity, from a keychain
#     of its own, into <signed.pkg>;
#  3. expand that, and require it identical to the unsigned one: the
#     Distribution, PackageInfo, BOM, scripts and every payload path, with its
#     bytes, mode, owner and link target. Only the signature may differ.
# Before it changes anything it reads the user keychain search list, and stops
# if it cannot. Whatever happens after that, cleanup attempts all of: putting
# that list back, deleting its keychain, removing its working directory (the
# certificate and the expansions), and reading the list back to check it is
# exactly as it was. If any of those fails, that is a failure too, even of a
# package otherwise signed.
#
# finish, after `ci/notarize.py submit <signed.pkg> --record R` and `wait`:
#  4. refuse unless R says Apple accepted it, and the file R submitted, by
#     name and SHA-256, is <signed.pkg> exactly as it is now;
#  5. staple the ticket to it, once, and validate it. Only validation is
#     retried, up to five attempts, after 0, 15, 30, 60 and 120 seconds; it
#     has to pass before anything else happens. Nothing is ever submitted
#     again. Each stapler command, its exit status and its complete output
#     are shown in the log, and kept;
#  6. expand it once more, and require it identical to the unsigned package;
#  7. require Gatekeeper to accept it as Notarized Developer ID, from the
#     Installer identity.
# If finish fails once stapling has begun, it keeps, in <diagnostics-dir>
# (which must not exist yet), these and nothing else: the package exactly as
# it is, its SHA-256, stapler's output, pkgutil --check-signature,
# Gatekeeper's assessment, and the user, admin and system trust settings,
# read only. Never a keychain, a certificate, a key, a password or the
# environment.
#
# In either mode, on any failure <signed.pkg> is removed, so nothing half-done
# can be shipped, and the exit status is the first failure's.
#
# staple-app, after notarize-binaries' one submission, which carried
# Syndeo.app with the seven executables, was accepted, and release.yml has
# checked <Syndeo.app.zip> is the bundle that submission carried, by its
# SHA-256:
#  A1. unpack it into <out-dir>, which must not exist yet, and record every
#      path in the bundle with its bytes, mode and owner;
#  A2. staple the ticket to it, once, and validate it, validation retried as
#      in 5;
#  A3. require the stapled bundle identical to the one unpacked, but for one
#      file stapler adds, Contents/CodeResources, the ticket;
#  A4. require its code signature to verify, strictly, with everything nested
#      in it;
#  A5. require Gatekeeper to accept it to run, as Notarized Developer ID, from
#      a Developer ID Application identity of MACOS_TEAM_ID.
# If it fails once stapling has begun, it keeps, in <diagnostics-dir>, the
# stapled bundle as a ZIP with its SHA-256, stapler's output, codesign's and
# Gatekeeper's, with the identity's name left out, and the trust settings;
# and it removes <out-dir>, so no package is built from it.
#
# The secrets: sign uses MACOS_INSTALLER_CERTIFICATE_P12_BASE64,
# MACOS_INSTALLER_CERTIFICATE_PASSWORD, MACOS_INSTALLER_SIGNING_IDENTITY and
# MACOS_TEAM_ID; finish uses MACOS_INSTALLER_SIGNING_IDENTITY; staple-app
# uses MACOS_TEAM_ID. Nothing is
# traced, and no value is printed. The tools are found on PATH, which is how
# the self-test puts stand-ins in their place.
set -euo pipefail
LC_ALL=C
export LC_ALL

say() { printf 'sign-macos-pkg: %s\n' "$*"; }
die() {
  printf 'sign-macos-pkg: %s\n' "$*" >&2
  exit 1
}

# manifest DIR: every path below DIR, one per line, with its type, mode,
# owner, and its SHA-256 or link target.
manifest() {
  (
    cd "$1"
    find . -print0 | LC_ALL=C sort -z | while IFS= read -r -d '' p; do
      if [ -L "$p" ]; then
        printf '%s link %s -> %s\n' "$p" "$(stat -f '%Lp %u:%g' "$p")" "$(readlink "$p")"
      elif [ -d "$p" ]; then
        printf '%s dir %s\n' "$p" "$(stat -f '%Lp %u:%g' "$p")"
      else
        printf '%s file %s %s\n' "$p" "$(stat -f '%Lp %u:%g %z' "$p")" "$(shasum -a 256 <"$p" | cut -d' ' -f1)"
      fi
    done
  )
}

# search_list TEXT: what `security list-keychains` printed, one keychain per
# line, without its indentation and quotes.
search_list() {
  local line
  while IFS= read -r line; do
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line#\"}"
    line="${line%\"}"
    [ -z "$line" ] || printf '%s\n' "$line"
  done <<<"$1"
}

# same_as_unsigned STEP DIR: DIR, an expansion, is exactly the unsigned one.
same_as_unsigned() {
  manifest "$2" >"$work/$1.manifest"
  if ! cmp -s "$state/unsigned.manifest" "$work/$1.manifest"; then
    diff "$state/unsigned.manifest" "$work/$1.manifest" | grep '^[<>]' | head -10 >&2 || true
    die "$1: the package's contents differ from the unsigned package's"
  fi
}

# signed_yes: SYNDEO_EXPECT_SIGNED says yes.
signed_yes() {
  [ "${SYNDEO_EXPECT_SIGNED-}" = yes ] || die "refusing: SYNDEO_EXPECT_SIGNED is not yes, so there is nothing to sign"
}

# not_blank NAME...: each is set to something besides white space.
not_blank() {
  local n missing=''
  for n in "$@"; do [[ "${!n-}" =~ [^[:space:]] ]] || missing="$missing $n"; done
  [ -z "$missing" ] || die "refusing: not set:$missing"
}

sign() {
  case "$-" in *x*) die "refusing: tracing is on, and would print the secrets" ;; esac
  [ "$#" = 3 ] || die "usage: $0 sign <unsigned.pkg> <signed.pkg> <state-dir>"
  unsigned="$1"
  out="$2"
  state="$3"
  signed_yes
  not_blank MACOS_INSTALLER_CERTIFICATE_P12_BASE64 MACOS_INSTALLER_CERTIFICATE_PASSWORD MACOS_INSTALLER_SIGNING_IDENTITY MACOS_TEAM_ID
  local identity="$MACOS_INSTALLER_SIGNING_IDENTITY" team="$MACOS_TEAM_ID"
  case "$identity" in *[[:cntrl:]]*) die "refusing: MACOS_INSTALLER_SIGNING_IDENTITY is not one line" ;; esac
  [[ "$team" =~ ^[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10}$ ]] || die "refusing: MACOS_TEAM_ID is not ten capital letters and digits"
  [[ "$identity" =~ ^Developer\ ID\ Installer:\ .+\ \(([ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10})\)$ ]] && [ "${BASH_REMATCH[1]}" = "$team" ] ||
    die "refusing: MACOS_INSTALLER_SIGNING_IDENTITY is not a Developer ID Installer identity of MACOS_TEAM_ID"
  [ -f "$unsigned" ] || die "no $unsigned"
  [ ! -e "$out" ] && [ ! -L "$out" ] || die "$out exists"
  [ ! -e "$state" ] && [ ! -L "$state" ] || die "$state exists"

  # The search list as it is, read before anything is changed or made, to
  # put back and check whatever happens. If it cannot be read, nothing has
  # been changed, and nothing is.
  local listing line
  listing="$(security list-keychains -d user)" || die "could not read the keychain search list; nothing was changed"
  saved_list="$(search_list "$listing")"
  saved=()
  while IFS= read -r line; do
    [ -z "$line" ] || saved+=("$line")
  done <<<"$saved_list"

  umask 077
  work=''
  keychain=''
  keychain_made=no
  succeeded=no
  trap cleanup_sign EXIT
  trap 'exit 130' INT TERM HUP
  mkdir "$state"
  work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sign-macos-pkg.XXXXXX")"
  keychain="$work/installer.keychain-db"

  pkgutil --expand-full "$unsigned" "$work/unsigned" >/dev/null || die "1. could not expand $unsigned"
  manifest "$work/unsigned" >"$state/unsigned.manifest"
  say "1. expanded the unsigned package: $(wc -l <"$state/unsigned.manifest" | tr -d ' ') paths"

  local password
  password="$(openssl rand -hex 32)"
  keychain_made=yes
  security create-keychain -p "$password" "$keychain" >/dev/null
  security set-keychain-settings -lut 21600 "$keychain" >/dev/null
  security unlock-keychain -p "$password" "$keychain" >/dev/null
  printf '%s' "$MACOS_INSTALLER_CERTIFICATE_P12_BASE64" | base64 --decode >"$work/installer.p12" ||
    die "2. the Installer certificate is not base64"
  security import "$work/installer.p12" -k "$keychain" -P "$MACOS_INSTALLER_CERTIFICATE_PASSWORD" \
    -T /usr/bin/productsign >/dev/null || die "2. could not import the Installer certificate"
  rm -f "$work/installer.p12"
  security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null
  security list-keychains -d user -s "$keychain" ${saved[@]+"${saved[@]}"}
  productsign --sign "$identity" --keychain "$keychain" --timestamp "$unsigned" "$out" >/dev/null ||
    die "2. productsign failed"
  say "2. signed with the Installer identity"

  pkgutil --expand-full "$out" "$work/signed" >/dev/null || die "3. could not expand the signed package"
  same_as_unsigned 3.signed "$work/signed"
  say "3. the signed package's contents are exactly the unsigned package's"
  succeeded=yes
  say "signed, not yet notarized: $out ($(shasum -a 256 "$out" | cut -d' ' -f1))"
}

# cleanup_sign: every step attempted, whatever failed before it; any of them
# failing is a failure, and the first failure's status is kept.
cleanup_sign() {
  local status=$? failed='' now
  # No step may stop the ones after it.
  set +e
  if ! security list-keychains -d user -s ${saved[@]+"${saved[@]}"} >/dev/null 2>&1; then
    failed="$failed putting the keychain search list back;"
  fi
  if [ "$keychain_made" = yes ] || { [ -n "$keychain" ] && [ -e "$keychain" ]; }; then
    security delete-keychain "$keychain" >/dev/null 2>&1 || failed="$failed deleting the temporary keychain;"
  fi
  if [ -n "$work" ]; then
    rm -rf "$work" 2>/dev/null || true
    [ ! -e "$work" ] || failed="$failed removing the working directory;"
  fi
  if now="$(security list-keychains -d user 2>/dev/null)"; then
    [ "$(search_list "$now")" = "$saved_list" ] || failed="$failed the keychain search list is not as it was;"
  else
    failed="$failed reading the keychain search list back;"
  fi
  if [ -n "$failed" ]; then
    printf 'sign-macos-pkg: cleanup failed:%s\n' "$failed" >&2
    succeeded=no
    [ "$status" != 0 ] || status=1
  fi
  if [ "$succeeded" != yes ]; then
    rm -f "${out:?}" 2>/dev/null || true
    [ ! -e "$out" ] || printf 'sign-macos-pkg: could not remove %s\n' "$out" >&2
    [ "$status" != 0 ] || status=1
  fi
  exit "$status"
}

# recorded LOG COMMAND...: run COMMAND, and show its exact command line, its
# complete output (stdout and stderr together) and its exit status, in the
# log and appended to LOG. Returns COMMAND's status. Only for stapler, whose
# command line and output name nothing but the package.
recorded() {
  local log="$1" status output block
  shift
  output="$("$@" 2>&1)" && status=0 || status=$?
  block="$(printf '$ %s\n%s[exit %s]' "$*" "${output:+$output$'\n'}" "$status")"
  printf '%s\n' "$block" >>"$log"
  printf '%s\n' "$block" | sed 's/^/    /'
  return "$status"
}

finish() {
  case "$-" in *x*) die "refusing: tracing is on, and would print the secrets" ;; esac
  [ "$#" = 4 ] || die "usage: $0 finish <signed.pkg> <state-dir> <notarization-record> <diagnostics-dir>"
  out="$1"
  state="$2"
  local record="$3"
  diag="$4"
  # The same umask as sign's, so the stapled expansion compares like for like
  # with the unsigned one.
  umask 077
  work=''
  succeeded=no
  stapling=no
  trap cleanup_finish EXIT
  trap 'exit 130' INT TERM HUP
  signed_yes
  not_blank MACOS_INSTALLER_SIGNING_IDENTITY
  local identity="$MACOS_INSTALLER_SIGNING_IDENTITY"
  [ -f "$out" ] || die "no $out"
  [ ! -e "$diag" ] || die "refusing: $diag already exists, and diagnostics are only ever written fresh"
  [ -f "$state/unsigned.manifest" ] || die "no $state/unsigned.manifest: was the package signed by this script's sign mode?"

  # 4. Apple accepted exactly this file.
  local verdict
  verdict="$(python3 -I - "$record" "$out" 2>&1 <<'PY'
import hashlib, json, os, sys
record, pkg = sys.argv[1:3]
try:
    sub = json.load(open(os.path.join(record, "submission.json")))
    res = json.load(open(os.path.join(record, "result.json")))
except (OSError, ValueError) as e:
    sys.exit("no notarization record in %s (%s)" % (record, e.__class__.__name__))
h = hashlib.sha256(open(pkg, "rb").read()).hexdigest()
if res.get("outcome") != "accepted" or res.get("id") != sub.get("id"):
    sys.exit("submission %s was not accepted: %s" % (sub.get("id"), res.get("outcome")))
if sub.get("file") != os.path.basename(pkg) or sub.get("sha256") != h:
    sys.exit("submission %s was %s (%s), not this %s (%s)" % (sub.get("id"), sub.get("file"), sub.get("sha256"), os.path.basename(pkg), h))
print(sub["id"])
PY
)" || die "4. refusing to staple: $verdict"
  say "4. Apple accepted submission $verdict, which is exactly this file"

  work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sign-macos-pkg.XXXXXX")"
  : >"$work/stapler.txt"
  stapling=yes
  recorded "$work/stapler.txt" xcrun stapler staple "$out" || die "5. could not staple the ticket"
  # Only validation is retried: the ticket is stapled once, and nothing here
  # ever submits anything again.
  local delay attempt=0 validated=no
  for delay in 0 15 30 60 120; do
    attempt=$((attempt + 1))
    if [ "$delay" != 0 ]; then
      say "5. the stapled ticket did not validate; validating again in ${delay}s (attempt $attempt of 5)"
      sleep "$delay"
    fi
    if recorded "$work/stapler.txt" xcrun stapler validate "$out"; then
      validated=yes
      break
    fi
  done
  [ "$validated" = yes ] || die "5. the stapled ticket does not validate, after $attempt attempts"
  say "5. the ticket is stapled and validates (validation attempt $attempt)"

  pkgutil --expand-full "$out" "$work/stapled" >/dev/null || die "6. could not expand the stapled package"
  same_as_unsigned 6.stapled "$work/stapled"
  say "6. the stapled package's contents are exactly the unsigned package's"

  local assessed=yes result
  spctl -a -vv -t install "$out" >"$work/spctl" 2>&1 || assessed=no
  result="$(cat "$work/spctl")"
  if [ "$assessed" = yes ] && printf '%s\n' "$result" | grep -qxF "$out: accepted" &&
    printf '%s\n' "$result" | grep -qx 'source=Notarized Developer ID' &&
    printf '%s\n' "$result" | grep -qxF "origin=$identity"; then
    say "7. Gatekeeper accepts it: Notarized Developer ID, from the Installer identity"
  else
    printf '%s\n' "${result//"$identity"/<the Installer identity>}" | sed 's/^/    /' >&2
    die "7. Gatekeeper does not accept it as Notarized Developer ID from the Installer identity"
  fi
  succeeded=yes
  say "signed, notarized and stapled: $out"
}

# keep_diagnostics: what finish keeps once stapling has begun and it fails,
# in $diag, and nothing else; see the top of this file. Run before the
# package is removed, and never changing the exit status.
keep_diagnostics() {
  local name="${out##*/}" failed=0
  mkdir -p "$diag" || return 1
  cp -p "$out" "$diag/$name" || failed=1
  (cd "$diag" && shasum -a 256 "$name") >"$diag/$name.sha256" || failed=1
  cp "$work/stapler.txt" "$diag/stapler.txt" || failed=1
  pkgutil --check-signature "$out" >"$diag/pkgutil-check-signature.txt" 2>&1
  spctl -a -vv -t install "$out" >"$diag/gatekeeper.txt" 2>&1
  {
    printf '== security dump-trust-settings (user)\n'
    security dump-trust-settings 2>&1
    printf '== security dump-trust-settings -d (admin)\n'
    security dump-trust-settings -d 2>&1
    printf '== security dump-trust-settings -s (system)\n'
    security dump-trust-settings -s 2>&1
  } >"$diag/trust-settings.txt"
  say "kept the diagnostics in $diag: $(cd "$diag" && ls | tr '\n' ' ')"
  return "$failed"
}

cleanup_finish() {
  local status=$?
  set +e
  if [ "$succeeded" != yes ] && [ "$stapling" = yes ]; then
    keep_diagnostics || printf 'sign-macos-pkg: could not keep all of the diagnostics in %s\n' "$diag" >&2
  fi
  [ -z "$work" ] || rm -rf "$work"
  if [ "$succeeded" != yes ]; then
    rm -f "${out:?}" 2>/dev/null || true
    [ ! -e "$out" ] || printf 'sign-macos-pkg: could not remove %s\n' "$out" >&2
    [ "$status" != 0 ] || status=1
  fi
  exit "$status"
}

# ------------------------------------------------------------------ staple-app

# validated LOG PATH: stapler validate PATH, retried as finish retries it.
# Prints the attempt it passed on; fails after five.
validated() {
  local log="$1" path="$2" delay attempt=0
  for delay in 0 15 30 60 120; do
    attempt=$((attempt + 1))
    if [ "$delay" != 0 ]; then
      say "the stapled ticket did not validate; validating again in ${delay}s (attempt $attempt of 5)" >&2
      sleep "$delay"
    fi
    if recorded "$log" xcrun stapler validate "$path" >&2; then
      echo "$attempt"
      return 0
    fi
  done
  return 1
}

staple_app() {
  case "$-" in *x*) die "refusing: tracing is on, and would print the secrets" ;; esac
  [ "$#" = 3 ] || die "usage: $0 staple-app <Syndeo.app.zip> <out-dir> <diagnostics-dir>"
  local zip="$1" team attempt
  out_dir="$2"
  diag="$3"
  umask 022
  work=''
  succeeded=no
  stapling=no
  made=no
  app=''
  trap cleanup_staple_app EXIT
  trap 'exit 130' INT TERM HUP
  signed_yes
  not_blank MACOS_TEAM_ID
  team="$MACOS_TEAM_ID"
  [[ "$team" =~ ^[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10}$ ]] || die "refusing: MACOS_TEAM_ID is not ten capital letters and digits"
  [ -f "$zip" ] || die "no $zip"
  [ ! -e "$out_dir" ] && [ ! -L "$out_dir" ] || die "$out_dir exists"
  [ ! -e "$diag" ] || die "refusing: $diag already exists, and diagnostics are only ever written fresh"

  mkdir -p "$out_dir"
  made=yes
  ditto -x -k "$zip" "$out_dir" || die "A1. could not unpack $zip"
  app="$out_dir/Syndeo.app"
  [ "$(cd "$out_dir" && ls -A)" = Syndeo.app ] && [ -d "$app" ] && [ ! -L "$app" ] || die "A1. $zip does not hold Syndeo.app alone"
  work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sign-macos-pkg.XXXXXX")"
  manifest "$app" >"$work/before.manifest"
  [ ! -e "$app/Contents/CodeResources" ] || die "A1. the bundle holds a ticket already"
  say "A1. unpacked Syndeo.app, $(wc -l <"$work/before.manifest" | tr -d ' ') paths"

  : >"$work/stapler.txt"
  stapling=yes
  recorded "$work/stapler.txt" xcrun stapler staple "$app" || die "A2. could not staple the ticket"
  attempt="$(validated "$work/stapler.txt" "$app")" || die "A2. the stapled ticket does not validate, after 5 attempts"
  say "A2. the ticket is stapled and validates (validation attempt $attempt)"

  manifest "$app" >"$work/after.manifest"
  if ! grep -vE '^\./Contents/CodeResources file ' "$work/after.manifest" | cmp -s - "$work/before.manifest" ||
    [ "$(grep -cE '^\./Contents/CodeResources file 644 ' "$work/after.manifest")" != 1 ]; then
    diff "$work/before.manifest" "$work/after.manifest" | grep '^[<>]' | head -10 >&2 || true
    die "A3. stapling changed the bundle other than by adding its ticket, Contents/CodeResources"
  fi
  say "A3. the stapled bundle is the one Apple accepted, byte for byte, with its ticket added"

  codesign --verify --strict --deep "$app" >"$work/codesign.txt" 2>&1 || die "A4. its code signature does not verify"
  say "A4. its code signature verifies, strictly, with everything nested in it"

  local assessed=yes result origin
  spctl -a -vv -t execute "$app" >"$work/spctl" 2>&1 || assessed=no
  result="$(cat "$work/spctl")"
  origin="$(printf '%s\n' "$result" | sed -n 's/^origin=//p')"
  if [ "$assessed" = yes ] && printf '%s\n' "$result" | grep -qxF "$app: accepted" &&
    printf '%s\n' "$result" | grep -qx 'source=Notarized Developer ID' &&
    [[ "$origin" =~ ^Developer\ ID\ Application:\ .+\ \(([ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10})\)$ ]] &&
    [ "${BASH_REMATCH[1]}" = "$team" ]; then
    say "A5. Gatekeeper accepts it to run: Notarized Developer ID, from a Developer ID Application identity of the team"
  else
    printf '%s\n' "$result" | sed 's/^origin=.*/origin=<an identity, not shown>/' | sed 's/^/    /' >&2
    die "A5. Gatekeeper does not accept it to run as Notarized Developer ID from a Developer ID Application identity of the team"
  fi
  succeeded=yes
  say "stapled: $app"
}

# keep_app_diagnostics: what staple-app keeps once stapling has begun and it
# fails, in $diag, and nothing else. Never changing the exit status.
keep_app_diagnostics() {
  local failed=0
  mkdir -p "$diag" || return 1
  if [ -d "$app" ]; then
    ditto -c -k --keepParent "$app" "$diag/Syndeo.app.zip" || failed=1
    (cd "$diag" && shasum -a 256 Syndeo.app.zip) >"$diag/Syndeo.app.zip.sha256" || failed=1
    codesign -dv --verbose=4 "$app" 2>&1 | sed 's/^Authority=Developer ID Application: .*/Authority=<the Application identity, not shown>/' \
      >"$diag/codesign.txt"
    spctl -a -vv -t execute "$app" 2>&1 | sed 's/^origin=.*/origin=<an identity, not shown>/' >"$diag/gatekeeper.txt"
  fi
  cp "$work/stapler.txt" "$diag/stapler.txt" || failed=1
  {
    printf '== security dump-trust-settings (user)\n'
    security dump-trust-settings 2>&1
    printf '== security dump-trust-settings -d (admin)\n'
    security dump-trust-settings -d 2>&1
    printf '== security dump-trust-settings -s (system)\n'
    security dump-trust-settings -s 2>&1
  } >"$diag/trust-settings.txt"
  say "kept the diagnostics in $diag: $(cd "$diag" && ls | tr '\n' ' ')"
  return "$failed"
}

cleanup_staple_app() {
  local status=$?
  set +e
  if [ "$succeeded" != yes ] && [ "$stapling" = yes ]; then
    keep_app_diagnostics || printf 'sign-macos-pkg: could not keep all of the diagnostics in %s\n' "$diag" >&2
  fi
  [ -z "$work" ] || rm -rf "$work"
  if [ "$succeeded" != yes ]; then
    # Only what this made: an output directory that was there is refused,
    # and left alone.
    if [ "$made" = yes ]; then
      rm -rf "${out_dir:?}" 2>/dev/null || true
      [ ! -e "$out_dir" ] || printf 'sign-macos-pkg: could not remove %s\n' "$out_dir" >&2
    fi
    [ "$status" != 0 ] || status=1
  fi
  exit "$status"
}

# ------------------------------------------------------------------ self-test

# The tools, as stand-ins that log what they were asked and do what the case
# says; pkgutil is the real one behind a log. Nothing here may notarize.
write_standins() {
  local bin="$1"
  mkdir -p "$bin"
  cat >"$bin/security" <<'EOF'
#!/bin/bash
echo "security $1" >>"$STANDIN_CALLS"
case "$1" in
  # Reading prints the list as security does; setting replaces it. From the
  # second setting on, which is cleanup's, the case may make it fail, or
  # succeed while leaving another list.
  list-keychains)
    if [ "${4:-}" = -s ]; then
      shift 4
      n=$(( $(cat "$STANDIN_LIST.sets" 2>/dev/null || echo 0) + 1 ))
      echo "$n" >"$STANDIN_LIST.sets"
      if [ "$n" -ge 2 ]; then
        case "${STANDIN_RESTORE:-works}" in
          fails) exit 1 ;;
          wrong) printf '%s\n' /Users/runner/Library/Keychains/login.keychain-db >"$STANDIN_LIST"; exit 0 ;;
        esac
      fi
      if [ "$#" = 0 ]; then : >"$STANDIN_LIST"; else printf '%s\n' "$@" >"$STANDIN_LIST"; fi
    else
      [ "${STANDIN_LIST_READ:-works}" = works ] || exit 1
      sed 's/.*/    "&"/' "$STANDIN_LIST"
    fi ;;
  create-keychain) : >"${@: -1}" ;;
  # As security does, deleting a keychain also takes it off the search list;
  # unless the case says deleting fails.
  delete-keychain)
    [ "${STANDIN_DELETE:-works}" = works ] || exit 1
    rm -f "$2"; grep -vxF "$2" "$STANDIN_LIST" >"$STANDIN_LIST.new" || true; mv "$STANDIN_LIST.new" "$STANDIN_LIST" ;;
  import) [ -s "$2" ] || exit 1 ;;
  # Read only, as on a runner with none set.
  dump-trust-settings) echo "SecTrustSettingsCopyCertificates: No Trust Settings were found." >&2; exit 1 ;;
esac
exit 0
EOF
  cat >"$bin/productsign" <<'EOF'
#!/bin/bash
echo "productsign" >>"$STANDIN_CALLS"
in="${@: -2:1}"; out="${@: -1}"
case "${STANDIN_PRODUCTSIGN:-copy}" in
  copy) cp "$in" "$out" ;;
  # An interruption while it signs: the signer is sent TERM.
  interrupted) kill -TERM "$PPID"; /bin/sleep 1; exit 1 ;;
  fails) exit 1 ;;
  *) cp "$STANDIN_PRODUCTSIGN" "$out" ;;
esac
EOF
  cat >"$bin/xcrun" <<'EOF'
#!/bin/bash
case "$1 $2" in
  "stapler staple")
    echo "xcrun stapler staple" >>"$STANDIN_CALLS"
    echo "Processing: $3"
    if [ "${STANDIN_STAPLE_FAILS:-no}" = yes ]; then echo "The staple and validate action failed! Error 65."; exit 65; fi
    case "$3" in
      *.app)
        # A ticket, as stapler adds one to a bundle; or not, or another
        # change besides, as the case says.
        [ "${STANDIN_APP_STAPLE:-ticket}" = none ] || { printf 'ticket' >"$3/Contents/CodeResources"; chmod 644 "$3/Contents/CodeResources"; }
        [ "${STANDIN_APP_STAPLE:-ticket}" != changes ] || printf 'changed' >>"$3/Contents/Info.plist" ;;
      *) [ -z "${STANDIN_STAPLE:-}" ] || cp "$STANDIN_STAPLE" "$3" ;;
    esac
    echo "The staple and validate action worked!" ;;
  "stapler validate")
    echo "xcrun stapler validate" >>"$STANDIN_CALLS"
    # STANDIN_VALIDATE_FAILS: how many validations fail before one passes
    # (all, by default none).
    n=$(( $(cat "$STANDIN_CALLS.validations" 2>/dev/null || echo 0) + 1 ))
    echo "$n" >"$STANDIN_CALLS.validations"
    echo "Processing: $3"
    if [ "${STANDIN_VALIDATE_FAILS:-0}" = all ] || [ "$n" -le "${STANDIN_VALIDATE_FAILS:-0}" ]; then
      echo "$3 does not have a ticket stapled to it (stand-in, validation $n)." >&2
      echo "The validate action failed! Error 65."
      exit 65
    fi
    echo "The validate action worked!" ;;
  *) echo "xcrun $1 $2" >>"$STANDIN_CALLS"; exit 1 ;;
esac
EOF
  cat >"$bin/spctl" <<'EOF'
#!/bin/bash
echo "spctl" >>"$STANDIN_CALLS"
if [ "${STANDIN_SPCTL:-accepted}" = accepted ]; then
  printf '%s: accepted\nsource=%s\norigin=%s\n' "${@: -1}" "${STANDIN_SOURCE:-Notarized Developer ID}" "$STANDIN_ORIGIN" >&2
else
  printf '%s: rejected\nsource=no usable signature\n' "${@: -1}" >&2
  exit 3
fi
EOF
  cat >"$bin/pkgutil" <<'EOF'
#!/bin/bash
echo "pkgutil $1" >>"$STANDIN_CALLS"
exec /usr/sbin/pkgutil "$@"
EOF
  cat >"$bin/codesign" <<'EOF'
#!/bin/bash
echo "codesign $1" >>"$STANDIN_CALLS"
case "$1" in
  --verify) [ "${STANDIN_UNVERIFIED:-no}" != yes ] ;;
  -dv) echo "Authority=Developer ID Application: SECRETVALUE Org (AB12CD34EF)" >&2 ;;
esac
EOF
  cat >"$bin/sleep" <<'EOF'
#!/bin/bash
echo "sleep $*" >>"$STANDIN_CALLS"
EOF
  chmod 755 "$bin"/*
}

self_test() {
  if [ "$(uname -s)" != Darwin ]; then
    printf '\n  on %s: signing a package needs pkgutil and pkgbuild, which are macOS-only; not run\n\n' "$(uname -s)"
    return 0
  fi
  local cases=0 wrong=0 team=AB12CD34EF
  # Not local: the EXIT trap runs after this function has returned.
  T="$(mktemp -d)"
  trap 'rm -rf "$T"' EXIT
  write_standins "$T/bin"
  local identity="Developer ID Installer: SECRETVALUE Org ($team)"
  # What the sign step is given; finish is given the identity alone.
  local -a signing=(
    SYNDEO_EXPECT_SIGNED=yes
    MACOS_INSTALLER_CERTIFICATE_P12_BASE64=U0VDUkVUVkFMVUUtaW5zdGFsbGVy
    MACOS_INSTALLER_CERTIFICATE_PASSWORD=SECRETVALUE-installer-password
    "MACOS_INSTALLER_SIGNING_IDENTITY=$identity"
    "MACOS_TEAM_ID=$team"
  )

  # Three packages: the one to sign, and two that differ from it in one
  # payload byte, or in its preinstall.
  local d="$T/pkgs"
  mkdir -p "$d/root/usr/local/libexec/signtest" "$d/scripts" "$d/root2/usr/local/libexec/signtest" "$d/scripts2"
  printf 'payload\n' >"$d/root/usr/local/libexec/signtest/file"
  printf 'payloae\n' >"$d/root2/usr/local/libexec/signtest/file"
  printf '#!/bin/sh\nexit 0\n' >"$d/scripts/preinstall"
  printf '#!/bin/sh\nexit 1\n' >"$d/scripts2/preinstall"
  chmod 755 "$d/scripts/preinstall" "$d/scripts2/preinstall"
  local r
  for r in "root scripts unsigned" "root2 scripts payload" "root scripts2 script"; do
    # shellcheck disable=SC2086 # three words, split on purpose
    set -- $r
    pkgbuild --root "$d/$1" --scripts "$d/$2" --identifier com.example.signtest --version 1 \
      --install-location / "$d/$3.pkg" >/dev/null 2>&1 || { echo "pkgbuild failed" >&2; return 1; }
  done

  # verdict_of LABEL VERDICT GOT OUT: count a case, and show what it did.
  verdict_of() {
    cases=$((cases + 1))
    if [ "$2" = yes ]; then
      printf '  ok    %s\n' "$1"
    else
      printf '  WRONG %s\n          | calls: %s\n' "$1" "$3"
      printf '%s\n' "$4" | sed 's/^/          | /'
      wrong=$((wrong + 1))
    fi
  }
  # says OUT TEXT: OUT holds each of TEXT's |-separated parts.
  says() {
    local t
    while IFS= read -r -d '|' t; do
      printf '%s' "$1" | grep -qF -- "$t" || return 1
    done <<<"$2|"
  }

  # sign_case LABEL WANT-STATUS WANT-CALLS WANT-TEXT [NAME=VALUE...]: sign
  # exits WANT-STATUS, calls exactly WANT-CALLS (the tools other than
  # security, in order, comma-separated), says WANT-TEXT, leaves the signed
  # package and the unsigned manifest only on success, never prints a
  # secret, never notarizes, and leaves nothing behind. Once it has made a
  # keychain, cleanup attempts every step: the list put back, the keychain
  # deleted, and the list read back last. The list ends as it began, unless
  # list_may_change is yes, for a case that breaks putting it back.
  # initial_list=empty starts it empty.
  list_may_change=no
  initial_list=two
  sign_case() {
    local label="$1" want="$2" calls="$3" text="$4" status out verdict=yes got
    shift 4
    rm -rf "$T/run"
    mkdir -p "$T/run/temp"
    : >"$T/run/calls"
    if [ "$initial_list" = empty ]; then
      : >"$T/run/list"
    else
      printf '%s\n' /Users/runner/Library/Keychains/login.keychain-db "/Library/Keychains/System.keychain" >"$T/run/list"
    fi
    cp "$T/run/list" "$T/run/list.before"
    out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" HOME="$T/run" RUNNER_TEMP="$T/run/temp" \
      STANDIN_CALLS="$T/run/calls" STANDIN_LIST="$T/run/list" "$@" \
      bash "$0" sign "$d/unsigned.pkg" "$T/run/signed.pkg" "$T/run/state" 2>&1)" && status=0 || status=$?
    got="$({ grep -v '^security' "$T/run/calls" || true; } | paste -sd, -)"
    [ "$status" = "$want" ] || verdict=no
    [ "$got" = "$calls" ] || verdict=no
    says "$out" "$text" || verdict=no
    if printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU'; then verdict=no; fi
    if grep -q notarytool "$T/run/calls"; then verdict=no; fi
    if [ "$list_may_change" != yes ]; then cmp -s "$T/run/list" "$T/run/list.before" || verdict=no; fi
    [ -z "$(ls -A "$T/run/temp")" ] || verdict=no
    if grep -qx 'security create-keychain' "$T/run/calls"; then
      # Every cleanup step attempted: the list set back after the keychain
      # was added to it, the keychain deleted, and the list read back last.
      [ "$(grep -cx 'security list-keychains' "$T/run/calls")" -ge 4 ] &&
        sed -n '/^security create-keychain$/,$p' "$T/run/calls" | grep -qx 'security delete-keychain' &&
        [ "$(tail -n 1 "$T/run/calls")" = 'security list-keychains' ] || verdict=no
    fi
    if [ "$want" = 0 ]; then
      [ -f "$T/run/signed.pkg" ] && [ -s "$T/run/state/unsigned.manifest" ] || verdict=no
      grep -qx 'security create-keychain' "$T/run/calls" || verdict=no
    else
      [ ! -e "$T/run/signed.pkg" ] || verdict=no
    fi
    verdict_of "$label" "$verdict" "$got" "$out"
  }
  local signed="pkgutil --expand-full,productsign,pkgutil --expand-full"

  printf '\n  sign: expand, productsign, compare; never notarize\n'
  sign_case "a consistent configuration: expand, sign, compare; the manifest kept for finish" \
    0 "$signed" "signed, not yet notarized" "${signing[@]}"
  sign_case "SYNDEO_EXPECT_SIGNED=no: refused before any tool runs" \
    1 "" "SYNDEO_EXPECT_SIGNED is not yes" SYNDEO_EXPECT_SIGNED=no
  local n v partial
  for n in MACOS_INSTALLER_CERTIFICATE_P12_BASE64 MACOS_INSTALLER_CERTIFICATE_PASSWORD MACOS_INSTALLER_SIGNING_IDENTITY MACOS_TEAM_ID; do
    partial=()
    for v in "${signing[@]}"; do [ "${v%%=*}" = "$n" ] || partial+=("$v"); done
    sign_case "without $n: refused before any tool runs" 1 "" "not set: $n" "${partial[@]}"
  done
  sign_case "an Installer identity of another team: refused" 1 "" "not a Developer ID Installer identity of MACOS_TEAM_ID" \
    "${signing[@]}" "MACOS_INSTALLER_SIGNING_IDENTITY=Developer ID Installer: SECRETVALUE Org (ZZ99ZZ99ZZ)"
  sign_case "an Application identity where the Installer one belongs: refused" 1 "" "not a Developer ID Installer identity" \
    "${signing[@]}" "MACOS_INSTALLER_SIGNING_IDENTITY=Developer ID Application: SECRETVALUE Org ($team)"
  sign_case "a productsign that changes a payload byte: caught, the package removed" \
    1 "$signed" "3.signed: the package's contents differ" "${signing[@]}" STANDIN_PRODUCTSIGN="$d/payload.pkg"
  sign_case "a productsign that changes the preinstall: caught, the package removed" \
    1 "$signed" "3.signed: the package's contents differ" "${signing[@]}" STANDIN_PRODUCTSIGN="$d/script.pkg"

  printf '\n  sign: the keychain search list, and cleanup\n'
  sign_case "the search list cannot be read: stopped before any keychain or output" \
    1 "" "could not read the keychain search list; nothing was changed" "${signing[@]}" STANDIN_LIST_READ=fails
  sign_case "putting the search list back fails: fatal, though it signed; the package removed" \
    1 "$signed" "cleanup failed: putting the keychain search list back;" "${signing[@]}" STANDIN_RESTORE=fails
  sign_case "deleting the keychain fails: fatal, though it signed; the package removed, the list put back" \
    1 "$signed" "cleanup failed: deleting the temporary keychain;" "${signing[@]}" STANDIN_DELETE=fails
  list_may_change=yes
  sign_case "both fail: both reported, the list found wrong, the package removed" \
    1 "$signed" "cleanup failed: putting the keychain search list back; deleting the temporary keychain; the keychain search list is not as it was;" \
    "${signing[@]}" STANDIN_RESTORE=fails STANDIN_DELETE=fails
  sign_case "putting the list back says it worked, but leaves another list: caught, the package removed" \
    1 "$signed" "cleanup failed: the keychain search list is not as it was;" "${signing[@]}" STANDIN_RESTORE=wrong
  list_may_change=no
  initial_list=empty
  sign_case "an empty search list to begin with: signed, and left empty" \
    0 "$signed" "signed, not yet notarized" "${signing[@]}"
  initial_list=two
  sign_case "an earlier failure, then a failing cleanup: both said, cleanup finished" \
    1 "pkgutil --expand-full,productsign" "2. productsign failed|cleanup failed: deleting the temporary keychain;" \
    "${signing[@]}" STANDIN_PRODUCTSIGN=fails STANDIN_DELETE=fails
  sign_case "interrupted while signing, and deleting fails: exit 130 kept, cleanup finished" \
    130 "pkgutil --expand-full,productsign" "cleanup failed: deleting the temporary keychain;" \
    "${signing[@]}" STANDIN_PRODUCTSIGN=interrupted STANDIN_DELETE=fails

  # A signed package and its state, made by sign, for finish to work on.
  rm -rf "$T/made"
  mkdir -p "$T/made/temp"
  printf '%s\n' /Users/runner/Library/Keychains/login.keychain-db >"$T/made/list"
  env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" HOME="$T/made" RUNNER_TEMP="$T/made/temp" \
    STANDIN_CALLS="$T/made/calls" STANDIN_LIST="$T/made/list" "${signing[@]}" \
    bash "$0" sign "$d/unsigned.pkg" "$T/made/signed.pkg" "$T/made/state" >/dev/null 2>&1 || { echo "the setup sign failed" >&2; return 1; }

  # record DIR OUTCOME [FILE [SHA256]]: a notarization record as
  # ci/notarize.py writes it, for the signed package unless told otherwise.
  record() {
    local dir="$1" outcome="$2" file="${3:-signed.pkg}" sum="${4:-}"
    [ -n "$sum" ] || sum="$(shasum -a 256 "$T/made/signed.pkg" | cut -d' ' -f1)"
    mkdir -p "$dir"
    printf '{"id": "11111111-2222-4333-8444-555555555555", "file": "%s", "sha256": "%s", "submitted_at": "2026-10-12T03:36:57Z"}\n' \
      "$file" "$sum" >"$dir/submission.json"
    printf '{"id": "11111111-2222-4333-8444-555555555555", "outcome": "%s"}\n' "$outcome" >"$dir/result.json"
  }

  # finish_case LABEL WANT-STATUS WANT-CALLS WANT-TEXT RECORD-SETUP
  # [NAME=VALUE...]: finish, on a copy of the signed package, with the
  # record RECORD-SETUP makes (a command given the record's directory).
  # bundle_ok DIR: the diagnostics finish keeps, exactly the allowlist: the
  # package with its SHA-256 matching it, stapler's recorded commands, exit
  # statuses and output, pkgutil's and Gatekeeper's output, and the trust
  # settings.
  bundle_ok() {
    [ "$(cd "$1" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = \
      "gatekeeper.txt pkgutil-check-signature.txt signed.pkg signed.pkg.sha256 stapler.txt trust-settings.txt " ] &&
      (cd "$1" && shasum -a 256 -c signed.pkg.sha256 >/dev/null 2>&1) &&
      grep -q '^\$ xcrun stapler staple ' "$1/stapler.txt" && grep -qE '^\[exit [0-9]+\]$' "$1/stapler.txt" &&
      grep -q 'Processing: ' "$1/stapler.txt" && [ -s "$1/pkgutil-check-signature.txt" ] && [ -s "$1/gatekeeper.txt" ] &&
      grep -q 'No Trust Settings were found' "$1/trust-settings.txt"
  }

  # finish_case LABEL WANT-STATUS WANT-CALLS WANT-TEXT SETUP [NAME=VALUE...]:
  # finish exits WANT-STATUS, calls exactly WANT-CALLS, says WANT-TEXT, never
  # prints a secret, never notarizes, leaves nothing in RUNNER_TEMP, keeps the
  # package only on success, and keeps the diagnostic bundle exactly when it
  # failed once stapling had begun. diag_path is where it is asked to keep it.
  diag_path=''
  finish_case() {
    local label="$1" want="$2" calls="$3" text="$4" setup="$5" status out verdict=yes got diag
    shift 5
    rm -rf "$T/run"
    mkdir -p "$T/run/temp"
    : >"$T/run/calls"
    cp -p "$T/made/signed.pkg" "$T/run/signed.pkg"
    cp -Rp "$T/made/state" "$T/run/state"
    $setup "$T/run/record"
    diag="${diag_path:-$T/run/diag}"
    out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" HOME="$T/run" RUNNER_TEMP="$T/run/temp" \
      STANDIN_CALLS="$T/run/calls" STANDIN_ORIGIN="$identity" SYNDEO_EXPECT_SIGNED=yes \
      "MACOS_INSTALLER_SIGNING_IDENTITY=$identity" "$@" \
      bash "$0" finish "$T/run/signed.pkg" "$T/run/state" "$T/run/record" "$diag" 2>&1)" && status=0 || status=$?
    got="$(paste -sd, - <"$T/run/calls")"
    [ "$status" = "$want" ] || verdict=no
    [ "$got" = "$calls" ] || verdict=no
    says "$out" "$text" || verdict=no
    if printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU'; then verdict=no; fi
    if grep -q notarytool "$T/run/calls"; then verdict=no; fi
    [ -z "$(ls -A "$T/run/temp")" ] || verdict=no
    if [ "$want" = 0 ]; then [ -f "$T/run/signed.pkg" ] || verdict=no; else [ ! -e "$T/run/signed.pkg" ] || verdict=no; fi
    if [ "$want" != 0 ] && [ -z "$diag_path" ] && grep -qx 'xcrun stapler staple' "$T/run/calls"; then
      bundle_ok "$diag" || verdict=no
    elif [ -z "$diag_path" ]; then
      [ ! -e "$diag" ] || verdict=no
    fi
    verdict_of "$label" "$verdict" "$got" "$out"
  }
  accepted() { record "$1" accepted; }
  none() { :; }
  unresolved() { record "$1" unresolved-deadline; }
  invalid() { record "$1" invalid; }
  other_file() { record "$1" accepted other.pkg; }
  other_bytes() { record "$1" accepted signed.pkg 0000000000000000000000000000000000000000000000000000000000000000; }
  local finished="xcrun stapler staple,xcrun stapler validate,pkgutil --expand-full,spctl"
  local diagnosed="pkgutil --check-signature,spctl,security dump-trust-settings,security dump-trust-settings,security dump-trust-settings"
  local retried="xcrun stapler validate,sleep 15,xcrun stapler validate,sleep 30,xcrun stapler validate,sleep 60,xcrun stapler validate,sleep 120,xcrun stapler validate"

  printf '\n  finish: only after Apple accepted exactly this file\n'
  finish_case "accepted: staple, validate, compare, Gatekeeper; stapler's commands, output and exit statuses shown" \
    0 "$finished" "4. Apple accepted submission|    \$ xcrun stapler staple |The staple and validate action worked!|    \$ xcrun stapler validate |The validate action worked!|[exit 0]|signed, notarized and stapled" accepted
  finish_case "no notarization record: refused, nothing stapled, the package removed" 1 "" "refusing to staple: no notarization record" none
  finish_case "a submission still unresolved at its deadline: refused, nothing stapled" 1 "" "was not accepted: unresolved-deadline" unresolved
  finish_case "an Invalid submission: refused, nothing stapled" 1 "" "was not accepted: invalid" invalid
  finish_case "Apple accepted another file: refused" 1 "" "not this signed.pkg" other_file
  finish_case "Apple accepted other bytes under this name: refused" 1 "" "not this signed.pkg" other_bytes
  finish_case "SYNDEO_EXPECT_SIGNED=no: refused" 1 "" "SYNDEO_EXPECT_SIGNED is not yes" accepted SYNDEO_EXPECT_SIGNED=no
  printf '\n  finish: stapled once, validation retried, nothing resubmitted\n'
  finish_case "validation failing once, then passing: accepted after 15 s, the ticket stapled once" \
    0 "xcrun stapler staple,xcrun stapler validate,sleep 15,xcrun stapler validate,pkgutil --expand-full,spctl" \
    "does not have a ticket stapled to it|[exit 65]|validating again in 15s (attempt 2 of 5)|validation attempt 2|signed, notarized and stapled" \
    accepted STANDIN_VALIDATE_FAILS=1
  finish_case "validation failing four times, passing the fifth: accepted, after 15, 30, 60 and 120 s" \
    0 "xcrun stapler staple,$retried,pkgutil --expand-full,spctl" "validation attempt 5|signed, notarized and stapled" \
    accepted STANDIN_VALIDATE_FAILS=4
  finish_case "validation failing every time: five attempts, then it fails closed; nothing compared or inspected; the diagnostics kept" \
    1 "xcrun stapler staple,$retried,$diagnosed" "does not validate, after 5 attempts|kept the diagnostics in" \
    accepted STANDIN_VALIDATE_FAILS=all
  finish_case "stapling failing: fails closed, nothing validated, the diagnostics kept" \
    1 "xcrun stapler staple,$diagnosed" "The staple and validate action failed! Error 65.|[exit 65]|5. could not staple the ticket" \
    accepted STANDIN_STAPLE_FAILS=yes
  finish_case "stapling that changes the contents: caught, the diagnostics kept" \
    1 "xcrun stapler staple,xcrun stapler validate,pkgutil --expand-full,$diagnosed" "6.stapled: the package's contents differ" accepted \
    STANDIN_STAPLE="$d/payload.pkg"
  finish_case "Gatekeeper accepts it, but as Developer ID, not notarized: fails, the diagnostics kept" \
    1 "$finished,$diagnosed" "does not accept it as Notarized Developer ID" accepted "STANDIN_SOURCE=Developer ID"
  finish_case "Gatekeeper accepts it from another identity: fails, the diagnostics kept" \
    1 "$finished,$diagnosed" "does not accept it as Notarized Developer ID" accepted "STANDIN_ORIGIN=Developer ID Installer: Someone Else (ZZ99ZZ99ZZ)"
  finish_case "Gatekeeper rejects it: fails, the diagnostics kept" 1 "$finished,$diagnosed" "does not accept it as Notarized Developer ID" accepted STANDIN_SPCTL=rejected
  mkdir -p "$T/readonly" && chmod 555 "$T/readonly"
  diag_path="$T/readonly/diag"
  finish_case "diagnostics that cannot be written: it still fails, with the first failure's status, and the package is still removed" \
    1 "xcrun stapler staple,$retried" "does not validate, after 5 attempts|could not keep all of the diagnostics" \
    accepted STANDIN_VALIDATE_FAILS=all
  mkdir -p "$T/existing/diag"
  diag_path="$T/existing/diag"
  finish_case "a diagnostics directory that already exists: refused before anything is stapled" \
    1 "" "already exists, and diagnostics are only ever written fresh" accepted
  chmod 755 "$T/readonly"
  diag_path=

  # A bundle, zipped as build-macos hands it on, for staple-app.
  mkdir -p "$T/bundle/Syndeo.app/Contents/MacOS" "$T/bundle/Syndeo.app/Contents/_CodeSignature"
  printf 'plist' >"$T/bundle/Syndeo.app/Contents/Info.plist"
  printf 'app' >"$T/bundle/Syndeo.app/Contents/MacOS/Syndeo"
  printf 'seal' >"$T/bundle/Syndeo.app/Contents/_CodeSignature/CodeResources"
  chmod 755 "$T/bundle/Syndeo.app/Contents/MacOS/Syndeo"
  ditto -c -k --keepParent "$T/bundle/Syndeo.app" "$T/Syndeo.app.zip"
  local app_origin="Developer ID Application: SECRETVALUE Org ($team)"

  # app_case LABEL WANT-STATUS WANT-CALLS WANT-TEXT [NAME=VALUE...]:
  # staple-app exits WANT-STATUS, calls exactly WANT-CALLS, says WANT-TEXT,
  # never prints a secret, never notarizes, leaves nothing in RUNNER_TEMP,
  # leaves the stapled bundle only on success, and keeps the diagnostics
  # exactly when it failed once stapling had begun.
  app_case() {
    local label="$1" want="$2" calls="$3" text="$4" status out verdict=yes got
    shift 4
    rm -rf "$T/run"
    mkdir -p "$T/run/temp"
    : >"$T/run/calls"
    out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" HOME="$T/run" RUNNER_TEMP="$T/run/temp" \
      STANDIN_CALLS="$T/run/calls" STANDIN_ORIGIN="$app_origin" SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID="$team" "$@" \
      bash "$0" staple-app "$T/Syndeo.app.zip" "$T/run/app" "$T/run/diag" 2>&1)" && status=0 || status=$?
    got="$(paste -sd, - <"$T/run/calls")"
    [ "$status" = "$want" ] || verdict=no
    [ "$got" = "$calls" ] || verdict=no
    says "$out" "$text" || verdict=no
    if printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU'; then verdict=no; fi
    if grep -rqE 'SECRETVALUE|U0VDUkVU' "$T/run/diag" 2>/dev/null; then verdict=no; fi
    if grep -q notarytool "$T/run/calls"; then verdict=no; fi
    [ -z "$(ls -A "$T/run/temp")" ] || verdict=no
    if [ "$want" = 0 ]; then
      [ -f "$T/run/app/Syndeo.app/Contents/CodeResources" ] && [ ! -e "$T/run/diag" ] || verdict=no
    else
      [ ! -e "$T/run/app" ] || verdict=no
      if grep -qx 'xcrun stapler staple' "$T/run/calls"; then
        [ "$(cd "$T/run/diag" 2>/dev/null && ls -A | LC_ALL=C sort | tr '\n' ' ')" = \
          "Syndeo.app.zip Syndeo.app.zip.sha256 codesign.txt gatekeeper.txt stapler.txt trust-settings.txt " ] || verdict=no
      else
        [ ! -e "$T/run/diag" ] || verdict=no
      fi
    fi
    verdict_of "$label" "$verdict" "$got" "$out"
  }
  local stapled="xcrun stapler staple,xcrun stapler validate,codesign --verify,spctl"
  local app_diagnosed="codesign -dv,spctl,security dump-trust-settings,security dump-trust-settings,security dump-trust-settings"

  printf '\n  staple-app: the bundle Apple accepted, stapled, and nothing else changed\n'
  app_case "accepted with the binaries: unpack, staple, validate, compare, verify, Gatekeeper" \
    0 "$stapled" "A1. unpacked Syndeo.app|A2. the ticket is stapled|A3. the stapled bundle is the one Apple accepted|A4. its code signature verifies|A5. Gatekeeper accepts it to run|stapled:"
  app_case "SYNDEO_EXPECT_SIGNED=no: refused before anything is unpacked" 1 "" "SYNDEO_EXPECT_SIGNED is not yes" SYNDEO_EXPECT_SIGNED=no
  app_case "validation failing once, then passing: stapled once, validated twice" \
    0 "xcrun stapler staple,xcrun stapler validate,sleep 15,xcrun stapler validate,codesign --verify,spctl" "validation attempt 2" STANDIN_VALIDATE_FAILS=1
  app_case "validation failing every time: fails closed after five, the diagnostics kept, the bundle removed" \
    1 "xcrun stapler staple,$retried,$app_diagnosed" "does not validate, after 5 attempts|kept the diagnostics in" STANDIN_VALIDATE_FAILS=all
  app_case "stapling failing: fails closed, the diagnostics kept" \
    1 "xcrun stapler staple,$app_diagnosed" "A2. could not staple the ticket" STANDIN_STAPLE_FAILS=yes
  app_case "stapling that adds no ticket: caught" \
    1 "xcrun stapler staple,xcrun stapler validate,$app_diagnosed" "A3. stapling changed the bundle other than by adding its ticket" STANDIN_APP_STAPLE=none
  app_case "stapling that changes another file: caught" \
    1 "xcrun stapler staple,xcrun stapler validate,$app_diagnosed" "A3. stapling changed the bundle other than by adding its ticket" STANDIN_APP_STAPLE=changes
  app_case "a signature that does not verify: fails, the diagnostics kept" \
    1 "xcrun stapler staple,xcrun stapler validate,codesign --verify,$app_diagnosed" "A4. its code signature does not verify" STANDIN_UNVERIFIED=yes
  app_case "Gatekeeper: Developer ID, not notarized: fails, the identity not shown" \
    1 "$stapled,$app_diagnosed" "A5. Gatekeeper does not accept it|origin=<an identity, not shown>" "STANDIN_SOURCE=Developer ID"
  app_case "Gatekeeper: another team's Application identity: fails" \
    1 "$stapled,$app_diagnosed" "A5. Gatekeeper does not accept it" "STANDIN_ORIGIN=Developer ID Application: SECRETVALUE Org (ZZ99ZZ99ZZ)"
  app_case "Gatekeeper: an Installer identity of the team: fails" \
    1 "$stapled,$app_diagnosed" "A5. Gatekeeper does not accept it" "STANDIN_ORIGIN=Developer ID Installer: SECRETVALUE Org ($team)"
  app_case "Gatekeeper rejects it: fails" 1 "$stapled,$app_diagnosed" "A5. Gatekeeper does not accept it" STANDIN_SPCTL=rejected
  mkdir -p "$T/run2"
  out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" RUNNER_TEMP="$T/run2" STANDIN_CALLS="$T/run2/calls" \
    SYNDEO_EXPECT_SIGNED=yes MACOS_TEAM_ID="$team" bash "$0" staple-app "$T/Syndeo.app.zip" "$T/run2" "$T/run2/diag" 2>&1)" && status=0 || status=$?
  cases=$((cases + 1))
  if [ "$status" = 1 ] && printf '%s' "$out" | grep -qF "$T/run2 exists" && [ -d "$T/run2" ] && [ ! -e "$T/run2/calls" ]; then
    printf '  ok    %s\n' "an output directory that exists: refused, and left alone"
  else
    printf '  WRONG %s (exit %s)\n' "an output directory that exists: refused, and left alone" "$status"
    wrong=$((wrong + 1))
  fi

  local out status mode
  for mode in sign finish staple-app; do
    out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" "${signing[@]}" STANDIN_CALLS="$T/run/calls" STANDIN_LIST="$T/run/list" \
      bash -x "$0" "$mode" "$d/unsigned.pkg" "$T/run/x.pkg" "$T/run/x" 2>&1)" && status=0 || status=$?
    cases=$((cases + 1))
    if [ "$status" = 1 ] && printf '%s' "$out" | grep -qF "tracing is on" && ! printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU'; then
      printf '  ok    %s\n' "$mode, traced: refused, before any value is read"
    else
      printf '  WRONG %s (exit %s)\n' "$mode, traced: refused, before any value is read" "$status"
      wrong=$((wrong + 1))
    fi
  done
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test ;;
  staple-app) shift; staple_app "$@" ;;
  sign) shift; sign "$@" ;;
  finish) shift; finish "$@" ;;
  *) die "usage: $0 staple-app <Syndeo.app.zip> <out-dir> <diagnostics-dir> | sign <unsigned.pkg> <signed.pkg> <state-dir> | finish <signed.pkg> <state-dir> <notarization-record> <diagnostics-dir> | --self-test" ;;
esac
