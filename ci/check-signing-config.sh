#!/usr/bin/env bash
# Decide whether a release is signed, and refuse every state in between.
#
#   ci/check-signing-config.sh [--github-output]
#   ci/check-signing-config.sh --self-test
#
# SYNDEO_EXPECT_SIGNED says which: `yes`, or `no`, which is also what empty or
# unset means (an undefined `vars.` value reaches a workflow as empty). Any
# other value is refused.
#
# The ten signing and notarization secrets, which exist only in the protected
# release-macos environment:
#
#   MACOS_CERTIFICATE_P12_BASE64            Developer ID Application, .p12, base64
#   MACOS_CERTIFICATE_PASSWORD              its export password
#   MACOS_SIGNING_IDENTITY                  "Developer ID Application: Name (TEAMID)"
#   MACOS_INSTALLER_CERTIFICATE_P12_BASE64  Developer ID Installer, .p12, base64
#   MACOS_INSTALLER_CERTIFICATE_PASSWORD    its export password
#   MACOS_INSTALLER_SIGNING_IDENTITY        "Developer ID Installer: Name (TEAMID)"
#   MACOS_TEAM_ID                           the ten-character team identifier
#   MACOS_NOTARY_KEY_BASE64                 App Store Connect API key, .p8, base64
#   MACOS_NOTARY_KEY_ID                     that key's id
#   MACOS_NOTARY_ISSUER_ID                  the issuer it belongs to
#
# `no` requires every one of them to be empty: a secret that is set means
# someone expects signing, and a release that silently ignores it is not what
# they asked for. `yes` requires all ten, none of them blank, the team id to be
# ten capital letters and digits, and both identities to be Developer ID
# identities of that team. ci/sign-macos.sh signs whatever it is given; this
# runs first, so the half-states it would accept never reach it.
#
# It prints names and its decision, never a value, and refuses to run traced.
# On success it prints `signing: yes` or `signing: no`; with --github-output it
# also appends `signed=yes` or `signed=no` to $GITHUB_OUTPUT.
set -euo pipefail
LC_ALL=C
export LC_ALL

NAMES=(
  MACOS_CERTIFICATE_P12_BASE64 MACOS_CERTIFICATE_PASSWORD MACOS_SIGNING_IDENTITY
  MACOS_INSTALLER_CERTIFICATE_P12_BASE64 MACOS_INSTALLER_CERTIFICATE_PASSWORD MACOS_INSTALLER_SIGNING_IDENTITY
  MACOS_TEAM_ID
  MACOS_NOTARY_KEY_BASE64 MACOS_NOTARY_KEY_ID MACOS_NOTARY_ISSUER_ID
)

refuse() {
  printf 'check-signing-config: refusing: %s\n' "$*" >&2
  exit 1
}

# identity_of_team KIND VALUE TEAM: VALUE is "Developer ID KIND: <name> (TEAM)",
# on one line.
identity_of_team() {
  local kind="$1" value="$2" team="$3"
  case "$value" in *[[:cntrl:]]*) return 1 ;; esac
  [[ "$value" =~ ^Developer\ ID\ $kind:\ .+\ \(([A-Z0-9]{10})\)$ ]] && [ "${BASH_REMATCH[1]}" = "$team" ]
}

check() {
  case "$-" in *x*) refuse "tracing is on, and would print the secrets" ;; esac
  local expect n set_names='' missing='' team
  case "${SYNDEO_EXPECT_SIGNED-}" in
    '' | no) expect=no ;;
    yes) expect=yes ;;
    *) refuse "SYNDEO_EXPECT_SIGNED must be yes, no or empty, not $(printf '%q' "$SYNDEO_EXPECT_SIGNED")" ;;
  esac
  for n in "${NAMES[@]}"; do
    # Set means anything at all; usable means something besides white space.
    [ -z "${!n-}" ] || set_names="$set_names $n"
    [[ "${!n-}" =~ [^[:space:]] ]] || missing="$missing $n"
  done
  if [ "$expect" = no ]; then
    [ -z "$set_names" ] || refuse "SYNDEO_EXPECT_SIGNED is no, but these are set:$set_names"
  else
    [ -z "$missing" ] || refuse "SYNDEO_EXPECT_SIGNED is yes, but these are empty or blank:$missing"
    team="$MACOS_TEAM_ID"
    [[ "$team" =~ ^[A-Z0-9]{10}$ ]] || refuse "MACOS_TEAM_ID is not ten capital letters and digits"
    identity_of_team Application "$MACOS_SIGNING_IDENTITY" "$team" ||
      refuse "MACOS_SIGNING_IDENTITY is not a Developer ID Application identity of MACOS_TEAM_ID"
    identity_of_team Installer "$MACOS_INSTALLER_SIGNING_IDENTITY" "$team" ||
      refuse "MACOS_INSTALLER_SIGNING_IDENTITY is not a Developer ID Installer identity of MACOS_TEAM_ID"
  fi
  printf 'signing: %s\n' "$expect"
  if [ "${1:-}" = --github-output ]; then
    [ -n "${GITHUB_OUTPUT:-}" ] || refuse "--github-output, but GITHUB_OUTPUT is not set"
    printf 'signed=%s\n' "$expect" >>"$GITHUB_OUTPUT"
  fi
}

