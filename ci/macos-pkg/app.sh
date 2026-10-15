# What the application package's root scripts, and the uninstaller inside
# Syndeo.app, know about Syndeo.app, its receipt, and the discontinued
# command-line package it replaces.
#
# Embedded after version.sh and common.sh; no root script sources a file when
# it runs. common.sh describes the v0.1.x command-line package
# (com.sum.syndeo.pkg) and holds the shared checks; this file describes the
# application package (com.sum.syndeo.app). Each script sets, besides
# common.sh's literals:
#   CODESIGN    /usr/bin/codesign in every package; a stand-in in the self-tests
#
# The application package installs /Applications/Syndeo.app and nothing
# else. Its receipt lists the bundle's paths relative to /Applications. The
# bundle holds exactly ci/macos-app/bundle.manifest (embedded below when the
# package is built), its code signature's two paths, and, in a notarized
# release, the stapled ticket, Contents/CodeResources. Everything is owned by
# root:wheel, directories and executables 0755, everything else 0644.

APP_RECEIPT=com.sum.syndeo.app
APP_BUNDLE_ID=com.sum.syndeo
APP_DIR="$ROOT/Applications"
APP="$APP_DIR/Syndeo.app"
APP_TICKET=Contents/CodeResources

# app_manifest: every path in Syndeo.app as the package installs it,
# relative to the bundle, with its type (d or f) and mode, signature
# included and the ticket not.
app_manifest() {
    /bin/cat <<'EOF'
@APP_MANIFEST@
Contents/_CodeSignature d 755
Contents/_CodeSignature/CodeResources f 644
EOF
}

# app_spec REL: "TYPE MODE" for REL, a path relative to the bundle, if the
# package installs it there; nothing otherwise.
app_spec() {
    { app_manifest; printf '%s f 644\n' "$APP_TICKET"; } | while read -r _as_p _as_t _as_m; do
        if [ "$_as_p" = "$1" ]; then
            echo "$_as_t $_as_m"
        fi
    done
}

# app_receipt_files yes|no: the paths a receipt for the bundle lists, sorted,
# with the stapled ticket or without it.
app_receipt_files() {
    {
        echo Syndeo.app
        app_manifest | while read -r _ar_p _ar_t _ar_m; do
            echo "Syndeo.app/$_ar_p"
        done
        if [ "$1" = yes ]; then
            echo "Syndeo.app/$APP_TICKET"
        fi
    } | LC_ALL=C /usr/bin/sort
}

# app_running: the process IDs, one per line, of anything running the
# bundle's application or its helper from /Applications. A process counts
# when that file is its program text, as lsof finds it, and it is called
# Syndeo or syndeo-proxy: something that only reads the file, as Spotlight
# or a malware scan may just after an install, does not. Only a path that is
# a regular file is looked up, so a link leads nowhere.
app_running() {
    for _ru_p in "$APP/Contents/MacOS/Syndeo" "$APP/Contents/Helpers/syndeo-proxy"; do
        if [ -f "$_ru_p" ] && [ ! -L "$_ru_p" ]; then
            for _ru_pid in $(/usr/sbin/lsof -a -d txt -t -- "$_ru_p" 2>/dev/null); do
                _ru_c=$(/bin/ps -o comm= -p "$_ru_pid" 2>/dev/null) || continue
                case "${_ru_c##*/}" in
                    Syndeo | syndeo-proxy) echo "$_ru_pid" ;;
                esac
            done
        fi
    done | LC_ALL=C /usr/bin/sort -u
}

# check_not_running: Syndeo.app is not running from /Applications.
check_not_running() {
    _nr=$(app_running)
    if [ -n "$_nr" ]; then
        finding "Syndeo is running from $APP (process $(printf '%s' "$_nr" | /usr/bin/tr '\n' ' ' | /usr/bin/sed 's/ $//')): quit it first"
    fi
    return 0
}

# check_apps_dir: /Applications is a directory, and not a link to one.
check_apps_dir() {
    load_meta "$APP_DIR"
    if [ "$m_type" != Directory ]; then
        finding "$APP_DIR: is a ${m_type:-path that cannot be read}, not a directory"
    fi
    return 0
}

# app_info KEY: KEY from the bundle's Info.plist, if that is a regular file;
# nothing otherwise.
app_info() {
    _ai="$APP/Contents/Info.plist"
    if [ -f "$_ai" ] && [ ! -L "$_ai" ]; then
        /usr/bin/plutil -extract "$1" raw -o - "$_ai" 2>/dev/null || :
    fi
}

