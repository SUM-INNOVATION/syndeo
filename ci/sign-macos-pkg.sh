#!/usr/bin/env bash
# Sign, notarize and staple the macOS installer package, in a fixed order.
#
#   ci/sign-macos-pkg.sh <unsigned.pkg> <signed.pkg>
#   ci/sign-macos-pkg.sh --self-test
#
# Only for a release that ci/check-signing-config.sh calls signed: it runs
# that first, with the whole configuration, and refuses anything else. Then,
# stopping at the first failure:
#  1. expand the unsigned package (pkgutil --expand-full);
#  2. productsign it with the Developer ID Installer identity, from a keychain
#     of its own, into <signed.pkg>;
#  3. expand that, and require it identical to the unsigned one: the
#     Distribution, PackageInfo, BOM, scripts and every payload path, with its
#     bytes, mode, owner and link target. Only the signature may differ;
#  4. notarize it (notarytool submit --wait). The status has to be Accepted;
#     anything else fetches the notary's log, prints it, and fails;
#  5. staple the ticket to it, and validate the ticket;
#  6. expand it once more, and require the same again;
#  7. require Gatekeeper to accept it as Notarized Developer ID, from the
#     Installer identity.
# On any failure <signed.pkg> is removed, so nothing half-done can be shipped.
# Whatever happens, the keychain is deleted, the user keychain search list is
# put back as it was, and the certificate, the notary key and the expansions
# are removed.
#
# The secrets it uses: MACOS_INSTALLER_CERTIFICATE_P12_BASE64,
# MACOS_INSTALLER_CERTIFICATE_PASSWORD, MACOS_INSTALLER_SIGNING_IDENTITY,
# MACOS_NOTARY_KEY_BASE64, MACOS_NOTARY_KEY_ID and MACOS_NOTARY_ISSUER_ID; see
# ci/check-signing-config.sh for all ten. Nothing is traced, and no value is
# printed. The tools are found on PATH, which is how the self-test puts
# stand-ins in their place.
set -euo pipefail
LC_ALL=C
export LC_ALL

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

# same_as_unsigned STEP DIR: DIR, an expansion, is exactly the unsigned one.
same_as_unsigned() {
  manifest "$2" >"$work/$1.manifest"
  if ! cmp -s "$work/unsigned.manifest" "$work/$1.manifest"; then
    diff "$work/unsigned.manifest" "$work/$1.manifest" | grep '^[<>]' | head -10 >&2 || true
    die "$1: the package's contents differ from the unsigned package's"
  fi
}

