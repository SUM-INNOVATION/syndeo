# What the package's three root scripts share: where Syndeo lives, what it
# looks like there, and how to tell.
#
# Embedded into preinstall, postinstall and uninstall.sh when the package is
# built, after version.sh; no root script sources a file when it runs. Each
# script sets these first, as literals the build filled in:
#   SCRIPT      its own name, for messages
#   ROOT        '' in every package; a temporary root in the self-tests
#   EXPECT_UID  0 in every package
#   EXPECT_GID  0 in every package
#   PKGUTIL     /usr/sbin/pkgutil in every package
#
# Every check reads the path itself and never what a symlink points at: stat
# without -L is lstat, and ls -d lists a link rather than its target. A check
# records what is wrong as a finding and carries on, so that a refusal can name
# everything at once. Predicates (is_*, has_*) only ever appear as conditions,
# so set -e never ends a scan halfway.

SYNDEO_ID=com.sum.syndeo.pkg
SYNDEO_NAMES='syndeo syndeo-net syndeo-keystore syndeo-agent syndeo-proxy syndeo-ui syndeo-webkit'
SYNDEO_EXECUTABLES="$SYNDEO_NAMES uninstall.sh"
SYNDEO_DOCUMENTS='README.md LICENSE'
SYNDEO_LOCAL="$ROOT/usr/local"
SYNDEO_BIN="$SYNDEO_LOCAL/bin"
SYNDEO_LIBEXEC="$SYNDEO_LOCAL/libexec"
SYNDEO_DIR="$SYNDEO_LIBEXEC/syndeo"

findings=''
nfindings=0

finding() {
    findings="$findings
syndeo $SCRIPT: refusing: $1"
    nfindings=$((nfindings + 1))
}

say() {
    printf 'syndeo %s: %s\n' "$SCRIPT" "$*"
}

# refuse_if_found: print every finding so far and stop, if there are any.
refuse_if_found() {
    if [ "$nfindings" -gt 0 ]; then
        printf '%s\n' "$findings" | /usr/bin/sed '1d'
        exit 1
    fi
}

# is_present PATH: anything at PATH, a dangling symlink included.
is_present() {
    [ -e "$1" ] || [ -L "$1" ]
}

# load_meta PATH: sets m_type, m_uid, m_gid, m_perm (three octal digits),
# m_special (setuid, setgid and sticky, one digit) and m_flags, for PATH itself.
load_meta() {
    m_type=''
    m_uid=''
    m_gid=''
    m_perm=''
    m_special=''
    m_flags=''
    _lm=$(/usr/bin/stat -f '%HT|%u|%g|%03Lp|%Mp|%Sf' "$1" 2>/dev/null) || return 0
    m_type=${_lm%%|*}
    _lm=${_lm#*|}
    m_uid=${_lm%%|*}
    _lm=${_lm#*|}
    m_gid=${_lm%%|*}
    _lm=${_lm#*|}
    m_perm=${_lm%%|*}
    _lm=${_lm#*|}
    m_special=${_lm%%|*}
    m_flags=${_lm#*|}
}

# meta_line PATH: type, owner, group, mode, flags and inode in one line, for
# telling whether a path changed between two looks at it.
meta_line() {
    /usr/bin/stat -f '%HT|%u|%g|%03Lp|%Mp|%Sf|%i' "$1" 2>/dev/null || echo absent
}

has_acl() {
    [ -n "$(/bin/ls -lde "$1" 2>/dev/null | /usr/bin/sed -n 2p)" ]
}

# has_locking_flag: m_flags holds a flag that stops even root changing a path.
has_locking_flag() {
    case ",$m_flags," in
        *,uchg,* | *,schg,* | *,uappnd,* | *,sappnd,*) return 0 ;;
    esac
    return 1
}

# is_writable_digit D: whether an octal permission digit includes write.
is_writable_digit() {
    case "$1" in
        2 | 3 | 6 | 7) return 0 ;;
    esac
    return 1
}

group_digit() {
    _gd=${1#?}
    echo "${_gd%?}"
}

other_digit() {
    echo "${1#??}"
}

# check_strict_dir PATH: for /usr/local, /usr/local/libexec and the private
# directory. Absent is fine here; whoever needs it to exist says so.
check_strict_dir() {
    is_present "$1" || return 0
    load_meta "$1"
    if [ "$m_type" != Directory ]; then
        finding "$1: is a ${m_type:-path that cannot be read}, not a directory"
        return 0
    fi
    if [ "$m_uid" != "$EXPECT_UID" ]; then
        finding "$1: is owned by uid $m_uid, not root"
    fi
    if is_writable_digit "$(group_digit "$m_perm")" || is_writable_digit "$(other_digit "$m_perm")"; then
        finding "$1: can be written by its group or by everyone (mode $m_perm)"
    fi
    if has_acl "$1"; then
        finding "$1: has an access control list"
    fi
    if has_locking_flag; then
        finding "$1: has an immutable or append-only flag ($m_flags)"
    fi
    return 0
}

# check_bin_dir PATH: /usr/local/bin may belong to a person (a Mac that once
# ran Homebrew on Intel has it so), and may be group-writable by admin or
# wheel. Not by everyone, not through an ACL, not locked, and not a symlink.
check_bin_dir() {
    is_present "$1" || return 0
    load_meta "$1"
    if [ "$m_type" != Directory ]; then
        finding "$1: is a ${m_type:-path that cannot be read}, not a directory"
        return 0
    fi
    if is_writable_digit "$(other_digit "$m_perm")"; then
        finding "$1: can be written by everyone (mode $m_perm)"
    fi
    if is_writable_digit "$(group_digit "$m_perm")"; then
        case "$m_gid" in
            0 | 80) ;;
            *) finding "$1: can be written by group $m_gid, which is neither wheel nor admin" ;;
        esac
    fi
    if has_acl "$1"; then
        finding "$1: has an access control list"
    fi
    if has_locking_flag; then
        finding "$1: has an immutable or append-only flag ($m_flags)"
    fi
    return 0
}

# is_private PATH TYPE MODE: whether PATH is TYPE, owned by the expected user
# and group, exactly MODE, with no special bits, no flags but compression, and
# no access control list. MODE is ignored for symlinks.
is_private() {
    load_meta "$1"
    [ "$m_type" = "$2" ] || return 1
    [ "$m_uid" = "$EXPECT_UID" ] || return 1
    [ "$m_gid" = "$EXPECT_GID" ] || return 1
    [ "$2" = 'Symbolic Link' ] && return 0
    [ "$m_perm" = "$3" ] || return 1
    [ "$m_special" = 0 ] || return 1
    case "$m_flags" in
        - | compressed) ;;
        *) return 1 ;;
    esac
    if has_acl "$1"; then
        return 1
    fi
    return 0
}