# check_app_bundle VERSION: $APP is Syndeo.app VERSION exactly as the
# package installs it: every path the manifest and the signature name, the
# stapled ticket or not, each owned by root:wheel with its mode and nothing
# else, and nothing more; its identifier and version; and a code signature
# that verifies, strictly and with everything nested in it, as
# com.sum.syndeo. Never descends into anything that is not a real directory.
# Sets app_ticket to yes or no.
check_app_bundle() {
    _cb_v=$1
    app_ticket=no
    _cb_before=$nfindings
    check_private "$APP" Directory 755
    [ "$nfindings" = "$_cb_before" ] || return 0
    _cb_dirs=$(app_manifest | while read -r _cb_p _cb_t _cb_m; do
        if [ "$_cb_t" = d ]; then echo "$_cb_p"; fi
    done)
    for _cb_d in '' $_cb_dirs; do
        if [ -n "$_cb_d" ]; then
            _cb_dp="$APP/$_cb_d"
            # A directory found wrong is a finding already; nothing in it is
            # looked at.
            is_private "$_cb_dp" Directory 755 || continue
        else
            _cb_dp=$APP
        fi
        for _cb_e in "$_cb_dp"/* "$_cb_dp"/.[!.]* "$_cb_dp"/..?*; do
            is_present "$_cb_e" || continue
            _cb_r=${_cb_e#"$APP/"}
            _cb_s=$(app_spec "$_cb_r")
            case "$_cb_s" in
                'd 755') check_private "$_cb_e" Directory 755 ;;
                'f 755') check_private "$_cb_e" 'Regular File' 755 ;;
                'f 644') check_private "$_cb_e" 'Regular File' 644 ;;
                *) finding "$_cb_e: is not part of Syndeo.app" ;;
            esac
        done
    done
    _cb_missing=$(app_manifest | while read -r _cb_p _cb_t _cb_m; do
        is_present "$APP/$_cb_p" || echo "$_cb_p"
    done)
    for _cb_p in $_cb_missing; do
        finding "$APP/$_cb_p: is missing"
    done
    if is_present "$APP/$APP_TICKET"; then
        app_ticket=yes
    fi
    [ "$nfindings" = "$_cb_before" ] || return 0
    _cb_id=$(app_info CFBundleIdentifier)
    if [ "$_cb_id" != "$APP_BUNDLE_ID" ]; then
        finding "$APP: is the bundle '$_cb_id', not $APP_BUNDLE_ID"
        return 0
    fi
    for _cb_k in CFBundleShortVersionString CFBundleVersion; do
        _cb_have=$(app_info "$_cb_k")
        if [ "$_cb_have" != "$_cb_v" ]; then
            finding "$APP: its $_cb_k is '$_cb_have', not $_cb_v"
        fi
    done
    if ! "$CODESIGN" --verify --strict --deep "$APP" >/dev/null 2>&1; then
        finding "$APP: its code signature does not verify: it has been changed since it was signed"
    else
        _cb_sid=$("$CODESIGN" --display --verbose=1 "$APP" 2>&1 | /usr/bin/sed -n 's/^Identifier=//p') || _cb_sid=''
        if [ "$_cb_sid" != "$APP_BUNDLE_ID" ]; then
            finding "$APP: is signed as '$_cb_sid', not $APP_BUNDLE_ID"
        fi
    fi
    return 0
}

# is_app_bundle VERSION: check_app_bundle, as a predicate that records no
# finding.
is_app_bundle() {
    _ib_findings=$findings
    _ib_n=$nfindings
    check_app_bundle "$1"
    _ib_ok=yes
    [ "$nfindings" = "$_ib_n" ] || _ib_ok=no
    findings=$_ib_findings
    nfindings=$_ib_n
    [ "$_ib_ok" = yes ]
}

# read_app_receipt: sets ar_present (yes or no) and ar_id, ar_version,
# ar_volume, ar_locations (how many location: fields) and ar_location (what
# follows "location:", the separating space included), from the receipt for
# com.sum.syndeo.app on the startup volume.
read_app_receipt() {
    ar_present=no
    ar_id=''
    ar_version=''
    ar_volume=''
    ar_locations=0
    ar_location=''
    _ra=$("$PKGUTIL" --pkg-info "$APP_RECEIPT" --volume / 2>/dev/null) || return 0
    ar_present=yes
    ar_id=$(printf '%s\n' "$_ra" | /usr/bin/sed -n 's/^package-id: //p')
    ar_version=$(printf '%s\n' "$_ra" | /usr/bin/sed -n 's/^version: //p')
    ar_volume=$(printf '%s\n' "$_ra" | /usr/bin/sed -n 's/^volume: //p')
    ar_locations=$(printf '%s\n' "$_ra" | /usr/bin/grep -c '^location:' || true)
    ar_location=$(printf '%s\n' "$_ra" | /usr/bin/sed -n 's/^location://p')
}

# check_app_receipt_meta: the receipt read by read_app_receipt is this
# package's, for the startup volume, installed in /Applications, for a valid
# version, and lists exactly the bundle's paths, with the ticket or without
# it. Sets ar_ticket to yes or no when it does. Says nothing about whether
# those paths are there: see check_app_bundle.
check_app_receipt_meta() {
    ar_ticket=''
    if [ "$ar_id" != "$APP_RECEIPT" ]; then
        finding "the receipt is for '$ar_id', not $APP_RECEIPT"
    fi
    if ! syndeo_version_valid "$ar_version"; then
        finding "the receipt for $APP_RECEIPT has the version '$ar_version', which is not a version"
        return 0
    fi
    if [ "$ar_volume" != / ]; then
        finding "the receipt for $APP_RECEIPT is for volume '$ar_volume', not /"
    fi
    if [ "$ar_locations" != 1 ] || [ "$ar_location" != ' Applications' ]; then
        finding "the receipt for $APP_RECEIPT does not say the bundle is in Applications (it has $ar_locations location fields, 'location:$ar_location')"
    fi
    _cm_have=$("$PKGUTIL" --files "$APP_RECEIPT" --volume / 2>/dev/null | LC_ALL=C /usr/bin/sort) || _cm_have=''
    if [ "$_cm_have" = "$(app_receipt_files no)" ]; then
        ar_ticket=no
    elif [ "$_cm_have" = "$(app_receipt_files yes)" ]; then
        ar_ticket=yes
    else
        finding "the receipt for $APP_RECEIPT $ar_version does not list exactly Syndeo.app's paths"
    fi
    return 0
}

# ------------------------------------------------------------------ v0.1.x
#
# The discontinued command-line package, com.sum.syndeo.pkg, is removed by
# this package's postinstall, and only when it is exactly as its package left
# it. Its paths are removed in one fixed order, each looked at again just
# before: the seven command links in /usr/local/bin, `current`, the
# version's files, its tools directory, its uninstaller, its directory, and
# the private directory. Then /usr/local/libexec, under the conditions in
# legacy_libexec, and only then is its receipt forgotten. /usr/local/bin,
# /usr/local and anything else are never removed. A removal that was
# interrupted leaves the paths after where it stopped, in that order, and the
# receipt: the same package, run again, carries on from there.

# legacy_steps VERSION: the v0.1.x paths, in the order they are removed, one
# per line.
legacy_steps() {
    for _ls_n in $SYNDEO_NAMES; do
        printf '%s\n' "$SYNDEO_BIN/$_ls_n"
    done
    printf '%s\n' "$SYNDEO_DIR/current"
    for _ls_n in $SYNDEO_NAMES $SYNDEO_DOCUMENTS tools/wordcount.wat tools uninstall.sh; do
        printf '%s\n' "$SYNDEO_DIR/$1/$_ls_n"
    done
    printf '%s\n' "$SYNDEO_DIR/$1" "$SYNDEO_DIR"
}

# legacy_step_ok PATH VERSION: PATH, one of legacy_steps' paths, is exactly
# what the v0.1.x package of VERSION put there.
legacy_step_ok() {
    case "$1" in
        "$SYNDEO_BIN"/*) is_ours_link "$1" "${1##*/}" ;;
        "$SYNDEO_DIR/current")
            is_private "$1" 'Symbolic Link' - && [ "$(/usr/bin/readlink "$1")" = "$2" ]
            ;;
        "$SYNDEO_DIR" | "$SYNDEO_DIR/$2" | "$SYNDEO_DIR/$2/tools") is_private "$1" Directory 755 ;;
        "$SYNDEO_DIR/$2/README.md" | "$SYNDEO_DIR/$2/LICENSE" | "$SYNDEO_DIR/$2/tools/wordcount.wat")
            is_private "$1" 'Regular File' 644
            ;;
        *) is_private "$1" 'Regular File' 755 ;;
    esac
}

