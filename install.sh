#!/bin/sh
# Install Syndeo.
#
#   curl -fsSL https://raw.githubusercontent.com/SUM-INNOVATION/syndeo/main/install.sh | sh
#
# Downloads the release tarball for this machine, checks it against the
# published SHA256SUMS, and puts the binaries somewhere on PATH. No sudo: the
# default target is under your own home directory, and nothing here writes
# outside it.
#
# Knobs, all optional:
#
#   SYNDEO_VERSION      a version to pin, e.g. 0.1.0 (default: the latest release)
#   SYNDEO_INSTALL_DIR  where the binaries go     (default: ~/.local/bin)
#   SYNDEO_HOME         where data goes           (default: ~/.syndeo)
#   GITHUB_TOKEN        for a repository you reach with credentials
set -eu

REPO="SUM-INNOVATION/syndeo"
INSTALL_DIR="${SYNDEO_INSTALL_DIR:-$HOME/.local/bin}"
DATA_DIR="${SYNDEO_HOME:-$HOME/.syndeo}"
BINARIES="syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui"
# syndeo-webkit exists on macOS only, and is installed from 0.1.3 on: the one
# in 0.1.2's tarball let a redirect's destination run as the site that
# redirected to it, so an install of 0.1.2 or earlier leaves it out.
WEBKIT_SINCE="0.1.3"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required and was not found on PATH."
}

# --- what machine is this -----------------------------------------------------

detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Darwin)
            case "$arch" in
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                x86_64)
                    die "Intel Macs have no prebuilt release yet. Build from source:
  git clone https://github.com/${REPO}.git && cd syndeo && cargo build --release" ;;
                *) die "unsupported macOS architecture: $arch" ;;
            esac
            ;;
        Linux)
            case "$arch" in
                x86_64|amd64) echo "x86_64-unknown-linux-gnu" ;;
                aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
                *) die "unsupported Linux architecture: $arch" ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT)
            die "Windows is not supported yet. It is tracked at
  https://github.com/${REPO}/issues" ;;
        *)
            die "unsupported operating system: $os" ;;
    esac
}

# --- fetching -----------------------------------------------------------------

fetch() {
    # fetch <url> <destination>
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        curl -fsSL -H "Authorization: Bearer ${GITHUB_TOKEN}" "$1" -o "$2"
    else
        curl -fsSL "$1" -o "$2"
    fi
}

latest_version() {
    api="https://api.github.com/repos/${REPO}/releases/latest"
    body="$(mktemp)"
    if ! fetch "$api" "$body"; then
        rm -f "$body"
        die "could not reach the release list for ${REPO}.
If the repository is private, set GITHUB_TOKEN, or pin SYNDEO_VERSION and
download the tarball yourself."
    fi
    tag="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$body" | head -n 1)"
    rm -f "$body"
    [ -n "$tag" ] || die "no releases published for ${REPO} yet."
    printf '%s\n' "${tag#v}"
}

# version_at_least <version> <minimum>
#
# Compares major.minor.patch as numbers, ignoring anything after a `-` or `+`
# (0.1.3-rc.1 counts as 0.1.3). Anything that is not three numeric fields is
# not at least anything, so a malformed version can only ever mean less.
version_at_least() {
    have="${1%%[-+]*}"
    want="$2"
    case "$have" in
        *[!0-9.]* | .* | *. | *..*) return 1 ;;
    esac
    case "$have" in
        *.*.*.*) return 1 ;;
        *.*.*) ;;
        *) return 1 ;;
    esac
    have_major="${have%%.*}"; rest="${have#*.}"
    have_minor="${rest%%.*}"; have_patch="${rest#*.}"
    want_major="${want%%.*}"; rest="${want#*.}"
    want_minor="${rest%%.*}"; want_patch="${rest#*.}"
    [ "$have_major" -ne "$want_major" ] && { [ "$have_major" -gt "$want_major" ]; return; }
    [ "$have_minor" -ne "$want_minor" ] && { [ "$have_minor" -gt "$want_minor" ]; return; }
    [ "$have_patch" -ge "$want_patch" ]
}

# profile_for <login shell> <operating system>
#
# The file an `export PATH=...` line belongs in for that shell, as a user would
# type it, or `fish` for fish, which has a command of its own for this.
profile_for() {
    case "${1##*/}" in
        zsh) echo '~/.zshrc' ;;
        # macOS Terminal opens login shells, which read .bash_profile and not
        # .bashrc; a Linux terminal opens interactive ones, which do the reverse.
        bash) if [ "$2" = Darwin ]; then echo '~/.bash_profile'; else echo '~/.bashrc'; fi ;;
        fish) echo fish ;;
        *) echo '~/.profile' ;;
    esac
}

