#!/usr/bin/env bash
# Sign and notarize the macOS binaries.
#
#   ci/sign-macos.sh <directory of built binaries>
#   ci/sign-macos.sh --self-test
#
# A no-op, loudly, when the signing secrets are not configured: an unsigned
# build is still a usable one.
#
# All seven executables are signed alike: Developer ID Application, the
# hardened runtime, a secure timestamp, and no entitlements. The keystore once
# carried a keychain access group, to reach the data protection keychain; that
# entitlement is restricted, macOS honours it only where a provisioning profile
# authorises it, and a standalone executable cannot carry one, so the signed
# keystore was killed at launch (the signed 0.1.6 dry run). Signed or not, the
# keystore uses the ordinary keychain, reports platform presence as
# unenforced, and requires the passphrase; `syndeo-keystore status` says so.
#
# Secrets this reads, all of them secrets of the release-macos environment,
# and only after ci/check-signing-config.sh has found all ten consistent.
# release.yml's build-macos gives it the Application certificate and the team
# alone, never a notary key, so there it signs and does not notarize:
# notarize-binaries does that, in a job of its own.
#
#   MACOS_CERTIFICATE_P12_BASE64  Developer ID Application certificate and key,
#                                 exported as .p12 and base64 encoded
#   MACOS_CERTIFICATE_PASSWORD    the export password for that .p12
#   MACOS_SIGNING_IDENTITY        e.g. "Developer ID Application: Example (AB12CD34EF)"
#   MACOS_TEAM_ID                 the ten-character team identifier
#   MACOS_NOTARY_KEY_BASE64       App Store Connect API key (.p8), base64 encoded
#   MACOS_NOTARY_KEY_ID           that key's id
#   MACOS_NOTARY_ISSUER_ID        the issuer uuid it belongs to
set -euo pipefail

