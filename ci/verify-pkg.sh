#!/usr/bin/env bash
# Check Syndeo's macOS installer package, before and after it is installed.
#
#   ci/verify-pkg.sh inspect <pkg> <version> --tarball <tarball>
#   ci/verify-pkg.sh installed <version>
#   ci/verify-pkg.sh --self-test
#
# inspect looks inside a package without installing it:
# - the name;
# - what the product and the component hold;
# - the product's limits (Apple Silicon, macOS 13, the startup disk only,
#   nothing to customize, no JavaScript);
# - the component's identity, its scripts, byte for byte against the
#   templates, and overwrite-permissions="false";
# - the BOM, path by path with modes, owners and link targets;
# - the payload's files against the release tarball;
# - every command's --version;
# - the signing state.
#
# installed checks what an installation left on this Mac, with the same checks
# the package's own scripts make: the receipt, the private directory, the
# version tree, `current`, the seven command links, and each command's
# --version as started from /usr/local/bin.
#
# --self-test checks the package's three root scripts, and this script's own
# judgement, against stand-ins in temporary directories, as an ordinary user.
# The scripts are macOS root scripts, written for BSD stat, ls and mv, so
# their behaviour is checked on macOS only. On Linux the version grammar, the
# comparator and the rendering of the scripts are checked.
#
# Knobs:
#   SYNDEO_EXPECT_SIGNED  no (default): the package carries no signature and
#                         Gatekeeper rejects it, and no executable carries a
#                         Developer ID. yes is judged by the signing step, which
#                         is not part of this script yet, so it is refused.
#
# Exits non-zero if any check fails.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
templates="$here/macos-pkg"
builder="$here/package-macos-pkg.sh"
NAMES="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit"

# shellcheck source=ci/macos-pkg/version.sh
. "$templates/version.sh"

pass=0; fail=0; failures=""
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; pass=$((pass+1)); }
bad()  {
  printf '  \033[31mFAIL\033[0m  %s — %s\n' "$1" "$2"
  fail=$((fail+1))
  failures="${failures}${1} — ${2}"$'\n'
}
note() { printf '\n  %s\n' "$1"; }
report() { printf '\n  %d passed, %d failed\n\n' "$pass" "$fail"; }

# expected_bom V: every line `lsbom -p fmugl` prints for the package of V.
expected_bom() {
  local v="$1" n
  printf '%s\t40755\t0\t0\t\n' . ./usr ./usr/local ./usr/local/bin ./usr/local/libexec \
    ./usr/local/libexec/syndeo "./usr/local/libexec/syndeo/$v" "./usr/local/libexec/syndeo/$v/tools"
  for n in $NAMES; do
    printf '%s\t120755\t0\t0\t%s\n' "./usr/local/bin/$n" "../libexec/syndeo/current/$n"
    printf '%s\t100755\t0\t0\t\n' "./usr/local/libexec/syndeo/$v/$n"
  done
  printf '%s\t100755\t0\t0\t\n' "./usr/local/libexec/syndeo/$v/uninstall.sh"
  printf '%s\t100644\t0\t0\t\n' "./usr/local/libexec/syndeo/$v/README.md" \
    "./usr/local/libexec/syndeo/$v/LICENSE" "./usr/local/libexec/syndeo/$v/tools/wordcount.wat"
}

# xp FILE XPATH: an XPath value from FILE, or nothing.
xp() { xmllint --xpath "$2" "$1" 2>/dev/null; }

# ------------------------------------------------------------------ inspect

