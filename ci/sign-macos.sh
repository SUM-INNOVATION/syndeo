#!/usr/bin/env bash
# Sign and notarize the macOS binaries.
#
#   ci/sign-macos.sh <directory of built binaries>
#
# A no-op, loudly, when the signing secrets are not configured: an unsigned
# build is still a usable one, it just cannot reach the data protection keychain
# and so reports platform presence as unenforced. `syndeo-keystore status` says
# which of the two a user is running.
#
# Secrets this reads, all of them repository secrets:
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

release="${1:?directory of built binaries}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binaries=(syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui)

if [ -z "${CERTIFICATE:-}" ]; then
  echo "No MACOS_CERTIFICATE_P12_BASE64 set: shipping unsigned binaries."
  echo "The keystore will fall back to the ordinary keychain and report"
  echo "platform presence as unenforced, which makes the passphrase mandatory."
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

# The committed entitlements carry $(AppIdentifierPrefix), which is Xcode's
# substitution and means nothing to codesign. The literal team prefix goes in
# its place; getting this wrong produces a binary the kernel kills at launch
# rather than one that merely fails to sign.
entitlements="$RUNNER_TEMP/Syndeo.entitlements"
sed "s/\$(AppIdentifierPrefix)/${TEAM_ID}./" \
  "$root/crates/syndeo-keystore/Syndeo.entitlements" > "$entitlements"

for binary in "${binaries[@]}"; do
  # Only the keystore asks for the keychain access group. Handing the same
  # entitlement to the agent would undo the point of the boundary.
  if [ "$binary" = "syndeo-keystore" ]; then
    codesign --force --timestamp --options runtime \
      --entitlements "$entitlements" \
      --sign "$SIGNING_IDENTITY" "${release}/${binary}"
  else
    codesign --force --timestamp --options runtime \
      --sign "$SIGNING_IDENTITY" "${release}/${binary}"
  fi
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

# Only the six binaries go to the notary service. `target/<triple>/release`
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
