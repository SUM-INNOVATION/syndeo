#!/usr/bin/env bash
# Make a faulty copy of a stand-in package, for the tests only.
#
#   ci/pkg-test-fault.sh <pkg> <out-pkg> <entry|after-switch> <marker>
#
# The copy is <pkg> with one block added to its postinstall, before its first
# command (entry) or just after it switches `current` (after-switch), and
# nothing else different: it is expanded with pkgutil, changed, flattened
# again, and checked against <pkg> part by part. The block fails the first run
# and lets every later run through, using <marker>.fired, so installing the
# same package file again is the retry an administrator would make. Every
# run, failing or not, appends what it finds to <marker>.seen.
#
# Only 0.0.x stand-ins, which no release uses, and the package checks fail
# any package whose scripts differ from the templates. The builder itself has
# no way to make one.
set -euo pipefail

die() {
  printf 'pkg-test-fault: %s\n' "$*" >&2
  exit 1
}

[ "$#" = 4 ] || die "usage: $0 <pkg> <out-pkg> <entry|after-switch> <marker>"
pkg="$1"
out="$2"
where="$3"
marker="$4"
case "$where" in entry | after-switch) ;; *) die "no fault at '$where'" ;; esac
case "$(basename "$pkg")" in syndeo-0.0.*-aarch64-apple-darwin.pkg) ;; *) die "only a 0.0.x stand-in" ;; esac
case "$marker" in /*) ;; *) die "the marker has to be absolute" ;; esac
[[ "$marker" =~ ^[A-Za-z0-9._/-]+$ ]] || die "the marker has to be a plain path"
[ -e "$out" ] && die "$out exists"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
pkgutil --expand "$pkg" "$work/expanded" || die "expanding $pkg"
script="$work/expanded/syndeo.pkg/Scripts/postinstall"
cp -p "$script" "$work/postinstall.original"
python3 -I - "$script" "$where" "$marker" <<'PYEOF' || die "changing the postinstall"
import os
import sys

path, where, marker = sys.argv[1:4]
block = """\
# Test fault ({where}): records what it finds, then fails the first run only.
{{ /bin/echo "run at $(/bin/date -u '+%H:%M:%S') ({where})"; /bin/ls -la /usr/local/libexec/syndeo; /bin/echo "current -> $(/usr/bin/readlink /usr/local/libexec/syndeo/current)"; }} >>'{marker}.seen' 2>&1 || :
if [ ! -e '{marker}.fired' ]; then
    : >'{marker}.fired'
    /bin/echo "syndeo postinstall: test fault {where}: failing this first run"
    exit 1
fi
""".format(where=where, marker=marker)
lines = open(path).read().split("\n")
if where == "entry":
    anchor = "set -eu"
    if lines.count(anchor) != 1:
        sys.exit("set -eu is not in the postinstall exactly once")
    at = lines.index(anchor)
else:
    anchor = '/bin/mv -h -f "$SYNDEO_DIR/.current.new" "$SYNDEO_DIR/current"'
    if lines.count(anchor) != 1:
        sys.exit("the switch is not in the postinstall exactly once")
    at = lines.index(anchor) + 1
lines[at:at] = block.rstrip("\n").split("\n")
tmp = path + ".new"
with open(tmp, "w") as f:
    f.write("\n".join(lines))
os.chmod(tmp, 0o755)
os.rename(tmp, path)
PYEOF
pkgutil --flatten "$work/expanded" "$out" || die "flattening $out"

# Nothing but the postinstall differs from the package it was made from.
pkgutil --expand "$out" "$work/check" || die "expanding $out"
pkgutil --expand "$pkg" "$work/original" || die "expanding $pkg again"
for part in Distribution syndeo.pkg/Bom syndeo.pkg/Payload syndeo.pkg/PackageInfo syndeo.pkg/Scripts/preinstall; do
  cmp -s "$work/original/$part" "$work/check/$part" || die "$part differs from $pkg's"
done
cmp -s "$script" "$work/check/syndeo.pkg/Scripts/postinstall" || die "the postinstall did not survive flattening"
[ "$(diff "$work/postinstall.original" "$script" | grep -c '^>')" = 7 ] || die "the postinstall gained more than the block"
