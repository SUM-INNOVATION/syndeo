#!/usr/bin/env bash
# Build the macOS installer package from the release tarball.
#
#   ci/package-macos-pkg.sh <version> <tarball> <out-dir>
#   ci/package-macos-pkg.sh --render <preinstall|postinstall|uninstall> \
#       <version> <root> <uid> <gid> <pkgutil> <out-file>
#
# The package installs what the tarball holds, unchanged, into
# /usr/local/libexec/syndeo/<version>/, with uninstall.sh beside it, and links
# each of the seven commands in /usr/local/bin to
# ../libexec/syndeo/current/<name>. `current` is not in the payload: the
# postinstall step switches it to the new version, once, after every file is
# in place. One version is installed at a time: on an upgrade Installer
# removes the version the receipt names before it places this one, so the
# commands do not start from that removal until the switch. See ci/macos-pkg/
# for the scripts, what each refuses, and what an upgrade leaves if it fails.
#
# The payload is built the same way from the same tarball every time: fixed
# modes, owners root:wheel, no extended attributes, and every timestamp the
# release commit's. The .pkg file itself is not byte-for-byte reproducible:
# its archive carries its own creation time.
#
# --render writes one of the three scripts with the given literals. Every
# package uses root '', uid 0, gid 0 and /usr/sbin/pkgutil; the self-tests in
# ci/verify-pkg.sh use a temporary root and a stand-in pkgutil.
#
# No package this builds carries anything for tests. The tests make their own
# faulty packages from a stand-in, by changing its postinstall (see
# ci/pkg-test-fault.sh), and the package checks fail any package whose
# scripts differ from the templates.
set -euo pipefail
# Modes in the payload are set explicitly; this fixes the ones that are not,
# such as a symlink's, so the BOM is the same whoever builds it.
umask 022

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
templates="$here/macos-pkg"
NAMES=(syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit)

# shellcheck source=ci/macos-pkg/version.sh
. "$templates/version.sh"

die() {
    printf 'package-macos-pkg: %s\n' "$*" >&2
    exit 1
}

# render KIND VERSION ROOT UID GID PKGUTIL OUT
render() {
    local kind="$1" version="$2" root="$3" uid="$4" gid="$5" pkgutil="$6" out="$7"
    local template
    case "$kind" in
        preinstall | postinstall) template="$templates/$kind.in" ;;
        uninstall) template="$templates/uninstall.sh.in" ;;
        *) die "no script called $kind" ;;
    esac
    syndeo_version_valid "$version" || die "'$version' is not a version"
    # Each literal goes into the script between single quotes and through sed,
    # so none may hold a quote, a backslash, a newline, & or |.
    [[ "$root" =~ ^(/[A-Za-z0-9._\ -]+)*$ ]] || die "unusable root '$root'"
    [[ "$uid" =~ ^[0-9]+$ && "$gid" =~ ^[0-9]+$ ]] || die "unusable uid or gid"
    [[ "$pkgutil" =~ ^(/[A-Za-z0-9._\ -]+)+$ ]] || die "unusable pkgutil path '$pkgutil'"
    awk -v version_sh="$templates/version.sh" -v common="$templates/common.sh" '
        $0 == "@COMMON@" {
            while ((getline line < version_sh) > 0) print line
            close(version_sh)
            print ""
            while ((getline line < common) > 0) print line
            close(common)
            next
        }
        { print }
    ' "$template" | sed \
        -e "s|@VERSION@|$version|g" \
        -e "s|@ROOT@|$root|g" \
        -e "s|@EXPECT_UID@|$uid|g" \
        -e "s|@EXPECT_GID@|$gid|g" \
        -e "s|@PKGUTIL@|$pkgutil|g" >"$out"
    if grep -n '@[A-Z_]*@' "$out" >&2; then
        die "a placeholder is left in $out"
    fi
    chmod 0755 "$out"
}

