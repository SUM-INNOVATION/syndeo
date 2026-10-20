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
#  5. staple it, in up to five attempts, after 0, 15, 30, 60 and 120
#     seconds. Stapling fetches the ticket from Apple over the network, and
#     a connection lost on the way fails it; so:
#     - the package Apple accepted is kept, read only, in the working
#       directory, its SHA-256 the record's, and <signed.pkg> is removed:
#       there is no <signed.pkg> again until an attempt succeeds;
#     - each attempt makes a fresh copy of the kept package, and requires
#       both to be the accepted bytes before stapler sees the copy; if either
#       is not, finish stops there;
#     - each attempt staples its copy, then validates it. A copy is never
#       stapled twice: one that stapler changed, whether it said it worked
#       or not, and that did not validate, is set aside, never used again;
#     - only a copy stapled and validated becomes <signed.pkg>: copied beside
#       it, checked, then renamed into place;
#     - five failed attempts are fatal, and leave no <signed.pkg>.
#     Nothing is ever submitted again. Each attempt is shown in the log as it
#     happens, as one line saying what it did and how it ended, with each
#     stapler command, its exit status and its complete output, and all of
#     it is kept;
#  6. expand it once more, and require it identical to the unsigned package;
#  7. require Gatekeeper to accept it as Notarized Developer ID, from the
#     Installer identity.
# If finish fails once stapling has begun, it keeps, in <diagnostics-dir>
# (which must not exist yet), these and nothing else:
# - the package Apple accepted, not stapled, as <name>.accepted;
# - the last copy stapler changed, if any, as <name>.last-candidate;
# - the stapled <signed.pkg>, if steps 6 or 7 failed, as <name>.stapled;
# - their SHA-256s, in <name>.sha256;
# - attempts.txt, one line per attempt;
# - stapler's output;
# - pkgutil --check-signature and Gatekeeper's assessment of each package
#   kept;
# - the user, admin and system trust settings, read only.
# None of the packages kept is named .pkg: none is a release candidate. It
# never keeps a keychain, a certificate, a key, a password or the
# environment.
#
# In either mode, on any failure <signed.pkg> is removed, so nothing half-done
# can be shipped, and the exit status is the first failure's. finish's
# cleanup removes its working directory, and failing to is a failure too,
# even of a package otherwise stapled.
#
# staple-app, after notarize-binaries' one submission, which carried
# Syndeo.app with the seven executables, was accepted, and release.yml has
# checked <Syndeo.app.zip> is the bundle that submission carried, by its
# SHA-256:
#  A1. keep the bundle Apple accepted: its ZIP, copied read only into the
#      working directory, and unpacked once as the reference. Every path in
#      it is recorded with its bytes, mode and owner. It must hold no ticket
#      yet, and its code signature must verify, strictly, with everything
#      nested in it. <out-dir>, which must not exist yet, is made empty;
#  A2. staple it, in up to five attempts, after 0, 15, 30, 60 and 120
#      seconds, on the terms of step 5 of finish:
#      - each attempt unpacks a fresh copy from the ZIP kept. Before stapler
#        sees the copy, the ZIP must still be the accepted bytes; the copy
#        must be the reference, path for path, byte for byte, mode for mode;
#        and its signature must verify. If any of this fails, staple-app
#        stops there;
#      - a copy is never stapled twice. One stapler changed but that did not
#        validate is set aside, and one it left unchanged is discarded;
#      - only a copy stapled and validated becomes <out-dir>/Syndeo.app:
#        copied into <out-dir> under another name, checked, then renamed;
#      - five failed attempts are fatal, and leave <out-dir> removed;
#  A3. require the stapled bundle identical to the reference, but for one
#      file stapler adds, Contents/CodeResources, the ticket;
#  A4. require its code signature to verify, strictly, with everything nested
#      in it;
#  A5. require Gatekeeper to accept it to run, as Notarized Developer ID, from
#      a Developer ID Application identity of MACOS_TEAM_ID.
# If it fails once A1 has kept the bundle, it keeps, in <diagnostics-dir>,
# these and nothing else:
# - the ZIP Apple accepted, as Syndeo.app.accepted.zip;
# - the last copy stapler changed, if any, as Syndeo.app.last-candidate.zip;
# - the stapled bundle, if A3 to A5 failed, as Syndeo.app.stapled.zip;
# - their SHA-256s;
# - attempts.txt and stapler's output;
# - codesign's and Gatekeeper's view of each bundle kept, with the
#   identity's name left out;
# - the trust settings.
# It removes <out-dir>, so no package is built from it. Its cleanup, like
# finish's, removes its working directory, and failing to is fatal.
#
# Every stapler command, in finish and in staple-app, is stopped if it runs
# past 60 seconds, which then counts as that attempt failing (exit 124).
# SYNDEO_STAPLER_LIMIT_SECONDS may make that shorter, 1 to 60, and nothing
# makes it longer. So stapling, in either mode, takes at most 225 seconds of
# waiting and ten stapler commands of 61 seconds each: under 15 minutes,
# with the copies and checks. The package job runs both, one before its
# submission and one after Apple's verdict; release.yml says how that fits
# its time.
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

# stapler_limit: how long a stapler command may run, in seconds: 60, or
# SYNDEO_STAPLER_LIMIT_SECONDS if it is a whole number from 1 to 60. Anything
# else is refused, so nothing makes the limit longer.
stapler_limit() {
  local limit="${SYNDEO_STAPLER_LIMIT_SECONDS:-60}"
  [[ "$limit" =~ ^[1-9][0-9]?$ ]] && [ "$limit" -le 60 ] ||
    die "refusing: SYNDEO_STAPLER_LIMIT_SECONDS is not a whole number of seconds from 1 to 60"
  printf '%s' "$limit"
}

# recorded LOG COMMAND...: run COMMAND, stopped if it runs past $limit
# seconds, and show its exact command line, its complete output (stdout and
# stderr together) and its exit status, 124 if it was stopped, in the log and
# appended to LOG. Returns that status. Only for stapler, whose command line
# and output name nothing but what it staples. While it runs, its process is
# $child, for cleanup to stop.
recorded() {
  local log="$1" status=0 output block tenths=0
  shift
  "$@" >"$work/command.out" 2>&1 &
  child=$!
  while kill -0 "$child" 2>/dev/null; do
    if [ "$tenths" -ge $((limit * 10)) ]; then
      stop_child
      status=124
      printf 'stopped: still running after %s seconds\n' "$limit" >>"$work/command.out"
      break
    fi
    /bin/sleep 0.1
    tenths=$((tenths + 1))
  done
  if [ "$status" != 124 ]; then
    wait "$child" && status=0 || status=$?
  fi
  child=''
  output="$(cat "$work/command.out")"
  block="$(printf '$ %s\n%s[exit %s]' "$*" "${output:+$output$'\n'}" "$status")"
  printf '%s\n' "$block" >>"$log"
  printf '%s\n' "$block" | sed 's/^/    /'
  return "$status"
}

# stop_child: the stapler command recorded runs, if any, stopped: TERM, then
# KILL a second later, and reaped.
stop_child() {
  [ -n "${child:-}" ] || return 0
  if kill -0 "$child" 2>/dev/null; then
    kill -TERM "$child" 2>/dev/null
    /bin/sleep 1
    kill -KILL "$child" 2>/dev/null
  fi
  wait "$child" 2>/dev/null
  child=''
}

# discard STEP N PATH: attempt N's copy, at PATH, removed. If it cannot be,
# that is said, and left for cleanup, which then fails.
discard() {
  rm -rf "$3" 2>/dev/null || say "$1. attempt $2: its copy could not be removed; cleanup will fail for it"
}

# attempted STEP LINE: one attempt, what it did and how it ended, shown now
# under STEP and kept in attempts.txt.
attempted() {
  say "$1. $2"
  printf '%s\n' "$2" >>"$work/attempts.txt"
}

