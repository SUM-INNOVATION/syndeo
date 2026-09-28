#!/usr/bin/env bash
# Check a published release the way somebody receiving it would.
#
#   ci/verify-release.sh [version]
#   ci/verify-release.sh --self-test
#
# Installs from the release with the same one-liner the README gives, into a
# throwaway directory, and then asks the installed binaries to demonstrate the
# things that have broken before. Every check here exists because something it
# covers shipped broken once:
#
#   - cache statistics vanished when the network process was killed rather than
#     asked to stop, so a hit was served and never recorded
#   - the proxy forwarded connection headers into HTTP/2 and every Google host
#     answered 502
#   - binaries were installed into separate directories and could not find each
#     other
#   - the headline renderer, syndeo-webkit, was in the tarball and never
#     installed, while syndeo-servo was in the tarball and should not have been
#
# What the release must contain is decided by the version asked for, and only
# by that: the installer is the thing under test, so what it says about itself
# is checked, never believed. With no version given, the installed syndeo's own
# report is used instead.
#
# Knobs, both optional:
#
#   SYNDEO_EXPECT_SIGNED  yes | no (default: no). On macOS, whether the binaries
#                         must carry a valid Developer ID signature and be
#                         accepted by Gatekeeper, or carry none and be rejected.
#                         The check fails when the release is not what this
#                         says, in either direction, and when it is half of each.
#   SYNDEO_INSTALLER      the install.sh to run: a URL or a local path (default:
#                         main's, from GitHub, as the README gives it). A path is
#                         how a change to the installer is checked before it
#                         reaches main.
#
# --self-test checks this script's own judgement against stand-ins, with no
# network and no real installer, codesign, spctl, uname or curl.
#
# Exits non-zero if any check fails, so it can gate a release rather than
# decorate one.
set -uo pipefail

REPO="SUM-INNOVATION/syndeo"
CORE="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui"
# The version syndeo-webkit is installed from, on macOS. Kept the same as
# install.sh's WEBKIT_SINCE.
WEBKIT_SINCE="0.1.3"

pass=0; fail=0; failures=""
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; pass=$((pass+1)); }
bad()  {
  printf '  \033[31mFAIL\033[0m  %s — %s\n' "$1" "$2"
  fail=$((fail+1))
  failures="${failures}${1} — ${2}"$'\n'
}
note() { printf '\n  %s\n' "$1"; }
report() { printf '\n  %d passed, %d failed\n\n' "$pass" "$fail"; }

# The same comparison install.sh makes: major.minor.patch as numbers, anything
# after `-` or `+` ignored, and anything malformed never at least anything.
version_at_least() {
  local have="${1%%[-+]*}" want="$2" rest
  case "$have" in
    *[!0-9.]* | .* | *. | *..*) return 1 ;;
  esac
  case "$have" in
    *.*.*.*) return 1 ;;
    *.*.*) ;;
    *) return 1 ;;
  esac
  local have_major="${have%%.*}"; rest="${have#*.}"
  local have_minor="${rest%%.*}" have_patch="${rest#*.}"
  local want_major="${want%%.*}"; rest="${want#*.}"
  local want_minor="${rest%%.*}" want_patch="${rest#*.}"
  if [ "$have_major" -ne "$want_major" ]; then [ "$have_major" -gt "$want_major" ]; return; fi
  if [ "$have_minor" -ne "$want_minor" ]; then [ "$have_minor" -gt "$want_minor" ]; return; fi
  [ "$have_patch" -ge "$want_patch" ]
}

well_formed_version() {
  version_at_least "$1" 0.0.0
}

# Run the installer into $BIN. Sets $banner to the version it said it
# installed, which is kept for comparison and never trusted.
install_release() {
  note "installing from the published release, as a user would"
  local command
  case "$INSTALLER" in
    http://* | https://*) command="curl -fsSL '$INSTALLER' | sh" ;;
    *) command="sh '$INSTALLER'" ;;
  esac
  if ! SYNDEO_INSTALL_DIR="$BIN" SYNDEO_HOME="$HOME_DIR" SYNDEO_VERSION="$VERSION" \
       sh -c "$command" >"$WORK/install.log" 2>&1; then
    bad "install.sh completes" "$(tail -3 "$WORK/install.log" | tr '\n' ' ')"
    return 1
  fi
  ok "install.sh completes, verifying the checksum"
  banner=$(sed -n 's/^Syndeo \([^ ]*\) for .*/\1/p' "$WORK/install.log" | head -1)
}