inspect() {
  local pkg="$1" version="$2" tarball="$3"
  local expect="${SYNDEO_EXPECT_SIGNED:-no}" name w full dist info scripts payload tree f n out
  case "$expect" in
    no) ;;
    yes) echo "SYNDEO_EXPECT_SIGNED=yes is judged by the signing step, which is not part of this script yet" >&2; return 2 ;;
    *) echo "SYNDEO_EXPECT_SIGNED must be yes or no, not '$expect'" >&2; return 2 ;;
  esac
  name="syndeo-$version-aarch64-apple-darwin"
  w="$inspect_work"

  note "the package, by name and shape"
  if syndeo_version_valid "$version" && [ "${pkg##*/}" = "$name.pkg" ]; then
    ok "named $name.pkg"
  else
    bad "name" "${pkg##*/} is not $name.pkg for a valid version"
  fi
  full="$w/full"
  if ! pkgutil --expand-full "$pkg" "$full" >/dev/null 2>&1; then
    bad "expand" "pkgutil could not expand $pkg"
    return 1
  fi
  if [ "$(cd "$full" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "Distribution syndeo.pkg " ]; then
    ok "the product holds a Distribution and one component, syndeo.pkg"
  else
    bad "product" "it holds: $(cd "$full" && ls -A | tr '\n' ' ')"
  fi
  if [ "$(cd "$full/syndeo.pkg" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "Bom PackageInfo Payload Scripts " ]; then
    ok "the component holds a Bom, PackageInfo, Payload and Scripts"
  else
    bad "component" "it holds: $(cd "$full/syndeo.pkg" && ls -A | tr '\n' ' ')"
  fi

  note "the product's limits"
  dist="$full/Distribution"
  local want got problems=""
  for want in \
    "string(/installer-gui-script/options/@customize)=never" \
    "string(/installer-gui-script/options/@require-scripts)=false" \
    "string(/installer-gui-script/options/@hostArchitectures)=arm64" \
    "string(/installer-gui-script/options/@rootVolumeOnly)=true" \
    "string(/installer-gui-script/domains/@enable_localSystem)=true" \
    "string(/installer-gui-script/domains/@enable_anywhere)=false" \
    "string(/installer-gui-script/domains/@enable_currentUserHome)=false" \
    "string(/installer-gui-script/volume-check/allowed-os-versions/os-version/@min)=13.0" \
    "string(/installer-gui-script/product/@id)=com.sum.syndeo" \
    "string(/installer-gui-script/product/@version)=$version" \
    "string(/installer-gui-script/pkg-ref[@version]/@version)=$version" \
    "count(/installer-gui-script/pkg-ref[@id!='com.sum.syndeo.pkg'])=0" \
    "count(//script)=0" \
    "count(//installation-check)=0" \
    "count(//@script)=0"; do
    got="$(xp "$dist" "${want%=*}")"
    [ "$got" = "${want##*=}" ] || problems="$problems ${want%=*} is '$got';"
  done
  if [ -z "$problems" ]; then
    ok "Apple Silicon, macOS 13 or later, the startup disk only, nothing to customize, no JavaScript"
  else
    bad "Distribution" "$problems"
  fi

  note "the component"
  info="$full/syndeo.pkg/PackageInfo"
  problems=""
  for want in \
    "string(/pkg-info/@identifier)=com.sum.syndeo.pkg" \
    "string(/pkg-info/@version)=$version" \
    "string(/pkg-info/@install-location)=/" \
    "string(/pkg-info/@overwrite-permissions)=false" \
    "string(/pkg-info/@auth)=root" \
    "count(/pkg-info/scripts/*)=2" \
    "string(/pkg-info/scripts/preinstall/@file)=./preinstall" \
    "string(/pkg-info/scripts/postinstall/@file)=./postinstall" \
    "count(/pkg-info/bundle)=0" \
    "count(/pkg-info/bundle-version/*)=0"; do
    got="$(xp "$info" "${want%=*}")"
    [ "$got" = "${want##*=}" ] || problems="$problems ${want%=*} is '$got';"
  done
  if [ -z "$problems" ]; then
    ok "com.sum.syndeo.pkg $version, at /, overwrite-permissions=\"false\", a preinstall and a postinstall, no bundles"
  else
    bad "PackageInfo" "$problems"
  fi

  scripts="$full/syndeo.pkg/Scripts"
  mkdir "$w/expected"
  "$builder" --render preinstall "$version" '' 0 0 /usr/sbin/pkgutil "$w/expected/preinstall" >/dev/null
  "$builder" --render postinstall "$version" '' 0 0 /usr/sbin/pkgutil "$w/expected/postinstall" >/dev/null
  "$builder" --render uninstall "$version" '' 0 0 /usr/sbin/pkgutil "$w/expected/uninstall.sh" >/dev/null
  if [ "$(cd "$scripts" && ls -A | LC_ALL=C sort | tr '\n' ' ')" = "postinstall preinstall " ]; then
    ok "exactly two scripts, preinstall and postinstall"
  else
    bad "scripts" "found: $(cd "$scripts" && ls -A | tr '\n' ' ')"
  fi
  for f in preinstall postinstall; do
    if cmp -s "$scripts/$f" "$w/expected/$f" && [ "$(stat -f %Lp "$scripts/$f")" = 755 ]; then
      ok "$f is the template with this version, root '', uid 0, gid 0 and /usr/sbin/pkgutil, mode 0755"
    else
      bad "$f" "differs from the template rendered for $version, or is not mode 0755"
    fi
  done

  note "the BOM: every path, mode, owner and link"
  expected_bom "$version" | LC_ALL=C sort >"$w/bom.want"
  lsbom -p fmugl "$full/syndeo.pkg/Bom" | LC_ALL=C sort >"$w/bom.have"
  if cmp -s "$w/bom.want" "$w/bom.have"; then
    ok "exactly the version tree, uninstall.sh and seven links to ../libexec/syndeo/current, all root:wheel; no current, nothing writable beyond its owner, no special bits"
  else
    bad "BOM" "$(diff "$w/bom.want" "$w/bom.have" | grep '^[<>]' | head -8 | tr '\n' ' ')"
  fi

  note "the payload, against the release tarball"
  payload="$full/syndeo.pkg/Payload"
  tree="$payload/usr/local/libexec/syndeo/$version"
  mkdir "$w/tarball"
  if ! tar -xzf "$tarball" -C "$w/tarball" 2>/dev/null; then
    bad "tarball" "could not unpack $tarball"
  else
    problems=""
    for f in $NAMES README.md LICENSE tools/wordcount.wat; do
      cmp -s "$tree/$f" "$w/tarball/$name/$f" || problems="$problems $f"
    done
    if [ -z "$problems" ]; then
      ok "the seven executables, README.md, LICENSE and tools/wordcount.wat are byte-identical to the tarball's"
    else
      bad "payload" "differs from the tarball:$problems"
    fi
  fi
  if cmp -s "$tree/uninstall.sh" "$w/expected/uninstall.sh"; then
    ok "uninstall.sh is the template rendered for $version"
  else
    bad "uninstall.sh" "differs from the template rendered for $version"
  fi
  if [ "$(cd "$payload" && find . -name '*servo*' | wc -l | tr -d ' ')" = 0 ]; then
    ok "nothing of syndeo-servo"
  else
    bad "syndeo-servo" "is in the payload"
  fi

  note "what each command says it is"
  problems=""
  for n in $NAMES; do
    out="$("$tree/$n" --version 2>&1 | head -n 1)"
    [ "$out" = "$n $version" ] || problems="$problems $n says '$out';"
  done
  if [ -z "$problems" ]; then
    ok "all seven report $version"
  else
    bad "versions" "$problems"
  fi

  note "signing (expected: none)"
  # Captured first: pkgutil exits non-zero for an unsigned package, which
  # pipefail would otherwise report as the check failing.
  local signature
  signature="$(pkgutil --check-signature "$pkg" 2>&1)"
  if printf '%s\n' "$signature" | grep -q 'Status: no signature'; then
    ok "the package carries no signature"
  else
    bad "package signature" "$(printf '%s\n' "$signature" | sed -n 2p)"
  fi
  if spctl -a -vv -t install "$pkg" >"$w/spctl" 2>&1; then
    bad "Gatekeeper" "accepts the unsigned package: $(tr '\n' ' ' <"$w/spctl")"
  else
    ok "Gatekeeper rejects the package: $(sed -n 's/.*: //p' "$w/spctl" | head -n 1 | tr -d '\n')"
  fi
  problems=""
  local adhoc=""
  for n in $NAMES; do
    out="$(codesign -dv --verbose=2 "$tree/$n" 2>&1)"
    if printf '%s\n' "$out" | grep -q '^Authority=Developer ID'; then
      problems="$problems $n"
    elif printf '%s\n' "$out" | grep -q '^Signature=adhoc'; then
      adhoc="$adhoc $n"
    fi
  done
  if [ -z "$problems" ]; then
    ok "no executable carries a Developer ID; ad-hoc signed:${adhoc:- none}"
  else
    bad "executables" "a Developer ID signature on:$problems"
  fi
}

# ------------------------------------------------------------------ installed

installed() {
  local version="$1" n out
  # The same checks the package's scripts make, from the same file. The
  # SYNDEO_VERIFY_* knobs exist for the self-test, which installs nothing.
  ROOT="${SYNDEO_VERIFY_ROOT:-}"
  EXPECT_UID="${SYNDEO_VERIFY_UID:-0}"
  EXPECT_GID="${SYNDEO_VERIFY_GID:-0}"
  PKGUTIL="${SYNDEO_VERIFY_PKGUTIL:-/usr/sbin/pkgutil}"
  SCRIPT=verify
  # shellcheck source=ci/macos-pkg/common.sh
  . "$templates/common.sh"

  note "the receipt"
  read_receipt
  if [ "$r_present" != yes ]; then
    bad "receipt" "there is no receipt for $SYNDEO_ID"
    return 0
  fi
  check_receipt
  if [ "$r_version" = "$version" ] && [ "$nfindings" = 0 ]; then
    ok "$SYNDEO_ID $version, for / at /, listing exactly its paths, and no other package claims them"
  else
    bad "receipt" "version '$r_version';$(printf '%s' "$findings" | tr '\n' ' ')"
  fi

  note "the files"
  findings=''; nfindings=0
  check_strict_dir "$SYNDEO_LOCAL"
  check_strict_dir "$SYNDEO_LIBEXEC"
  check_bin_dir "$SYNDEO_BIN"
  scan_private
  scan_bin
  [ "$current_v" = "$version" ] || finding "$SYNDEO_DIR/current: points at '$current_v', not $version"
  check_tree "$version" exact
  [ "$bin_present" = 7 ] || finding "$SYNDEO_BIN: has $bin_present of the seven commands"
  for n in $bin_foreign; do
    finding "$SYNDEO_BIN/$n: is not a link the package writes"
  done
  if [ "$nfindings" = 0 ]; then
    ok "$SYNDEO_DIR/$version is exactly the package's, current points at it, and all seven commands link through current${versions:+ (versions present:$versions)}"
  else
    bad "installation" "$(printf '%s' "$findings" | tr '\n' ' ')"
  fi

  note "the commands, started from $SYNDEO_BIN"
  local problems=""
  for n in $NAMES; do
    out="$(env -i PATH=/usr/bin:/bin HOME="${HOME:-/}" "$SYNDEO_BIN/$n" --version 2>&1 | head -n 1)"
    [ "$out" = "$n $version" ] || problems="$problems $n says '$out';"
  done
  if [ -z "$problems" ]; then
    ok "all seven report $version"
  else
    bad "versions" "$problems"
  fi
}

# ------------------------------------------------------------------ self-test

cases=0; wrong=0

# expect NAME COMMAND...: one self-test case, passing when COMMAND does.
expect() {
  local name="$1"; shift
  cases=$((cases+1))
  if "$@"; then
    printf '  ok    %s\n' "$name"
  else
    printf '  WRONG %s\n' "$name"
    wrong=$((wrong+1))
  fi
}

st_versions() {
  local v r
  printf '\n  the version grammar\n'
  for v in 0.0.0 0.1.5 1.2.3 10.20.30 0.1.18446744073709551616; do
    expect "valid: $v" syndeo_version_valid "$v"
  done
  for v in '' 1.2 1.2.3.4 01.2.3 1.02.3 1.2.03 -1.2.3 +1.2.3 v1.2.3 ' 1.2.3' '1.2.3 ' 1.2.3-rc1 1.2.x 1..3 \
    $'1.2.3\n4.5.6' $'1.2.3\n' '1.2.3/' '1.2.3*'; do
    expect "invalid: $(printf '%q' "$v")" eval '! syndeo_version_valid "$v"'
  done

  printf '\n  the comparator\n'
  while read -r a want b; do
    r="$(syndeo_version_cmp "$a" "$b")"
    expect "$a $want $b" test "$r" = "$want"
  done <<'EOF'
0.1.9 lt 0.1.10
0.1.10 gt 0.1.9
0.2.0 gt 0.1.99
1.0.0 gt 0.999.999
10.0.0 gt 9.9.9
0.1.5 eq 0.1.5
0.0.1 lt 0.0.2
0.1.18446744073709551616 gt 0.1.18446744073709551615
0.1.99999999999999999999 gt 0.1.18446744073709551616
2.0.0 gt 1.999999999999.999999999999
0.10.0 gt 0.9.99
EOF
}

st_render() {
  local d="$1" k
  printf '\n  rendering the scripts\n'
  for k in preinstall postinstall uninstall; do
    "$builder" --render "$k" 0.1.7 '' 0 0 /usr/sbin/pkgutil "$d/$k" >/dev/null 2>&1
    expect "$k renders" test -s "$d/$k"
    expect "$k: no placeholder left" eval "! grep -q '@[A-Z_]*@' '$d/$k'"
    expect "$k: root '', uid 0, gid 0, /usr/sbin/pkgutil" eval "grep -qx \"ROOT=''\" '$d/$k' && grep -qx \"EXPECT_UID='0'\" '$d/$k' && grep -qx \"EXPECT_GID='0'\" '$d/$k' && grep -qx \"PKGUTIL='/usr/sbin/pkgutil'\" '$d/$k'"
    expect "$k: the comparator and the shared checks, once each" eval "[ \"\$(grep -c '^syndeo_version_cmp() {' '$d/$k')\" = 1 ] && [ \"\$(grep -c '^check_tree() {' '$d/$k')\" = 1 ]"
    expect "$k: no test fault" eval "! grep -q 'test build' '$d/$k'"
    expect "$k: POSIX sh parses it" sh -n "$d/$k"
  done
  expect "preinstall carries the incoming version" grep -qx "INCOMING='0.1.7'" "$d/preinstall"
  expect "uninstall.sh carries its own version" grep -qx "EMBEDDED='0.1.7'" "$d/uninstall"
  expect "a postinstall fault renders for 0.0.x" eval "'$builder' --render postinstall 0.0.3 '' 0 0 /usr/sbin/pkgutil '$d/fault' --test-fault postinstall-fails >/dev/null 2>&1 && grep -q 'test build: stopping before the switch' '$d/fault'"
  expect "no fault for a release version" eval "! '$builder' --render postinstall 0.1.7 '' 0 0 /usr/sbin/pkgutil '$d/x' --test-fault postinstall-fails >/dev/null 2>&1"
  expect "no fault in preinstall" eval "! '$builder' --render preinstall 0.0.3 '' 0 0 /usr/sbin/pkgutil '$d/x' --test-fault postinstall-fails >/dev/null 2>&1"
  expect "a root with a quote is refused" eval "! '$builder' --render preinstall 0.1.7 \"/tmp/it's\" 0 0 /usr/sbin/pkgutil '$d/x' >/dev/null 2>&1"
  expect "a relative pkgutil is refused" eval "! '$builder' --render preinstall 0.1.7 '' 0 0 pkgutil '$d/x' >/dev/null 2>&1"
  expect "a malformed version is refused" eval "! '$builder' --render preinstall 0.1 '' 0 0 /usr/sbin/pkgutil '$d/x' >/dev/null 2>&1"
}

# --- macOS: the root scripts, under a temporary root ------------------------

# st_new: a fresh case. C is its directory; R the root, with a space in it,
# holding usr/local; DB the stand-in receipts; U and G the expected owner.
st_new() {
  C="$T/case-$((++st_n))"
  R="$C/root with space"
  DB="$C/receipts"
  D="$R/usr/local/libexec/syndeo"
  mkdir -p "$R/usr/local" "$DB"
  chmod 755 "$R" "$R/usr" "$R/usr/local"
  U="$(id -u)"
  G="$(stat -f %g "$R")"
  cat >"$C/pkgutil" <<'EOF'
#!/bin/sh
db="$(dirname "$0")/receipts"
case "$1" in
  --pkg-info)
    [ -f "$db/$2.info" ] || { echo "No receipt for '$2' found at '/'." >&2; exit 1; }
    cat "$db/$2.info" ;;
  --files)
    [ -f "$db/$2.files" ] || exit 1
    cat "$db/$2.files" ;;
  --file-info)
    printf 'volume: /\npath: %s\n' "$2"
    [ -f "$db/claims" ] || exit 0
    while IFS='	' read -r p id; do
      [ "$p" = "$2" ] && printf '\npkgid: %s\n' "$id"
    done <"$db/claims"
    exit 0 ;;
  --forget)
    [ -f "$db/forget-fails" ] && { echo "pkgutil: could not forget" >&2; exit 1; }
    [ -f "$db/$2.info" ] || exit 1
    rm -f "$db/$2.info" "$db/$2.files" ;;
  *) exit 64 ;;