# legacy_plan ALLOW-PARTIAL: what this package may do with a v0.1.x
# command-line package. Sets legacy to
#   none     there is no com.sum.syndeo.pkg receipt: nothing to remove, and
#            nothing in /usr/local is looked at, whatever is there;
#   whole    the receipt, and an exact, healthy installation of its 0.1.x
#            version: every path as its package left it, and nothing else;
#   partial  the receipt, and what is left of that installation after this
#            package's own removal stopped: the paths from legacy_from on,
#            each exact, and none before. Only with ALLOW-PARTIAL yes, which
#            the caller passes when Syndeo.app is already exactly this
#            package's version, so that only this package can have stopped
#            there.
# and legacy_v to its version and legacy_from to the number of the first
# step still to do (none left: one past the last). Anything else, it records
# as findings.
legacy_plan() {
    legacy=none
    legacy_v=''
    legacy_from=''
    read_receipt
    [ "$r_present" = yes ] || return 0
    _lp_before=$nfindings
    check_receipt_meta
    [ "$nfindings" = "$_lp_before" ] || return 0
    case "$r_version" in
        0.1.*) ;;
        *)
            finding "the receipt for $SYNDEO_ID is for $r_version, which is not a v0.1.x command-line package"
            return 0
            ;;
    esac
    legacy_v=$r_version
    check_strict_dir "$SYNDEO_LOCAL"
    check_strict_dir "$SYNDEO_LIBEXEC"
    check_bin_dir "$SYNDEO_BIN"
    [ "$nfindings" = "$_lp_before" ] || return 0
    # Nothing in the private directory but the version and `current`, and the
    # version holding nothing it did not install.
    if is_present "$SYNDEO_DIR"; then
        scan_private
        case "$versions" in
            '' | " $legacy_v") ;;
            *) finding "$SYNDEO_DIR: holds${versions}; only $legacy_v may be there" ;;
        esac
        case "$current_v" in
            '' | "$legacy_v") ;;
            *) finding "$SYNDEO_DIR/current: points at $current_v, not $legacy_v" ;;
        esac
        if [ -n "$pending_v" ]; then
            finding "$SYNDEO_DIR/.current.new: is there, from a switch that did not finish"
        fi
    fi
    _lp_i=0
    _lp_steps=$(legacy_steps "$legacy_v")
    while IFS= read -r _lp_p; do
        _lp_i=$((_lp_i + 1))
        if is_present "$_lp_p"; then
            [ -n "$legacy_from" ] || legacy_from=$_lp_i
            legacy_step_ok "$_lp_p" "$legacy_v" ||
                finding "$_lp_p: is not what the v0.1.x $legacy_v package put there"
        elif [ -n "$legacy_from" ]; then
            finding "$_lp_p: is missing, and the receipt for $legacy_v lists it"
        fi
    done <<EOF