# --self-test: what this script asks codesign to do, with stand-ins for
# security and codesign. The stand-in codesign refuses any invocation carrying
# --entitlements, so a signing call that brings an entitlement back fails here
# before it can fail a release at launch.
self_test() {
  local cases=0 wrong=0 n identity="Developer ID Application: SECRETVALUE Org (AB12CD34EF)"
  local names=(syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit)
  # Not local: the EXIT trap runs after this function has returned.
  T="$(mktemp -d)"
  trap 'rm -rf "$T"' EXIT
  mkdir -p "$T/bin" "$T/release"
  for n in "${names[@]}"; do : >"$T/release/$n"; done
  cat >"$T/bin/security" <<'EOF'
#!/bin/bash
echo "security $1" >>"$STANDIN_CALLS"
if [ "$1" = list-keychains ] && [ "$#" = 3 ]; then echo '    "/Users/runner/Library/Keychains/login.keychain-db"'; fi
EOF
  cat >"$T/bin/codesign" <<'EOF'
#!/bin/bash
echo "codesign $*" >>"$STANDIN_CALLS"
for arg in "$@"; do
  if [ "$arg" = --entitlements ]; then echo "stand-in codesign: --entitlements refused: no shipped executable may claim one" >&2; exit 1; fi
done
EOF
  chmod 755 "$T/bin/security" "$T/bin/codesign"
  local -a signing=(
    CERTIFICATE=U0VDUkVUVkFMVUUtYXBwbGljYXRpb24=
    CERTIFICATE_PASSWORD=SECRETVALUE-application-password
    "SIGNING_IDENTITY=$identity"
    TEAM_ID=AB12CD34EF
  )
  # What signing has to ask codesign, exactly, in order: each executable
  # signed alike and then verified.
  local want=""
  for n in "${names[@]}"; do
    want="${want}codesign --force --timestamp --options runtime --sign $identity $T/release/$n"$'\n'
    want="${want}codesign --verify --strict --verbose=2 $T/release/$n"$'\n'
  done

  # sign_case LABEL WANT-STATUS WANT-CODESIGN TEXT [NAME=VALUE...]: exits
  # WANT-STATUS, asks codesign exactly WANT-CODESIGN, never --entitlements,
  # says TEXT, and never prints a secret.
  sign_case() {
    local label="$1" status_want="$2" codesign_want="$3" text="$4" out status verdict=yes got
    shift 4
    rm -rf "$T/temp" && mkdir -p "$T/temp" && : >"$T/calls"
    out="$(env -i PATH="$T/bin:/usr/bin:/bin:/usr/sbin:/sbin" RUNNER_TEMP="$T/temp" STANDIN_CALLS="$T/calls" "$@" \
      bash "$0" "$T/release" 2>&1)" && status=0 || status=$?
    got="$(grep '^codesign ' "$T/calls" || true)"
    [ "$status" = "$status_want" ] || verdict=no
    [ "$got" = "${codesign_want%$'\n'}" ] || verdict=no
    ! grep -q -- '--entitlements' "$T/calls" || verdict=no
    printf '%s' "$out" | grep -qF -- "$text" || verdict=no
    ! printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU' || verdict=no
    cases=$((cases + 1))
    if [ "$verdict" = yes ]; then
      printf '  ok    %s\n' "$label"
    else
      printf '  WRONG %s (exit %s)\n' "$label" "$status"
      printf '%s\n' "$out" "$got" | sed 's/^/          | /'
      wrong=$((wrong + 1))
    fi
  }

  printf '\n  sign-macos.sh: seven executables, signed alike, without entitlements\n'
  sign_case "the Application identity: all seven with the hardened runtime and a timestamp, no entitlements, each verified" \
    0 "$want" "Signed but not notarized" "${signing[@]}"
  sign_case "no certificate: nothing signed, and it says so" 0 "" "shipping unsigned binaries"
  sign_case "a certificate without its password: refused before anything is signed" \
    1 "" "MACOS_CERTIFICATE_PASSWORD is required" CERTIFICATE=U0VDUkVUVkFMVUUtYXBwbGljYXRpb24= "SIGNING_IDENTITY=$identity" TEAM_ID=AB12CD34EF
  sign_case "a certificate without an identity: refused before anything is signed" \
    1 "" "MACOS_SIGNING_IDENTITY is required" CERTIFICATE=U0VDUkVUVkFMVUUtYXBwbGljYXRpb24= CERTIFICATE_PASSWORD=SECRETVALUE-application-password TEAM_ID=AB12CD34EF
  # The guard itself: the stand-in really does refuse an entitlement.
  cases=$((cases + 1))
  : >"$T/calls"
  if STANDIN_CALLS="$T/calls" "$T/bin/codesign" --force --entitlements "$T/x.plist" --sign - "$T/release/syndeo" 2>/dev/null; then
    printf '  WRONG %s\n' "the stand-in codesign refuses --entitlements"
    wrong=$((wrong + 1))
  else
    printf '  ok    %s\n' "the stand-in codesign refuses --entitlements"
  fi
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

if [ "${1:-}" = --self-test ]; then
  self_test
  exit
fi

release="${1:?directory of built binaries}"
# Every macOS executable in the tarball, and the list used for signing,
# verifying and notarizing alike, so none of them can be shipped unsigned
# beside the others.
binaries=(syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit)

if [ -z "${CERTIFICATE:-}" ]; then
  echo "No MACOS_CERTIFICATE_P12_BASE64 set: shipping unsigned binaries."
  echo "The keystore uses the ordinary keychain and reports platform presence"
  echo "as unenforced, which makes the passphrase mandatory, signed or not."
  exit 0
fi

: "${CERTIFICATE_PASSWORD:?MACOS_CERTIFICATE_PASSWORD is required alongside the certificate}"
: "${SIGNING_IDENTITY:?MACOS_SIGNING_IDENTITY is required alongside the certificate}"
: "${TEAM_ID:?MACOS_TEAM_ID is required alongside the certificate}"

# A keychain of our own, so nothing is left in the runner's login keychain and
# the password never has to be a real one.
keychain="$RUNNER_TEMP/syndeo-signing.keychain-db"
keychain_password="$(openssl rand -hex 32)"
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"

certificate="$RUNNER_TEMP/certificate.p12"
echo "$CERTIFICATE" | base64 --decode > "$certificate"
security import "$certificate" -k "$keychain" -P "$CERTIFICATE_PASSWORD" \
  -T /usr/bin/codesign -T /usr/bin/security
rm -f "$certificate"
security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$keychain_password" "$keychain" > /dev/null
# Prepended to the search list rather than replacing it, so anything else the
# runner depends on still resolves.
existing=$(security list-keychains -d user | sed -e 's/^[[:space:]]*"//' -e 's/"$//')
# shellcheck disable=SC2086
security list-keychains -d user -s "$keychain" $existing

# All seven alike, and none with entitlements. A restricted one, such as the
# keychain access group the keystore once had, gets a standalone executable
# killed at launch; the release runs every signed binary before Apple sees
# any of them, and ci/verify-pkg.sh refuses an entitlement on any of them.
for binary in "${binaries[@]}"; do
  codesign --force --timestamp --options runtime \
    --sign "$SIGNING_IDENTITY" "${release}/${binary}"
  codesign --verify --strict --verbose=2 "${release}/${binary}"
done

if [ -z "${NOTARY_KEY:-}" ]; then
  echo "Signed but not notarized: MACOS_NOTARY_KEY_BASE64 is not set."
  echo "A browser download will be quarantined; a curl | sh install will not."
  exit 0
fi

: "${NOTARY_KEY_ID:?MACOS_NOTARY_KEY_ID is required alongside the notary key}"
: "${NOTARY_ISSUER_ID:?MACOS_NOTARY_ISSUER_ID is required alongside the notary key}"

notary_key="$RUNNER_TEMP/notary.p8"
echo "$NOTARY_KEY" | base64 --decode > "$notary_key"

# Only the shipped binaries go to the notary service. `target/<triple>/release`
# also holds deps/, build scripts and incremental output, which is hundreds of
# megabytes of nothing the notary needs.
submission_dir="$RUNNER_TEMP/notarize"
rm -rf "$submission_dir"
mkdir -p "$submission_dir"
for binary in "${binaries[@]}"; do
  cp "${release}/${binary}" "$submission_dir/"
done

submission="$RUNNER_TEMP/notarize.zip"
rm -f "$submission"
/usr/bin/ditto -c -k --keepParent "$submission_dir" "$submission"

xcrun notarytool submit "$submission" \
  --key "$notary_key" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" \
  --wait --timeout 30m
rm -rf "$notary_key" "$submission" "$submission_dir"

# Nothing is stapled, and nothing can be: a ticket staples to a bundle, a disk
# image or an installer, never to a bare executable. Gatekeeper checks these
# ones with the notary service online instead.
echo "Signed and notarized."
