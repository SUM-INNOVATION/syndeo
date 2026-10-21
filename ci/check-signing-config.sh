#!/usr/bin/env bash
# Decide whether a release is signed, and refuse every state in between.
#
#   ci/check-signing-config.sh [--github-output]
#   ci/check-signing-config.sh --stage NAME
#   ci/check-signing-config.sh --self-test
#
# SYNDEO_EXPECT_SIGNED says which: `yes`, or `no`, which is also what empty or
# unset means (an undefined `vars.` value reaches a workflow as empty). Any
# other value is refused.
#
# The eleven signing and notarization secrets, which belong only to the
# release-macos environment, and so only to the jobs that declare it, each of
# which is given only its own share (see --stage below):
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
#   SYNDEO_PROVISIONING_PROFILE_B64         Syndeo.app's Developer ID provisioning
#                                           profile, base64
#
# `no` requires every one of them to be empty: a secret that is set means
# someone expects signing, and a release that silently ignores it is not what
# they asked for. `yes` requires all eleven, none of them blank, the team id
# to be ten capital letters and digits, both identities to be Developer ID
# identities of that team, and the profile a Developer ID provisioning
# profile for that team's com.sum.syndeo that authorizes Apple's browser
# passkey entitlement and has not expired (ci/macos-app/profile.py; that it
# lists the signing certificate is checked where the certificate is, in
# ci/sign-macos.sh). ci/sign-macos.sh signs whatever it is given; this
# runs first, so the half-states it would accept never reach it.
#
# --stage NAME checks one job of the release, given the decision as
# SYNDEO_EXPECT_SIGNED: under yes, that job's own secrets have to be there
# and consistent, and every other one absent, so a job that is handed more
# than it needs fails; under no, all eleven have to be absent. The stages:
#
#   binary-sign      the Application certificate, its password, its
#                    identity, the team id, and Syndeo.app's profile
#   binary-notarize  the notary key, its id, its issuer, and the team id
#   package          the Installer certificate, its password, its identity,
#                    the team id, and the notary key, its id and its issuer
#
# It prints names and its decision, never a value, and refuses to run traced.
# On success it prints `signing: yes` or `signing: no`; with --github-output it
# also appends `signed=yes` or `signed=no` to $GITHUB_OUTPUT.
set -euo pipefail
LC_ALL=C
export LC_ALL
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

NAMES=(
  MACOS_CERTIFICATE_P12_BASE64 MACOS_CERTIFICATE_PASSWORD MACOS_SIGNING_IDENTITY
  MACOS_INSTALLER_CERTIFICATE_P12_BASE64 MACOS_INSTALLER_CERTIFICATE_PASSWORD MACOS_INSTALLER_SIGNING_IDENTITY
  MACOS_TEAM_ID
  MACOS_NOTARY_KEY_BASE64 MACOS_NOTARY_KEY_ID MACOS_NOTARY_ISSUER_ID
  SYNDEO_PROVISIONING_PROFILE_B64
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

# profile_of_team TEAM: SYNDEO_PROVISIONING_PROFILE_B64 is a Developer ID
# provisioning profile for TEAM's com.sum.syndeo that authorizes the browser
# passkey entitlement and has not expired. Refuses, saying what is wrong and
# never what it holds, if it is not.
profile_of_team() {
  local file why
  file="$(mktemp)"
  if ! printf '%s' "$SYNDEO_PROVISIONING_PROFILE_B64" | base64 --decode >"$file" 2>/dev/null; then
    rm -f "$file"
    refuse "SYNDEO_PROVISIONING_PROFILE_B64 is not base64"
  fi
  why="$(python3 -I "$here/macos-app/profile.py" check "$file" "$1" 2>&1)" || {
    rm -f "$file"
    refuse "SYNDEO_PROVISIONING_PROFILE_B64 is not Syndeo.app's profile: $(printf '%s\n' "$why" | sed 's/^profile.py: //' | tr '\n' ' ')"
  }
  rm -f "$file"
}

# stage_names STAGE: the names that stage's job is given.
stage_names() {
  case "$1" in
    binary-sign) echo MACOS_CERTIFICATE_P12_BASE64 MACOS_CERTIFICATE_PASSWORD MACOS_SIGNING_IDENTITY MACOS_TEAM_ID SYNDEO_PROVISIONING_PROFILE_B64 ;;
    binary-notarize) echo MACOS_NOTARY_KEY_BASE64 MACOS_NOTARY_KEY_ID MACOS_NOTARY_ISSUER_ID MACOS_TEAM_ID ;;
    package) echo MACOS_INSTALLER_CERTIFICATE_P12_BASE64 MACOS_INSTALLER_CERTIFICATE_PASSWORD MACOS_INSTALLER_SIGNING_IDENTITY MACOS_TEAM_ID MACOS_NOTARY_KEY_BASE64 MACOS_NOTARY_KEY_ID MACOS_NOTARY_ISSUER_ID ;;
    *) return 1 ;;
  esac
}