# sha FILE: FILE's SHA-256, or nothing, failing, if it cannot be read.
sha() { shasum -a 256 "$1" | cut -d' ' -f1; }

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
  accepted=''
  last=''
  child=''
  trap cleanup_finish EXIT
  trap 'exit 130' INT TERM HUP
  signed_yes
  limit="$(stapler_limit)"
  not_blank MACOS_INSTALLER_SIGNING_IDENTITY
  local identity="$MACOS_INSTALLER_SIGNING_IDENTITY"
  [ -f "$out" ] || die "no $out"
  [ ! -e "$diag" ] || die "refusing: $diag already exists, and diagnostics are only ever written fresh"
  [ -f "$state/unsigned.manifest" ] || die "no $state/unsigned.manifest: was the package signed by this script's sign mode?"

  # 4. Apple accepted exactly this file.
  local verdict id want
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
print(sub["id"], h)
PY
)" || die "4. refusing to staple: $verdict"
  id="${verdict% *}"
  want="${verdict##* }"
  say "4. Apple accepted submission $id, which is exactly this file"

  # 5. The package Apple accepted, kept read only; <signed.pkg> taken away
  # until a copy of it is stapled and validated.
  local name="${out##*/}" mode
  work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sign-macos-pkg.XXXXXX")"
  mkdir "$work/accepted" "$work/attempts" "$work/last" || die "5. could not make the working directories"
  : >"$work/stapler.txt"
  : >"$work/attempts.txt"
  mode="$(stat -f '%Lp' "$out")" || die "5. could not read $out's mode"
  accepted="$work/accepted/$name"
  cp "$out" "$accepted" && chmod 0444 "$accepted" || die "5. could not keep the package Apple accepted"
  [ "$(sha "$accepted")" = "$want" ] || die "5. the package kept is not the one Apple accepted"
  stapling=yes
  rm -f "$out"
  [ ! -e "$out" ] || die "5. could not take $out away while it is stapled"
  say "5. kept the package Apple accepted, read only ($want); $out is made again only from a copy stapled and validated"

  local delay attempt=0 candidate now got staple validate promoted=no line
  for delay in 0 15 30 60 120; do
    attempt=$((attempt + 1))
    if [ "$delay" != 0 ]; then
      say "5. attempt $attempt of 5 in ${delay}s"
      sleep "$delay"
    fi
    now="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    # A fresh copy, it and the package kept both the accepted bytes, before
    # stapler sees it.
    [ "$(sha "$accepted")" = "$want" ] ||
      die "5. attempt $attempt: the package kept is no longer the one Apple accepted; nothing more is stapled"
    mkdir "$work/attempts/$attempt" || die "5. attempt $attempt: could not make its directory"
    candidate="$work/attempts/$attempt/$name"
    cp "$accepted" "$candidate" && chmod 0600 "$candidate" || die "5. attempt $attempt: could not copy the package kept"
    got="$(sha "$candidate")" || die "5. attempt $attempt: could not read its copy"
    [ "$got" = "$want" ] || die "5. attempt $attempt: its copy is $got, not the package Apple accepted; nothing is stapled"
    line="attempt $attempt of 5, $now: a fresh copy of the package Apple accepted"
    staple=0
    recorded "$work/stapler.txt" xcrun stapler staple "$candidate" || staple=$?
    got="$(sha "$candidate")" || die "5. attempt $attempt: could not read its copy after stapler"
    if [ "$staple" != 0 ] && [ "$got" = "$want" ]; then
      attempted 5 "$line; stapler staple exit $staple, the copy unchanged: discarded"
      discard 5 "$attempt" "$candidate"
      continue
    elif [ "$staple" != 0 ]; then
      attempted 5 "$line; stapler staple exit $staple, and the copy changed, to $got: set aside, never stapled again"
      set_aside "$candidate"
      continue
    elif [ "$got" = "$want" ]; then
      attempted 5 "$line; stapler staple exit 0, but the copy is unchanged, with no ticket: discarded"
      discard 5 "$attempt" "$candidate"
      continue
    fi
    validate=0
    recorded "$work/stapler.txt" xcrun stapler validate "$candidate" || validate=$?
    if [ "$validate" != 0 ]; then
      attempted 5 "$line; stapled, $got; stapler validate exit $validate: set aside, never stapled again"
      set_aside "$candidate"
      continue
    fi
    attempted 5 "$line; stapled, $got; validated"
    promote "$candidate" "$got" "$mode"
    promoted=yes
    break
  done
  [ "$promoted" = yes ] || die "5. no attempt stapled a ticket that validates, after $attempt attempts; there is no $out"
  say "5. the ticket is stapled and validates (attempt $attempt of 5): $out ($got)"

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

# set_aside COPY: a copy stapler changed that did not validate, kept as the
# last such for the diagnostics, in place of any earlier one, and never used
# again.
set_aside() {
  rm -f "$work/last/$name"
  mv -f "$1" "$work/last/$name" || die "5. could not set aside $1"
  last="$work/last/$name"
}

# promote COPY SHA256 MODE: COPY, stapled and validated, becomes <signed.pkg>:
# copied beside it with MODE, checked, then renamed into place, so there is
# never a <signed.pkg> other than exactly COPY.
promote() {
  local staged="$out.promoting"
  cp "$1" "$staged" && chmod "$3" "$staged" || die "5. could not copy the stapled package beside $out"
  [ "$(sha "$staged")" = "$2" ] || die "5. the copy beside $out is not the stapled package"
  mv -f "$staged" "$out" || die "5. could not rename the stapled package into place"
  [ "$(sha "$out")" = "$2" ] || die "5. $out is not the stapled package"
}

# keep_diagnostics: what finish keeps once stapling has begun and it fails,
# in $diag, and nothing else; see the top of this file. Run before anything
# is removed, and never changing the exit status. No package is kept under a
# name ending .pkg: none of them is a release candidate.
keep_diagnostics() {
  local name="${out##*/}" failed=0 kind file sums=''
  mkdir -p "$diag" || return 1
  for kind in accepted last-candidate stapled; do
    case "$kind" in
      accepted) file="$accepted" ;;
      last-candidate) file="$last" ;;
      stapled) file="$out" ;;
    esac
    [ -n "$file" ] && [ -f "$file" ] || continue
    cp -p "$file" "$diag/$name.$kind" || failed=1
    sums="$sums$(cd "$diag" && shasum -a 256 "$name.$kind")"$'\n' || failed=1
    { printf '== %s\n' "$name.$kind"; pkgutil --check-signature "$file" 2>&1; } >>"$diag/pkgutil-check-signature.txt"
    { printf '== %s\n' "$name.$kind"; spctl -a -vv -t install "$file" 2>&1; } >>"$diag/gatekeeper.txt"
  done
  printf '%s' "$sums" >"$diag/$name.sha256" || failed=1
  cp "$work/stapler.txt" "$diag/stapler.txt" || failed=1
  cp "$work/attempts.txt" "$diag/attempts.txt" || failed=1
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

