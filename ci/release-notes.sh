#!/usr/bin/env bash
# The notes a release is published with.
#
#   ci/release-notes.sh <version> <signed: yes|no>
#
# Two ways to install, which the notes keep apart: the per-user install
# script, from the tarball, into ~/.local/bin, with no sudo, for the
# command-line tools; and, on macOS, the installer package of Syndeo.app,
# into /Applications, which needs an administrator, runs a preinstall and a
# postinstall script, and removes the discontinued v0.1.x command-line
# package if it is installed exactly as its package left it. Then the
# signing state, stated exactly for what was built, and the CHANGELOG's
# section for the version, when it has one. The release job writes these, and
# checks the draft carries exactly them before publishing it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=ci/macos-pkg/version.sh
. "$here/macos-pkg/version.sh"

[ "$#" = 2 ] || { echo "usage: $0 <version> <signed: yes|no>" >&2; exit 2; }
version="$1"
signed="$2"
syndeo_version_valid "$version" || { echo "release-notes: '$version' is not a version" >&2; exit 2; }
case "$signed" in yes | no) ;; *) echo "release-notes: signed must be yes or no, not '$signed'" >&2; exit 2 ;; esac
repo="${GITHUB_REPOSITORY:-SUM-INNOVATION/syndeo}"
pkg="syndeo-${version}-aarch64-apple-darwin.pkg"
download="https://github.com/${repo}/releases/download/v${version}"

cat <<EOF
## Install

### For one user, on macOS or Linux

\`\`\`sh
curl -fsSL https://raw.githubusercontent.com/${repo}/v${version}/install.sh | sh
\`\`\`

macOS on Apple Silicon, and Linux on x86_64 or arm64. It downloads the
tarball for your machine, checks it against \`SHA256SUMS\`, and puts the
binaries in \`~/.local/bin\`. No \`sudo\`, and nothing written outside your home
directory.

### Syndeo.app, for the whole Mac: the installer package

\`\`\`sh
curl -fsSLO ${download}/${pkg}
curl -fsSLO ${download}/SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing
sudo installer -pkg ${pkg} -target /
\`\`\`

macOS 13 or later, on Apple Silicon, and an administrator. It installs
Syndeo.app in \`/Applications\`, and nothing else: the command-line tools come
from the tarball and the install script above, and no longer from this
package. Quit Syndeo before installing or upgrading. If the v0.1.6 or v0.1.7
command-line package is installed, exactly as its package left it, this
package removes it: its commands in \`/usr/local/bin\`, its files under
\`/usr/local/libexec\`, and its receipt. If it is installed in any other
state, this package refuses, and touches none of it. To remove Syndeo.app,
either run \`sudo /bin/sh /Applications/Syndeo.app/Contents/Resources/uninstall.sh\`,
which also forgets the package's receipt, or move it to the Trash. Your
settings and data are kept either way.

### Signing

EOF
if [ "$signed" = yes ]; then
  cat <<'EOF'
The macOS binaries and Syndeo.app are
signed with a Developer ID and notarized, Syndeo.app with its ticket stapled.
The installer package is signed with a Developer ID Installer identity and
notarized, with its notarization ticket stapled.
EOF
else
  cat <<'EOF'
The macOS binaries and Syndeo.app are
ad-hoc signed, with no Developer ID, and not notarized.
The installer package is unsigned: Gatekeeper rejects it when it is opened
from a browser download, so install it with the commands above.
EOF
fi
cat <<'EOF'

Verify every download against `SHA256SUMS` before running it.
EOF
if [ -f "$here/../CHANGELOG.md" ]; then
  section="$(awk -v v="$version" '$0 == "## " v {f=1; next} /^## /{f=0} f' "$here/../CHANGELOG.md")"
  if [ -n "$section" ]; then
    printf '\n%s\n' "$section"
  fi
fi