# Decide which release this is meant to be: the version asked for, or else
# the one the installed syndeo reports. Sets $expected and $expected_ok.
resolve_version() {
  local source
  expected=""; expected_ok=no
  if [ -n "$VERSION" ]; then
    expected="${VERSION#v}"
    source="the version asked for"
  elif [ -x "$BIN/syndeo" ]; then
    expected=$("$BIN/syndeo" --version 2>/dev/null | awk '{print $2}')
    source="what the installed syndeo reports"
  else
    source="nothing: no version was asked for and syndeo is not installed"
  fi
  if [ -n "$expected" ] && well_formed_version "$expected"; then
    expected_ok=yes
    ok "judging the release as ${expected}, from ${source}"
  else
    bad "release version" "no valid version to judge against (${expected:-none}, from ${source})"
  fi
}

check_artifacts() {
  local os missing b reported
  os="$(uname -s)"
  note "what it installed (${expected:-unknown version}, $os)"

  missing=""
  for b in $CORE; do
    [ -x "$BIN/$b" ] || missing="$missing $b"
  done
  if [ -z "$missing" ]; then
    ok "every core binary is present and executable"
  else
    bad "binaries present" "missing:$missing"
  fi

  if [ -e "$BIN/syndeo-servo" ]; then
    bad "syndeo-servo is not installed" "it is at $BIN/syndeo-servo"
  else
    ok "syndeo-servo is not installed"
  fi

  if [ "$expected_ok" != yes ]; then
    bad "syndeo-webkit expectation" "cannot be decided without a valid release version"
  elif [ "$os" = Darwin ] && version_at_least "$expected" "$WEBKIT_SINCE"; then
    if [ -x "$BIN/syndeo-webkit" ] && [ -x "$BIN/syndeo-proxy" ]; then
      ok "syndeo-webkit is installed beside syndeo-proxy"
    else
      bad "syndeo-webkit beside syndeo-proxy" "not both executable in $BIN"
    fi
    if "$BIN/syndeo-webkit" --help >/dev/null 2>&1; then
      ok "syndeo-webkit --help answers"
    else
      bad "syndeo-webkit --help" "exited non-zero"
    fi
  elif [ -e "$BIN/syndeo-webkit" ]; then
    bad "syndeo-webkit is not installed" "$expected on $os does not include it"
  else
    ok "syndeo-webkit is not installed, as $expected on $os expects"
  fi

  # The installer's own account, checked against the release it was asked for.
  if [ "$expected_ok" = yes ]; then
    if [ "$banner" = "$expected" ]; then
      ok "the installer says it installed $expected"
    else
      bad "installer banner" "it said ${banner:-nothing}, the release is $expected"
    fi
  fi

  reported=$("$BIN/syndeo" --version 2>/dev/null | awk '{print $2}')
  if [ "$expected_ok" = yes ] && [ "$reported" = "$expected" ]; then
    ok "syndeo reports version $expected"
  elif [ "$expected_ok" = yes ]; then
    bad "version" "syndeo reports ${reported:-nothing}, the release is $expected"
  fi
}