# check_private PATH TYPE MODE: is_private, as a finding.
check_private() {
    if ! is_private "$1" "$2" "$3"; then
        finding "$1: is not a $2 owned by root:wheel with mode $3 and nothing else (found ${m_type:-nothing} $m_uid:$m_gid ${m_perm}${m_special:+ special $m_special} flags ${m_flags:--})"
    fi
    return 0
}

# check_tree VERSION exact|subset: the version directory holds the package's
# files, owned and moded exactly, and nothing else; exact also needs them all.
# Never descends into anything that is not a real directory.
check_tree() {
    _ct_v=$1
    _ct_t="$SYNDEO_DIR/$1"
    _ct_before=$nfindings
    check_private "$_ct_t" Directory 755
    [ "$nfindings" = "$_ct_before" ] || return 0
    for _ct_e in "$_ct_t"/* "$_ct_t"/.[!.]* "$_ct_t"/..?*; do
        is_present "$_ct_e" || continue
        _ct_n=${_ct_e##*/}
        case " $SYNDEO_EXECUTABLES " in
            *" $_ct_n "*)
                check_private "$_ct_e" 'Regular File' 755
                continue
                ;;
        esac
        case " $SYNDEO_DOCUMENTS " in
            *" $_ct_n "*)
                check_private "$_ct_e" 'Regular File' 644
                continue
                ;;
        esac
        if [ "$_ct_n" = tools ]; then
            _ct_tb=$nfindings
            check_private "$_ct_e" Directory 755
            if [ "$nfindings" = "$_ct_tb" ]; then
                for _ct_f in "$_ct_e"/* "$_ct_e"/.[!.]* "$_ct_e"/..?*; do
                    is_present "$_ct_f" || continue
                    if [ "${_ct_f##*/}" = wordcount.wat ]; then
                        check_private "$_ct_f" 'Regular File' 644
                    else
                        finding "$_ct_f: is not part of Syndeo $_ct_v"
                    fi
                done
            fi
            continue
        fi
        finding "$_ct_e: is not part of Syndeo $_ct_v"
    done
    if [ "$2" = exact ]; then
        for _ct_n in $SYNDEO_EXECUTABLES $SYNDEO_DOCUMENTS tools tools/wordcount.wat; do
            is_present "$_ct_t/$_ct_n" || finding "$_ct_t/$_ct_n: is missing"
        done
    fi
    return 0
}