sign() {
  case "$-" in *x*) die "refusing: tracing is on, and would print the secrets" ;; esac
  [ "$#" = 2 ] || die "usage: $0 <unsigned.pkg> <signed.pkg>"
  unsigned="$1"
  out="$2"
  local decision
  decision="$(bash "$here/check-signing-config.sh")" || die "refusing: the signing configuration is not consistent"
  [ "$decision" = "signing: yes" ] || die "refusing: SYNDEO_EXPECT_SIGNED is not yes, so there is nothing to sign"
  [ -f "$unsigned" ] || die "no $unsigned"
  [ ! -e "$out" ] && [ ! -L "$out" ] || die "$out exists"
  local identity="$MACOS_INSTALLER_SIGNING_IDENTITY"

  umask 077
  work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sign-macos-pkg.XXXXXX")"
  keychain="$work/installer.keychain-db"
  saved=()
  succeeded=no
  trap cleanup EXIT
  trap 'exit 130' INT TERM HUP

  pkgutil --expand-full "$unsigned" "$work/unsigned" >/dev/null || die "1. could not expand $unsigned"
  manifest "$work/unsigned" >"$work/unsigned.manifest"
  say "1. expanded the unsigned package: $(wc -l <"$work/unsigned.manifest" | tr -d ' ') paths"

  # The search list as it was, to put back whatever happens.
  local line
  while IFS= read -r line; do
    line="${line#"${line%%[![:space:]]*}"}"
    line="${line#\"}"
    line="${line%\"}"
    [ -z "$line" ] || saved+=("$line")
  done < <(security list-keychains -d user)
  local password
  password="$(openssl rand -hex 32)"
  security create-keychain -p "$password" "$keychain" >/dev/null
  security set-keychain-settings -lut 21600 "$keychain" >/dev/null
  security unlock-keychain -p "$password" "$keychain" >/dev/null
  printf '%s' "$MACOS_INSTALLER_CERTIFICATE_P12_BASE64" | base64 --decode >"$work/installer.p12" ||
    die "2. the Installer certificate is not base64"
  security import "$work/installer.p12" -k "$keychain" -P "$MACOS_INSTALLER_CERTIFICATE_PASSWORD" \
    -T /usr/bin/productsign >/dev/null || die "2. could not import the Installer certificate"
  rm -f "$work/installer.p12"
  security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null
  security list-keychains -d user -s "$keychain" "${saved[@]}"
  productsign --sign "$identity" --keychain "$keychain" --timestamp "$unsigned" "$out" >/dev/null ||
    die "2. productsign failed"
  say "2. signed with the Installer identity"

  pkgutil --expand-full "$out" "$work/signed" >/dev/null || die "3. could not expand the signed package"
  same_as_unsigned 3.signed "$work/signed"
  say "3. the signed package's contents are exactly the unsigned package's"

  printf '%s' "$MACOS_NOTARY_KEY_BASE64" | base64 --decode >"$work/notary.p8" || die "4. the notary key is not base64"
  local notary=(--key "$work/notary.p8" --key-id "$MACOS_NOTARY_KEY_ID" --issuer "$MACOS_NOTARY_ISSUER_ID")
  xcrun notarytool submit "$out" "${notary[@]}" --wait --timeout 30m --output-format json \
    >"$work/notary.json" 2>"$work/notary.err" || true
  local status id
  status="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1])).get("status",""))' "$work/notary.json" 2>/dev/null)" || status=''
  id="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id",""))' "$work/notary.json" 2>/dev/null)" || id=''
  if [ "$status" != Accepted ]; then
    sed 's/^/    /' "$work/notary.err" >&2 || true
    if [[ "$id" =~ ^[0-9A-Za-z-]+$ ]]; then
      xcrun notarytool log "$id" "${notary[@]}" "$work/notary-log.json" >/dev/null 2>&1 || true
      say "4. the notary's log for submission $id:"
      sed 's/^/    /' "$work/notary-log.json" 2>/dev/null || say "   (it could not be fetched)"
    fi
    die "4. notarization ended '${status:-without a status}', not Accepted"
  fi
  rm -f "$work/notary.p8"
  say "4. notarized: submission $id, Accepted"

  xcrun stapler staple "$out" >/dev/null || die "5. could not staple the ticket"
  xcrun stapler validate "$out" >/dev/null || die "5. the stapled ticket does not validate"
  say "5. the ticket is stapled and validates"

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

cleanup() {
  local status=$?
  if [ "${#saved[@]}" -gt 0 ]; then
    security list-keychains -d user -s "${saved[@]}" >/dev/null 2>&1 || true
  fi
  if [ -e "$keychain" ]; then
    security delete-keychain "$keychain" >/dev/null 2>&1 || true
  fi
  rm -rf "${work:?}"
  if [ "$succeeded" != yes ]; then
    rm -f "${out:?}"
    [ "$status" != 0 ] || status=1
  fi
  exit "$status"
}

# ------------------------------------------------------------------ self-test

# The tools, as stand-ins that log what they were asked and do what the case
# says; pkgutil is the real one behind a log.
write_standins() {
  local bin="$1"
  mkdir -p "$bin"
  cat >"$bin/security" <<'EOF'
#!/bin/bash
echo "security $1" >>"$STANDIN_CALLS"
case "$1" in
  list-keychains)
    if [ "${4:-}" = -s ]; then shift 4; printf '%s\n' "$@" >"$STANDIN_LIST"; else sed 's/.*/    "&"/' "$STANDIN_LIST"; fi ;;
  create-keychain) : >"${@: -1}" ;;
  # As security does, deleting a keychain also takes it off the search list;
  # unless the case says deleting fails.
  delete-keychain)
    [ "${STANDIN_DELETE:-works}" = works ] || exit 1
    rm -f "$2"; grep -vxF "$2" "$STANDIN_LIST" >"$STANDIN_LIST.new" || true; mv "$STANDIN_LIST.new" "$STANDIN_LIST" ;;
  import) [ -s "$2" ] || exit 1 ;;
esac
exit 0
EOF
  cat >"$bin/productsign" <<'EOF'
#!/bin/bash
echo "productsign" >>"$STANDIN_CALLS"
in="${@: -2:1}"; out="${@: -1}"
case "${STANDIN_PRODUCTSIGN:-copy}" in
  copy) cp "$in" "$out" ;;
  *) cp "$STANDIN_PRODUCTSIGN" "$out" ;;
esac
EOF
  cat >"$bin/xcrun" <<'EOF'