esac
EOF
  chmod 755 "$C/pkgutil"
}

st_render_for() { "$builder" --render "$1" "$2" "$R" "$U" "$G" "$C/pkgutil" "$3" ${4:+--test-fault "$4"} >/dev/null; }

st_dirs() {
  mkdir -p "$D"
  chmod 755 "$R/usr/local/libexec" "$D"
}

# st_tree V [subset]: a version tree as the package lays it down.
st_tree() {
  local v="$1" t n
  st_dirs
  t="$D/$v"
  mkdir -p "$t/tools"
  chmod 755 "$t" "$t/tools"
  for n in $NAMES; do
    if [ "${2:-}" = subset ] && [ "$n" = syndeo-webkit ]; then continue; fi
    printf '#!/bin/sh\necho "%s %s"\n' "$n" "$v" >"$t/$n"
    chmod 755 "$t/$n"
  done
  echo readme >"$t/README.md"
  chmod 644 "$t/README.md"
  if [ "${2:-}" != subset ]; then
    echo license >"$t/LICENSE"
    echo '(module)' >"$t/tools/wordcount.wat"
    chmod 644 "$t/LICENSE" "$t/tools/wordcount.wat"
  fi
  st_render_for uninstall "$v" "$t/uninstall.sh"
}

st_links() {
  local n
  mkdir -p "$R/usr/local/bin"
  chmod 755 "$R/usr/local/bin"
  for n in $NAMES; do
    ln -s "../libexec/syndeo/current/$n" "$R/usr/local/bin/$n"
  done
}

