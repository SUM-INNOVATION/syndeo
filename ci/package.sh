#!/usr/bin/env bash
# Lay out one release tarball.
#
#   ci/package.sh <version> <target-triple>
#
# Every binary goes in one flat directory because that is how they find each
# other: the shell spawns its siblings by looking next to itself. Splitting them
# across bin/ and libexec/ would break that for no gain.
set -euo pipefail

version="${1:?version}"
target="${2:?target triple}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release="${root}/target/${target}/release"

name="syndeo-${version}-${target}"
stage="${root}/dist/${name}"
rm -rf "$stage"
mkdir -p "$stage/tools"

binaries=(syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui)
for binary in "${binaries[@]}"; do
  if [ ! -x "${release}/${binary}" ]; then
    echo "missing ${release}/${binary}" >&2
    exit 1
  fi
  cp "${release}/${binary}" "${stage}/${binary}"
done

# syndeo-servo is deliberately not here. It needs the `renderer` feature, which
# is Servo, Stylo and SpiderMonkey: about 1,200 crates and a binary two orders
# of magnitude larger than everything above put together. Built from source by
# whoever wants it.

cp "${root}/README.md" "${root}/LICENSE" "$stage/"
cp "${root}/crates/syndeo-agent/tools/wordcount.wat" "${stage}/tools/"

# Stripped on Linux only: codesign on macOS has already run by this point, and
# strip would invalidate the signature.
case "$target" in
  *-linux-*) strip "${stage}"/syndeo* 2>/dev/null || true ;;
esac

cd "${root}/dist"
tar --format=ustar -czf "${name}.tar.gz" "$name"
rm -rf "$name"

echo "built dist/${name}.tar.gz"
ls -lh "${name}.tar.gz"