$_lp_steps
EOF
    [ -n "$legacy_from" ] || legacy_from=$((_lp_i + 1))
    [ "$nfindings" = "$_lp_before" ] || return 0
    if [ "$legacy_from" = 1 ]; then
        legacy=whole
    elif [ "$1" = yes ]; then
        legacy=partial
    else
        finding "the v0.1.x $legacy_v command-line package is only partly there: $(legacy_step_name $((legacy_from - 1))) is gone, and everything removed before it"
    fi
    return 0
}

# legacy_running: the process IDs, one per line, of anything running one of
# the v0.1.x package's seven programs from its own tree,
# /usr/local/libexec/syndeo/<version>/, however it was started: through its
# link in /usr/local/bin, through `current`, or directly, its program text is
# that file. As for Syndeo.app, a process counts when lsof finds that file its
# program text and it is called by that program's name, so that a scan of the
# file does not. Only a path that is a regular file is looked up.
legacy_running() {
    for _lr_n in $SYNDEO_NAMES; do
        _lr_p="$SYNDEO_DIR/$legacy_v/$_lr_n"
        if [ -f "$_lr_p" ] && [ ! -L "$_lr_p" ]; then
            for _lr_pid in $(/usr/sbin/lsof -a -d txt -t -- "$_lr_p" 2>/dev/null); do
                _lr_c=$(/bin/ps -o comm= -p "$_lr_pid" 2>/dev/null) || continue
                if [ "${_lr_c##*/}" = "$_lr_n" ]; then
                    echo "$_lr_pid"
                fi
            done
        fi
    done | LC_ALL=C /usr/bin/sort -u
}