st_current() { ln -s "$1" "$D/current"; }

# st_receipt V [LOCATION]: the receipt for V. LOCATION is its location lines,
# none if empty; by default the one pkgutil prints for this package, whose
# location relative to the volume is empty.
st_receipt() {
  local v="$1" loc="${2-location: }" n
  {
    printf 'package-id: com.sum.syndeo.pkg\nversion: %s\nvolume: /\n' "$v"
    [ -z "$loc" ] || printf '%s\n' "$loc"
    printf 'install-time: 1\n'
  } >"$DB/com.sum.syndeo.pkg.info"
  {
    printf '%s\n' usr usr/local usr/local/bin usr/local/libexec usr/local/libexec/syndeo \
      "usr/local/libexec/syndeo/$v" "usr/local/libexec/syndeo/$v/tools" \
      "usr/local/libexec/syndeo/$v/tools/wordcount.wat" "usr/local/libexec/syndeo/$v/README.md" \
      "usr/local/libexec/syndeo/$v/LICENSE" "usr/local/libexec/syndeo/$v/uninstall.sh"
    for n in $NAMES; do
      printf '%s\n' "usr/local/libexec/syndeo/$v/$n" "usr/local/bin/$n"
    done
  } >"$DB/com.sum.syndeo.pkg.files"
}

# st_installed V [LOCATION]: what a completed installation of V leaves.
st_installed() { st_tree "$1"; st_links; st_current "$1"; st_receipt "$@"; }

st_snap() {
  { find "$R" "$DB" -print0 | xargs -0 stat -f '%N|%HT|%u|%g|%Lp|%Mp|%Sf|%i|%z|%Y'; } | LC_ALL=C sort
}

# st_run SCRIPT [args...]: run a rendered script, recording its status,
# output, and whether anything under the root or the receipts changed.
st_run() {
  local before after
  before="$(st_snap)"
  /bin/sh "$@" >"$C/out" 2>&1
  status=$?
  after="$(st_snap)"
  [ "$before" = "$after" ] && changed=no || changed=yes
}

