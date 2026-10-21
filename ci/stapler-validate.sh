#!/usr/bin/env bash
# Validating a stapled notarization ticket, in up to five attempts. Read only.
#
#   . ci/stapler-validate.sh       then   validated_ticket PATH [LOG]
#   bash ci/stapler-validate.sh --self-test
#
# `xcrun stapler validate` looks the ticket up with Apple over the network, and
# a connection lost on the way fails it. The signed dry run of 0.2.0, run
# 39520714210, failed that way: its test validated the installed Syndeo.app
# once. Validating reads, and changes nothing, so it is tried again:
# - up to five attempts, after 0, 15, 30, 60 and 120 seconds;
# - each command stopped at 60 seconds, which then counts as that attempt
#   failing (exit 124). SYNDEO_STAPLER_LIMIT_SECONDS may make that shorter, 1
#   to 60, and nothing makes it longer;
# - so at most 225 seconds of waiting and five commands of about 61 seconds
#   each: under 9 minutes.
#
# Every attempt is printed as it happens, indented: its exact command, its
# complete output and its exit status. If LOG is given, each attempt is
# appended to it too.
#
# An interruption while it runs (INT, TERM or HUP) stops the stapler command
# it is waiting on, and exits 130. Otherwise it puts back the caller's
# handling of those signals as it was. It returns:
# - 0 for the first attempt that validates;
# - 1 after five that did not;
# - 2, with nothing run, for a limit it refuses.

# _validate_stop: the stapler command running, if any, stopped: TERM, then
# KILL a second later, and reaped. Every step may find it gone already, and
# none of them may stop a caller running under set -e, not even from the
# interrupt handler, before it exits 130.
_validate_stop() {
  [ -n "${_validate_child:-}" ] || return 0
  if kill -0 "$_validate_child" 2>/dev/null; then
    kill -TERM "$_validate_child" 2>/dev/null || true
    /bin/sleep 1
    kill -KILL "$_validate_child" 2>/dev/null || true
  fi
  wait "$_validate_child" 2>/dev/null || true
  _validate_child=''
}

validated_ticket() {
  local path="$1" log="${2:-}" limit delay attempt=0 status tenths saved out output block
  limit="${SYNDEO_STAPLER_LIMIT_SECONDS:-60}"
  if ! [[ "$limit" =~ ^[1-9][0-9]?$ ]] || [ "$limit" -gt 60 ]; then
    printf 'stapler-validate: refusing: SYNDEO_STAPLER_LIMIT_SECONDS is not a whole number of seconds from 1 to 60\n' >&2
    return 2
  fi
  out="$(mktemp "${TMPDIR:-/tmp}/stapler-validate.XXXXXX")" || return 2
  saved="$(trap -p INT TERM HUP)"
  _validate_child=''
  trap '_validate_stop; rm -f "$out"; exit 130' INT TERM HUP
  for delay in 0 15 30 60 120; do
    attempt=$((attempt + 1))
    if [ "$delay" != 0 ]; then
      printf '      the ticket did not validate; validating again in %ss (attempt %s of 5)\n' "$delay" "$attempt"
      sleep "$delay"
    fi
    xcrun stapler validate "$path" >"$out" 2>&1 &
    _validate_child=$!
    status=''
    tenths=0
    while kill -0 "$_validate_child" 2>/dev/null; do
      if [ "$tenths" -ge $((limit * 10)) ]; then
        _validate_stop
        status=124
        printf 'stopped: still running after %s seconds\n' "$limit" >>"$out"
        break
      fi
      /bin/sleep 0.1
      tenths=$((tenths + 1))
    done
    if [ -z "$status" ]; then
      wait "$_validate_child" && status=0 || status=$?
    fi
    _validate_child=''
    output="$(cat "$out")"
    block="$(printf '$ xcrun stapler validate %s\n%s[exit %s] (attempt %s of 5)' "$path" "${output:+$output$'\n'}" "$status" "$attempt")"
    printf '%s\n' "$block" | sed 's/^/      /'
    [ -z "$log" ] || printf '%s\n' "$block" >>"$log"
    [ "$status" != 0 ] || break
  done
  rm -f "$out"
  trap - INT TERM HUP
  eval "$saved"
  [ "$status" = 0 ]
}

# ------------------------------------------------------------------ self-test

