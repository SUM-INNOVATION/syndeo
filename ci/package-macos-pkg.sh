#!/usr/bin/env bash
# Build the macOS installer package from a signed Syndeo.app.
#
#   ci/package-macos-pkg.sh <version> <Syndeo.app> <out-dir>
#   ci/package-macos-pkg.sh --render <preinstall|postinstall|uninstall> \
#       <version> <root> <uid> <gid> <pkgutil> <codesign> <out-file>
#   ci/package-macos-pkg.sh --library <out-file>
#
# The package installs /Applications/Syndeo.app and nothing else: no command
# in /usr/local, no link, no other file. Its component identifier is
# com.sum.syndeo.app, which no earlier package used; the discontinued v0.1.x
# command-line package was com.sum.syndeo.pkg, and this package's
# postinstall removes that one explicitly, never through Installer's own
# upgrade of a receipt. The command-line tools ship in the release tarball,
# for install.sh, as before. See ci/macos-pkg/ for the scripts, what each
# refuses, and what an upgrade leaves if it fails.
#
# The bundle has to be exactly Syndeo.app <version> as
# ci/package-macos-app.sh assembles it, signed (ad hoc in a pull request, or
# Developer ID and stapled in a release), its signature verifying strictly
# and deeply. It goes into the payload unchanged but for owners root:wheel,
# no extended attributes, and every timestamp the release commit's. The .pkg
# file itself is not byte-for-byte reproducible: its archive carries its own
# creation time.
#
# --render writes one of the three scripts with the given literals. Every
# package uses root '', uid 0, gid 0, /usr/sbin/pkgutil and /usr/bin/codesign;
# ci/package-macos-app.sh renders the uninstaller into the bundle the same
# way, and the self-tests in ci/verify-pkg.sh use a temporary root and
# stand-ins. --library writes only the libraries every script embeds, for
# ci/verify-pkg.sh to make the same checks with.
#
# No package this builds carries anything for tests. The tests make their own
# faulty packages from a stand-in, by changing its postinstall (see
# ci/pkg-test-fault.sh), and the package checks fail any package whose
# scripts differ from the templates.
set -euo pipefail
# Modes in the payload are set explicitly; this fixes the ones that are not,
# so the BOM is the same whoever builds it.
umask 022

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"
templates="$here/macos-pkg"

# shellcheck source=ci/macos-pkg/version.sh
. "$templates/version.sh"

die() {
    printf 'package-macos-pkg: %s\n' "$*" >&2
    exit 1
}

# libraries: version.sh, common.sh and app.sh, as every script embeds them,
# with the bundle's manifest where app.sh says @APP_MANIFEST@.
libraries() {
    awk -v manifest="$here/macos-app/bundle.manifest" '
        FNR == 1 && NR > 1 { print "" }
        $0 == "@APP_MANIFEST@" {
            while ((getline m < manifest) > 0) print m
            close(manifest)
            next
        }
        { print }
    ' "$templates/version.sh" "$templates/common.sh" "$templates/app.sh"
}

# render KIND VERSION ROOT UID GID PKGUTIL CODESIGN OUT
render() {
    local kind="$1" version="$2" root="$3" uid="$4" gid="$5" pkgutil="$6" codesign="$7" out="$8"
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
    [[ "$codesign" =~ ^(/[A-Za-z0-9._\ -]+)+$ ]] || die "unusable codesign path '$codesign'"
    # The libraries go in whole where the template says @COMMON@.
    local lib
    lib="$(mktemp)"
    libraries >"$lib"
    awk -v lib="$lib" '
        $0 == "@COMMON@" {
            while ((getline line < lib) > 0) print line
            close(lib)
            next
        }
        { print }
    ' "$template" | sed \
        -e "s|@VERSION@|$version|g" \
        -e "s|@ROOT@|$root|g" \
        -e "s|@EXPECT_UID@|$uid|g" \
        -e "s|@EXPECT_GID@|$gid|g" \
        -e "s|@PKGUTIL@|$pkgutil|g" \
        -e "s|@CODESIGN@|$codesign|g" >"$out"
    rm -f "$lib"
    if grep -n '@[A-Z_]*@' "$out" >&2; then
        die "a placeholder is left in $out"
    fi
    chmod 0755 "$out"
}