# ------------------------------------------------------------------ self-test

self_test() {
  local cases=0 wrong=0 n
  # Not local: the EXIT trap runs after this function has returned.
  T="$(mktemp -d)"
  trap 'rm -rf "$T"' EXIT
  # Every value carries SECRETVALUE, except the team id, which cannot; no
  # output may contain either.
  local team=AB12CD34EF
  local -a good=(
    MACOS_CERTIFICATE_P12_BASE64=U0VDUkVUVkFMVUUtYXBw.SECRETVALUE
    MACOS_CERTIFICATE_PASSWORD=SECRETVALUE-app-password
    "MACOS_SIGNING_IDENTITY=Developer ID Application: SECRETVALUE Org ($team)"
    MACOS_INSTALLER_CERTIFICATE_P12_BASE64=U0VDUkVUVkFMVUUtaW5zdA.SECRETVALUE
    MACOS_INSTALLER_CERTIFICATE_PASSWORD=SECRETVALUE-installer-password
    "MACOS_INSTALLER_SIGNING_IDENTITY=Developer ID Installer: SECRETVALUE Org ($team)"
    "MACOS_TEAM_ID=$team"
    MACOS_NOTARY_KEY_BASE64=U0VDUkVUVkFMVUUta2V5.SECRETVALUE
    MACOS_NOTARY_KEY_ID=SECRETVALUE-key-id
    MACOS_NOTARY_ISSUER_ID=SECRETVALUE-issuer
  )

  # run WANT-STATUS WANT-TEXT LABEL [NAME=VALUE...]: this script, with only
  # PATH and the given variables, exits WANT-STATUS, says WANT-TEXT, and says
  # no value.
  run() {
    local want="$1" text="$2" label="$3" status out verdict=yes
    shift 3
    out="$(env -i PATH="$PATH" "$@" bash "$0" 2>&1)" && status=0 || status=$?
    [ "$status" = "$want" ] || verdict=no
    printf '%s' "$out" | grep -qF -- "$text" || verdict=no
    if printf '%s' "$out" | grep -qE "SECRETVALUE|$team\)|U0VDUkVU"; then verdict=no; fi
    cases=$((cases + 1))
    if [ "$verdict" = yes ]; then
      printf '  ok    %s\n' "$label"
    else
      printf '  WRONG %s (exit %s: %s)\n' "$label" "$status" "$out"
      wrong=$((wrong + 1))
    fi
  }

  # without NAME: the good configuration, less NAME.
  without() {
    local v
    for v in "${good[@]}"; do [ "${v%%=*}" = "$1" ] || printf '%s\0' "$v"; done
  }
  # with NAME VALUE: the good configuration, NAME replaced by VALUE.
  with() {
    local v
    for v in "${good[@]}"; do
      if [ "${v%%=*}" = "$1" ]; then printf '%s=%s\0' "$1" "$2"; else printf '%s\0' "$v"; fi
    done
  }
  local -a env_
  load() { env_=(); while IFS= read -r -d '' v; do env_+=("$v"); done; }

  printf '\n  unsigned: SYNDEO_EXPECT_SIGNED no, empty or unset, and nothing set\n'
  run 0 "signing: no" "unset, nothing set: no"
  run 0 "signing: no" "empty, nothing set: no" SYNDEO_EXPECT_SIGNED=
  run 0 "signing: no" "no, nothing set: no" SYNDEO_EXPECT_SIGNED=no
  for n in "${NAMES[@]}"; do
    run 1 "these are set: $n" "no, with $n set: refused" SYNDEO_EXPECT_SIGNED=no "$n=SECRETVALUE"
    run 1 "these are set: $n" "unset, with $n set: refused" "$n=SECRETVALUE"
    run 1 "these are set: $n" "no, with $n blank: refused" SYNDEO_EXPECT_SIGNED=no "$n= "
  done
  run 1 "these are set: ${NAMES[*]}" "no, with all ten set: refused" SYNDEO_EXPECT_SIGNED=no "${good[@]}"

  printf '\n  signed: SYNDEO_EXPECT_SIGNED yes, and all ten consistent\n'
  run 0 "signing: yes" "yes, all ten: yes" SYNDEO_EXPECT_SIGNED=yes "${good[@]}"
  for n in "${NAMES[@]}"; do
    load < <(without "$n")
    run 1 "empty or blank: $n" "yes, without $n: refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
    load < <(with "$n" "  ")
    run 1 "empty or blank: $n" "yes, with $n blank: refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
  done
  run 1 "empty or blank: ${NAMES[*]}" "yes, nothing set: refused" SYNDEO_EXPECT_SIGNED=yes
  local t
  for t in ab12cd34ef AB12CD34E AB12CD34EF1 "AB12 D34EF" "AB12CD34É"; do
    load < <(with MACOS_TEAM_ID "$t")
    run 1 "MACOS_TEAM_ID is not" "yes, team id $(printf '%q' "$t"): refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
  done
  local id
  for id in "Developer ID Application: SECRETVALUE Org (ZZ99ZZ99ZZ)" \
    "Developer ID Installer: SECRETVALUE Org ($team)" \
    "Apple Development: SECRETVALUE Org ($team)" \
    "Developer ID Application: ($team)" \
    "Developer ID Application: SECRETVALUE Org ($team) " \
    "Developer ID Application: SECRETVALUE Org ($team)"$'\n'"x" \
    "Developer ID Application: SECRETVALUE"$'\n'"Org ($team)"; do
    load < <(with MACOS_SIGNING_IDENTITY "$id")
    run 1 "MACOS_SIGNING_IDENTITY is not" "yes, application identity $(printf '%q' "${id//SECRETVALUE/…}"): refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
  done
  for id in "Developer ID Installer: SECRETVALUE Org (ZZ99ZZ99ZZ)" \
    "Developer ID Application: SECRETVALUE Org ($team)" \
    "3rd Party Mac Developer Installer: SECRETVALUE Org ($team)" \
    "Developer ID Installer: SECRETVALUE"$'\n'"Org ($team)"; do
    load < <(with MACOS_INSTALLER_SIGNING_IDENTITY "$id")
    run 1 "MACOS_INSTALLER_SIGNING_IDENTITY is not" "yes, installer identity $(printf '%q' "${id//SECRETVALUE/…}"): refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
  done

  printf '\n  SYNDEO_EXPECT_SIGNED itself\n'
  local e
  for e in YES Yes true 1 y "yes " " no" $'yes\n' $'no\nyes' maybe; do
    run 1 "SYNDEO_EXPECT_SIGNED must be yes, no or empty" "$(printf '%q' "$e"), all ten set: refused" SYNDEO_EXPECT_SIGNED="$e" "${good[@]}"
    run 1 "SYNDEO_EXPECT_SIGNED must be yes, no or empty" "$(printf '%q' "$e"), nothing set: refused" SYNDEO_EXPECT_SIGNED="$e"
  done

  printf '\n  how it runs\n'
  local out status
  out="$(env -i PATH="$PATH" SYNDEO_EXPECT_SIGNED=yes "${good[@]}" bash -x "$0" 2>&1)" && status=0 || status=$?
  cases=$((cases + 1))
  if [ "$status" = 1 ] && printf '%s' "$out" | grep -qF "tracing is on" && ! printf '%s' "$out" | grep -qE 'SECRETVALUE|U0VDUkVU'; then
    printf '  ok    %s\n' "traced: refused, before any value is read"
  else
    printf '  WRONG %s (exit %s)\n' "traced: refused, before any value is read" "$status"
    wrong=$((wrong + 1))
  fi
  : >"$T/out-no"
  env -i PATH="$PATH" GITHUB_OUTPUT="$T/out-no" bash "$0" --github-output >/dev/null 2>&1 || true
  : >"$T/out-yes"
  env -i PATH="$PATH" GITHUB_OUTPUT="$T/out-yes" SYNDEO_EXPECT_SIGNED=yes "${good[@]}" bash "$0" --github-output >/dev/null 2>&1 || true
  cases=$((cases + 1))
  if [ "$(cat "$T/out-no")" = "signed=no" ] && [ "$(cat "$T/out-yes")" = "signed=yes" ]; then
    printf '  ok    %s\n' "--github-output writes signed=no or signed=yes, and nothing else"
  else
    printf '  WRONG %s\n' "--github-output writes signed=no or signed=yes, and nothing else"
    wrong=$((wrong + 1))
  fi
  out="$(env -i PATH="$PATH" SYNDEO_EXPECT_SIGNED=no bash "$0" --github-output 2>&1)" && status=0 || status=$?
  cases=$((cases + 1))
  if [ "$status" = 1 ] && printf '%s' "$out" | grep -qF "GITHUB_OUTPUT is not set"; then
    printf '  ok    %s\n' "--github-output without GITHUB_OUTPUT: refused"
  else
    printf '  WRONG %s (exit %s)\n' "--github-output without GITHUB_OUTPUT: refused" "$status"
    wrong=$((wrong + 1))
  fi
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test ;;
  '' | --github-output) check "${1:-}" ;;
  *) echo "usage: $0 [--github-output] | --self-test" >&2; exit 2 ;;
esac