# cleanup_finish: the diagnostics, if stapling began and finish failed; then
# the working directory and anything beside <signed.pkg> removed, failing to
# being a failure, even of a package otherwise stapled; then, unless finish
# succeeded, <signed.pkg> removed. The first failure's status is kept.
cleanup_finish() {
  local status=$? failed=''
  set +e
  stop_child
  if [ "$succeeded" != yes ] && [ "$stapling" = yes ]; then
    keep_diagnostics || printf 'sign-macos-pkg: could not keep all of the diagnostics in %s\n' "$diag" >&2
  fi
  if [ -n "$work" ]; then
    rm -rf "$work" 2>/dev/null
    [ ! -e "$work" ] || failed="$failed removing the working directory;"
  fi
  rm -f "${out:?}.promoting" 2>/dev/null
  [ ! -e "$out.promoting" ] || failed="$failed removing $out.promoting;"
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

# ------------------------------------------------------------------ staple-app

staple_app() {
  case "$-" in *x*) die "refusing: tracing is on, and would print the secrets" ;; esac
  [ "$#" = 3 ] || die "usage: $0 staple-app <Syndeo.app.zip> <out-dir> <diagnostics-dir>"
  local zip="$1" team want
  out_dir="$2"
  diag="$3"
  umask 022
  work=''
  succeeded=no
  stapling=no
  made=no
  app=''
  accepted=''
  reference=''
  last=''
  child=''
  trap cleanup_staple_app EXIT
  trap 'exit 130' INT TERM HUP
  signed_yes
  limit="$(stapler_limit)"
  not_blank MACOS_TEAM_ID
  team="$MACOS_TEAM_ID"
  [[ "$team" =~ ^[ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789]{10}$ ]] || die "refusing: MACOS_TEAM_ID is not ten capital letters and digits"
  [ -f "$zip" ] || die "no $zip"
  [ ! -e "$out_dir" ] && [ ! -L "$out_dir" ] || die "$out_dir exists"
  [ ! -e "$diag" ] || die "refusing: $diag already exists, and diagnostics are only ever written fresh"

  # A1. The bundle Apple accepted, kept: its ZIP, read only, and unpacked
  # once as the reference every copy is held to.
  mkdir -p "$out_dir"
  made=yes
  work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sign-macos-pkg.XXXXXX")"
  mkdir "$work/accepted" "$work/reference" "$work/attempts" "$work/manifests" "$work/last" ||
    die "A1. could not make the working directories"
  : >"$work/stapler.txt"
  : >"$work/attempts.txt"
  want="$(sha "$zip")" || die "A1. could not read $zip"
  accepted="$work/accepted/Syndeo.app.zip"
  cp "$zip" "$accepted" && chmod 0444 "$accepted" || die "A1. could not keep $zip"
  [ "$(sha "$accepted")" = "$want" ] || die "A1. the ZIP kept is not $zip"
  ditto -x -k "$accepted" "$work/reference" || die "A1. could not unpack $zip"
  reference="$work/reference/Syndeo.app"
  [ "$(cd "$work/reference" && ls -A)" = Syndeo.app ] && [ -d "$reference" ] && [ ! -L "$reference" ] ||
    die "A1. $zip does not hold Syndeo.app alone"
  manifest "$reference" >"$work/manifests/accepted" || die "A1. could not record the bundle"
  [ ! -e "$reference/Contents/CodeResources" ] || die "A1. the bundle holds a ticket already"
  codesign --verify --strict --deep "$reference" >"$work/codesign-reference.txt" 2>&1 ||
    die "A1. the bundle's code signature does not verify as Apple accepted it"
  stapling=yes
  say "A1. kept Syndeo.app as Apple accepted it: its ZIP, read only ($want), $(wc -l <"$work/manifests/accepted" | tr -d ' ') paths, its signature verifying"

  # A2. Up to five attempts, each on a fresh copy.
  local delay attempt=0 candidate now staple validate promoted=no line
  for delay in 0 15 30 60 120; do
    attempt=$((attempt + 1))
    if [ "$delay" != 0 ]; then
      say "A2. attempt $attempt of 5 in ${delay}s"
      sleep "$delay"
    fi
    now="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    # A fresh copy from the ZIP kept, the ZIP still the accepted bytes, and
    # the copy the reference exactly, its signature verifying, before
    # stapler sees it.
    [ "$(sha "$accepted")" = "$want" ] ||
      die "A2. attempt $attempt: the ZIP kept is no longer the one Apple accepted; nothing more is stapled"
    mkdir "$work/attempts/$attempt" || die "A2. attempt $attempt: could not make its directory"
    ditto -x -k "$accepted" "$work/attempts/$attempt" || die "A2. attempt $attempt: could not unpack a fresh copy"
    candidate="$work/attempts/$attempt/Syndeo.app"
    manifest "$candidate" >"$work/manifests/$attempt" 2>/dev/null && cmp -s "$work/manifests/accepted" "$work/manifests/$attempt" ||
      die "A2. attempt $attempt: its copy is not the bundle Apple accepted, path for path; nothing is stapled"
    codesign --verify --strict --deep "$candidate" >"$work/codesign-$attempt.txt" 2>&1 ||
      die "A2. attempt $attempt: its copy's code signature does not verify; nothing is stapled"
    line="attempt $attempt of 5, $now: a fresh copy of the bundle Apple accepted, its signature verifying"
    staple=0
    recorded "$work/stapler.txt" xcrun stapler staple "$candidate" || staple=$?
    if [ "$staple" != 0 ] && unchanged "$candidate" "$attempt"; then
      attempted A2 "$line; stapler staple exit $staple, the copy unchanged: discarded"
      discard A2 "$attempt" "$work/attempts/$attempt"
      continue
    elif [ "$staple" != 0 ]; then
      attempted A2 "$line; stapler staple exit $staple, and the copy changed: set aside, never stapled again"
      set_aside_app "$candidate"
      continue
    elif unchanged "$candidate" "$attempt"; then
      attempted A2 "$line; stapler staple exit 0, but the copy is unchanged, with no ticket: discarded"
      discard A2 "$attempt" "$work/attempts/$attempt"
      continue
    fi
    validate=0
    recorded "$work/stapler.txt" xcrun stapler validate "$candidate" || validate=$?
    if [ "$validate" != 0 ]; then
      attempted A2 "$line; stapled; stapler validate exit $validate: set aside, never stapled again"
      set_aside_app "$candidate"
      continue
    fi
    attempted A2 "$line; stapled; validated"
    promote_app "$candidate"
    promoted=yes
    break
  done
  [ "$promoted" = yes ] || die "A2. no attempt stapled a ticket that validates, after $attempt attempts; there is no $out_dir/Syndeo.app"
  say "A2. the ticket is stapled and validates (attempt $attempt of 5): $app"

  manifest "$app" >"$work/manifests/stapled"
  if ! grep -vE '^\./Contents/CodeResources file ' "$work/manifests/stapled" | cmp -s - "$work/manifests/accepted" ||
    [ "$(grep -cE '^\./Contents/CodeResources file 644 ' "$work/manifests/stapled")" != 1 ]; then
    diff "$work/manifests/accepted" "$work/manifests/stapled" | grep '^[<>]' | head -10 >&2 || true
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

# unchanged COPY N: COPY, attempt N's, is still the reference exactly.
unchanged() {
  manifest "$1" >"$work/manifests/$2.after" 2>/dev/null && cmp -s "$work/manifests/accepted" "$work/manifests/$2.after"
}

# set_aside_app COPY: a copy stapler changed that did not validate, kept as
# the last such for the diagnostics, in place of any earlier one, and never
# used again.
set_aside_app() {
  rm -rf "$work/last/Syndeo.app"
  mv "$1" "$work/last/Syndeo.app" || die "A2. could not set aside $1"
  last="$work/last/Syndeo.app"
}

# promote_app COPY: COPY, stapled and validated, becomes <out-dir>/Syndeo.app:
# copied into <out-dir> under another name, checked path for path, then
# renamed, so there is never a <out-dir>/Syndeo.app other than exactly COPY.
promote_app() {
  local staged="$out_dir/.Syndeo.app.promoting"
  manifest "$1" >"$work/manifests/candidate" || die "A2. could not record the stapled bundle"
  ditto "$1" "$staged" || die "A2. could not copy the stapled bundle into $out_dir"
  manifest "$staged" >"$work/manifests/staged" 2>/dev/null && cmp -s "$work/manifests/candidate" "$work/manifests/staged" ||
    die "A2. the copy in $out_dir is not the stapled bundle"
  mv "$staged" "$out_dir/Syndeo.app" || die "A2. could not rename the stapled bundle into place"
  app="$out_dir/Syndeo.app"
}

# keep_app_diagnostics: what staple-app keeps once A1 has kept the bundle
# and it fails, in $diag, and nothing else; see the top of this file. Never
# changing the exit status.
keep_app_diagnostics() {
  local failed=0 kind bundle sums=''
  mkdir -p "$diag" || return 1
  cp -p "$accepted" "$diag/Syndeo.app.accepted.zip" || failed=1
  sums="$(cd "$diag" && shasum -a 256 Syndeo.app.accepted.zip)"$'\n' || failed=1
  for kind in accepted last-candidate stapled; do
    case "$kind" in
      accepted) bundle="$reference" ;;
      last-candidate) bundle="$last" ;;
      stapled) bundle="$app" ;;
    esac
    [ -n "$bundle" ] && [ -d "$bundle" ] || continue
    if [ "$kind" != accepted ]; then
      ditto -c -k --keepParent "$bundle" "$diag/Syndeo.app.$kind.zip" || failed=1
      sums="$sums$(cd "$diag" && shasum -a 256 "Syndeo.app.$kind.zip")"$'\n' || failed=1
    fi
    { printf '== %s\n' "$kind"
      codesign -dv --verbose=4 "$bundle" 2>&1 | sed 's/^Authority=Developer ID Application: .*/Authority=<the Application identity, not shown>/'
    } >>"$diag/codesign.txt"
    { printf '== %s\n' "$kind"
      spctl -a -vv -t execute "$bundle" 2>&1 | sed 's/^origin=.*/origin=<an identity, not shown>/'
    } >>"$diag/gatekeeper.txt"
  done
  printf '%s' "$sums" >"$diag/Syndeo.app.sha256" || failed=1
  cp "$work/stapler.txt" "$diag/stapler.txt" || failed=1
  cp "$work/attempts.txt" "$diag/attempts.txt" || failed=1
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