build() {
    local version="$1" app="$2" out="$3" state
    syndeo_version_valid "$version" || die "'$version' is not a version"
    [ "$(basename "$app")" = Syndeo.app ] || die "expected a Syndeo.app, not $(basename "$app")"
    [ -d "$app" ] && [ ! -L "$app" ] || die "no bundle at $app"
    mkdir -p "$out"

    # Exactly the bundle for this version, signed, stapled or not.
    if [ -e "$app/Contents/CodeResources" ]; then state=stapled; else state=signed; fi
    bash "$here/package-macos-app.sh" --check "$app" "$version" "$state" >&2 ||
        die "$app is not exactly Syndeo.app $version, $state"
    codesign --verify --strict --deep "$app" || die "$app's signature does not verify"

    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT

    # The payload: the bundle, copied whole, then checked again.
    local stage="$work/root"
    mkdir -p "$stage"
    chmod 0755 "$stage"
    ditto --norsrc --noextattr --noqtn --noacl "$app" "$stage/Syndeo.app"
    xattr -crs "$stage"
    local epoch stamp
    epoch="${SOURCE_DATE_EPOCH:-$(git -C "$repo" log -1 --format=%ct)}"
    stamp="$(date -u -r "$epoch" +%Y%m%d%H%M.%S)"
    find "$stage" -exec env TZ=UTC touch -h -t "$stamp" {} +
    bash "$here/package-macos-app.sh" --check "$stage/Syndeo.app" "$version" "$state" >&2 ||
        die "the copy of $app is not exactly Syndeo.app $version"
    codesign --verify --strict --deep "$stage/Syndeo.app" || die "the copy's signature does not verify"

    mkdir "$work/scripts" "$work/component"
    render preinstall "$version" '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign "$work/scripts/preinstall"
    render postinstall "$version" '' 0 0 /usr/sbin/pkgutil /usr/bin/codesign "$work/scripts/postinstall"

    /usr/bin/pkgbuild --quiet --root "$stage" --component-plist "$templates/component.plist" \
        --identifier com.sum.syndeo.app --version "$version" --install-location /Applications \
        --ownership recommended --info "$templates/PackageInfo.xml" \
        --scripts "$work/scripts" "$work/component/Syndeo.pkg"

    # What the design rests on, checked rather than assumed: the info
    # template's overwrite-permissions, the bundle's settings, and the scripts.
    pkgutil --expand "$work/component/Syndeo.pkg" "$work/check"
    bash "$here/verify-pkg.sh" package-info "$work/check/PackageInfo" "$version" ||
        die "pkgbuild did not record what the component has to be; stopping, as the plan requires"

    sed "s|@VERSION@|$version|g" "$templates/Distribution.xml.in" >"$work/Distribution.xml"
    local name="syndeo-$version-aarch64-apple-darwin"
    /usr/bin/productbuild --quiet --distribution "$work/Distribution.xml" \
        --package-path "$work/component" "$out/$name.pkg"
    echo "built $out/$name.pkg"
    shasum -a 256 "$out/$name.pkg"
}

case "${1:-}" in
    --render)
        shift
        [ "$#" -eq 8 ] || die "--render <kind> <version> <root> <uid> <gid> <pkgutil> <codesign> <out-file>"
        render "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8"
        ;;
    --library)
        [ "$#" -eq 2 ] || die "--library <out-file>"
        libraries >"$2"
        ;;
    '' | -*)
        die "usage: $0 <version> <Syndeo.app> <out-dir>"
        ;;
    *)
        [ "$#" -eq 3 ] || die "usage: $0 <version> <Syndeo.app> <out-dir>"
        build "$1" "$2" "$3"
        ;;
esac