# is_complete_tree VERSION: whether every file the package puts in the
# version directory is there. What is there is for check_tree to judge.
is_complete_tree() {
    for _ic_n in $SYNDEO_EXECUTABLES $SYNDEO_DOCUMENTS tools tools/wordcount.wat; do
        is_present "$SYNDEO_DIR/$1/$_ic_n" || return 1
    done
    return 0
}

# is_ours_link PATH NAME: a command link exactly as the package writes it.
is_ours_link() {
    is_private "$1" 'Symbolic Link' - || return 1
    [ "$(/usr/bin/readlink "$1")" = "../libexec/syndeo/current/$2" ]
}

# scan_bin: sets bin_present (how many of the seven names exist) and
# bin_foreign (the names, not paths, of those that are not the package's
# links).
scan_bin() {
    bin_present=0
    bin_foreign=''
    for _sb_n in $SYNDEO_NAMES; do
        _sb_p="$SYNDEO_BIN/$_sb_n"
        is_present "$_sb_p" || continue
        bin_present=$((bin_present + 1))
        if ! is_ours_link "$_sb_p" "$_sb_n"; then
            bin_foreign="$bin_foreign $_sb_n"
        fi
    done
    return 0
}

# scan_private: what the private directory holds. Sets d_present,
# versions (space-separated), current_v and pending_v (the targets of current
# and .current.new, or ''), and checks every version directory is at least an
# exact subset of the package. Anything else in it is a finding. Whether
# current may name a version that is not there is for each script to say:
# after a failed upgrade it does.
scan_private() {
    d_present=no
    versions=''
    current_v=''
    pending_v=''
    is_present "$SYNDEO_DIR" || return 0
    d_present=yes
    _sp_before=$nfindings
    check_strict_dir "$SYNDEO_DIR"
    [ "$nfindings" = "$_sp_before" ] || return 0
    for _sp_e in "$SYNDEO_DIR"/* "$SYNDEO_DIR"/.[!.]* "$SYNDEO_DIR"/..?*; do
        is_present "$_sp_e" || continue
        _sp_n=${_sp_e##*/}
        case "$_sp_n" in
            current | .current.new)
                if ! is_private "$_sp_e" 'Symbolic Link' -; then
                    finding "$_sp_e: is not a symlink owned by root:wheel"
                    continue
                fi
                _sp_t=$(/usr/bin/readlink "$_sp_e") || _sp_t=''
                if ! syndeo_version_valid "$_sp_t"; then
                    finding "$_sp_e: points at '$_sp_t', which is not a version"
                    continue
                fi
                if [ "$_sp_n" = current ]; then
                    current_v=$_sp_t
                else
                    pending_v=$_sp_t
                fi
                ;;
            *)
                if syndeo_version_valid "$_sp_n"; then
                    versions="$versions $_sp_n"
                    check_tree "$_sp_n" subset
                else
                    finding "$_sp_e: is not part of Syndeo"
                fi
                ;;
        esac
    done
    return 0
}

# version_count: how many version directories scan_private found.
version_count() {
    set -- $versions
    echo $#
}

# has_version V: whether V is one of the version directories found.
has_version() {
    case " $versions " in
        *" $1 "*) return 0 ;;
    esac
    return 1
}

# expected_files V: every path the receipt for V lists, as pkgutil prints them.
expected_files() {
    printf '%s\n' usr usr/local usr/local/bin usr/local/libexec usr/local/libexec/syndeo \
        "usr/local/libexec/syndeo/$1" "usr/local/libexec/syndeo/$1/tools" \
        "usr/local/libexec/syndeo/$1/tools/wordcount.wat"
    for _ef_n in $SYNDEO_EXECUTABLES $SYNDEO_DOCUMENTS; do
        printf '%s\n' "usr/local/libexec/syndeo/$1/$_ef_n"
    done
    for _ef_n in $SYNDEO_NAMES; do
        printf '%s\n' "usr/local/bin/$_ef_n"
    done
}