# cleanup_staple_app: any stapler command still running stopped; the
# diagnostics, if A1 kept the bundle and staple-app failed; then the working
# directory removed, failing to being a failure, even of a bundle otherwise
# stapled; then, unless it succeeded, <out-dir> removed, if this made it.
# The first failure's status is kept.
cleanup_staple_app() {
  local status=$? failed=''
  set +e
  stop_child
  if [ "$succeeded" != yes ] && [ "$stapling" = yes ]; then
    keep_app_diagnostics || printf 'sign-macos-pkg: could not keep all of the diagnostics in %s\n' "$diag" >&2
  fi
  if [ -n "$work" ]; then
    rm -rf "$work" 2>/dev/null
    [ ! -e "$work" ] || failed="$failed removing the working directory;"
  fi
  if [ -n "$failed" ]; then
    printf 'sign-macos-pkg: cleanup failed:%s\n' "$failed" >&2
    succeeded=no
    [ "$status" != 0 ] || status=1
  fi
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
# at PATH: " @N", N the attempt whose copy PATH is, by its directory.
at() { printf ' @%s' "$(basename "$(dirname "$1")")"; }
# lost: stapler's failure when the connection to Apple is lost, as a release
# run saw it.
lost() {
  echo 'error is Error Domain=NSURLErrorDomain Code=-1005 "The network connection was lost." (stand-in)'
  echo "CloudKit's response is inconsistent with expections: (null)"
  echo "The staple and validate action failed! Error 68."
  exit 68
}
# lock DIR: a file in DIR that cannot be removed, until the self-test lets it.
lock() { : >"$1/locked"; chflags uchg "$1/locked"; }
# ticket PATH: a ticket stapled to PATH, as stapler adds one: appended to a
# package; to a bundle, Contents/CodeResources, or none, or another change
# besides, as STANDIN_APP_STAPLE says.
ticket() {
  case "$1" in
    *.app)
      [ "${STANDIN_APP_STAPLE:-ticket}" = none ] || { printf 'ticket' >"$1/Contents/CodeResources"; chmod 644 "$1/Contents/CodeResources"; }
      [ "${STANDIN_APP_STAPLE:-ticket}" != changes ] || printf 'changed' >>"$1/Contents/Info.plist" ;;
    *) printf 'ticket\n' >>"$1" ;;
  esac
}
case "$1 $2" in
  "stapler staple")
    echo "xcrun stapler staple$(at "$3")" >>"$STANDIN_CALLS"
    echo "$$" >"$STANDIN_CALLS.stapler-pid"
    echo "Processing: $3"
    if [ "${STANDIN_STAPLE_FAILS:-no}" = yes ]; then echo "The staple and validate action failed! Error 65."; exit 65; fi
    # STANDIN_STAPLE: a package put in its place, as if stapling had changed
    # its contents. Otherwise STANDIN_STAPLES: each attempt's outcome,
    # comma-separated, the last one repeated: ok; lost; partial (lost, the
    # copy changed); noticket (it says it worked, and changes nothing);
    # tamper (lost, and what was kept of what Apple accepted changed);
    # interrupt (the script sent TERM, stapler still running); hang (never
    # finishing by itself); lock (ok, and a file left that cannot be
    # removed); locklost.
    if [ -n "${STANDIN_STAPLE:-}" ]; then
      cp "$STANDIN_STAPLE" "$3"
    else
      n=$(( $(cat "$STANDIN_CALLS.staples" 2>/dev/null || echo 0) + 1 ))
      echo "$n" >"$STANDIN_CALLS.staples"
      IFS=, read -ra outcomes <<<"${STANDIN_STAPLES:-ok}"
      o="${outcomes[$((n - 1))]:-${outcomes[${#outcomes[@]} - 1]}}"
      work="$(dirname "$(dirname "$(dirname "$3")")")"
      case "$3" in *.app) kept="$work/accepted/Syndeo.app.zip" ;; *) kept="$work/accepted/$(basename "$3")" ;; esac
      case "$o" in
        ok) ticket "$3" ;;
        lost) lost ;;
        partial) case "$3" in *.app) printf 'half a ticket' >"$3/Contents/CodeResources" ;; *) printf 'half a ticket' >>"$3" ;; esac; lost ;;
        noticket) ;;
        tamper) chmod u+w "$kept"; printf 'x' >>"$kept"; lost ;;
        interrupt) "$(dirname "$0")/interrupt-finish"; exec /bin/sleep 30 ;;
        hang) exec /bin/sleep 30 ;;
        lock) ticket "$3"; lock "$(dirname "$3")" ;;
        locklost) lock "$(dirname "$3")"; lost ;;
      esac
    fi
    echo "The staple and validate action worked!" ;;
  "stapler validate")
    echo "xcrun stapler validate$(at "$3")" >>"$STANDIN_CALLS"
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
  # Anything else, notarytool above all, is logged, and fails.
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
  # codesign: a signature that verifies, unless the path matches the
  # pattern STANDIN_UNVERIFIED.
  cat >"$bin/codesign" <<'EOF'
#!/bin/bash
echo "codesign $1" >>"$STANDIN_CALLS"
case "$1" in
  # shellcheck disable=SC2254 # the pattern is a pattern
  --verify) case "${*: -1}" in ${STANDIN_UNVERIFIED:-/nothing}) exit 1 ;; esac ;;
  -dv) echo "Authority=Developer ID Application: SECRETVALUE Org (AB12CD34EF)" >&2 ;;
esac
exit 0
EOF
  # ditto, as it is; but a bundle it makes at a path matching the pattern
  # STANDIN_DITTO_ALTERS comes out with one byte of Info.plist different;
  # one matching STANDIN_DITTO_MODE, with a file's mode different; one
  # matching STANDIN_DITTO_INTERRUPTS has the script sent TERM.
  cat >"$bin/ditto" <<'EOF'
#!/bin/bash
/usr/bin/ditto "$@" || exit
dest="${*: -1}"
bundle="$dest"
[ ! -d "$dest/Syndeo.app" ] || bundle="$dest/Syndeo.app"
# shellcheck disable=SC2254 # the patterns are patterns
case "$dest" in ${STANDIN_DITTO_ALTERS:-/nothing}) printf 'x' >>"$bundle/Contents/Info.plist" ;; esac
# shellcheck disable=SC2254
case "$dest" in ${STANDIN_DITTO_MODE:-/nothing}) chmod 600 "$bundle/Contents/Info.plist" ;; esac
# shellcheck disable=SC2254
case "$dest" in ${STANDIN_DITTO_INTERRUPTS:-/nothing}) "$(dirname "$0")/interrupt-finish"; /bin/sleep 1 ;; esac
exit 0
EOF
  cat >"$bin/sleep" <<'EOF'
#!/bin/bash
echo "sleep $*" >>"$STANDIN_CALLS"
# STANDIN_SLEEP_INTERRUPT=yes: finish is sent TERM while it waits.
[ "${STANDIN_SLEEP_INTERRUPT:-no}" != yes ] || { "$(dirname "$0")/interrupt-finish"; /bin/sleep 1; }
EOF
  # cp, as it is; but a copy made to a path matching the pattern
  # STANDIN_COPY_ALTERS comes out one byte different from what it copied,
  # and one to a path matching STANDIN_COPY_INTERRUPTS has finish sent TERM.
  cat >"$bin/cp" <<'EOF'
#!/bin/bash
/bin/cp "$@" || exit
dest="${*: -1}"
# shellcheck disable=SC2254 # the patterns are patterns
case "$dest" in ${STANDIN_COPY_ALTERS:-/nothing}) chmod u+w "$dest"; printf 'x' >>"$dest" ;; esac
# shellcheck disable=SC2254
case "$dest" in ${STANDIN_COPY_INTERRUPTS:-/nothing}) "$(dirname "$0")/interrupt-finish"; /bin/sleep 1 ;; esac
exit 0
EOF
  # TERM to the script itself: the topmost of this process's ancestors
  # running finish or staple-app, not one of its subshells.
  cat >"$bin/interrupt-finish" <<'EOF'
#!/bin/bash
p=$PPID
top=''
while [ -n "$p" ] && [ "$p" -gt 1 ]; do
  case "$(ps -o args= -p "$p" 2>/dev/null)" in
    *" finish "* | *" staple-app "*) top="$p" ;;
    *) [ -z "$top" ] || break ;;
  esac
  p="$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')"