self_test() {
  local cases=0 wrong=0 here T
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  T="$(mktemp -d)"
  # shellcheck disable=SC2064 # T is fixed now
  trap "rm -rf '$T'" EXIT
  mkdir -p "$T/bin"
  # xcrun: stapler validate, as STANDIN_RESULTS says, attempt by attempt,
  # comma-separated, the last one repeated: ok, lost (the connection to Apple
  # lost, exit 68), hang (never finishing by itself), interrupt (the caller
  # sent TERM, then hanging). Anything else is logged and fails.
  cat >"$T/bin/xcrun" <<'EOF'
#!/bin/bash
[ "$1 $2" = "stapler validate" ] || { echo "xcrun $1 $2" >>"$STANDIN_CALLS"; exit 1; }
echo "validate" >>"$STANDIN_CALLS"
echo "$$" >"$STANDIN_CALLS.pid"
n=$(( $(cat "$STANDIN_CALLS.n" 2>/dev/null || echo 0) + 1 ))
echo "$n" >"$STANDIN_CALLS.n"
IFS=, read -ra results <<<"${STANDIN_RESULTS:-ok}"
r="${results[$((n - 1))]:-${results[${#results[@]} - 1]}}"
echo "Processing: $3"
case "$r" in
  ok) echo "The validate action worked!" ;;
  lost)
    echo 'error is Error Domain=NSURLErrorDomain Code=-1005 "The network connection was lost." (stand-in)'
    echo "The validate action failed! Error 68."
    exit 68 ;;
  hang) exec /bin/sleep 30 ;;
  interrupt) kill -TERM "$STANDIN_CALLER"; exec /bin/sleep 30 ;;
esac
EOF
  # sleep: logged, and over at once; or, with STANDIN_SLEEP_INTERRUPT=yes,
  # the caller sent TERM while it waits.
  cat >"$T/bin/sleep" <<'EOF'
#!/bin/bash
echo "sleep $*" >>"$STANDIN_CALLS"
[ "${STANDIN_SLEEP_INTERRUPT:-no}" != yes ] || { kill -TERM "$STANDIN_CALLER"; /bin/sleep 1; }
EOF
  chmod 755 "$T/bin/xcrun" "$T/bin/sleep"
  # The caller: sources this file, with handlers of its own for INT and TERM,
  # validates, says what it returned, and that its handlers are back.
  cat >"$T/caller.sh" <<'EOF'
