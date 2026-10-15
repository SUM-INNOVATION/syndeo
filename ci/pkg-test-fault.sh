#!/usr/bin/env bash
# Make a faulty copy of a stand-in package, for the tests only.
#
#   ci/pkg-test-fault.sh <pkg> <out-pkg> <entry|mid-removal> <marker>
#   ci/pkg-test-fault.sh --script <postinstall> <entry|mid-removal> <marker>
#
# The copy is <pkg> with one block added to its postinstall, and nothing else
# different: it is expanded with pkgutil, changed, flattened again, and
# checked against <pkg> part by part. entry puts the block before the
# postinstall's first command, so the upgrade fails with the new bundle in
# place and the old receipt; mid-removal puts it in the removal of the v0.1.x
# command-line package, just after its seven command links are removed, so
# that removal stops part way. The block fails the first run and lets every
# later run through, using <marker>.fired, so installing the same package file
# again is the retry an administrator would make. Every run, failing or not,
# appends what it finds to <marker>.seen.
#
# Only 0.0.x stand-ins, which no release uses, and the package checks fail
# any package whose scripts differ from the templates. The builder itself has
# no way to make one.
#
# --script adds the same block, in place, to a postinstall rendered for the
# self-test's temporary root, so that what the block says can be checked
# directly, without installing anything.
set -euo pipefail

die() {
  printf 'pkg-test-fault: %s\n' "$*" >&2
  exit 1
}

# insert_fault SCRIPT WHERE MARKER: add the block to the postinstall SCRIPT.
# Prints how many lines it added.
insert_fault() {
  python3 -I - "$1" "$2" "$3" <<'PYEOF' || die "changing the postinstall"
import os
import sys

path, where, marker = sys.argv[1:4]
record = (
    "{{ /bin/echo \"run at $(/bin/date -u '+%H:%M:%S') ({where})\"; "
    "/usr/bin/plutil -extract CFBundleShortVersionString raw /Applications/Syndeo.app/Contents/Info.plist; "
    "/bin/ls -la /usr/local/bin /usr/local/libexec/syndeo; }} >>'{marker}.seen' 2>&1 || :"
).format(where=where, marker=marker)
lines = open(path).read().split("\n")
if where == "entry":
    anchor = "set -eu"
    condition = "[ ! -e '{marker}.fired' ]".format(marker=marker)
    indent = ""
    if lines.count(anchor) != 1:
        sys.exit("set -eu is not in the postinstall exactly once")
    at = lines.index(anchor)
else:
    # Inside the removal's loop: step 7 is the last of the seven links.
    anchor = '    removed="$removed $p"'
    condition = "[ \"$i\" = 7 ] && [ ! -e '{marker}.fired' ]".format(marker=marker)
    indent = "    "
    if lines.count(anchor) != 1:
        sys.exit("the removal's step is not in the postinstall exactly once")
    at = lines.index(anchor) + 1
block = [
    "# Test fault ({where}): records what it finds, then fails the first run only.".format(where=where),
    record,
    "if {condition}; then".format(condition=condition),
    "    : >'{marker}.fired'".format(marker=marker),
    "    /bin/echo \"syndeo postinstall: test fault {where}: failing this first run\"".format(where=where),
    "    exit 1",
    "fi",
]
lines[at:at] = [indent + b for b in block]
tmp = path + ".new"
with open(tmp, "w") as f:
    f.write("\n".join(lines))
os.chmod(tmp, 0o755)
os.rename(tmp, path)
PYEOF
}

check_where_and_marker() {
  case "$1" in entry | mid-removal) ;; *) die "no fault at '$1'" ;; esac
  case "$2" in /*) ;; *) die "the marker has to be absolute" ;; esac
  [[ "$2" =~ ^[A-Za-z0-9._/-]+$ ]] || die "the marker has to be a plain path"
}

# gained ORIGINAL CHANGED: the change is the block, seven lines added and none
# lost.
gained() {
  [ "$(diff "$1" "$2" | grep -c '^>')" = 7 ] || die "the postinstall gained more than the block"
  [ "$(diff "$1" "$2" | grep -c '^<')" = 0 ] || die "the postinstall lost a line"
}

if [ "${1:-}" = --script ]; then
  [ "$#" = 4 ] || die "usage: $0 --script <postinstall> <entry|mid-removal> <marker>"
  check_where_and_marker "$3" "$4"
  [ -f "$2" ] || die "no $2"
  original="$(mktemp)"
  trap 'rm -f "$original"' EXIT
  cp -p "$2" "$original"
  insert_fault "$2" "$3" "$4"
  gained "$original" "$2"
  exit 0
fi

[ "$#" = 4 ] || die "usage: $0 <pkg> <out-pkg> <entry|mid-removal> <marker>"
pkg="$1"
out="$2"
where="$3"
marker="$4"
check_where_and_marker "$where" "$marker"
case "$(basename "$pkg")" in syndeo-0.0.*-aarch64-apple-darwin.pkg) ;; *) die "only a 0.0.x stand-in" ;; esac
[ -e "$out" ] && die "$out exists"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
pkgutil --expand "$pkg" "$work/expanded" || die "expanding $pkg"
script="$work/expanded/Syndeo.pkg/Scripts/postinstall"
cp -p "$script" "$work/postinstall.original"
insert_fault "$script" "$where" "$marker"
pkgutil --flatten "$work/expanded" "$out" || die "flattening $out"

# Nothing but the postinstall differs from the package it was made from.
pkgutil --expand "$out" "$work/check" || die "expanding $out"
pkgutil --expand "$pkg" "$work/original" || die "expanding $pkg again"
for part in Distribution Syndeo.pkg/Bom Syndeo.pkg/Payload Syndeo.pkg/PackageInfo Syndeo.pkg/Scripts/preinstall; do
  cmp -s "$work/original/$part" "$work/check/$part" || die "$part differs from $pkg's"
done
cmp -s "$script" "$work/check/Syndeo.pkg/Scripts/postinstall" || die "the postinstall did not survive flattening"
gained "$work/postinstall.original" "$script"