# check_stage STAGE: one release job's share of the configuration.
check_stage() {
  case "$-" in *x*) refuse "tracing is on, and would print the secrets" ;; esac
  local stage="$1" own expect n missing='' extra='' set_names=''
  own="$(stage_names "$stage")" || refuse "no stage called $(printf '%q' "$stage")"
  case "${SYNDEO_EXPECT_SIGNED-}" in
    '' | no) expect=no ;;
    yes) expect=yes ;;
    *) refuse "SYNDEO_EXPECT_SIGNED must be yes, no or empty, not $(printf '%q' "$SYNDEO_EXPECT_SIGNED")" ;;
  esac
  for n in "${NAMES[@]}"; do
    [ -z "${!n-}" ] || set_names="$set_names $n"
    case " $own " in
      *" $n "*) [[ "${!n-}" =~ [^[:space:]] ]] || missing="$missing $n" ;;
      *) [ -z "${!n-}" ] || extra="$extra $n" ;;
    esac
  done
  if [ "$expect" = no ]; then
    [ -z "$set_names" ] || refuse "SYNDEO_EXPECT_SIGNED is no, but these are set:$set_names"
  else
    [ -z "$extra" ] || refuse "the $stage stage was given what it does not need:$extra"
    [ -z "$missing" ] || refuse "SYNDEO_EXPECT_SIGNED is yes, but the $stage stage lacks, or has blank:$missing"
    [[ "$MACOS_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] || refuse "MACOS_TEAM_ID is not ten capital letters and digits"
    case " $own " in
      *" MACOS_SIGNING_IDENTITY "*)
        identity_of_team Application "$MACOS_SIGNING_IDENTITY" "$MACOS_TEAM_ID" ||
          refuse "MACOS_SIGNING_IDENTITY is not a Developer ID Application identity of MACOS_TEAM_ID" ;;
    esac
    case " $own " in
      *" MACOS_INSTALLER_SIGNING_IDENTITY "*)
        identity_of_team Installer "$MACOS_INSTALLER_SIGNING_IDENTITY" "$MACOS_TEAM_ID" ||
          refuse "MACOS_INSTALLER_SIGNING_IDENTITY is not a Developer ID Installer identity of MACOS_TEAM_ID" ;;
    esac
    case " $own " in
      *" SYNDEO_PROVISIONING_PROFILE_B64 "*) profile_of_team "$MACOS_TEAM_ID" ;;
    esac
  fi
  printf 'stage %s: signing %s\n' "$stage" "$expect"
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
    profile_of_team "$team"
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
  # Every value carries SECRETVALUE, except the team id, which cannot, and
  # the profile, a stand-in forged with openssl (ci/macos-app/profile.py
  # forge), which has to be one; no output may contain any of them.
  local team=AB12CD34EF profile n
  if ! command -v openssl >/dev/null 2>&1 || ! openssl cms -help >/dev/null 2>&1; then
    printf '  WRONG %s\n\n  1 cases, 1 wrong\n\n' "openssl with cms is needed, to forge a stand-in provisioning profile"
    return 1
  fi
  for n in good other-team expired no-passkeys; do
    case "$n" in
      good) python3 -I "$here/macos-app/profile.py" forge "$T/profile-$n" "$team" ;;
      other-team) python3 -I "$here/macos-app/profile.py" forge "$T/profile-$n" ZZ99ZZ99ZZ ;;
      expired) python3 -I "$here/macos-app/profile.py" forge "$T/profile-$n" "$team" --expired ;;
      no-passkeys) python3 -I "$here/macos-app/profile.py" forge "$T/profile-$n" "$team" --without-passkeys ;;
    esac
  done
  profile="$(base64 <"$T/profile-good" | tr -d '\n')"
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
    "SYNDEO_PROVISIONING_PROFILE_B64=$profile"
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
    if printf '%s' "$out" | grep -qF -- "${profile:0:40}"; then verdict=no; fi
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
  run 1 "these are set: ${NAMES[*]}" "no, with all eleven set: refused" SYNDEO_EXPECT_SIGNED=no "${good[@]}"

  printf '\n  signed: SYNDEO_EXPECT_SIGNED yes, and all eleven consistent\n'
  run 0 "signing: yes" "yes, all eleven: yes" SYNDEO_EXPECT_SIGNED=yes "${good[@]}"
  local bad_profile why
  for bad_profile in "other-team|another team's profile|its team is" "expired|an expired profile|it expired" \
    "no-passkeys|a profile without the passkey entitlement|does not authorize"; do
    IFS='|' read -r n label why <<<"$bad_profile"
    load < <(with SYNDEO_PROVISIONING_PROFILE_B64 "$(base64 <"$T/profile-$n" | tr -d '\n')")
    run 1 "$why" "yes, $label: refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
  done
  load < <(with SYNDEO_PROVISIONING_PROFILE_B64 "U0VDUkVUVkFMVUUtbm90LWEtcHJvZmlsZQ==")
  run 1 "not a CMS-signed property list" "yes, a profile that is not one: refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
  load < <(with SYNDEO_PROVISIONING_PROFILE_B64 "SECRETVALUE!!!")
  run 1 "SYNDEO_PROVISIONING_PROFILE_B64 is not" "yes, a profile that is not base64: refused" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
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
    run 1 "SYNDEO_EXPECT_SIGNED must be yes, no or empty" "$(printf '%q' "$e"), all eleven set: refused" SYNDEO_EXPECT_SIGNED="$e" "${good[@]}"
    run 1 "SYNDEO_EXPECT_SIGNED must be yes, no or empty" "$(printf '%q' "$e"), nothing set: refused" SYNDEO_EXPECT_SIGNED="$e"
  done

  printf '\n  one release job at a time (--stage)\n'
  # stage_run WANT TEXT LABEL STAGE [NAME=VALUE...]: --stage STAGE, with only
  # PATH and the given variables.
  stage_run() {
    local want="$1" text="$2" label="$3" stage="$4" status out verdict=yes
    shift 4
    out="$(env -i PATH="$PATH" "$@" bash "$0" --stage "$stage" 2>&1)" && status=0 || status=$?
    [ "$status" = "$want" ] || verdict=no
    printf '%s' "$out" | grep -qF -- "$text" || verdict=no
    if printf '%s' "$out" | grep -qE "SECRETVALUE|$team\)|U0VDUkVU"; then verdict=no; fi
    if printf '%s' "$out" | grep -qF -- "${profile:0:40}"; then verdict=no; fi
    cases=$((cases + 1))
    if [ "$verdict" = yes ]; then
      printf '  ok    %s\n' "$label"
    else
      printf '  WRONG %s (exit %s: %s)\n' "$label" "$status" "$out"
      wrong=$((wrong + 1))
    fi
  }
  # only NAMES...: the good values of exactly those names.
  only() {
    local v n
    for v in "${good[@]}"; do
      for n in "$@"; do [ "${v%%=*}" != "$n" ] || printf '%s\0' "$v"; done
    done
  }
  local stage names first outsider
  for stage in binary-sign binary-notarize package; do
    names="$(stage_names "$stage")"
    # shellcheck disable=SC2086
    load < <(only $names)
    stage_run 0 "stage $stage: signing yes" "$stage, yes, exactly its own: passes" "$stage" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
    outsider=''
    for n in "${NAMES[@]}"; do case " $names " in *" $n "*) ;; *) outsider="$n"; break ;; esac; done
    stage_run 1 "given what it does not need: $outsider" "$stage, yes, and $outsider too: refused" "$stage" \
      SYNDEO_EXPECT_SIGNED=yes "${env_[@]}" "$outsider=SECRETVALUE"
    for first in $names; do
      # shellcheck disable=SC2086,SC2046 # the names, split on purpose
      load < <(only $(printf '%s\n' $names | grep -vx "$first"))
      stage_run 1 "lacks, or has blank: $first" "$stage, yes, without $first: refused" "$stage" SYNDEO_EXPECT_SIGNED=yes "${env_[@]}"
    done
    stage_run 0 "stage $stage: signing no" "$stage, no, nothing set: passes" "$stage" SYNDEO_EXPECT_SIGNED=no
    first="${names%% *}"
    stage_run 1 "these are set: $first" "$stage, no, with $first set: refused" "$stage" SYNDEO_EXPECT_SIGNED=no "$first=SECRETVALUE"
  done
  # shellcheck disable=SC2046
  load < <(only $(stage_names binary-sign))
  stage_run 1 "MACOS_SIGNING_IDENTITY is not" "binary-sign, an Application identity of another team: refused" binary-sign \
    SYNDEO_EXPECT_SIGNED=yes "${env_[@]}" "MACOS_SIGNING_IDENTITY=Developer ID Application: SECRETVALUE Org (ZZ99ZZ99ZZ)"
  stage_run 1 "its team is" "binary-sign, another team's profile: refused" binary-sign \
    SYNDEO_EXPECT_SIGNED=yes "${env_[@]}" "SYNDEO_PROVISIONING_PROFILE_B64=$(base64 <"$T/profile-other-team" | tr -d '\n')"
  stage_run 1 "it expired" "binary-sign, an expired profile: refused" binary-sign \
    SYNDEO_EXPECT_SIGNED=yes "${env_[@]}" "SYNDEO_PROVISIONING_PROFILE_B64=$(base64 <"$T/profile-expired" | tr -d '\n')"
  # shellcheck disable=SC2046
  load < <(only $(stage_names package))
  stage_run 1 "MACOS_INSTALLER_SIGNING_IDENTITY is not" "package, an Installer identity of another team: refused" package \
    SYNDEO_EXPECT_SIGNED=yes "${env_[@]}" "MACOS_INSTALLER_SIGNING_IDENTITY=Developer ID Application: SECRETVALUE Org ($team)"
  # shellcheck disable=SC2046
  load < <(only $(stage_names binary-notarize))
  stage_run 1 "MACOS_TEAM_ID is not" "binary-notarize, a malformed team id: refused" binary-notarize \
    SYNDEO_EXPECT_SIGNED=yes "${env_[@]}" MACOS_TEAM_ID=ab12cd34ef
  stage_run 1 "no stage called" "an unknown stage: refused" release SYNDEO_EXPECT_SIGNED=no
  stage_run 1 "SYNDEO_EXPECT_SIGNED must be yes, no or empty" "a stage with SYNDEO_EXPECT_SIGNED=YES: refused" package SYNDEO_EXPECT_SIGNED=YES

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
  --stage)
    [ "$#" = 2 ] || { echo "usage: $0 --stage NAME" >&2; exit 2; }
    check_stage "$2"
    ;;
  *) echo "usage: $0 [--github-output] | --stage NAME | --self-test" >&2; exit 2 ;;
esac