# pre_case NAME EXIT TEXT VERSION [TARGET]: the preinstall for VERSION exits
# EXIT, says TEXT, and changes nothing.
pre_case() {
  local name="$1" want="$2" text="$3" v="$4" target="${5:-/}"
  st_render_for preinstall "$v" "$C/preinstall"
  st_run "$C/preinstall" "$C/fake.pkg" / "$target" /
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qF -- "$text" "$C/out" || verdict=no
  [ "$changed" = no ] || verdict=no
  expect "preinstall: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_preinstall() {
  printf '\n  preinstall: every state, read-only\n'
  st_new; pre_case "1: nothing installed" 0 "installing 0.0.9" 0.0.9
  st_new; rm -rf "${R:?}/usr/local"; pre_case "1: no /usr/local at all" 0 "installing 0.0.9" 0.0.9
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; echo x >"$R/usr/local/bin/syndeo"
  pre_case "2: a plain file at a command, no receipt" 1 "/usr/local/bin/syndeo: exists, and no Syndeo package is installed" 0.0.9
  st_new; st_links; pre_case "2: the package's own link shapes, but no receipt" 1 "exists, and no Syndeo package is installed" 0.0.9
  st_new; st_tree 0.0.9 subset; pre_case "3: an interrupted first install of this version" 0 "resuming an interrupted installation of 0.0.9" 0.0.9
  st_new; st_tree 0.0.9; st_current 0.0.9; st_links; pre_case "3: interrupted after the switch, links in place" 0 "resuming an interrupted installation of 0.0.9" 0.0.9
  st_new; st_tree 0.0.9 subset; st_current 0.0.9; pre_case "3 refused: current set, but the tree incomplete" 1 "is missing" 0.0.9
  st_new; st_tree 0.0.8; pre_case "4: another version, no receipt" 1 "exists, and no Syndeo package is installed" 0.0.9
  st_new; st_tree 0.0.9 subset; echo x >"$D/notes"; pre_case "4: something else in the private directory" 1 "$D/notes: is not part of Syndeo" 0.0.9
  st_new; st_tree 0.0.9 subset; st_links; rm "$R/usr/local/bin/syndeo-ui"; ln -s /tmp/elsewhere "$R/usr/local/bin/syndeo-ui"
  pre_case "4: a foreign link beside an interrupted install" 1 "syndeo-ui: is not a link this package writes" 0.0.9
  st_new; st_installed 0.0.9; pre_case "5: upgrade 0.0.9 to 0.0.10, numerically" 0 "upgrading from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; pre_case "6: the same version again, exact" 0 "reinstalling 0.0.10" 0.0.10
  st_new; st_installed 0.0.9; st_tree 0.0.10; rm "$D/current"; st_current 0.0.10
  pre_case "7: interrupted after the switch, before the receipt" 0 "finishing the interrupted upgrade to 0.0.10" 0.0.10
  st_new; st_installed 0.0.9; st_tree 0.0.10 subset; pre_case "8: interrupted while the payload was written" 0 "resuming the interrupted upgrade from 0.0.9 to 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; pre_case "9: a downgrade, 0.0.10 to 0.0.9" 1 "this package is the older 0.0.9" 0.0.9
  st_new; st_installed 0.0.9; st_tree 0.0.11 subset; pre_case "9: older than an interrupted newer install" 1 "Syndeo 0.0.11 is installed, or partly installed" 0.0.10
  st_new; st_installed 0.0.9; echo usr/local/extra >>"$DB/com.sum.syndeo.pkg.files"
  pre_case "10: the receipt lists other paths" 1 "does not list exactly the package's paths" 0.0.10
  st_new; st_installed 0.0.9; sed -i '' 's/^volume: \//volume: \/Volumes\/Other/' "$DB/com.sum.syndeo.pkg.info"
  pre_case "10: the receipt is for another volume" 1 "the receipt is for volume '/Volumes/Other', not /" 0.0.10
  st_new; st_installed 0.0.9; sed -i '' 's/^version: .*/version: 0.9/' "$DB/com.sum.syndeo.pkg.info"
  pre_case "10: the receipt's version is malformed" 1 "is not a version" 0.0.10
  st_new; st_installed 0.0.9; printf '%s\tcom.example.other\n' "$R/usr/local/bin/syndeo" >"$DB/claims"
  pre_case "10: another package claims a command" 1 "is claimed by another package, com.example.other" 0.0.10
  st_new; st_installed 0.0.9; rm "$D/current"; pre_case "11: current is missing" 1 "$D/current: is missing" 0.0.10
  st_new; st_installed 0.0.9; st_tree 0.0.8; rm "$D/current"; st_current 0.0.8
  pre_case "11: current older than the receipt" 1 "points at 0.0.8, older than the installed 0.0.9" 0.0.10
  st_new; st_installed 0.0.9; rm "$D/current"; ln -s elsewhere "$D/current"
  pre_case "11: current does not name a version" 1 "points at 'elsewhere', which is not a version" 0.0.10
  st_new; st_installed 0.0.9; rm -rf "$D/0.0.9"; rm "$D/current"; st_current 0.0.9
  pre_case "11: the receipt's version is gone" 1 "points at 0.0.9, which is not installed" 0.0.10
  st_new; st_installed 0.0.9; rm "$R/usr/local/bin/syndeo-net"; echo x >"$R/usr/local/bin/syndeo-net"
  pre_case "12: a command replaced by a file" 1 "syndeo-net: is not a link this package writes" 0.0.10
  st_new; st_installed 0.0.10; chmod 775 "$D/0.0.10/syndeo"; pre_case "13: same version, an executable's mode changed" 1 "$D/0.0.10/syndeo: is not a Regular File" 0.0.10
  st_new; st_installed 0.0.10; echo x >"$D/0.0.10/extra"; pre_case "13: same version, an extra file" 1 "$D/0.0.10/extra: is not part of Syndeo 0.0.10" 0.0.10
  st_new; st_installed 0.0.10; rm "$D/0.0.10/LICENSE"; ln -s README.md "$D/0.0.10/LICENSE"
  pre_case "13: same version, a symlink inside the tree" 1 "$D/0.0.10/LICENSE: is not a Regular File" 0.0.10
  st_new; st_installed 0.0.10; chmod +a "everyone allow read" "$D/0.0.10/README.md"
  pre_case "13: same version, an ACL on a file" 1 "$D/0.0.10/README.md: is not a Regular File" 0.0.10
  st_new; st_installed 0.0.10; chflags uchg "$D/0.0.10/README.md"
  pre_case "13: same version, a file flagged immutable" 1 "$D/0.0.10/README.md: is not a Regular File" 0.0.10
  chflags nouchg "$D/0.0.10/README.md"

  printf '\n  preinstall: where, and the paths around it\n'
  st_new; pre_case "another target volume" 1 "the target volume is '/Volumes/Other'" 0.0.9 /Volumes/Other
  st_new; mkdir -p "$R/usr/local/bin"; chmod 777 "$R/usr/local/bin"; pre_case "/usr/local/bin writable by everyone" 1 "/usr/local/bin: can be written by everyone" 0.0.9
  st_new; mkdir -p "$R/usr/local/bin"; chmod 775 "$R/usr/local/bin"
  if chgrp 80 "$R/usr/local/bin" 2>/dev/null; then
    pre_case "/usr/local/bin group-writable by admin is accepted" 0 "installing 0.0.9" 0.0.9
  else
    expect "/usr/local/bin group-writable by admin (needs membership of admin to set up)" false
  fi
  st_new; mkdir -p "$R/usr/local/bin"; chmod 775 "$R/usr/local/bin"; chgrp 20 "$R/usr/local/bin"
  pre_case "/usr/local/bin group-writable by staff" 1 "neither wheel nor admin" 0.0.9
  st_new; mkdir -p "$R/usr/local/bin"; chmod 755 "$R/usr/local/bin"; chmod +a "everyone allow add_file" "$R/usr/local/bin"
  pre_case "/usr/local/bin with an ACL" 1 "/usr/local/bin: has an access control list" 0.0.9
  st_new; mkdir -p "$R/usr/local/libexec"; chmod 775 "$R/usr/local/libexec"; pre_case "/usr/local/libexec group-writable" 1 "/usr/local/libexec: can be written by its group or by everyone" 0.0.9
  st_new; mkdir -p "$R/usr/local/libexec" "$C/elsewhere"; chmod 755 "$R/usr/local/libexec"; ln -s "$C/elsewhere" "$D"
  pre_case "the private directory is a symlink" 1 "$D: is a Symbolic Link, not a directory" 0.0.9
  st_new; st_dirs; pre_case "an empty private directory, no receipt" 1 "exists, and no Syndeo package is installed" 0.0.9
  st_new; U=0; pre_case "/usr/local not owned by root" 1 "/usr/local: is owned by uid $(id -u), not root" 0.0.9
}