build() {
    local version="$1" tarball="$2" out="$3"
    syndeo_version_valid "$version" || die "'$version' is not a version"
    local name="syndeo-$version-aarch64-apple-darwin"
    [ "$(basename "$tarball")" = "$name.tar.gz" ] || die "expected $name.tar.gz, not $(basename "$tarball")"
    [ -f "$tarball" ] || die "no $tarball"
    mkdir -p "$out"

    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT

    # Exactly what the tarball should hold, and nothing that is a link.
    mkdir "$work/x"
    tar -xzf "$tarball" -C "$work/x"
    local want have
    want=$(
        {
            printf '%s\n' . "./$name" "./$name/README.md" "./$name/LICENSE" "./$name/tools" "./$name/tools/wordcount.wat"
            for b in "${NAMES[@]}"; do printf '%s\n' "./$name/$b"; done
        } | LC_ALL=C sort
    )
    have=$(cd "$work/x" && find . | LC_ALL=C sort)
    [ "$want" = "$have" ] || die "the tarball does not hold exactly the release's files:
$(diff <(printf '%s\n' "$want") <(printf '%s\n' "$have") || true)"
    [ -z "$(find "$work/x" -type l)" ] || die "the tarball holds a symlink"
    for b in "${NAMES[@]}" README.md LICENSE tools/wordcount.wat; do
        [ -f "$work/x/$name/$b" ] || die "$b in the tarball is not a regular file"
    done

    # The payload.
    local stage="$work/root"
    local tree="$stage/usr/local/libexec/syndeo/$version"
    mkdir -p "$tree/tools" "$stage/usr/local/bin"
    for b in "${NAMES[@]}"; do
        cp "$work/x/$name/$b" "$tree/$b"
        chmod 0755 "$tree/$b"
    done
    for f in README.md LICENSE tools/wordcount.wat; do
        cp "$work/x/$name/$f" "$tree/$f"
        chmod 0644 "$tree/$f"
    done
    render uninstall "$version" '' 0 0 /usr/sbin/pkgutil "$tree/uninstall.sh"
    for b in "${NAMES[@]}"; do
        ln -s "../libexec/syndeo/current/$b" "$stage/usr/local/bin/$b"
    done
    for d in "$stage" "$stage/usr" "$stage/usr/local" "$stage/usr/local/bin" \
        "$stage/usr/local/libexec" "$stage/usr/local/libexec/syndeo" "$tree" "$tree/tools"; do
        chmod 0755 "$d"
    done
    xattr -crs "$stage"
    local epoch stamp
    epoch="${SOURCE_DATE_EPOCH:-$(git -C "$repo" log -1 --format=%ct)}"
    stamp="$(date -u -r "$epoch" +%Y%m%d%H%M.%S)"
    find "$stage" -exec env TZ=UTC touch -h -t "$stamp" {} +

    mkdir "$work/scripts" "$work/component"
    render preinstall "$version" '' 0 0 /usr/sbin/pkgutil "$work/scripts/preinstall"
    render postinstall "$version" '' 0 0 /usr/sbin/pkgutil "$work/scripts/postinstall"

    /usr/bin/pkgbuild --quiet --root "$stage" --identifier com.sum.syndeo.pkg --version "$version" \
        --install-location / --ownership recommended --info "$templates/PackageInfo.xml" \
        --scripts "$work/scripts" "$work/component/syndeo.pkg"

    # The two things the design rests on, checked rather than assumed: the
    # info template kept existing directories' modes, and the scripts are in.
    pkgutil --expand "$work/component/syndeo.pkg" "$work/check"
    grep -q 'overwrite-permissions="false"' "$work/check/PackageInfo" ||
        die "pkgbuild did not keep overwrite-permissions=\"false\"; stopping, as the plan requires"
    if ! grep -q '<preinstall file="./preinstall"' "$work/check/PackageInfo" ||
        ! grep -q '<postinstall file="./postinstall"' "$work/check/PackageInfo"; then
        die "pkgbuild did not record both scripts; stopping, as the plan requires"
    fi

    sed "s|@VERSION@|$version|g" "$templates/Distribution.xml.in" >"$work/Distribution.xml"
    /usr/bin/productbuild --quiet --distribution "$work/Distribution.xml" \
        --package-path "$work/component" "$out/$name.pkg"
    echo "built $out/$name.pkg"
    shasum -a 256 "$out/$name.pkg"
}

case "${1:-}" in
    --render)
        shift
        [ "$#" -eq 7 ] || die "--render <kind> <version> <root> <uid> <gid> <pkgutil> <out-file>"
        render "$1" "$2" "$3" "$4" "$5" "$6" "$7"
        ;;
    '' | -*)
        die "usage: $0 <version> <tarball> <out-dir>"
        ;;
    *)
        [ "$#" -eq 3 ] || die "usage: $0 <version> <tarball> <out-dir>"
        build "$1" "$2" "$3"
        ;;
esac