#!/bin/bash
set -euo pipefail
. "$1"
export STANDIN_CALLER=$$
trap 'echo "the caller handled INT"' INT
trap 'echo "the caller handled TERM"' TERM
before="$(trap -p INT TERM HUP)"
status=0
validated_ticket "$2" ${3:+"$3"} || status=$?
echo "returned $status"
[ "$(trap -p INT TERM HUP)" = "$before" ] && echo "handlers as before" || echo "handlers changed"
EOF
  printf 'a stapled ticket\n' >"$T/Syndeo-0.0.1.pkg"
  chmod 644 "$T/Syndeo-0.0.1.pkg"

  # beside: everything beside the package, with its mode, size and time,
  # but the case's own directory. ls, not stat, whose options macOS and
  # Linux spell differently.
  beside() { LC_ALL=C ls -lnA "$T" | awk '$NF != "run"'; }

  # vcase LABEL WANT-EXIT WANT-CALLS WANT-TEXT ATTEMPTS-LOGGED [NAME=VALUE...]:
  # the caller exits WANT-EXIT, the stand-ins are called exactly WANT-CALLS,
  # the output says each of WANT-TEXT's |-separated parts, the log holds
  # ATTEMPTS-LOGGED attempts, the package is unchanged and nothing is left
  # beside it, and no stapler command is left running.
  vcase() {
    local label="$1" want="$2" calls="$3" text="$4" logged="$5" status out verdict=yes got t
    shift 5
    rm -rf "$T/run"
    mkdir -p "$T/run/tmp"
    : >"$T/run/calls"
    local listing sum
    listing="$(beside)"
    sum="$(shasum -a 256 "$T/Syndeo-0.0.1.pkg")"
    out="$(env -i PATH="$T/bin:/usr/bin:/bin" TMPDIR="$T/run/tmp" STANDIN_CALLS="$T/run/calls" "$@" \
      bash "$T/caller.sh" "$here/stapler-validate.sh" "$T/Syndeo-0.0.1.pkg" "$T/run/validate.log" 2>&1)" && status=0 || status=$?
    got="$(paste -sd, - <"$T/run/calls")"
    [ "$status" = "$want" ] || verdict=no
    [ "$got" = "$calls" ] || verdict=no
    while IFS= read -r -d '|' t; do [ -z "$t" ] || printf '%s' "$out" | grep -qF -- "$t" || verdict=no; done <<<"$text|"
    local n=0
    [ ! -f "$T/run/validate.log" ] || n="$(grep -cE '^\[exit [0-9]+\] \(attempt [1-5] of 5\)$' "$T/run/validate.log" || true)"
    [ "$n" = "$logged" ] || verdict=no
    [ "$(shasum -a 256 "$T/Syndeo-0.0.1.pkg")" = "$sum" ] && [ "$(beside)" = "$listing" ] || verdict=no
    [ -z "$(ls -A "$T/run/tmp")" ] || verdict=no
    if [ -s "$T/run/calls.pid" ] && kill -0 "$(cat "$T/run/calls.pid")" 2>/dev/null; then
      kill -KILL "$(cat "$T/run/calls.pid")" 2>/dev/null
      verdict=no
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
  local all="validate,sleep 15,validate,sleep 30,validate,sleep 60,validate,sleep 120,validate"

  printf '\n  validated_ticket: read only, up to five attempts, each stopped at its limit\n'
  vcase "the ticket validates at once: one attempt, shown and logged" \
    0 "validate" "\$ xcrun stapler validate |The validate action worked!|[exit 0] (attempt 1 of 5)|returned 0|handlers as before" 1
  vcase "the connection lost once, then validating: the second attempt, after 15 s" \
    0 "validate,sleep 15,validate" "Error Domain=NSURLErrorDomain Code=-1005|[exit 68] (attempt 1 of 5)|validating again in 15s (attempt 2 of 5)|[exit 0] (attempt 2 of 5)|returned 0" 2 \
    STANDIN_RESULTS=lost,ok
  vcase "the connection lost four times, validating the fifth: after 15, 30, 60 and 120 s" \
    0 "$all" "validating again in 120s (attempt 5 of 5)|[exit 0] (attempt 5 of 5)|returned 0|handlers as before" 5 \
    STANDIN_RESULTS=lost,lost,lost,lost,ok
  vcase "the connection lost every time: five attempts, each shown and logged, then it fails" \
    0 "$all" "[exit 68] (attempt 5 of 5)|returned 1|handlers as before" 5 STANDIN_RESULTS=lost
  vcase "stapler running past its limit: stopped, exit 124, then the next attempt" \
    0 "validate,sleep 15,validate" "stopped: still running after 1 seconds|[exit 124] (attempt 1 of 5)|[exit 0] (attempt 2 of 5)|returned 0" 2 \
    STANDIN_RESULTS=hang,ok SYNDEO_STAPLER_LIMIT_SECONDS=1
  vcase "a limit past 60 seconds: refused, nothing run" \
    0 "" "SYNDEO_STAPLER_LIMIT_SECONDS is not a whole number of seconds from 1 to 60|returned 2|handlers as before" 0 SYNDEO_STAPLER_LIMIT_SECONDS=61
  vcase "interrupted while validating: exit 130, the stapler command stopped, nothing more run" \
    130 "validate" "" 0 STANDIN_RESULTS=interrupt
  vcase "interrupted while waiting to try again: exit 130, nothing more run" \
    130 "validate,sleep 15" "[exit 68] (attempt 1 of 5)" 1 STANDIN_RESULTS=lost STANDIN_SLEEP_INTERRUPT=yes

  # Where it is used: the five validations of the tests and of verify-pkg,
  # each through validated_ticket, and none directly.
  local f direct uses
  cases=$((cases + 1))
  direct="$(grep -nF 'xcrun stapler validate' "$here/test-pkg-install.sh" "$here/test-journey.sh" "$here/verify-pkg.sh" || true)"
  uses="$(for f in test-pkg-install.sh:1 test-journey.sh:2 verify-pkg.sh:2; do
    [ "$(grep -cE '^[^#]*validated_ticket "' "$here/${f%%:*}")" = "${f##*:}" ] || echo "${f%%:*}"
  done)"
  if [ -z "$direct" ] && [ -z "$uses" ] && grep -qF 'stapler-validate.sh' "$here/test-pkg-install.sh" && grep -qF 'stapler-validate.sh' "$here/verify-pkg.sh"; then
    printf '  ok    %s\n' "all five validations go through validated_ticket: test-pkg-install 1, test-journey 2, verify-pkg 2; none directly"
  else
    printf '  WRONG %s\n          | direct: %s; counts wrong in: %s\n' "all five validations go through validated_ticket" "${direct:-none}" "${uses:-none}"
    wrong=$((wrong + 1))
  fi
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  case "${1:-}" in
    --self-test) self_test ;;
    *) printf 'usage: . %s, then validated_ticket PATH [LOG] | bash %s --self-test\n' "$0" "$0" >&2; exit 2 ;;
  esac
fi