# post_case NAME EXIT TEXT VERSION [FAULT]
post_case() {
  local name="$1" want="$2" text="$3" v="$4" fault="${5:-}"
  st_render_for postinstall "$v" "$C/postinstall" "$fault"
  /bin/sh "$C/postinstall" "$C/fake.pkg" / / / >"$C/out" 2>&1
  status=$?
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qF -- "$text" "$C/out" || verdict=no
  expect "postinstall: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_postinstall() {
  printf '\n  postinstall: the switch\n'
  st_new; st_tree 0.0.9; st_links; post_case "first install: current is created" 0 "0.0.9 is current" 0.0.9
  expect "  current points at 0.0.9" test "$(readlink "$D/current")" = 0.0.9
  st_new; st_installed 0.0.9; st_tree 0.0.10; post_case "upgrade: current moves" 0 "0.0.10 is current" 0.0.10
  expect "  current points at 0.0.10, and 0.0.9 is kept" eval "[ \"\$(readlink '$D/current')\" = 0.0.10 ] && [ -f '$D/0.0.9/syndeo' ]"
  st_new; st_installed 0.0.9; st_tree 0.0.10; ln -s 0.0.9 "$D/.current.new"
  post_case "a stale .current.new from an interrupted switch" 0 "0.0.10 is current" 0.0.10
  expect "  .current.new is gone" eval "! [ -e '$D/.current.new' ] && ! [ -L '$D/.current.new' ]"
  st_new; st_installed 0.0.9; st_tree 0.0.10 subset; post_case "an incomplete tree is not made current" 1 "is missing" 0.0.10
  expect "  current still points at 0.0.9" test "$(readlink "$D/current")" = 0.0.9
  st_new; st_tree 0.0.9; mkdir "$D/current"; post_case "current is a directory" 1 "$D/current: is not a symlink owned by root:wheel" 0.0.9
  st_new; st_installed 0.0.9; st_tree 0.0.10; post_case "the test fault stops before the switch" 1 "test build: stopping before the switch" 0.0.10 postinstall-fails
  expect "  current still points at 0.0.9" test "$(readlink "$D/current")" = 0.0.9
  st_new; st_tree 0.0.9
  st_render_for postinstall 0.0.9 "$C/postinstall"
  /bin/sh "$C/postinstall" "$C/fake.pkg" / /Volumes/Other / >"$C/out" 2>&1
  status=$?
  expect "postinstall: refuses another target volume" eval "[ $status = 1 ] && grep -q \"the target volume is '/Volumes/Other'\" '$C/out' && ! [ -L '$D/current' ]"
}

# The stress test of the switch, shared with ci/test-pkg-install.sh, which
# runs it against the installed package.
st_atomic() {
  printf '\n  the switch, under load\n'
  st_new; st_installed 0.0.8; st_tree 0.0.9
  st_render_for postinstall 0.0.8 "$C/post-0.0.8"
  st_render_for postinstall 0.0.9 "$C/post-0.0.9"
  local out
  out="$(python3 -I "$here/switch-stress.py" \
    --private "$D" --command "$R/usr/local/bin/syndeo" --versions 0.0.8 0.0.9 \
    --expect-output 'syndeo {version}' --renames 10000 \
    --postinstall 0.0.8 "$C/post-0.0.8" --postinstall 0.0.9 "$C/post-0.0.9" --postinstalls 20 2>&1)"
  local status=$?
  printf '%s\n' "$out" | sed 's/^/          | /'
  expect "10,000 renames and 20 postinstalls: only ENOENT or EINVAL while switching, every success whole, none after" test "$status" = 0
}