# keystore_check <install dir> <scratch dir>
#
# On Linux, whether the keystore can start at all. It links libdbus-1.so.3 for
# the credential store, and a machine without it gets a keystore that the
# loader refuses before main, which every later command reported only as a
# ten-second timeout. A warning, not a failure: browsing, the proxy and the
# window work without the keystore.
keystore_check() {
    [ "$(uname -s)" = Linux ] || return 0
    status=0
    "$1/syndeo-keystore" --version >/dev/null 2>"$2/keystore.err" || status=$?
    [ "$status" -eq 0 ] && return 0
    first="$(head -n 1 "$2/keystore.err")"
    say "warning: syndeo-keystore did not start (exit ${status}): ${first:-no message}"
    say "  Keys, signing and site identities need it; browsing does not."
    say "  The usual cause is a missing libdbus-1.so.3. Install it with"
    say "    sudo apt-get install libdbus-1-3     (Debian, Ubuntu)"
    say "    sudo dnf install dbus-libs           (Fedora)"
    say "  and check with: syndeo-keystore --version"
    if command -v ldconfig >/dev/null 2>&1; then
        if ! ldconfig -p 2>/dev/null | grep -q 'libdbus-1\.so\.3'; then
            say "  (ldconfig does not list libdbus-1.so.3 on this machine.)"
        fi
    fi
    say ""
}

verify() {
    # verify <tarball> <sums file> <name inside the sums file>
    # Matched as a whole field rather than by pattern: a file name has dots in
    # it, and a regular expression would let a near-miss through.
    expected="$(awk -v want="$3" '$2 == want { print $1 }' "$2")"
    [ -n "$expected" ] || die "$3 is not listed in SHA256SUMS; refusing to install it."

    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$1" | cut -d' ' -f1)"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$1" | cut -d' ' -f1)"
    else
        die "neither sha256sum nor shasum is available; cannot verify the download."
    fi

    [ "$expected" = "$actual" ] || die "checksum mismatch for $3.
  expected $expected
  got      $actual
Do not run it. Report this."
}

# --- go -----------------------------------------------------------------------

need curl
need tar

target="$(detect_target)"
version="${SYNDEO_VERSION:-}"
[ -n "$version" ] || version="$(latest_version)"
version="${version#v}"

name="syndeo-${version}-${target}"
base="https://github.com/${REPO}/releases/download/v${version}"

webkit=no
case "$target" in
    *-apple-darwin)
        if version_at_least "$version" "$WEBKIT_SINCE"; then
            webkit=yes
        fi
        ;;
esac
install_list="$BINARIES"
if [ "$webkit" = yes ]; then
    install_list="$install_list syndeo-webkit"
fi

say "Syndeo ${version} for ${target}"

work="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$work'" EXIT INT TERM

say "  downloading"
fetch "${base}/${name}.tar.gz" "${work}/${name}.tar.gz" \
    || die "no release asset ${name}.tar.gz at ${base}"
fetch "${base}/SHA256SUMS" "${work}/SHA256SUMS" \
    || die "no SHA256SUMS at ${base}; refusing to install an unverified binary."

say "  verifying"
verify "${work}/${name}.tar.gz" "${work}/SHA256SUMS" "${name}.tar.gz"

say "  extracting"
tar -xzf "${work}/${name}.tar.gz" -C "$work"
[ -d "${work}/${name}" ] || die "the tarball did not contain ${name}/"

# Every binary lands in one directory because that is how the shell finds the
# others: it looks beside itself before it looks at PATH. Scattering them is
# the one way to get a working install that cannot start the net process.
mkdir -p "$INSTALL_DIR"
for binary in $install_list; do
    [ -f "${work}/${name}/${binary}" ] || die "${binary} missing from the tarball."
done
for binary in $install_list; do
    # Written to a neighbouring name and moved into place, so an install over a
    # running Syndeo replaces the file rather than writing through it.
    cp "${work}/${name}/${binary}" "${INSTALL_DIR}/.${binary}.new"
    chmod 755 "${INSTALL_DIR}/.${binary}.new"
    mv -f "${INSTALL_DIR}/.${binary}.new" "${INSTALL_DIR}/${binary}"
done

# Only now, with the requested version verified and in place: a syndeo-webkit
# left by a newer install would run against this older proxy, so it goes.
if [ "$webkit" = no ] && [ -e "${INSTALL_DIR}/syndeo-webkit" ]; then
    rm -f "${INSTALL_DIR}/syndeo-webkit"
    say "  removed ${INSTALL_DIR}/syndeo-webkit, which this version does not include"
fi

# The example WebAssembly tool, only if there is nothing there already: this
# directory is the user's, and an upgrade has no business overwriting it.
mkdir -p "${DATA_DIR}/tools"
if [ ! -e "${DATA_DIR}/tools/wordcount.wat" ]; then
    cp "${work}/${name}/tools/wordcount.wat" "${DATA_DIR}/tools/wordcount.wat"
fi

say "  installed to ${INSTALL_DIR}"
say ""

keystore_check "$INSTALL_DIR" "$work"

case ":${PATH}:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
        say "${INSTALL_DIR} is not on your PATH. Add it:"
        say ""
        profile="$(profile_for "${SHELL:-sh}" "$(uname -s)")"
        if [ "$profile" = fish ]; then
            say "  fish_add_path ${INSTALL_DIR}"
        else
            say "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ${profile}"
        fi
        say ""
        ;;
esac

say "Next:"
say ""
say "  syndeo browse https://www.rust-lang.org/ --twice   # the second fetch says cache"
say "  syndeo-ui https://www.rust-lang.org/               # the window"
if [ "$webkit" = yes ]; then
    say "  syndeo-proxy ca --trust                            # once, before the next line"
    say "  syndeo-webkit https://www.rust-lang.org/           # the page, rendered, with video"
fi
say "  syndeo-keystore init                               # keys, if you want them"
say "  syndeo doctor                                      # what is set up, and where"
say ""