done
[ -n "$top" ] && kill -TERM "$top"
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
  trap 'chflags -R nouchg "$T" 2>/dev/null; rm -rf "$T"' EXIT
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

  # bundle_ok DIR: the diagnostics finish keeps, exactly the allowlist, with
  # the packages kept_pkgs names (accepted, and last-candidate or stapled as
  # the case says), none named .pkg, their SHA-256s listed and matching;
  # attempts.txt with attempts_kept lines, each an attempt of 5; stapler's
  # recorded commands, exit statuses and output, if it ran; pkgutil's and
  # Gatekeeper's output for each package kept; and the trust settings. The
  # accepted package kept is the signed package byte for byte, unless the
  # case changed it.
  bundle_ok() {
    local d="$1" k n=0 f
    # Nothing but the allowlist, in which no name ends .pkg.
    for f in "$d"/* "$d"/.[!.]*; do
      [ -e "$f" ] || continue
      case "${f##*/}" in
        attempts.txt | gatekeeper.txt | pkgutil-check-signature.txt | stapler.txt | trust-settings.txt) ;;
        signed.pkg.accepted | signed.pkg.last-candidate | signed.pkg.stapled | signed.pkg.sha256) ;;
        *) return 1 ;;
      esac
    done
    for k in accepted last-candidate stapled; do
      if [[ " $kept_pkgs " == *" $k "* ]]; then
        [ -f "$d/signed.pkg.$k" ] && grep -q "  signed\.pkg\.$k\$" "$d/signed.pkg.sha256" &&
          grep -qx "== signed.pkg.$k" "$d/pkgutil-check-signature.txt" && grep -qx "== signed.pkg.$k" "$d/gatekeeper.txt" || return 1
        n=$((n + 1))
      else
        [ ! -e "$d/signed.pkg.$k" ] || return 1
      fi
    done
    [ "$(wc -l <"$d/signed.pkg.sha256" | tr -d ' ')" = "$n" ] && (cd "$d" && shasum -a 256 -c signed.pkg.sha256 >/dev/null 2>&1) || return 1
    [ "$accepted_intact" != yes ] || cmp -s "$d/signed.pkg.accepted" "$T/made/signed.pkg" || return 1
    [ "$(wc -l <"$d/attempts.txt" | tr -d ' ')" = "$attempts_kept" ] || return 1
    [ "$attempts_kept" = 0 ] || [ "$(grep -cE '^attempt [1-5] of 5, ' "$d/attempts.txt")" = "$attempts_kept" ] || return 1
    if [ "$attempts_kept" != 0 ]; then
      grep -q '^\$ xcrun stapler staple ' "$d/stapler.txt" && grep -qE '^\[exit [0-9]+\]$' "$d/stapler.txt" &&
        grep -q 'Processing: ' "$d/stapler.txt" || return 1
    fi
    grep -q 'No Trust Settings were found' "$d/trust-settings.txt"
  }

  # finish_case LABEL WANT-STATUS WANT-CALLS WANT-TEXT SETUP [NAME=VALUE...]:
  # finish, on a copy of the signed package, with the notarization record
  # SETUP makes (a command given the record's directory). It exits
  # WANT-STATUS, calls exactly WANT-CALLS, says WANT-TEXT, never prints a
  # secret, never notarizes, and leaves nothing in RUNNER_TEMP (unless
  # temp_may_remain is yes, for a case that breaks its removal). Only on
  # success is there a signed.pkg: the copy stapled, with the signed
  # package's mode; on failure there is none, and nothing beside it. It keeps
  # the diagnostic bundle exactly when it failed once stapling had begun:
  # once it says it kept the package Apple accepted, unless diag_kept is no.
  # diag_path is where it is asked to keep it.
  diag_path=''
  temp_may_remain=no
  diag_kept=auto
  kept_pkgs=accepted
  attempts_kept=0
  accepted_intact=yes
  finish_case() {
    local label="$1" want="$2" calls="$3" text="$4" setup="$5" status out verdict=yes got diag
    shift 5
    chflags -R nouchg "$T/run" 2>/dev/null
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
    # No stapler left running.
    if [ -s "$T/run/calls.stapler-pid" ] && kill -0 "$(cat "$T/run/calls.stapler-pid")" 2>/dev/null; then
      kill -KILL "$(cat "$T/run/calls.stapler-pid")" 2>/dev/null
      verdict=no
    fi
    [ "$temp_may_remain" = yes ] || [ -z "$(ls -A "$T/run/temp")" ] || verdict=no
    [ ! -e "$T/run/signed.pkg.promoting" ] || verdict=no
    if [ "$want" = 0 ]; then
      [ -f "$T/run/signed.pkg" ] && { cat "$T/made/signed.pkg"; printf 'ticket\n'; } | cmp -s - "$T/run/signed.pkg" &&
        [ "$(stat -f '%Lp' "$T/run/signed.pkg")" = "$(stat -f '%Lp' "$T/made/signed.pkg")" ] || verdict=no
    else
      [ ! -e "$T/run/signed.pkg" ] || verdict=no
    fi
    if [ -z "$diag_path" ]; then
      if [ "$want" != 0 ] && [ "$diag_kept" != no ] && says "$out" "5. kept the package Apple accepted"; then
        bundle_ok "$diag" || verdict=no
      else
        [ ! -e "$diag" ] || verdict=no
      fi
    fi
    chflags -R nouchg "$T/run" 2>/dev/null
    verdict_of "$label" "$verdict" "$got" "$out"
  }
  accepted() { record "$1" accepted; }
  none() { :; }
  unresolved() { record "$1" unresolved-deadline; }
  invalid() { record "$1" invalid; }
  other_file() { record "$1" accepted other.pkg; }
  other_bytes() { record "$1" accepted signed.pkg 0000000000000000000000000000000000000000000000000000000000000000; }
  # The calls, in pieces: attempt N stapled; attempt N stapled and
  # validated; the wait before attempt N; and the diagnostics, for K
  # packages kept.
  st() { printf 'xcrun stapler staple @%s' "$1"; }
  sv() { printf 'xcrun stapler staple @%s,xcrun stapler validate @%s' "$1" "$1"; }
  wait_for() { case "$1" in 2) printf 'sleep 15' ;; 3) printf 'sleep 30' ;; 4) printf 'sleep 60' ;; 5) printf 'sleep 120' ;; esac; }
  diagnosed() {
    local k
    for ((k = 0; k < $1; k++)); do printf 'pkgutil --check-signature,spctl,'; done
    printf 'security dump-trust-settings,security dump-trust-settings,security dump-trust-settings'
  }
  local finished n all_lost all_unvalidated
  finished="$(sv 1),pkgutil --expand-full,spctl"
  all_lost="$(st 1)"
  all_unvalidated="$(sv 1)"
  for n in 2 3 4 5; do
    all_lost="$all_lost,$(wait_for "$n"),$(st "$n")"
    all_unvalidated="$all_unvalidated,$(wait_for "$n"),$(sv "$n")"
  done

  printf '\n  finish: only after Apple accepted exactly this file\n'
  finish_case "accepted: the package kept, a fresh copy stapled, validated and promoted, compared, Gatekeeper; stapler's commands, output and exit statuses shown" \
    0 "$finished" "4. Apple accepted submission|5. kept the package Apple accepted, read only|    \$ xcrun stapler staple |The staple and validate action worked!|    \$ xcrun stapler validate |The validate action worked!|[exit 0]|5. attempt 1 of 5, |; validated|stapled and validates (attempt 1 of 5)|signed, notarized and stapled" accepted
  finish_case "no notarization record: refused, nothing stapled, the package removed" 1 "" "refusing to staple: no notarization record" none
  finish_case "a submission still unresolved at its deadline: refused, nothing stapled" 1 "" "was not accepted: unresolved-deadline" unresolved
  finish_case "an Invalid submission: refused, nothing stapled" 1 "" "was not accepted: invalid" invalid
  finish_case "Apple accepted another file: refused" 1 "" "not this signed.pkg" other_file
  finish_case "Apple accepted other bytes under this name: refused" 1 "" "not this signed.pkg" other_bytes
  finish_case "SYNDEO_EXPECT_SIGNED=no: refused" 1 "" "SYNDEO_EXPECT_SIGNED is not yes" accepted SYNDEO_EXPECT_SIGNED=no

  printf '\n  finish: up to five attempts, each on a fresh copy of the package Apple accepted; nothing resubmitted\n'
  finish_case "the connection lost once, then stapled: the second attempt, on a fresh copy, after 15 s" \
    0 "$(st 1),sleep 15,$(sv 2),pkgutil --expand-full,spctl" \
    "Error Domain=NSURLErrorDomain Code=-1005|[exit 68]|stapler staple exit 68, the copy unchanged: discarded|attempt 2 of 5 in 15s|stapled and validates (attempt 2 of 5)|signed, notarized and stapled" \
    accepted STANDIN_STAPLES=lost,ok
  finish_case "the connection lost four times, stapled the fifth: after 15, 30, 60 and 120 s" \
    0 "$(st 1),sleep 15,$(st 2),sleep 30,$(st 3),sleep 60,$(st 4),sleep 120,$(sv 5),pkgutil --expand-full,spctl" \
    "attempt 5 of 5 in 120s|stapled and validates (attempt 5 of 5)|signed, notarized and stapled" \
    accepted STANDIN_STAPLES=lost,lost,lost,lost,ok
  attempts_kept=5
  finish_case "the connection lost every time: five attempts, then fatal; no package; each attempt in the diagnostics" \
    1 "$all_lost,$(diagnosed 1)" \
    "attempt 5 of 5 in 120s|no attempt stapled a ticket that validates, after 5 attempts; there is no|kept the diagnostics in" \
    accepted STANDIN_STAPLES=lost
  finish_case "stapler failing every time, Error 65: five attempts, then fatal; no package" \
    1 "$all_lost,$(diagnosed 1)" "The staple and validate action failed! Error 65.|[exit 65]|no attempt stapled a ticket that validates, after 5 attempts" \
    accepted STANDIN_STAPLE_FAILS=yes
  kept_pkgs="accepted last-candidate"
  finish_case "stapled, but never validating: five fresh copies, never one stapled twice, then fatal; the last copy kept, not as .pkg" \
    1 "$all_unvalidated,$(diagnosed 2)" "stapler validate exit 65: set aside, never stapled again|no attempt stapled a ticket that validates, after 5 attempts" \
    accepted STANDIN_VALIDATE_FAILS=all
  kept_pkgs=accepted
  attempts_kept=0
  finish_case "stapled, but not validating once: the next attempt staples a fresh copy, not that one" \
    0 "$(sv 1),sleep 15,$(sv 2),pkgutil --expand-full,spctl" \
    "does not have a ticket stapled to it|stapler validate exit 65: set aside, never stapled again|stapled and validates (attempt 2 of 5)" \
    accepted STANDIN_VALIDATE_FAILS=1
  finish_case "stapled, but validating only the fifth time: five copies, the fifth promoted" \
    0 "$all_unvalidated,pkgutil --expand-full,spctl" "stapled and validates (attempt 5 of 5)|signed, notarized and stapled" \
    accepted STANDIN_VALIDATE_FAILS=4
  finish_case "a failure that changed the copy: set aside, never stapled again; the next attempt staples a fresh copy" \
    0 "$(st 1),sleep 15,$(sv 2),pkgutil --expand-full,spctl" \
    "stapler staple exit 68, and the copy changed, to|set aside, never stapled again|stapled and validates (attempt 2 of 5)" \
    accepted STANDIN_STAPLES=partial,ok
  kept_pkgs="accepted last-candidate"
  attempts_kept=5
  finish_case "failures that change the copy every time: five fresh copies, then fatal; the last changed copy kept" \
    1 "$all_lost,$(diagnosed 2)" "and the copy changed, to|no attempt stapled a ticket that validates, after 5 attempts" \
    accepted STANDIN_STAPLES=partial
  kept_pkgs=accepted
  attempts_kept=0
  finish_case "stapler saying it worked but adding no ticket: discarded, not validated, never promoted" \
    0 "$(st 1),sleep 15,$(sv 2),pkgutil --expand-full,spctl" \
    "stapler staple exit 0, but the copy is unchanged, with no ticket: discarded|stapled and validates (attempt 2 of 5)" \
    accepted STANDIN_STAPLES=noticket,ok
  finish_case "stapler running past its limit: stopped, exit 124, the attempt failed; the next stapled" \
    0 "$(st 1),sleep 15,$(sv 2),pkgutil --expand-full,spctl" \
    "stopped: still running after 1 seconds|[exit 124]|stapler staple exit 124, the copy unchanged: discarded|stapled and validates (attempt 2 of 5)" \
    accepted STANDIN_STAPLES=hang,ok SYNDEO_STAPLER_LIMIT_SECONDS=1
  finish_case "a stapler limit past 60 seconds: refused before anything" \
    1 "" "SYNDEO_STAPLER_LIMIT_SECONDS is not a whole number of seconds from 1 to 60" accepted SYNDEO_STAPLER_LIMIT_SECONDS=61

  printf '\n  finish: never stapling anything but the bytes Apple accepted\n'
  finish_case "the package kept not the accepted bytes from the start: stopped before any attempt; no package" \
    1 "" "5. the package kept is not the one Apple accepted" accepted 'STANDIN_COPY_ALTERS=*/accepted/*'
  finish_case "the first copy not the accepted bytes: stopped before stapler sees it; no package" \
    1 "$(diagnosed 1)" "5. attempt 1: its copy is|not the package Apple accepted; nothing is stapled|kept the diagnostics in" \
    accepted 'STANDIN_COPY_ALTERS=*/attempts/1/*'
  attempts_kept=1
  finish_case "a later copy not the accepted bytes: stopped there, nothing more stapled" \
    1 "$(st 1),sleep 15,$(diagnosed 1)" "5. attempt 2: its copy is|nothing is stapled" \
    accepted STANDIN_STAPLES=lost 'STANDIN_COPY_ALTERS=*/attempts/2/*'
  accepted_intact=no
  finish_case "the package kept changed between attempts: stopped, nothing more stapled; no package" \
    1 "$(st 1),sleep 15,$(diagnosed 1)" "5. attempt 2: the package kept is no longer the one Apple accepted; nothing more is stapled" \
    accepted STANDIN_STAPLES=tamper
  accepted_intact=yes
  attempts_kept=1
  finish_case "the copy beside signed.pkg not the stapled bytes: stopped before it is renamed into place; no package" \
    1 "$(sv 1),$(diagnosed 1)" "5. the copy beside|is not the stapled package" accepted 'STANDIN_COPY_ALTERS=*.promoting'

  printf '\n  finish: interrupted, and cleanup\n'
  attempts_kept=0
  finish_case "interrupted while stapling: exit 130 kept, no further attempt, no package, the diagnostics kept" \
    130 "$(st 1),$(diagnosed 1)" "kept the diagnostics in" accepted STANDIN_STAPLES=interrupt
  attempts_kept=1
  finish_case "interrupted while promoting the stapled copy: exit 130 kept, neither signed.pkg nor the copy beside it left" \
    130 "$(sv 1),$(diagnosed 1)" "; validated|kept the diagnostics in" accepted 'STANDIN_COPY_INTERRUPTS=*.promoting'
  finish_case "interrupted while waiting to try again: exit 130 kept, no further attempt, no package, the diagnostics kept" \
    130 "$(st 1),sleep 15,$(diagnosed 1)" "stapler staple exit 68, the copy unchanged: discarded|kept the diagnostics in" \
    accepted STANDIN_STAPLES=lost STANDIN_SLEEP_INTERRUPT=yes
  attempts_kept=0
  temp_may_remain=yes
  diag_kept=no
  finish_case "stapled, validated and accepted, but its working directory cannot be removed: fatal; no package" \
    1 "$finished" "signed, notarized and stapled|cleanup failed: removing the working directory;" \
    accepted STANDIN_STAPLES=lock
  diag_kept=auto
  attempts_kept=5
  finish_case "five attempts lost, and the working directory cannot be removed: both said, the first failure's status, the diagnostics kept" \
    1 "$all_lost,$(diagnosed 1)" "no attempt stapled a ticket that validates, after 5 attempts|kept the diagnostics in|cleanup failed: removing the working directory;" \
    accepted STANDIN_STAPLES=locklost
  temp_may_remain=no
  attempts_kept=0

  printf '\n  finish: after stapling\n'
  attempts_kept=1
  kept_pkgs="accepted stapled"
  finish_case "stapling that changes the contents: caught, the stapled package kept as .stapled, not as .pkg" \
    1 "$(sv 1),pkgutil --expand-full,$(diagnosed 2)" "6.stapled: the package's contents differ" accepted \
    STANDIN_STAPLE="$d/payload.pkg"
  finish_case "Gatekeeper accepts it, but as Developer ID, not notarized: fails, the diagnostics kept" \
    1 "$finished,$(diagnosed 2)" "does not accept it as Notarized Developer ID" accepted "STANDIN_SOURCE=Developer ID"
  finish_case "Gatekeeper accepts it from another identity: fails, the diagnostics kept" \
    1 "$finished,$(diagnosed 2)" "does not accept it as Notarized Developer ID" accepted "STANDIN_ORIGIN=Developer ID Installer: Someone Else (ZZ99ZZ99ZZ)"
  finish_case "Gatekeeper rejects it: fails, the diagnostics kept" 1 "$finished,$(diagnosed 2)" "does not accept it as Notarized Developer ID" accepted STANDIN_SPCTL=rejected
  kept_pkgs=accepted
  attempts_kept=0
  mkdir -p "$T/readonly" && chmod 555 "$T/readonly"
  diag_path="$T/readonly/diag"
  finish_case "diagnostics that cannot be written: it still fails, with the first failure's status, and there is no package" \
    1 "$all_unvalidated" "no attempt stapled a ticket that validates, after 5 attempts|could not keep all of the diagnostics" \
    accepted STANDIN_VALIDATE_FAILS=all
  mkdir -p "$T/existing/diag"
  diag_path="$T/existing/diag"
  finish_case "a diagnostics directory that already exists: refused before anything is stapled" \
    1 "" "already exists, and diagnostics are only ever written fresh" accepted
  chmod 755 "$T/readonly"
  diag_path=

  # Nothing in this script submits anything: no notarytool, and no
  # notarize.py, outside its comments and this self-test.
  local submits
  submits="$(sed -n '1,/^# -* self-test$/p' "$0" | grep -vE '^[[:space:]]*#' | grep -nE 'notarytool|notarize\.py' || true)"
  cases=$((cases + 1))
  if [ -z "$submits" ]; then
    printf '  ok    %s\n' "no mode submits anything: no notarytool or notarize.py outside the comments"
  else
    printf '  WRONG %s\n%s\n' "no mode submits anything: no notarytool or notarize.py outside the comments" "$(printf '%s\n' "$submits" | sed 's/^/          | /')"
    wrong=$((wrong + 1))
  fi
  # A bundle, zipped as build-macos hands it on, for staple-app; and that
  # ZIP unpacked once, as the bundle Apple accepted, to hold a stapled one to.
  mkdir -p "$T/bundle/Syndeo.app/Contents/MacOS" "$T/bundle/Syndeo.app/Contents/_CodeSignature"
  printf 'plist' >"$T/bundle/Syndeo.app/Contents/Info.plist"
  printf 'app' >"$T/bundle/Syndeo.app/Contents/MacOS/Syndeo"
  printf 'seal' >"$T/bundle/Syndeo.app/Contents/_CodeSignature/CodeResources"
  chmod 755 "$T/bundle/Syndeo.app/Contents/MacOS/Syndeo"
  ditto -c -k --keepParent "$T/bundle/Syndeo.app" "$T/Syndeo.app.zip"
  mkdir -p "$T/unzipped"
  ditto -x -k "$T/Syndeo.app.zip" "$T/unzipped"
  manifest "$T/unzipped/Syndeo.app" >"$T/accepted.manifest"
  local app_origin="Developer ID Application: SECRETVALUE Org ($team)"

  # app_bundle_ok DIR: the diagnostics staple-app keeps, exactly the
  # allowlist, with the bundles app_kept names (accepted, and last-candidate
  # or stapled as the case says) as ZIPs, their SHA-256s listed and matching;
  # the accepted ZIP the one given, unless the case changed it; attempts.txt
  # with attempts_kept lines; stapler's output, if it ran; codesign's and
  # Gatekeeper's view of each bundle kept; and the trust settings.
  app_bundle_ok() {
    local d="$1" k n=0 f
    for f in "$d"/* "$d"/.[!.]*; do
      [ -e "$f" ] || continue
      case "${f##*/}" in
        attempts.txt | codesign.txt | gatekeeper.txt | stapler.txt | trust-settings.txt | Syndeo.app.sha256) ;;
        Syndeo.app.accepted.zip | Syndeo.app.last-candidate.zip | Syndeo.app.stapled.zip) ;;
        *) return 1 ;;
      esac
    done
    for k in accepted last-candidate stapled; do
      if [[ " $app_kept " == *" $k "* ]]; then
        [ -f "$d/Syndeo.app.$k.zip" ] && grep -q "  Syndeo\.app\.$k\.zip\$" "$d/Syndeo.app.sha256" &&
          grep -qx "== $k" "$d/codesign.txt" && grep -qx "== $k" "$d/gatekeeper.txt" || return 1
        n=$((n + 1))
      else
        [ ! -e "$d/Syndeo.app.$k.zip" ] || return 1
      fi
    done
    [ "$(wc -l <"$d/Syndeo.app.sha256" | tr -d ' ')" = "$n" ] && (cd "$d" && shasum -a 256 -c Syndeo.app.sha256 >/dev/null 2>&1) || return 1
    [ "$accepted_intact" != yes ] || cmp -s "$d/Syndeo.app.accepted.zip" "$T/Syndeo.app.zip" || return 1
    [ "$(wc -l <"$d/attempts.txt" | tr -d ' ')" = "$attempts_kept" ] || return 1
    [ "$attempts_kept" = 0 ] || [ "$(grep -cE '^attempt [1-5] of 5, ' "$d/attempts.txt")" = "$attempts_kept" ] || return 1
    if [ "$attempts_kept" != 0 ]; then
      grep -q '^\$ xcrun stapler staple ' "$d/stapler.txt" && grep -qE '^\[exit [0-9]+\]$' "$d/stapler.txt" || return 1
    fi
    ! grep -q 'SECRETVALUE' "$d/codesign.txt" "$d/gatekeeper.txt" && grep -q 'No Trust Settings were found' "$d/trust-settings.txt"
  }

  # app_case LABEL WANT-STATUS WANT-CALLS WANT-TEXT [NAME=VALUE...]:
  # staple-app exits WANT-STATUS, calls exactly WANT-CALLS, says WANT-TEXT,
  # never prints a secret, never notarizes, leaves no stapler running and
  # nothing in RUNNER_TEMP (unless temp_may_remain is yes). Only on success
  # is there an <out-dir>: holding exactly Syndeo.app, the bundle Apple
  # accepted with its ticket added and nothing else changed; on failure there
  # is none. It keeps the diagnostics exactly when it failed once A1 had
  # kept the bundle, unless diag_kept is no.
  app_kept=accepted
  app_case() {
    local label="$1" want="$2" calls="$3" text="$4" status out verdict=yes got
    shift 4
    chflags -R nouchg "$T/run" 2>/dev/null
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
    if [ -s "$T/run/calls.stapler-pid" ] && kill -0 "$(cat "$T/run/calls.stapler-pid")" 2>/dev/null; then
      kill -KILL "$(cat "$T/run/calls.stapler-pid")" 2>/dev/null
      verdict=no
    fi
    [ "$temp_may_remain" = yes ] || [ -z "$(ls -A "$T/run/temp")" ] || verdict=no
    if [ "$want" = 0 ]; then
      [ "$(ls -A "$T/run/app")" = Syndeo.app ] && [ "$(cat "$T/run/app/Syndeo.app/Contents/CodeResources")" = ticket ] &&
        manifest "$T/run/app/Syndeo.app" | grep -vE '^\./Contents/CodeResources file 644 ' | cmp -s - "$T/accepted.manifest" &&
        [ ! -e "$T/run/diag" ] || verdict=no
    else
      [ ! -e "$T/run/app" ] || verdict=no
      if [ "$diag_kept" != no ] && says "$out" "A1. kept Syndeo.app as Apple accepted it"; then
        app_bundle_ok "$T/run/diag" || verdict=no
      else
        [ ! -e "$T/run/diag" ] || verdict=no
      fi
    fi
    chflags -R nouchg "$T/run" 2>/dev/null
    verdict_of "staple-app: $label" "$verdict" "$got" "$out"
  }
  # The calls, in pieces: attempt N's copy checked and stapled; checked,
  # stapled and validated; and the diagnostics, for K bundles kept.
  ast() { printf 'codesign --verify,xcrun stapler staple @%s' "$1"; }
  asv() { printf 'codesign --verify,xcrun stapler staple @%s,xcrun stapler validate @%s' "$1" "$1"; }
  app_diagnosed() {
    local k
    for ((k = 0; k < $1; k++)); do printf 'codesign -dv,spctl,'; done
    printf 'security dump-trust-settings,security dump-trust-settings,security dump-trust-settings'
  }
  local stapled app_lost app_unvalidated
  stapled="codesign --verify,$(asv 1),codesign --verify,spctl"
  app_lost="codesign --verify,$(ast 1)"
  app_unvalidated="codesign --verify,$(asv 1)"
  for n in 2 3 4 5; do
    app_lost="$app_lost,$(wait_for "$n"),$(ast "$n")"
    app_unvalidated="$app_unvalidated,$(wait_for "$n"),$(asv "$n")"
  done

  printf '\n  staple-app: the bundle Apple accepted, kept, and a fresh copy stapled, validated and promoted\n'
  attempts_kept=0
  app_case "accepted with the binaries: kept, a fresh copy checked, stapled, validated and promoted; compared, verified, Gatekeeper" \
    0 "$stapled" "A1. kept Syndeo.app as Apple accepted it: its ZIP, read only|its signature verifying|A2. attempt 1 of 5, |; validated|A2. the ticket is stapled and validates (attempt 1 of 5)|A3. the stapled bundle is the one Apple accepted|A4. its code signature verifies|A5. Gatekeeper accepts it to run|stapled:"
  app_case "SYNDEO_EXPECT_SIGNED=no: refused before anything is unpacked" 1 "" "SYNDEO_EXPECT_SIGNED is not yes" SYNDEO_EXPECT_SIGNED=no
  app_case "a stapler limit past 60 seconds: refused before anything is unpacked" \
    1 "" "SYNDEO_STAPLER_LIMIT_SECONDS is not a whole number of seconds from 1 to 60" SYNDEO_STAPLER_LIMIT_SECONDS=90

  printf '\n  staple-app: up to five attempts; nothing resubmitted\n'
  app_case "the connection lost once, then stapled: the second attempt, on a fresh copy, after 15 s" \
    0 "codesign --verify,$(ast 1),sleep 15,$(asv 2),codesign --verify,spctl" \
    "[exit 68]|stapler staple exit 68, the copy unchanged: discarded|A2. attempt 2 of 5 in 15s|stapled and validates (attempt 2 of 5)|stapled:" \
    STANDIN_STAPLES=lost,ok
  app_case "the connection lost four times, stapled the fifth: after 15, 30, 60 and 120 s" \
    0 "codesign --verify,$(ast 1),sleep 15,$(ast 2),sleep 30,$(ast 3),sleep 60,$(ast 4),sleep 120,$(asv 5),codesign --verify,spctl" \
    "A2. attempt 5 of 5 in 120s|stapled and validates (attempt 5 of 5)" STANDIN_STAPLES=lost,lost,lost,lost,ok
  app_case "validation failing once: the next attempt staples a fresh copy, not that one" \
    0 "codesign --verify,$(asv 1),sleep 15,$(asv 2),codesign --verify,spctl" \
    "stapler validate exit 65: set aside, never stapled again|stapled and validates (attempt 2 of 5)" STANDIN_VALIDATE_FAILS=1
  app_case "a failure that changed the copy: set aside, never stapled again; the next attempt staples a fresh copy" \
    0 "codesign --verify,$(ast 1),sleep 15,$(asv 2),codesign --verify,spctl" \
    "stapler staple exit 68, and the copy changed: set aside, never stapled again|stapled and validates (attempt 2 of 5)" \
    STANDIN_STAPLES=partial,ok
  app_case "stapler saying it worked but adding no ticket: discarded, never promoted; the next stapled" \
    0 "codesign --verify,$(ast 1),sleep 15,$(asv 2),codesign --verify,spctl" \
    "stapler staple exit 0, but the copy is unchanged, with no ticket: discarded|stapled and validates (attempt 2 of 5)" \
    STANDIN_STAPLES=noticket,ok
  app_case "stapler running past its limit: stopped, exit 124, the attempt failed; the next stapled" \
    0 "codesign --verify,$(ast 1),sleep 15,$(asv 2),codesign --verify,spctl" \
    "stopped: still running after 1 seconds|stapler staple exit 124, the copy unchanged: discarded|stapled and validates (attempt 2 of 5)" \
    STANDIN_STAPLES=hang,ok SYNDEO_STAPLER_LIMIT_SECONDS=1
  attempts_kept=5
  app_case "the connection lost every time: five attempts, then fatal; no bundle; each attempt in the diagnostics" \
    1 "$app_lost,$(app_diagnosed 1)" "A2. attempt 5 of 5 in 120s|A2. no attempt stapled a ticket that validates, after 5 attempts|kept the diagnostics in" \
    STANDIN_STAPLES=lost
  app_case "stapler failing every time, Error 65: five attempts, then fatal" \
    1 "$app_lost,$(app_diagnosed 1)" "The staple and validate action failed! Error 65.|no attempt stapled a ticket that validates, after 5 attempts" \
    STANDIN_STAPLE_FAILS=yes
  app_case "stapling that adds no ticket, every time: five discarded, then fatal" \
    1 "$app_lost,$(app_diagnosed 1)" "but the copy is unchanged, with no ticket: discarded|no attempt stapled a ticket that validates" \
    STANDIN_APP_STAPLE=none
  app_kept="accepted last-candidate"
  app_case "stapled, but never validating: five fresh copies, never one stapled twice, then fatal; the last copy kept" \
    1 "$app_unvalidated,$(app_diagnosed 2)" "set aside, never stapled again|no attempt stapled a ticket that validates, after 5 attempts" \
    STANDIN_VALIDATE_FAILS=all
  app_case "failures that change the copy every time: five fresh copies, then fatal; the last changed copy kept" \
    1 "$app_lost,$(app_diagnosed 2)" "and the copy changed: set aside, never stapled again|no attempt stapled a ticket that validates" \
    STANDIN_STAPLES=partial
  app_kept=accepted

  printf '\n  staple-app: never stapling anything but the bundle Apple accepted, its signature intact\n'
  attempts_kept=0
  app_case "the ZIP kept not the one given: stopped before anything is unpacked or stapled" \
    1 "" "A1. the ZIP kept is not" 'STANDIN_COPY_ALTERS=*/accepted/*'
  app_case "the bundle's signature not verifying as Apple accepted it: stopped before any attempt" \
    1 "codesign --verify" "A1. the bundle's code signature does not verify as Apple accepted it" 'STANDIN_UNVERIFIED=*/reference/*'
  app_case "the first copy not the bundle, byte for byte: stopped before stapler sees it; no bundle" \
    1 "codesign --verify,$(app_diagnosed 1)" "A2. attempt 1: its copy is not the bundle Apple accepted, path for path; nothing is stapled" \
    'STANDIN_DITTO_ALTERS=*/attempts/1'
  attempts_kept=1
  app_case "a later copy with a file's mode changed: stopped before stapler sees it" \
    1 "codesign --verify,$(ast 1),sleep 15,$(app_diagnosed 1)" "A2. attempt 2: its copy is not the bundle Apple accepted" \
    STANDIN_STAPLES=lost 'STANDIN_DITTO_MODE=*/attempts/2'
  attempts_kept=0
  app_case "a copy whose signature does not verify: stopped before stapler sees it; no bundle" \
    1 "codesign --verify,codesign --verify,$(app_diagnosed 1)" "A2. attempt 1: its copy's code signature does not verify; nothing is stapled" \
    'STANDIN_UNVERIFIED=*/attempts/1/*'
  attempts_kept=1
  accepted_intact=no
  app_case "the ZIP kept changed between attempts: stopped, nothing more stapled; no bundle" \
    1 "codesign --verify,$(ast 1),sleep 15,$(app_diagnosed 1)" "A2. attempt 2: the ZIP kept is no longer the one Apple accepted; nothing more is stapled" \
    STANDIN_STAPLES=tamper
  accepted_intact=yes
  app_case "the copy into <out-dir> not the stapled bundle: stopped before it is renamed into place; no bundle" \
    1 "codesign --verify,$(asv 1),$(app_diagnosed 1)" "A2. the copy in" 'STANDIN_DITTO_ALTERS=*/.Syndeo.app.promoting'
  attempts_kept=1

  printf '\n  staple-app: interrupted, and cleanup\n'
  attempts_kept=0
  app_case "interrupted while stapling: exit 130 kept, stapler stopped, no further attempt, no bundle, the diagnostics kept" \
    130 "codesign --verify,$(ast 1),$(app_diagnosed 1)" "kept the diagnostics in" STANDIN_STAPLES=interrupt
  attempts_kept=1
  app_case "interrupted while waiting to try again: exit 130 kept, no further attempt, no bundle" \
    130 "codesign --verify,$(ast 1),sleep 15,$(app_diagnosed 1)" "stapler staple exit 68, the copy unchanged: discarded|kept the diagnostics in" \
    STANDIN_STAPLES=lost STANDIN_SLEEP_INTERRUPT=yes
  app_case "interrupted while promoting the stapled copy: exit 130 kept, no bundle and no copy left in <out-dir>" \
    130 "codesign --verify,$(asv 1),$(app_diagnosed 1)" "; validated|kept the diagnostics in" 'STANDIN_DITTO_INTERRUPTS=*/.Syndeo.app.promoting'
  attempts_kept=0
  temp_may_remain=yes
  diag_kept=no
  app_case "stapled, validated and accepted, but its working directory cannot be removed: fatal; no bundle" \
    1 "$stapled" "stapled:|cleanup failed: removing the working directory;" STANDIN_STAPLES=lock
  diag_kept=auto
  attempts_kept=5
  app_case "five attempts lost, and the working directory cannot be removed: both said, the first failure's status, the diagnostics kept" \
    1 "$app_lost,$(app_diagnosed 1)" "its copy could not be removed; cleanup will fail for it|no attempt stapled a ticket that validates, after 5 attempts|kept the diagnostics in|cleanup failed: removing the working directory;" \
    STANDIN_STAPLES=locklost
  temp_may_remain=no

  printf '\n  staple-app: after stapling\n'
  attempts_kept=1
  app_kept="accepted stapled"
  app_case "stapling that changes another file: caught, the stapled bundle kept as a ZIP" \
    1 "codesign --verify,$(asv 1),$(app_diagnosed 2)" "A3. stapling changed the bundle other than by adding its ticket" STANDIN_APP_STAPLE=changes
  app_case "a signature that does not verify once stapled: fails, the diagnostics kept" \
    1 "codesign --verify,$(asv 1),codesign --verify,$(app_diagnosed 2)" "A4. its code signature does not verify" 'STANDIN_UNVERIFIED=*/run/app/Syndeo.app'
  app_case "Gatekeeper: Developer ID, not notarized: fails, the identity not shown" \
    1 "$stapled,$(app_diagnosed 2)" "A5. Gatekeeper does not accept it|origin=<an identity, not shown>" "STANDIN_SOURCE=Developer ID"
  app_case "Gatekeeper: another team's Application identity: fails" \
    1 "$stapled,$(app_diagnosed 2)" "A5. Gatekeeper does not accept it" "STANDIN_ORIGIN=Developer ID Application: SECRETVALUE Org (ZZ99ZZ99ZZ)"
  app_case "Gatekeeper: an Installer identity of the team: fails" \
    1 "$stapled,$(app_diagnosed 2)" "A5. Gatekeeper does not accept it" "STANDIN_ORIGIN=Developer ID Installer: SECRETVALUE Org ($team)"
  app_case "Gatekeeper rejects it: fails" 1 "$stapled,$(app_diagnosed 2)" "A5. Gatekeeper does not accept it" STANDIN_SPCTL=rejected
  app_kept=accepted
  attempts_kept=0
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