# read_receipt: sets r_present (yes or no) and r_id, r_version, r_volume,
# r_locations (how many location: fields there are) and r_location (what
# follows "location:", the separating space included) from the receipt on
# the startup volume.
read_receipt() {
    r_present=no
    r_id=''
    r_version=''
    r_volume=''
    r_locations=0
    r_location=''
    _rr=$("$PKGUTIL" --pkg-info "$SYNDEO_ID" --volume / 2>/dev/null) || return 0
    r_present=yes
    r_id=$(printf '%s\n' "$_rr" | /usr/bin/sed -n 's/^package-id: //p')
    r_version=$(printf '%s\n' "$_rr" | /usr/bin/sed -n 's/^version: //p')
    r_volume=$(printf '%s\n' "$_rr" | /usr/bin/sed -n 's/^volume: //p')
    r_locations=$(printf '%s\n' "$_rr" | /usr/bin/grep -c '^location:' || true)
    r_location=$(printf '%s\n' "$_rr" | /usr/bin/sed -n 's/^location://p')
}

# check_receipt_meta: the receipt read by read_receipt is Syndeo's, for the
# startup volume, for a valid version, listing exactly that version's paths,
# and no other package claims the private directory or a command. It says
# nothing about whether those paths are there: see check_receipt_installed.
# Sets r_files, the paths the receipt lists.
check_receipt_meta() {
    r_files=
    if [ "$r_id" != "$SYNDEO_ID" ]; then
        finding "the receipt is for '$r_id', not $SYNDEO_ID"
    fi
    if ! syndeo_version_valid "$r_version"; then
        finding "the receipt's version, '$r_version', is not a version"
        return 0
    fi
    if [ "$r_volume" != / ]; then
        finding "the receipt is for volume '$r_volume', not /"
    fi
    # pkgutil always prints one "location: %s" line, the location relative to
    # the volume: "/" for some packages installed at its root, and nothing for
    # this one. Only those two values after exactly that one space pass; a
    # missing or repeated field, any other spacing and any other location are
    # refused.
    if [ "$r_locations" != 1 ]; then
        finding "the receipt has $r_locations location fields, not one"
    else
        case "$r_location" in
            ' ' | ' /') ;;
            *) finding "the receipt's location is 'location:$r_location', not the volume's root" ;;
        esac
    fi
    _cr_want=$(expected_files "$r_version" | LC_ALL=C /usr/bin/sort)
    _cr_have=$("$PKGUTIL" --files "$SYNDEO_ID" --volume / 2>/dev/null | LC_ALL=C /usr/bin/sort) || _cr_have=''
    if [ "$_cr_want" != "$_cr_have" ]; then
        finding "the receipt for $r_version does not list exactly the package's paths"
    else
        r_files=$_cr_have
    fi
    check_claims "$SYNDEO_DIR"
    for _cr_n in $SYNDEO_NAMES; do
        check_claims "$SYNDEO_BIN/$_cr_n"
    done
    return 0
}

# check_receipt_installed V: every path the receipt for V lists is there, as
# the package writes it. Each is looked at itself: the parents as the
# directories they have to be, each command as the package's own link, and
# the version directory, whose entries check_tree judges, also finding
# anything in it the receipt does not list. After check_receipt_meta, which
# has found the receipt lists exactly the package's paths; none has a space.
check_receipt_installed() {
    _ci_v=$1
    for _ci_p in $r_files; do
        _ci_t="$ROOT/$_ci_p"
        if ! is_present "$_ci_t"; then
            finding "$_ci_t: is missing, and the receipt for $_ci_v lists it"
            continue
        fi
        case "$_ci_p" in
            usr)
                load_meta "$_ci_t"
                [ "$m_type" = Directory ] || finding "$_ci_t: is a ${m_type:-path that cannot be read}, not a directory"
                ;;
            usr/local | usr/local/libexec | usr/local/libexec/syndeo) check_strict_dir "$_ci_t" ;;
            usr/local/bin) check_bin_dir "$_ci_t" ;;
            usr/local/bin/*)
                is_ours_link "$_ci_t" "${_ci_p##*/}" || finding "$_ci_t: is not a link this package writes"
                ;;
            "usr/local/libexec/syndeo/$_ci_v" | "usr/local/libexec/syndeo/$_ci_v"/*) ;;
            *) finding "the receipt lists $_ci_p, which is not the package's" ;;
        esac
    done
    if is_present "$SYNDEO_DIR/$_ci_v"; then
        check_tree "$_ci_v" subset
    fi
    return 0
}

# check_claims PATH: no package but Syndeo's has PATH in its receipt.
check_claims() {
    _cc_ids=$("$PKGUTIL" --file-info "$1" 2>/dev/null | /usr/bin/sed -n 's/^pkgid: //p') || _cc_ids=''
    for _cc_id in $_cc_ids; do
        if [ "$_cc_id" != "$SYNDEO_ID" ]; then
            finding "$1: is claimed by another package, $_cc_id"
        fi
    done
    return 0
}