# un_case NAME EXIT TEXT SCRIPT [ARG]: run an uninstaller.
un_case() {
  local name="$1" want="$2" text="$3" script="$4"; shift 4
  st_run "$script" "$@"
  local verdict=yes
  [ "$status" = "$want" ] || verdict=no
  grep -qF -- "$text" "$C/out" || verdict=no
  expect "uninstall: $name" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_uninstall() {
  printf '\n  uninstall: look first, then remove\n'
  st_new; st_installed 0.0.10; st_tree 0.0.9; ln -s 0.0.10 "$D/.current.new"
  mkdir -p "$R/usr/local/libexec/someone-else"; echo keep >"$R/usr/local/bin/unrelated"
  un_case "everything, an older version and a stale .current.new included" 0 "removed Syndeo 0.0.10" "$D/0.0.10/uninstall.sh"
  expect "  nothing of Syndeo left, and the receipt forgotten" eval "! [ -e '$D' ] && ! [ -L '$R/usr/local/bin/syndeo' ] && ! [ -f '$DB/com.sum.syndeo.pkg.info' ]"
  expect "  /usr/local/bin, /usr/local/libexec and what else they hold are kept" eval "[ -d '$R/usr/local/bin' ] && [ -d '$R/usr/local/libexec/someone-else' ] && [ \"\$(cat '$R/usr/local/bin/unrelated')\" = keep ]"

  st_new; st_installed 0.0.9; st_tree 0.0.10; rm "$D/current"; st_current 0.0.10; st_receipt 0.0.10
  un_case "an older version's uninstaller refuses after an upgrade" 1 "this is 0.0.9's uninstaller, and the installed package is 0.0.10" "$D/0.0.9/uninstall.sh"
  un_case "  ... with --old-versions too" 1 "this is 0.0.9's uninstaller" "$D/0.0.9/uninstall.sh" --old-versions

  local plant
  for plant in tools-extra tree-extra private-file private-dir bad-version foreign-link mode acl pending; do
    st_new; st_installed 0.0.10
    case "$plant" in
      tools-extra) echo x >"$D/0.0.10/tools/extra.wat" ;;
      tree-extra) echo x >"$D/0.0.10/notes.txt" ;;
      private-file) echo x >"$D/notes.txt" ;;
      private-dir) mkdir "$D/backup" ;;
      bad-version) mkdir "$D/0.1" ;;
      foreign-link) rm "$R/usr/local/bin/syndeo-ui"; ln -s /Applications/Other.app "$R/usr/local/bin/syndeo-ui" ;;
      mode) chmod 775 "$D/0.0.10/syndeo-net" ;;
      acl) chmod +a "everyone allow list" "$D/0.0.10" ;;
      pending) ln -s elsewhere "$D/.current.new" ;;
    esac
    un_case "all or nothing, with $plant planted: refused, nothing changed" 1 "refusing:" "$D/0.0.10/uninstall.sh"
    [ "$changed" = no ] || { expect "  ... and nothing changed" false; }
  done

  st_new; st_installed 0.0.10; st_tree 0.0.8; st_tree 0.0.9; st_tree 0.0.11 subset
  un_case "--old-versions" 0 "removed older versions: 0.0.8 0.0.9" "$D/0.0.10/uninstall.sh" --old-versions
  expect "  keeps the receipt's version, current's and the newer one; removes only the older ones" eval "[ -d '$D/0.0.10' ] && [ -d '$D/0.0.11' ] && ! [ -e '$D/0.0.8' ] && ! [ -e '$D/0.0.9' ] && [ \"\$(readlink '$D/current')\" = 0.0.10 ] && [ -L '$R/usr/local/bin/syndeo' ] && [ -f '$DB/com.sum.syndeo.pkg.info' ]"

  st_new; st_installed 0.0.10; touch "$DB/forget-fails"
  un_case "the receipt cannot be forgotten" 2 "sudo /usr/sbin/pkgutil --forget com.sum.syndeo.pkg --volume /" "$D/0.0.10/uninstall.sh"
  expect "  every file is gone, and the receipt is still there" eval "! [ -e '$D' ] && [ -f '$DB/com.sum.syndeo.pkg.info' ]"

  st_new; st_installed 0.0.10; st_tree 0.0.11; rm "$D/current"; st_current 0.0.11
  un_case "current points past the receipt" 1 "points at '0.0.11', not 0.0.10" "$D/0.0.10/uninstall.sh"

  st_new; st_installed 0.0.10; U=0; st_render_for uninstall 0.0.10 "$C/as-root"
  un_case "not run as root" 1 "run it as root, with sudo" "$C/as-root"

  st_new; st_installed 0.0.10
  un_case "an unknown argument" 1 "usage:" "$D/0.0.10/uninstall.sh" --everything

  st_new; st_tree 0.0.10; st_links; st_current 0.0.10
  un_case "no receipt" 1 "no Syndeo package is installed" "$D/0.0.10/uninstall.sh"
}

st_installed_mode() {
  printf '\n  this script: installed\n'
  st_new; st_installed 0.0.10
  local out
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.10 2>&1)"
  expect "installed: a complete installation passes" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  rm "$D/current"; st_current 0.0.10; rm "$R/usr/local/bin/syndeo-agent"
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.10 2>&1)"
  expect "installed: a missing command fails" eval "printf '%s' \"\$out\" | grep -q 'has 6 of the seven commands'"
}

# loc_case NAME accepted|refused LOCATION: a receipt whose location lines are
# LOCATION, judged by every script that reads a receipt: this script's
# installed mode, the preinstall of an upgrade, and the uninstaller.
loc_case() {
  local name="$1" want="$2" loc="$3" out verdict
  st_new; st_installed 0.0.9 "$loc"
  out="$(SYNDEO_VERIFY_ROOT="$R" SYNDEO_VERIFY_UID="$U" SYNDEO_VERIFY_GID="$G" SYNDEO_VERIFY_PKGUTIL="$C/pkgutil" \
    bash "$here/verify-pkg.sh" installed 0.0.9 2>&1)"
  verdict=yes
  if [ "$want" = accepted ]; then
    printf '%s' "$out" | grep -q '[1-9][0-9]* passed, 0 failed' || verdict=no
  else
    printf '%s\n' "$out" | grep 'FAIL' | grep -qE 'location fields, not one|location is .*, not the volume.s root' || verdict=no
  fi
  expect "location: $name: installed $want" test "$verdict" = yes
  [ "$verdict" = yes ] || printf '%s\n' "$out" | sed 's/^/          | /'

  st_render_for preinstall 0.0.10 "$C/preinstall"
  st_run "$C/preinstall" "$C/fake.pkg" / / /
  verdict=yes
  [ "$changed" = no ] || verdict=no
  if [ "$want" = accepted ]; then
    [ "$status" = 0 ] && grep -qF "upgrading from 0.0.9 to 0.0.10" "$C/out" || verdict=no
  else
    [ "$status" = 1 ] && grep -qE 'location fields, not one|location is .*, not the volume.s root' "$C/out" || verdict=no
  fi
  expect "location: $name: preinstall $want" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"

  st_run "$D/0.0.9/uninstall.sh"
  verdict=yes
  if [ "$want" = accepted ]; then
    [ "$status" = 0 ] && grep -qF "removed Syndeo 0.0.9" "$C/out" && ! [ -f "$DB/com.sum.syndeo.pkg.info" ] || verdict=no
  else
    [ "$status" = 1 ] && [ "$changed" = no ] && grep -qE 'location fields, not one|location is .*, not the volume.s root' "$C/out" || verdict=no
  fi
  expect "location: $name: uninstall $want" test "$verdict" = yes
  [ "$verdict" = yes ] || sed 's/^/          | /' "$C/out"
}

st_location() {
  printf '\n  the receipt'"'"'s location field\n'
  loc_case "empty, as pkgutil prints it for this package" accepted 'location: '
  loc_case "/" accepted 'location: /'
  loc_case "missing" refused ''
  loc_case "duplicated" refused $'location: /\nlocation: /'
  loc_case "duplicated, empty and /" refused $'location: \nlocation: /'
  loc_case "empty, without the separating space" refused 'location:'
  loc_case "/, without the separating space" refused 'location:/'
  loc_case "/, after two spaces" refused 'location:  /'
  loc_case "/, then a space" refused 'location: / '
  loc_case "/, after a tab" refused $'location:\t/'
  loc_case "two spaces" refused 'location:  '
  loc_case "another directory" refused 'location: Applications/Other.app'
  loc_case "an absolute directory" refused 'location: /usr/local'
  loc_case "(null)" refused 'location: (null)'
}