#!/bin/bash
case "$1 $2" in
  "notarytool submit")
    echo "xcrun notarytool submit" >>"$STANDIN_CALLS"
    [ "$(cat "$5")" = "notary-key" ] || exit 1
    printf '{"id":"4f1d0e3a-0000-4000-8000-000000000001","status":"%s","message":"Processing complete"}\n' "${STANDIN_NOTARY:-Accepted}"
    [ "${STANDIN_NOTARY:-Accepted}" = Accepted ] ;;
  "notarytool log")
    echo "xcrun notarytool log" >>"$STANDIN_CALLS"
    printf '{"status":"Invalid","issues":[{"message":"The binary is not signed."}]}\n' >"${@: -1}" ;;
  "stapler staple")
    echo "xcrun stapler staple" >>"$STANDIN_CALLS"
    [ -z "${STANDIN_STAPLE:-}" ] || cp "$STANDIN_STAPLE" "$3" ;;
  "stapler validate")
    echo "xcrun stapler validate" >>"$STANDIN_CALLS"
    [ "${STANDIN_VALIDATE:-yes}" = yes ] ;;
  *) exit 1 ;;
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
  local -a good=(
    MACOS_CERTIFICATE_P12_BASE64=U0VDUkVUVkFMVUUtYXBw
    MACOS_CERTIFICATE_PASSWORD=SECRETVALUE-app-password
    "MACOS_SIGNING_IDENTITY=Developer ID Application: SECRETVALUE Org ($team)"
    MACOS_INSTALLER_CERTIFICATE_P12_BASE64=U0VDUkVUVkFMVUUtaW5zdGFsbGVy
    MACOS_INSTALLER_CERTIFICATE_PASSWORD=SECRETVALUE-installer-password
    "MACOS_INSTALLER_SIGNING_IDENTITY=$identity"
    "MACOS_TEAM_ID=$team"
    "MACOS_NOTARY_KEY_BASE64=$(printf notary-key | base64)"
    MACOS_NOTARY_KEY_ID=SECRETVALUE-key-id
    MACOS_NOTARY_ISSUER_ID=SECRETVALUE-issuer
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

  # sign_case LABEL WANT-STATUS WANT-CALLS WANT-TEXT [NAME=VALUE...]: signing
  # the unsigned package exits WANT-STATUS, calls exactly WANT-CALLS (the
  # tools other than security, in order, comma-separated), says WANT-TEXT,
  # leaves the signed package only on success, never prints a secret, and
  # always puts the keychain search list back and leaves nothing behind.
  sign_case() {
    local label="$1" want="$2" calls="$3" text="$4" status out verdict=yes got
    shift 4
    rm -rf "$T/run"
    mkdir -p "$T/run/temp"
    : >"$T/run/calls"
    printf '%s\n' /Users/runner/Library/Keychains/login.keychain-db "/Library/Keychains/System.keychain" >"$T/run/list"
    cp "$T/run/list" "$T/run/list.before"
    out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" HOME="$T/run" RUNNER_TEMP="$T/run/temp" \
      STANDIN_CALLS="$T/run/calls" STANDIN_LIST="$T/run/list" STANDIN_ORIGIN="$identity" \
      "$@" bash "$0" "$d/unsigned.pkg" "$T/run/signed.pkg" 2>&1)" && status=0 || status=$?
    got="$({ grep -v '^security' "$T/run/calls" || true; } | paste -sd, -)"
    [ "$status" = "$want" ] || verdict=no
    [ "$got" = "$calls" ] || verdict=no
    printf '%s' "$out" | grep -qF -- "$text" || verdict=no
    if printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU|notary-key'; then verdict=no; fi
    cmp -s "$T/run/list" "$T/run/list.before" || verdict=no
    [ -z "$(ls -A "$T/run/temp")" ] || verdict=no
    if [ "$want" = 0 ]; then
      [ -f "$T/run/signed.pkg" ] || verdict=no
      # The keychain was made, used and deleted.
      grep -qx 'security create-keychain' "$T/run/calls" && [ "$(tail -n 1 "$T/run/calls")" = 'security delete-keychain' ] || verdict=no
    else
      [ ! -e "$T/run/signed.pkg" ] || verdict=no
    fi
    cases=$((cases + 1))
    if [ "$verdict" = yes ]; then
      printf '  ok    %s\n' "$label"
    else
      printf '  WRONG %s\n          | exit %s; calls: %s\n' "$label" "$status" "$got"
      printf '%s\n' "$out" | sed 's/^/          | /'
      wrong=$((wrong + 1))
    fi
  }
  local all="pkgutil --expand-full,productsign,pkgutil --expand-full,xcrun notarytool submit,xcrun stapler staple,xcrun stapler validate,pkgutil --expand-full,spctl"

  printf '\n  signing the package, in order, with stand-ins for the tools\n'
  sign_case "a consistent signed configuration: expand, sign, compare, notarize, staple, validate, compare, Gatekeeper" \
    0 "$all" "signed, notarized and stapled" SYNDEO_EXPECT_SIGNED=yes "${good[@]}"
  sign_case "deleting the keychain fails: the search list is put back all the same" \
    0 "$all" "signed, notarized and stapled" SYNDEO_EXPECT_SIGNED=yes "${good[@]}" STANDIN_DELETE=fails
  sign_case "SYNDEO_EXPECT_SIGNED=no: refused before any tool runs" \
    1 "" "SYNDEO_EXPECT_SIGNED is not yes" SYNDEO_EXPECT_SIGNED=no
  local n v partial
  for n in MACOS_INSTALLER_CERTIFICATE_P12_BASE64 MACOS_INSTALLER_SIGNING_IDENTITY MACOS_NOTARY_KEY_BASE64 MACOS_CERTIFICATE_PASSWORD; do
    partial=()
    for v in "${good[@]}"; do [ "${v%%=*}" = "$n" ] || partial+=("$v"); done
    sign_case "yes without $n: refused before any tool runs" 1 "" "not consistent" SYNDEO_EXPECT_SIGNED=yes "${partial[@]}"
  done
  sign_case "a productsign that changes a payload byte: caught, nothing notarized" \
    1 "pkgutil --expand-full,productsign,pkgutil --expand-full" "3.signed: the package's contents differ" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" STANDIN_PRODUCTSIGN="$d/payload.pkg"
  sign_case "a productsign that changes the preinstall: caught, nothing notarized" \
    1 "pkgutil --expand-full,productsign,pkgutil --expand-full" "3.signed: the package's contents differ" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" STANDIN_PRODUCTSIGN="$d/script.pkg"
  sign_case "notarization Invalid: its log is fetched and printed, nothing stapled" \
    1 "pkgutil --expand-full,productsign,pkgutil --expand-full,xcrun notarytool submit,xcrun notarytool log" "The binary is not signed." \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" STANDIN_NOTARY=Invalid
  sign_case "notarization In Progress after the wait: not Accepted, fails" \
    1 "pkgutil --expand-full,productsign,pkgutil --expand-full,xcrun notarytool submit,xcrun notarytool log" "'In Progress', not Accepted" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" "STANDIN_NOTARY=In Progress"
  sign_case "a ticket that does not validate: fails, Gatekeeper not asked" \
    1 "pkgutil --expand-full,productsign,pkgutil --expand-full,xcrun notarytool submit,xcrun stapler staple,xcrun stapler validate" "does not validate" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" STANDIN_VALIDATE=no
  sign_case "stapling that changes the contents: caught" \
    1 "pkgutil --expand-full,productsign,pkgutil --expand-full,xcrun notarytool submit,xcrun stapler staple,xcrun stapler validate,pkgutil --expand-full" "6.stapled: the package's contents differ" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" STANDIN_STAPLE="$d/payload.pkg"
  sign_case "Gatekeeper accepts it, but as Developer ID, not notarized: fails" \
    1 "$all" "does not accept it as Notarized Developer ID" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" "STANDIN_SOURCE=Developer ID"
  sign_case "Gatekeeper accepts it from another identity: fails" \
    1 "$all" "does not accept it as Notarized Developer ID" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" "STANDIN_ORIGIN=Developer ID Installer: Someone Else (ZZ99ZZ99ZZ)"
  sign_case "Gatekeeper rejects it: fails" \
    1 "$all" "does not accept it as Notarized Developer ID" \
    SYNDEO_EXPECT_SIGNED=yes "${good[@]}" STANDIN_SPCTL=rejected

  local out status
  out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" SYNDEO_EXPECT_SIGNED=yes "${good[@]}" \
    STANDIN_CALLS="$T/run/calls" STANDIN_LIST="$T/run/list" bash -x "$0" "$d/unsigned.pkg" "$T/run/x.pkg" 2>&1)" && status=0 || status=$?
  cases=$((cases + 1))
  if [ "$status" = 1 ] && printf '%s' "$out" | grep -qF "tracing is on" && ! printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU'; then
    printf '  ok    %s\n' "traced: refused, before any value is read"
  else
    printf '  WRONG %s (exit %s)\n' "traced: refused, before any value is read" "$status"
    wrong=$((wrong + 1))
  fi
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test ;;
  *) sign "$@" ;;
esac