# Every executable the installer put in $BIN, assessed once each by codesign and
# Gatekeeper before anything is compared with what was expected.
check_signing() {
  local executables="" f b valid="" invalid="" unsigned="" accepted="" rejected=""
  note "signing (expected: $([ "$EXPECT_SIGNED" = yes ] && echo "Developer ID signed and notarized" || echo "unsigned"))"

  for f in "$BIN"/*; do
    if [ -f "$f" ] && [ -x "$f" ]; then
      executables="$executables ${f##*/}"
    fi
  done
  if [ -z "$executables" ]; then
    bad "signing" "no executables were installed to assess"
    return
  fi
  if ! spctl --status 2>&1 | grep -q 'assessments enabled'; then
    bad "Gatekeeper" "assessments are disabled on this machine; the signing check cannot run"
    return
  fi

  for b in $executables; do
    if codesign -dv --verbose=2 "$BIN/$b" 2>&1 | grep -q '^Authority=Developer ID Application'; then
      if codesign --verify --strict "$BIN/$b" >/dev/null 2>&1; then
        valid="$valid $b"
      else
        invalid="$invalid $b"
      fi
    else
      unsigned="$unsigned $b"
    fi
    if spctl -a -t execute "$BIN/$b" >/dev/null 2>&1; then
      accepted="$accepted $b"
    else
      rejected="$rejected $b"
    fi
  done
  printf '        assessed:%s\n' "$executables"
  printf '        Developer ID, valid:%s\n' "${valid:- none}"
  printf '        Developer ID, invalid:%s\n' "${invalid:- none}"
  printf '        no Developer ID:%s\n' "${unsigned:- none}"
  printf '        Gatekeeper accepts:%s\n' "${accepted:- none}"
  printf '        Gatekeeper rejects:%s\n' "${rejected:- none}"

  if [ "$EXPECT_SIGNED" = yes ]; then
    if [ -z "$invalid" ] && [ -z "$unsigned" ]; then
      ok "every executable carries a valid Developer ID signature"
    else
      bad "Developer ID signature" "invalid:${invalid:- none}; missing:${unsigned:- none}"
    fi
    if [ -z "$rejected" ]; then
      ok "Gatekeeper accepts every executable"
    else
      bad "Gatekeeper" "rejects:$rejected"
    fi
  else
    if [ -z "$valid" ] && [ -z "$invalid" ]; then
      ok "no executable carries a Developer ID signature, as expected for an unsigned release"
    else
      bad "unsigned release" "Developer ID signature found on:${valid}${invalid} (set SYNDEO_EXPECT_SIGNED=yes if that is intended)"
    fi
    # An unsigned release has no Developer ID and no notarization, so
    # Gatekeeper refusing it is the correct answer. One it accepts means the
    # assessment is being overridden here, and says nothing about what users get.
    if [ -z "$accepted" ]; then
      ok "Gatekeeper rejects every executable, as it should an unsigned release"
    else
      bad "Gatekeeper" "accepts unsigned executables:$accepted"
    fi
  fi
}

# What `syndeo doctor` says about the installed release, for a home with no
# keys in it: the only kind this script makes, since making keys would write
# the machine's credential store. Its stderr is kept, because when the
# keystore cannot start that is where the reason is.
check_doctor() {
  local os doctor missing
  os="$(uname -s)"
  note "the process model"
  doctor=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" doctor 2>"$WORK/doctor.err")
  if echo "$doctor" | grep -q 'NOT FOUND'; then
    bad "the shell finds its sibling processes" "$(echo "$doctor" | grep 'NOT FOUND' | tr '\n' ' ')"
  else
    ok "the shell finds its sibling processes"
  fi
  if echo "$doctor" | grep -q 'keystore .*initialized'; then
    ok "the keystore starts and answers"
  else
    bad "the keystore starts" "$(echo "$doctor" | grep -i keystore | head -1 | tr -s ' ')"
    if [ -s "$WORK/doctor.err" ]; then
      printf '        doctor said on stderr:\n'
      tail -n 5 "$WORK/doctor.err" | sed 's/^/        | /'
    fi
    # Which libraries the loader cannot find, for the reader: never a verdict,
    # since what ldd prints differs from one libc to another.
    if [ "$os" = Linux ] && command -v ldd >/dev/null 2>&1; then
      missing=$(ldd "$BIN/syndeo-keystore" 2>&1 | grep 'not found')
      printf '        ldd syndeo-keystore: %s\n' "${missing:-every library found}"
    fi
  fi

  # The boundaries are the keystore's own account of this session. 0.1.3
  # printed a fixed list whatever the platform or session could do.
  if echo "$doctor" | grep -q 'the seed is forgotten on idleness, on sleep, and on screen lock'; then
    bad "doctor states facts, not a fixed list" "it prints 0.1.3's fixed seed-retention claim"
  elif echo "$doctor" | grep -q 'no seed exists: the keystore is not initialized'; then
    ok "doctor says no seed exists, for a home with no keys"
  else
    bad "doctor states facts, not a fixed list" "no seed-retention line for an uninitialized keystore"
  fi
  if [ "$os" = Linux ]; then
    if echo "$doctor" | grep -q 'on screen lock'; then
      bad "no screen-lock claim on Linux" "$(echo "$doctor" | grep 'on screen lock' | head -1 | sed 's/^ *//')"
    else
      ok "doctor claims nothing about screen lock on Linux"
    fi
  fi
  if [ "$os" = Darwin ]; then
    if echo "$doctor" | grep -q 'syndeo-webkit is outside that' \
       && echo "$doctor" | grep -q 'localhost and loopback'; then
      ok "doctor says WebKit is outside the net process, and names the loopback bypass"
    else
      bad "the WebKit line" "doctor does not say WebKit is outside the net process, with its loopback bypass"
    fi
  fi
}

