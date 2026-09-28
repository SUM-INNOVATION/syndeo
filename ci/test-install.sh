#!/usr/bin/env bash
# Run install.sh, the real one, against a release made here, with no network.
#
#   ci/test-install.sh
#
# curl, uname and ldconfig are stand-ins: curl hands out files from a local
# directory by the name the installer asks for, uname says whichever machine
# the case is about, and ldconfig lists no libraries. The tarball and its
# SHA256SUMS are built here, holding stand-in binaries, so the installer's
# checksum check, extraction, placement and messages are all the real ones.
#
# Exits non-zero if any case is wrong.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
installer="$here/../install.sh"
version="0.1.4"

root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT

cases=0; wrong=0

stand_ins="$root/stand-ins"
mkdir -p "$stand_ins"
cat >"$stand_ins/curl" <<'EOF'
#!/bin/sh
# curl -fsSL [-H header] <url> -o <file>
url=""; out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -H) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
echo "curl $url" >>"$FAKE_LOG"
[ -f "$FAKE_RELEASE/${url##*/}" ] || exit 22
cp "$FAKE_RELEASE/${url##*/}" "$out"
EOF
cat >"$stand_ins/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo "$FAKE_OS" ;; -m) echo "$FAKE_ARCH" ;; *) echo "$FAKE_OS" ;; esac
EOF
cat >"$stand_ins/ldconfig" <<'EOF'
#!/bin/sh
echo "0 libs found in cache \`/etc/ld.so.cache'"
EOF
chmod 755 "$stand_ins/curl" "$stand_ins/uname" "$stand_ins/ldconfig"

# release <dir> <target> <keystore: works|missing-lib>
# A tarball for <target> and the SHA256SUMS that lists it, in <dir>.
release() {
  local dir="$1" target="$2" keystore="$3" name b
  name="syndeo-${version}-${target}"
  mkdir -p "$dir/$name/tools"
  for b in syndeo syndeo-net syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit; do
    printf '#!/bin/sh\necho "%s %s"\n' "$b" "$version" >"$dir/$name/$b"
  done
  if [ "$keystore" = works ]; then
    printf '#!/bin/sh\necho "syndeo-keystore %s"\n' "$version" >"$dir/$name/syndeo-keystore"
  else
    # What the loader prints, and the status it exits with, when a library
    # the binary needs is not there.
    cat >"$dir/$name/syndeo-keystore" <<'EOF'
#!/bin/sh
echo "syndeo-keystore: error while loading shared libraries: libdbus-1.so.3: cannot open shared object file: No such file or directory" >&2
exit 127
EOF
  fi
  chmod 755 "$dir/$name"/syndeo*
  echo '(module)' >"$dir/$name/tools/wordcount.wat"
  tar -czf "$dir/$name.tar.gz" -C "$dir" "$name"
  rm -rf "${dir:?}/$name"
  (cd "$dir" && if command -v sha256sum >/dev/null 2>&1; then sha256sum "$name.tar.gz"; else shasum -a 256 "$name.tar.gz"; fi) >"$dir/SHA256SUMS"
}

# run_case <name> <os> <arch> <keystore> <login shell> <must contain, "|"-separated> <must not contain, "|"-separated>
run_case() {
  local name="$1" os="$2" arch="$3" keystore="$4" login="$5" want="$6" refuse="$7" dir target status text problems=""
  cases=$((cases+1))
  dir="$root/case-$cases"
  mkdir -p "$dir/release" "$dir/home"
  case "$os" in
    Darwin) target="aarch64-apple-darwin" ;;
    Linux) target="x86_64-unknown-linux-gnu" ;;
  esac
  release "$dir/release" "$target" "$keystore"
  status=0
  FAKE_OS="$os" FAKE_ARCH="$arch" FAKE_RELEASE="$dir/release" FAKE_LOG="$dir/curl.log" \
    PATH="$stand_ins:/usr/bin:/bin" SHELL="$login" HOME="$dir/home" \
    SYNDEO_VERSION="$version" SYNDEO_INSTALL_DIR="$dir/bin" SYNDEO_HOME="$dir/data" \
    sh "$installer" >"$dir/output.log" 2>&1 || status=$?
  [ "$status" -eq 0 ] || problems="$problems; exited $status"
  [ -x "$dir/bin/syndeo-keystore" ] || problems="$problems; the keystore was not installed"
  while [ -n "$want" ]; do
    text="${want%%|*}"
    grep -qF -- "$text" "$dir/output.log" || problems="$problems; no line with '$text'"
    [ "$want" = "$text" ] && break
    want="${want#*|}"
  done
  while [ -n "$refuse" ]; do
    text="${refuse%%|*}"
    if grep -qF -- "$text" "$dir/output.log"; then problems="$problems; a line with '$text'"; fi
    [ "$refuse" = "$text" ] && break
    refuse="${refuse#*|}"
  done
  if [ -z "$problems" ]; then
    printf '  ok    %s\n' "$name"
  else
    printf '  WRONG %s%s\n' "$name" "$problems"
    sed 's/^/          | /' "$dir/output.log"
    wrong=$((wrong+1))
  fi
}

printf '\n  the keystore on Linux\n'
run_case "Linux, keystore missing libdbus: warned, installed, exit 0" \
  Linux x86_64 missing-lib /bin/bash \
  "warning: syndeo-keystore did not start (exit 127): syndeo-keystore: error while loading shared libraries: libdbus-1.so.3|sudo apt-get install libdbus-1-3|sudo dnf install dbus-libs|ldconfig does not list libdbus-1.so.3|installed to" \
  ""
run_case "Linux, keystore works: no warning" \
  Linux x86_64 works /bin/bash \
  "installed to" \
  "warning:|libdbus"
run_case "macOS: the keystore is not probed" \
  Darwin arm64 missing-lib /bin/zsh \
  "installed to" \
  "warning:|libdbus"

printf '\n  where the PATH line goes\n'
run_case "zsh on macOS" Darwin arm64 works /bin/zsh \
  ">> ~/.zshrc" ">> ~/.profile|>> ~/.bash"
run_case "zsh on Linux" Linux x86_64 works /usr/bin/zsh \
  ">> ~/.zshrc" ">> ~/.profile|>> ~/.bash"
run_case "bash on macOS reads .bash_profile" Darwin arm64 works /bin/bash \
  ">> ~/.bash_profile" ">> ~/.bashrc|>> ~/.profile"
run_case "bash on Linux reads .bashrc" Linux x86_64 works /bin/bash \
  ">> ~/.bashrc" ">> ~/.bash_profile|>> ~/.profile"
run_case "fish has its own command" Linux x86_64 works /usr/bin/fish \
  "fish_add_path " ">> ~/"
run_case "anything else gets .profile" Linux x86_64 works /bin/dash \
  ">> ~/.profile" ">> ~/.bash|>> ~/.zshrc"
run_case "no SHELL at all gets .profile" Linux x86_64 works "" \
  ">> ~/.profile" ">> ~/.bash|>> ~/.zshrc"

printf '\n  %d cases, %d wrong\n\n' "$cases" "$wrong"
[ "$wrong" -eq 0 ]