# st_tarball DIR V: a release tarball of stand-ins, as ci/package.sh lays one out.
st_tarball() {
  local dir="$1" v="$2" name n
  name="syndeo-$v-aarch64-apple-darwin"
  mkdir -p "$dir/src/$name/tools"
  for n in $NAMES; do
    printf '#!/bin/sh\necho "%s %s"\n' "$n" "$v" >"$dir/src/$name/$n"
    chmod 755 "$dir/src/$name/$n"
  done
  cp "$here/../README.md" "$here/../LICENSE" "$dir/src/$name/"
  cp "$here/../crates/syndeo-agent/tools/wordcount.wat" "$dir/src/$name/tools/"
  tar -C "$dir/src" --format=ustar -czf "$dir/$name.tar.gz" "$name"
}

st_inspect() {
  printf '\n  this script: inspect, against stand-in packages\n'
  local d="$T/inspect" out
  mkdir -p "$d/out"
  st_tarball "$d" 0.0.1
  "$builder" 0.0.1 "$d/syndeo-0.0.1-aarch64-apple-darwin.tar.gz" "$d/out" >/dev/null
  local pkg="$d/out/syndeo-0.0.1-aarch64-apple-darwin.pkg" tgz="$d/syndeo-0.0.1-aarch64-apple-darwin.tar.gz"
  out="$(bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$tgz" 2>&1)"
  expect "inspect: a package built from the tarball passes" eval "printf '%s' \"\$out\" | grep -q '[1-9][0-9]* passed, 0 failed'"
  printf '%s\n' "$out" | grep FAIL | sed 's/^/          | /'

  mkdir -p "$d/other"
  st_tarball "$d/other" 0.0.1
  echo changed >>"$d/other/src/syndeo-0.0.1-aarch64-apple-darwin/README.md"
  tar -C "$d/other/src" --format=ustar -czf "$d/other/syndeo-0.0.1-aarch64-apple-darwin.tar.gz" syndeo-0.0.1-aarch64-apple-darwin
  out="$(bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$d/other/syndeo-0.0.1-aarch64-apple-darwin.tar.gz" 2>&1)"
  expect "inspect: a payload that differs from the tarball fails" eval "printf '%s' \"\$out\" | grep -q 'differs from the tarball: README.md'"

  mkdir -p "$d/fault"
  st_tarball "$d/fault" 0.0.2
  "$builder" 0.0.2 "$d/fault/syndeo-0.0.2-aarch64-apple-darwin.tar.gz" "$d/fault" --test-fault postinstall-fails >/dev/null
  out="$(bash "$here/verify-pkg.sh" inspect "$d/fault/syndeo-0.0.2-aarch64-apple-darwin.pkg" 0.0.2 --tarball "$d/fault/syndeo-0.0.2-aarch64-apple-darwin.tar.gz" 2>&1)"
  expect "inspect: a test-fault postinstall fails" eval "printf '%s' \"\$out\" | grep -q 'postinstall — differs from the template'"

  # Edited packages: expanded, one thing changed, flattened again.
  edit_case() {
    local label="$1" file="$2" from="$3" to="$4" text="$5"
    rm -rf "$d/edit" "$d/edited"
    pkgutil --expand "$pkg" "$d/edit"
    sed -i '' "s|$from|$to|" "$d/edit/$file"
    mkdir -p "$d/edited"
    pkgutil --flatten "$d/edit" "$d/edited/syndeo-0.0.1-aarch64-apple-darwin.pkg"
    out="$(bash "$here/verify-pkg.sh" inspect "$d/edited/syndeo-0.0.1-aarch64-apple-darwin.pkg" 0.0.1 --tarball "$tgz" 2>&1)"
    expect "inspect: $label fails" eval "printf '%s' \"\$out\" | grep -q -- '$text'"
  }
  edit_case "overwrite-permissions true" syndeo.pkg/PackageInfo 'overwrite-permissions="false"' 'overwrite-permissions="true"' "overwrite-permissions)"
  edit_case "an Intel-capable product" Distribution 'hostArchitectures="arm64"' 'hostArchitectures="arm64,x86_64"' "hostArchitectures)"
  edit_case "a product installable anywhere" Distribution 'enable_anywhere="false"' 'enable_anywhere="true"' "enable_anywhere)"
  edit_case "JavaScript in the product" Distribution '<title>' '<installation-check script="true()"/><title>' "count(//installation-check)"

  cp "$pkg" "$d/renamed.pkg"
  out="$(bash "$here/verify-pkg.sh" inspect "$d/renamed.pkg" 0.0.1 --tarball "$tgz" 2>&1)"
  expect "inspect: a misnamed package fails" eval "printf '%s' \"\$out\" | grep -q 'renamed.pkg is not syndeo-0.0.1-aarch64-apple-darwin.pkg'"
  out="$(SYNDEO_EXPECT_SIGNED=yes bash "$here/verify-pkg.sh" inspect "$pkg" 0.0.1 --tarball "$tgz" 2>&1)"
  expect "inspect: SYNDEO_EXPECT_SIGNED=yes is refused until signing is checked" eval "printf '%s' \"\$out\" | grep -q 'judged by the signing step'"
}

self_test() {
  T="$(mktemp -d)"
  # Not local: the EXIT trap runs after this function has returned.
  trap 'chflags -R nouchg "$T" 2>/dev/null; rm -rf "$T"' EXIT
  st_n=0
  st_versions
  mkdir -p "$T/render"
  st_render "$T/render"
  if [ "$(uname -s)" = Darwin ]; then
    st_preinstall
    st_postinstall
    st_atomic
    st_uninstall
    st_installed_mode
    st_location
    st_inspect
  else
    printf '\n  on %s: the root scripts, the package build and inspect are macOS-only (BSD stat, ls -e, mv -h, pkgbuild) and were not run\n' "$(uname -s)"
  fi
  printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
  [ "$wrong" -eq 0 ]
}

case "${1:-}" in
  --self-test) self_test; exit ;;
  inspect)
    [ "$#" = 5 ] && [ "$4" = --tarball ] || { echo "usage: $0 inspect <pkg> <version> --tarball <tarball>" >&2; exit 2; }
    inspect_work="$(mktemp -d)"
    trap 'rm -rf "$inspect_work"' EXIT
    inspect "$2" "$3" "$5"
    status=$?
    [ "$status" = 2 ] && exit 2
    report
    [ "$fail" -eq 0 ]
    ;;
  installed)
    [ "$#" = 2 ] || { echo "usage: $0 installed <version>" >&2; exit 2; }
    installed "$2"
    report
    [ "$fail" -eq 0 ]
    ;;
  *) echo "usage: $0 inspect <pkg> <version> --tarball <tarball> | installed <version> | --self-test" >&2; exit 2 ;;
esac