check_runtime() {
  local out stats proxy_pid host code
  check_doctor

  note "fetching, and the cache"
  out=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" browse https://www.rust-lang.org/ --twice 2>/dev/null)
  if echo "$out" | grep -qE '^  200'; then
    ok "fetches a page over the network"
  else
    bad "fetch" "no 200 in the output"
  fi
  if echo "$out" | grep -q 'again   cache'; then
    ok "the second fetch is served from cache"
  else
    bad "cache hit" "the second fetch was not a hit"
  fi

  # The regression that nearly shipped: the hit is served and then forgotten,
  # because the process holding the cache is killed before it can write.
  stats=$(SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo" stats 2>/dev/null | head -1)
  if echo "$stats" | grep -qE 'hits [1-9]'; then
    ok "the hit survives the process that served it"
  else
    bad "statistics persist" "$stats"
  fi

  note "the proxy, on hosts that have broken it before"
  SYNDEO_HOME="$HOME_DIR" "$BIN/syndeo-proxy" run >"$WORK/proxy.log" 2>&1 &
  proxy_pid=$!
  sleep 4
  CA="$HOME_DIR/proxy/syndeo-ca.pem"
  for host in https://www.google.com/ https://www.youtube.com/ https://github.com/; do
    code=$(curl -s --cacert "$CA" -x http://127.0.0.1:8899 -o /dev/null --max-time 30 -w '%{http_code}' "$host" 2>/dev/null)
    if [ "$code" = "200" ]; then
      ok "proxy serves $host"
    else
      bad "proxy $host" "HTTP ${code:-no answer}"
    fi
  done
  kill "$proxy_pid" 2>/dev/null
  # Reaped here, so the shell does not report the kill it was asked for.
  wait "$proxy_pid" 2>/dev/null

  note "privacy defaults"
  if "$BIN/syndeo-net" --help 2>&1 | grep -q 'doh:cloudflare'; then
    ok "DNS resolves over HTTPS by default"
  else
    bad "DoH default" "not the default"
  fi
  if "$BIN/syndeo-net" --help 2>&1 | grep -q 'unpartitioned-cache'; then
    ok "the cache is partitioned, with a documented opt-out"
  else
    bad "partitioning" "no opt-out flag"
  fi
}

main() {
  VERSION="${1:-}"
  EXPECT_SIGNED="${SYNDEO_EXPECT_SIGNED:-no}"
  INSTALLER="${SYNDEO_INSTALLER:-https://raw.githubusercontent.com/${REPO}/main/install.sh}"
  case "$EXPECT_SIGNED" in
    yes | no) ;;
    *) echo "SYNDEO_EXPECT_SIGNED must be yes or no, not '$EXPECT_SIGNED'" >&2; exit 2 ;;
  esac

  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  BIN="$WORK/bin"
  HOME_DIR="$WORK/home"
  banner=""

  if ! install_release; then
    report; exit 1
  fi
  resolve_version
  check_artifacts
  if [ "$(uname -s)" = Darwin ]; then
    check_signing
  fi
  check_runtime
  report
  [ "$fail" -eq 0 ]
}

# ------------------------------------------------------------------ self-test

# Stand-ins for everything the checks above ask the system, each writing every
# call it receives to $FAKE_LOG. Nothing here reaches the network or the real
# codesign, spctl, uname or curl.
write_stand_ins() {
  local dir="$1"
  cat >"$dir/uname" <<'EOF'
#!/bin/sh
echo "uname $*" >>"$FAKE_LOG"
case "$1" in -s) echo "$FAKE_OS" ;; -m) echo arm64 ;; *) echo "$FAKE_OS" ;; esac
EOF
  cat >"$dir/codesign" <<'EOF'
#!/bin/sh
echo "codesign $*" >>"$FAKE_LOG"
for last; do :; done
name="${last##*/}"
case "$1" in
  -dv)
    case " $FAKE_SIGNED $FAKE_INVALID " in
      *" $name "*) echo "Authority=Developer ID Application: Example (AB12CD34EF)" >&2 ;;
      *) echo "Signature=adhoc" >&2 ;;
    esac ;;
  --verify)
    case " $FAKE_INVALID " in *" $name "*) exit 1 ;; esac ;;
