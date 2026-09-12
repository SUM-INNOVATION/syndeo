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

binaries=(syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-servo)

# syndeo-webkit embeds WKWebView, so it exists on macOS and nowhere else. It is
# the only one of these that plays video.
case "$target" in
  *-apple-darwin) binaries+=(syndeo-webkit) ;;
esac
for binary in "${binaries[@]}"; do
  if [ ! -x "${release}/${binary}" ]; then
    echo "missing ${release}/${binary}" >&2
    exit 1
  fi
  cp "${release}/${binary}" "${stage}/${binary}"
done

# syndeo-servo is in that list and is most of the weight: Servo, Stylo and
# SpiderMonkey come to about 140 MB on their own, against four for everything
# else put together. It is here because a browser that cannot draw a page is
# not a browser, and leaving it out made every download a reader.

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