# check_legacy_not_running: nothing of the v0.1.x package that legacy_plan
# found to remove is running.
check_legacy_not_running() {
    case "$legacy" in
        whole | partial) ;;
        *) return 0 ;;
    esac
    _ln=$(legacy_running)
    if [ -n "$_ln" ]; then
        finding "the v0.1.x command-line package $legacy_v is running from $SYNDEO_DIR/$legacy_v (process $(printf '%s' "$_ln" | /usr/bin/tr '\n' ' ' | /usr/bin/sed 's/ $//')): quit it first"
    fi
    return 0
}

# legacy_step_name N: the path of step N.
legacy_step_name() {
    legacy_steps "$legacy_v" | /usr/bin/sed -n "${1}p" | /usr/bin/tr -d '\n'
}

# legacy_refusal: what to do about a v0.1.x installation this package will
# not touch, once its findings are printed.
legacy_refusal() {
    if [ "$r_present" = yes ] && syndeo_version_valid "$r_version"; then
        _lr="repair it by installing the Syndeo $r_version package again"
        if [ -f "$SYNDEO_DIR/$r_version/uninstall.sh" ]; then
            _lr="$_lr, or remove it with sudo /bin/sh $SYNDEO_DIR/$r_version/uninstall.sh"
        fi
        finding "nothing of the v0.1.x command-line package $r_version is removed unless it is exactly as its package left it: $_lr; then install this package"
    fi
    return 0
}

# legacy_libexec: after the v0.1.x private directory is gone, before the
# receipt is forgotten, /usr/local/libexec is removed only if it is an
# actual, empty directory, root:wheel, mode 0755, with no access control
# list, no flags and no extended attributes, which is how the v0.1.x package
# created it, and no package but com.sum.syndeo.pkg claims it. With rmdir,
# which removes nothing but an empty directory and follows no link. Anything
# else, a failed rmdir included, leaves it where it is: a shared parent
# directory is harmless. Says which.
legacy_libexec() {
    if ! is_present "$SYNDEO_LIBEXEC"; then
        return 0
    fi
    _ll_why=''
    load_meta "$SYNDEO_LIBEXEC"
    if [ "$m_type" != Directory ]; then
        _ll_why="it is a ${m_type:-path that cannot be read}"
    elif [ "$m_uid" != "$EXPECT_UID" ] || [ "$m_gid" != "$EXPECT_GID" ] || [ "$m_perm" != 755 ] || [ "$m_special" != 0 ]; then
        _ll_why="it is $m_uid:$m_gid mode $m_special$m_perm, not root:wheel 0755"
    elif [ "$m_flags" != - ]; then
        _ll_why="it has flags ($m_flags)"
    elif has_acl "$SYNDEO_LIBEXEC"; then
        _ll_why="it has an access control list"
    elif [ -n "$(/usr/bin/xattr -s "$SYNDEO_LIBEXEC" 2>/dev/null || echo unreadable)" ]; then
        _ll_why="it has extended attributes"
    elif [ -n "$(/bin/ls -A "$SYNDEO_LIBEXEC" 2>/dev/null || echo unreadable)" ]; then
        _ll_why="it is not empty"
    elif ! _ll_info=$("$PKGUTIL" --file-info "$SYNDEO_LIBEXEC" 2>/dev/null); then
        _ll_why="which packages claim it could not be read"
    else
        for _ll_id in $(printf '%s\n' "$_ll_info" | /usr/bin/sed -n 's/^pkgid: //p'); do
            if [ "$_ll_id" != "$SYNDEO_ID" ]; then
                _ll_why="another package, $_ll_id, claims it"
                break
            fi
        done
    fi
    if [ -z "$_ll_why" ]; then
        if /bin/rmdir -- "$SYNDEO_LIBEXEC" 2>/dev/null; then
            say "removed $SYNDEO_LIBEXEC, which the v0.1.x package created, empty"
            return 0
        fi
        _ll_why="it could not be removed"
    fi
    say "left $SYNDEO_LIBEXEC in place: $_ll_why"
    return 0
}