esac
exit 0
EOF
  cat >"$dir/spctl" <<'EOF'
#!/bin/sh
echo "spctl $*" >>"$FAKE_LOG"
if [ "$1" = --status ]; then
  echo "assessments $FAKE_GATEKEEPER"
  exit 0
fi
for last; do :; done
case " $FAKE_ACCEPTED " in *" ${last##*/} "*) exit 0 ;; esac
exit 3
EOF
  cat >"$dir/curl" <<'EOF'
#!/bin/sh
echo "curl $*" >>"$FAKE_LOG"
exit 97
EOF
  # The installer under test in each case: says $FAKE_BANNER, and installs
  # $FAKE_INSTALL, with a syndeo that reports $FAKE_REPORTS.
  cat >"$dir/installer" <<'EOF'
#!/bin/sh
echo "Syndeo $FAKE_BANNER for fake-target"
mkdir -p "$SYNDEO_INSTALL_DIR"
for b in $FAKE_INSTALL; do
  if [ "$b" = syndeo ]; then
    printf '#!/bin/sh\necho "syndeo %s"\n' "$FAKE_REPORTS" >"$SYNDEO_INSTALL_DIR/$b"
  else
    printf '#!/bin/sh\nexit 0\n' >"$SYNDEO_INSTALL_DIR/$b"
  fi
  chmod 755 "$SYNDEO_INSTALL_DIR/$b"
done
EOF
  cat >"$dir/ldd" <<'EOF'
#!/bin/sh
echo "ldd $*" >>"$FAKE_LOG"
echo "	libdbus-1.so.3 => not found"
echo "	libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6"
EOF
  chmod 755 "$dir/uname" "$dir/codesign" "$dir/spctl" "$dir/curl" "$dir/installer" "$dir/ldd"
}

# A doctor that says what $FAKE_DOCTOR holds on stdout and $FAKE_DOCTOR_ERR on
# stderr, installed as <dir>/syndeo.
fake_doctor() {
  mkdir -p "$1"
  cat >"$1/syndeo" <<'EOF'
#!/bin/sh
printf '%s\n' "$FAKE_DOCTOR"
[ -n "${FAKE_DOCTOR_ERR:-}" ] && printf '%s\n' "$FAKE_DOCTOR_ERR" >&2
exit 0
EOF
  chmod 755 "$1/syndeo"
}

self_test() {
  local stand_ins cases=0 wrong=0
  # Not local: the EXIT trap runs after this function has returned, and has to
  # still be able to name the directory it removes.
  root="$(mktemp -d)"
  trap 'rm -rf "$root"' EXIT
  stand_ins="$root/stand-ins"
  mkdir -p "$stand_ins"
  write_stand_ins "$stand_ins"

  # run_case <name> <pass|fail> <version or ""> <failure texts, "|"-separated>
  # The fixture comes from the FAKE_* variables set on the call. Expected
  # assessments are derived from $FAKE_INSTALL, the executables it installs.
  run_case() {
    local name="$1" want="$2" version="$3" texts="$4" dir result n b expected_verify text problems=""
    cases=$((cases+1))
    dir="$root/case-$cases"
    mkdir -p "$dir"
    export FAKE_LOG="$dir/calls.log"
    : >"$FAKE_LOG"
    result=$(
      PATH="$stand_ins:$PATH"
      VERSION="$version"
      EXPECT_SIGNED="${FAKE_EXPECT:-no}"
      INSTALLER="$stand_ins/installer"
      WORK="$dir"; BIN="$dir/bin"; HOME_DIR="$dir/home"; banner=""
      pass=0; fail=0; failures=""
      {
        if install_release; then
          resolve_version
          check_artifacts
          if [ "$(uname -s)" = Darwin ]; then
            check_signing
          fi
        fi
      } >"$dir/output.log" 2>&1
      printf '%s\n%s' "$fail" "$failures"
    )
    n="${result%%$'\n'*}"
    if [ "$want" = pass ] && [ "$n" != 0 ]; then
      problems="$problems; expected to pass, $n failed"
    elif [ "$want" = fail ] && [ "$n" = 0 ]; then
      problems="$problems; expected to fail, passed"
    fi
    # Each expected failure text must be among those reported.
    while [ -n "$texts" ]; do
      text="${texts%%|*}"
      case "$result" in
        *"$text"*) ;;
        *) problems="$problems; no failure reading '$text'" ;;
      esac
      [ "$texts" = "$text" ] && break
      texts="${texts#*|}"
    done
    if grep -q '^curl' "$FAKE_LOG"; then
      problems="$problems; curl was called"
    fi
    # On macOS with signing assessed: every installed executable exactly once
    # by codesign and by Gatekeeper, and verified only if it claimed Developer ID.
    if [ "$FAKE_OS" = Darwin ] && [ "${FAKE_ASSESSED:-yes}" = yes ]; then
      for b in $FAKE_INSTALL; do
        n=$(grep -c "^codesign -dv --verbose=2 .*/bin/$b\$" "$FAKE_LOG")
        [ "$n" = 1 ] || problems="$problems; codesign -dv on $b $n times"
        n=$(grep -c "^spctl -a -t execute .*/bin/$b\$" "$FAKE_LOG")
        [ "$n" = 1 ] || problems="$problems; spctl -a on $b $n times"
        expected_verify=0
        case " ${FAKE_SIGNED:-} ${FAKE_INVALID:-} " in *" $b "*) expected_verify=1 ;; esac
        n=$(grep -c "^codesign --verify --strict .*/bin/$b\$" "$FAKE_LOG")
        [ "$n" = "$expected_verify" ] || problems="$problems; codesign --verify on $b $n times, not $expected_verify"
      done
      # And nothing that was not installed.
      n=$(grep -c '^codesign -dv' "$FAKE_LOG")
      [ "$n" = "$(echo "$FAKE_INSTALL" | wc -w | tr -d ' ')" ] || problems="$problems; codesign -dv ran $n times"
    elif [ "$FAKE_OS" != Darwin ] && grep -qE '^(codesign|spctl)' "$FAKE_LOG"; then
      problems="$problems; signing was assessed off macOS"
    fi
    if [ -z "$problems" ]; then
      printf '  ok    %s\n' "$name"
    else
      printf '  WRONG %s%s\n' "$name" "$problems"
      sed 's/^/          | /' "$dir/output.log"
      wrong=$((wrong+1))
    fi
  }

  local six="$CORE" seven="$CORE syndeo-webkit"
  export FAKE_OS=Darwin FAKE_GATEKEEPER=enabled FAKE_SIGNED="" FAKE_INVALID="" FAKE_ACCEPTED="" FAKE_EXPECT=no FAKE_ASSESSED=yes

  printf '\n  which release it is\n'
  FAKE_BANNER=0.1.2 FAKE_REPORTS=0.1.2 FAKE_INSTALL="$six" \
    run_case "0.1.2 as asked: six core binaries, no WebKit" pass 0.1.2 ""
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$seven" \
    run_case "0.1.3 as asked: WebKit beside the proxy" pass 0.1.3 ""
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$seven" \
    run_case "a leading v on the version asked for is stripped" pass v0.1.3 ""
  FAKE_BANNER=0.1.2 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$six" \
    run_case "asked for 0.1.3, installer says 0.1.2, no WebKit" fail 0.1.3 "syndeo-webkit beside syndeo-proxy|installer banner"
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.2 FAKE_INSTALL="$seven" \
    run_case "asked for 0.1.2, installer says 0.1.3, WebKit installed" fail 0.1.2 "syndeo-webkit is not installed|installer banner"
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$seven" \
    run_case "no version asked for: judged by what syndeo reports (0.1.3)" pass "" ""
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$six" \
    run_case "no version asked for, syndeo reports 0.1.3, no WebKit" fail "" "syndeo-webkit beside syndeo-proxy"
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.2 FAKE_INSTALL="$six" \
    run_case "no version asked for, syndeo reports 0.1.2, installer said 0.1.3" fail "" "installer banner"
  FAKE_BANNER=0.1 FAKE_REPORTS=0.1 FAKE_INSTALL="$six" \
    run_case "a malformed version asked for decides nothing" fail 0.1 "release version|syndeo-webkit expectation"
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui" \
    run_case "no version asked for and no syndeo installed" fail "" "release version|syndeo-webkit expectation|binaries present"
  FAKE_BANNER=0.1.3 FAKE_REPORTS=garbage FAKE_INSTALL="$six" \
    run_case "no version asked for and syndeo reports nonsense" fail "" "release version|syndeo-webkit expectation"
  FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$six syndeo-servo" \
    run_case "syndeo-servo installed" fail 0.1.3 "syndeo-servo is not installed|syndeo-webkit beside syndeo-proxy"
  FAKE_OS=Linux FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$six" \
    run_case "Linux 0.1.3: no WebKit, and signing not assessed" pass 0.1.3 ""
  FAKE_OS=Linux FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$seven" \
    run_case "Linux 0.1.3 with WebKit installed" fail 0.1.3 "syndeo-webkit is not installed"

  printf '\n  signing and Gatekeeper, every executable\n'
  export FAKE_BANNER=0.1.3 FAKE_REPORTS=0.1.3 FAKE_INSTALL="$seven"
  run_case "unsigned expected: none signed, all rejected" pass 0.1.3 ""
  FAKE_EXPECT=yes FAKE_SIGNED="$seven" FAKE_ACCEPTED="$seven" \
    run_case "signed expected: all valid, all accepted" pass 0.1.3 ""
  FAKE_SIGNED="syndeo-net" \
    run_case "unsigned expected, one signed" fail 0.1.3 "unsigned release — Developer ID signature found on: syndeo-net"
  FAKE_EXPECT=yes FAKE_SIGNED="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-webkit" FAKE_ACCEPTED="$seven" \
    run_case "signed expected, one without a signature" fail 0.1.3 "Developer ID signature — invalid: none; missing: syndeo-ui"
  FAKE_ACCEPTED="syndeo-proxy" \
    run_case "unsigned expected, Gatekeeper accepts one" fail 0.1.3 "Gatekeeper — accepts unsigned executables: syndeo-proxy"
  FAKE_EXPECT=yes FAKE_SIGNED="$seven" FAKE_ACCEPTED="syndeo syndeo-net syndeo-keystore syndeo-proxy syndeo-ui syndeo-webkit" \
    run_case "signed expected, Gatekeeper rejects one" fail 0.1.3 "Gatekeeper — rejects: syndeo-agent"
  FAKE_INVALID="syndeo-keystore" \
    run_case "unsigned expected, one with an invalid Developer ID signature" fail 0.1.3 "unsigned release — Developer ID signature found on: syndeo-keystore"
  FAKE_EXPECT=yes FAKE_SIGNED="syndeo syndeo-net syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit" FAKE_INVALID="syndeo-keystore" FAKE_ACCEPTED="$seven" \
    run_case "signed expected, one with an invalid Developer ID signature" fail 0.1.3 "Developer ID signature — invalid: syndeo-keystore; missing: none"
  FAKE_INSTALL="" FAKE_ASSESSED=no \
    run_case "unsigned expected, nothing installed to assess" fail 0.1.3 "signing — no executables were installed to assess"
  FAKE_INSTALL="" FAKE_ASSESSED=no FAKE_EXPECT=yes \
    run_case "signed expected, nothing installed to assess" fail 0.1.3 "signing — no executables were installed to assess"
  FAKE_GATEKEEPER=disabled FAKE_ASSESSED=no \
    run_case "Gatekeeper assessments disabled" fail 0.1.3 "Gatekeeper — assessments are disabled"

  # doctor_case <name> <os> <pass|fail> <failure texts> <shown texts>, each
  # list "|"-separated. The doctor's output is $FAKE_DOCTOR, its stderr
  # $FAKE_DOCTOR_ERR.
  doctor_case() {
    local name="$1" want="$3" texts="$4" shown="$5" dir result n text problems=""
    cases=$((cases+1))
    dir="$root/case-$cases"
    mkdir -p "$dir"
    export FAKE_LOG="$dir/calls.log"
    : >"$FAKE_LOG"
    fake_doctor "$dir/bin"
    result=$(
      PATH="$stand_ins:$PATH"
      FAKE_OS="$2"
      WORK="$dir"; BIN="$dir/bin"; HOME_DIR="$dir/home"
      pass=0; fail=0; failures=""
      check_doctor >"$dir/output.log" 2>&1
      printf '%s\n%s' "$fail" "$failures"
    )
    n="${result%%$'\n'*}"
    if [ "$want" = pass ] && [ "$n" != 0 ]; then
      problems="$problems; expected to pass, $n failed"
    elif [ "$want" = fail ] && [ "$n" = 0 ]; then
      problems="$problems; expected to fail, passed"
    fi
    while [ -n "$texts" ]; do
      text="${texts%%|*}"
      case "$result" in *"$text"*) ;; *) problems="$problems; no failure reading '$text'" ;; esac
      [ "$texts" = "$text" ] && break
      texts="${texts#*|}"
    done
    while [ -n "$shown" ]; do
      text="${shown%%|*}"
      grep -qF -- "$text" "$dir/output.log" || problems="$problems; the output does not show '$text'"
      [ "$shown" = "$text" ] && break
      shown="${shown#*|}"
    done
    if [ -z "$problems" ]; then
      printf '  ok    %s\n' "$name"
    else
      printf '  WRONG %s%s\n' "$name" "$problems"
      sed 's/^/          | /' "$dir/output.log"
      wrong=$((wrong+1))
    fi
  }

  local head="version         0.1.4
syndeo-net      /x/syndeo-net
syndeo-keystore /x/syndeo-keystore
syndeo-agent    /x/syndeo-agent"
  local uninitialized="keystore        initialized false, unsealed false
                passphrase required true
boundaries
  the shell's renderers (syndeo-ui, syndeo-servo) and the agent fetch only through the net process
  the agent has no keystore socket and no session secret
  the keystore signs only what the shell confirmed, once, for one payload
  no seed exists: the keystore is not initialized"
  local webkit="  syndeo-webkit is outside that: WebKit sends traffic to syndeo-proxy by its proxy setting, which does not cover every transport, and was seen to bypass it for localhost and loopback addresses (see the README)"
  local fixed="keystore        initialized false, unsealed false
boundaries
  renderers and the agent reach the network only through the net process
  the agent has no keystore socket and no session secret
  the keystore signs only what the shell confirmed, once, for one payload
  the seed is forgotten on idleness, on sleep, and on screen lock"
  local unreachable="keystore        not reachable: syndeo-keystore exited before it was ready (exit status: 127)
boundaries
  seed retention unknown: the keystore is not reachable"

  printf '\n  what doctor says\n'
  FAKE_DOCTOR="$head
$uninitialized" \
    doctor_case "Linux, no keys: facts, and no screen-lock claim" Linux pass "" ""
  FAKE_DOCTOR="$head
$uninitialized
$webkit" \
    doctor_case "macOS, no keys, with the WebKit line and its loopback bypass" Darwin pass "" ""
  FAKE_DOCTOR="$head
$uninitialized" \
    doctor_case "macOS without the WebKit line" Darwin fail "the WebKit line" ""
  FAKE_DOCTOR="$head
$uninitialized
  syndeo-webkit is outside that: WebKit sends traffic to syndeo-proxy by its proxy setting" \
    doctor_case "macOS, the WebKit line without the loopback bypass" Darwin fail "the WebKit line" ""
  FAKE_DOCTOR="$head
$fixed" \
    doctor_case "Linux, 0.1.3's fixed claims" Linux fail "0.1.3's fixed seed-retention claim|no screen-lock claim on Linux" ""
  FAKE_DOCTOR="$head
$uninitialized
  once unsealed, the seed is forgotten on sleep, on screen lock" \
    doctor_case "Linux, a screen-lock claim" Linux fail "no screen-lock claim on Linux" ""
  FAKE_DOCTOR="$head
$unreachable" FAKE_DOCTOR_ERR="syndeo-keystore: error while loading shared libraries: libdbus-1.so.3: cannot open shared object file" \
    doctor_case "Linux, the keystore cannot start: its stderr and ldd are shown" Linux fail \
      "the keystore starts|no seed-retention line" \
      "doctor said on stderr:|libdbus-1.so.3: cannot open shared object file|ldd syndeo-keystore: 	libdbus-1.so.3 => not found"
  FAKE_DOCTOR="$head
$unreachable" FAKE_DOCTOR_ERR="" \
    doctor_case "macOS, the keystore cannot start: no ldd" Darwin fail "the keystore starts" ""
  if grep -q '^ldd' "$root/case-$cases/calls.log"; then
    printf '  WRONG ldd was run on macOS\n'
    wrong=$((wrong+1))
  fi

  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit
fi
main "$@"
